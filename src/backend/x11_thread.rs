//! X11 clipboard bg thread — selection ownership, set, clear, get, available.
//!
//! Phase 5c: adds Get + Available on top of 5b's Set + Clear.
//!
//! One invisible INPUT_OUTPUT window is created per process. The thread
//! services two kinds of work:
//!   1. Inbox messages  (Set / Clear / Get / Available) via mpsc.
//!   2. X server events (SELECTION_REQUEST / SELECTION_CLEAR / PROPERTY_NOTIFY)
//!      via xcb_poll.
//!
//! The event loop uses `recv_timeout(50 ms)` + `xcb_poll_for_event` — no
//! self-pipe needed. 50 ms is acceptable latency for clipboard semantics.
//!
//! For Get operations the handler drives an inner poll loop (bounded by
//! timeout) that dispatches SELECTION_REQUEST events while waiting for
//! SELECTION_NOTIFY, avoiding deadlock when we own the selection we are
//! reading.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Arc, Condvar, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

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
const XCB_PROPERTY_NOTIFY: u8 = 28;
const XCB_PROPERTY_NEW_VALUE: u8 = 0;
const XCB_NONE: u32 = 0;
const XCB_CURRENT_TIME: u32 = 0;
/// Predefined atom ATOM (type for a list of atoms) = 4.
const XCB_ATOM_ATOM: u32 = 4;
/// CW_EVENT_MASK attribute bit for xcb_create_window value_mask.
const XCB_CW_EVENT_MASK: u32 = 0x800;
/// Subscribe to PropertyChange events on our window so INCR receive works.
const XCB_EVENT_MASK_PROPERTY_CHANGE: u32 = 0x0040_0000;
/// AnyPropertyType — pass as `type` to xcb_get_property to accept any type.
const XCB_GET_PROPERTY_TYPE_ANY: u32 = 0;

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

/// xcb_selection_notify_event_t (32 bytes, host byte order).
#[derive(Clone, Copy)]
#[repr(C)]
struct SelectionNotifyEvent {
    response_type: u8,
    pad0: u8,
    sequence: u16,
    time: u32,
    requestor: u32,
    selection: u32,
    target: u32,
    property: u32,
}

/// xcb_property_notify_event_t (32 bytes, host byte order).
#[derive(Clone, Copy)]
#[repr(C)]
struct PropertyNotifyEvent {
    response_type: u8,
    pad0: u8,
    sequence: u16,
    window: u32,
    atom: u32,
    time: u32,
    state: u8,
    pad1: [u8; 3],
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

/// Operations the X11 thread can handle.
pub(crate) enum X11Op {
    Set {
        sel_atom: u32,
        mime_atom: u32,
        bytes: Vec<u8>,
    },
    Clear {
        sel_atom: u32,
    },
    Get {
        sel_atom: u32,
        mime_atom: u32,
    },
    Available {
        sel_atom: u32,
    },
}

/// Per-op reply payload.
pub(crate) enum X11OpResult {
    Set(Result<(), ClipboardError>),
    Clear(Result<(), ClipboardError>),
    Get(Result<Vec<u8>, ClipboardError>),
    /// Raw atoms; lib.rs maps to MimeType via atom_to_mime.
    Available(Result<Vec<u32>, ClipboardError>),
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
    // We set XCB_CW_EVENT_MASK with XCB_EVENT_MASK_PROPERTY_CHANGE so that
    // PROPERTY_NOTIFY events are delivered to us during INCR receives.
    // SELECTION_REQUEST/CLEAR events arrive regardless of event mask.
    let value_mask: u32 = XCB_CW_EVENT_MASK;
    let value_list: [u32; 1] = [XCB_EVENT_MASK_PROPERTY_CHANGE];

    // SAFETY: all parameters are valid; conn is live on this thread.
    // value_list is a packed u32 array indexed by set bits in value_mask.
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
            value_mask,
            value_list.as_ptr().cast::<c_void>(),
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
        drain_events(state, DrainGoal::AnyEvent);

        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(req) => handle_op(state, req),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

// ---------------------------------------------------------------------------
// X event handling
// ---------------------------------------------------------------------------

/// What drain_events is looking for in the event stream.
enum DrainGoal {
    /// Just drain all pending events (normal run_loop pass).
    AnyEvent,
    /// Looking for SELECTION_NOTIFY on our private get-property.
    SelectionNotify { our_property: u32 },
    /// Looking for PROPERTY_NOTIFY (new value) on our private get-property.
    PropertyNotify { our_property: u32, our_window: u32 },
}

/// Result from drain_events when a goal is set.
enum DrainResult {
    /// Did not see the target event; caller should retry.
    NotFound,
    /// Saw SELECTION_NOTIFY; property field indicates owner's reply.
    SelectionNotifySeen { property: u32 },
    /// Saw PROPERTY_NOTIFY (new value); caller should read the property.
    PropertyNotifySeen,
}

/// Drain all pending X events, dispatching SELECTION_REQUEST/CLEAR as they
/// arrive.  When a goal is set, returns as soon as the target event is seen.
fn drain_events(state: &mut X11State, goal: DrainGoal) -> DrainResult {
    let fns = state.conn.fns();
    let raw = state.conn.raw();

    loop {
        // SAFETY: xcb_poll_for_event returns null when the event queue is empty.
        let ev = unsafe { (fns.xcb_poll_for_event)(raw) };
        if ev.is_null() {
            return DrainResult::NotFound;
        }

        // High bit is set on synthetic events; mask it to get the real type.
        // SAFETY: ev is non-null; first byte is response_type.
        let response_type = unsafe { *(ev as *const u8) } & 0x7f;

        let result = match response_type {
            XCB_SELECTION_REQUEST => {
                // SAFETY: ev is a valid SelectionRequest event (32 bytes).
                let req = unsafe { *(ev as *const SelectionRequestEvent) };
                handle_selection_request(state, &req);
                DrainResult::NotFound
            }
            XCB_SELECTION_CLEAR => {
                // SAFETY: ev is a valid SelectionClear event (32 bytes).
                let clr = unsafe { *(ev as *const SelectionClearEvent) };
                // Another client has taken the selection — drop our data.
                state.owned.remove(&clr.selection);
                DrainResult::NotFound
            }
            XCB_SELECTION_NOTIFY => {
                // SAFETY: ev is a valid SelectionNotify event (32 bytes).
                let notify = unsafe { *(ev as *const SelectionNotifyEvent) };
                match &goal {
                    DrainGoal::SelectionNotify { our_property }
                        if notify.requestor == state.window && notify.property == *our_property =>
                    {
                        // SAFETY: ev was heap-allocated by xcb (via malloc); free it.
                        unsafe { libc::free(ev.cast()) };
                        return DrainResult::SelectionNotifySeen {
                            property: notify.property,
                        };
                    }
                    DrainGoal::SelectionNotify { .. } if notify.requestor == state.window => {
                        // Refusal: owner set property = XCB_NONE.
                        // SAFETY: ev was heap-allocated by xcb.
                        unsafe { libc::free(ev.cast()) };
                        return DrainResult::SelectionNotifySeen { property: XCB_NONE };
                    }
                    _ => {
                        // Not our notify or not looking for one; ignore.
                        DrainResult::NotFound
                    }
                }
            }
            XCB_PROPERTY_NOTIFY => {
                // SAFETY: ev is a valid PropertyNotify event (32 bytes).
                let pn = unsafe { *(ev as *const PropertyNotifyEvent) };
                match &goal {
                    DrainGoal::PropertyNotify {
                        our_property,
                        our_window,
                    } if pn.window == *our_window
                        && pn.atom == *our_property
                        && pn.state == XCB_PROPERTY_NEW_VALUE =>
                    {
                        // SAFETY: ev was heap-allocated by xcb.
                        unsafe { libc::free(ev.cast()) };
                        return DrainResult::PropertyNotifySeen;
                    }
                    _ => DrainResult::NotFound,
                }
            }
            _ => {
                // Ignore events we don't handle.
                DrainResult::NotFound
            }
        };

        // SAFETY: ev was heap-allocated by xcb (via malloc); free it.
        unsafe { libc::free(ev.cast()) };

        // Only return early for found-goal; otherwise keep draining.
        if !matches!(result, DrainResult::NotFound) {
            return result;
        }
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
        X11Op::Get {
            sel_atom,
            mime_atom,
        } => X11OpResult::Get(do_get(state, sel_atom, mime_atom)),
        X11Op::Available { sel_atom } => X11OpResult::Available(do_available(state, sel_atom)),
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
// Read helpers: xcb_get_property round-trip
// ---------------------------------------------------------------------------

/// Read our private property from our window. Returns (type_atom, bytes).
/// delete=1 so the server frees the property after we read it.
fn read_property(state: &X11State) -> Result<(u32, Vec<u8>), ClipboardError> {
    let fns = state.conn.fns();
    let raw = state.conn.raw();
    let window = state.window;
    let our_property = state.conn.atoms().hjkl_clipboard_get;

    // long_length: max u32/4 to request the full property in one shot.
    // For very large data the X server caps at max_request_length; values that
    // exceed it will be delivered via INCR instead, so we don't need to chunk.
    let cookie = unsafe {
        (fns.xcb_get_property)(
            raw,
            1, // delete=1: server frees property after this read
            window,
            our_property,
            XCB_GET_PROPERTY_TYPE_ANY,
            0,            // long_offset
            u32::MAX / 4, // long_length
        )
    };

    // SAFETY: cookie from xcb_get_property; null error pointer (null reply ->
    // error).
    let reply = unsafe { (fns.xcb_get_property_reply)(raw, cookie, std::ptr::null_mut()) };
    if reply.is_null() {
        return Err(ClipboardError::Io(std::io::Error::other(
            "xcb_get_property_reply returned null",
        )));
    }

    // SAFETY: reply is non-null xcb_get_property_reply_t.
    let type_atom = unsafe { (*reply).r#type };
    // SAFETY: reply non-null; xcb_get_property_value returns pointer into
    // reply's trailing data buffer.
    let value_ptr = unsafe { (fns.xcb_get_property_value)(reply) };
    // SAFETY: same.
    let value_len = unsafe { (fns.xcb_get_property_value_length)(reply) } as usize;

    let bytes = if value_len == 0 || value_ptr.is_null() {
        Vec::new()
    } else {
        // SAFETY: value_ptr is valid for value_len bytes inside the reply
        // allocation. We copy immediately before freeing reply.
        unsafe { std::slice::from_raw_parts(value_ptr as *const u8, value_len).to_vec() }
    };

    // SAFETY: reply was malloc'd by xcb.
    unsafe { libc::free(reply.cast()) };

    Ok((type_atom, bytes))
}

// ---------------------------------------------------------------------------
// do_get
// ---------------------------------------------------------------------------

fn do_get(state: &mut X11State, sel_atom: u32, mime_atom: u32) -> Result<Vec<u8>, ClipboardError> {
    let fns = state.conn.fns();
    let raw = state.conn.raw();
    let window = state.window;
    let our_property = state.conn.atoms().hjkl_clipboard_get;
    let incr_atom = state.conn.atoms().incr;

    // Delete any stale data on our private property before issuing the request.
    // SAFETY: standard xcb call; conn live on this thread.
    unsafe { (fns.xcb_delete_property)(raw, window, our_property) };

    // Ask the selection owner to write the desired mime type into our property.
    // SAFETY: all parameters valid; conn live.
    unsafe {
        (fns.xcb_convert_selection)(
            raw,
            window,
            sel_atom,
            mime_atom,
            our_property,
            XCB_CURRENT_TIME,
        );
    }
    // SAFETY: conn live.
    unsafe { (fns.xcb_flush)(raw) };

    // Wait for SELECTION_NOTIFY, dispatching any SELECTION_REQUEST events that
    // arrive while we wait (needed for self-reads where we own the selection).
    // Timeout: 5 seconds.
    let deadline = Instant::now() + Duration::from_secs(5);
    let replied_property = loop {
        if let DrainResult::SelectionNotifySeen { property } =
            drain_events(state, DrainGoal::SelectionNotify { our_property })
        {
            break property;
        }
        if Instant::now() >= deadline {
            return Err(ClipboardError::Io(std::io::Error::other(
                "xcb_convert_selection timed out waiting for SELECTION_NOTIFY",
            )));
        }
        std::thread::sleep(Duration::from_millis(10));
    };

    if replied_property == XCB_NONE {
        // Owner refused (no owner, or unknown target).
        return Err(ClipboardError::UnsupportedMime);
    }

    // Read the property the owner wrote into.
    let (type_atom, bytes) = read_property(state)?;

    if type_atom != incr_atom {
        // Normal (non-INCR) case: all data arrived in one property.
        return Ok(bytes);
    }

    // ---------------------------------------------------------------------------
    // INCR receive sub-protocol
    // ---------------------------------------------------------------------------
    //
    // The initial property contains a u32 total-size hint (informational).
    // We signal readiness by deleting our property, then loop reading chunks
    // as PROPERTY_NOTIFY (new value) events arrive.  A zero-length chunk
    // signals end of transfer.

    let fns = state.conn.fns();
    let raw = state.conn.raw();

    // Delete initial INCR property to signal we are ready to receive chunks.
    // SAFETY: conn live.
    unsafe { (fns.xcb_delete_property)(raw, window, our_property) };
    // SAFETY: conn live.
    unsafe { (fns.xcb_flush)(raw) };

    let mut accumulator: Vec<u8> = Vec::new();
    let total_deadline = Instant::now() + Duration::from_secs(30);

    loop {
        // Wait for PROPERTY_NOTIFY (new value) on our window/property.
        let chunk_deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let DrainResult::PropertyNotifySeen = drain_events(
                state,
                DrainGoal::PropertyNotify {
                    our_property,
                    our_window: window,
                },
            ) {
                break;
            }
            if Instant::now() >= chunk_deadline || Instant::now() >= total_deadline {
                return Err(ClipboardError::Io(std::io::Error::other(
                    "INCR receive timed out waiting for PROPERTY_NOTIFY",
                )));
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        // Read the chunk (delete=1 to ack it to the sender).
        let (_type_atom, chunk) = read_property(state)?;

        if chunk.is_empty() {
            // Zero-length chunk = end of INCR transfer.
            break;
        }

        accumulator.extend_from_slice(&chunk);

        if Instant::now() >= total_deadline {
            return Err(ClipboardError::Io(std::io::Error::other(
                "INCR receive exceeded total timeout",
            )));
        }
    }

    Ok(accumulator)
}

// ---------------------------------------------------------------------------
// do_available
// ---------------------------------------------------------------------------

fn do_available(state: &mut X11State, sel_atom: u32) -> Result<Vec<u32>, ClipboardError> {
    let targets_atom = state.conn.atoms().targets;

    // Use the same read path as do_get, but request TARGETS.
    let data = do_get(state, sel_atom, targets_atom);

    match data {
        Err(ClipboardError::UnsupportedMime) => {
            // No owner or owner refused — return empty list, not an error.
            Ok(vec![])
        }
        Err(e) => Err(e),
        Ok(bytes) => {
            // TARGETS reply is a list of u32 atoms (format=32).
            // Each atom is 4 bytes in native byte order.
            if bytes.len() % 4 != 0 {
                return Err(ClipboardError::Io(std::io::Error::other(
                    "TARGETS reply has non-multiple-of-4 byte length",
                )));
            }
            let atoms: Vec<u32> = bytes
                .chunks_exact(4)
                .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
                .collect();
            Ok(atoms)
        }
    }
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

/// Map a raw atom back to a MimeType, returning None for unknown atoms.
pub(crate) fn atom_to_mime(atoms: &Atoms, atom: u32) -> Option<MimeType> {
    if atom == atoms.utf8_string || atom == atoms.text_plain_utf8 || atom == atoms.string {
        Some(MimeType::Text)
    } else if atom == atoms.text_html {
        Some(MimeType::Html)
    } else if atom == atoms.text_rtf {
        Some(MimeType::Rtf)
    } else if atom == atoms.text_uri_list {
        Some(MimeType::UriList)
    } else if atom == atoms.image_png {
        Some(MimeType::Png)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Public helpers for lib.rs wiring
// ---------------------------------------------------------------------------

/// Set a clipboard payload via the X11 thread.
///
/// `Custom` mime types are not supported (no live intern path); they return
/// `UnsupportedMime`.
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

/// Get clipboard bytes via the X11 thread.
pub(crate) fn get_clipboard(
    thread: &X11Thread,
    sel: Selection,
    mime: &MimeType,
) -> Result<Vec<u8>, ClipboardError> {
    let mime_atom =
        mime_to_atom_static(&thread.atoms, mime).ok_or(ClipboardError::UnsupportedMime)?;
    let sel_atom = sel_to_atom(&thread.atoms, sel);
    let result = thread.send_sync(X11Op::Get {
        sel_atom,
        mime_atom,
    })?;
    match result {
        X11OpResult::Get(r) => r,
        _ => unreachable!(),
    }
}

/// Return the available MIME types in a selection via the X11 thread.
pub(crate) fn available_clipboard(
    thread: &X11Thread,
    sel: Selection,
) -> Result<Vec<MimeType>, ClipboardError> {
    let sel_atom = sel_to_atom(&thread.atoms, sel);
    let result = thread.send_sync(X11Op::Available { sel_atom })?;
    match result {
        X11OpResult::Available(r) => {
            let raw_atoms = r?;
            // Map raw atoms to MimeType, deduplicating (multiple atoms may map
            // to the same MimeType, e.g. UTF8_STRING and STRING both -> Text).
            let mut mimes: Vec<MimeType> = Vec::new();
            for atom in raw_atoms {
                if let Some(mime) = atom_to_mime(&thread.atoms, atom) && !mimes.contains(&mime) {
                    mimes.push(mime);
                }
            }
            Ok(mimes)
        }
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
    use std::io::{self, Write};
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

    /// Write data into xclip's clipboard (xclip owns it; stays alive until we
    /// read it).  Returns a Child that must be waited on after reading.
    fn xclip_write(sel: &str, data: &[u8]) -> Option<Child> {
        if !Path::new("/usr/bin/xclip").exists() {
            return None;
        }
        let session = ensure_xvfb()?;
        let mut child = Command::new("/usr/bin/xclip")
            .args(["-selection", sel, "-i"])
            .env("DISPLAY", &session.display)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(data).ok()?;
        }
        Some(child)
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
    // 5b tests (set/clear)
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

    // -----------------------------------------------------------------------
    // 5c tests (get/available)
    // -----------------------------------------------------------------------

    #[test]
    fn get_clipboard_text() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let thread = match get_thread() {
            Some(t) => t,
            None => return,
        };

        let data = b"hello-get-5c\n";

        // Write via xclip; it stays alive as a selection owner.
        let mut child = match xclip_write("clipboard", data) {
            Some(c) => c,
            None => {
                eprintln!("SKIP get_clipboard_text: xclip not available");
                return;
            }
        };

        // Give xclip time to claim ownership.
        std::thread::sleep(Duration::from_millis(150));

        let result = get_clipboard(thread, Selection::Clipboard, &MimeType::Text);
        // Let xclip exit before asserting (avoid zombie).
        let _ = child.wait();

        let bytes = result.expect("get_clipboard failed");
        assert_eq!(bytes, data, "get_clipboard text mismatch");
    }

    #[test]
    fn get_primary_text() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let thread = match get_thread() {
            Some(t) => t,
            None => return,
        };

        let data = b"primary-get-5c\n";

        let mut child = match xclip_write("primary", data) {
            Some(c) => c,
            None => {
                eprintln!("SKIP get_primary_text: xclip not available");
                return;
            }
        };

        std::thread::sleep(Duration::from_millis(150));

        let result = get_clipboard(thread, Selection::Primary, &MimeType::Text);
        let _ = child.wait();

        let bytes = result.expect("get_clipboard (primary) failed");
        assert_eq!(bytes, data, "get_clipboard primary text mismatch");
    }

    #[test]
    fn get_after_self_set() {
        // We own the selection and then try to read it back from ourselves.
        // The X server sends our own window a SELECTION_REQUEST which our event
        // loop dispatches inside do_get's wait loop, so this must not deadlock.
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let thread = match get_thread() {
            Some(t) => t,
            None => return,
        };

        set_clipboard(thread, Selection::Clipboard, &MimeType::Text, b"loop").expect("set failed");
        std::thread::sleep(Duration::from_millis(50));

        let bytes =
            get_clipboard(thread, Selection::Clipboard, &MimeType::Text).expect("self-read failed");
        assert_eq!(bytes, b"loop", "self-read mismatch");
    }

    #[test]
    fn available_lists_text() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let thread = match get_thread() {
            Some(t) => t,
            None => return,
        };

        let mut child = match xclip_write("clipboard", b"available-test\n") {
            Some(c) => c,
            None => {
                eprintln!("SKIP available_lists_text: xclip not available");
                return;
            }
        };

        std::thread::sleep(Duration::from_millis(150));

        let result = available_clipboard(thread, Selection::Clipboard);
        let _ = child.wait();

        let mimes = result.expect("available_clipboard failed");
        assert!(
            mimes.contains(&MimeType::Text),
            "expected Text in available mimes, got: {mimes:?}"
        );
    }

    #[test]
    fn get_unowned_returns_unsupported() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let thread = match get_thread() {
            Some(t) => t,
            None => return,
        };

        // Clear so there is no owner.
        clear_clipboard(thread, Selection::Clipboard).expect("clear failed");
        std::thread::sleep(Duration::from_millis(100));

        let err = get_clipboard(thread, Selection::Clipboard, &MimeType::Text)
            .expect_err("expected error for unowned selection");
        assert!(
            matches!(err, ClipboardError::UnsupportedMime),
            "expected UnsupportedMime, got: {err}"
        );
    }

    #[test]
    fn available_no_owner_returns_empty() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let thread = match get_thread() {
            Some(t) => t,
            None => return,
        };

        clear_clipboard(thread, Selection::Clipboard).expect("clear failed");
        std::thread::sleep(Duration::from_millis(100));

        let mimes = available_clipboard(thread, Selection::Clipboard)
            .expect("available_clipboard should return Ok");
        assert!(
            mimes.is_empty(),
            "expected empty available list, got: {mimes:?}"
        );
    }

    // get_incr_payload: xclip uses INCR for payloads it considers "large" but
    // the threshold varies by version and is not reliably below our
    // max_request_length in xvfb.  We exercise the normal read path above.
    // TODO(5d): test INCR send + receive once we can control chunk size.
}
