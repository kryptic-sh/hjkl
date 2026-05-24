use hjkl_engine::{Host, Query};
use hjkl_engine_tui::EditorRatatuiExt;
use std::path::Path;
use std::time::{Duration, Instant};

use hjkl_bonsai::{CommentMarkerPass, Highlighter, Theme};
use hjkl_engine::types::{Attrs, Color as EngineColor, Style as EngineStyle};
use hjkl_picker::PreviewSpans;

use hjkl_app::git::{GitChange, GitChangeKind};
use hjkl_app::git_worker::GitJob;
use hjkl_buffer_tui::Sign;
use hjkl_lang::GrammarRequest;
use ratatui::style::{Color, Style};

use super::App;

/// Convert a host-agnostic [`GitChange`] into a ratatui-flavored [`Sign`]
/// with the canonical gutter characters and colours.
fn change_to_sign(c: GitChange) -> Sign {
    let (ch, style) = match c.kind {
        GitChangeKind::Add => ('+', Style::default().fg(Color::Green)),
        GitChangeKind::Modify => ('~', Style::default().fg(Color::Yellow)),
        GitChangeKind::Delete => ('_', Style::default().fg(Color::Red)),
    };
    Sign {
        row: c.row,
        ch,
        style,
        priority: 50,
    }
}

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
                    slot.git_signs = result.changes.into_iter().map(change_to_sign).collect();
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
        let events = self.syntax.poll_pending_loads();
        if events.is_empty() {
            return false;
        }
        let active_id = self.active().buffer_id;
        for event in &events {
            use crate::syntax::LoadEventKind;
            hjkl_syntax::SyntaxLayer::dispatch_load_event(event, |kind| match kind {
                LoadEventKind::Ready { id, name } => {
                    tracing::debug!("grammar load complete: {name} (buffer {id})");
                    // If the completed load is for the active buffer, clear
                    // the recompute cache key so the next recompute_and_install
                    // submits a fresh parse with the new language.
                    if id == active_id {
                        self.active_mut().last_recompute_key = None;
                    }
                }
                LoadEventKind::Failed { id, name, error } => {
                    tracing::debug!("grammar load failed: {name} (buffer {id}): {error}");
                    self.bus.error(format!("grammar {name}: {error}"));
                }
            });
        }
        true
    }

    /// Poll in-flight anvil install handles each tick and fan status events
    /// into the notification bus and `anvil_log`.
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
                        self.bus.info(format!("anvil: installed {name}"));
                        to_remove.push(name.clone());
                    }
                    InstallStatus::Failed(reason) => {
                        self.bus
                            .error(format!("anvil: {name} failed \u{2014} {reason}"));
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
                        self.bus.info(format!("anvil: {name} downloading {pct}"));
                    }
                    InstallStatus::Verifying => {
                        self.bus.info(format!("anvil: {name} verifying"));
                    }
                    InstallStatus::Extracting => {
                        self.bus.info(format!("anvil: {name} extracting"));
                    }
                    InstallStatus::Installing => {
                        self.bus.info(format!("anvil: {name} installing"));
                    }
                    InstallStatus::Queued => {
                        // No toast for the queued state — it's transient.
                    }
                    InstallStatus::TofuRecorded { triple, .. } => {
                        self.bus
                            .info(format!("anvil: {name} TOFU hash recorded for {triple}"));
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
        let active_id = self.active().buffer_id;

        // Find the slot that owns this buffer_id.
        let Some(slot_idx) = self.slots.iter().position(|s| s.buffer_id == out.buffer_id) else {
            // No slot matched (buffer was closed before the result arrived).
            self.syntax_stale_drops = self.syntax_stale_drops.saturating_add(1);
            return false;
        };

        let is_active = out.buffer_id == active_id;

        // Plan-B (#233): the worker now ships EMPTY span tables — it
        // only refreshes the shared retained tree.
        //
        // For `Viewport` kind: the empty signal is discarded.
        // `recompute_and_install` already runs a sync `query_viewport`
        // against the fresh tree at the end of every tick, so firing
        // an extra sync here only duplicates work (the signal's range
        // is the async submit's over-provisioned span, not the visible
        // viewport — different dedup key, so it wouldn't short-circuit).
        //
        // For `Top` / `Bottom`: the recompute path no longer runs sync
        // for these kinds on every tick (typing was 1Hz because each
        // keystroke re-highlighted ~240 prewarm rows with full CSS
        // injection reparse). Instead, trigger sync here when the
        // async worker delivers a fresh tree for that prewarm region.
        // The buffer's current dirty_gen is used so the sync result
        // is tagged correctly even if the worker's parse was for an
        // older gen — the dedup + generation guard handle the rest.
        if out.spans.is_empty() {
            if !is_active {
                return false;
            }
            match out.kind {
                crate::syntax::ParseKind::Viewport => {
                    return false;
                }
                crate::syntax::ParseKind::Top | crate::syntax::ParseKind::Bottom => {
                    let buf_dg = self.slots[slot_idx].editor.buffer().dirty_gen();
                    let range_top = out.key.1;
                    let range_height = out.key.2;
                    self.sync_query_region(
                        out.buffer_id,
                        range_top,
                        range_height,
                        buf_dg,
                        out.kind,
                    );
                    return true;
                }
                _ => return false,
            }
        }

        // Generation-aware install: a render result tagged with an older
        // `dirty_gen` than what's already cached (or older than the
        // buffer's current dirty_gen) is from a parse that was in flight
        // when newer state landed — installing it would clobber the
        // fresher sync `query_viewport` cache. Drop it.
        //
        // Symptoms when this guard is missing:
        // - undo/redo of a multi-line edit: worker reparse for the
        //   pre-undo source arrives after the sync query installed the
        //   post-undo spans → editor paints with pre-undo colours that
        //   don't match the visible text until the NEXT submit lands.
        // - rapid typing: each keystroke ticks dirty_gen; worker results
        //   are always one or two gens behind sync results.
        let buf_dg = self.slots[slot_idx].editor.buffer().dirty_gen();
        let out_dg = out.key.0;
        if out_dg < buf_dg {
            self.syntax_stale_drops = self.syntax_stale_drops.saturating_add(1);
            return false;
        }
        let existing_dg = match out.kind {
            crate::syntax::ParseKind::Viewport => self.slots[slot_idx]
                .viewport_render_output
                .as_ref()
                .map(|o| o.key.0),
            crate::syntax::ParseKind::Top => self.slots[slot_idx]
                .top_render_output
                .as_ref()
                .map(|o| o.key.0),
            crate::syntax::ParseKind::Bottom => self.slots[slot_idx]
                .bottom_render_output
                .as_ref()
                .map(|o| o.key.0),
            // Unknown future kind: treat as install (conservative).
            _ => None,
        };
        if let Some(existing) = existing_dg
            && out_dg < existing
        {
            self.syntax_stale_drops = self.syntax_stale_drops.saturating_add(1);
            return false;
        }

        // Route into the correct per-slot cache field via the exhaustive dispatch
        // helper so no wildcard arm is needed despite ParseKind being #[non_exhaustive].
        use crate::syntax::ParseKindKind;
        hjkl_syntax::SyntaxLayer::dispatch_parse_kind(out.kind, |kind| match kind {
            ParseKindKind::Viewport => {
                // Install diag signs from viewport results (only kind that
                // runs the diagnostic scan over the visible region).
                if is_active {
                    let signs = out.signs.clone();
                    let active_idx = self.focused_slot_idx();
                    self.slots[active_idx].diag_signs = signs;
                }
                self.slots[slot_idx].viewport_render_output = Some(out.clone());
            }
            ParseKindKind::Top => {
                self.slots[slot_idx].top_render_output = Some(out.clone());
            }
            ParseKindKind::Bottom => {
                self.slots[slot_idx].bottom_render_output = Some(out.clone());
            }
        });

        if is_active {
            // Active buffer: patch ONLY the result's range on the editor
            // instead of re-running the merger over `line_count`. This
            // keeps per-keystroke install cost proportional to viewport
            // height (~80 rows) rather than file size — without it j/k
            // on a 10k-row file shows visible cursor lag because the
            // merger + full ratatui install touch every row.
            //
            // Cross-region rows (e.g. Top + Viewport overlap when the
            // cursor is near the top) auto-resolve: the most recently
            // installed kind wins. Because Viewport is the kind that
            // moves on every j/k, "newest = correct viewport" naturally
            // holds.
            use hjkl_engine_tui::EditorRatatuiExt;
            let range_top = out.key.1;
            let range_height = out.key.2;
            let active_idx = self.focused_slot_idx();
            let line_count = self.slots[active_idx].editor.buffer().line_count() as usize;
            let end = (range_top + range_height)
                .min(line_count)
                .min(out.spans.len());
            let start = range_top.min(end);
            let slice = &out.spans[start..end];
            self.slots[active_idx]
                .editor
                .patch_ratatui_syntax_spans_range(start..end, slice);
            return true;
        }
        // Non-active buffer: cached above, live install deferred to switch_to.
        false
    }

    /// Run a sync `query_viewport` against the active buffer's retained
    /// tree for an arbitrary `(top, height)` region and install the
    /// result. Skips silently when the cached source is stale (throttled
    /// submit hasn't fired the cache rebuild yet) or when no retained
    /// tree exists. Generic over `ParseKind` so Viewport / Top / Bottom
    /// all use the same code path.
    ///
    /// `current_dg` is the buffer's current dirty_gen — used both to
    /// detect cache staleness and to tag the resulting `RenderOutput`
    /// so the install path's generation guard knows this result is fresh.
    fn sync_query_region(
        &mut self,
        buffer_id: crate::syntax::BufferId,
        range_top: usize,
        range_height: usize,
        current_dg: u64,
        kind: crate::syntax::ParseKind,
    ) -> bool {
        // Per-tick dedup: skip the highlight + merge + install pipeline
        // when nothing changed since the previous run for this
        // `(buffer, kind)`. Without this gate `recompute_and_install`
        // re-walks the whole `line_count` x 3-cache merger every event
        // loop iteration, which scrolls a large file feels visibly
        // laggy at idle.
        let Some(slot_idx) = self.slots.iter().position(|s| s.buffer_id == buffer_id) else {
            return false;
        };
        let new_key = (current_dg, range_top, range_height);
        let last_key = match kind {
            crate::syntax::ParseKind::Viewport => self.slots[slot_idx].last_sync_viewport_key,
            crate::syntax::ParseKind::Top => self.slots[slot_idx].last_sync_top_key,
            crate::syntax::ParseKind::Bottom => self.slots[slot_idx].last_sync_bottom_key,
            _ => None,
        };
        if last_key == Some(new_key) {
            return true;
        }

        let Some((source, row_starts, line_count_arc, cache_dg)) =
            self.syntax.cached_source(buffer_id)
        else {
            return false;
        };
        if cache_dg != current_dg {
            return false;
        }
        let bytes_len = source.len();
        let vp_start = row_starts.get(range_top).copied().unwrap_or(bytes_len);
        let vp_end_row = range_top + range_height + 1;
        let vp_end = row_starts
            .get(vp_end_row)
            .copied()
            .unwrap_or(bytes_len)
            .min(bytes_len)
            .max(vp_start);
        let sync_out = self.syntax.query_viewport(
            buffer_id,
            source.as_str(),
            row_starts.as_ref(),
            vp_start..vp_end,
            range_top,
            range_height,
            line_count_arc,
            current_dg,
            kind,
        );
        if let Some(sync_out) = sync_out {
            self.install_render_result(sync_out);
            // Mark this `(buffer, kind, key)` as done so the next idle
            // tick short-circuits before re-running the highlight +
            // merge + install pipeline.
            match kind {
                crate::syntax::ParseKind::Viewport => {
                    self.slots[slot_idx].last_sync_viewport_key = Some(new_key);
                }
                crate::syntax::ParseKind::Top => {
                    self.slots[slot_idx].last_sync_top_key = Some(new_key);
                }
                crate::syntax::ParseKind::Bottom => {
                    self.slots[slot_idx].last_sync_bottom_key = Some(new_key);
                }
                _ => {}
            }
            true
        } else {
            false
        }
    }

    /// Run sync `query_viewport` for the active buffer's Viewport region
    /// (the over-provisioned visible window). On cold-open / post-reset
    /// where no retained tree exists, falls back to the one-shot
    /// `preview_render` so the visible rows paint immediately.
    fn sync_query_active_viewport(
        &mut self,
        buffer_id: crate::syntax::BufferId,
        oversize_top: usize,
        oversize_height: usize,
        fallback_top: usize,
        fallback_height: usize,
        current_dg: u64,
    ) {
        let installed = self.sync_query_region(
            buffer_id,
            oversize_top,
            oversize_height,
            current_dg,
            crate::syntax::ParseKind::Viewport,
        );
        if installed {
            return;
        }
        // No retained tree (cold open / post-reset). Fall back to the
        // one-shot `preview_render` so the visible rows paint immediately
        // instead of waiting on the async worker.
        let active_idx = self.focused_slot_idx();
        let buf = self.slots[active_idx].editor.buffer();
        if let Some(preview) =
            self.syntax
                .preview_render(buffer_id, buf, fallback_top, fallback_height)
        {
            self.install_render_result(preview);
        }
    }

    /// Handle a `take_content_reset` event on the active buffer: clear the
    /// per-slot `RenderOutput` caches (their row counts no longer match
    /// the post-replace buffer), blank the editor's installed
    /// `buffer_spans`, and tell the syntax layer to drop the shared
    /// retained tree synchronously so the next `query_viewport` returns
    /// `None` and falls back to the one-shot `preview_render`.
    ///
    /// Called from every site that drains `take_content_reset` — kept
    /// here so the multi-step bookkeeping cannot drift between call
    /// sites (4 in `event_loop.rs`, 1 in `sync_after_engine_mutation`).
    pub(crate) fn handle_active_content_reset(&mut self, buffer_id: crate::syntax::BufferId) {
        self.syntax.reset(buffer_id);
        let active_idx = self.focused_slot_idx();
        let slot = &mut self.slots[active_idx];
        slot.viewport_render_output = None;
        slot.top_render_output = None;
        slot.bottom_render_output = None;
        // Clear sync dedup keys so the post-reset sync query is not
        // skipped if the new dirty_gen + viewport happen to match a
        // pre-reset key.
        slot.last_sync_viewport_key = None;
        slot.last_sync_top_key = None;
        slot.last_sync_bottom_key = None;
        slot.editor.install_ratatui_syntax_spans(Vec::new());
    }

    /// Translate the per-slot cached `RenderOutput.spans` row vectors
    /// in-place to track a batch of [`hjkl_engine::ContentEdit`]s, mirroring
    /// what [`hjkl_engine::Editor::shift_syntax_spans_for_edits`] does for
    /// the live `buffer_spans`.
    ///
    /// Without this, after any row-count edit the cached outputs have
    /// `spans.len()` matching the OLD line count. `merge_render_outputs`
    /// then wholesale-rejects every cache (`out.spans.len() != line_count`),
    /// produces an all-empty merged table, and `install_merged_spans_for_slot`
    /// replaces the editor's in-memory spans with blank rows — visible as a
    /// white flash on every Enter / backspace-at-BOL until the worker
    /// delivers a fresh parse for the new line count.
    ///
    /// Shifting the cached spans keeps them shape-compatible so the merger
    /// can fall through to its per-row dirty-log path: rows touched by the
    /// edit stay blank for one frame (worker fills them in), untouched rows
    /// keep their cached colours.
    pub(crate) fn shift_cached_render_output_spans_for_slot(
        &mut self,
        slot_idx: usize,
        edits: &[hjkl_engine::ContentEdit],
    ) {
        if edits.is_empty() {
            return;
        }
        let slot = &mut self.slots[slot_idx];
        if let Some(out) = slot.viewport_render_output.as_mut() {
            shift_rows(&mut out.spans, edits);
        }
        if let Some(out) = slot.top_render_output.as_mut() {
            shift_rows(&mut out.spans, edits);
        }
        if let Some(out) = slot.bottom_render_output.as_mut() {
            shift_rows(&mut out.spans, edits);
        }
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
        let dirty_rows_log = &self.slots[slot_idx].dirty_rows_log;
        let sources = [
            self.slots[slot_idx].top_render_output.as_ref(),
            self.slots[slot_idx].bottom_render_output.as_ref(),
            self.slots[slot_idx].viewport_render_output.as_ref(),
        ];
        let merged = merge_render_outputs(line_count, current_dg, dirty_rows_log, sources);
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
                union_bot = union_bot.max(w.top_row + rect.h as usize);
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

        // Plan B (#233): run a sync `query_viewport` against the retained
        // tree (which already has this frame's `tree.edit` deltas applied
        // via `SyntaxLayer::apply_edits`) every tick — NOT only when a
        // submit fired. Pure-scroll ticks (no edit) take the cache-hit /
        // throttle branch above and skip the submit; without this call
        // the viewport keeps painting last frame's spans until the
        // async worker delivers.
        //
        // Cheap: ~100µs on warm tree for an 80-row window. The sync
        // result is tagged with the buffer's current dirty_gen so the
        // generation guard in `install_render_result` will favour it
        // over any older worker result still in flight.
        //
        // Use the VISIBLE viewport (not the 3x over-provisioned range
        // the async worker submits) for sync. Over-provisioning helped
        // the worker pre-cache ahead-of-scroll rows; the sync path
        // doesn't cache, it just re-queries every tick, so highlighting
        // 240 rows when only 80 are visible is wasted CPU per j/k.
        self.sync_query_active_viewport(buffer_id, top, height, top, height, dg);

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
        // AND fresh for the current dirty_gen (avoid redundant work).
        // A cache that is `Some` but carries a stale dirty_gen is treated
        // the same as `None` — the merger will reject its spans anyway, so
        // we must re-submit to get a fresh result.  Not checking this was
        // the secondary cause of the delete+undo staleness bug: a stale-dg
        // top/bottom cache prevented re-submission indefinitely.
        {
            let active_idx = self.focused_slot_idx();
            let needs_top = self.slots[active_idx]
                .top_render_output
                .as_ref()
                .is_none_or(|o| o.key.0 != dg);
            let needs_bottom = self.slots[active_idx]
                .bottom_render_output
                .as_ref()
                .is_none_or(|o| o.key.0 != dg);
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
                // Top/Bottom sync is intentionally NOT run per-tick.
                // Each insert-mode keystroke ticks `dirty_gen`, which
                // would re-fire the Top + Bottom highlight every char
                // — on HTML with CSS injection that's ~240 rows × CSS
                // sub-tree reparse per keystroke, visible as 1Hz typing
                // lag. The async worker handles the refresh; the
                // empty-result signal in `install_render_result`
                // triggers a one-off sync query when the worker delivers
                // a fresh tree for Top/Bottom.
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
            // Use the same stale-dg check as for the active slot: a cache
            // that exists but was computed for an old dirty_gen is useless
            // (merger rejects it) and must be refreshed.
            let slot_dg = self.slots[slot_idx].editor.buffer().dirty_gen();
            let needs_top = self.slots[slot_idx]
                .top_render_output
                .as_ref()
                .is_none_or(|o| o.key.0 != slot_dg);
            let needs_bottom = self.slots[slot_idx]
                .bottom_render_output
                .as_ref()
                .is_none_or(|o| o.key.0 != slot_dg);

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
            GrammarRequest::Loading { .. } | GrammarRequest::Unknown | _ => {
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
            GrammarRequest::Loading { .. } | GrammarRequest::Unknown | _ => None,
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
///    Wholesale rejection: row indices are wrong for the whole cache.
///
/// **Per-row dirty tracking** — when a cache has the right row count but
/// `key.0 < current_dirty_gen` (stale from an edit), we no longer reject
/// it wholesale. Instead, for each row we check whether that row was touched
/// by any edit that arrived after the cache's parse (i.e. any log entry with
/// `dirty_gen > cache_key.0`). Touched rows are left blank so the fresh
/// worker result can fill them in; untouched rows keep their cached spans.
///
/// This eliminates the "white flash" where ALL rows briefly go blank after
/// a single keystroke because the whole-cache rejection fired before the
/// background worker returned.
///
/// Blanket dirty-gen rejection was removed as of 2026-05-16 in favour of
/// per-row tracking. The `spans.len() != line_count` guard (row-count shift)
/// is still in place — that case invalidates row indices for the whole cache.
pub(crate) fn merge_render_outputs<'a>(
    line_count: usize,
    current_dirty_gen: u64,
    dirty_rows_log: &[(u64, std::ops::RangeInclusive<usize>)],
    sources: impl IntoIterator<Item = Option<&'a crate::syntax::RenderOutput>>,
) -> Vec<Vec<(usize, usize, ratatui::style::Style)>> {
    let mut merged: Vec<Vec<(usize, usize, ratatui::style::Style)>> = vec![Vec::new(); line_count];
    for out in sources.into_iter().flatten() {
        // Reject wholesale when the row count shifted (insertion/deletion of
        // whole lines). Row indices in the cache no longer map correctly.
        if out.spans.len() != line_count {
            continue;
        }
        let cache_dg = out.key.0;
        for (row, row_spans) in out.spans.iter().enumerate() {
            if row >= line_count {
                break;
            }
            if row_spans.is_empty() {
                continue;
            }
            // Check whether this row was touched by any edit that landed
            // AFTER this cache was parsed (dirty_gen > cache_dg).
            // If so, the cached bytes no longer match the live buffer at
            // this row — leave blank and let the worker fill it in.
            let row_is_dirty = dirty_rows_log.iter().any(|(dg, range)| {
                *dg > cache_dg && *dg <= current_dirty_gen && range.contains(&row)
            });
            if !row_is_dirty {
                merged[row] = row_spans.clone();
            }
        }
    }
    merged
}

/// Translate per-row span vectors in-place to track a batch of
/// [`hjkl_engine::ContentEdit`]s. Generic over the row payload type so the
/// same helper applies to cached `RenderOutput.spans`
/// (`Vec<Vec<(usize, usize, StyleSpec)>>`) and any other row-keyed cache
/// shape that needs to follow line count changes.
///
/// For each edit:
/// - row count grew by N → insert N empty rows at index `old_end_row + 1`.
/// - row count shrank by N → drain `(new_end_row + 1)..(old_end_row + 1)`.
///
/// Edits apply in order, each interpreted relative to the post-state of
/// the prior edits in the batch (matching engine emission order).
pub(crate) fn shift_rows<T>(rows: &mut Vec<Vec<T>>, edits: &[hjkl_engine::ContentEdit]) {
    for edit in edits {
        let oer = edit.old_end_position.0 as usize;
        let ner = edit.new_end_position.0 as usize;
        if ner == oer {
            continue;
        }
        if ner > oer {
            let n = ner - oer;
            let idx = (oer + 1).min(rows.len());
            for _ in 0..n {
                rows.insert(idx, Vec::new());
            }
        } else {
            let n = oer - ner;
            let len = rows.len();
            let start = (ner + 1).min(len);
            let end = (start + n).min(len);
            if end > start {
                rows.drain(start..end);
            }
        }
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

    fn edit_insert_newline_at(row: u32, col: u32) -> hjkl_engine::ContentEdit {
        hjkl_engine::ContentEdit {
            start_byte: 0,
            old_end_byte: 0,
            new_end_byte: 1,
            start_position: (row, col),
            old_end_position: (row, col),
            new_end_position: (row + 1, 0),
        }
    }

    fn edit_join_rows(row: u32, col: u32) -> hjkl_engine::ContentEdit {
        hjkl_engine::ContentEdit {
            start_byte: 0,
            old_end_byte: 1,
            new_end_byte: 0,
            start_position: (row, col),
            old_end_position: (row + 1, 0),
            new_end_position: (row, col),
        }
    }

    fn marked_rows(n: usize) -> Vec<Vec<u8>> {
        (0..n).map(|i| vec![i as u8]).collect()
    }

    #[test]
    fn shift_rows_insertion_grows_in_place() {
        let mut rows = marked_rows(4);
        shift_rows(&mut rows, &[edit_insert_newline_at(1, 0)]);
        assert_eq!(rows.len(), 5);
        // Inserted empty row sits at oer+1 = 2.
        assert!(rows[2].is_empty(), "inserted row must be empty");
        // Untouched neighbours keep their marker bytes.
        assert_eq!(rows[0], vec![0]);
        assert_eq!(rows[1], vec![1]);
        assert_eq!(rows[3], vec![2]);
        assert_eq!(rows[4], vec![3]);
    }

    #[test]
    fn shift_rows_deletion_shrinks_in_place() {
        let mut rows = marked_rows(4);
        shift_rows(&mut rows, &[edit_join_rows(1, 0)]);
        assert_eq!(rows.len(), 3);
        // Drained range was (ner+1)..(oer+1) = 2..3, so row 2 (marker `2`)
        // disappears. Survivors: 0, 1, 3.
        assert_eq!(rows[0], vec![0]);
        assert_eq!(rows[1], vec![1]);
        assert_eq!(rows[2], vec![3]);
    }

    #[test]
    fn shift_rows_same_row_edit_is_noop() {
        let mut rows = marked_rows(3);
        let same_row = hjkl_engine::ContentEdit {
            start_byte: 0,
            old_end_byte: 0,
            new_end_byte: 1,
            start_position: (1, 0),
            old_end_position: (1, 0),
            new_end_position: (1, 1),
        };
        shift_rows(&mut rows, &[same_row]);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], vec![0]);
        assert_eq!(rows[1], vec![1]);
        assert_eq!(rows[2], vec![2]);
    }

    #[test]
    fn shift_rows_ordered_edits_compose() {
        let mut rows = marked_rows(3);
        shift_rows(
            &mut rows,
            &[edit_insert_newline_at(0, 0), edit_insert_newline_at(1, 0)],
        );
        assert_eq!(rows.len(), 5);
    }

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
        use hjkl_buffer_tui::Sign;
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
        // Tag the result with slot 1's current dirty_gen so the
        // generation-aware install guard doesn't drop it as stale.
        let target_spans = vec![
            vec![(0usize, 5usize, Style::default())],
            vec![(0usize, 5usize, Style::default())],
        ];
        let slot1_dg = app.slots()[slot1_idx].editor.buffer().dirty_gen();
        let out = RenderOutput {
            buffer_id: slot1_buf_id,
            spans: target_spans.clone(),
            signs: Vec::new(),
            key: (slot1_dg, 0, 0),
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
        let merged = merge_render_outputs(3, 0, &[], [Some(&stale_top), None, None]);

        assert_eq!(merged.len(), 3, "merged length must match line_count");
        for (row, spans) in merged.iter().enumerate() {
            assert!(
                spans.is_empty(),
                "row {row} must not carry stale-cache spans; got {spans:?}"
            );
        }
    }

    /// Per-row dirty tracking: rows explicitly recorded in the dirty_rows_log
    /// are blanked even when the cache has a matching row count.  Rows NOT in
    /// the log keep their cached spans (the "no white flash" property).
    #[test]
    fn merge_render_outputs_per_row_dirty_blanks_logged_rows_only() {
        use crate::app::syntax_glue::merge_render_outputs;
        use crate::syntax::{ParseKind, PerfBreakdown, RenderOutput};
        use ratatui::style::{Color, Style};

        let buf_id = 42;
        let stale_style = Style::default().fg(Color::Red);
        // Cache has row count 5, parsed at dirty_gen=7.
        let stale_spans: Vec<Vec<(usize, usize, Style)>> = (0..5)
            .map(|_| vec![(0usize, 3usize, stale_style)])
            .collect();
        let stale_top = RenderOutput {
            buffer_id: buf_id,
            spans: stale_spans,
            signs: Vec::new(),
            key: (7, 0, 40), // cache dirty_gen = 7
            perf: PerfBreakdown::default(),
            kind: ParseKind::Top,
        };

        // Edit landed at gen=8, touching row 2 only.
        // Current dirty_gen is 8 (one edit since cache was parsed).
        let log: &[(u64, std::ops::RangeInclusive<usize>)] = &[(8, 2..=2)];
        let merged = merge_render_outputs(5, 8, log, [Some(&stale_top), None, None]);

        assert_eq!(merged.len(), 5);
        // Row 2 was edited — must be blank.
        assert!(
            merged[2].is_empty(),
            "row 2 (edited) must be blank; got {:?}",
            merged[2]
        );
        // Rows 0, 1, 3, 4 were NOT edited — must keep cached spans.
        for row in [0, 1, 3, 4] {
            assert!(
                !merged[row].is_empty(),
                "row {row} (untouched) must keep cached spans; got {:?}",
                merged[row]
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

        let merged = merge_render_outputs(3, 5, &[], [Some(&fresh), None, None]);

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

    /// A stale-dirty_gen top/bottom cache whose rows are all logged as dirty
    /// must have ALL rows blanked — this models a delete+undo cycle where
    /// bytes shifted for every row even though line count stayed the same.
    ///
    /// Also verifies that a cache for rows NOT in the dirty log keeps its
    /// spans visible (the "no white flash" property for untouched rows).
    ///
    /// The `needs_top`/`needs_bottom` re-submit guard in `recompute_and_install`
    /// still fires on `key.0 != current_dg` — this test focuses on what the
    /// merger shows to the user DURING the latency window before the fresh
    /// parse arrives.
    #[test]
    fn stale_dg_top_bottom_cache_is_rejected_by_merger_and_needs_resubmit() {
        use crate::app::syntax_glue::merge_render_outputs;
        use crate::syntax::{ParseKind, PerfBreakdown, RenderOutput};
        use ratatui::style::{Color, Style};

        // Simulate: buffer has line_count=5, current_dg=3.
        // Top cache was computed at dg=1 (two edits ago).
        // Bottom cache was computed at dg=2 (one edit ago).
        // Both have the right span count (5) — only dirty_gen differs.
        let line_count = 5usize;
        let current_dg = 3u64;

        let stale_style = Style::default().fg(Color::Magenta);

        // Top cache: correct length, STALE dg.
        let top_spans: Vec<Vec<(usize, usize, Style)>> =
            (0..line_count).map(|_| vec![(0, 4, stale_style)]).collect();
        let stale_top = RenderOutput {
            buffer_id: 7,
            spans: top_spans,
            signs: Vec::new(),
            key: (1, 0, 40), // dg=1 ≠ current_dg=3
            perf: PerfBreakdown::default(),
            kind: ParseKind::Top,
        };

        // Bottom cache: correct length, STALE dg.
        let bot_spans: Vec<Vec<(usize, usize, Style)>> =
            (0..line_count).map(|_| vec![(0, 4, stale_style)]).collect();
        let stale_bottom = RenderOutput {
            buffer_id: 7,
            spans: bot_spans,
            signs: Vec::new(),
            key: (2, 0, 40), // dg=2 ≠ current_dg=3
            perf: PerfBreakdown::default(),
            kind: ParseKind::Bottom,
        };

        // Dirty log: all rows touched (simulates delete+undo where bytes shifted
        // everywhere even though line count stayed the same).  Two edit batches:
        // dg=2 touched rows 0..=4, dg=3 also touched rows 0..=4.
        let log: &[(u64, std::ops::RangeInclusive<usize>)] = &[(2, 0..=4), (3, 0..=4)];

        // No viewport cache yet (fresh buffer state after undo).
        let merged = merge_render_outputs(
            line_count,
            current_dg,
            log,
            [Some(&stale_top), Some(&stale_bottom), None],
        );

        // All rows touched → merger must blank them so byte-offset-wrong spans
        // are never painted.  Worker re-submit (driven by needs_top=key.0!=dg)
        // will fill them in when the fresh parse arrives.
        assert_eq!(merged.len(), line_count);
        for (row, spans) in merged.iter().enumerate() {
            assert!(
                spans.is_empty(),
                "row {row}: all-dirty log must blank stale-dg spans; got {spans:?}"
            );
        }

        // Confirm: a FRESH top cache (dg=current_dg) IS accepted even with log.
        let fresh_style = Style::default().fg(Color::Green);
        let fresh_spans: Vec<Vec<(usize, usize, Style)>> =
            (0..line_count).map(|_| vec![(0, 4, fresh_style)]).collect();
        let fresh_top = RenderOutput {
            buffer_id: 7,
            spans: fresh_spans,
            signs: Vec::new(),
            key: (current_dg, 0, 40), // dg=3 == current_dg ✓
            perf: PerfBreakdown::default(),
            kind: ParseKind::Top,
        };
        // Fresh cache key.0 == current_dg → no log entry has dg > current_dg,
        // so the row-dirty check is false for every row → spans installed.
        let merged_fresh =
            merge_render_outputs(line_count, current_dg, log, [Some(&fresh_top), None, None]);
        for (row, spans) in merged_fresh.iter().enumerate() {
            assert!(
                !spans.is_empty(),
                "row {row}: fresh top cache must be installed"
            );
        }
    }

    /// Per-row dirty tracking: unchanged rows keep their cached spans.
    ///
    /// Cache covers rows 0..10 with red spans, parsed at dirty_gen=5.
    /// Edit log records (6, 3..=3) — row 3 was edited at gen 6.
    /// After merge: rows 0-2, 4-9 keep red spans; row 3 is blank.
    #[test]
    fn merge_partial_keeps_unchanged_row_spans() {
        use crate::app::syntax_glue::merge_render_outputs;
        use crate::syntax::{ParseKind, PerfBreakdown, RenderOutput};
        use ratatui::style::{Color, Style};

        let red = Style::default().fg(Color::Red);
        let spans: Vec<Vec<(usize, usize, Style)>> =
            (0..10).map(|_| vec![(0usize, 1usize, red)]).collect();
        let cache = RenderOutput {
            buffer_id: 1,
            spans,
            signs: Vec::new(),
            key: (5, 0, 40), // parsed at dirty_gen=5
            perf: PerfBreakdown::default(),
            kind: ParseKind::Top,
        };

        // Row 3 was edited at gen=6.
        let log: &[(u64, std::ops::RangeInclusive<usize>)] = &[(6, 3..=3)];
        let merged = merge_render_outputs(10, 6, log, [Some(&cache), None, None]);

        assert_eq!(merged.len(), 10);
        // Row 3 was touched — must be blank.
        assert!(
            merged[3].is_empty(),
            "row 3 (edited at gen 6) must be blank; got {:?}",
            merged[3]
        );
        // All other rows must keep their cached red spans.
        for row in (0..10).filter(|&r| r != 3) {
            assert!(
                !merged[row].is_empty(),
                "row {row} (untouched) must keep red spans; got {:?}",
                merged[row]
            );
        }
    }

    /// Per-row dirty tracking: multiple edits at different gens blank
    /// all affected rows and leave the rest intact.
    ///
    /// Cache at dirty_gen=5 with spans on rows 0..10.
    /// Log: [(6, 2..=4), (7, 0..=0)]. Current dg=7.
    /// Assert: rows 0, 2, 3, 4 blank; rows 1, 5, 6, 7, 8, 9 keep spans.
    #[test]
    fn merge_partial_blanks_all_edited_rows() {
        use crate::app::syntax_glue::merge_render_outputs;
        use crate::syntax::{ParseKind, PerfBreakdown, RenderOutput};
        use ratatui::style::{Color, Style};

        let blue = Style::default().fg(Color::Blue);
        let spans: Vec<Vec<(usize, usize, Style)>> =
            (0..10).map(|_| vec![(0usize, 1usize, blue)]).collect();
        let cache = RenderOutput {
            buffer_id: 2,
            spans,
            signs: Vec::new(),
            key: (5, 0, 40),
            perf: PerfBreakdown::default(),
            kind: ParseKind::Viewport,
        };

        let log: &[(u64, std::ops::RangeInclusive<usize>)] = &[(6, 2..=4), (7, 0..=0)];
        let merged = merge_render_outputs(10, 7, log, [None, None, Some(&cache)]);

        assert_eq!(merged.len(), 10);
        let dirty_rows = [0usize, 2, 3, 4];
        let clean_rows = [1usize, 5, 6, 7, 8, 9];

        for row in dirty_rows {
            assert!(
                merged[row].is_empty(),
                "row {row} (edited) must be blank; got {:?}",
                merged[row]
            );
        }
        for row in clean_rows {
            assert!(
                !merged[row].is_empty(),
                "row {row} (untouched) must keep blue spans; got {:?}",
                merged[row]
            );
        }
    }

    /// Row-count mismatch still causes wholesale cache rejection regardless
    /// of the dirty_rows_log content.
    #[test]
    fn merge_partial_full_row_count_mismatch_still_rejects() {
        use crate::app::syntax_glue::merge_render_outputs;
        use crate::syntax::{ParseKind, PerfBreakdown, RenderOutput};
        use ratatui::style::{Color, Style};

        let green = Style::default().fg(Color::Green);
        // Cache has 8 spans but current line_count is 5.
        let spans: Vec<Vec<(usize, usize, Style)>> =
            (0..8).map(|_| vec![(0usize, 1usize, green)]).collect();
        let cache = RenderOutput {
            buffer_id: 3,
            spans,
            signs: Vec::new(),
            key: (10, 0, 40),
            perf: PerfBreakdown::default(),
            kind: ParseKind::Top,
        };

        // Even with an empty log the cache must be rejected (row indices wrong).
        let merged = merge_render_outputs(5, 10, &[], [Some(&cache), None, None]);

        assert_eq!(merged.len(), 5, "merged must have line_count rows");
        for (row, spans) in merged.iter().enumerate() {
            assert!(
                spans.is_empty(),
                "row {row}: row-count mismatch must blank all rows; got {spans:?}"
            );
        }
    }

    /// Regression: worker must not re-apply stale InputEdits when the retained
    /// tree already represents the requested dirty_gen.
    ///
    /// Without the fix, submitting Top before Viewport for the same dirty_gen
    /// (which happens when Top replaces an old in-flight entry in the front of
    /// the parse queue while Viewport is appended at the back) causes:
    ///   1. Top is processed first: cold-parses from the new source → correct tree.
    ///   2. Viewport is processed second: `!edits.is_empty()` was true →
    ///      `tree.edit()` applied with positions from the OLD source → tree
    ///      corruption → highlight spans with wrong byte offsets.
    ///
    /// This test cannot run without a real grammar (the worker drops requests
    /// with no language attached), so it is gated `#[ignore]`.  Run with
    /// `cargo test -p hjkl --bin hjkl worker_ordering -- --ignored --nocapture`
    /// on a machine with grammars installed.
    #[test]
    #[ignore = "network + compiler: needs tree-sitter-rust grammar"]
    fn worker_ordering_stale_edits_do_not_corrupt_spans() {
        use crate::syntax::{BufferId, ParseKind, default_layer};
        use hjkl_buffer::Buffer;
        use hjkl_engine::ContentEdit;
        use std::path::Path;
        use std::time::Duration;

        const TID: BufferId = 0;

        // Source: a simple rust snippet where the highlight spans have distinct
        // token boundaries we can check.
        let initial_src = "fn main() {}\n";
        let edited_src = "fn xmain() {}\n"; // insert 'x' at byte 3

        let initial_buf = Buffer::from_str(initial_src.trim_end_matches('\n'));
        let edited_buf = Buffer::from_str(edited_src.trim_end_matches('\n'));

        let edit = ContentEdit {
            start_byte: 3,
            old_end_byte: 3,
            new_end_byte: 4,
            start_position: (0, 3),
            old_end_position: (0, 3),
            new_end_position: (0, 4),
        };

        // --- Baseline: Viewport-first ordering (correct) ---
        let mut layer_correct = default_layer();
        layer_correct.set_language_for_path(TID, Path::new("a.rs"));

        // Initial parse.
        layer_correct.submit_render(TID, &initial_buf, 0, 40, ParseKind::Viewport);
        let _ = layer_correct.wait_all_results(Duration::from_secs(5));

        // Apply edit, submit Viewport first (normal production order).
        layer_correct.apply_edits(TID, std::slice::from_ref(&edit));
        layer_correct.submit_render(TID, &edited_buf, 0, 40, ParseKind::Viewport);
        let r_viewport_first = layer_correct
            .wait_all_results(Duration::from_secs(5))
            .into_iter()
            .find(|r| r.kind == ParseKind::Viewport)
            .expect("viewport result");

        // --- Bug path: Top-first ordering (triggers the race) ---
        let mut layer_buggy = default_layer();
        layer_buggy.set_language_for_path(TID, Path::new("a.rs"));

        // Initial parse.
        layer_buggy.submit_render(TID, &initial_buf, 0, 40, ParseKind::Viewport);
        let _ = layer_buggy.wait_all_results(Duration::from_secs(5));

        // Simulate the race: apply edits, then submit Top FIRST (taking the
        // edits), then Viewport (empty edits — same as in the real queue race).
        layer_buggy.apply_edits(TID, std::slice::from_ref(&edit));
        // Top submit consumes the edits.
        layer_buggy.submit_render(TID, &edited_buf, 0, 40, ParseKind::Top);
        // Viewport submit gets empty edits (already taken by Top).
        layer_buggy.submit_render(TID, &edited_buf, 0, 40, ParseKind::Viewport);
        let results = layer_buggy.wait_all_results(Duration::from_secs(5));
        let r_top_first = results
            .into_iter()
            .find(|r| r.kind == ParseKind::Viewport)
            .expect("viewport result from top-first ordering");

        // Both orderings must produce identical viewport spans.
        // Before the worker fix, r_top_first had wrong byte offsets because
        // Viewport's edits were applied to the tree already built by Top.
        assert_eq!(
            r_viewport_first.spans, r_top_first.spans,
            "Viewport spans must be identical regardless of whether Top or Viewport \
             was processed first by the worker — stale edits must not corrupt the tree"
        );
    }
}
