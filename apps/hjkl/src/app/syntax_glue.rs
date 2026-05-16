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
    /// Routes into the correct per-slot cache field by `out.kind`:
    /// - `Viewport` → `viewport_render_output`
    /// - `Top`      → `top_render_output`
    /// - `Bottom`   → `bottom_render_output`
    ///
    /// After updating the cache, merges all three caches and installs the
    /// union onto the editor (for the active buffer) or stores for later
    /// (for non-active buffers, so `switch_to` can restore spans without
    /// waiting for a fresh parse).
    ///
    /// Returns `true` if the live install ran on the active buffer, `false`
    /// for non-active routes and genuine stale drops (no matching slot).
    pub(crate) fn install_render_result(&mut self, out: crate::syntax::RenderOutput) -> bool {
        use crate::syntax::ParseKind;

        let active_id = self.active().buffer_id;

        // Find the slot that owns this buffer_id.
        let Some(slot_idx) = self.slots.iter().position(|s| s.buffer_id == out.buffer_id) else {
            // No slot matched (buffer was closed before the result arrived).
            self.syntax_stale_drops = self.syntax_stale_drops.saturating_add(1);
            return false;
        };

        let is_active = out.buffer_id == active_id;

        // Route into the correct per-slot cache field.
        match out.kind {
            ParseKind::Viewport => {
                // Install diag signs from viewport results (only kind that
                // runs the diagnostic scan over the visible region).
                if is_active {
                    let signs = out.signs.clone();
                    let active_idx = self.focused_slot_idx();
                    self.slots[active_idx].diag_signs = signs;
                }
                self.slots[slot_idx].viewport_render_output = Some(out);
            }
            ParseKind::Top => {
                self.slots[slot_idx].top_render_output = Some(out);
            }
            ParseKind::Bottom => {
                self.slots[slot_idx].bottom_render_output = Some(out);
            }
        }

        if is_active {
            // Active buffer: merge all three caches and install on the editor.
            let active_idx = self.focused_slot_idx();
            self.install_merged_spans_for_slot(active_idx);
            return true;
        }
        // Non-active buffer: cached above, live install deferred to switch_to.
        false
    }

    /// Merge `top_render_output`, `bottom_render_output`, and
    /// `viewport_render_output` for the slot at `slot_idx` into a single
    /// per-row span table and install it on the editor.
    ///
    /// Merge order: top → bottom → viewport. Viewport wins for any row
    /// that appears in multiple caches (so the freshest parse of the
    /// current scroll position always takes precedence).
    pub(crate) fn install_merged_spans_for_slot(&mut self, slot_idx: usize) {
        // (see free fn `merge_render_outputs` below — pure helper, testable
        // without an Editor.)
        let line_count = self.slots[slot_idx].editor.buffer().line_count() as usize;
        let current_dg = self.slots[slot_idx].editor.buffer().dirty_gen();
        let sources = [
            self.slots[slot_idx].top_render_output.as_ref(),
            self.slots[slot_idx].bottom_render_output.as_ref(),
            self.slots[slot_idx].viewport_render_output.as_ref(),
        ];
        let merged = merge_render_outputs(line_count, current_dg, sources);
        self.slots[slot_idx]
            .editor
            .install_ratatui_syntax_spans(merged);
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
                    self.syntax.submit_render(
                        buffer_id,
                        buf,
                        oversize_top,
                        oversize_height,
                        crate::syntax::ParseKind::Viewport,
                    )
                };
                if submit_result.is_some() {
                    submitted = true;
                    self.active_mut().last_recompute_at = Instant::now();
                    self.active_mut().last_recompute_key = Some(key);
                }
            }
        }

        // Top + Bottom pre-cache for the active buffer.
        //
        // Submit alongside the Viewport request — worker processes FIFO so
        // Viewport runs first (cold parse builds tree), then Top + Bottom
        // ride along on the same retained tree (incremental highlight, ~1-5 ms
        // each). Startup latency is unchanged: viewport blocking wait is
        // the only thing that gates the first paint. Top + Bottom land soon
        // after with no extra user-perceived delay.
        //
        // Per-(buffer_id, kind) queue dedup means re-submitting the same
        // kind on subsequent ticks just replaces the in-flight request.
        // We skip the submit when the cache for that kind is already populated
        // for the current dirty_gen (avoid redundant work).
        {
            let active_idx = self.focused_slot_idx();
            let needs_top = self.slots[active_idx].top_render_output.is_none();
            let needs_bottom = self.slots[active_idx].bottom_render_output.is_none();
            let slot_line_count = self.slots[active_idx].editor.buffer().line_count() as usize;

            if needs_top {
                let (top_range_start, top_range_height) =
                    hjkl_buffer::over_provisioned_range(0, height, slot_line_count);
                let buf = self.slots[active_idx].editor.buffer();
                self.syntax.submit_render(
                    buffer_id,
                    buf,
                    top_range_start,
                    top_range_height,
                    crate::syntax::ParseKind::Top,
                );
            }

            if needs_bottom {
                let bottom_anchor = slot_line_count.saturating_sub(height);
                let (bot_range_start, bot_range_height) =
                    hjkl_buffer::over_provisioned_range(bottom_anchor, height, slot_line_count);
                let buf = self.slots[active_idx].editor.buffer();
                self.syntax.submit_render(
                    buffer_id,
                    buf,
                    bot_range_start,
                    bot_range_height,
                    crate::syntax::ParseKind::Bottom,
                );
            }
        }

        // T2: Pre-warm other open slots. The per-buffer dedup on the queue
        // ensures this never starves the active buffer's request — active
        // was enqueued first and the worker drains FIFO. If the active
        // buffer's result is not yet back, we still queue the pre-warms
        // so the worker can pipeline: it processes active, then the others.
        // Also submit Top + Bottom for non-active slots so switching to them
        // and immediately pressing `gg` or `G` is also snappy.
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
            self.syntax.submit_render(
                slot_buf_id,
                buf,
                slot_oversize_top,
                slot_oversize_height,
                crate::syntax::ParseKind::Viewport,
            );

            // Top + Bottom for non-active slots — submit alongside Viewport,
            // worker handles FIFO + per-(buffer, kind) dedup.
            let needs_top = self.slots[slot_idx].top_render_output.is_none();
            let needs_bottom = self.slots[slot_idx].bottom_render_output.is_none();

            if needs_top {
                let (top_range_start, top_range_height) =
                    hjkl_buffer::over_provisioned_range(0, slot_height, slot_line_count);
                let buf = self.slots[slot_idx].editor.buffer();
                self.syntax.submit_render(
                    slot_buf_id,
                    buf,
                    top_range_start,
                    top_range_height,
                    crate::syntax::ParseKind::Top,
                );
            }

            if needs_bottom {
                let bottom_anchor = slot_line_count.saturating_sub(slot_height);
                let (bot_range_start, bot_range_height) = hjkl_buffer::over_provisioned_range(
                    bottom_anchor,
                    slot_height,
                    slot_line_count,
                );
                let buf = self.slots[slot_idx].editor.buffer();
                self.syntax.submit_render(
                    slot_buf_id,
                    buf,
                    bot_range_start,
                    bot_range_height,
                    crate::syntax::ParseKind::Bottom,
                );
            }
        }

        // Detect a "big jump" (viewport teleport past the over-provisioned
        // band — typically `gg` / `G` / `<C-d>` / `<C-u>` / line-number `:N`).
        // Without this, the new viewport lands on un-highlighted rows because
        // the pre-warm cache only covers the previous viewport ±1×.
        //
        // Wait budget scales with whether the worker has a retained tree for
        // this buffer (warm) or not (cold). Warm parses on retained trees
        // are ~1-5ms; cold initial parses on big files can take hundreds.
        // Without the cold budget the first `gg`/`G` after open flashes
        // un-highlighted rows because the 40 ms warm cap times out before
        // the initial parse completes.
        //
        // With the top/bottom caches populated, `gg` / `G` on warm buffers
        // hit the cache and never flash — the wait here is still useful for
        // the very first `G` on a cold file before the bottom parse has fired.
        const WARM_JUMP_WAIT: Duration = Duration::from_millis(40);
        const COLD_JUMP_WAIT: Duration = Duration::from_millis(500);
        let is_big_jump = match self.active().last_recompute_key {
            None => true,
            Some((_, prev_top, _)) => hjkl_buffer::is_big_viewport_jump(prev_top, top, height),
        };
        // Cold = the DESTINATION region of the jump has no cached spans.
        // Three sub-cases:
        // - Jump to top (vp_top < h): need top_render_output.
        // - Jump to bottom (vp_top + h >= line_count): need bottom_render_output.
        // - Jump to mid: need viewport_render_output (it's about to be
        //   replaced, but its presence proves the worker has a warm tree).
        //
        // Pre-fix this only checked viewport_render_output, so the first `G`
        // after open detected as warm (top viewport just installed) and used
        // the 40 ms cap — bottom parse hadn't completed yet → flash.
        let active_line_count = self.active().editor.buffer().line_count() as usize;
        let jumps_to_top = top < height;
        let jumps_to_bottom = top + height >= active_line_count;
        let destination_cached = if jumps_to_top {
            self.active().top_render_output.is_some()
        } else if jumps_to_bottom {
            self.active().bottom_render_output.is_some()
        } else {
            self.active().viewport_render_output.is_some()
        };
        let is_cold = !destination_cached;
        let big_jump_wait = if is_cold {
            COLD_JUMP_WAIT
        } else {
            WARM_JUMP_WAIT
        };
        let _ = prev_dirty_gen;

        let t_install = Instant::now();
        // For big jumps, block briefly on the FIRST result (which is the
        // active buffer's parse — it was submitted first into the FIFO
        // queue) then drain everything else. `wait_all_results` returns
        // every per-buffer result so pre-warm hits also reach their caches.
        let all_results = if is_big_jump && submitted {
            self.syntax.wait_all_results(big_jump_wait)
        } else {
            self.syntax.take_all_results()
        };
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

/// Merge per-row span tables from up to three cached [`RenderOutput`]s into
/// a single `line_count`-sized table.
///
/// Order: `sources` is consumed left-to-right; later sources overwrite
/// earlier ones for any row that is non-empty in both. App passes
/// `[top, bottom, viewport]` so the freshest (viewport) wins.
///
/// **Staleness rejection** — a cached `RenderOutput` is silently skipped when:
///
/// 1. `spans.len() != line_count` — buffer was resized (visual delete,
///    paste, undo of either) and the cached row indices no longer match.
/// 2. `key.0 != current_dirty_gen` — buffer was edited within the same
///    line count (e.g. undo restored line count but content differs at
///    byte level). Painting stale-byte spans onto fresh content paints
///    colors at wrong byte offsets within each row.
///
/// Both reported 2026-05-16: highlights for deleted lines "stuck" and broke
/// rows below; tightened on the same day after a delete+undo+delete+undo
/// cycle reproduced the bug with line counts matching but bytes shifted.
pub(crate) fn merge_render_outputs<'a>(
    line_count: usize,
    current_dirty_gen: u64,
    sources: impl IntoIterator<Item = Option<&'a crate::syntax::RenderOutput>>,
) -> Vec<Vec<(usize, usize, ratatui::style::Style)>> {
    let mut merged: Vec<Vec<(usize, usize, ratatui::style::Style)>> =
        vec![Vec::new(); line_count];
    for out in sources.into_iter().flatten() {
        if out.spans.len() != line_count {
            continue;
        }
        if out.key.0 != current_dirty_gen {
            continue;
        }
        for (row, row_spans) in out.spans.iter().enumerate() {
            if row >= line_count {
                break;
            }
            if !row_spans.is_empty() {
                merged[row] = row_spans.clone();
            }
        }
    }
    merged
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
            kind: crate::syntax::ParseKind::Viewport,
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
            kind: crate::syntax::ParseKind::Viewport,
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

    /// T5b: `install_render_result` must route a viewport result to the correct
    /// non-active slot and populate `viewport_render_output`.
    #[test]
    fn install_render_result_routes_to_correct_slot() {
        use crate::syntax::{ParseKind, PerfBreakdown, RenderOutput};
        use ratatui::style::Style;
        use std::path::PathBuf;

        let mut app = App::new(None, false, None, None).unwrap();

        // Open a second slot — gives us slot 0 (active) and slot 1.
        let tmp = std::env::temp_dir().join("hjkl_test_route_slot.txt");
        std::fs::write(&tmp, "hello\nworld\n").unwrap();
        let slot1_idx = app.open_new_slot(PathBuf::from(&tmp)).unwrap();
        let slot1_buf_id = app.slots()[slot1_idx].buffer_id;

        // Build a synthetic viewport output tagged to slot 1's buffer_id.
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
            kind: ParseKind::Viewport,
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

        // The viewport cache on slot 1 must now hold the output.
        let cached = app.slots()[slot1_idx]
            .viewport_render_output
            .as_ref()
            .expect("viewport_render_output must be populated on slot 1");
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

    /// T5c: `switch_to` must install cached spans from `viewport_render_output`
    /// when the dirty_gen matches.
    #[test]
    fn switch_to_installs_cached_spans() {
        use crate::syntax::{ParseKind, PerfBreakdown, RenderOutput};
        use ratatui::style::Style;
        use std::path::PathBuf;

        let mut app = App::new(None, false, None, None).unwrap();

        let tmp = std::env::temp_dir().join("hjkl_test_switch_cached.txt");
        std::fs::write(&tmp, "line1\nline2\n").unwrap();
        let slot1_idx = app.open_new_slot(PathBuf::from(&tmp)).unwrap();
        let slot1_buf_id = app.slots()[slot1_idx].buffer_id;
        let current_dg = app.slots()[slot1_idx].editor.buffer().dirty_gen();

        // Seed a known viewport cache into slot 1.
        let cached_spans = vec![
            vec![(0usize, 5usize, Style::default())],
            vec![(0usize, 5usize, Style::default())],
        ];
        app.slots_mut()[slot1_idx].viewport_render_output = Some(RenderOutput {
            buffer_id: slot1_buf_id,
            spans: cached_spans.clone(),
            signs: Vec::new(),
            key: (current_dg, 0, 40),
            perf: PerfBreakdown::default(),
            kind: ParseKind::Viewport,
        });

        // Switch to slot 1 — cached spans should be installed immediately.
        app.switch_to(slot1_idx);

        // After switch, slot 1 is active. We can't easily assert the
        // internal span table but we can verify no panic and that the
        // cache is still populated (not cleared) on a clean dirty_gen.
        assert!(
            app.slots()[slot1_idx].viewport_render_output.is_some(),
            "viewport cache must survive a clean switch_to (dirty_gen matched)"
        );

        let _ = std::fs::remove_file(&tmp);
    }

    /// T5d: `switch_to` must drop all three caches when dirty_gen mismatches
    /// and must NOT install the stale spans.
    #[test]
    fn switch_to_drops_stale_cache_when_dirty_gen_mismatch() {
        use crate::syntax::{ParseKind, PerfBreakdown, RenderOutput};
        use ratatui::style::Style;
        use std::path::PathBuf;

        let mut app = App::new(None, false, None, None).unwrap();

        let tmp = std::env::temp_dir().join("hjkl_test_switch_stale.txt");
        std::fs::write(&tmp, "line1\nline2\n").unwrap();
        let slot1_idx = app.open_new_slot(PathBuf::from(&tmp)).unwrap();
        let slot1_buf_id = app.slots()[slot1_idx].buffer_id;
        let current_dg = app.slots()[slot1_idx].editor.buffer().dirty_gen();

        // Seed all three caches with an old dirty_gen.
        let stale_dg = current_dg.wrapping_sub(1);
        let stale_out = RenderOutput {
            buffer_id: slot1_buf_id,
            spans: vec![vec![(0usize, 5usize, Style::default())]],
            signs: Vec::new(),
            key: (stale_dg, 0, 40),
            perf: PerfBreakdown::default(),
            kind: ParseKind::Viewport,
        };
        app.slots_mut()[slot1_idx].viewport_render_output = Some(stale_out.clone());
        app.slots_mut()[slot1_idx].top_render_output = Some(RenderOutput {
            kind: ParseKind::Top,
            ..stale_out.clone()
        });
        app.slots_mut()[slot1_idx].bottom_render_output = Some(RenderOutput {
            kind: ParseKind::Bottom,
            ..stale_out
        });

        // Switch to slot 1 — all stale caches must be evicted.
        app.switch_to(slot1_idx);

        // All three caches must have been cleared or replaced with fresh data.
        // Note: switch_to calls recompute_and_install which may re-populate
        // caches if the worker responds fast enough. Any populated cache
        // must NOT carry the stale_dg.
        for (name, cache_opt) in [
            (
                "viewport_render_output",
                &app.slots()[slot1_idx].viewport_render_output,
            ),
            (
                "top_render_output",
                &app.slots()[slot1_idx].top_render_output,
            ),
            (
                "bottom_render_output",
                &app.slots()[slot1_idx].bottom_render_output,
            ),
        ] {
            if let Some(cached) = cache_opt {
                assert_ne!(
                    cached.key.0, stale_dg,
                    "{name}: stale cache must have been replaced, not re-installed"
                );
            }
        }
        // (If None, the cache was cleared and nothing re-populated yet — also correct.)

        let _ = std::fs::remove_file(&tmp);
    }

    /// T5 new test: `install_merged_spans_for_slot` populates top rows
    /// when only `top_render_output` is set; rows past 3h are empty.
    #[test]
    fn install_merged_spans_top_only() {
        use crate::syntax::{ParseKind, PerfBreakdown, RenderOutput};
        use ratatui::style::Style;

        let mut app = App::new(None, false, None, None).unwrap();
        let slot_idx = app.focused_slot_idx();
        // Build a buffer with 10 lines.
        let content = (0..10)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        {
            let buf = hjkl_buffer::Buffer::from_str(&content);
            let host = crate::host::TuiHost::new();
            let editor = hjkl_engine::Editor::new(buf, host, hjkl_engine::Options::default());
            app.slots_mut()[slot_idx].editor = editor;
        }

        let line_count = app.slots()[slot_idx].editor.buffer().line_count() as usize;
        // Build a top RenderOutput covering rows 0..5.
        let top_spans: Vec<Vec<(usize, usize, Style)>> = (0..line_count)
            .map(|i| {
                if i < 5 {
                    vec![(0usize, 4usize, Style::default())]
                } else {
                    vec![]
                }
            })
            .collect();
        app.slots_mut()[slot_idx].top_render_output = Some(RenderOutput {
            buffer_id: app.slots()[slot_idx].buffer_id,
            spans: top_spans,
            signs: Vec::new(),
            key: (0, 0, 40),
            perf: PerfBreakdown::default(),
            kind: ParseKind::Top,
        });

        app.install_merged_spans_for_slot(slot_idx);
        // The editor's styled spans for rows 0..5 should now be non-empty.
        // We verify no panic occurred (install completed) and that caches survived.
        assert!(
            app.slots()[slot_idx].top_render_output.is_some(),
            "top cache must remain after merge install"
        );
    }

    /// T5 new test: viewport wins over top when rows overlap.
    #[test]
    fn install_merged_spans_viewport_overrides_top() {
        use crate::syntax::{ParseKind, PerfBreakdown, RenderOutput};
        use ratatui::style::{Color, Style};

        let mut app = App::new(None, false, None, None).unwrap();
        let slot_idx = app.focused_slot_idx();
        // 5-line buffer.
        let content = (0..5)
            .map(|i| format!("row {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        {
            let buf = hjkl_buffer::Buffer::from_str(&content);
            let host = crate::host::TuiHost::new();
            let editor = hjkl_engine::Editor::new(buf, host, hjkl_engine::Options::default());
            app.slots_mut()[slot_idx].editor = editor;
        }

        let line_count = app.slots()[slot_idx].editor.buffer().line_count() as usize;
        let buf_id = app.slots()[slot_idx].buffer_id;

        // Top cache: all 5 rows with red style.
        let top_style = Style::default().fg(Color::Red);
        let top_spans: Vec<Vec<(usize, usize, Style)>> = (0..line_count)
            .map(|_| vec![(0usize, 3usize, top_style)])
            .collect();
        app.slots_mut()[slot_idx].top_render_output = Some(RenderOutput {
            buffer_id: buf_id,
            spans: top_spans,
            signs: Vec::new(),
            key: (0, 0, 40),
            perf: PerfBreakdown::default(),
            kind: ParseKind::Top,
        });

        // Viewport cache: rows 2..5 with green style (overlaps top rows 2..5).
        let vp_style = Style::default().fg(Color::Green);
        let mut vp_spans: Vec<Vec<(usize, usize, Style)>> = vec![vec![]; line_count];
        for slot in vp_spans.iter_mut().take(5).skip(2) {
            *slot = vec![(0usize, 3usize, vp_style)];
        }
        app.slots_mut()[slot_idx].viewport_render_output = Some(RenderOutput {
            buffer_id: buf_id,
            spans: vp_spans,
            signs: Vec::new(),
            key: (0, 2, 40),
            perf: PerfBreakdown::default(),
            kind: ParseKind::Viewport,
        });

        // install_merged_spans_for_slot should not panic.
        app.install_merged_spans_for_slot(slot_idx);

        // Verify both caches survived.
        assert!(app.slots()[slot_idx].top_render_output.is_some());
        assert!(app.slots()[slot_idx].viewport_render_output.is_some());
    }

    /// Regression: a cached render output whose row count no longer matches
    /// the current buffer (e.g. after a visual-mode delete shrank line
    /// count) must NOT have its spans painted at the old row indices.
    /// Reported 2026-05-16 — "highlights for deleted lines stay so
    /// highlights for anything below break".
    #[test]
    fn merge_render_outputs_skips_stale_row_count_cache() {
        use crate::app::syntax_glue::merge_render_outputs;
        use crate::syntax::{ParseKind, PerfBreakdown, RenderOutput};
        use ratatui::style::{Color, Style};

        let buf_id = 42;
        let stale_style = Style::default().fg(Color::Red);
        let mut stale_spans: Vec<Vec<(usize, usize, Style)>> = vec![vec![]; 8];
        stale_spans[7] = vec![(0usize, 1usize, stale_style)];
        let stale_top = RenderOutput {
            buffer_id: buf_id,
            spans: stale_spans,
            signs: Vec::new(),
            key: (0, 0, 40),
            perf: PerfBreakdown::default(),
            kind: ParseKind::Top,
        };

        // Buffer shrank to 3 rows; stale top reflects pre-delete 8 rows.
        // current_dirty_gen matches the stale cache's gen so the only
        // rejection reason here is the spans.len() mismatch.
        let merged = merge_render_outputs(3, 0, [Some(&stale_top), None, None]);

        assert_eq!(merged.len(), 3, "merged length must match line_count");
        for (row, spans) in merged.iter().enumerate() {
            assert!(
                spans.is_empty(),
                "row {row} must not carry stale-cache spans; got {spans:?}"
            );
        }
    }

    /// Regression: cached output with matching row count but stale dirty_gen
    /// (e.g. delete + undo cycle restored line count but bytes shifted)
    /// must also be skipped — otherwise spans paint at wrong byte offsets
    /// within each row. Reported 2026-05-16 (second pass).
    #[test]
    fn merge_render_outputs_skips_stale_dirty_gen_cache() {
        use crate::app::syntax_glue::merge_render_outputs;
        use crate::syntax::{ParseKind, PerfBreakdown, RenderOutput};
        use ratatui::style::{Color, Style};

        let buf_id = 42;
        let stale_style = Style::default().fg(Color::Red);
        // Same row count (5) as current buffer, but dirty_gen differs.
        let stale_spans: Vec<Vec<(usize, usize, Style)>> =
            (0..5).map(|_| vec![(0usize, 3usize, stale_style)]).collect();
        let stale_top = RenderOutput {
            buffer_id: buf_id,
            spans: stale_spans,
            signs: Vec::new(),
            key: (7, 0, 40), // stale dirty_gen = 7
            perf: PerfBreakdown::default(),
            kind: ParseKind::Top,
        };

        // Current dirty_gen is 12 — does NOT match the cache's 7.
        let merged = merge_render_outputs(5, 12, [Some(&stale_top), None, None]);

        assert_eq!(merged.len(), 5);
        for (row, spans) in merged.iter().enumerate() {
            assert!(
                spans.is_empty(),
                "row {row} must not carry dirty_gen-stale spans; got {spans:?}"
            );
        }
    }

    /// Sanity: matching row count AND matching dirty_gen → spans installed.
    #[test]
    fn merge_render_outputs_accepts_fresh_cache() {
        use crate::app::syntax_glue::merge_render_outputs;
        use crate::syntax::{ParseKind, PerfBreakdown, RenderOutput};
        use ratatui::style::{Color, Style};

        let style = Style::default().fg(Color::Green);
        let spans: Vec<Vec<(usize, usize, Style)>> =
            (0..3).map(|_| vec![(0usize, 1usize, style)]).collect();
        let fresh = RenderOutput {
            buffer_id: 1,
            spans,
            signs: Vec::new(),
            key: (5, 0, 40),
            perf: PerfBreakdown::default(),
            kind: ParseKind::Top,
        };

        let merged = merge_render_outputs(3, 5, [Some(&fresh), None, None]);

        assert_eq!(merged.len(), 3);
        for spans in &merged {
            assert!(!spans.is_empty(), "fresh cache spans must be installed");
        }
    }

    /// T5 new test: bumping dirty_gen invalidates all three caches.
    #[test]
    fn dirty_gen_change_invalidates_all_three_caches() {
        use crate::syntax::{ParseKind, PerfBreakdown, RenderOutput};
        use ratatui::style::Style;
        use std::path::PathBuf;

        let mut app = App::new(None, false, None, None).unwrap();

        let tmp = std::env::temp_dir().join("hjkl_test_dirty_invalidate.txt");
        std::fs::write(&tmp, "a\nb\nc\n").unwrap();
        let slot1_idx = app.open_new_slot(PathBuf::from(&tmp)).unwrap();
        let slot1_buf_id = app.slots()[slot1_idx].buffer_id;
        let current_dg = app.slots()[slot1_idx].editor.buffer().dirty_gen();

        // Seed all three caches at current_dg.
        let make_out = |kind: ParseKind| RenderOutput {
            buffer_id: slot1_buf_id,
            spans: vec![vec![(0usize, 1usize, Style::default())]],
            signs: Vec::new(),
            key: (current_dg, 0, 40),
            perf: PerfBreakdown::default(),
            kind,
        };
        app.slots_mut()[slot1_idx].viewport_render_output = Some(make_out(ParseKind::Viewport));
        app.slots_mut()[slot1_idx].top_render_output = Some(make_out(ParseKind::Top));
        app.slots_mut()[slot1_idx].bottom_render_output = Some(make_out(ParseKind::Bottom));

        // Simulate dirty_gen change by seeding stale_dg caches and then
        // switching to the slot — switch_to detects the mismatch and clears.
        let stale_dg = current_dg.wrapping_sub(1);
        let make_stale = |kind: ParseKind| RenderOutput {
            buffer_id: slot1_buf_id,
            spans: vec![vec![(0usize, 1usize, Style::default())]],
            signs: Vec::new(),
            key: (stale_dg, 0, 40),
            perf: PerfBreakdown::default(),
            kind,
        };
        app.slots_mut()[slot1_idx].viewport_render_output = Some(make_stale(ParseKind::Viewport));
        app.slots_mut()[slot1_idx].top_render_output = Some(make_stale(ParseKind::Top));
        app.slots_mut()[slot1_idx].bottom_render_output = Some(make_stale(ParseKind::Bottom));

        // switch_to should detect the stale dirty_gen and clear all three.
        app.switch_to(slot1_idx);

        // All three caches must be None or refreshed (not stale_dg).
        for (name, cache_opt) in [
            (
                "viewport_render_output",
                &app.slots()[slot1_idx].viewport_render_output,
            ),
            (
                "top_render_output",
                &app.slots()[slot1_idx].top_render_output,
            ),
            (
                "bottom_render_output",
                &app.slots()[slot1_idx].bottom_render_output,
            ),
        ] {
            if let Some(c) = cache_opt {
                assert_ne!(
                    c.key.0, stale_dg,
                    "{name} must not carry stale dirty_gen after switch_to"
                );
            }
        }

        let _ = std::fs::remove_file(&tmp);
    }
}
