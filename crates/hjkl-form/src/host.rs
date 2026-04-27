//! Minimal `Host` impl for per-field editors.

use hjkl_engine::{CursorShape, Host, Viewport};
use std::time::Instant;

/// Host adapter mounted on every text field's `Editor`. Mirrors
/// [`hjkl_engine::DefaultHost`] but lives in this crate so consumers
/// can reach it without the `default` feature gating.
pub struct FormFieldHost {
    last_cursor_shape: CursorShape,
    started: Instant,
    clipboard: Option<String>,
    viewport: Viewport,
}

impl FormFieldHost {
    /// Construct a field host with a one-row default viewport. The
    /// renderer overwrites `width` / `height` per frame from the field's
    /// body rect.
    pub fn new() -> Self {
        Self {
            last_cursor_shape: CursorShape::Block,
            started: Instant::now(),
            clipboard: None,
            viewport: Viewport {
                top_row: 0,
                top_col: 0,
                width: 40,
                height: 1,
                ..Viewport::default()
            },
        }
    }

    /// Most recent cursor shape requested by the engine.
    pub fn cursor_shape(&self) -> CursorShape {
        self.last_cursor_shape
    }
}

impl Default for FormFieldHost {
    fn default() -> Self {
        Self::new()
    }
}

impl Host for FormFieldHost {
    type Intent = ();

    fn write_clipboard(&mut self, text: String) {
        self.clipboard = Some(text);
    }

    fn read_clipboard(&mut self) -> Option<String> {
        self.clipboard.clone()
    }

    fn now(&self) -> std::time::Duration {
        self.started.elapsed()
    }

    fn prompt_search(&mut self) -> Option<String> {
        None
    }

    fn emit_cursor_shape(&mut self, shape: CursorShape) {
        self.last_cursor_shape = shape;
    }

    fn emit_intent(&mut self, _intent: Self::Intent) {}

    fn viewport(&self) -> &Viewport {
        &self.viewport
    }

    fn viewport_mut(&mut self) -> &mut Viewport {
        &mut self.viewport
    }
}
