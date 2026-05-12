use hjkl_engine::{Host, Query};
use std::path::Path;
use std::time::{Duration, Instant};

use hjkl_bonsai::{CommentMarkerPass, Highlighter, Theme};
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
                }
            }
        }

        for name in to_remove {
            self.anvil_handles.remove(&name);
        }

        redraw
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
                    self.syntax.submit_render(buffer_id, buf, top, height)
                };
                if submit_result.is_some() {
                    submitted = true;
                    self.active_mut().last_recompute_at = Instant::now();
                    self.active_mut().last_recompute_key = Some(key);
                }
            }
        }

        // Non-blocking drain. Previously a viewport-only resubmit waited
        // up to 5ms on the worker; with the bonsai 0.6.2 child-highlighter
        // cache + preview-highlighter cache on the layer, the initial paint
        // is cheap enough that letting the worker spans arrive on a
        // subsequent tick avoids the per-switch hitch.
        let t_install = Instant::now();
        let drained = self.syntax.take_result();
        let _ = prev_dirty_gen;
        if let Some(out) = drained {
            self.active_mut()
                .editor
                .install_ratatui_syntax_spans(out.spans);
            self.active_mut().diag_signs = out.signs;
            self.last_install_us = t_install.elapsed().as_micros();
        } else {
            self.last_install_us = 0;
        }
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
        let ranges: Vec<(std::ops::Range<usize>, ratatui::style::Style)> = flat
            .into_iter()
            .filter_map(|span| {
                theme
                    .style(span.capture())
                    .map(|s| (span.byte_range.clone(), s.to_ratatui()))
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
}
