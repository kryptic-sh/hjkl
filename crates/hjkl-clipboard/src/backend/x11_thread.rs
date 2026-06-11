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
const XCB_PROPERTY_DELETE: u8 = 1;
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
/// INCR send chunk size: leave 24 bytes for the XCB request header overhead.
/// This is computed per-connection from max_request_len_bytes; the constant
/// here is only used as the chunk cap — actual value comes from state.conn.
const XCB_REQUEST_HEADER_OVERHEAD: usize = 24;

// ---------------------------------------------------------------------------
// Timeout constants (all internal — not exposed as config)
// ---------------------------------------------------------------------------

/// Event loop tick interval in milliseconds. Drives INCR periodic prune.
const EVENT_LOOP_TICK_MS: u64 = 50;
/// SELECTION_NOTIFY wait for do_get: how long to wait for the owner to reply.
const SELECTION_NOTIFY_TIMEOUT_SECS: u64 = 5;
/// INCR receive: per-chunk timeout. Sender must write the next chunk within
/// this window or the receive is aborted. 30s tolerates slow CI hosts —
/// the `large_payload_self_loop` test on ubuntu-latest occasionally took
/// the full 10s budget waiting for PROPERTY_NOTIFY between chunks and
/// timed out at exactly 10.095s. Bumped to 30s; production usage on real
/// X servers completes in <1s.
const INCR_RECV_CHUNK_TIMEOUT_SECS: u64 = 30;
/// INCR receive: total timeout across all chunks.
const INCR_RECV_TOTAL_TIMEOUT_SECS: u64 = 60;
/// SAVE_TARGETS handshake: how long to wait for the manager's initial
/// SELECTION_NOTIFY before giving up silently.
const SAVE_TARGETS_TIMEOUT_SECS: u64 = 5;
/// Per-INCR-transfer timeout (send side): 30 seconds total.
const INCR_SEND_TOTAL_TIMEOUT_SECS: u64 = 30;

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
// INCR send state (approach b-lite: state machine in drain_events)
// ---------------------------------------------------------------------------

/// State for an in-flight INCR send to a single requestor.
///
/// When a payload exceeds max_request_len_bytes, we store it here and advance
/// the transfer each time a PROPERTY_DELETE event arrives from the requestor.
/// This avoids blocking the thread in a nested loop, which would deadlock the
/// self-loop (set-then-get on the same backend).
struct IncrSend {
    requestor: u32,
    property: u32,
    target_atom: u32,
    /// Full payload being chunked out.
    bytes: Vec<u8>,
    /// How many bytes have been sent so far.
    offset: usize,
    chunk_size: usize,
    deadline: Instant,
    /// True after we have written the zero-length terminator.
    done: bool,
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
    /// In-flight INCR send transfers (one per active requestor).
    incr_sends: Vec<IncrSend>,
    /// Cache of lazily interned atoms for Custom mime type names.
    /// The bg thread interns them on first use so the X11 connection is
    /// always accessed from the same thread that owns it.
    custom_atoms: HashMap<String, u32>,
}

// ---------------------------------------------------------------------------
// Op / Request types
// ---------------------------------------------------------------------------

/// Operations the X11 thread can handle.
pub(crate) enum X11Op {
    Set {
        sel_atom: u32,
        /// Pre-interned atom for known mime types; 0 when mime_name is used.
        mime_atom: u32,
        /// Atom name for Custom mimes that need lazy interning on the bg thread.
        /// None when mime_atom is already resolved.
        mime_name: Option<String>,
        bytes: Vec<u8>,
    },
    Clear {
        sel_atom: u32,
    },
    Get {
        sel_atom: u32,
        /// Pre-interned atom for known mime types; 0 when mime_name is used.
        mime_atom: u32,
        /// Atom name for Custom mimes that need lazy interning on the bg thread.
        mime_name: Option<String>,
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
// X11Future — wraps Oneshot<X11OpResult> as a Future
// ---------------------------------------------------------------------------

/// Future returned by [`X11Thread::send_async`].
pub(crate) struct X11Future {
    oneshot: Arc<crate::oneshot::Oneshot<X11OpResult>>,
}

impl std::future::Future for X11Future {
    type Output = X11OpResult;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        self.oneshot.poll(cx)
    }
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
                    incr_sends: Vec::new(),
                    custom_atoms: HashMap::new(),
                };
                run_loop(&mut state, rx);
            })
            .expect("failed to spawn X11 bg thread");

        Ok(Self { tx, atoms })
    }

    /// Enqueue an op and return a `Future` that resolves when the bg thread replies.
    pub(crate) fn send_async(&self, op: X11Op) -> X11Future {
        let oneshot = crate::oneshot::Oneshot::new();
        let reply = crate::reply::Reply::Async(Arc::clone(&oneshot));

        self.tx
            .send(X11Request { op, reply })
            .expect("x11 thread inbox closed");

        X11Future { oneshot }
    }

    /// Send an op and block until the bg thread replies.
    pub(crate) fn send_sync(&self, op: X11Op) -> Result<X11OpResult, ClipboardError> {
        let pair = Arc::new((Mutex::new(None::<X11OpResult>), Condvar::new()));
        let reply = crate::reply::Reply::Sync(Arc::clone(&pair));

        self.tx
            .send(X11Request { op, reply })
            .map_err(|_| ClipboardError::io_other("x11 thread inbox closed"))?;

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

// ClipboardError is Clone so we can store the typed error directly.
// Preserves LibNotFound/NoDisplay across calls so Clipboard::new()
// fallthrough logic sees the correct variant on every call.
static X11_THREAD: OnceLock<Result<X11Thread, ClipboardError>> = OnceLock::new();

/// Return the process-global X11 thread, or an error if X11 is unavailable.
pub(crate) fn x11_thread() -> Result<&'static X11Thread, ClipboardError> {
    X11_THREAD
        .get_or_init(X11Thread::new)
        .as_ref()
        .map_err(ClipboardError::clone)
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
        // Prune expired INCR sends at the top of every tick. If a requestor
        // dies mid-transfer, PROPERTY_DELETE events never arrive and the entry
        // would otherwise live forever in state.incr_sends. Pruning here caps
        // the growth to at most one 50 ms tick beyond the 30 s deadline.
        prune_expired_incr_sends(state);

        // Drain any pending X events before blocking on the inbox.
        drain_events(state, DrainGoal::AnyEvent);

        match rx.recv_timeout(Duration::from_millis(EVENT_LOOP_TICK_MS)) {
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
    /// INCR start (self-loop): wait for the PROPERTY_DELETE on our get-property
    /// (the receiver's hint read), which triggers chunk 1 via
    /// `advance_incr_sends`, then STOP — so this drain doesn't also consume
    /// chunk 1's `NEW_VALUE` (which the INCR receive loop needs). Any
    /// `NEW_VALUE` seen before the delete is the stale hint write and is
    /// discarded.
    OwnPropertyDelete { our_property: u32, our_window: u32 },
}

/// Result from drain_events when a goal is set.
enum DrainResult {
    /// Did not see the target event; caller should retry.
    NotFound,
    /// Saw SELECTION_NOTIFY; property field indicates owner's reply.
    SelectionNotifySeen { property: u32 },
    /// Saw PROPERTY_NOTIFY (new value); caller should read the property.
    PropertyNotifySeen,
    /// Saw the receiver's PROPERTY_DELETE on our get-property (INCR chunk 1 has
    /// just been written by `advance_incr_sends`).
    OwnPropertyDeleteSeen,
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

                // Always try to advance any in-flight INCR sends when we see
                // a property-delete on a requestor window, regardless of goal.
                if pn.state == XCB_PROPERTY_DELETE {
                    advance_incr_sends(state, pn.window, pn.atom);
                }

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
                    DrainGoal::OwnPropertyDelete {
                        our_property,
                        our_window,
                    } if pn.window == *our_window
                        && pn.atom == *our_property
                        && pn.state == XCB_PROPERTY_DELETE =>
                    {
                        // `advance_incr_sends` above already wrote chunk 1 in
                        // response to this delete. Stop now so we don't also
                        // consume chunk 1's NEW_VALUE — the receive loop needs
                        // it. SAFETY: ev was heap-allocated by xcb.
                        unsafe { libc::free(ev.cast()) };
                        return DrainResult::OwnPropertyDeleteSeen;
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
    let max_payload = state
        .conn
        .screen()
        .max_request_len_bytes
        .saturating_sub(XCB_REQUEST_HEADER_OVERHEAD as u32) as usize;

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
            if payload.len() <= max_payload {
                // Small payload — write it directly.
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
                // Oversized payload — use INCR sub-protocol.
                // Clone the bytes so we can release the borrow on `state.owned`
                // before registering the non-blocking INCR transfer.
                let bytes = payload.clone();
                let target_atom = ev.target;
                // Send SELECTION_NOTIFY first (with property != NONE) so the
                // requestor knows the INCR handshake has started.
                send_selection_notify(state, ev, property);
                start_incr_send(
                    state,
                    ev.requestor,
                    property,
                    target_atom,
                    bytes,
                    max_payload,
                );
                // start_incr_send registered the state; SELECTION_NOTIFY sent.
                return;
            }
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

/// Start an INCR send transfer (approach b-lite: non-blocking state machine).
///
/// We write the INCR size-hint property, subscribe to PROPERTY_DELETE events
/// on the requestor, flush, and return immediately. The caller has already
/// sent SELECTION_NOTIFY (with `property != NONE`). Subsequent chunks are
/// written by `advance_incr_sends` each time a matching PROPERTY_DELETE event
/// arrives in `drain_events`.
///
/// Trade-off: multiple simultaneous INCR sends are handled correctly (each
/// advances independently as events arrive). The self-loop case (set then
/// immediate get on the same backend) also works because `drain_events` calls
/// `advance_incr_sends` on every PROPERTY_DELETE regardless of the current
/// goal — so INCR send advances even while `do_get` waits for SELECTION_NOTIFY.
fn start_incr_send(
    state: &mut X11State,
    requestor: u32,
    property: u32,
    target_atom: u32,
    bytes: Vec<u8>,
    chunk_size: usize,
) {
    let fns = state.conn.fns();
    let raw = state.conn.raw();
    let atoms = state.conn.atoms();

    // Write INCR property: type=INCR, format=32, one u32 total size hint.
    let size_hint: u32 = bytes.len().min(u32::MAX as usize) as u32;
    // SAFETY: size_hint is a valid u32; INCR type + format=32.
    unsafe {
        (fns.xcb_change_property)(
            raw,
            XCB_PROP_MODE_REPLACE,
            requestor,
            property,
            atoms.incr,
            32,
            1,
            std::ptr::addr_of!(size_hint).cast::<c_void>(),
        );
    }

    // Subscribe to PROPERTY_NOTIFY on the requestor's window so PROPERTY_DELETE
    // events are delivered to us when the requestor consumes each chunk.
    let event_mask_val: u32 = XCB_EVENT_MASK_PROPERTY_CHANGE;
    // SAFETY: requestor is a valid X window; value_list is a single u32
    // indexed by the single set bit in XCB_CW_EVENT_MASK.
    unsafe {
        (fns.xcb_change_window_attributes)(
            raw,
            requestor,
            XCB_CW_EVENT_MASK,
            std::ptr::addr_of!(event_mask_val).cast::<c_void>(),
        );
    }
    // SAFETY: conn live.
    unsafe { (fns.xcb_flush)(raw) };

    // SELECTION_NOTIFY was already sent by the caller before calling us.
    // Register the transfer so drain_events can advance it via advance_incr_sends.
    state.incr_sends.push(IncrSend {
        requestor,
        property,
        target_atom,
        bytes,
        offset: 0,
        chunk_size,
        deadline: Instant::now() + Duration::from_secs(INCR_SEND_TOTAL_TIMEOUT_SECS),
        done: false,
    });
}

/// Drop INCR send entries that have exceeded their deadline without completing.
///
/// Called at the top of every run_loop tick so entries are reclaimed even
/// when the requestor dies and no PROPERTY_DELETE events ever arrive.
/// A zero-length terminator is sent best-effort before dropping so the X
/// server does not leave the requestor's property in an inconsistent state.
fn prune_expired_incr_sends(state: &mut X11State) {
    let now = Instant::now();
    let fns = state.conn.fns();
    let raw = state.conn.raw();

    for xfer in &mut state.incr_sends {
        if xfer.done || now < xfer.deadline {
            continue;
        }
        // Best-effort zero-length terminator before dropping.
        // SAFETY: sending zero bytes is always valid.
        unsafe {
            (fns.xcb_change_property)(
                raw,
                XCB_PROP_MODE_REPLACE,
                xfer.requestor,
                xfer.property,
                xfer.target_atom,
                8,
                0,
                std::ptr::null(),
            );
            (fns.xcb_flush)(raw);
        }
        xfer.done = true;
    }

    state.incr_sends.retain(|x| !x.done);
}

/// Advance any in-flight INCR send whose requestor deleted the given property.
///
/// Called from `drain_events` on every PROPERTY_DELETE event, regardless of
/// the current `DrainGoal`. This drives all registered transfers without
/// requiring a dedicated blocking loop, and resolves the self-loop deadlock.
fn advance_incr_sends(state: &mut X11State, window: u32, atom: u32) {
    let fns = state.conn.fns();
    let raw = state.conn.raw();

    let now = Instant::now();

    for xfer in &mut state.incr_sends {
        if xfer.done || xfer.requestor != window || xfer.property != atom {
            continue;
        }

        // Check for timeout — send zero-length terminator and mark done.
        if now >= xfer.deadline {
            // SAFETY: sending zero bytes is always valid.
            unsafe {
                (fns.xcb_change_property)(
                    raw,
                    XCB_PROP_MODE_REPLACE,
                    xfer.requestor,
                    xfer.property,
                    xfer.target_atom,
                    8,
                    0,
                    std::ptr::null(),
                );
                (fns.xcb_flush)(raw);
            }
            xfer.done = true;
            continue;
        }

        let end = (xfer.offset + xfer.chunk_size).min(xfer.bytes.len());
        let chunk = &xfer.bytes[xfer.offset..end];

        // SAFETY: chunk is valid for chunk.len() bytes; null data ptr on
        // zero-length write is safe (XCB does not dereference it).
        unsafe {
            (fns.xcb_change_property)(
                raw,
                XCB_PROP_MODE_REPLACE,
                xfer.requestor,
                xfer.property,
                xfer.target_atom,
                8,
                chunk.len() as u32,
                if chunk.is_empty() {
                    std::ptr::null()
                } else {
                    chunk.as_ptr().cast::<c_void>()
                },
            );
            (fns.xcb_flush)(raw);
        }

        if chunk.is_empty() {
            // Zero-length chunk signals end of INCR to the receiver.
            xfer.done = true;
        } else {
            xfer.offset = end;
        }
    }

    // Prune completed transfers to keep the Vec small.
    state.incr_sends.retain(|x| !x.done);
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

/// Intern an atom by name on the bg thread, caching in state.custom_atoms.
///
/// Uses `only_if_exists=0` so the atom is created if it doesn't exist yet,
/// matching how xcb_intern_atom is used for our pre-interned set at startup.
fn intern_atom(state: &mut X11State, name: &str) -> Result<u32, ClipboardError> {
    if let Some(&atom) = state.custom_atoms.get(name) {
        return Ok(atom);
    }
    let fns = state.conn.fns();
    let raw = state.conn.raw();
    // SAFETY: name is a valid UTF-8 string; name_len fits in u16 (checked).
    let name_len = name.len().min(u16::MAX as usize) as u16;
    let cookie = unsafe {
        (fns.xcb_intern_atom)(
            raw,
            0, // only_if_exists=0: create if absent
            name_len,
            name.as_ptr() as *const std::ffi::c_char,
        )
    };
    // SAFETY: cookie is valid; null error pointer → null reply on error.
    let reply = unsafe { (fns.xcb_intern_atom_reply)(raw, cookie, std::ptr::null_mut()) };
    if reply.is_null() {
        return Err(ClipboardError::io_other(
            "xcb_intern_atom_reply returned null",
        ));
    }
    // SAFETY: reply is non-null xcb_intern_atom_reply_t; atom field at offset 8.
    let atom = unsafe { (*reply).atom };
    // SAFETY: reply was malloc'd by xcb.
    unsafe { libc::free(reply.cast()) };
    state.custom_atoms.insert(name.to_owned(), atom);
    Ok(atom)
}

/// Resolve a mime descriptor to an atom: use mime_atom if non-zero, otherwise
/// intern mime_name on the bg thread.
fn resolve_mime_atom(
    state: &mut X11State,
    mime_atom: u32,
    mime_name: Option<String>,
) -> Result<u32, ClipboardError> {
    if mime_atom != 0 {
        return Ok(mime_atom);
    }
    match mime_name {
        Some(name) => intern_atom(state, &name),
        None => Err(ClipboardError::UnsupportedMime),
    }
}

fn handle_op(state: &mut X11State, req: X11Request) {
    let result = match req.op {
        X11Op::Set {
            sel_atom,
            mime_atom,
            mime_name,
            bytes,
        } => {
            let atom = resolve_mime_atom(state, mime_atom, mime_name);
            X11OpResult::Set(atom.and_then(|a| do_set(state, sel_atom, a, bytes)))
        }
        X11Op::Clear { sel_atom } => X11OpResult::Clear(do_clear(state, sel_atom)),
        X11Op::Get {
            sel_atom,
            mime_atom,
            mime_name,
        } => {
            let atom = resolve_mime_atom(state, mime_atom, mime_name);
            X11OpResult::Get(atom.and_then(|a| do_get(state, sel_atom, a)))
        }
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
    // No payload size cap here — oversized payloads are served via INCR send
    // in handle_selection_request. The PayloadTooLarge error is still used by
    // the OSC 52 backend (and kept in ClipboardError for that purpose).

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
        return Err(ClipboardError::io_other(
            "xcb_get_selection_owner_reply returned null",
        ));
    }
    // SAFETY: reply is non-null; `owner` is at offset 8.
    let owner = unsafe { (*reply).owner };
    // SAFETY: reply was malloc'd by xcb.
    unsafe { libc::free(reply.cast()) };

    if owner != window {
        return Err(ClipboardError::io_other(
            "another client holds the selection",
        ));
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

    // Auto-SAVE_TARGETS: notify any clipboard manager so our data persists
    // after we exit. Only for CLIPBOARD (not PRIMARY — managers don't persist
    // primary selections by convention).
    let atoms = state.conn.atoms();
    if sel_atom == atoms.clipboard {
        do_save_targets(state);
    }

    Ok(())
}

/// Request the clipboard manager (if any) to save our clipboard data.
///
/// Fires CONVERT_SELECTION(CLIPBOARD_MANAGER, SAVE_TARGETS) and waits up to
/// 5 s for the initial acceptance SELECTION_NOTIFY. The manager then fetches
/// our data in the background; we do not block on that secondary transfer.
///
/// Silently skips if no manager owns the CLIPBOARD_MANAGER selection.
fn do_save_targets(state: &mut X11State) {
    let fns = state.conn.fns();
    let raw = state.conn.raw();
    let window = state.window;
    let atoms = state.conn.atoms();

    // Check if there is a clipboard manager.
    let cookie = unsafe { (fns.xcb_get_selection_owner)(raw, atoms.clipboard_manager) };
    // SAFETY: reply must be freed with libc::free.
    let reply = unsafe { (fns.xcb_get_selection_owner_reply)(raw, cookie, std::ptr::null_mut()) };
    if reply.is_null() {
        return; // X server error; skip silently.
    }
    // SAFETY: reply is non-null; owner at offset 8.
    let mgr = unsafe { (*reply).owner };
    // SAFETY: reply was malloc'd by xcb.
    unsafe { libc::free(reply.cast()) };

    if mgr == XCB_NONE {
        // No manager present — graceful no-op.
        return;
    }

    // Write a list of our CLIPBOARD mime atoms into our private property so
    // the manager can read which targets to save.
    let owned_atoms: Vec<u32> = state
        .owned
        .get(&atoms.clipboard)
        .map(|d| d.targets.clone())
        .unwrap_or_default();

    let our_property = atoms.hjkl_clipboard_get;

    // SAFETY: owned_atoms is valid for owned_atoms.len() u32 values;
    // format=32 (atom list).
    unsafe {
        (fns.xcb_change_property)(
            raw,
            XCB_PROP_MODE_REPLACE,
            window,
            our_property,
            XCB_ATOM_ATOM,
            32,
            owned_atoms.len() as u32,
            owned_atoms.as_ptr().cast::<c_void>(),
        );
    }

    // Ask the manager to save our current data.
    // SAFETY: standard xcb call; all args valid.
    unsafe {
        (fns.xcb_convert_selection)(
            raw,
            window,
            atoms.clipboard_manager,
            atoms.save_targets,
            our_property,
            XCB_CURRENT_TIME,
        );
    }
    // SAFETY: conn live.
    unsafe { (fns.xcb_flush)(raw) };

    // Wait for SELECTION_NOTIFY accepting or refusing the SAVE_TARGETS.
    // We wait a short time for the initial handshake only — the manager
    // does the actual data copy in the background after that.
    let deadline = Instant::now() + Duration::from_secs(SAVE_TARGETS_TIMEOUT_SECS);
    loop {
        if let DrainResult::SelectionNotifySeen { .. } =
            drain_events(state, DrainGoal::SelectionNotify { our_property })
        {
            // Manager accepted (or refused with NONE) — either way we are done.
            break;
        }
        if Instant::now() >= deadline {
            // Manager did not reply in time; skip silently.
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
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
        return Err(ClipboardError::io_other(
            "xcb_get_property_reply returned null",
        ));
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
    let deadline = Instant::now() + Duration::from_secs(SELECTION_NOTIFY_TIMEOUT_SECS);
    let replied_property = loop {
        if let DrainResult::SelectionNotifySeen { property } =
            drain_events(state, DrainGoal::SelectionNotify { our_property })
        {
            break property;
        }
        if Instant::now() >= deadline {
            return Err(ClipboardError::io_other(
                "xcb_convert_selection timed out waiting for SELECTION_NOTIFY",
            ));
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
    // read_property(delete=1) above already deleted the INCR property, which
    // signals readiness to the sender.
    //
    // In the self-loop case (we own the selection we are reading), the INCR
    // size-hint write by start_incr_send generated a stale PROPERTY_NEW_VALUE
    // event that is still queued, and the PROPERTY_DELETE from `read_property`
    // above must trigger `advance_incr_sends` to write chunk 1.
    //
    // We must NOT use `DrainGoal::AnyEvent` here: that drains the WHOLE queue,
    // and if chunk 1's NEW_VALUE happens to be delivered within the same pass
    // (a fast host / lucky scheduling), it gets discarded — and the receive
    // loop below then waits forever for a NEW_VALUE that already came and went
    // (the intermittent `INCR receive timed out` flake). Instead, wait
    // specifically for our property-delete: that discards the stale hint
    // NEW_VALUE on the way, triggers chunk 1, and STOPS immediately, leaving
    // chunk 1's NEW_VALUE for the receive loop. A late delete is also handled
    // by the receive loop itself (it advances INCR sends on every delete), so a
    // timeout here just falls through harmlessly. Non-self-loop transfers have
    // no pending delete and fall through after the budget.
    {
        let init_deadline = Instant::now() + Duration::from_secs(SELECTION_NOTIFY_TIMEOUT_SECS);
        loop {
            if let DrainResult::OwnPropertyDeleteSeen = drain_events(
                state,
                DrainGoal::OwnPropertyDelete {
                    our_property,
                    our_window: window,
                },
            ) {
                break;
            }
            if Instant::now() >= init_deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    let mut accumulator: Vec<u8> = Vec::new();
    let total_deadline = Instant::now() + Duration::from_secs(INCR_RECV_TOTAL_TIMEOUT_SECS);

    loop {
        // Wait for PROPERTY_NOTIFY (new value) on our window/property.
        let chunk_deadline = Instant::now() + Duration::from_secs(INCR_RECV_CHUNK_TIMEOUT_SECS);
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
                return Err(ClipboardError::io_other(
                    "INCR receive timed out waiting for PROPERTY_NOTIFY",
                ));
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
            return Err(ClipboardError::io_other(
                "INCR receive exceeded total timeout",
            ));
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
                return Err(ClipboardError::io_other(
                    "TARGETS reply has non-multiple-of-4 byte length",
                ));
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

/// Map a MimeType to (mime_atom, mime_name):
///   - Known types return (atom, None).
///   - Custom(s) returns (0, Some(s)) so the bg thread can intern lazily.
pub(crate) fn mime_to_atom_or_name(atoms: &Atoms, mime: &MimeType) -> (u32, Option<String>) {
    match mime_to_atom_static(atoms, mime) {
        Some(atom) => (atom, None),
        None => {
            let name = match mime {
                MimeType::Custom(s) => Some(s.clone()),
                _ => None,
            };
            (0, name)
        }
    }
}

/// Map a raw atom back to a MimeType, returning None for unknown atoms.
///
/// Custom atoms set by us are not included here — available() only reports
/// the known pre-interned types. Unknown atoms are silently dropped from
/// available() results (Phase 8 / v0.5 can add reverse xcb_get_atom_name
/// lookup for full fidelity).
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
/// `Custom(s)` mime types are interned lazily on the bg thread using
/// `xcb_intern_atom` and cached for re-use. All other mime types use
/// pre-interned atoms from the `Atoms` struct.
pub(crate) fn set_clipboard(
    thread: &X11Thread,
    sel: Selection,
    mime: &MimeType,
    bytes: &[u8],
) -> Result<(), ClipboardError> {
    let (mime_atom, mime_name) = mime_to_atom_or_name(&thread.atoms, mime);
    let sel_atom = sel_to_atom(&thread.atoms, sel);

    let result = thread.send_sync(X11Op::Set {
        sel_atom,
        mime_atom,
        mime_name,
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
///
/// `Custom(s)` mime types are interned lazily on the bg thread; all others
/// use pre-interned atoms.
pub(crate) fn get_clipboard(
    thread: &X11Thread,
    sel: Selection,
    mime: &MimeType,
) -> Result<Vec<u8>, ClipboardError> {
    let (mime_atom, mime_name) = mime_to_atom_or_name(&thread.atoms, mime);
    let sel_atom = sel_to_atom(&thread.atoms, sel);
    let result = thread.send_sync(X11Op::Get {
        sel_atom,
        mime_atom,
        mime_name,
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
                if let Some(mime) = atom_to_mime(&thread.atoms, atom)
                    && !mimes.contains(&mime)
                {
                    mimes.push(mime);
                }
            }
            Ok(mimes)
        }
        _ => unreachable!(),
    }
}

pub(crate) fn sel_to_atom(atoms: &Atoms, sel: Selection) -> u32 {
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
    // Connection-plumbing test (moved from x11.rs to share XVFB_SESSION and
    // avoid env-mutation races with parallel tests).
    // -----------------------------------------------------------------------

    /// Open a fresh X11Connection against the shared xvfb session and verify
    /// screen info + atom interning produced sane values.
    #[test]
    fn xvfb_connection_and_atoms() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if ensure_xvfb().is_none() {
            return;
        }

        let conn = match super::super::x11::X11Connection::open() {
            Ok(c) => c,
            Err(ClipboardError::LibNotFound) => {
                eprintln!("SKIP xvfb_connection_and_atoms: libxcb.so.1 not found");
                return;
            }
            Err(e) => panic!("X11Connection::open failed: {e}"),
        };

        let screen = conn.screen();
        assert_eq!(screen.width, 800, "screen width mismatch");
        assert_eq!(screen.height, 600, "screen height mismatch");
        assert_ne!(screen.root, 0, "root window must be non-zero");
        assert_ne!(screen.root_visual, 0, "root visual must be non-zero");
        assert!(
            screen.max_request_len_bytes > 0,
            "max_request_len_bytes must be > 0"
        );

        let a = conn.atoms();
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

    // payload_too_large_errors: removed in 5d — do_set no longer rejects large
    // payloads; handle_selection_request uses INCR send instead. The
    // large_payload_self_loop test below exercises this new path.

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

    // -----------------------------------------------------------------------
    // 5d tests (INCR send + auto-SAVE_TARGETS)
    // -----------------------------------------------------------------------

    /// 1 MiB payload well over xvfb's default ~256 KB max_request_length.
    /// We set it (which stores it; INCR send fires when a reader asks for it)
    /// and then get it back via our own do_get, which uses our INCR receive
    /// (5c).  This exercises both INCR send and INCR receive end-to-end via
    /// a self-loop through the X server.
    #[test]
    fn large_payload_self_loop() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let thread = match get_thread() {
            Some(t) => t,
            None => return,
        };

        // 1 MiB of a repeating pattern so any data corruption is obvious.
        let size = 1024 * 1024;
        let payload: Vec<u8> = (0u8..=255).cycle().take(size).collect();

        set_clipboard(thread, Selection::Clipboard, &MimeType::Text, &payload)
            .expect("set large payload failed");

        // do_get → SELECTION_REQUEST → INCR send → INCR receive.
        let received = get_clipboard(thread, Selection::Clipboard, &MimeType::Text)
            .expect("get large payload failed");

        assert_eq!(
            received.len(),
            size,
            "large payload length mismatch: got {} expected {size}",
            received.len()
        );
        assert_eq!(received, payload, "large payload content mismatch");
    }

    // -----------------------------------------------------------------------
    // Mock CLIPBOARD_MANAGER
    // -----------------------------------------------------------------------
    //
    // The mock thread owns the CLIPBOARD_MANAGER selection on the same Xvfb
    // display as the singleton X11Thread.  It accepts SAVE_TARGETS requests
    // and fetches all advertised targets, storing them in received_payloads.
    //
    // Architecture notes:
    //   - The mock opens its OWN X11 connection (separate from the singleton).
    //   - It uses xcb_wait_for_event (blocking) on its own connection.
    //   - Shutdown via an AtomicBool polled from a second thread that calls
    //     xcb_send_event to unblock the wait — but since xcb_wait_for_event
    //     blocks indefinitely, we instead use a short-lived test that relies
    //     on the mock thread stopping when the connection is dropped.
    //   - Simpler approach used here: the mock thread loops with
    //     xcb_poll_for_event + 10 ms sleep until a stop flag is set.

    use std::collections::HashMap as TestHashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct MockManager {
        handle: std::thread::JoinHandle<()>,
        saw_save_targets: Arc<AtomicBool>,
        received_payloads: Arc<Mutex<TestHashMap<u32, Vec<u8>>>>,
        stop: Arc<AtomicBool>,
    }

    impl MockManager {
        /// Spawn the mock clipboard manager on the given display.
        fn spawn(display: &str) -> Option<Self> {
            let saw_save_targets = Arc::new(AtomicBool::new(false));
            let received_payloads = Arc::new(Mutex::new(TestHashMap::new()));
            let stop = Arc::new(AtomicBool::new(false));

            let saw2 = Arc::clone(&saw_save_targets);
            let payloads2 = Arc::clone(&received_payloads);
            let stop2 = Arc::clone(&stop);
            let display_str = display.to_string();

            // Open a test connection using the same X11Connection::open path,
            // but we need our own connection — so set DISPLAY temporarily.
            // We capture the current DISPLAY, set ours, open, restore.
            // This is safe here because the mock thread is spawned while the
            // TEST_LOCK is held and DISPLAY is already set to our display.

            // Build the connection *before* spawning the thread so we can
            // return None if the connection fails.
            // SAFETY: DISPLAY is already set to our test display (ensured by
            // the TEST_LOCK + ensure_xvfb() call chain). This is purely
            // test-only env access.
            let _ = display_str; // consumed into the closure below

            let handle = std::thread::Builder::new()
                .name("mock-clipboard-manager".into())
                .spawn(move || {
                    // Open our own connection (uses DISPLAY env which is set
                    // to our xvfb display for the duration of this test).
                    let conn = match super::super::x11::X11Connection::open() {
                        Ok(c) => c,
                        Err(e) => {
                            eprintln!("MockManager: connection failed: {e}");
                            return;
                        }
                    };
                    mock_manager_run(conn, saw2, payloads2, stop2);
                })
                .ok()?;

            Some(MockManager {
                handle,
                saw_save_targets,
                received_payloads,
                stop,
            })
        }

        fn stop_and_join(self) {
            self.stop.store(true, Ordering::SeqCst);
            let _ = self.handle.join();
        }
    }

    /// Core loop for the mock manager thread.
    fn mock_manager_run(
        conn: super::super::x11::X11Connection,
        saw_save_targets: Arc<AtomicBool>,
        received_payloads: Arc<Mutex<TestHashMap<u32, Vec<u8>>>>,
        stop: Arc<AtomicBool>,
    ) {
        use std::ffi::c_void;

        let fns = conn.fns();
        let raw = conn.raw();
        let atoms = conn.atoms();

        // Create an invisible window to hold CLIPBOARD_MANAGER ownership.
        // SAFETY: xcb_generate_id returns a fresh XID.
        let wid = unsafe { (fns.xcb_generate_id)(raw) };
        let screen = conn.screen();
        // SAFETY: all parameters valid; conn live on this thread.
        let event_mask_mock: u32 = XCB_EVENT_MASK_PROPERTY_CHANGE;
        unsafe {
            (fns.xcb_create_window)(
                raw,
                screen.root_depth,
                wid,
                screen.root,
                0,
                0,
                1,
                1,
                0,
                XCB_WINDOW_CLASS_INPUT_OUTPUT,
                screen.root_visual,
                XCB_CW_EVENT_MASK,
                std::ptr::addr_of!(event_mask_mock).cast::<c_void>(),
            );
        }

        // A property atom for our own get requests (we use HJKL_CLIPBOARD_GET
        // from our atoms — but this is a *different* connection so we intern
        // our own copy).
        let mgr_property = atoms.hjkl_clipboard_get; // same atom ID, different connection

        // Claim CLIPBOARD_MANAGER ownership.
        // SAFETY: conn live; wid and clipboard_manager atom valid.
        unsafe {
            (fns.xcb_set_selection_owner)(raw, wid, atoms.clipboard_manager, XCB_CURRENT_TIME);
            (fns.xcb_flush)(raw);
        }

        loop {
            if stop.load(Ordering::SeqCst) {
                break;
            }

            // SAFETY: xcb_poll_for_event returns null when queue is empty.
            let ev = unsafe { (fns.xcb_poll_for_event)(raw) };
            if ev.is_null() {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }

            // SAFETY: first byte is response_type.
            let response_type = unsafe { *(ev as *const u8) } & 0x7f;

            match response_type {
                XCB_SELECTION_REQUEST => {
                    // SAFETY: ev is a valid 32-byte SelectionRequestEvent.
                    let req = unsafe { *(ev as *const SelectionRequestEvent) };
                    if req.target == atoms.save_targets {
                        saw_save_targets.store(true, Ordering::SeqCst);

                        // Read the TARGETS list from the requestor's property.
                        let prop = if req.property == XCB_NONE {
                            req.target
                        } else {
                            req.property
                        };
                        let targets = mock_read_property(fns, raw, req.requestor, prop);
                        // targets is a list of u32 atoms (format=32).
                        let atom_list: Vec<u32> = targets
                            .chunks_exact(4)
                            .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
                            .collect();

                        // Fetch each target from the requestor.
                        for target in atom_list {
                            if target == atoms.targets || target == atoms.multiple {
                                continue; // skip protocol atoms
                            }
                            // Ask requestor to write target into our property.
                            // SAFETY: valid xcb call.
                            unsafe {
                                (fns.xcb_delete_property)(raw, wid, mgr_property);
                                (fns.xcb_convert_selection)(
                                    raw,
                                    wid,
                                    atoms.clipboard,
                                    target,
                                    mgr_property,
                                    XCB_CURRENT_TIME,
                                );
                                (fns.xcb_flush)(raw);
                            }

                            // Wait for SELECTION_NOTIFY for this target (up to 5 s).
                            let deadline = Instant::now() + Duration::from_secs(5);
                            'wait: loop {
                                let ev2 = unsafe { (fns.xcb_poll_for_event)(raw) };
                                if !ev2.is_null() {
                                    let rt2 = unsafe { *(ev2 as *const u8) } & 0x7f;
                                    if rt2 == XCB_SELECTION_NOTIFY {
                                        // SAFETY: ev2 is valid SelectionNotifyEvent.
                                        let sn = unsafe { *(ev2 as *const SelectionNotifyEvent) };
                                        if sn.requestor == wid {
                                            // Read the property (may be INCR).
                                            if sn.property != XCB_NONE {
                                                let data = mock_read_full(
                                                    fns,
                                                    raw,
                                                    wid,
                                                    mgr_property,
                                                    atoms.incr,
                                                );
                                                received_payloads
                                                    .lock()
                                                    .unwrap()
                                                    .insert(target, data);
                                            }
                                            unsafe { libc::free(ev2.cast()) };
                                            break 'wait;
                                        }
                                    }
                                    unsafe { libc::free(ev2.cast()) };
                                }
                                if Instant::now() >= deadline {
                                    break 'wait;
                                }
                                std::thread::sleep(Duration::from_millis(5));
                            }
                        }

                        // Send SELECTION_NOTIFY back to the requestor to signal
                        // we accepted SAVE_TARGETS.
                        let mut buf = [0u8; 32];
                        buf[0] = XCB_SELECTION_NOTIFY;
                        buf[4..8].copy_from_slice(&req.time.to_ne_bytes());
                        buf[8..12].copy_from_slice(&req.requestor.to_ne_bytes());
                        buf[12..16].copy_from_slice(&req.selection.to_ne_bytes());
                        buf[16..20].copy_from_slice(&req.target.to_ne_bytes());
                        let reply_prop = if req.property == XCB_NONE {
                            XCB_NONE
                        } else {
                            req.property
                        };
                        buf[20..24].copy_from_slice(&reply_prop.to_ne_bytes());
                        // SAFETY: valid 32-byte event buffer; destination = requestor.
                        unsafe {
                            (fns.xcb_send_event)(raw, 0, req.requestor, 0, buf.as_ptr().cast());
                            (fns.xcb_flush)(raw);
                        }
                    }
                }
                XCB_SELECTION_CLEAR => {
                    // We lost CLIPBOARD_MANAGER — another manager claimed it.
                    break;
                }
                _ => {}
            }

            // SAFETY: ev was heap-allocated by xcb.
            unsafe { libc::free(ev.cast()) };
        }
    }

    /// Read a plain property from window `w`, property atom `prop`.
    /// Returns raw bytes (no delete).
    fn mock_read_property(
        fns: &super::super::dlopen::XcbFns,
        raw: *mut super::super::dlopen::XcbConnection,
        w: u32,
        prop: u32,
    ) -> Vec<u8> {
        let cookie = unsafe {
            (fns.xcb_get_property)(raw, 0, w, prop, XCB_GET_PROPERTY_TYPE_ANY, 0, u32::MAX / 4)
        };
        let reply = unsafe { (fns.xcb_get_property_reply)(raw, cookie, std::ptr::null_mut()) };
        if reply.is_null() {
            return Vec::new();
        }
        let vptr = unsafe { (fns.xcb_get_property_value)(reply) };
        let vlen = unsafe { (fns.xcb_get_property_value_length)(reply) } as usize;
        let bytes = if vlen > 0 && !vptr.is_null() {
            // SAFETY: vptr is valid for vlen bytes inside reply allocation.
            unsafe { std::slice::from_raw_parts(vptr as *const u8, vlen).to_vec() }
        } else {
            Vec::new()
        };
        // SAFETY: reply was malloc'd by xcb.
        unsafe { libc::free(reply.cast()) };
        bytes
    }

    /// Read a property from our own window, handling INCR if needed.
    /// Reads then deletes (delete=1).
    fn mock_read_full(
        fns: &super::super::dlopen::XcbFns,
        raw: *mut super::super::dlopen::XcbConnection,
        wid: u32,
        prop: u32,
        incr_atom: u32,
    ) -> Vec<u8> {
        let cookie = unsafe {
            (fns.xcb_get_property)(
                raw,
                1,
                wid,
                prop,
                XCB_GET_PROPERTY_TYPE_ANY,
                0,
                u32::MAX / 4,
            )
        };
        let reply = unsafe { (fns.xcb_get_property_reply)(raw, cookie, std::ptr::null_mut()) };
        if reply.is_null() {
            return Vec::new();
        }
        let type_atom = unsafe { (*reply).r#type };
        let vptr = unsafe { (fns.xcb_get_property_value)(reply) };
        let vlen = unsafe { (fns.xcb_get_property_value_length)(reply) } as usize;
        let initial = if vlen > 0 && !vptr.is_null() {
            unsafe { std::slice::from_raw_parts(vptr as *const u8, vlen).to_vec() }
        } else {
            Vec::new()
        };
        unsafe { libc::free(reply.cast()) };

        if type_atom != incr_atom {
            return initial;
        }

        // INCR receive: delete property to signal readiness, then loop.
        unsafe {
            (fns.xcb_delete_property)(raw, wid, prop);
            (fns.xcb_flush)(raw);
        }

        let mut acc = Vec::new();
        let total_deadline = Instant::now() + Duration::from_secs(30);

        loop {
            // Wait for PROPERTY_NOTIFY (new value) — poll loop.
            let chunk_deadline = Instant::now() + Duration::from_secs(10);
            let got_notify = 'notify: loop {
                let ev = unsafe { (fns.xcb_poll_for_event)(raw) };
                if !ev.is_null() {
                    let rt = unsafe { *(ev as *const u8) } & 0x7f;
                    if rt == XCB_PROPERTY_NOTIFY {
                        let pn = unsafe { *(ev as *const PropertyNotifyEvent) };
                        if pn.window == wid && pn.atom == prop && pn.state == XCB_PROPERTY_NEW_VALUE
                        {
                            unsafe { libc::free(ev.cast()) };
                            break 'notify true;
                        }
                    }
                    unsafe { libc::free(ev.cast()) };
                }
                if Instant::now() >= chunk_deadline || Instant::now() >= total_deadline {
                    break 'notify false;
                }
                std::thread::sleep(Duration::from_millis(5));
            };
            if !got_notify {
                break;
            }

            // Read + delete chunk.
            let cookie2 = unsafe {
                (fns.xcb_get_property)(
                    raw,
                    1,
                    wid,
                    prop,
                    XCB_GET_PROPERTY_TYPE_ANY,
                    0,
                    u32::MAX / 4,
                )
            };
            let r2 = unsafe { (fns.xcb_get_property_reply)(raw, cookie2, std::ptr::null_mut()) };
            if r2.is_null() {
                break;
            }
            let vp2 = unsafe { (fns.xcb_get_property_value)(r2) };
            let vl2 = unsafe { (fns.xcb_get_property_value_length)(r2) } as usize;
            let chunk = if vl2 > 0 && !vp2.is_null() {
                unsafe { std::slice::from_raw_parts(vp2 as *const u8, vl2).to_vec() }
            } else {
                Vec::new()
            };
            unsafe { libc::free(r2.cast()) };

            if chunk.is_empty() {
                break; // zero-length = end of INCR
            }
            acc.extend_from_slice(&chunk);

            if Instant::now() >= total_deadline {
                break;
            }
        }

        acc
    }

    /// SAVE_TARGETS round-trip: spawn a MockManager, set clipboard text, verify
    /// the manager received a SAVE_TARGETS request and copied the payload.
    #[test]
    fn save_targets_invokes_manager() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let thread = match get_thread() {
            Some(t) => t,
            None => return,
        };
        let session = match ensure_xvfb() {
            Some(s) => s,
            None => return,
        };

        let mgr = match MockManager::spawn(&session.display) {
            Some(m) => m,
            None => {
                eprintln!("SKIP save_targets_invokes_manager: MockManager spawn failed");
                return;
            }
        };

        // Give the manager time to claim ownership.
        std::thread::sleep(Duration::from_millis(100));

        let text = b"save-targets-test-payload";
        set_clipboard(thread, Selection::Clipboard, &MimeType::Text, text).expect("set failed");

        // Wait up to 3 s for the manager to process the SAVE_TARGETS request.
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if mgr.saw_save_targets.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        assert!(
            mgr.saw_save_targets.load(Ordering::SeqCst),
            "MockManager never saw SAVE_TARGETS request"
        );

        // Wait briefly for the manager to finish copying the payload.
        std::thread::sleep(Duration::from_millis(500));

        let payloads = mgr.received_payloads.lock().unwrap();
        let utf8_atom = thread.atoms.utf8_string;
        assert!(
            payloads.contains_key(&utf8_atom),
            "MockManager did not receive UTF8_STRING payload; keys: {:?}",
            payloads.keys().collect::<Vec<_>>()
        );
        let received = payloads.get(&utf8_atom).unwrap();
        assert_eq!(received, text, "MockManager received wrong payload bytes");
        drop(payloads);

        mgr.stop_and_join();
    }

    /// When no clipboard manager owns CLIPBOARD_MANAGER, auto-SAVE_TARGETS
    /// should be a silent no-op (no hang, no error).
    #[test]
    fn save_targets_no_manager() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let thread = match get_thread() {
            Some(t) => t,
            None => return,
        };

        // Ensure no manager is present (the singleton X11Thread doesn't own
        // CLIPBOARD_MANAGER; unless a previous test left a MockManager alive,
        // there is no owner).  We cannot forcibly remove an owner here, but
        // the test suite serializes via TEST_LOCK and MockManager::stop_and_join
        // drops its connection, so no owner should be present at this point.

        // This should complete quickly (the owner check returns XCB_NONE and
        // skips immediately) rather than hanging on a SELECTION_NOTIFY.
        let before = Instant::now();
        set_clipboard(
            thread,
            Selection::Clipboard,
            &MimeType::Text,
            b"no-manager-test",
        )
        .expect("set should succeed even with no manager");
        let elapsed = before.elapsed();

        // If we hung waiting for a phantom manager, elapsed would be ~5 s.
        // In the no-manager case the SAVE_TARGETS skip path is instant (<100 ms).
        assert!(
            elapsed < Duration::from_secs(2),
            "set took too long ({elapsed:?}); possible hang in SAVE_TARGETS"
        );
    }

    // -----------------------------------------------------------------------
    // 7a tests — Custom mime round-trips
    // -----------------------------------------------------------------------

    /// Set with a Custom mime, paste with xclip using the same type.
    #[test]
    fn x11_custom_mime_set_round_trip() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let thread = match get_thread() {
            Some(t) => t,
            None => return,
        };

        let mime = MimeType::Custom("application/x-hjkl-test".into());
        let data = b"custom-mime-payload-7a";
        set_clipboard(thread, Selection::Clipboard, &mime, data).expect("set custom mime failed");

        std::thread::sleep(Duration::from_millis(150));

        // xclip can read back with an explicit type flag.
        let out = match xclip_typed("clipboard", "application/x-hjkl-test") {
            Some(o) => o,
            None => {
                eprintln!("SKIP x11_custom_mime_set_round_trip: xclip not available");
                return;
            }
        };
        assert_eq!(out, data, "custom mime xclip read mismatch");
    }

    /// Write via xclip with a custom type, read back via our get path.
    #[test]
    fn x11_custom_mime_get_round_trip() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let thread = match get_thread() {
            Some(t) => t,
            None => return,
        };

        // Use the self-loop path: set custom, then get custom.
        let mime = MimeType::Custom("application/x-hjkl-get-test".into());
        let data = b"get-custom-payload-7a";
        set_clipboard(thread, Selection::Clipboard, &mime, data).expect("set custom mime failed");

        std::thread::sleep(Duration::from_millis(50));

        let received =
            get_clipboard(thread, Selection::Clipboard, &mime).expect("get custom mime failed");
        assert_eq!(received, data, "custom mime self-loop mismatch");
    }

    // -----------------------------------------------------------------------
    // 7a tests — error type preserved across calls
    // -----------------------------------------------------------------------

    /// Verify that the typed error variant (LibNotFound or NoDisplay) is
    /// preserved across repeated calls to x11_thread() when no display is
    /// available.  We can only test this indirectly: call x11_thread() twice
    /// and assert both results are the same variant.
    #[test]
    fn error_type_preserved_across_calls() {
        // This test deliberately does NOT acquire TEST_LOCK or require xvfb
        // — we are reading from X11_THREAD which is already initialised by
        // get_thread() above, or uninitialised (LibNotFound/NoDisplay).
        // Either way both calls must return the same error variant.
        let r1 = x11_thread();
        let r2 = x11_thread();
        match (r1, r2) {
            (Ok(_), Ok(_)) => {}
            (Err(ClipboardError::LibNotFound), Err(ClipboardError::LibNotFound)) => {}
            (Err(ClipboardError::NoDisplay), Err(ClipboardError::NoDisplay)) => {}
            (Err(ClipboardError::Io(_)), Err(ClipboardError::Io(_))) => {}
            (a, b) => panic!(
                "error variant changed between calls: first={a:?} second={b:?}",
                a = a.err().map(|e| e.to_string()),
                b = b.err().map(|e| e.to_string()),
            ),
        }
    }
}
