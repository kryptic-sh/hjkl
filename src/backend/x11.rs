//! Linux X11 connection helpers used by `x11_thread.rs`.
//!
//! Opens a libxcb connection via `dlopen`, interns the atom table, and reads
//! screen info from the XCB setup reply. The resulting `X11Connection` is
//! consumed by the bg thread in `x11_thread.rs`, which owns an invisible
//! window, handles `SelectionRequest` events, and performs all clipboard ops
//! (`set`/`get`/`clear`/`available`) including INCR and auto-`SAVE_TARGETS`.

use std::ffi::c_char;

use crate::{ClipboardError, MimeType, Selection};

use super::{
    Backend,
    dlopen::{XcbConnection, XcbFns, xcb_fns},
};

// ---------------------------------------------------------------------------
// Screen info
// ---------------------------------------------------------------------------

/// Key fields extracted from the first screen in the XCB setup.
pub(crate) struct ScreenInfo {
    /// Root window XID.
    pub root: u32,
    /// Visual ID of the root window (needed for `xcb_create_window`).
    pub root_visual: u32,
    /// Root window color depth in bits per pixel.
    pub root_depth: u8,
    /// Screen width in pixels. Used in xvfb tests; not needed for clipboard ops.
    #[allow(dead_code)]
    pub width: u16,
    /// Screen height in pixels. Used in xvfb tests; not needed for clipboard ops.
    #[allow(dead_code)]
    pub height: u16,
    /// Maximum request length in bytes (from setup; value in setup is in
    /// 4-byte units so we multiply by 4 here).
    pub max_request_len_bytes: u32,
}

// ---------------------------------------------------------------------------
// Atoms
// ---------------------------------------------------------------------------

/// XCB atom IDs interned at connection time.
#[derive(Clone, Copy)]
pub(crate) struct Atoms {
    pub clipboard: u32,
    pub primary: u32,
    pub targets: u32,
    pub utf8_string: u32,
    pub string: u32,
    pub text_plain_utf8: u32,
    pub text_html: u32,
    pub text_rtf: u32,
    pub text_uri_list: u32,
    pub image_png: u32,
    pub incr: u32,
    pub clipboard_manager: u32,
    pub save_targets: u32,
    pub multiple: u32,
    /// Private property atom used as the reply target for our own get requests.
    pub hjkl_clipboard_get: u32,
}

// Atom names in the same order as the fields above.
const ATOM_NAMES: &[&str] = &[
    "CLIPBOARD",
    "PRIMARY",
    "TARGETS",
    "UTF8_STRING",
    "STRING",
    "text/plain;charset=utf-8",
    "text/html",
    "text/rtf",
    "text/uri-list",
    "image/png",
    "INCR",
    "CLIPBOARD_MANAGER",
    "SAVE_TARGETS",
    "MULTIPLE",
    "HJKL_CLIPBOARD_GET",
];

// ---------------------------------------------------------------------------
// Connection
// ---------------------------------------------------------------------------

/// A live XCB connection with interned atoms and screen info.
///
/// Not `Sync` — XCB connections must be driven from a single thread.
/// `Send` is implemented manually so the bg thread can take ownership after
/// `X11Connection::open()` returns on the calling thread.
pub(crate) struct X11Connection {
    fns: &'static XcbFns,
    conn: *mut XcbConnection,
    pub screen: ScreenInfo,
    pub atoms: Atoms,
}

// SAFETY: We cross the thread boundary exactly once (when handing the
// connection to the bg thread). After that the connection is owned and
// driven exclusively by the bg thread.
unsafe impl Send for X11Connection {}

impl X11Connection {
    /// Open a connection to the X server specified by `$DISPLAY`.
    ///
    /// `xcb_connect` handles both `$DISPLAY` parsing and MIT-MAGIC-COOKIE-1
    /// authentication by reading `~/.Xauthority` internally — we do not need
    /// to hand-parse the authority file ourselves.
    pub(crate) fn open() -> Result<Self, ClipboardError> {
        // Bail early if there is no DISPLAY — avoids the ~50 ms timeout XCB
        // would spend trying a default display that doesn't exist.
        if std::env::var_os("DISPLAY").is_none() {
            return Err(ClipboardError::NoDisplay);
        }

        let fns = xcb_fns()?;

        // SAFETY: passing NULL for both arguments tells xcb_connect to use
        // $DISPLAY and ignore the screen-number out-param. The returned
        // pointer is always non-null (xcb_connect may return an error
        // connection, detected by xcb_connection_has_error).
        let conn = unsafe { (fns.xcb_connect)(std::ptr::null(), std::ptr::null_mut()) };
        if conn.is_null() {
            return Err(ClipboardError::io_other("xcb_connect returned null"));
        }

        // SAFETY: conn is a non-null pointer returned by xcb_connect.
        let err = unsafe { (fns.xcb_connection_has_error)(conn) };
        if err != 0 {
            // SAFETY: conn is owned by us; disconnect on error path.
            unsafe { (fns.xcb_disconnect)(conn) };
            return Err(ClipboardError::io(std::io::Error::other(format!(
                "xcb_connection_has_error: {err}"
            ))));
        }

        // Read screen info from the setup reply.
        //
        // SAFETY: conn is a valid, error-free connection. xcb_get_setup
        // returns a pointer into conn's internal buffer; it is valid for
        // the lifetime of conn.
        let setup = unsafe { (fns.xcb_get_setup)(conn) };
        if setup.is_null() {
            // SAFETY: conn is still owned by us.
            unsafe { (fns.xcb_disconnect)(conn) };
            return Err(ClipboardError::io_other("xcb_get_setup returned null"));
        }

        // SAFETY: setup is non-null and valid. The iterator's `data` field
        // points to the first screen; we check it below.
        let iter = unsafe { (fns.xcb_setup_roots_iterator)(setup) };
        if iter.data.is_null() {
            unsafe { (fns.xcb_disconnect)(conn) };
            return Err(ClipboardError::io_other("no screens in XCB setup"));
        }

        // SAFETY: iter.data is non-null; xcb_screen_t layout is:
        //   u32 root, u32 default_colormap, u32 white_pixel, u32 black_pixel,
        //   u32 current_input_masks, u16 width_in_pixels, u16 height_in_pixels,
        //   u16 width_in_millimeters, u16 height_in_millimeters,
        //   u16 min_installed_maps, u16 max_installed_maps,
        //   u32 root_visual, u8 backing_stores, u8 save_unders,
        //   u8 root_depth, u8 allowed_depths_len.
        // We read only the fields we need. This layout is part of the stable
        // XCB ABI and matches libxcb's generated bindings.
        let screen_ptr = iter.data as *const u8;
        let root = u32::from_ne_bytes(
            // SAFETY: offset 0 is u32 root.
            unsafe { *screen_ptr.cast::<[u8; 4]>() },
        );
        // root_visual is at byte offset 32 (8 * u32 + 4 * u16 = 32 + 8 = 40?
        // Let us count: root(4), default_colormap(4), white_pixel(4),
        // black_pixel(4), current_input_masks(4) = 20 bytes; then
        // width_in_pixels(2), height_in_pixels(2), width_in_mm(2),
        // height_in_mm(2), min_installed_maps(2), max_installed_maps(2) = 12;
        // total so far = 32. root_visual is at offset 32.
        let width = u16::from_ne_bytes(unsafe { *screen_ptr.add(20).cast::<[u8; 2]>() });
        let height = u16::from_ne_bytes(unsafe { *screen_ptr.add(22).cast::<[u8; 2]>() });
        let root_visual = u32::from_ne_bytes(unsafe { *screen_ptr.add(32).cast::<[u8; 4]>() });
        // root_depth is at offset 38 in xcb_screen_t.
        // SAFETY: screen_ptr is valid; offset 38 is the root_depth u8 field.
        let root_depth = unsafe { *screen_ptr.add(38) };

        // maximum_request_length is in xcb_setup_t, not xcb_screen_t.
        // xcb_setup_t layout (relevant fields):
        //   u8 status, u8 pad0, u16 protocol_major_version,
        //   u16 protocol_minor_version, u16 length,
        //   u32 release_number, u32 resource_id_base, u32 resource_id_mask,
        //   u32 motion_buffer_size, u16 vendor_len, u16 maximum_request_length,
        // Offsets: status(1)+pad0(1)+major(2)+minor(2)+length(2)=8; then
        //   release(4)+id_base(4)+id_mask(4)+motion(4)=16; total=24;
        //   vendor_len(2) at 24; maximum_request_length(2) at 26.
        let setup_ptr = setup as *const u8;
        let max_req_units = u16::from_ne_bytes(unsafe { *setup_ptr.add(26).cast::<[u8; 2]>() });
        // Per X11 protocol: if maximum_request_length == 0 the server supports
        // the BigRequests extension and the real limit must be queried via
        // xcb_get_maximum_request_length. We treat 0 as "no practical limit" and
        // cap at 256 KiB so callers get reasonable INCR thresholds on modern
        // servers. Actual BigRequests limits are in the gigabytes — 256 KiB is
        // a safe conservative chunk size that works across all server variants.
        let max_request_len_bytes = if max_req_units == 0 {
            256 * 1024 // conservative 256 KiB when BigRequests active
        } else {
            u32::from(max_req_units) * 4
        };

        let screen = ScreenInfo {
            root,
            root_visual,
            root_depth,
            width,
            height,
            max_request_len_bytes,
        };

        // Intern atoms — batch all requests, then collect replies.
        let atoms = intern_atoms(fns, conn).inspect_err(|_| {
            // SAFETY: conn is still ours to disconnect on error.
            unsafe { (fns.xcb_disconnect)(conn) };
        })?;

        Ok(Self {
            fns,
            conn,
            screen,
            atoms,
        })
    }
}

impl Drop for X11Connection {
    fn drop(&mut self) {
        // SAFETY: conn was returned by xcb_connect and is still valid. We
        // only call xcb_disconnect once (here, in Drop).
        unsafe { (self.fns.xcb_disconnect)(self.conn) };
    }
}

impl X11Connection {
    /// The XCB function-pointer table.
    pub(crate) fn fns(&self) -> &'static XcbFns {
        self.fns
    }

    /// The raw XCB connection pointer.
    ///
    /// # Safety
    ///
    /// Callers must ensure they are on the thread that owns this connection.
    pub(crate) fn raw(&self) -> *mut super::dlopen::XcbConnection {
        self.conn
    }

    /// Screen info extracted at connection time.
    pub(crate) fn screen(&self) -> &ScreenInfo {
        &self.screen
    }

    /// Interned atom table.
    pub(crate) fn atoms(&self) -> &Atoms {
        &self.atoms
    }
}

// ---------------------------------------------------------------------------
// Atom interning helper
// ---------------------------------------------------------------------------

/// Send all intern-atom requests first, then collect replies (XCB pipeline).
fn intern_atoms(fns: &'static XcbFns, conn: *mut XcbConnection) -> Result<Atoms, ClipboardError> {
    use super::dlopen::XcbInternAtomCookie;

    let mut cookies: [XcbInternAtomCookie; 15] = [XcbInternAtomCookie { sequence: 0 }; 15];

    for (i, name) in ATOM_NAMES.iter().enumerate() {
        let len = name.len() as u16;
        // SAFETY: name.as_ptr() is valid for `len` bytes; only_if_exists=0
        // creates the atom if it doesn't already exist, which is what we want
        // for custom MIME type atoms.
        cookies[i] = unsafe { (fns.xcb_intern_atom)(conn, 0, len, name.as_ptr().cast::<c_char>()) };
    }

    let mut values = [0u32; 15];
    for (i, cookie) in cookies.into_iter().enumerate() {
        // SAFETY: cookie was returned by xcb_intern_atom for conn. Passing
        // null for the error pointer: any error causes a null reply, which
        // we detect below.
        let reply = unsafe { (fns.xcb_intern_atom_reply)(conn, cookie, std::ptr::null_mut()) };
        if reply.is_null() {
            return Err(ClipboardError::io(std::io::Error::other(format!(
                "xcb_intern_atom_reply null for atom {i} ({})",
                ATOM_NAMES[i]
            ))));
        }
        // SAFETY: reply is non-null and points to a heap-allocated
        // xcb_intern_atom_reply_t from libxcb. We read the atom field
        // then immediately free the reply.
        let atom = unsafe { (*reply).atom };
        // SAFETY: reply was allocated by xcb_intern_atom_reply via malloc;
        // we must free it with libc::free.
        unsafe { libc::free(reply.cast()) };
        values[i] = atom;
    }

    Ok(Atoms {
        clipboard: values[0],
        primary: values[1],
        targets: values[2],
        utf8_string: values[3],
        string: values[4],
        text_plain_utf8: values[5],
        text_html: values[6],
        text_rtf: values[7],
        text_uri_list: values[8],
        image_png: values[9],
        incr: values[10],
        clipboard_manager: values[11],
        save_targets: values[12],
        multiple: values[13],
        hjkl_clipboard_get: values[14],
    })
}

// ---------------------------------------------------------------------------
// Backend stub (superseded by X11Thread — kept for cross-platform compile)
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub(crate) struct X11Backend;

impl Backend for X11Backend {
    fn set(&self, _sel: Selection, _mime: MimeType, _bytes: &[u8]) -> Result<(), ClipboardError> {
        unimplemented!("phase 0 scaffold")
    }

    fn get(&self, _sel: Selection, _mime: MimeType) -> Result<Vec<u8>, ClipboardError> {
        unimplemented!("phase 0 scaffold")
    }

    fn clear(&self, _sel: Selection) -> Result<(), ClipboardError> {
        unimplemented!("phase 0 scaffold")
    }

    fn available(&self, _sel: Selection) -> Result<Vec<MimeType>, ClipboardError> {
        unimplemented!("phase 0 scaffold")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that libxcb loads and all symbols resolve.
    ///
    /// Connection + atom-interning + screen-info coverage lives in
    /// `x11_thread::tests::xvfb_connection_and_atoms` so it can share the
    /// process-wide `XVFB_SESSION` and avoid env-mutation races with parallel
    /// tests.
    #[test]
    fn dlopen_smoke() {
        match xcb_fns() {
            Ok(_) => {}
            Err(ClipboardError::LibNotFound) => {
                eprintln!("SKIP dlopen_smoke: libxcb.so.1 not found");
            }
            Err(e) => panic!("unexpected error: {e}"),
        }
    }
}
