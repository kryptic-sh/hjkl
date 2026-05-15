use hjkl_engine::{Host, Query};
use std::path::Path;
use std::time::{Duration, Instant};

use hjkl_bonsai::{CommentMarkerPass, Highlighter, Theme};
use hjkl_engine::types::{Attrs, Color as EngineColor, Style as EngineStyle};
use hjkl_picker::PreviewSpans;

use crate::git_worker::GitJob;
use crate::lang::GrammarRequest;
use crate::syntax::LoadEvent;

use super::App;

impl App {
    /// Queue a git diff-sign refresh for the current buffer (throttled).
    /// Non-blocking: submits a job to the background worker.
    pub(crate) fn refresh_git_signs(&mut self) {
        self.refresh_git_signs_inner(false);
    }

    /// Queue a git diff-sign refresh for the current buffer, bypassing
    /// the 250 ms throttle. The result still arrives asynchronously.
    pub(crate) fn refresh_git_signs_force(&mut self) {
        self.refresh_git_signs_inner(true);
    }

    /// Shared submission logic.
    ///
    /// Checks dirty_gen + throttle, then snapshots buffer content and
    /// submits a [`GitJob`] to the background worker. Returns immediately.
    pub(crate) fn refresh_git_signs_inner(&mut self, force: bool) {
        const REFRESH_MIN_INTERVAL: Duration = Duration::from_millis(250);
        let huge_file_lines = self.config.editor.huge_file_threshold;

        let path = match self.active().filename.as_deref() {
            Some(p) => p.to_path_buf(),
            None => {
                let slot = self.active_mut();
                slot.git_signs.clear();
                slot.last_git_dirty_gen = None;
                return;
            }
        };
        let dg = self.active().editor.buffer().dirty_gen();
        if !force && self.active().last_git_dirty_gen == Some(dg) {
            return;
        }
        if !force && self.active().editor.buffer().line_count() >= huge_file_lines {
            return;
        }
        let now = Instant::now();
        if !force && now.duration_since(self.active().last_git_refresh_at) < REFRESH_MIN_INTERVAL {
            return;
        }

        let lines = self.active().editor.buffer().lines();
        let mut bytes = lines.join("\n").into_bytes();
        if !bytes.is_empty() {
            bytes.push(b'\n');
        }
        let buffer_id = self.active().buffer_id;
        self.active_mut().last_git_refresh_at = now;

        self.git_worker.submit(GitJob {
            buffer_id,
            path,
            bytes,
            dirty_gen: dg,
        });
    }

    /// Drain completed git-sign results from the worker and install them
    /// onto their target slots. Called once per event-loop tick.
    ///
    /// Returns `true` when at least one result was installed and a redraw
    /// is needed.
    pub(crate) fn poll_git_signs(&mut self) -> bool {
        let mut redraw = false;
        while let Some(result) = self.git_worker.try_recv() {
            // Find the slot with this buffer_id (may have been deleted; drop).
            if let Some(slot) = self
                .slots
                .iter_mut()
                .find(|s| s.buffer_id == result.buffer_id)
            {
                // Stale check: only install if no newer dirty_gen has overtaken.
                if slot
                    .last_git_dirty_gen
                    .is_none_or(|dg| dg <= result.dirty_gen)
                {
                    slot.git_signs = result.signs;
                    slot.is_untracked = result.is_untracked;
                    slot.last_git_dirty_gen = Some(result.dirty_gen);
                    redraw = true;
                }
            }
        }
        redraw
    }

    /// Poll in-flight async grammar loads and wire any that completed into
    /// the syntax layer.  Called each tick alongside `try_recv_latest` so
    /// freshly-compiled grammars activate without waiting for the next
    /// file-open event.
    ///
    /// Returns `true` when at least one load resolved and a redraw is needed.
    pub(crate) fn poll_grammar_loads(&mut self) -> bool {
        let mut needs_redraw = false;
        // Expire stale grammar-load errors so the indicator clears itself.
        if self
            .grammar_load_error
            .as_ref()
            .is_some_and(|e| e.is_expired())
        {
            self.grammar_load_error = None;
            needs_redraw = true;
        }
        let events = self.syntax.poll_pending_loads();
        if events.is_empty() {
            return needs_redraw;
        }
        let active_id = self.active().buffer_id;
        for event in &events {
            match event {
                LoadEvent::Ready { id, name } => {
                    tracing::debug!("grammar load complete: {name} (buffer {id})");
                    // If the completed load is for the active buffer, clear
                    // the recompute cache key so the next recompute_and_install
                    // submits a fresh parse with the new language.
                    if *id == active_id {
                        self.active_mut().last_recompute_key = None;
                    }
                }
                LoadEvent::Failed { id, name, error } => {
                    tracing::debug!("grammar load failed: {name} (buffer {id}): {error}");
                    self.grammar_load_error = Some(crate::app::GrammarLoadError {
                        name: name.clone(),
                        message: error.clone(),
                        at: Instant::now(),
                    });
                }
            }
        }
        true
    }

    /// Poll in-flight anvil install handles each tick and fan status events
    /// into `status_message` and `anvil_log`.
    ///
    /// Returns `true` when at least one event arrived and a redraw is useful.
    pub(crate) fn poll_anvil_jobs(&mut self) -> bool {
        use hjkl_anvil::InstallStatus;

        let mut redraw = false;
        let mut to_remove: Vec<String> = Vec::new();

        for (name, handle) in self.anvil_handles.iter() {
            while let Some(status) = handle.try_recv() {
                redraw = true;
                let log_line = format_anvil_status(&status);
                self.anvil_log
                    .entry(name.clone())
                    .or_default()
                    .push(log_line);

                match &status {
                    InstallStatus::Done { .. } => {
                        self.status_message = Some(format!("anvil: installed {name}"));
                        to_remove.push(name.clone());
                    }
                    InstallStatus::Failed(reason) => {
                        self.status_message =
                            Some(format!("anvil: {name} failed \u{2014} {reason}"));
                        to_remove.push(name.clone());
                    }
                    InstallStatus::Downloading {
                        bytes_downloaded,
                        total,
                    } => {
                        let pct = match total {
                            Some(t) if *t > 0 => {
                                format!("{}%", (bytes_downloaded * 100) / t)
                            }
                            _ => format!("{bytes_downloaded} bytes"),
                        };
                        self.status_message = Some(format!("anvil: {name} downloading {pct}"));
                    }
                    InstallStatus::Verifying => {
                        self.status_message = Some(format!("anvil: {name} verifying"));
                    }
                    InstallStatus::Extracting => {
                        self.status_message = Some(format!("anvil: {name} extracting"));
                    }
                    InstallStatus::Installing => {
                        self.status_message = Some(format!("anvil: {name} installing"));
                    }
                    InstallStatus::Queued => {
                        // No toast for the queued state — it's transient.
                    }
                    InstallStatus::TofuRecorded { triple, .. } => {
                        self.status_message =
                            Some(format!("anvil: {name} TOFU hash recorded for {triple}"));
                    }
                }
            }
        }

        for name in to_remove {
            self.anvil_handles.remove(&name);
        }

        redraw
    }

    /// Install a worker-produced `RenderOutput` onto the slot whose
    /// `buffer_id` matches `out.buffer_id`.
    ///
    /// - When the result is for the **active** buffer: install spans +
    ///   signs onto the editor immediately and update the slot cache.
    /// - When the result is for a **non-active** buffer: store the output
    ///   in `last_render_output` on that slot so a subsequent `switch_to`
    ///   can restore spans without waiting for a fresh parse (T3 cache).
    ///
    /// Returns `true` if the install ran on the active buffer, `false` for
    /// non-active routes and genuine stale drops (no matching slot).
    pub(crate) fn install_render_result(&mut self, out: crate::syntax::RenderOutput) -> bool {
        let active_id = self.active().buffer_id;

        // Find the slot that owns this buffer_id.
        if let Some(slot_idx) = self.slots.iter().position(|s| s.buffer_id == out.buffer_id) {
            // Always cache the latest completed output on the owning slot.
            self.slots[slot_idx].last_render_output = Some(out.clone());

            if out.buffer_id == active_id {
                // Active buffer: install spans + signs directly.
                let active_idx = self.focused_slot_idx();
                self.slots[active_idx]
                    .editor
                    .install_ratatui_syntax_spans(out.spans);
                self.slots[active_idx].diag_signs = out.signs;
                return true;
            }
            // Non-active buffer: cached above, no live install needed.
            return false;
        }

        // No slot matched (buffer was closed before the result arrived).
        self.syntax_stale_drops = self.syntax_stale_drops.saturating_add(1);
        false
    }

    /// Submit a new viewport-scoped parse on the syntax worker and install
    /// whatever the worker has produced since the last frame.
    pub(crate) fn recompute_and_install(&mut self) {
        const RECOMPUTE_THROTTLE: Duration = Duration::from_millis(100);
        let buffer_id = self.active().buffer_id;
        let (focused_top, focused_height) = {
            let vp = self.active().editor.host().viewport();
            (vp.top_row, vp.height as usize)
        };

        // Compute the union viewport across all windows that show the same
        // slot as the focused window.  This ensures syntax spans are
        // populated for every visible row, not just the focused window's
        // rows.  Without the union, switching focus between two windows on
        // the same buffer leaves one window with un-highlighted rows.
        let active_slot = self.focused_slot_idx();
        let mut union_top = focused_top;
        let mut union_bot = focused_top + focused_height;
        for w in self.windows.iter().flatten() {
            if w.slot == active_slot
                && let Some(rect) = w.last_rect
            {
                union_top = union_top.min(w.top_row);
                union_bot = union_bot.max(w.top_row + rect.height as usize);
            }
        }
        let top = union_top;
        let height = union_bot - union_top;

        // T1: Over-provision the parse range to 3× the visible height
        // (one viewport above + current + one viewport below). The
        // math lives in `hjkl_buffer::over_provisioned_range` so future
        // host crates (floem GUI, web …) compute the same range from
        // their own viewport without re-deriving it.
        let line_count = self.active().editor.buffer().line_count() as usize;
        let (oversize_top, oversize_height) =
            hjkl_buffer::over_provisioned_range(top, height, line_count);

        let dg = self.active().editor.buffer().dirty_gen();
        let key = (dg, top, height);

        let prev_dirty_gen = self
            .active()
            .last_recompute_key
            .map(|(prev_dg, _, _)| prev_dg);

        let t_total = Instant::now();
        let mut submitted = false;
        if self.active().last_recompute_key == Some(key) {
            self.recompute_hits = self.recompute_hits.saturating_add(1);
        } else {
            let buffer_changed = self
                .active()
                .last_recompute_key
                .map(|(prev_dg, _, _)| prev_dg != dg)
                .unwrap_or(true);
            let now = Instant::now();
            if buffer_changed
                && now.duration_since(self.active().last_recompute_at) < RECOMPUTE_THROTTLE
            {
                self.recompute_throttled = self.recompute_throttled.saturating_add(1);
            } else {
                self.recompute_runs = self.recompute_runs.saturating_add(1);
                // Split borrow: get a raw pointer to the buffer so `self.syntax`
                // can be borrowed mutably without fighting the borrow checker on
                // `self.slots`. Safety: the buffer lives inside `self.slots[active]`
                // which is not touched inside `submit_render`.
                let submit_result = {
                    let active_idx = self.focused_slot_idx();
                    let buf = self.slots[active_idx].editor.buffer();
                    // T1: Submit oversized range so ahead-of-scroll spans are ready.
                    self.syntax
                        .submit_render(buffer_id, buf, oversize_top, oversize_height)
                };
                if submit_result.is_some() {
                    submitted = true;
                    self.active_mut().last_recompute_at = Instant::now();
                    self.active_mut().last_recompute_key = Some(key);
                }
            }
        }

        // T2: Pre-warm other open slots. The per-buffer dedup on the queue
        // ensures this never starves the active buffer's request — active
        // was enqueued first and the worker drains FIFO. If the active
        // buffer's result is not yet back, we still queue the pre-warms
        // so the worker can pipeline: it processes active, then the others.
        let active_idx = self.focused_slot_idx();
        let slot_indices: Vec<usize> = (0..self.slots.len()).filter(|&i| i != active_idx).collect();
        for slot_idx in slot_indices {
            let slot_buf_id = self.slots[slot_idx].buffer_id;
            let (slot_top, slot_height) = {
                let vp = self.slots[slot_idx].editor.host().viewport();
                (vp.top_row, vp.height as usize)
            };
            // Over-provision the secondary slots too so switching into
            // them is likely already covered. Same host-agnostic helper.
            let slot_line_count = self.slots[slot_idx].editor.buffer().line_count() as usize;
            let (slot_oversize_top, slot_oversize_height) =
                hjkl_buffer::over_provisioned_range(slot_top, slot_height, slot_line_count);
            let buf = self.slots[slot_idx].editor.buffer();
            self.syntax
                .submit_render(slot_buf_id, buf, slot_oversize_top, slot_oversize_height);
        }

        // Non-blocking drain of ALL results. Routes each completed result
        // to the correct slot's editor + cache (T3).
        let t_install = Instant::now();
        let all_results = self.syntax.take_all_results();
        let _ = prev_dirty_gen;
        let mut active_installed = false;
        for out in all_results {
            if self.install_render_result(out) {
                active_installed = true;
            }
        }
        self.last_install_us = if active_installed {
            t_install.elapsed().as_micros()
        } else {
            0
        };
        self.last_perf = self.syntax.last_perf;

        let t_git = Instant::now();
        self.refresh_git_signs();
        self.last_git_us = t_git.elapsed().as_micros();
        self.last_recompute_us = t_total.elapsed().as_micros();
        let _ = submitted;
    }

    /// Compute syntax highlight spans for a one-off preview snippet
    /// (`path`, `bytes`). Used by the picker preview pane so the picker
    /// itself stays bonsai-agnostic — sources only ship the buffer
    /// contents and a path, this helper handles language resolution
    /// and the actual highlighter call.
    ///
    /// Async-safe on the UI thread:
    /// - **Cached** grammar → highlight immediately and return spans.
    /// - **Loading** grammar → drop the handle (the bonsai pool keeps
    ///   compiling) and return empty spans for this frame; the next
    ///   call after the grammar lands picks up Cached.
    /// - **Unknown** path → empty spans.
    ///
    /// The per-language `Highlighter` cache lives on `App` so a Rust
    /// preview triggers one parser construction; subsequent Rust files
    /// (or buffer/rg pickers in the same session) reuse it.
    pub fn preview_spans_for(&self, path: &Path, bytes: &[u8]) -> PreviewSpans {
        self.preview_spans_for_range(path, bytes, 0..bytes.len())
    }

    /// Viewport-clipped variant of [`Self::preview_spans_for`]. The parent
    /// parse still runs over the full `bytes` (tree-sitter has no partial-
    /// parse API for a fresh tree), but the injection scan + child highlights
    /// are restricted to `byte_range`. For markdown — the only common grammar
    /// with high injection density — this is the difference between paying
    /// for every fence in the file vs. only the fences on screen.
    pub fn preview_spans_for_range(
        &self,
        path: &Path,
        bytes: &[u8],
        byte_range: std::ops::Range<usize>,
    ) -> PreviewSpans {
        let grammar = match self.directory.request_for_path(path) {
            GrammarRequest::Cached(g) => g,
            GrammarRequest::Loading { .. } | GrammarRequest::Unknown => {
                return PreviewSpans::default();
            }
        };
        let name = grammar.name().to_string();
        let mut cache = match self.preview_highlighters.lock() {
            Ok(c) => c,
            Err(_) => return PreviewSpans::default(),
        };
        let h = match cache.entry(name) {
            std::collections::hash_map::Entry::Occupied(o) => o.into_mut(),
            std::collections::hash_map::Entry::Vacant(v) => match Highlighter::new(grammar) {
                Ok(h) => v.insert(h),
                Err(_) => return PreviewSpans::default(),
            },
        };
        h.reset();
        h.parse_initial(bytes);
        // Resolve injected languages via the async loader so unknown grammars
        // kick off a background clone+compile (global spinner reflects this);
        // returns None while loading so the parent renders without injection
        // spans for now — the next preview tick after the grammar lands picks
        // up the Cached fast path and fills in the children.
        let directory = std::sync::Arc::clone(&self.directory);
        let resolve = move |name: &str| match directory.request_by_name(name) {
            GrammarRequest::Cached(g) => Some(g),
            GrammarRequest::Loading { .. } | GrammarRequest::Unknown => None,
        };
        let mut flat = h.highlight_range_with_injections(bytes, byte_range, resolve);
        drop(cache);
        CommentMarkerPass::new().apply(&mut flat, bytes);
        let theme = self.theme.syntax.clone();
        let ranges: Vec<(std::ops::Range<usize>, EngineStyle)> = flat
            .into_iter()
            .filter_map(|span| {
                theme.style(span.capture()).map(|s| {
                    let fg = s.fg.map(|c| EngineColor(c.r, c.g, c.b));
                    let bg = s.bg.map(|c| EngineColor(c.r, c.g, c.b));
                    let mut attrs = Attrs::empty();
                    if s.modifiers.bold {
                        attrs |= Attrs::BOLD;
                    }
                    if s.modifiers.italic {
                        attrs |= Attrs::ITALIC;
                    }
                    if s.modifiers.underline {
                        attrs |= Attrs::UNDERLINE;
                    }
                    if s.modifiers.reverse {
                        attrs |= Attrs::REVERSE;
                    }
                    if s.modifiers.strikethrough {
                        attrs |= Attrs::STRIKE;
                    }
                    (span.byte_range.clone(), EngineStyle { fg, bg, attrs })
                })
            })
            .collect();
        PreviewSpans::from_byte_ranges(&ranges, bytes)
    }
}

/// Number of off-screen rows above/below the visible window to include in the
/// highlighter's byte range. Gives the injection query a buffer so a fenced
/// code block whose opening backtick is just above the viewport (with content
/// still on screen) still resolves its child grammar.
const VIEWPORT_SLACK_ROWS: usize = 50;

/// Find the byte offset where row `target_row` begins (row 0 = byte 0). For
/// `target_row` past the end, returns `bytes.len()`.
fn byte_offset_of_row(bytes: &[u8], target_row: usize) -> usize {
    if target_row == 0 {
        return 0;
    }
    let mut row = 0usize;
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'\n' {
            row += 1;
            if row == target_row {
                return i + 1;
            }
        }
    }
    bytes.len()
}

/// Bridge: route `hjkl-picker`'s preview-pane highlighter through the
/// editor's bonsai pipeline. Picker stays bonsai-agnostic — the trait
/// impl lives consumer-side.
impl hjkl_picker::PreviewHighlighter for App {
    fn spans_for(&self, path: &Path, bytes: &[u8]) -> PreviewSpans {
        self.preview_spans_for(path, bytes)
    }

    fn spans_for_viewport(
        &self,
        path: &Path,
        bytes: &[u8],
        top_row: usize,
        height: usize,
    ) -> PreviewSpans {
        let start_row = top_row.saturating_sub(VIEWPORT_SLACK_ROWS);
        let end_row = top_row
            .saturating_add(height)
            .saturating_add(VIEWPORT_SLACK_ROWS);
        let start = byte_offset_of_row(bytes, start_row);
        let end = byte_offset_of_row(bytes, end_row);
        self.preview_spans_for_range(path, bytes, start..end)
    }
}

/// Format an [`hjkl_anvil::InstallStatus`] as a human-readable log line.
fn format_anvil_status(status: &hjkl_anvil::InstallStatus) -> String {
    use hjkl_anvil::InstallStatus;
    match status {
        InstallStatus::Queued => "queued".into(),
        InstallStatus::Downloading {
            bytes_downloaded,
            total,
        } => match total {
            Some(t) if *t > 0 => format!(
                "downloading {}% ({bytes_downloaded}/{t} bytes)",
                (bytes_downloaded * 100) / t
            ),
            _ => format!("downloading {bytes_downloaded} bytes"),
        },
        InstallStatus::Verifying => "verifying checksum".into(),
        InstallStatus::Extracting => "extracting archive".into(),
        InstallStatus::Installing => "installing binary".into(),
        InstallStatus::Done { bin_path } => format!("done → {}", bin_path.display()),
        InstallStatus::Failed(reason) => format!("failed: {reason}"),
        InstallStatus::TofuRecorded { triple, sha256 } => {
            format!("tofu recorded for {triple}: {}", &sha256[..8])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Regression catcher for the picker preview injection wiring: a
    /// markdown buffer with a fenced rust code block must produce
    /// multiple styled spans on the rust line. If `preview_spans_for`
    /// regresses to the non-injection `highlight_range` variant, the
    /// rust row collapses to a single uniform span and this fails.
    #[test]
    #[ignore = "network + compiler: fetches markdown + rust grammars"]
    fn preview_spans_for_markdown_includes_rust_injection() {
        let app = App::new(None, false, None, None).unwrap();

        // Force sync builds so the preview's async resolver hits Cached.
        // Both calls block on first run; subsequent runs hit the on-disk
        // cache instantly.
        assert!(
            app.directory.by_name("markdown").is_some(),
            "markdown grammar should resolve"
        );
        assert!(
            app.directory.by_name("rust").is_some(),
            "rust grammar should resolve"
        );

        let source = b"# Title\n\n```rust\nfn main() {}\n```\n";
        let path = PathBuf::from("test.md");
        let spans = app.preview_spans_for(&path, source);

        // Row layout of the test source:
        //   0: # Title
        //   1: (blank)
        //   2: ```rust
        //   3: fn main() {}   ← injection target
        //   4: ```
        const RUST_ROW: usize = 3;
        assert!(
            spans.by_row.len() > RUST_ROW,
            "expected at least {} rows, got {}",
            RUST_ROW + 1,
            spans.by_row.len()
        );
        let rust_row = &spans.by_row[RUST_ROW];

        // Without injection the whole `fn main() {}` slice is one uniform
        // markdown `code_fence_content` capture (≤ 1 styled span). With
        // rust injection we expect distinct keyword + function + punctuation
        // spans (≥ 3 styled regions in any theme that styles those captures).
        assert!(
            rust_row.len() >= 3,
            "expected ≥3 styled spans on the rust row (keyword/function/punct from injection); \
             got {} spans: {:?}",
            rust_row.len(),
            rust_row
        );
    }

    /// Regression catcher for the async-highlight cross-buffer bug
    /// (https://github.com/kryptic-sh/hjkl/issues/...): a parse submitted
    /// against buffer A could complete after the user switched to buffer B,
    /// and `recompute_and_install` would paint A's spans onto B because
    /// the install path ignored `RenderOutput::buffer_id`.
    ///
    /// Manifested as: under PC lag, hjkl shows the previous tab's syntax
    /// colors on the now-active tab until the next keystroke.
    ///
    /// This test calls `install_render_result` with a synthetic output
    /// whose `buffer_id` does not match the active buffer and verifies
    /// the install is dropped and `syntax_stale_drops` increments.
    #[test]
    fn install_render_result_drops_stale_buffer_id() {
        use crate::syntax::{PerfBreakdown, RenderOutput};
        use hjkl_buffer::Sign;
        use ratatui::style::{Color, Style};

        let mut app = App::new(None, false, None, None).unwrap();

        let active_id = app.active().buffer_id;
        let stale_id = active_id.wrapping_add(999);

        // Seed a recognisable diag_sign on the active buffer so we can
        // detect whether a (stale) install overwrote it.
        let sentinel = Sign {
            row: 0,
            ch: '!',
            style: Style::default(),
            priority: 1,
        };
        app.active_mut().diag_signs = vec![sentinel];

        // Stale: install carries a buffer_id the active buffer doesn't match.
        let stale_signs = vec![Sign {
            row: 0,
            ch: 'X',
            style: Style::default().fg(Color::Red),
            priority: 9,
        }];
        let stale = RenderOutput {
            buffer_id: stale_id,
            spans: vec![vec![(0, 1, Style::default().fg(Color::Red))]],
            signs: stale_signs,
            key: (0, 0, 0),
            perf: PerfBreakdown::default(),
        };

        let installed = app.install_render_result(stale);
        assert!(
            !installed,
            "stale RenderOutput (mismatched buffer_id) must not install"
        );
        assert_eq!(
            app.syntax_stale_drops, 1,
            "stale_drops counter should increment when a result is dropped"
        );
        let signs = &app.active().diag_signs;
        assert_eq!(
            signs.len(),
            1,
            "active buffer's diag_signs must not be overwritten by stale install"
        );
        assert_eq!(
            signs[0].ch, '!',
            "sentinel sign survived: active buffer untouched"
        );

        // Matching install: same buffer_id as active — must apply.
        let fresh = RenderOutput {
            buffer_id: active_id,
            spans: vec![vec![]],
            signs: vec![Sign {
                row: 0,
                ch: 'G',
                style: Style::default(),
                priority: 5,
            }],
            key: (0, 0, 0),
            perf: PerfBreakdown::default(),
        };
        let installed = app.install_render_result(fresh);
        assert!(installed, "matching buffer_id must install");
        assert_eq!(
            app.syntax_stale_drops, 1,
            "valid install must not bump stale_drops counter"
        );
        assert_eq!(
            app.active().diag_signs[0].ch,
            'G',
            "fresh signs replaced the sentinel"
        );
    }

    /// T5a: oversize_height must not exceed the buffer's line count.
    /// 5-line buffer, viewport_height=10 → oversize must clamp to 5.
    #[test]
    fn oversize_height_clamped_to_buffer_line_count() {
        // The computation mirrors what recompute_and_install does:
        //   oversize_top    = top.saturating_sub(height)
        //   oversize_height = (height * 3).min(line_count - oversize_top)
        let line_count: usize = 5;
        let top: usize = 0;
        let height: usize = 10;

        let oversize_top = top.saturating_sub(height);
        let oversize_height = height
            .saturating_mul(3)
            .min(line_count.saturating_sub(oversize_top));

        assert_eq!(
            oversize_top, 0,
            "oversize_top must clamp to 0 when top < height"
        );
        assert_eq!(
            oversize_height, 5,
            "oversize_height must not exceed line_count (5)"
        );
    }

    /// T5b: `install_render_result` must route a result to the correct
    /// non-active slot and populate `last_render_output`.
    #[test]
    fn install_render_result_routes_to_correct_slot() {
        use crate::syntax::{PerfBreakdown, RenderOutput};
        use ratatui::style::Style;
        use std::path::PathBuf;

        let mut app = App::new(None, false, None, None).unwrap();

        // Open a second slot — gives us slot 0 (active) and slot 1.
        let tmp = std::env::temp_dir().join("hjkl_test_route_slot.txt");
        std::fs::write(&tmp, "hello\nworld\n").unwrap();
        let slot1_idx = app.open_new_slot(PathBuf::from(&tmp)).unwrap();
        let slot1_buf_id = app.slots()[slot1_idx].buffer_id;

        // Build a synthetic output tagged to slot 1's buffer_id.
        let target_spans = vec![
            vec![(0usize, 5usize, Style::default())],
            vec![(0usize, 5usize, Style::default())],
        ];
        let out = RenderOutput {
            buffer_id: slot1_buf_id,
            spans: target_spans.clone(),
            signs: Vec::new(),
            key: (0, 0, 0),
            perf: PerfBreakdown::default(),
        };

        // Active slot is still slot 0 — install must NOT touch it.
        let active_id = app.active().buffer_id;
        assert_ne!(
            active_id, slot1_buf_id,
            "precondition: slot 0 must be active"
        );

        let installed_on_active = app.install_render_result(out);
        assert!(
            !installed_on_active,
            "result for non-active slot must not return true"
        );

        // The cache on slot 1 must now hold the output.
        let cached = app.slots()[slot1_idx]
            .last_render_output
            .as_ref()
            .expect("last_render_output must be populated on slot 1");
        assert_eq!(
            cached.spans, target_spans,
            "cached spans must match what was installed"
        );

        // Active slot 0 must be untouched.
        assert_eq!(
            app.syntax_stale_drops, 0,
            "routing to non-active slot must not count as stale drop"
        );

        let _ = std::fs::remove_file(&tmp);
    }

    /// T5c: `switch_to` must install cached spans from `last_render_output`
    /// when the dirty_gen matches.
    #[test]
    fn switch_to_installs_cached_spans() {
        use crate::syntax::{PerfBreakdown, RenderOutput};
        use ratatui::style::Style;
        use std::path::PathBuf;

        let mut app = App::new(None, false, None, None).unwrap();

        let tmp = std::env::temp_dir().join("hjkl_test_switch_cached.txt");
        std::fs::write(&tmp, "line1\nline2\n").unwrap();
        let slot1_idx = app.open_new_slot(PathBuf::from(&tmp)).unwrap();
        let slot1_buf_id = app.slots()[slot1_idx].buffer_id;
        let current_dg = app.slots()[slot1_idx].editor.buffer().dirty_gen();

        // Seed a known cache into slot 1.
        let cached_spans = vec![
            vec![(0usize, 5usize, Style::default())],
            vec![(0usize, 5usize, Style::default())],
        ];
        app.slots_mut()[slot1_idx].last_render_output = Some(RenderOutput {
            buffer_id: slot1_buf_id,
            spans: cached_spans.clone(),
            signs: Vec::new(),
            key: (current_dg, 0, 40),
            perf: PerfBreakdown::default(),
        });

        // Switch to slot 1 — cached spans should be installed immediately.
        app.switch_to(slot1_idx);

        // After switch, slot 1 is active. We can't easily assert the
        // internal span table but we can verify no panic and that the
        // cache is still populated (not cleared) on a clean dirty_gen.
        assert!(
            app.slots()[slot1_idx].last_render_output.is_some(),
            "cache must survive a clean switch_to (dirty_gen matched)"
        );

        let _ = std::fs::remove_file(&tmp);
    }

    /// T5d: `switch_to` must drop a stale cache when dirty_gen mismatches
    /// and must NOT install the stale spans.
    #[test]
    fn switch_to_drops_stale_cache_when_dirty_gen_mismatch() {
        use crate::syntax::{PerfBreakdown, RenderOutput};
        use ratatui::style::Style;
        use std::path::PathBuf;

        let mut app = App::new(None, false, None, None).unwrap();

        let tmp = std::env::temp_dir().join("hjkl_test_switch_stale.txt");
        std::fs::write(&tmp, "line1\nline2\n").unwrap();
        let slot1_idx = app.open_new_slot(PathBuf::from(&tmp)).unwrap();
        let slot1_buf_id = app.slots()[slot1_idx].buffer_id;
        let current_dg = app.slots()[slot1_idx].editor.buffer().dirty_gen();

        // Seed a cache with an old dirty_gen (current_dg + 1 simulates
        // the buffer having been modified since the parse).
        let stale_dg = current_dg.wrapping_sub(1);
        app.slots_mut()[slot1_idx].last_render_output = Some(RenderOutput {
            buffer_id: slot1_buf_id,
            spans: vec![vec![(0usize, 5usize, Style::default())]],
            signs: Vec::new(),
            key: (stale_dg, 0, 40),
            perf: PerfBreakdown::default(),
        });

        // Switch to slot 1 — stale cache must be evicted.
        app.switch_to(slot1_idx);

        // The cache for slot 1 must have been cleared.
        // Note: switch_to calls recompute_and_install which may re-populate
        // last_render_output if the worker responds fast enough. We check
        // immediately after switch — if populated, its key must NOT be stale_dg.
        if let Some(ref cached) = app.slots()[slot1_idx].last_render_output {
            assert_ne!(
                cached.key.0, stale_dg,
                "stale cache must have been replaced, not re-installed"
            );
        }
        // (If None, the cache was cleared and nothing re-populated yet — also correct.)

        let _ = std::fs::remove_file(&tmp);
    }
}
