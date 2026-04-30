//! Linux X11 clipboard backend via libxcb (`dlopen`).
//!
//! Runs a singleton background thread that owns an invisible window, handles
//! `SelectionRequest` events, and auto-`SAVE_TARGETS` after every set.
//!
//! Phase 5a: connection + atom interning only. Clipboard ops are
//! `unimplemented!()` until 5b/5c/5d.

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
    /// Screen width in pixels.
    pub width: u16,
    /// Screen height in pixels.
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
            return Err(ClipboardError::Io(std::io::Error::other(
                "xcb_connect returned null",
            )));
        }

        // SAFETY: conn is a non-null pointer returned by xcb_connect.
        let err = unsafe { (fns.xcb_connection_has_error)(conn) };
        if err != 0 {
            // SAFETY: conn is owned by us; disconnect on error path.
            unsafe { (fns.xcb_disconnect)(conn) };
            return Err(ClipboardError::Io(std::io::Error::other(format!(
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
            return Err(ClipboardError::Io(std::io::Error::other(
                "xcb_get_setup returned null",
            )));
        }

        // SAFETY: setup is non-null and valid. The iterator's `data` field
        // points to the first screen; we check it below.
        let iter = unsafe { (fns.xcb_setup_roots_iterator)(setup) };
        if iter.data.is_null() {
            unsafe { (fns.xcb_disconnect)(conn) };
            return Err(ClipboardError::Io(std::io::Error::other(
                "no screens in XCB setup",
            )));
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
        let max_request_len_bytes = u32::from(max_req_units) * 4;

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
            return Err(ClipboardError::Io(std::io::Error::other(format!(
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
// Backend stub (clipboard ops wired in 5b/5c/5d)
// ---------------------------------------------------------------------------

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
    use std::io;
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    // -----------------------------------------------------------------------
    // Xvfb guard — ensures the server is torn down after each test.
    // -----------------------------------------------------------------------

    struct XvfbGuard {
        child: Child,
        display: String,
    }

    impl Drop for XvfbGuard {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    /// Spawn Xvfb on `:99` and wait up to 5 s for the socket to appear.
    ///
    /// Returns `None` if Xvfb is not installed or fails to start.
    fn spawn_xvfb() -> Option<XvfbGuard> {
        // Check Xvfb is available before trying to spawn.
        let xvfb_path = Path::new("/usr/bin/Xvfb");
        if !xvfb_path.exists() {
            eprintln!("SKIP: Xvfb not found at {}", xvfb_path.display());
            return None;
        }

        let child = match Command::new(xvfb_path)
            // -ac disables host-based access control for the test server so
            // xcb_connect succeeds without ~/.Xauthority on the test machine.
            .args([":99", "-screen", "0", "800x600x24", "-ac"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                eprintln!("SKIP: Xvfb not found: {e}");
                return None;
            }
            Err(e) => {
                eprintln!("SKIP: failed to spawn Xvfb: {e}");
                return None;
            }
        };

        // Poll until we can actually connect to the Unix socket — the socket
        // file appears slightly before Xvfb is ready to accept connections.
        let socket_path = "/tmp/.X11-unix/X99";
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if UnixStream::connect(socket_path).is_ok() {
                return Some(XvfbGuard {
                    child,
                    display: ":99".into(),
                });
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        eprintln!("SKIP: Xvfb socket did not become connectable within 5 s");
        None
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    /// Verify that libxcb loads and all symbols resolve.
    #[test]
    fn dlopen_smoke() {
        match xcb_fns() {
            Ok(_) => {} // all symbols resolved
            Err(ClipboardError::LibNotFound) => {
                eprintln!("SKIP dlopen_smoke: libxcb.so.1 not found");
            }
            Err(e) => panic!("unexpected error: {e}"),
        }
    }

    /// Spin up Xvfb, connect, intern atoms, check screen dimensions.
    #[test]
    fn xvfb_connection_and_atoms() {
        let guard = match spawn_xvfb() {
            Some(g) => g,
            None => return,
        };

        // Point DISPLAY at our Xvfb instance for the duration of this test.
        // SAFETY: we are the only thread that touches DISPLAY in these tests.
        // Cargo test parallelism is within-process but each test has its own
        // stack; mutating env vars is UB only under true concurrent access.
        // Using display :99 (reserved for test use) prevents collisions with
        // any real compositor.
        let prev_display = std::env::var("DISPLAY").ok();
        // SAFETY: see comment above.
        unsafe { std::env::set_var("DISPLAY", &guard.display) };

        let result = X11Connection::open();

        // Restore DISPLAY before asserting so teardown is clean on failure.
        // SAFETY: see comment above.
        match &prev_display {
            Some(d) => unsafe { std::env::set_var("DISPLAY", d) },
            None => unsafe { std::env::remove_var("DISPLAY") },
        }

        let conn = match result {
            Ok(c) => c,
            Err(ClipboardError::LibNotFound) => {
                eprintln!("SKIP xvfb_connection_and_atoms: libxcb.so.1 not found");
                return;
            }
            Err(e) => panic!("X11Connection::open failed: {e}"),
        };

        // Screen dimensions must match what we passed to Xvfb.
        assert_eq!(conn.screen.width, 800, "screen width mismatch");
        assert_eq!(conn.screen.height, 600, "screen height mismatch");
        assert_ne!(conn.screen.root, 0, "root window must be non-zero");
        assert_ne!(conn.screen.root_visual, 0, "root visual must be non-zero");
        assert!(
            conn.screen.max_request_len_bytes > 0,
            "max_request_len_bytes must be > 0"
        );

        // All 15 atoms must be non-zero XCB atoms.
        let a = &conn.atoms;
        for (val, name) in [
            (a.clipboard, "CLIPBOARD"),
            (a.primary, "PRIMARY"),
            (a.targets, "TARGETS"),
            (a.utf8_string, "UTF8_STRING"),
            (a.string, "STRING"),
            (a.text_plain_utf8, "text/plain;charset=utf-8"),
            (a.text_html, "text/html"),
            (a.text_rtf, "text/rtf"),
            (a.text_uri_list, "text/uri-list"),
            (a.image_png, "image/png"),
            (a.incr, "INCR"),
            (a.clipboard_manager, "CLIPBOARD_MANAGER"),
            (a.save_targets, "SAVE_TARGETS"),
            (a.multiple, "MULTIPLE"),
            (a.hjkl_clipboard_get, "HJKL_CLIPBOARD_GET"),
        ] {
            assert_ne!(val, 0, "atom {name} must be non-zero");
        }

        // XvfbGuard::drop kills Xvfb.
        drop(guard);
    }
}
