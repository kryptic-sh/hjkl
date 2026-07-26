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
use hjkl_syntax_tui::render_output_ref_to_tui;

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

/// Convert a borrowed [`hjkl_syntax::RenderOutputRef`] to the TUI-typed
/// [`RenderOutput`].
///
/// Takes the borrowed form so the ratatui-typed table is built directly from
/// the syntax layer's viewport cache — one span-table allocation per
/// recompute instead of two (cache copy, then conversion copy).
fn convert_output(raw: &hjkl_syntax::RenderOutputRef<'_>) -> RenderOutput {
    let (spans, signs) = render_output_ref_to_tui(raw);
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

    /// Push rainbow bracket state from the app's active editor settings.
    /// No-op when the value is unchanged so per-frame pushes are cheap.
    pub fn set_rainbow_brackets(&mut self, enabled: bool) {
        self.inner.set_rainbow_brackets(enabled);
    }

    /// Drop the buffer's retained tree. Next render_viewport reparses from scratch.
    pub fn reset(&mut self, id: BufferId) {
        self.inner.reset(id);
    }

    /// Apply a batch of engine `ContentEdit`s to the retained tree synchronously.
    pub fn apply_edits(&mut self, id: BufferId, edits: &[hjkl_engine::ContentEdit]) {
        self.inner.apply_edits(id, edits);
    }

    /// Extract fold ranges from the retained parse tree (not viewport-bounded).
    ///
    /// Returns `Some(ranges)` when the grammar is attached and the tree has
    /// been parsed — `ranges` may be empty when no bundled `folds.scm` exists
    /// for the grammar or the file has no multi-line foldable nodes.
    ///
    /// Returns `None` when the grammar is not yet ready (still loading, unknown
    /// extension, or no highlighter). Callers must NOT update their dirty_gen
    /// cache when `None` is returned — fold extraction must retry on the next
    /// call once the grammar has loaded.
    ///
    /// Call once per reparse (when dirty_gen changes), not per-frame.
    pub fn extract_fold_ranges(
        &mut self,
        id: BufferId,
        buffer: &impl hjkl_engine::Query,
    ) -> Option<Vec<(usize, usize)>> {
        self.inner.extract_fold_ranges(id, buffer)
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
            .render_viewport_ref(id, buffer, viewport_top, viewport_height)?;
        Some(convert_output(&raw))
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
    use hjkl_buffer::View;
    use std::path::Path;

    /// Test buffer id.
    const TID: BufferId = 0;

    /// The shim's `PartialEq` deliberately compares signs field-by-field
    /// (`row`/`ch`/`priority`) instead of deriving it, because `Sign` carries a
    /// ratatui `Style` that render caching must not key on. No grammar needed.
    #[test]
    fn render_output_eq_ignores_sign_style() {
        let mk = |style: ratatui::style::Style| RenderOutput {
            spans: vec![vec![(0, 2, ratatui::style::Style::default())]],
            signs: vec![Sign {
                row: 1,
                ch: 'E',
                style,
                priority: 100,
            }],
            key: (7, 0, 30),
        };
        let plain = mk(ratatui::style::Style::default());
        let styled = mk(ratatui::style::Style::default().fg(ratatui::style::Color::Red));
        assert_eq!(plain, styled, "sign style must not affect equality");

        let mut differing = plain.clone();
        differing.signs[0].ch = 'W';
        assert_ne!(plain, differing, "sign `ch` must affect equality");
    }

    #[test]
    #[ignore = "network + compiler: needs tree-sitter-rust grammar"]
    fn render_viewport_converts_spans_to_ratatui() {
        let buf = View::from_str("fn main() { let x = 1; }\n");
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
    fn render_viewport_converts_diag_signs_to_tui_signs() {
        let buf = View::from_str("fn main() {\nlet x = ;\n}\n");
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
}
