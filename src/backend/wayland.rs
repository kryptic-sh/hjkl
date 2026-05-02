//! Linux Wayland clipboard backend.
//!
//! Phase 6a: open the compositor socket, send `wl_display.get_registry` +
//! `wl_display.sync`, collect `wl_registry.global` events until the sync
//! callback fires. Returns the advertised globals so later phases can bind
//! `ext_data_control_v1` / `wlr_data_control_v1` / `zwp_primary_selection_v1`.
//!
//! Clipboard ops (`set`/`get`/`clear`/`available`) remain `unimplemented!()`;
//! they are wired in phases 6b/6c.

use crate::{ClipboardError, MimeType, Selection};

use super::Backend;
use super::wayland_socket::WaylandSocket;
use super::wayland_wire::{encode_message, encode_u32, parse_string, parse_u32};

// ---------------------------------------------------------------------------
// Wayland well-known object IDs and opcodes
// ---------------------------------------------------------------------------

// Object IDs assigned by us at startup (before the compositor assigns dynamic ones).
const WL_DISPLAY_ID: u32 = 1; // always 1 per protocol spec
const WL_REGISTRY_ID: u32 = 2; // we bind get_registry to id=2
const WL_CALLBACK_ID: u32 = 3; // we bind sync callback to id=3

// wl_display request opcodes
const WL_DISPLAY_SYNC: u16 = 0;
const WL_DISPLAY_GET_REGISTRY: u16 = 1;

// wl_display event opcodes
const WL_DISPLAY_ERROR: u16 = 0;
// const WL_DISPLAY_DELETE_ID: u16 = 1; // ignored

// wl_registry event opcodes
const WL_REGISTRY_GLOBAL: u16 = 0;
// const WL_REGISTRY_GLOBAL_REMOVE: u16 = 1; // ignored

// wl_callback event opcode
const WL_CALLBACK_DONE: u16 = 0;

// ---------------------------------------------------------------------------
// Global descriptor
// ---------------------------------------------------------------------------

/// A single global advertised by `wl_registry.global`.
#[derive(Debug, Clone)]
pub(crate) struct Global {
    /// The numeric name (used with `wl_registry.bind`).
    pub name: u32,
    /// The interface name string (e.g. `"wl_seat"`, `"ext_data_control_manager_v1"`).
    pub interface: String,
    /// The maximum version supported by the compositor.
    pub version: u32,
}

// ---------------------------------------------------------------------------
// Connection
// ---------------------------------------------------------------------------

/// A live Wayland connection with the compositor globals probed.
///
/// After `open()` returns, `globals()` lists every interface the compositor
/// advertised during the registry probe.  Clipboard operations will be added in
/// phase 6b/6c; the `Backend` impl stubs remain `unimplemented!()`.
pub(crate) struct WaylandConnection {
    socket: WaylandSocket,
    globals: Vec<Global>,
    /// Next unused new_id to allocate when binding globals.
    next_id: u32,
}

impl WaylandConnection {
    /// Connect to the Wayland compositor and probe available globals.
    ///
    /// Steps:
    /// 1. Open the Unix socket.
    /// 2. Send `wl_display.get_registry(new_id=2)`.
    /// 3. Send `wl_display.sync(new_id=3)` — the compositor will reply with
    ///    `wl_callback.done` after it has flushed all pending `wl_registry.global`
    ///    events.
    /// 4. Receive messages until `wl_callback.done` on object 3.
    ///    Collect `wl_registry.global` events on object 2.
    pub(crate) fn open() -> Result<Self, ClipboardError> {
        let mut socket = WaylandSocket::connect()?;

        // wl_display.get_registry(new_id = WL_REGISTRY_ID)
        {
            let mut args = Vec::new();
            encode_u32(&mut args, WL_REGISTRY_ID);
            let msg = encode_message(WL_DISPLAY_ID, WL_DISPLAY_GET_REGISTRY, &args);
            socket.send(&msg, &[])?;
        }

        // wl_display.sync(new_id = WL_CALLBACK_ID)
        // The compositor will emit wl_callback.done after flushing global events.
        {
            let mut args = Vec::new();
            encode_u32(&mut args, WL_CALLBACK_ID);
            let msg = encode_message(WL_DISPLAY_ID, WL_DISPLAY_SYNC, &args);
            socket.send(&msg, &[])?;
        }

        let mut globals = Vec::new();
        let mut done = false;

        // Receive events until wl_callback.done fires.
        // Use a generous iteration cap to guard against runaway compositors.
        for _ in 0..4096 {
            // Block until data is available.
            socket.recv(true)?;

            // Drain all complete messages from the buffer.
            while let Some((hdr, args)) = socket.next_message() {
                match (hdr.object_id, hdr.opcode) {
                    // wl_registry.global: name(u32) + interface(string) + version(u32)
                    (WL_REGISTRY_ID, WL_REGISTRY_GLOBAL) => {
                        if let Some(g) = parse_global(&args) {
                            globals.push(g);
                        }
                    }

                    // wl_callback.done: callback_data(u32) — we don't need the value.
                    (WL_CALLBACK_ID, WL_CALLBACK_DONE) => {
                        done = true;
                    }

                    // wl_display.error: object_id(u32) + code(u32) + message(string)
                    (WL_DISPLAY_ID, WL_DISPLAY_ERROR) => {
                        let msg = parse_display_error(&args);
                        return Err(ClipboardError::io(std::io::Error::other(format!(
                            "wl_display error: {msg}"
                        ))));
                    }

                    // Everything else (delete_id, global_remove, …) — ignore.
                    _ => {}
                }

                if done {
                    break;
                }
            }

            if done {
                break;
            }
        }

        if !done {
            return Err(ClipboardError::io_other(
                "timed out waiting for wl_callback.done",
            ));
        }

        // Next allocatable id: wl_display=1, registry=2, callback=3 → start at 4.
        Ok(WaylandConnection {
            socket,
            globals,
            next_id: 4,
        })
    }

    /// All globals advertised by the compositor.
    #[allow(dead_code)]
    pub(crate) fn globals(&self) -> &[Global] {
        &self.globals
    }

    /// Find the first global with the given interface name.
    pub(crate) fn find_global(&self, interface: &str) -> Option<&Global> {
        self.globals.iter().find(|g| g.interface == interface)
    }

    /// Allocate a fresh Wayland new_id.
    #[allow(dead_code)]
    pub(crate) fn alloc_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Borrow the underlying socket.
    #[allow(dead_code)]
    pub(crate) fn socket_mut(&mut self) -> &mut WaylandSocket {
        &mut self.socket
    }

    /// Consume the connection and return (socket, next_id) for the bg thread.
    ///
    /// After this call the caller owns the socket and the id allocator state.
    /// The registry probe globals are discarded (they were already read).
    pub(crate) fn into_parts(self) -> (WaylandSocket, u32) {
        (self.socket, self.next_id)
    }
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

/// Parse a `wl_registry.global` event args: name(u32) + interface(string) + version(u32).
fn parse_global(args: &[u8]) -> Option<Global> {
    let (name, rest) = parse_u32(args)?;
    let (interface, rest) = parse_string(rest)?;
    let (version, _) = parse_u32(rest)?;
    Some(Global {
        name,
        interface: interface.to_owned(),
        version,
    })
}

/// Extract a human-readable description from `wl_display.error` args.
fn parse_display_error(args: &[u8]) -> String {
    let Some((obj_id, rest)) = parse_u32(args) else {
        return "(malformed error event)".to_owned();
    };
    let Some((code, rest)) = parse_u32(rest) else {
        return format!("object={obj_id} (malformed code)");
    };
    let msg = if let Some((s, _)) = parse_string(rest) {
        s.to_owned()
    } else {
        "(no message)".to_owned()
    };
    format!("object={obj_id} code={code} msg={msg:?}")
}

// ---------------------------------------------------------------------------
// Backend stub (superseded by WaylandThread — kept for cross-platform compile)
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub(crate) struct WaylandBackend;

impl Backend for WaylandBackend {
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

    /// Probe the compositor globals if a Wayland session is available.
    ///
    /// Skips gracefully when `WAYLAND_DISPLAY` is unset or the socket is absent
    /// (e.g. CI without a Wayland compositor).
    #[test]
    fn probe_globals_if_compositor_available() {
        if std::env::var_os("WAYLAND_DISPLAY").is_none() {
            eprintln!("SKIP probe_globals: no WAYLAND_DISPLAY");
            return;
        }
        match WaylandConnection::open() {
            Ok(conn) => {
                for g in conn.globals() {
                    eprintln!("global: {} v{} (id={})", g.interface, g.version, g.name);
                }
                assert!(!conn.globals().is_empty(), "no globals returned");
            }
            Err(ClipboardError::NoDisplay) | Err(ClipboardError::Io(_)) => {
                eprintln!("SKIP probe_globals: connection failed");
            }
            Err(e) => panic!("unexpected error: {e}"),
        }
    }
}
