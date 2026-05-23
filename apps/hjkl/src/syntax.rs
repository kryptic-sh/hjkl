//! App-side syntax wiring shim.
//!
//! Delegates all background-thread machinery to [`hjkl_syntax::SyntaxLayer`]
//! and [`hjkl_syntax::SyntaxWorker`]. This file is the TUI-side adapter:
//! it converts [`hjkl_syntax::RenderOutput`] (renderer-agnostic
//! [`hjkl_theme::StyleSpec`] spans) to the ratatui-typed output that
//! `syntax_glue.rs` installs onto the editor.
//!
//! **Public surface for `apps/hjkl` internal use only.** The agnostic types
//! now live in `hjkl-syntax`; the TUI conversion lives in `hjkl-syntax-tui`.

use std::path::Path;
use std::sync::Arc;

use hjkl_bonsai::{DotFallbackTheme, Theme};
use hjkl_buffer_tui::Sign;
use hjkl_engine::Query;
use hjkl_syntax_tui::render_output_to_tui;

use hjkl_lang::LanguageDirectory;

// Re-export agnostic types used by app/mod.rs and syntax_glue.rs.
pub use hjkl_syntax::{
    BufferId, LoadEvent, LoadEventKind, ParseKind, ParseKindKind, PerfBreakdown, SetLanguageOutcome,
};

// ---------------------------------------------------------------------------
// TUI-typed RenderOutput (wiring shim)
// ---------------------------------------------------------------------------

/// TUI-layer render output: spans have been converted to
/// `ratatui::style::Style` by `hjkl-syntax-tui`.
///
/// Wraps the fields that `syntax_glue.rs` and `app/mod.rs` read directly.
#[derive(Debug, Clone)]
pub struct RenderOutput {
    /// Buffer this result belongs to (same semantics as
    /// [`hjkl_syntax::RenderOutput::buffer_id`]).
    pub buffer_id: BufferId,
    /// Per-row span table with ratatui styles.
    pub spans: Vec<Vec<(usize, usize, ratatui::style::Style)>>,
    /// Diagnostic gutter signs (ratatui-styled via `hjkl-syntax-tui`).
    pub signs: Vec<Sign>,
    /// `(dirty_gen, viewport_top, viewport_height)` cache key.
    pub key: (u64, usize, usize),
    /// Sub-step timing breakdown (available for perf_overlay; accessed via
    /// `syntax.last_perf` on the layer rather than per-output in practice).
    #[allow(dead_code)]
    pub perf: PerfBreakdown,
    /// Which region this result covers (Viewport / Top / Bottom).
    pub kind: ParseKind,
}

impl PartialEq for RenderOutput {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.spans == other.spans
            && self.signs.len() == other.signs.len()
            && self
                .signs
                .iter()
                .zip(other.signs.iter())
                .all(|(a, b)| a.row == b.row && a.ch == b.ch && a.priority == b.priority)
    }
}

/// Convert a [`hjkl_syntax::RenderOutput`] to the TUI-typed [`RenderOutput`]
/// used by `syntax_glue.rs`. Spans and signs are materialised via
/// `hjkl-syntax-tui`.
fn convert_output(raw: hjkl_syntax::RenderOutput) -> RenderOutput {
    let (spans, signs) = render_output_to_tui(&raw);
    RenderOutput {
        buffer_id: raw.buffer_id,
        spans,
        signs,
        key: raw.key,
        perf: raw.perf,
        kind: raw.kind,
    }
}

// ---------------------------------------------------------------------------
// SyntaxLayer — TUI wiring shim
// ---------------------------------------------------------------------------

/// App-side syntax layer. Delegates background-thread work to
/// [`hjkl_syntax::SyntaxLayer`] and converts outputs to ratatui types on
/// the way out.
pub struct SyntaxLayer {
    inner: hjkl_syntax::SyntaxLayer,
    /// Last perf breakdown received. Kept here so `app/mod.rs` can read it
    /// via `self.syntax.last_perf` without going through `inner`.
    pub last_perf: PerfBreakdown,
}

impl SyntaxLayer {
    /// Create a new layer with the given theme and language directory.
    pub fn new(theme: Arc<dyn Theme + Send + Sync>, directory: Arc<LanguageDirectory>) -> Self {
        Self {
            inner: hjkl_syntax::SyntaxLayer::new(theme, directory),
            last_perf: PerfBreakdown::default(),
        }
    }

    /// Detect the language for `path` and ship it to the worker.
    pub fn set_language_for_path(&mut self, id: BufferId, path: &Path) -> SetLanguageOutcome {
        self.inner.set_language_for_path(id, path)
    }

    /// Resolve a path to its canonical language name (e.g. `"rust"` for
    /// `foo.rs`) without loading any grammar. Returns `None` for unknown
    /// extensions. Used to seed `Editor::set_filetype` on file open so
    /// filetype-aware features (`gcc`, comment continuation, …) light up
    /// regardless of whether the grammar itself is installed yet.
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

    /// Synchronous viewport-only preview render.
    /// Returns `None` when no language is attached or the viewport is empty.
    pub fn preview_render(
        &self,
        id: BufferId,
        buffer: &impl Query,
        viewport_top: usize,
        viewport_height: usize,
    ) -> Option<RenderOutput> {
        let raw = self
            .inner
            .preview_render(id, buffer, viewport_top, viewport_height)?;
        Some(convert_output(raw))
    }

    /// Ask the worker to drop this buffer's retained tree on the next parse.
    pub fn reset(&mut self, id: BufferId) {
        self.inner.reset(id);
    }

    /// Buffer a batch of engine `ContentEdit`s to be shipped to the worker
    /// on the next `submit_render`.
    pub fn apply_edits(&mut self, id: BufferId, edits: &[hjkl_engine::ContentEdit]) {
        self.inner.apply_edits(id, edits);
    }

    /// Submit a parse + render job to the worker. Returns `None` when no
    /// language is attached; `Some(source_build_us)` on submit.
    pub fn submit_render(
        &mut self,
        id: BufferId,
        buffer: &impl Query,
        viewport_top: usize,
        viewport_height: usize,
        kind: ParseKind,
    ) -> Option<u128> {
        self.inner
            .submit_render(id, buffer, viewport_top, viewport_height, kind)
    }

    /// Drain the most recent render result (oldest discarded).
    #[allow(dead_code)]
    pub fn take_result(&mut self) -> Option<RenderOutput> {
        let raw = self.inner.take_result()?;
        self.last_perf = raw.perf;
        Some(convert_output(raw))
    }

    /// Drain all render results produced since the last drain.
    pub fn take_all_results(&mut self) -> Vec<RenderOutput> {
        let raws = self.inner.take_all_results();
        if let Some(last) = raws.last() {
            self.last_perf = last.perf;
        }
        raws.into_iter().map(convert_output).collect()
    }

    /// Block up to `timeout` for the next result, then drain any others.
    pub fn wait_result(&mut self, timeout: std::time::Duration) -> Option<RenderOutput> {
        let raw = self.inner.wait_result(timeout)?;
        self.last_perf = raw.perf;
        Some(convert_output(raw))
    }

    /// Block up to `timeout` for the first result, then drain ALL in arrival
    /// order. Used for big-jump paths.
    pub fn wait_all_results(&mut self, timeout: std::time::Duration) -> Vec<RenderOutput> {
        let raws = self.inner.wait_all_results(timeout);
        if let Some(last) = raws.last() {
            self.last_perf = last.perf;
        }
        raws.into_iter().map(convert_output).collect()
    }

    /// Block up to `timeout` for the first result. Used at startup.
    pub fn wait_for_initial_result(
        &mut self,
        timeout: std::time::Duration,
    ) -> Option<RenderOutput> {
        self.wait_result(timeout)
    }

    /// Test-only alias for [`Self::wait_for_initial_result`].
    #[cfg(test)]
    pub fn wait_for_result(&mut self, timeout: std::time::Duration) -> Option<RenderOutput> {
        self.wait_for_initial_result(timeout)
    }
}

// ---------------------------------------------------------------------------
// Factory helpers (kept for call-site compat in app/mod.rs)
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
    use std::time::Duration;

    fn submit_and_wait(
        layer: &mut SyntaxLayer,
        buf: &Buffer,
        top: usize,
        height: usize,
    ) -> Option<RenderOutput> {
        layer.submit_render(TID, buf, top, height, ParseKind::Viewport)?;
        layer.wait_for_result(Duration::from_secs(5))
    }

    /// Test buffer id.
    const TID: BufferId = 0;

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
        let out = submit_and_wait(&mut layer, &buf, 0, 10).expect("worker output");
        assert_eq!(out.spans.len(), buf.row_count());
        assert!(
            out.spans.iter().any(|r| !r.is_empty()),
            "expected at least one styled span"
        );
    }

    #[test]
    fn submit_with_no_language_returns_none() {
        let buf = Buffer::from_str("hello world");
        let mut layer = default_layer();
        assert!(
            !layer
                .set_language_for_path(TID, Path::new("a.unknownext"))
                .is_known()
        );
        assert!(
            layer
                .submit_render(TID, &buf, 0, 10, ParseKind::Viewport)
                .is_none()
        );
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
    #[ignore = "network + compiler: needs tree-sitter-rust grammar"]
    fn first_load_highlights_entire_viewport() {
        let mut content = String::new();
        for i in 0..50 {
            content.push_str(&format!("fn f{i}() {{ let x = {i}; }}\n"));
        }
        let buf = Buffer::from_str(content.strip_suffix('\n').unwrap_or(&content));

        let mut layer = default_layer();
        assert!(
            layer
                .set_language_for_path(TID, Path::new("a.rs"))
                .is_known()
        );
        let out = submit_and_wait(&mut layer, &buf, 0, 30).unwrap();

        for (r, row) in out.spans.iter().take(30).enumerate() {
            assert!(
                !row.is_empty(),
                "row {r} has no highlight spans on first load (content: {:?})",
                buf.line(r)
            );
        }
    }

    #[test]
    #[ignore = "network + compiler: needs tree-sitter-rust grammar"]
    fn first_load_full_viewport_matches_full_parse() {
        let mut content = String::new();
        for i in 0..50 {
            content.push_str(&format!("fn f{i}() {{ let x = {i}; }}\n"));
        }
        let buf = Buffer::from_str(content.strip_suffix('\n').unwrap_or(&content));

        let mut narrow = default_layer();
        narrow.set_language_for_path(TID, Path::new("a.rs"));
        let narrow_out = submit_and_wait(&mut narrow, &buf, 0, 30).unwrap();

        let mut full = default_layer();
        full.set_language_for_path(TID, Path::new("a.rs"));
        let full_out = submit_and_wait(&mut full, &buf, 0, 100).unwrap();

        for r in 0..30 {
            assert_eq!(
                narrow_out.spans[r], full_out.spans[r],
                "row {r} differs between viewport-scoped and full parse"
            );
        }
    }

    #[test]
    #[ignore = "network + compiler: needs tree-sitter-rust grammar"]
    fn diagnostics_emit_sign_for_syntax_error() {
        let buf = Buffer::from_str("fn main() {\nlet x = ;\n}\n");
        let mut layer = default_layer();
        layer.set_language_for_path(TID, Path::new("a.rs"));
        let out = submit_and_wait(&mut layer, &buf, 0, 10).unwrap();
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
    fn diagnostics_clean_source_no_signs() {
        let buf = Buffer::from_str("fn main() { let x = 1; }\n");
        let mut layer = default_layer();
        layer.set_language_for_path(TID, Path::new("a.rs"));
        let out = submit_and_wait(&mut layer, &buf, 0, 10).unwrap();
        assert!(
            out.signs.is_empty(),
            "expected no signs; got {:?}",
            out.signs
        );
    }

    #[test]
    #[ignore = "network + compiler: needs tree-sitter-rust grammar"]
    fn incremental_path_matches_cold_for_small_edit() {
        let pre = Buffer::from_str("fn main() { let x = 1; }");
        let mut layer = default_layer();
        layer.set_language_for_path(TID, Path::new("a.rs"));
        let _ = submit_and_wait(&mut layer, &pre, 0, 10).unwrap();
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
        let inc = submit_and_wait(&mut layer, &post, 0, 10).unwrap();
        let mut cold_layer = default_layer();
        cold_layer.set_language_for_path(TID, Path::new("a.rs"));
        let cold = submit_and_wait(&mut cold_layer, &post, 0, 10).unwrap();
        assert_eq!(inc.spans, cold.spans);
    }

    #[test]
    fn worker_handles_quit_cleanly() {
        let layer = default_layer();
        drop(layer);
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
    #[ignore = "disk-state dependent: result depends on whether rust grammar is pre-installed"]
    fn set_language_for_path_returns_loading_for_uncached_grammar() {
        let mut layer = default_layer();
        let t0 = std::time::Instant::now();
        let outcome = layer.set_language_for_path(TID, Path::new("a.rs"));
        let elapsed = t0.elapsed();
        assert!(
            elapsed.as_millis() < 500,
            "set_language_for_path blocked for {}ms — must be non-blocking",
            elapsed.as_millis()
        );
        assert!(
            matches!(outcome, SetLanguageOutcome::Loading(_)),
            "expected Loading on cold disk"
        );
    }

    #[test]
    #[ignore = "network + compiler: needs tree-sitter-rust grammar"]
    fn reset_pending_request_is_consumed_once() {
        let buf = Buffer::from_str("fn main() {}");
        let mut layer = default_layer();
        layer.set_language_for_path(TID, Path::new("a.rs"));
        layer.reset(TID);
        assert!(layer.inner.client_pending_reset(TID));
        let _ = submit_and_wait(&mut layer, &buf, 0, 10).unwrap();
        assert!(
            !layer.inner.client_pending_reset(TID),
            "pending_reset should clear after submit"
        );
    }

    #[test]
    #[ignore = "network + compiler: needs tree-sitter-rust grammar"]
    fn forget_drops_buffer_state() {
        let buf = Buffer::from_str("fn main() {}");
        let mut layer = default_layer();
        layer.set_language_for_path(TID, Path::new("a.rs"));
        let _ = submit_and_wait(&mut layer, &buf, 0, 10).unwrap();
        assert!(layer.inner.has_client(TID));
        layer.forget(TID);
        assert!(!layer.inner.has_client(TID));
    }
}

#[cfg(test)]
mod perf_smoke {
    use super::*;
    use hjkl_buffer::Buffer;
    use std::path::Path;
    use std::time::{Duration, Instant};

    #[test]
    #[ignore = "network + compiler: needs tree-sitter-rust grammar"]
    fn big_rs_smoke() {
        let path = Path::new("/tmp/big.rs");
        if !path.exists() {
            eprintln!("/tmp/big.rs not present; skipping perf smoke");
            return;
        }
        let content = std::fs::read_to_string(path).unwrap();
        let buf = Buffer::from_str(content.strip_suffix('\n').unwrap_or(&content));
        let mut layer = default_layer();
        const TID: BufferId = 0;
        assert!(layer.set_language_for_path(TID, path).is_known());

        let t0 = Instant::now();
        layer.submit_render(TID, &buf, 0, 50, ParseKind::Viewport);
        let main_t = t0.elapsed();
        let out = layer.wait_for_result(Duration::from_secs(10));
        eprintln!(
            "first submit_render main-thread: {:?}, worker turnaround total: {:?}",
            main_t,
            t0.elapsed()
        );
        assert!(out.is_some(), "first parse should produce output");

        let t0 = Instant::now();
        let mut main_total = Duration::ZERO;
        for top in 0..100 {
            let s = Instant::now();
            layer.submit_render(TID, &buf, top * 100, 50, ParseKind::Viewport);
            main_total += s.elapsed();
        }
        while layer.take_result().is_some() {}
        eprintln!(
            "100 viewport scrolls: total wall {:?}, main-thread total {:?} (avg {:?}/submit)",
            t0.elapsed(),
            main_total,
            main_total / 100
        );

        let lines = buf.lines().to_vec();
        let mut new_lines = lines.clone();
        new_lines[50_000].insert(0, 'X');
        let post = Buffer::from_str(&new_lines.join("\n"));
        let edit_byte = (0..50_000).map(|r| lines[r].len() + 1).sum::<usize>();
        layer.apply_edits(
            TID,
            &[hjkl_engine::ContentEdit {
                start_byte: edit_byte,
                old_end_byte: edit_byte,
                new_end_byte: edit_byte + 1,
                start_position: (50_000, 0),
                old_end_position: (50_000, 0),
                new_end_position: (50_000, 1),
            }],
        );
        let t = Instant::now();
        layer.submit_render(TID, &post, 0, 50, ParseKind::Viewport);
        let main_us = t.elapsed();
        let out = layer.wait_for_result(Duration::from_secs(10));
        eprintln!(
            "post-edit submit: main-thread {:?}, worker total {:?} (per-step: {:?})",
            main_us,
            t.elapsed(),
            out.as_ref().map(|o| o.perf),
        );
    }
}
