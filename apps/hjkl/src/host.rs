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

use hjkl_clipboard::{Clipboard, MimeType, Selection};
use hjkl_engine::{CursorShape, Host, Viewport};
use std::time::Instant;

/// Standalone-binary host adapter. See module docs.
pub struct TuiHost {
    last_cursor_shape: CursorShape,
    started: Instant,
    cancel: bool,
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
            cancel: false,
            clipboard: Clipboard::new().ok(),
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
    #[allow(dead_code)] // Phase 2: renderer wires this in.
    pub fn cursor_shape(&self) -> CursorShape {
        self.last_cursor_shape
    }

    /// Set / clear the cancellation flag (`Ctrl-C` handler hooks here
    /// once the event loop lands in Phase 2).
    #[allow(dead_code)] // Phase 2: event loop wires this in.
    pub fn set_cancel(&mut self, cancel: bool) {
        self.cancel = cancel;
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
        if let Some(cb) = &self.clipboard {
            let _ = cb.set(Selection::Clipboard, MimeType::Text, text.as_bytes());
        }
    }

    fn read_clipboard(&mut self) -> Option<String> {
        let cb = self.clipboard.as_ref()?;
        let bytes = cb.get(Selection::Clipboard, MimeType::Text).ok()?;
        String::from_utf8(bytes).ok()
    }

    fn now(&self) -> std::time::Duration {
        self.started.elapsed()
    }

    fn should_cancel(&self) -> bool {
        self.cancel
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
