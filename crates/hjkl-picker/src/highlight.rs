//! Pluggable preview-pane highlighter.
//!
//! `hjkl-picker` is intentionally agnostic about how preview content gets
//! syntax-coloured. Consumers implement [`PreviewHighlighter`] and pass it to
//! [`crate::preview_pane`]; the picker hands over the preview file's path and
//! raw bytes and renders whatever spans the consumer returns.
//!
//! Typical implementations:
//!
//! - **Tree-sitter / bonsai**: route through the consumer's grammar resolver
//!   (see `apps/hjkl/src/app/syntax_glue.rs::App::preview_spans_for`).
//! - **LSP semantic tokens**: ask the language server.
//! - **Regex / hand-rolled**: produce spans directly.
//! - **None**: use [`PlainPreview`] for monochrome preview.

use std::path::Path;

use crate::preview::PreviewSpans;

/// Provides syntax-highlight spans for the preview pane.
///
/// Called once per preview refresh from [`crate::preview_pane`]. Implementations
/// should be cheap on cache-hit and may run grammar / LSP work asynchronously
/// in the background — return [`PreviewSpans::default()`] while in flight; the
/// next render pass will pick up the resolved spans.
pub trait PreviewHighlighter {
    /// Compute spans for `bytes`, dispatched by `path`. The path is whatever the
    /// active [`crate::PickerLogic::preview_path`] returned and may not exist
    /// on disk (it may also be a virtual buffer-tab path).
    fn spans_for(&self, path: &Path, bytes: &[u8]) -> PreviewSpans;

    /// Viewport-aware variant. Called by [`crate::preview_pane`] with the
    /// preview pane's current top row and visible height so consumers can
    /// clip the underlying highlighter's byte range to what's on screen.
    ///
    /// The default implementation delegates to [`Self::spans_for`], so
    /// existing implementations remain source-compatible. Consumers with
    /// expensive highlighters (tree-sitter with injections, etc.) should
    /// override this and clip their work to the visible window.
    fn spans_for_viewport(
        &self,
        path: &Path,
        bytes: &[u8],
        top_row: usize,
        height: usize,
    ) -> PreviewSpans {
        let _ = (top_row, height);
        self.spans_for(path, bytes)
    }
}

/// No-op highlighter — returns empty spans, preview renders monochrome.
///
/// Useful for consumers that don't ship a syntax layer, for tests, and as a
/// drop-in default when the preview pane should explicitly skip highlighting.
pub struct PlainPreview;

impl PreviewHighlighter for PlainPreview {
    fn spans_for(&self, _path: &Path, _bytes: &[u8]) -> PreviewSpans {
        PreviewSpans::default()
    }
}
