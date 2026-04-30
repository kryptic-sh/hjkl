//! X11 clipboard bg thread — selection ownership, set, and clear.
//!
//! Phase 5b: Set + Clear only. Get / Available / INCR land in 5c/5d.
//!
//! One invisible INPUT_OUTPUT window is created per process. The thread
//! services two kinds of work:
//!   1. Inbox messages  (Set / Clear) via mpsc.
//!   2. X server events (SELECTION_REQUEST / SELECTION_CLEAR) via xcb_poll.
//!
//! The event loop uses `recv_timeout(50 ms)` + `xcb_poll_for_event` — no
//! self-pipe needed. 50 ms is acceptable latency for clipboard semantics.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Arc, Condvar, Mutex, OnceLock, mpsc};

use crate::{ClipboardError, MimeType, Selection};

use super::x11::{Atoms, X11Connection};

// ---------------------------------------------------------------------------
// XCB protocol constants
// ---------------------------------------------------------------------------

const XCB_WINDOW_CLASS_INPUT_OUTPUT: u16 = 1;
const XCB_PROP_MODE_REPLACE: u8 = 0;
const XCB_SELECTION_CLEAR: u8 = 29;
const XCB_SELECTION_REQUEST: u8 = 30;
const XCB_SELECTION_NOTIFY: u8 = 31;
const XCB_NONE: u32 = 0;
const XCB_CURRENT_TIME: u32 = 0;
/// Predefined atom ATOM (type for a list of atoms) = 4.
const XCB_ATOM_ATOM: u32 = 4;

// ---------------------------------------------------------------------------
// Wire layouts for events we parse
// ---------------------------------------------------------------------------

/// xcb_selection_request_event_t (32 bytes, host byte order).
#[derive(Clone, Copy)]
#[repr(C)]
struct SelectionRequestEvent {
    response_type: u8,
    pad0: u8,
    sequence: u16,
    time: u32,
    owner: u32,
    requestor: u32,
    selection: u32,
    target: u32,
    /// Property atom named by the requestor; XCB_NONE if not specified.
    property: u32,
}

/// xcb_selection_clear_event_t (32 bytes, host byte order).
#[derive(Clone, Copy)]
#[repr(C)]
struct SelectionClearEvent {
    response_type: u8,
    pad0: u8,
    sequence: u16,
    time: u32,
    owner: u32,
    selection: u32,
}

// ---------------------------------------------------------------------------
// Per-selection ownership data
// ---------------------------------------------------------------------------

struct OwnedData {
    /// mime atom -> payload bytes; one selection can own multiple mime types.
    payloads: HashMap<u32, Vec<u8>>,
    /// ordered list of mime atoms (insertion order preserved for TARGETS reply).
    targets: Vec<u32>,
}

// ---------------------------------------------------------------------------
// Thread-internal state
// ---------------------------------------------------------------------------

struct X11State {
    conn: X11Connection,
    /// Invisible window that holds selection ownership.
    window: u32,
    /// per-selection data: key = selection atom (CLIPBOARD or PRIMARY).
    owned: HashMap<u32, OwnedData>,
}

// ---------------------------------------------------------------------------
// Op / Request types
// ---------------------------------------------------------------------------

/// Operations the X11 thread can handle (phase 5b: Set + Clear).
pub(crate) enum X11Op {
    Set {
        sel_atom: u32,
        mime_atom: u32,
        bytes: Vec<u8>,
    },
    Clear {
        sel_atom: u32,
    },
}

/// Per-op reply payload.
pub(crate) enum X11OpResult {
    Set(Result<(), ClipboardError>),
    Clear(Result<(), ClipboardError>),
}

/// Envelope sent to the X11 thread inbox.
pub(crate) struct X11Request {
    pub op: X11Op,
    pub reply: crate::reply::Reply<X11OpResult>,
}

// ---------------------------------------------------------------------------
// X11Thread public handle
// ---------------------------------------------------------------------------

/// Handle to the singleton X11 bg thread.
pub(crate) struct X11Thread {
    tx: mpsc::Sender<X11Request>,
    /// Pre-interned atoms — stable for process lifetime.
    pub(crate) atoms: Atoms,
}

impl X11Thread {
    fn new() -> Result<Self, ClipboardError> {
        // Open connection on calling thread to report errors synchronously.
        let conn = X11Connection::open()?;

        // Copy atoms (Atoms: Copy) before moving conn into the thread.
        let atoms = *conn.atoms();

        let (tx, rx) = mpsc::channel::<X11Request>();

        std::thread::Builder::new()
            .name("hjkl-clipboard-x11".into())
            .spawn(move || {
                let window = match create_selection_window(&conn) {
                    Ok(w) => w,
                    Err(e) => {
                        eprintln!("hjkl-clipboard x11 thread: window creation failed: {e}");
                        return;
                    }
                };
                let mut state = X11State {
                    conn,
                    window,
                    owned: HashMap::new(),
                };
                run_loop(&mut state, rx);
            })
            .expect("failed to spawn X11 bg thread");

        Ok(Self { tx, atoms })
    }

    /// Send an op and block until the bg thread replies.
    pub(crate) fn send_sync(&self, op: X11Op) -> Result<X11OpResult, ClipboardError> {
        let pair = Arc::new((Mutex::new(None::<X11OpResult>), Condvar::new()));
        let reply = crate::reply::Reply::Sync(Arc::clone(&pair));

        self.tx
            .send(X11Request { op, reply })
            .map_err(|_| ClipboardError::Io(std::io::Error::other("x11 thread inbox closed")))?;

        let (lock, cvar) = &*pair;
        let mut guard = lock.lock().unwrap();
        while guard.is_none() {
            guard = cvar.wait(guard).unwrap();
        }
        Ok(guard.take().unwrap())
    }
}

// ---------------------------------------------------------------------------
// Singleton accessor
// ---------------------------------------------------------------------------

// The OnceLock stores Result<X11Thread, String> so the value is Sync.
// ClipboardError is not Clone, so we serialize it as a String error message.
static X11_THREAD: OnceLock<Result<X11Thread, String>> = OnceLock::new();

/// Return the process-global X11 thread, or an error if X11 is unavailable.
pub(crate) fn x11_thread() -> Result<&'static X11Thread, ClipboardError> {
    X11_THREAD
        .get_or_init(|| X11Thread::new().map_err(|e| e.to_string()))
        .as_ref()
        .map_err(|s| ClipboardError::Io(std::io::Error::other(s.as_str())))
}

// ---------------------------------------------------------------------------
// Window creation
// ---------------------------------------------------------------------------

fn create_selection_window(conn: &X11Connection) -> Result<u32, ClipboardError> {
    let fns = conn.fns();
    let raw = conn.raw();
    let screen = conn.screen();

    // SAFETY: xcb_generate_id returns a fresh XID from the server's pool.
    let wid = unsafe { (fns.xcb_generate_id)(raw) };

    // Create a 1x1 INPUT_OUTPUT window (never mapped — invisible by default).
    // We need INPUT_OUTPUT (not INPUT_ONLY) because XCB requires a matching
    // visual + depth for INPUT_OUTPUT. value_mask=0: no extra attributes needed.
    // SAFETY: all parameters are valid; conn is live on this thread.
    unsafe {
        (fns.xcb_create_window)(
            raw,
            screen.root_depth,
            wid,
            screen.root,
            0, // x
            0, // y
            1, // width
            1, // height
            0, // border_width
            XCB_WINDOW_CLASS_INPUT_OUTPUT,
            screen.root_visual,
            0,                          // value_mask (no extra attributes)
            std::ptr::null::<c_void>(), // value_list
        )
    };

    // Flush so the server processes the create before we proceed.
    // SAFETY: conn is live.
    unsafe { (fns.xcb_flush)(raw) };

    Ok(wid)
}

// ---------------------------------------------------------------------------
// Main event loop
// ---------------------------------------------------------------------------

fn run_loop(state: &mut X11State, rx: mpsc::Receiver<X11Request>) {
    loop {
        // Drain any pending X events before blocking on the inbox.
        drain_events(state);

        match rx.recv_timeout(std::time::Duration::from_millis(50)) {
            Ok(req) => handle_op(state, req),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

// ---------------------------------------------------------------------------
// X event handling
// ---------------------------------------------------------------------------

fn drain_events(state: &mut X11State) {
    let fns = state.conn.fns();
    let raw = state.conn.raw();

    loop {
        // SAFETY: xcb_poll_for_event returns null when the event queue is empty.
        let ev = unsafe { (fns.xcb_poll_for_event)(raw) };
        if ev.is_null() {
            break;
        }

        // High bit is set on synthetic events; mask it to get the real type.
        // SAFETY: ev is non-null; first byte is response_type.
        let response_type = unsafe { *(ev as *const u8) } & 0x7f;

        match response_type {
            XCB_SELECTION_REQUEST => {
                // SAFETY: ev is a valid SelectionRequest event (32 bytes).
                let req = unsafe { *(ev as *const SelectionRequestEvent) };
                handle_selection_request(state, &req);
            }
            XCB_SELECTION_CLEAR => {
                // SAFETY: ev is a valid SelectionClear event (32 bytes).
                let clr = unsafe { *(ev as *const SelectionClearEvent) };
                // Another client has taken the selection — drop our data.
                state.owned.remove(&clr.selection);
            }
            _ => {
                // Ignore events we don't handle.
            }
        }

        // SAFETY: ev was heap-allocated by xcb (via malloc); free it.
        unsafe { libc::free(ev.cast()) };
    }
}

fn handle_selection_request(state: &mut X11State, ev: &SelectionRequestEvent) {
    let fns = state.conn.fns();
    let raw = state.conn.raw();
    let atoms = state.conn.atoms();

    // If requestor didn't name a property, fall back to the target atom (old
    // ICCCM compatibility).
    let property = if ev.property == XCB_NONE {
        ev.target
    } else {
        ev.property
    };

    let owned = state.owned.get(&ev.selection);

    let reply_property = if ev.target == atoms.targets {
        // TARGETS request — reply with our list of offered atoms.
        if let Some(data) = owned {
            let mut list: Vec<u32> = Vec::with_capacity(data.targets.len() + 2);
            // Always include TARGETS and MULTIPLE in the TARGETS list so
            // compliant clients know we understand the protocol.
            list.push(atoms.targets);
            list.push(atoms.multiple);
            list.extend_from_slice(&data.targets);

            // SAFETY: list.as_ptr() is valid for list.len() u32 values.
            // format=32 means each item is a 32-bit atom.
            unsafe {
                (fns.xcb_change_property)(
                    raw,
                    XCB_PROP_MODE_REPLACE,
                    ev.requestor,
                    property,
                    XCB_ATOM_ATOM,
                    32,
                    list.len() as u32,
                    list.as_ptr().cast::<c_void>(),
                );
            }
            property
        } else {
            XCB_NONE
        }
    } else if let Some(data) = owned {
        if let Some(payload) = data.payloads.get(&ev.target) {
            // SAFETY: payload.as_ptr() is valid for payload.len() bytes.
            // format=8 means raw bytes.
            unsafe {
                (fns.xcb_change_property)(
                    raw,
                    XCB_PROP_MODE_REPLACE,
                    ev.requestor,
                    property,
                    ev.target,
                    8,
                    payload.len() as u32,
                    payload.as_ptr().cast::<c_void>(),
                );
            }
            property
        } else {
            // We own the selection but not this specific target.
            XCB_NONE
        }
    } else {
        // We don't own this selection at all.
        XCB_NONE
    };

    send_selection_notify(state, ev, reply_property);
}

/// Send XCB_SELECTION_NOTIFY to the requestor.
fn send_selection_notify(state: &mut X11State, req: &SelectionRequestEvent, property: u32) {
    let fns = state.conn.fns();
    let raw = state.conn.raw();

    // xcb_selection_notify_event_t layout (32 bytes):
    //   [0]      response_type (31)
    //   [1]      pad0
    //   [2..4]   sequence (XCB fills this)
    //   [4..8]   time
    //   [8..12]  requestor
    //   [12..16] selection
    //   [16..20] target
    //   [20..24] property (XCB_NONE = refused)
    //   [24..32] padding
    let mut buf = [0u8; 32];
    buf[0] = XCB_SELECTION_NOTIFY;
    buf[4..8].copy_from_slice(&req.time.to_ne_bytes());
    buf[8..12].copy_from_slice(&req.requestor.to_ne_bytes());
    buf[12..16].copy_from_slice(&req.selection.to_ne_bytes());
    buf[16..20].copy_from_slice(&req.target.to_ne_bytes());
    buf[20..24].copy_from_slice(&property.to_ne_bytes());

    // SAFETY: buf is a valid 32-byte event buffer; propagate=0, mask=0 is the
    // correct way to send SelectionNotify directly to the requestor window.
    unsafe {
        (fns.xcb_send_event)(
            raw,
            0, // propagate = false
            req.requestor,
            0, // event_mask = 0
            buf.as_ptr().cast(),
        );
    }

    // Flush immediately so the notify reaches the requestor.
    // SAFETY: conn is live on this thread.
    unsafe { (fns.xcb_flush)(raw) };
}

// ---------------------------------------------------------------------------
// Op handlers
// ---------------------------------------------------------------------------

fn handle_op(state: &mut X11State, req: X11Request) {
    let result = match req.op {
        X11Op::Set {
            sel_atom,
            mime_atom,
            bytes,
        } => X11OpResult::Set(do_set(state, sel_atom, mime_atom, bytes)),
        X11Op::Clear { sel_atom } => X11OpResult::Clear(do_clear(state, sel_atom)),
    };
    req.reply.resolve(result);
}

fn do_set(
    state: &mut X11State,
    sel_atom: u32,
    mime_atom: u32,
    bytes: Vec<u8>,
) -> Result<(), ClipboardError> {
    let max = state.conn.screen().max_request_len_bytes.saturating_sub(24); // XCB request-header overhead (24 bytes)

    if bytes.len() > max as usize {
        return Err(ClipboardError::PayloadTooLarge);
    }

    let fns = state.conn.fns();
    let raw = state.conn.raw();
    let window = state.window;

    // Claim selection ownership with XCB_CURRENT_TIME so the server assigns
    // a monotonically increasing timestamp.
    // SAFETY: all args are valid; conn is live on this thread.
    unsafe {
        (fns.xcb_set_selection_owner)(raw, window, sel_atom, XCB_CURRENT_TIME);
    }
    // Flush before reading back the owner so the server has processed our claim.
    // SAFETY: conn is live.
    unsafe { (fns.xcb_flush)(raw) };

    // Verify we actually hold the selection (another client may have beaten us).
    let cookie = unsafe { (fns.xcb_get_selection_owner)(raw, sel_atom) };
    // SAFETY: reply must be freed with libc::free.
    let reply = unsafe { (fns.xcb_get_selection_owner_reply)(raw, cookie, std::ptr::null_mut()) };
    if reply.is_null() {
        return Err(ClipboardError::Io(std::io::Error::other(
            "xcb_get_selection_owner_reply returned null",
        )));
    }
    // SAFETY: reply is non-null; `owner` is at offset 8.
    let owner = unsafe { (*reply).owner };
    // SAFETY: reply was malloc'd by xcb.
    unsafe { libc::free(reply.cast()) };

    if owner != window {
        return Err(ClipboardError::Io(std::io::Error::other(
            "another client holds the selection",
        )));
    }

    // Store the payload in our in-memory table.
    let data = state.owned.entry(sel_atom).or_insert_with(|| OwnedData {
        payloads: HashMap::new(),
        targets: Vec::new(),
    });
    if !data.targets.contains(&mime_atom) {
        data.targets.push(mime_atom);
    }
    data.payloads.insert(mime_atom, bytes);

    Ok(())
}

fn do_clear(state: &mut X11State, sel_atom: u32) -> Result<(), ClipboardError> {
    let fns = state.conn.fns();
    let raw = state.conn.raw();

    // Relinquish ownership by setting owner = XCB_NONE.
    // SAFETY: standard XCB call; conn is live on this thread.
    unsafe {
        (fns.xcb_set_selection_owner)(raw, XCB_NONE, sel_atom, XCB_CURRENT_TIME);
    }
    // SAFETY: conn is live.
    unsafe { (fns.xcb_flush)(raw) };

    state.owned.remove(&sel_atom);
    Ok(())
}

// ---------------------------------------------------------------------------
// MimeType -> atom mapping
// ---------------------------------------------------------------------------

/// Map a known MimeType to its pre-interned atom.
///
/// Returns `None` for `Custom` (requires live intern) and for unknown future
/// variants.
pub(crate) fn mime_to_atom_static(atoms: &Atoms, mime: &MimeType) -> Option<u32> {
    match mime {
        MimeType::Text => Some(atoms.utf8_string),
        MimeType::Html => Some(atoms.text_html),
        MimeType::Rtf => Some(atoms.text_rtf),
        MimeType::UriList => Some(atoms.text_uri_list),
        MimeType::Png => Some(atoms.image_png),
        MimeType::Custom(_) => None,
        #[allow(unreachable_patterns)]
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Public helpers for lib.rs wiring
// ---------------------------------------------------------------------------

/// Set a clipboard payload via the X11 thread.
///
/// `Custom` mime types are not supported in 5b (no live intern path); they
/// return `UnsupportedMime` and will be wired in 5c.
pub(crate) fn set_clipboard(
    thread: &X11Thread,
    sel: Selection,
    mime: &MimeType,
    bytes: &[u8],
) -> Result<(), ClipboardError> {
    let mime_atom =
        mime_to_atom_static(&thread.atoms, mime).ok_or(ClipboardError::UnsupportedMime)?;

    let sel_atom = sel_to_atom(&thread.atoms, sel);

    let result = thread.send_sync(X11Op::Set {
        sel_atom,
        mime_atom,
        bytes: bytes.to_vec(),
    })?;
    match result {
        X11OpResult::Set(r) => r,
        _ => unreachable!(),
    }
}

/// Clear a selection via the X11 thread.
pub(crate) fn clear_clipboard(thread: &X11Thread, sel: Selection) -> Result<(), ClipboardError> {
    let sel_atom = sel_to_atom(&thread.atoms, sel);
    let result = thread.send_sync(X11Op::Clear { sel_atom })?;
    match result {
        X11OpResult::Clear(r) => r,
        _ => unreachable!(),
    }
}

fn sel_to_atom(atoms: &Atoms, sel: Selection) -> u32 {
    match sel {
        Selection::Clipboard => atoms.clipboard,
        Selection::Primary => atoms.primary,
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
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    // One Xvfb session for all X11 thread tests (approach b from the spec).
    // Spawned once; leaks for process lifetime.
    static XVFB_SESSION: OnceLock<Option<XvfbSession>> = OnceLock::new();

    // Serializes all X11 thread tests — they share a singleton connection.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    struct XvfbSession {
        _child: Child,
        display: String,
    }

    fn ensure_xvfb() -> Option<&'static XvfbSession> {
        XVFB_SESSION
            .get_or_init(|| {
                let xvfb_path = Path::new("/usr/bin/Xvfb");
                if !xvfb_path.exists() {
                    eprintln!("SKIP: Xvfb not found");
                    return None;
                }

                // Use display :98 to avoid conflicting with x11.rs tests (:99).
                let child = match Command::new(xvfb_path)
                    .args([":98", "-screen", "0", "800x600x24", "-ac"])
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

                let socket = "/tmp/.X11-unix/X98";
                let deadline = Instant::now() + Duration::from_secs(5);
                while Instant::now() < deadline {
                    if UnixStream::connect(socket).is_ok() {
                        // Set DISPLAY then eagerly initialize X11_THREAD while
                        // still inside this OnceLock callback so no other test
                        // thread can race between "DISPLAY is set" and "X11_THREAD
                        // is initialized".
                        // SAFETY: test-only; we are the only writer at this point
                        // (XVFB_SESSION OnceLock ensures single-threaded init).
                        unsafe { std::env::set_var("DISPLAY", ":98") };
                        // Warm up X11_THREAD now — if it fails we skip.
                        let _ = x11_thread();
                        return Some(XvfbSession {
                            _child: child,
                            display: ":98".into(),
                        });
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }

                eprintln!("SKIP: Xvfb socket did not become connectable within 5 s");
                None
            })
            .as_ref()
    }

    /// Run xclip against the test display. Returns stdout on success.
    fn xclip(args: &[&str]) -> Option<Vec<u8>> {
        if !Path::new("/usr/bin/xclip").exists() {
            return None;
        }
        let session = ensure_xvfb()?;
        let output = Command::new("/usr/bin/xclip")
            .args(args)
            .env("DISPLAY", &session.display)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        Some(output.stdout)
    }

    fn xclip_clipboard() -> Option<Vec<u8>> {
        xclip(&["-selection", "clipboard", "-o"])
    }

    fn xclip_primary() -> Option<Vec<u8>> {
        xclip(&["-selection", "primary", "-o"])
    }

    fn xclip_typed(sel: &str, mime: &str) -> Option<Vec<u8>> {
        xclip(&["-selection", sel, "-t", mime, "-o"])
    }

    /// Initialize Xvfb + get the singleton X11Thread for tests.
    fn get_thread() -> Option<&'static X11Thread> {
        ensure_xvfb()?;
        match x11_thread() {
            Ok(t) => Some(t),
            Err(e) => {
                eprintln!("SKIP: x11_thread init failed: {e}");
                None
            }
        }
    }

    // -----------------------------------------------------------------------

    #[test]
    fn set_clear_clipboard_text() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let thread = match get_thread() {
            Some(t) => t,
            None => return,
        };

        let text = b"hello-x11-5b";
        set_clipboard(thread, Selection::Clipboard, &MimeType::Text, text)
            .expect("set_clipboard failed");

        // Give the X11 thread time to process the ownership claim and respond
        // to any queued SELECTION_REQUEST events from xclip.
        std::thread::sleep(Duration::from_millis(150));

        let out = match xclip_clipboard() {
            Some(o) => o,
            None => {
                eprintln!("SKIP set_clear_clipboard_text: xclip not available");
                return;
            }
        };
        assert_eq!(out, text, "xclip clipboard read mismatch");

        clear_clipboard(thread, Selection::Clipboard).expect("clear_clipboard failed");

        std::thread::sleep(Duration::from_millis(150));

        // After clear the selection has no owner; xclip returns empty output.
        let after = xclip_clipboard().unwrap_or_default();
        assert!(
            after.is_empty(),
            "expected empty after clear, got: {after:?}"
        );
    }

    #[test]
    fn set_primary_text() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let thread = match get_thread() {
            Some(t) => t,
            None => return,
        };

        let text = b"primary-selection-5b";
        set_clipboard(thread, Selection::Primary, &MimeType::Text, text)
            .expect("set primary failed");

        std::thread::sleep(Duration::from_millis(150));

        let out = match xclip_primary() {
            Some(o) => o,
            None => {
                eprintln!("SKIP set_primary_text: xclip not available");
                return;
            }
        };
        assert_eq!(out, text, "xclip primary read mismatch");
    }

    #[test]
    fn set_html_payload() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let thread = match get_thread() {
            Some(t) => t,
            None => return,
        };

        let html = b"<b>hello</b>";
        set_clipboard(thread, Selection::Clipboard, &MimeType::Html, html)
            .expect("set html failed");

        std::thread::sleep(Duration::from_millis(150));

        let out = match xclip_typed("clipboard", "text/html") {
            Some(o) => o,
            None => {
                eprintln!("SKIP set_html_payload: xclip not available");
                return;
            }
        };
        assert_eq!(out, html, "xclip html read mismatch");
    }

    #[test]
    fn payload_too_large_errors() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let thread = match get_thread() {
            Some(t) => t,
            None => return,
        };

        // 17 MiB far exceeds XCB's default ~256 KB max_request_length.
        let big = vec![0u8; 17 * 1024 * 1024];
        let err = set_clipboard(thread, Selection::Clipboard, &MimeType::Text, &big)
            .expect_err("expected PayloadTooLarge");
        assert!(
            matches!(err, ClipboardError::PayloadTooLarge),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn set_replaces_previous() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let thread = match get_thread() {
            Some(t) => t,
            None => return,
        };

        set_clipboard(thread, Selection::Clipboard, &MimeType::Text, b"hello")
            .expect("first set failed");
        std::thread::sleep(Duration::from_millis(150));

        set_clipboard(thread, Selection::Clipboard, &MimeType::Text, b"world")
            .expect("second set failed");
        std::thread::sleep(Duration::from_millis(150));

        let out = match xclip_clipboard() {
            Some(o) => o,
            None => {
                eprintln!("SKIP set_replaces_previous: xclip not available");
                return;
            }
        };
        assert_eq!(out, b"world", "expected 'world' after replace");
    }
}
