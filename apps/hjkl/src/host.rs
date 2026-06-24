//! `TuiHost` — terminal `Host` adapter for the standalone `hjkl` binary.
//!
//! Implements [`hjkl_engine::Host`] with the minimum surface needed for
//! `Editor<B, H>` to compile against this host:
//!
//! - Owns the runtime [`Viewport`] (engine reads/writes scroll offsets,
//!   the renderer publishes width/height per frame).
//! - Tracks last-emitted [`CursorShape`] so the renderer can repaint.
//! - Real clipboard via [`hjkl_clipboard::Clipboard`] — our in-house
//!   cross-platform clipboard with OSC 52 fallback (SSH-aware). Clipboard
//!   construction is fallible; if probe fails, ops silently no-op.
//! - Unit `Intent` type — the standalone binary doesn't fan out LSP /
//!   fold / buffer-list requests yet. Phase 4+ swaps this for a real
//!   enum once intents start firing.
//!
//! Mirrors the shape of `sqeel-tui::SqeelHost` so the eventual switch
//! to `Editor<B, H>` is a single-callsite swap.

use hjkl_clipboard::{Capabilities, Clipboard, MimeType, Selection};
use hjkl_engine::{CursorShape, Host, Viewport};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// Process-global gate: when set, [`TuiHost::new`] builds with **no** clipboard
/// backend.
///
/// The msgpack-rpc servers (`--nvim-api`, `--embed`, `--headless`) own stdout
/// for the protocol stream. On a display-less host (e.g. CI) the clipboard
/// probe falls back to OSC 52, which writes escape sequences to **stdout** — the
/// rpc pipe — corrupting the protocol and deadlocking the peer (#264). Those
/// entry points call [`disable_clipboard_for_rpc`] before constructing any
/// `App`, so every editor host in rpc mode runs clipboard-free. The client owns
/// the clipboard in an embedding scenario.
static RPC_NO_CLIPBOARD: AtomicBool = AtomicBool::new(false);

/// Disable the OS clipboard for all subsequently-built [`TuiHost`]s in this
/// process. Call once, at rpc-server startup, before building the `App`. See
/// [`RPC_NO_CLIPBOARD`].
pub fn disable_clipboard_for_rpc() {
    RPC_NO_CLIPBOARD.store(true, Ordering::Relaxed);
}

/// Standalone-binary host adapter. See module docs.
pub struct TuiHost {
    last_cursor_shape: CursorShape,
    started: Instant,
    clipboard: Option<Clipboard>,
    viewport: Viewport,
}

impl TuiHost {
    /// Build a host with a sensible default viewport. The renderer
    /// overwrites `width` / `height` per frame from the editor pane's
    /// chunk rect, so the initial 80x24 is just a placeholder for the
    /// pre-first-draw state.
    pub fn new() -> Self {
        Self {
            last_cursor_shape: CursorShape::Block,
            started: Instant::now(),
            // Rpc modes (--nvim-api/--embed/--headless) own stdout; a clipboard
            // probe that lands on OSC 52 would corrupt the protocol (#264).
            clipboard: if RPC_NO_CLIPBOARD.load(Ordering::Relaxed) {
                None
            } else {
                Clipboard::new().ok()
            },
            viewport: Viewport {
                top_row: 0,
                top_col: 0,
                width: 80,
                height: 24,
                ..Viewport::default()
            },
        }
    }

    /// Most recent cursor shape requested by the engine. Renderer reads.
    pub fn cursor_shape(&self) -> CursorShape {
        self.last_cursor_shape
    }

    /// Borrow the active clipboard, if construction succeeded.
    ///
    /// Used by the `:clipboard` ex command to display backend kind +
    /// capabilities without the host having to pre-format the status string.
    pub fn clipboard(&self) -> Option<&Clipboard> {
        self.clipboard.as_ref()
    }
}

impl Default for TuiHost {
    fn default() -> Self {
        Self::new()
    }
}

impl Host for TuiHost {
    type Intent = ();

    fn write_clipboard(&mut self, text: String) {
        if let Some(cb) = &self.clipboard
            && cb.capabilities().contains(Capabilities::WRITE)
        {
            let _ = cb.set(Selection::Clipboard, MimeType::Text, text.as_bytes());
        }
    }

    fn read_clipboard(&mut self) -> Option<String> {
        let cb = self.clipboard.as_ref()?;
        // Skip the round-trip when the active backend can't read at all
        // (OSC 52 over SSH, Mock without preset_get, etc).
        if !cb.capabilities().contains(Capabilities::READ) {
            return None;
        }
        let bytes = cb.get(Selection::Clipboard, MimeType::Text).ok()?;
        String::from_utf8(bytes).ok()
    }

    fn now(&self) -> std::time::Duration {
        self.started.elapsed()
    }

    fn should_cancel(&self) -> bool {
        false
    }

    fn prompt_search(&mut self) -> Option<String> {
        // Phase 4+: hook into the command-line prompt overlay.
        None
    }

    fn emit_cursor_shape(&mut self, shape: CursorShape) {
        self.last_cursor_shape = shape;
    }

    fn emit_intent(&mut self, _intent: Self::Intent) {
        // Unit intent — nothing to fan out in Phase 1.
    }

    fn viewport(&self) -> &Viewport {
        &self.viewport
    }

    fn viewport_mut(&mut self) -> &mut Viewport {
        &mut self.viewport
    }
}
