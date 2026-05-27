//! App-side syntax wiring shim.
//!
//! Delegates to [`hjkl_syntax::SyntaxLayer`] (fully synchronous — no worker
//! thread) and converts [`hjkl_syntax::RenderOutput`] (renderer-agnostic
//! [`hjkl_theme::StyleSpec`] spans) to the ratatui-typed output that
//! `syntax_glue.rs` installs onto the editor.

use std::path::Path;
use std::sync::Arc;

use hjkl_bonsai::{DotFallbackTheme, Theme};
use hjkl_buffer_tui::Sign;
use hjkl_engine::Query;
use hjkl_syntax_tui::render_output_to_tui;

use hjkl_lang::LanguageDirectory;

// Re-export agnostic types used by app/mod.rs and syntax_glue.rs.
pub use hjkl_syntax::{BufferId, LoadEvent, LoadEventKind, SetLanguageOutcome};

// ---------------------------------------------------------------------------
// TUI-typed RenderOutput (wiring shim)
// ---------------------------------------------------------------------------

/// TUI-layer render output: spans have been converted to
/// `ratatui::style::Style` by `hjkl-syntax-tui`.
#[derive(Debug, Clone)]
pub struct RenderOutput {
    /// Per-row span table with ratatui styles.
    pub spans: Vec<Vec<(usize, usize, ratatui::style::Style)>>,
    /// Diagnostic gutter signs (ratatui-styled via `hjkl-syntax-tui`).
    pub signs: Vec<Sign>,
    /// `(dirty_gen, viewport_top, viewport_height)` cache key.
    pub key: (u64, usize, usize),
}

impl PartialEq for RenderOutput {
    fn eq(&self, other: &Self) -> bool {
        self.spans == other.spans
            && self.signs.len() == other.signs.len()
            && self
                .signs
                .iter()
                .zip(other.signs.iter())
                .all(|(a, b)| a.row == b.row && a.ch == b.ch && a.priority == b.priority)
    }
}

/// Convert a [`hjkl_syntax::RenderOutput`] to the TUI-typed [`RenderOutput`].
fn convert_output(raw: hjkl_syntax::RenderOutput) -> RenderOutput {
    let (spans, signs) = render_output_to_tui(&raw);
    RenderOutput {
        spans,
        signs,
        key: raw.key,
    }
}

// ---------------------------------------------------------------------------
// SyntaxLayer — TUI wiring shim
// ---------------------------------------------------------------------------

/// App-side syntax layer. Delegates to [`hjkl_syntax::SyntaxLayer`] and
/// converts outputs to ratatui types on the way out.
pub struct SyntaxLayer {
    inner: hjkl_syntax::SyntaxLayer,
}

impl SyntaxLayer {
    /// Create a new layer with the given theme and language directory.
    pub fn new(theme: Arc<dyn Theme + Send + Sync>, directory: Arc<LanguageDirectory>) -> Self {
        Self {
            inner: hjkl_syntax::SyntaxLayer::new(theme, directory),
        }
    }

    /// Detect the language for `path` and attach a grammar.
    pub fn set_language_for_path(&mut self, id: BufferId, path: &Path) -> SetLanguageOutcome {
        self.inner.set_language_for_path(id, path)
    }

    /// Resolve a path to its canonical language name without loading any grammar.
    pub fn language_name_for_path(&self, path: &Path) -> Option<String> {
        self.inner.directory().name_for_path(path)
    }

    /// Poll all in-flight grammar loads. Call once per tick.
    pub fn poll_pending_loads(&mut self) -> Vec<LoadEvent> {
        self.inner.poll_pending_loads()
    }

    /// Drop all state for a buffer. Call on close.
    pub fn forget(&mut self, id: BufferId) {
        self.inner.forget(id);
    }

    /// Swap the active theme.
    pub fn set_theme(&mut self, theme: Arc<dyn Theme + Send + Sync>) {
        self.inner.set_theme(theme);
    }

    /// Push colorizer state from the app's active editor settings.
    /// No-op when the values are unchanged so per-frame pushes are cheap.
    pub fn set_colorizer(&mut self, enabled: bool, filetypes: Vec<String>) {
        self.inner.set_colorizer(enabled, filetypes);
    }

    /// Drop the buffer's retained tree. Next render_viewport reparses from scratch.
    pub fn reset(&mut self, id: BufferId) {
        self.inner.reset(id);
    }

    /// Apply a batch of engine `ContentEdit`s to the retained tree synchronously.
    pub fn apply_edits(&mut self, id: BufferId, edits: &[hjkl_engine::ContentEdit]) {
        self.inner.apply_edits(id, edits);
    }

    /// Render spans for the visible viewport. Fully synchronous.
    pub fn render_viewport(
        &mut self,
        id: BufferId,
        buffer: &impl Query,
        viewport_top: usize,
        viewport_height: usize,
    ) -> Option<RenderOutput> {
        let raw = self
            .inner
            .render_viewport(id, buffer, viewport_top, viewport_height)?;
        Some(convert_output(raw))
    }
}

// ---------------------------------------------------------------------------
// Factory helpers
// ---------------------------------------------------------------------------

/// Build a `SyntaxLayer` using the given theme + language directory.
pub fn layer_with_theme(
    theme: Arc<DotFallbackTheme>,
    directory: Arc<LanguageDirectory>,
) -> SyntaxLayer {
    SyntaxLayer::new(theme, directory)
}

/// Build a `SyntaxLayer` with hjkl-bonsai's bundled dark theme.
/// Used by tests.
#[cfg(test)]
pub fn default_layer() -> SyntaxLayer {
    let directory = Arc::new(LanguageDirectory::new().expect("language directory"));
    SyntaxLayer::new(Arc::new(DotFallbackTheme::dark()), directory)
}

// ---------------------------------------------------------------------------
// Tests (TUI-side — validate the conversion layer)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use hjkl_buffer::Buffer;
    use std::path::Path;

    /// Test buffer id.
    const TID: BufferId = 0;

    #[test]
    fn render_viewport_with_no_language_returns_none() {
        let buf = Buffer::from_str("hello world");
        let mut layer = default_layer();
        assert!(
            !layer
                .set_language_for_path(TID, Path::new("a.unknownext"))
                .is_known()
        );
        assert!(layer.render_viewport(TID, &buf, 0, 10).is_none());
    }

    #[test]
    fn apply_edits_with_no_language_is_noop() {
        let mut layer = default_layer();
        let edits = vec![hjkl_engine::ContentEdit {
            start_byte: 0,
            old_end_byte: 0,
            new_end_byte: 1,
            start_position: (0, 0),
            old_end_position: (0, 0),
            new_end_position: (0, 1),
        }];
        layer.apply_edits(TID, &edits);
        // No panic — that's the test.
    }

    #[test]
    fn set_language_for_path_returns_unknown_for_unrecognized_extension() {
        let mut layer = default_layer();
        let outcome = layer.set_language_for_path(TID, Path::new("a.zzznope_not_real"));
        assert!(
            !outcome.is_known(),
            "expected Unknown for unrecognized extension"
        );
        assert!(matches!(outcome, SetLanguageOutcome::Unknown));
    }

    #[test]
    fn poll_pending_loads_drains_ready_handles() {
        let mut layer = default_layer();
        let events = layer.poll_pending_loads();
        assert!(
            events.is_empty(),
            "expected no events with no pending loads"
        );
    }

    #[test]
    #[ignore = "network + compiler: needs tree-sitter-rust grammar"]
    fn parse_and_render_small_rust_buffer() {
        let buf = Buffer::from_str("fn main() { let x = 1; }\n");
        let mut layer = default_layer();
        assert!(
            layer
                .set_language_for_path(TID, Path::new("a.rs"))
                .is_known()
        );
        let out = layer
            .render_viewport(TID, &buf, 0, 10)
            .expect("render output");
        assert!(
            out.spans.iter().any(|r| !r.is_empty()),
            "expected at least one styled span"
        );
    }

    #[test]
    #[ignore = "network + compiler: needs tree-sitter-rust grammar"]
    fn diagnostics_emit_sign_for_syntax_error() {
        let buf = Buffer::from_str("fn main() {\nlet x = ;\n}\n");
        let mut layer = default_layer();
        layer.set_language_for_path(TID, Path::new("a.rs"));
        let out = layer.render_viewport(TID, &buf, 0, 10).unwrap();
        assert!(
            !out.signs.is_empty(),
            "expected at least one diagnostic sign for `let x = ;`"
        );
        assert!(
            out.signs.iter().any(|s| s.row == 1 && s.ch == 'E'),
            "expected an 'E' sign on row 1; got {:?}",
            out.signs
        );
    }

    #[test]
    #[ignore = "network + compiler: needs tree-sitter-rust grammar"]
    fn incremental_path_matches_cold_for_small_edit() {
        let pre = Buffer::from_str("fn main() { let x = 1; }");
        let mut layer = default_layer();
        layer.set_language_for_path(TID, Path::new("a.rs"));
        let _ = layer.render_viewport(TID, &pre, 0, 10).unwrap();
        layer.apply_edits(
            TID,
            &[hjkl_engine::ContentEdit {
                start_byte: 3,
                old_end_byte: 3,
                new_end_byte: 4,
                start_position: (0, 3),
                old_end_position: (0, 3),
                new_end_position: (0, 4),
            }],
        );
        let post = Buffer::from_str("fn Ymain() { let x = 1; }");
        let inc = layer.render_viewport(TID, &post, 0, 10).unwrap();
        let mut cold_layer = default_layer();
        cold_layer.set_language_for_path(TID, Path::new("a.rs"));
        let cold = cold_layer.render_viewport(TID, &post, 0, 10).unwrap();
        assert_eq!(inc.spans, cold.spans);
    }
}
