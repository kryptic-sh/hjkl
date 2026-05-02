//! Wayland clipboard bg thread — data-control bind, set, clear (CLIPBOARD only).
//!
//! Phase 6b: singleton bg thread that connects to the Wayland compositor,
//! binds `ext_data_control_manager_v1`, creates a data device, and services
//! Set / Clear operations. Read path (Get / Available) and PRIMARY come in 6c.
//!
//! No libwayland-client — all wire protocol is hand-rolled via wayland_wire.rs
//! and wayland_socket.rs from phase 6a.
//!
//! Event loop: poll(2) on the socket fd (50 ms timeout) + non-blocking drain of
//! the mpsc inbox each iteration. Same latency trade-off as x11_thread.rs.

use std::collections::HashMap;
use std::ffi::c_int;
use std::sync::{Arc, Condvar, Mutex, OnceLock, mpsc};
use std::time::Duration;

use crate::{ClipboardError, MimeType, Selection};

use super::wayland::WaylandConnection;
use super::wayland_socket::WaylandSocket;
use super::wayland_wire::{encode_message, encode_string, encode_u32, parse_string};

// ---------------------------------------------------------------------------
// Wayland object IDs — well-known (from open()) and client-allocated.
// ---------------------------------------------------------------------------

// Reserved by protocol / phase 6a connection open.
const WL_DISPLAY_ID: u32 = 1;
// const WL_REGISTRY_ID: u32 = 2;  // used during open()
// const WL_CALLBACK_ID: u32 = 3;  // used during open()

// Client allocations start at 100 for clarity (avoids confusion with
// compositor-allocated IDs which are typically low numbered).
const FIRST_CLIENT_ID: u32 = 100;

// ---------------------------------------------------------------------------
// ext_data_control_v1 interface names
// ---------------------------------------------------------------------------

const EXT_DATA_CONTROL_MANAGER: &str = "ext_data_control_manager_v1";
const WL_SEAT: &str = "wl_seat";

// ---------------------------------------------------------------------------
// Wayland opcodes (request side — what WE send)
// ---------------------------------------------------------------------------

// wl_display
const WL_DISPLAY_SYNC: u16 = 0;
const WL_DISPLAY_GET_REGISTRY: u16 = 1;

// wl_registry
const WL_REGISTRY_BIND: u16 = 0;

// ext_data_control_manager_v1 requests
const EXT_MANAGER_CREATE_DATA_SOURCE: u16 = 0;
const EXT_MANAGER_GET_DATA_DEVICE: u16 = 1;

// ext_data_control_device_v1 requests
const EXT_DEVICE_SET_SELECTION: u16 = 0;

// ext_data_control_source_v1 requests
const EXT_SOURCE_OFFER: u16 = 0;
const EXT_SOURCE_DESTROY: u16 = 1;

// ---------------------------------------------------------------------------
// Wayland event opcodes (what WE receive)
// ---------------------------------------------------------------------------

// wl_display events
const WL_DISPLAY_ERROR: u16 = 0;
const WL_DISPLAY_DELETE_ID: u16 = 1;

// wl_registry events
// const WL_REGISTRY_GLOBAL: u16 = 0; // handled in open()

// wl_callback events
const WL_CALLBACK_DONE: u16 = 0;

// ext_data_control_source_v1 events
const EXT_SOURCE_SEND: u16 = 0;
const EXT_SOURCE_CANCELLED: u16 = 1;

// ext_data_control_device_v1 events (we mostly ignore these in 6b)
// const EXT_DEVICE_DATA_OFFER: u16 = 0;
// const EXT_DEVICE_SELECTION: u16 = 1;
// const EXT_DEVICE_FINISHED: u16 = 2;

// ---------------------------------------------------------------------------
// MIME type strings for ext_data_control_source
// ---------------------------------------------------------------------------

/// MIME types we advertise for MimeType::Text.
const TEXT_MIME_TYPES: &[&str] = &[
    "text/plain;charset=utf-8",
    "text/plain",
    "UTF8_STRING",
    "STRING",
];

/// MIME types we advertise for MimeType::Html.
const HTML_MIME_TYPES: &[&str] = &["text/html"];

/// MIME types we advertise for MimeType::Rtf.
const RTF_MIME_TYPES: &[&str] = &["text/rtf", "application/rtf"];

/// MIME types we advertise for MimeType::UriList.
const URI_LIST_MIME_TYPES: &[&str] = &["text/uri-list"];

/// MIME types we advertise for MimeType::Png.
const PNG_MIME_TYPES: &[&str] = &["image/png"];

fn mimes_for(mime: &MimeType) -> &'static [&'static str] {
    match mime {
        MimeType::Text => TEXT_MIME_TYPES,
        MimeType::Html => HTML_MIME_TYPES,
        MimeType::Rtf => RTF_MIME_TYPES,
        MimeType::UriList => URI_LIST_MIME_TYPES,
        MimeType::Png => PNG_MIME_TYPES,
        MimeType::Custom(_) => &[], // handled separately
    }
}

// ---------------------------------------------------------------------------
// Op / Request types
// ---------------------------------------------------------------------------

pub(crate) enum WaylandOp {
    Set {
        sel: Selection,
        mime: MimeType,
        bytes: Vec<u8>,
    },
    Clear {
        sel: Selection,
    },
}

pub(crate) enum WaylandOpResult {
    Set(Result<(), ClipboardError>),
    Clear(Result<(), ClipboardError>),
}

pub(crate) struct WaylandRequest {
    pub op: WaylandOp,
    pub reply: crate::reply::Reply<WaylandOpResult>,
}

// ---------------------------------------------------------------------------
// WaylandThread public handle
// ---------------------------------------------------------------------------

pub(crate) struct WaylandThread {
    tx: mpsc::Sender<WaylandRequest>,
}

impl WaylandThread {
    fn new() -> Result<Self, ClipboardError> {
        // Open connection and probe globals on the calling thread so we can
        // return ClipboardError immediately on failure.
        let conn = WaylandConnection::open()?;

        // Check for required globals before handing off to the thread.
        if conn.find_global(EXT_DATA_CONTROL_MANAGER).is_none() {
            // GNOME case: no data-control protocol available.
            // 6c will wire OSC52 fallback here.
            return Err(ClipboardError::FocusRequired);
        }
        if conn.find_global(WL_SEAT).is_none() {
            return Err(ClipboardError::FocusRequired);
        }

        let (tx, rx) = mpsc::channel::<WaylandRequest>();

        std::thread::Builder::new()
            .name("hjkl-clipboard-wayland".into())
            .spawn(move || {
                let mut state = match WaylandState::init(conn) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("hjkl-clipboard wayland thread: init failed: {e}");
                        return;
                    }
                };
                run_loop(&mut state, rx);
            })
            .expect("failed to spawn Wayland bg thread");

        Ok(Self { tx })
    }

    /// Send an op and block until the bg thread replies.
    pub(crate) fn send_sync(&self, op: WaylandOp) -> Result<WaylandOpResult, ClipboardError> {
        let pair = Arc::new((Mutex::new(None::<WaylandOpResult>), Condvar::new()));
        let reply = crate::reply::Reply::Sync(Arc::clone(&pair));

        self.tx.send(WaylandRequest { op, reply }).map_err(|_| {
            ClipboardError::Io(std::io::Error::other("wayland thread inbox closed"))
        })?;

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

static WAYLAND_THREAD: OnceLock<Result<WaylandThread, String>> = OnceLock::new();

/// Return the process-global Wayland thread, or an error if unavailable.
///
/// # Known limitation (Phase 7 fix required)
///
/// OnceLock memoises the first result. If the first `Clipboard::new()` call
/// gets an Io error, subsequent calls see the same Io error even after the
/// environment changes. Phase 7 must fix this (e.g. by storing error kind
/// separately or making ClipboardError Clone).
pub(crate) fn wayland_thread() -> Result<&'static WaylandThread, ClipboardError> {
    WAYLAND_THREAD
        .get_or_init(|| WaylandThread::new().map_err(|e| e.to_string()))
        .as_ref()
        .map_err(|s| ClipboardError::Io(std::io::Error::other(s.as_str())))
}

// ---------------------------------------------------------------------------
// Per-source state tracked by the thread
// ---------------------------------------------------------------------------

struct OwnedSource {
    /// The client-side object id allocated for this source.
    id: u32,
    /// Payloads keyed by MIME type string.
    payloads: HashMap<String, Vec<u8>>,
    /// All advertised MIME type strings (including aliases).
    offered_mimes: Vec<String>,
}

// ---------------------------------------------------------------------------
// Thread-internal state
// ---------------------------------------------------------------------------

struct WaylandState {
    socket: WaylandSocket,
    /// Monotonically increasing id allocator (starts at FIRST_CLIENT_ID after
    /// startup objects were allocated).
    next_id: u32,
    /// Server-side global id (name) for the seat.
    seat_name: u32,
    /// Our bound seat object id.
    seat_id: u32,
    /// Server-side global id (name) for the data-control manager.
    manager_name: u32,
    /// Our bound manager object id.
    manager_id: u32,
    /// Our data-control device object id.
    device_id: u32,
    /// Sync callback object id used during bind round-trip.
    sync_id: u32,
    /// Currently owned clipboard source, if any.
    clipboard_source: Option<OwnedSource>,
}

impl WaylandState {
    fn alloc_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}

// ---------------------------------------------------------------------------
// State initialisation (bind globals, create device)
// ---------------------------------------------------------------------------

fn init_bind(
    socket: &mut WaylandSocket,
    next_id: &mut u32,
    seat_name: u32,
    seat_version: u32,
    manager_name: u32,
) -> Result<(u32, u32, u32, u32), ClipboardError> {
    // Allocate IDs for: registry, sync, seat, manager, device.
    // We re-do registry + sync to get a fresh view during bind.
    // Re-use the object ids allocated in open() for registry(2)/callback(3)
    // but we need NEW ids for seat/manager/device which are post-open.
    let registry_id: u32 = 2; // same as open()

    let seat_id = *next_id;
    *next_id += 1;
    let manager_id = *next_id;
    *next_id += 1;
    let device_id = *next_id;
    *next_id += 1;
    let sync_id = *next_id;
    *next_id += 1;

    // Bind wl_seat.
    send_registry_bind(
        socket,
        registry_id,
        seat_name,
        WL_SEAT,
        seat_version.min(7),
        seat_id,
    )?;

    // Bind ext_data_control_manager_v1.
    send_registry_bind(
        socket,
        registry_id,
        manager_name,
        EXT_DATA_CONTROL_MANAGER,
        1,
        manager_id,
    )?;

    // manager.get_data_device(new_id, seat)
    {
        let mut args = Vec::new();
        encode_u32(&mut args, device_id);
        encode_u32(&mut args, seat_id);
        let msg = encode_message(manager_id, EXT_MANAGER_GET_DATA_DEVICE, &args);
        socket.send(&msg, &[])?;
    }

    // wl_display.sync to confirm the above requests were processed.
    {
        let mut args = Vec::new();
        encode_u32(&mut args, sync_id);
        let msg = encode_message(WL_DISPLAY_ID, WL_DISPLAY_SYNC, &args);
        socket.send(&msg, &[])?;
    }

    // Drain until we see the sync callback.done.
    drain_until_sync(socket, sync_id)?;

    Ok((seat_id, manager_id, device_id, sync_id))
}

/// Send wl_registry.bind for a global (typeless new_id form).
///
/// Wire format for bind with typeless new_id (libwayland convention):
///   name:u32 + interface:string + version:u32 + new_id:u32
fn send_registry_bind(
    socket: &mut WaylandSocket,
    registry_id: u32,
    name: u32,
    interface: &str,
    version: u32,
    new_id: u32,
) -> Result<(), ClipboardError> {
    let mut args = Vec::new();
    encode_u32(&mut args, name);
    encode_string(&mut args, interface);
    encode_u32(&mut args, version);
    encode_u32(&mut args, new_id);
    let msg = encode_message(registry_id, WL_REGISTRY_BIND, &args);
    socket.send(&msg, &[])
}

/// Block-recv until we see wl_callback.done on `sync_id`.
fn drain_until_sync(socket: &mut WaylandSocket, sync_id: u32) -> Result<(), ClipboardError> {
    for _ in 0..4096 {
        socket.recv(true)?;
        while let Some((hdr, _args)) = socket.next_message() {
            if hdr.object_id == sync_id && hdr.opcode == WL_CALLBACK_DONE {
                return Ok(());
            }
            if hdr.object_id == WL_DISPLAY_ID && hdr.opcode == WL_DISPLAY_ERROR {
                return Err(ClipboardError::Io(std::io::Error::other(
                    "wl_display.error during bind sync",
                )));
            }
        }
    }
    Err(ClipboardError::Io(std::io::Error::other(
        "timed out waiting for bind sync callback",
    )))
}

impl WaylandState {
    fn init(conn: WaylandConnection) -> Result<Self, ClipboardError> {
        // Extract the globals we need before consuming conn.
        let seat_global = conn
            .find_global(WL_SEAT)
            .ok_or(ClipboardError::FocusRequired)?;
        let seat_name = seat_global.name;
        let seat_version = seat_global.version;

        let manager_global = conn
            .find_global(EXT_DATA_CONTROL_MANAGER)
            .ok_or(ClipboardError::FocusRequired)?;
        let manager_name = manager_global.name;

        // Destructure conn to get the socket and next_id.
        // WaylandConnection::into_parts() doesn't exist; we need access to socket.
        // We'll use the open() socket and next_id directly.
        let (mut socket, mut next_id) = conn.into_parts();

        // Start client IDs at FIRST_CLIENT_ID to leave room for protocol objects.
        if next_id < FIRST_CLIENT_ID {
            next_id = FIRST_CLIENT_ID;
        }

        let (seat_id, manager_id, device_id, sync_id) = init_bind(
            &mut socket,
            &mut next_id,
            seat_name,
            seat_version,
            manager_name,
        )?;

        Ok(WaylandState {
            socket,
            next_id,
            seat_name,
            seat_id,
            manager_name,
            manager_id,
            device_id,
            sync_id,
            clipboard_source: None,
        })
    }
}

// ---------------------------------------------------------------------------
// Main event loop
// ---------------------------------------------------------------------------

fn run_loop(state: &mut WaylandState, rx: mpsc::Receiver<WaylandRequest>) {
    loop {
        // poll(2) on the socket with a 50ms timeout.
        let socket_readable = poll_socket(state.socket.raw_fd(), 50);

        if socket_readable {
            // Drain all available compositor events.
            if let Err(e) = state.socket.recv(false) {
                eprintln!("hjkl-clipboard wayland: recv error: {e}");
                break;
            }
            dispatch_events(state);
        }

        // Drain the inbox (non-blocking).
        loop {
            match rx.recv_timeout(Duration::from_millis(0)) {
                Ok(req) => handle_op(state, req),
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
    }
}

/// Call poll(2) on the socket fd with a timeout in milliseconds.
/// Returns true if data is available to read.
fn poll_socket(fd: c_int, timeout_ms: i32) -> bool {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: pfd is valid; nfds=1; timeout_ms is i32.
    let ret = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
    ret > 0 && (pfd.revents & libc::POLLIN) != 0
}

// ---------------------------------------------------------------------------
// Event dispatch
// ---------------------------------------------------------------------------

fn dispatch_events(state: &mut WaylandState) {
    // Collect messages to avoid borrow issues — socket.next_message() borrows
    // socket mutably. We snapshot all pending messages first.
    let mut messages: Vec<(super::wayland_wire::MessageHeader, Vec<u8>, Option<c_int>)> =
        Vec::new();

    loop {
        // Check if there's an fd waiting (for data_source.send events).
        let opt_fd = state.socket.next_fd();
        match state.socket.next_message() {
            Some((hdr, args)) => messages.push((hdr, args, opt_fd)),
            None => {
                // Return the fd if we took one but there was no message.
                // We cannot put it back into socket; just close it to avoid leak.
                if let Some(fd) = opt_fd {
                    // SAFETY: fd came from the socket receive queue and is valid.
                    unsafe { libc::close(fd) };
                }
                break;
            }
        }
    }

    for (hdr, args, opt_fd) in messages {
        handle_event(state, hdr.object_id, hdr.opcode, &args, opt_fd);
    }
}

fn handle_event(
    state: &mut WaylandState,
    object_id: u32,
    opcode: u16,
    args: &[u8],
    opt_fd: Option<c_int>,
) {
    if object_id == WL_DISPLAY_ID {
        match opcode {
            WL_DISPLAY_ERROR => {
                eprintln!("hjkl-clipboard wayland: wl_display.error — terminating bg thread");
            }
            WL_DISPLAY_DELETE_ID => {
                // Compositor deleted one of our object ids; no action needed.
            }
            _ => {}
        }
        if let Some(fd) = opt_fd {
            // SAFETY: fd is valid and unclaimed.
            unsafe { libc::close(fd) };
        }
        return;
    }

    // Check if this is from our current clipboard source.
    let is_our_source = state
        .clipboard_source
        .as_ref()
        .map(|s| s.id == object_id)
        .unwrap_or(false);

    if is_our_source {
        match opcode {
            EXT_SOURCE_SEND => handle_source_send(state, args, opt_fd),
            EXT_SOURCE_CANCELLED => handle_source_cancelled(state, opt_fd),
            _ => {
                if let Some(fd) = opt_fd {
                    // SAFETY: fd is valid and unclaimed.
                    unsafe { libc::close(fd) };
                }
            }
        }
        return;
    }

    // Unknown object (data_offer events etc.) — ignore.
    if let Some(fd) = opt_fd {
        // SAFETY: fd is valid and unclaimed.
        unsafe { libc::close(fd) };
    }
}

// ---------------------------------------------------------------------------
// data_source.send event handler
// ---------------------------------------------------------------------------

fn handle_source_send(state: &mut WaylandState, args: &[u8], opt_fd: Option<c_int>) {
    // Event args: mime(string) + fd(out-of-band via SCM_RIGHTS).
    let Some((mime, _rest)) = parse_string(args) else {
        if let Some(fd) = opt_fd {
            // SAFETY: fd is valid; we close it to avoid leaking.
            unsafe { libc::close(fd) };
        }
        return;
    };

    let Some(write_fd) = opt_fd else {
        // No fd means this event is malformed.
        return;
    };

    // Look up payload for this mime type.
    let payload = state
        .clipboard_source
        .as_ref()
        .and_then(|s| s.payloads.get(mime))
        .cloned()
        .unwrap_or_default();

    // Write payload to the pipe write end. The compositor reads from the read end.
    write_to_fd(write_fd, &payload);

    // SAFETY: write_fd is ours (received via SCM_RIGHTS); close after write.
    unsafe { libc::close(write_fd) };
}

/// Write all bytes to fd, handling partial writes.
fn write_to_fd(fd: c_int, data: &[u8]) {
    let mut written = 0;
    while written < data.len() {
        // SAFETY: fd is valid; slice is valid memory.
        let n = unsafe {
            libc::write(
                fd,
                data[written..].as_ptr() as *const libc::c_void,
                data.len() - written,
            )
        };
        if n <= 0 {
            // Write error or EOF; abort.
            break;
        }
        written += n as usize;
    }
}

// ---------------------------------------------------------------------------
// data_source.cancelled event handler
// ---------------------------------------------------------------------------

fn handle_source_cancelled(state: &mut WaylandState, opt_fd: Option<c_int>) {
    // Compositor revoked our selection. Drop the source.
    if let Some(source) = state.clipboard_source.take() {
        // Send source.destroy so the compositor can clean up.
        let msg = encode_message(source.id, EXT_SOURCE_DESTROY, &[]);
        let _ = state.socket.send(&msg, &[]);
    }
    if let Some(fd) = opt_fd {
        // SAFETY: fd is valid and unclaimed.
        unsafe { libc::close(fd) };
    }
}

// ---------------------------------------------------------------------------
// Op handlers
// ---------------------------------------------------------------------------

fn handle_op(state: &mut WaylandState, req: WaylandRequest) {
    let result = match req.op {
        WaylandOp::Set { sel, mime, bytes } => {
            WaylandOpResult::Set(do_set(state, sel, mime, bytes))
        }
        WaylandOp::Clear { sel } => WaylandOpResult::Clear(do_clear(state, sel)),
    };
    req.reply.resolve(result);
}

fn do_set(
    state: &mut WaylandState,
    sel: Selection,
    mime: MimeType,
    bytes: Vec<u8>,
) -> Result<(), ClipboardError> {
    // PRIMARY not implemented until 6c.
    if sel == Selection::Primary {
        return Err(ClipboardError::UnsupportedMime);
    }

    // Destroy existing source if any, then create a fresh one.
    if let Some(old) = state.clipboard_source.take() {
        let msg = encode_message(old.id, EXT_SOURCE_DESTROY, &[]);
        let _ = state.socket.send(&msg, &[]);
    }

    let source_id = state.alloc_id();

    // manager.create_data_source(new_id)
    {
        let mut args = Vec::new();
        encode_u32(&mut args, source_id);
        let msg = encode_message(state.manager_id, EXT_MANAGER_CREATE_DATA_SOURCE, &args);
        state.socket.send(&msg, &[])?;
    }

    // Build the set of MIME types to offer.
    let mimes: Vec<String> = if let MimeType::Custom(ref s) = mime {
        vec![s.clone()]
    } else {
        mimes_for(&mime).iter().map(|s| s.to_string()).collect()
    };

    // Build payloads: primary mime string -> bytes; all aliases serve same bytes.
    let mut payloads: HashMap<String, Vec<u8>> = HashMap::new();
    for m in &mimes {
        payloads.insert(m.clone(), bytes.clone());
    }

    // source.offer(mime) for each offered mime type.
    for m in &mimes {
        let mut args = Vec::new();
        encode_string(&mut args, m);
        let msg = encode_message(source_id, EXT_SOURCE_OFFER, &args);
        state.socket.send(&msg, &[])?;
    }

    // device.set_selection(source)
    {
        let mut args = Vec::new();
        encode_u32(&mut args, source_id);
        let msg = encode_message(state.device_id, EXT_DEVICE_SET_SELECTION, &args);
        state.socket.send(&msg, &[])?;
    }

    state.clipboard_source = Some(OwnedSource {
        id: source_id,
        payloads,
        offered_mimes: mimes,
    });

    Ok(())
}

fn do_clear(state: &mut WaylandState, sel: Selection) -> Result<(), ClipboardError> {
    // PRIMARY not implemented until 6c.
    if sel == Selection::Primary {
        return Err(ClipboardError::UnsupportedMime);
    }

    // Destroy existing source.
    if let Some(source) = state.clipboard_source.take() {
        let msg = encode_message(source.id, EXT_SOURCE_DESTROY, &[]);
        let _ = state.socket.send(&msg, &[]);
    }

    // device.set_selection(0) — null source = NONE (clear).
    {
        let mut args = Vec::new();
        encode_u32(&mut args, 0);
        let msg = encode_message(state.device_id, EXT_DEVICE_SET_SELECTION, &args);
        state.socket.send(&msg, &[])?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Public helpers for lib.rs wiring
// ---------------------------------------------------------------------------

pub(crate) fn set_clipboard(
    thread: &WaylandThread,
    sel: Selection,
    mime: &MimeType,
    bytes: &[u8],
) -> Result<(), ClipboardError> {
    let result = thread.send_sync(WaylandOp::Set {
        sel,
        mime: mime.clone(),
        bytes: bytes.to_vec(),
    })?;
    match result {
        WaylandOpResult::Set(r) => r,
        _ => unreachable!(),
    }
}

pub(crate) fn clear_clipboard(
    thread: &WaylandThread,
    sel: Selection,
) -> Result<(), ClipboardError> {
    let result = thread.send_sync(WaylandOp::Clear { sel })?;
    match result {
        WaylandOpResult::Clear(r) => r,
        _ => unreachable!(),
    }
}

// ---------------------------------------------------------------------------
// WaylandConnection::into_parts()
// (Defined in wayland.rs via a helper; we add it here as a method extension
//  in wayland.rs — we need socket + next_id from the connection.)
// ---------------------------------------------------------------------------
// NOTE: `into_parts()` is implemented in wayland.rs (added in this phase).

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;
    use std::ffi::c_int;
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};

    use super::super::wayland_socket::WaylandSocket;
    use super::super::wayland_wire::{
        encode_message, encode_string, encode_u32, parse_string, parse_u32,
    };

    // One mock compositor for all Wayland thread tests (approach a — shared
    // singleton matching XVFB_SESSION pattern in x11_thread tests).
    static MOCK_SESSION: OnceLock<Option<Arc<MockCompositor>>> = OnceLock::new();
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    // ---------------------------------------------------------------------------
    // Mock compositor
    // ---------------------------------------------------------------------------

    /// State the mock compositor tracks (accessible from test assertions).
    pub(crate) struct MockState {
        /// bound globals: new_id -> (global_name, interface, version)
        pub bound: HashMap<u32, (u32, String, u32)>,
        /// For each created data source: offered mimes.
        pub source_mimes: HashMap<u32, Vec<String>>,
        /// The source id of the most recent set_selection call (0 = cleared).
        pub current_selection: Option<u32>,
        /// Paste results: mime -> bytes the client wrote when we sent send events.
        pub paste_results: HashMap<String, Vec<u8>>,
        /// Object id counter for server-side allocations.
        next_server_id: u32,
        /// Pipe write ends we use to trigger send events, keyed by source id.
        /// The mock server thread reads these to send events to the client.
        pending_sends: Vec<PendingSend>,
        /// Whether a cancelled event was triggered.
        pub cancelled_triggered: bool,
    }

    struct PendingSend {
        mime: String,
        read_fd: c_int,
        write_fd: c_int,
        source_id: u32,
        complete: bool,
    }

    impl Default for MockState {
        fn default() -> Self {
            MockState {
                bound: HashMap::new(),
                source_mimes: HashMap::new(),
                current_selection: None,
                paste_results: HashMap::new(),
                next_server_id: 200,
                pending_sends: Vec::new(),
                cancelled_triggered: false,
            }
        }
    }

    impl MockState {
        fn alloc_server_id(&mut self) -> u32 {
            let id = self.next_server_id;
            self.next_server_id += 1;
            id
        }

        /// Reset between test runs (except bound/device state).
        fn reset(&mut self) {
            self.source_mimes.clear();
            self.current_selection = None;
            self.paste_results.clear();
            self.pending_sends.clear();
            self.cancelled_triggered = false;
        }
    }

    /// Handle for the mock Wayland compositor.
    pub(crate) struct MockCompositor {
        pub socket_path: PathBuf,
        pub state: Arc<Mutex<MockState>>,
        shutdown: Arc<AtomicBool>,
    }

    impl MockCompositor {
        pub(crate) fn socket_path(&self) -> &Path {
            &self.socket_path
        }

        pub(crate) fn state(&self) -> std::sync::MutexGuard<'_, MockState> {
            self.state.lock().unwrap()
        }

        /// Trigger a paste: send data_source.send(mime, write_fd) to the client
        /// via the server thread, then collect bytes from the read_fd.
        ///
        /// This creates a pipe, enqueues a PendingSend in the mock state (the
        /// server thread will detect it and send the event), then reads from
        /// the read_fd until EOF to collect what the client wrote.
        pub(crate) fn trigger_paste(&self, mime: &str) -> Result<Vec<u8>, std::io::Error> {
            // Create a pipe.
            let mut fds = [0i32; 2];
            // SAFETY: pipe2 with O_CLOEXEC is safe to call with a valid [i32;2].
            let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
            if rc != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let read_fd = fds[0];
            let write_fd = fds[1];

            // Enqueue the send event.
            {
                let mut st = self.state.lock().unwrap();
                let source_id = st.current_selection.unwrap_or(0);
                st.pending_sends.push(PendingSend {
                    mime: mime.to_owned(),
                    read_fd,
                    write_fd,
                    source_id,
                    complete: false,
                });
            }

            // Wait for the server thread to process the send and the client to
            // write its response. We poll the complete flag.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                {
                    let st = self.state.lock().unwrap();
                    if st.pending_sends.iter().all(|p| p.complete) {
                        break;
                    }
                }
                if std::time::Instant::now() > deadline {
                    return Err(std::io::Error::other("trigger_paste timed out"));
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }

            // Read from the read_fd until EOF.
            let mut result = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                // SAFETY: read_fd is valid; buf is valid memory.
                let n = unsafe {
                    libc::read(read_fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len())
                };
                if n <= 0 {
                    break;
                }
                result.extend_from_slice(&buf[..n as usize]);
            }
            // SAFETY: read_fd is valid; we close it after reading.
            unsafe { libc::close(read_fd) };

            // Store in paste_results.
            {
                let mut st = self.state.lock().unwrap();
                st.paste_results.insert(mime.to_owned(), result.clone());
            }

            Ok(result)
        }

        pub(crate) fn shutdown(self) {
            self.shutdown.store(true, Ordering::Relaxed);
        }
    }

    /// Spawn a mock Wayland compositor on a temporary socket path.
    pub(crate) fn spawn_mock_compositor(advertise_data_control: bool) -> Arc<MockCompositor> {
        // Use a unique socket path in /tmp.
        let socket_path = PathBuf::from(format!("/tmp/hjkl-clipboard-mock-{}.sock", unsafe {
            libc::getpid()
        }));

        // Remove stale socket if any.
        let _ = std::fs::remove_file(&socket_path);

        let listener = UnixListener::bind(&socket_path).expect("failed to bind mock socket");

        let state = Arc::new(Mutex::new(MockState::default()));
        let shutdown = Arc::new(AtomicBool::new(false));

        let state_clone = Arc::clone(&state);
        let shutdown_clone = Arc::clone(&shutdown);
        let socket_path_clone = socket_path.clone();

        std::thread::Builder::new()
            .name("hjkl-mock-compositor".into())
            .spawn(move || {
                run_mock_compositor(
                    listener,
                    state_clone,
                    shutdown_clone,
                    advertise_data_control,
                    socket_path_clone,
                );
            })
            .expect("failed to spawn mock compositor thread");

        Arc::new(MockCompositor {
            socket_path,
            state,
            shutdown,
        })
    }

    // ---------------------------------------------------------------------------
    // Mock compositor server thread
    // ---------------------------------------------------------------------------

    /// Object type tags for the mock server's object table.
    #[derive(Debug, Clone, PartialEq)]
    enum MockObjectType {
        Display,
        Registry,
        Callback,
        Seat,
        DataControlManager,
        DataControlDevice,
        DataControlSource,
    }

    struct MockServer {
        socket: WaylandSocket,
        objects: HashMap<u32, MockObjectType>,
        next_id: u32,
        state: Arc<Mutex<MockState>>,
        advertise_data_control: bool,
        /// Globals we advertise: (name, interface, version).
        globals: Vec<(u32, &'static str, u32)>,
    }

    impl MockServer {
        fn alloc_id(&mut self) -> u32 {
            let id = self.next_id;
            self.next_id += 1;
            id
        }

        fn send(&self, object_id: u32, opcode: u16, args: &[u8]) {
            let msg = encode_message(object_id, opcode, args);
            let _ = self.socket.send(&msg, &[]);
        }

        fn send_with_fd(&self, object_id: u32, opcode: u16, args: &[u8], fd: c_int) {
            let msg = encode_message(object_id, opcode, args);
            let _ = self.socket.send(&msg, &[fd]);
        }
    }

    fn run_mock_compositor(
        listener: UnixListener,
        state: Arc<Mutex<MockState>>,
        shutdown: Arc<AtomicBool>,
        advertise_data_control: bool,
        _socket_path: PathBuf,
    ) {
        // Accept exactly one connection (our client).
        listener.set_nonblocking(true).ok();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let (stream, _) = loop {
            match listener.accept() {
                Ok(pair) => break pair,
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if shutdown.load(Ordering::Relaxed) || std::time::Instant::now() > deadline {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                }
                Err(_) => return,
            }
        };

        // Wrap the accepted stream in a WaylandSocket-compatible fd.
        // We need raw fd access — extract it via into_raw_fd.
        use std::os::unix::io::IntoRawFd;
        let raw_fd = stream.into_raw_fd();

        // Build a WaylandSocket from the raw fd (using the same internal
        // structure as a client connection — they share the same wire format).
        let socket = unsafe { WaylandSocket::from_raw_fd(raw_fd) };

        let mut globals: Vec<(u32, &'static str, u32)> = vec![(1, WL_SEAT, 7)];
        if advertise_data_control {
            globals.push((2, EXT_DATA_CONTROL_MANAGER, 1));
        }

        let mut server = MockServer {
            socket,
            objects: HashMap::new(),
            next_id: 300,
            state,
            advertise_data_control,
            globals,
        };

        // Register well-known objects.
        server.objects.insert(1, MockObjectType::Display);
        server.objects.insert(2, MockObjectType::Registry);

        run_mock_server_loop(&mut server, shutdown);
    }

    fn run_mock_server_loop(server: &mut MockServer, shutdown: Arc<AtomicBool>) {
        loop {
            if shutdown.load(Ordering::Relaxed) {
                return;
            }

            // Check for pending send events to dispatch.
            dispatch_pending_sends(server);

            // Non-blocking receive.
            if let Err(e) = server.socket.recv(false) {
                let err_str = e.to_string();
                if err_str.contains("closed") || err_str.contains("reset") {
                    return;
                }
                break;
            }

            // Drain messages.
            while let Some((hdr, args)) = server.socket.next_message() {
                handle_mock_message(server, hdr.object_id, hdr.opcode, &args);
            }

            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    fn dispatch_pending_sends(server: &mut MockServer) {
        // Collect work items without holding the lock across the send.
        // Each item: (source_id, mime, write_fd).
        let work: Vec<(u32, String, c_int)> = {
            let mut st = server.state.lock().unwrap();
            let mut items = Vec::new();
            for pending in st.pending_sends.iter_mut() {
                if !pending.complete {
                    items.push((pending.source_id, pending.mime.clone(), pending.write_fd));
                    pending.complete = true;
                    pending.write_fd = -1; // prevent double-close
                }
            }
            items
        };
        // Lock is released here. Now send events without holding state lock.

        for (source_id, mime, write_fd) in work {
            let mut args = Vec::new();
            encode_string(&mut args, &mime);

            // Send data_source.send(mime, write_fd) to the client.
            let msg = encode_message(source_id, EXT_SOURCE_SEND, &args);
            let _ = server.socket.send(&msg, &[write_fd]);

            // Close our copy of the write fd (kernel dups it during SCM_RIGHTS).
            // SAFETY: write_fd is valid and was created by pipe2; we own this copy.
            unsafe { libc::close(write_fd) };
        }
    }

    fn handle_mock_message(server: &mut MockServer, object_id: u32, opcode: u16, args: &[u8]) {
        let obj_type = server.objects.get(&object_id).cloned();

        match obj_type {
            Some(MockObjectType::Display) => handle_mock_display(server, opcode, args),
            Some(MockObjectType::Registry) => handle_mock_registry(server, opcode, args),
            Some(MockObjectType::Callback) => {
                // Callbacks have no requests from the client; ignore.
            }
            Some(MockObjectType::Seat) => {
                // Seat capabilities etc — ignore in 6b.
            }
            Some(MockObjectType::DataControlManager) => handle_mock_manager(server, opcode, args),
            Some(MockObjectType::DataControlDevice) => {
                handle_mock_device(server, object_id, opcode, args)
            }
            Some(MockObjectType::DataControlSource) => {
                handle_mock_source(server, object_id, opcode, args)
            }
            None => {
                // Unknown object; ignore.
            }
        }
    }

    // wl_display request handling (server side).
    fn handle_mock_display(server: &mut MockServer, opcode: u16, args: &[u8]) {
        match opcode {
            0 => {
                // wl_display.sync(new_id) — send wl_callback.done(serial=0).
                if let Some((callback_id, _)) = parse_u32(args) {
                    server.objects.insert(callback_id, MockObjectType::Callback);
                    // wl_callback.done: opcode 0, args: callback_data(u32)
                    let mut done_args = Vec::new();
                    encode_u32(&mut done_args, 0u32); // serial
                    server.send(callback_id, 0, &done_args);
                }
            }
            1 => {
                // wl_display.get_registry(new_id)
                if let Some((registry_id, _)) = parse_u32(args) {
                    server.objects.insert(registry_id, MockObjectType::Registry);
                    // Send wl_registry.global for each advertised global.
                    for (name, interface, version) in &server.globals.clone() {
                        let mut ga = Vec::new();
                        encode_u32(&mut ga, *name);
                        encode_string(&mut ga, interface);
                        encode_u32(&mut ga, *version);
                        // wl_registry.global opcode = 0
                        server.send(registry_id, 0, &ga);
                    }
                }
            }
            _ => {}
        }
    }

    // wl_registry.bind handling.
    fn handle_mock_registry(server: &mut MockServer, opcode: u16, args: &[u8]) {
        if opcode != 0 {
            return; // only bind
        }
        // bind args: name(u32) + interface(string) + version(u32) + new_id(u32)
        let Some((name, rest)) = parse_u32(args) else {
            return;
        };
        let Some((interface, rest)) = parse_string(rest) else {
            return;
        };
        let Some((version, rest)) = parse_u32(rest) else {
            return;
        };
        let Some((new_id, _)) = parse_u32(rest) else {
            return;
        };

        let obj_type = match interface {
            "wl_seat" => MockObjectType::Seat,
            "ext_data_control_manager_v1" => MockObjectType::DataControlManager,
            _ => return,
        };

        server.objects.insert(new_id, obj_type);

        let mut st = server.state.lock().unwrap();
        st.bound
            .insert(new_id, (name, interface.to_owned(), version));
    }

    // ext_data_control_manager_v1 request handling.
    fn handle_mock_manager(server: &mut MockServer, opcode: u16, args: &[u8]) {
        match opcode {
            0 => {
                // create_data_source(new_id)
                if let Some((new_id, _)) = parse_u32(args) {
                    server
                        .objects
                        .insert(new_id, MockObjectType::DataControlSource);
                    let mut st = server.state.lock().unwrap();
                    st.source_mimes.insert(new_id, Vec::new());
                }
            }
            1 => {
                // get_data_device(new_id, seat)
                if let Some((new_id, _)) = parse_u32(args) {
                    server
                        .objects
                        .insert(new_id, MockObjectType::DataControlDevice);
                }
            }
            2 => {
                // destroy — no-op for mock
            }
            _ => {}
        }
    }

    // ext_data_control_device_v1 request handling.
    fn handle_mock_device(server: &mut MockServer, _object_id: u32, opcode: u16, args: &[u8]) {
        match opcode {
            0 => {
                // set_selection(source_id)
                if let Some((source_id, _)) = parse_u32(args) {
                    let mut st = server.state.lock().unwrap();
                    if source_id == 0 {
                        st.current_selection = None;
                    } else {
                        st.current_selection = Some(source_id);
                    }
                }
            }
            1 => {
                // destroy — no-op
            }
            2 => {
                // set_primary_selection — not tested in 6b; ignore.
                let _ = args;
            }
            _ => {}
        }
    }

    // ext_data_control_source_v1 request handling.
    fn handle_mock_source(server: &mut MockServer, object_id: u32, opcode: u16, args: &[u8]) {
        match opcode {
            0 => {
                // offer(mime_type: string)
                if let Some((mime, _)) = parse_string(args) {
                    let mut st = server.state.lock().unwrap();
                    st.source_mimes
                        .entry(object_id)
                        .or_default()
                        .push(mime.to_owned());
                }
            }
            1 => {
                // destroy — remove from object table
                server.objects.remove(&object_id);
            }
            _ => {}
        }
    }

    // ---------------------------------------------------------------------------
    // Test infrastructure — shared mock session
    // ---------------------------------------------------------------------------

    /// Ensure the shared mock compositor is running and return it.
    ///
    /// The mock is started once for the whole test process. Tests reset the
    /// MockState between runs via reset() inside TEST_LOCK.
    fn ensure_mock() -> Option<Arc<MockCompositor>> {
        MOCK_SESSION
            .get_or_init(|| {
                let mock = spawn_mock_compositor(true);

                // Set WAYLAND_DISPLAY to the mock socket path before initialising
                // WAYLAND_THREAD so it connects to our mock, not a real compositor.
                //
                // SAFETY: test-only; single-threaded at this point (OnceLock callback).
                let path = mock.socket_path().to_str().unwrap().to_owned();
                unsafe { std::env::set_var("WAYLAND_DISPLAY", &path) };

                // The socket file is created by UnixListener::bind BEFORE the
                // mock thread is spawned, so we only need to verify the file
                // exists. We do NOT probe with a real connection here — that
                // would consume the mock's single accept slot, leaving the
                // real WaylandThread::new() connection unhandled.
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                while std::time::Instant::now() < deadline {
                    if mock.socket_path().exists() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }

                // Brief sleep so the mock thread's accept loop is spinning
                // before we connect.
                std::thread::sleep(std::time::Duration::from_millis(20));

                // Eagerly initialise WAYLAND_THREAD inside the OnceLock callback to
                // avoid races between "WAYLAND_DISPLAY is set" and "thread is init".
                let _ = wayland_thread();

                Some(mock)
            })
            .as_ref()
            .cloned()
    }

    fn get_thread_for_test() -> Option<&'static WaylandThread> {
        ensure_mock()?;
        match wayland_thread() {
            Ok(t) => Some(t),
            Err(e) => {
                eprintln!("SKIP: wayland_thread failed: {e}");
                None
            }
        }
    }

    // ---------------------------------------------------------------------------
    // Tests
    // ---------------------------------------------------------------------------

    /// Set text then trigger a paste via the mock and assert the bytes match.
    #[test]
    fn mock_compositor_set_then_paste_text() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mock = match ensure_mock() {
            Some(m) => m,
            None => return,
        };
        let thread = match get_thread_for_test() {
            Some(t) => t,
            None => return,
        };

        mock.state().reset();

        let payload = b"hello wayland 6b";
        set_clipboard(thread, Selection::Clipboard, &MimeType::Text, payload)
            .expect("set_clipboard failed");

        // Give the bg thread time to send the protocol messages and the mock
        // to process them.
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Verify mock recorded the selection.
        assert!(
            mock.state().current_selection.is_some(),
            "mock should have a current_selection after set"
        );

        // Trigger a paste for the primary text MIME type.
        let received = mock
            .trigger_paste("text/plain;charset=utf-8")
            .expect("trigger_paste failed");

        assert_eq!(received, payload, "pasted bytes should match what was set");
    }

    /// Set then clear — mock should report no current selection.
    #[test]
    fn mock_compositor_clear_unsets_selection() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mock = match ensure_mock() {
            Some(m) => m,
            None => return,
        };
        let thread = match get_thread_for_test() {
            Some(t) => t,
            None => return,
        };

        mock.state().reset();

        set_clipboard(
            thread,
            Selection::Clipboard,
            &MimeType::Text,
            b"to-be-cleared",
        )
        .expect("set failed");
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(
            mock.state().current_selection.is_some(),
            "selection should be set"
        );

        clear_clipboard(thread, Selection::Clipboard).expect("clear failed");
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(
            mock.state().current_selection.is_none(),
            "selection should be cleared"
        );
    }

    /// Set HTML payload and verify paste returns the correct bytes.
    #[test]
    fn mock_compositor_offer_html() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mock = match ensure_mock() {
            Some(m) => m,
            None => return,
        };
        let thread = match get_thread_for_test() {
            Some(t) => t,
            None => return,
        };

        mock.state().reset();

        let html = b"<b>bold</b>";
        set_clipboard(thread, Selection::Clipboard, &MimeType::Html, html)
            .expect("set html failed");
        std::thread::sleep(std::time::Duration::from_millis(50));

        let received = mock
            .trigger_paste("text/html")
            .expect("trigger_paste html failed");
        assert_eq!(received, html, "html paste mismatch");
    }

    /// Set "hello", then set "world" — paste should return "world".
    #[test]
    fn mock_compositor_replace_selection() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mock = match ensure_mock() {
            Some(m) => m,
            None => return,
        };
        let thread = match get_thread_for_test() {
            Some(t) => t,
            None => return,
        };

        mock.state().reset();

        set_clipboard(thread, Selection::Clipboard, &MimeType::Text, b"hello")
            .expect("set hello failed");
        std::thread::sleep(std::time::Duration::from_millis(50));

        set_clipboard(thread, Selection::Clipboard, &MimeType::Text, b"world")
            .expect("set world failed");
        std::thread::sleep(std::time::Duration::from_millis(50));

        let received = mock
            .trigger_paste("text/plain;charset=utf-8")
            .expect("trigger_paste failed");
        assert_eq!(received, b"world", "expected replaced selection");
    }
}
