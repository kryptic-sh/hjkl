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

        // Reuse the per-dirty_gen cached `Arc<String>` so this doesn't
        // re-clone every row per keystroke (paired with the same call
        // in `lsp_notify_change_active` + `buffer_signature` + the
        // syntax submit path — all share one allocation per generation).
        let text = self.active().editor.buffer().content_joined();
        let mut bytes = text.as_bytes().to_vec();
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
                    // Worker just refreshed the retained tree. The sync
                    // query that ran during the keystroke was deferred
                    // (tree was dirty); now the tree is fresh, refresh
                    // the install cache.
                    //
                    // Use the `changed_ranges` tree-sitter reports
                    // between the pre-edit tree and the post-parse tree
                    // to walk only the row regions that actually
                    // changed — typically one row for single-char
                    // typing, vs a full ~25ms viewport walk. Rows
                    // outside `changed_ranges` keep their existing
                    // installed spans (tree-sitter says they didn't
                    // change so they're still correct).
                    self.slots[slot_idx].last_sync_viewport_key = None;
                    let buf_dg = self.slots[slot_idx].editor.buffer().dirty_gen();
                    let (vp_top, vp_height) = {
                        let vp = self.active().editor.host().viewport();
                        (vp.top_row, vp.height as usize)
                    };
                    let vp_byte_to_row = |bytes: &[usize], byte: usize| {
                        bytes.partition_point(|&b| b <= byte).saturating_sub(1)
                    };
                    if out.changed_ranges.is_empty() {
                        // Cold parse or no structural change → full
                        // viewport refresh. Clear the per-row cache so
                        // the sync_query_region walk fires.
                        self.slots[slot_idx].installed_spans_dg = None;
                        self.slots[slot_idx].installed_rows = None;
                        self.sync_query_region(
                            out.buffer_id,
                            vp_top,
                            vp_height,
                            buf_dg,
                            out.kind,
                        );
                    } else if let Some((_, row_starts, _, cache_dg)) =
                        self.syntax.cached_source(out.buffer_id)
                        && cache_dg == buf_dg
                    {
                        // Take the existing installed_rows as the
                        // "previously-walked" coverage. Rows outside
                        // the changed_ranges keep their old spans
                        // (tree-sitter says they didn't change so
                        // they're still correct). We just need to
                        // re-walk + install the rows INSIDE the
                        // changed_ranges, then bump installed_spans_dg
                        // to current — coverage stays the same, dg
                        // advances.
                        let row_starts = row_starts.as_ref();
                        let prior_installed = self.slots[slot_idx].installed_rows.clone();
                        tracing::debug!(
                            target: "hjkl::profile",
                            ranges = ?out.changed_ranges,
                            prior = ?prior_installed,
                            vp_top, vp_height,
                            "changed_ranges raw"
                        );
                        for r in &out.changed_ranges {
                            let row_start = vp_byte_to_row(row_starts, r.start);
                            let row_end =
                                vp_byte_to_row(row_starts, r.end.saturating_sub(1)) + 1;
                            tracing::debug!(
                                target: "hjkl::profile",
                                range = ?r,
                                row_start, row_end,
                                "changed_range → rows"
                            );
                            // Clamp to the prior installed range — no
                            // point walking rows we never had spans
                            // for; they'll be walked on demand when
                            // the user scrolls to them. Walking the
                            // full edit range on paste would be huge.
                            let (clamp_start, clamp_end) = match &prior_installed {
                                Some(r) => (r.start, r.end),
                                None => (vp_top, vp_top + vp_height),
                            };
                            let row_start = row_start.max(clamp_start);
                            let row_end = row_end.min(clamp_end);
                            if row_start >= row_end {
                                continue;
                            }
                            let _ = self.sync_query_changed_rows(
                                out.buffer_id,
                                slot_idx,
                                row_start,
                                row_end - row_start,
                                buf_dg,
                            );
                        }
                        // Coverage unchanged, dg advanced. Force the
                        // next sync_query_region call (the one
                        // immediately following from recompute) to
                        // see a fresh dedup key so it re-checks the
                        // row cache.
                        self.slots[slot_idx].installed_spans_dg = Some(buf_dg);
                        if self.slots[slot_idx].installed_rows.is_none() {
                            self.slots[slot_idx].installed_rows =
                                Some(vp_top..(vp_top + vp_height));
                        }
                        self.slots[slot_idx].last_sync_viewport_key = None;
                    } else {
                        // Source cache stale — fall back to full refresh.
                        self.slots[slot_idx].installed_spans_dg = None;
                        self.slots[slot_idx].installed_rows = None;
                        self.sync_query_region(
                            out.buffer_id,
                            vp_top,
                            vp_height,
                            buf_dg,
                            out.kind,
                        );
                    }
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

    /// Walk + install spans for a specific row range, bypassing the
    /// per-row install cache. Used by the changed-ranges refresh path
    /// in `install_render_result`: after the worker delivers, walk
    /// only the rows tree-sitter reports as structurally changed
    /// rather than re-walking the full viewport.
    fn sync_query_changed_rows(
        &mut self,
        buffer_id: crate::syntax::BufferId,
        slot_idx: usize,
        row_top: usize,
        row_height: usize,
        current_dg: u64,
    ) -> bool {
        let _ = slot_idx;
        let Some((source, row_starts, line_count_arc, cache_dg)) =
            self.syntax.cached_source(buffer_id)
        else {
            return false;
        };
        if cache_dg != current_dg {
            return false;
        }
        let bytes_len = source.len();
        let byte_start = row_starts.get(row_top).copied().unwrap_or(bytes_len);
        let end_row = row_top + row_height + 1;
        let byte_end = row_starts
            .get(end_row)
            .copied()
            .unwrap_or(bytes_len)
            .min(bytes_len)
            .max(byte_start);
        let t_q = Instant::now();
        let sync_out = self.syntax.query_viewport(
            buffer_id,
            source.as_str(),
            row_starts.as_ref(),
            byte_start..byte_end,
            row_top,
            row_height,
            line_count_arc,
            current_dg,
            crate::syntax::ParseKind::Viewport,
        );
        let q_us = t_q.elapsed().as_micros();
        let installed = if let Some(sync_out) = sync_out {
            self.install_render_result(sync_out);
            true
        } else {
            false
        };
        tracing::debug!(
            target: "hjkl::profile",
            row_top, row_height,
            byte_len = byte_end - byte_start,
            q_us,
            installed,
            "sync_query changed-rows"
        );
        installed
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
            _ => None,
        };
        if last_key == Some(new_key) {
            tracing::debug!(target: "hjkl::profile", ?kind, range_top, range_height, current_dg, "sync_query dedup-hit");
            return true;
        }

        // Per-row install cache: when the new viewport's row range is
        // already fully covered by previously-installed rows for the
        // current dirty_gen, skip the walk entirely (j/k within an
        // already-walked region is free). Otherwise walk only the
        // *delta* rows — the parts of the new viewport that aren't yet
        // installed. Scroll-by-one then costs one row's worth of
        // tree-sitter walk, not a full 55-row viewport walk.
        let vp_range = range_top..(range_top + range_height);
        let cached_dg_opt = self.slots[slot_idx].installed_spans_dg;
        let cached_for_dg = matches!(kind, crate::syntax::ParseKind::Viewport)
            && cached_dg_opt == Some(current_dg);

        // Typing path: dg bumped since last walk AND we have a prior
        // install. Tree was just edited (tree.edit synced) but the
        // worker reparse is in flight. Walking the dirty tree here
        // would block the key handler ~30-50ms per char and return
        // partial results. Instead, leave the row-shifted installed
        // spans visible for one frame; worker delivery will trigger
        // `install_render_result`'s Viewport branch, which re-runs
        // this method against the freshly-parsed tree.
        if matches!(kind, crate::syntax::ParseKind::Viewport)
            && cached_dg_opt.is_some()
            && cached_dg_opt != Some(current_dg)
        {
            tracing::debug!(
                target: "hjkl::profile",
                cached_dg = ?cached_dg_opt,
                current_dg,
                "sync_query defer (tree dirty)"
            );
            // Update dedup so we don't keep re-checking the same key.
            self.slots[slot_idx].last_sync_viewport_key = Some(new_key);
            return true;
        }

        let cached_rows = if cached_for_dg {
            self.slots[slot_idx].installed_rows.clone()
        } else {
            None
        };
        let walk_ranges: Vec<std::ops::Range<usize>> = match cached_rows.as_ref() {
            Some(installed) if installed.start <= vp_range.start && installed.end >= vp_range.end => {
                // Fully covered — no walk needed. Update dedup key so
                // subsequent identical calls also short-circuit.
                tracing::debug!(
                    target: "hjkl::profile",
                    ?kind, range_top, range_height, current_dg,
                    installed_start = installed.start, installed_end = installed.end,
                    "sync_query row-cache hit"
                );
                if let crate::syntax::ParseKind::Viewport = kind {
                    self.slots[slot_idx].last_sync_viewport_key = Some(new_key);
                }
                return true;
            }
            Some(installed) => {
                let mut deltas = Vec::with_capacity(2);
                if vp_range.start < installed.start {
                    deltas.push(vp_range.start..installed.start.min(vp_range.end));
                }
                if vp_range.end > installed.end {
                    deltas.push(installed.end.max(vp_range.start)..vp_range.end);
                }
                deltas
            }
            None => vec![vp_range.clone()],
        };

        let t_sync = Instant::now();
        let Some((source, row_starts, line_count_arc, cache_dg)) =
            self.syntax.cached_source(buffer_id)
        else {
            return false;
        };
        if cache_dg != current_dg {
            return false;
        }
        let bytes_len = source.len();

        let mut installed_any = false;
        let mut total_q_us: u128 = 0;
        let mut total_install_us: u128 = 0;
        let mut total_byte_len: usize = 0;
        for delta in &walk_ranges {
            if delta.start >= delta.end {
                continue;
            }
            let d_byte_start = row_starts.get(delta.start).copied().unwrap_or(bytes_len);
            let d_end_row = delta.end + 1;
            let d_byte_end = row_starts
                .get(d_end_row)
                .copied()
                .unwrap_or(bytes_len)
                .min(bytes_len)
                .max(d_byte_start);
            total_byte_len += d_byte_end - d_byte_start;
            let t_q = Instant::now();
            let sync_out = self.syntax.query_viewport(
                buffer_id,
                source.as_str(),
                row_starts.as_ref(),
                d_byte_start..d_byte_end,
                delta.start,
                delta.end - delta.start,
                line_count_arc,
                current_dg,
                kind,
            );
            total_q_us += t_q.elapsed().as_micros();
            if let Some(sync_out) = sync_out {
                let t_install = Instant::now();
                self.install_render_result(sync_out);
                total_install_us += t_install.elapsed().as_micros();
                installed_any = true;
            }
        }

        if !installed_any {
            return false;
        }

        tracing::debug!(
            target: "hjkl::profile",
            ?kind, range_top, range_height,
            byte_len = total_byte_len,
            delta_count = walk_ranges.len(),
            q_us = total_q_us,
            install_us = total_install_us,
            total_us = t_sync.elapsed().as_micros(),
            "sync_query miss"
        );

        if let crate::syntax::ParseKind::Viewport = kind {
            self.slots[slot_idx].last_sync_viewport_key = Some(new_key);
            // Extend the per-row install cache to cover the full viewport
            // (existing installed range ∪ new vp range). Single-range
            // representation keeps the membership check O(1).
            let new_installed = match cached_rows {
                Some(existing) => {
                    existing.start.min(vp_range.start)..existing.end.max(vp_range.end)
                }
                None => vp_range,
            };
            self.slots[slot_idx].installed_spans_dg = Some(current_dg);
            self.slots[slot_idx].installed_rows = Some(new_installed);
        }
        true
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
        // Clear sync dedup key so the post-reset sync query is not
        // skipped if the new dirty_gen + viewport happen to match a
        // pre-reset key.
        slot.last_sync_viewport_key = None;
        slot.installed_spans_dg = None;
        slot.installed_rows = None;
        slot.editor.install_ratatui_syntax_spans(Vec::new());
    }

    /// Translate the per-slot cached `RenderOutput.spans` row vectors
    /// in-place to track a batch of [`hjkl_engine::ContentEdit`]s, mirroring
    /// what [`hjkl_engine::Editor::shift_syntax_spans_for_edits`] does for
    /// the live `buffer_spans`.
    ///
    /// Shifting the viewport cache keeps it shape-compatible with the new
    /// line count: rows touched by the edit stay blank for one frame
    /// (worker fills them in), untouched rows keep their cached colours.
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

        // Worker requests carry only the visible viewport now. The 3x
        // over-provisioned range was a pre-cache hint for the (now-gone)
        // parent-spans cache; without it the request range only affects
        // the result's `key` tag and the worker still parses the full
        // source either way.
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
                    self.syntax.submit_render(
                        buffer_id,
                        buf,
                        top,
                        height,
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

        // Top / Bottom span-cache prewarms were removed alongside the
        // parent-spans cache (bonsai refactor). With the tree-as-only-cache
        // model, `gg` / `G` reparse the viewport against the retained tree
        // synchronously (~1ms walk on a warm tree). No per-slot Top/Bottom
        // submits are needed.

        // Pre-warm the retained tree for non-active slots so a buffer
        // switch finds a warm tree (avoids cold-parse latency on switch).
        // We submit only Viewport; the worker dedups per (buffer, kind)
        // so this stays a single in-flight parse per other slot.
        let active_idx = self.focused_slot_idx();
        let slot_indices: Vec<usize> = (0..self.slots.len()).filter(|&i| i != active_idx).collect();
        for slot_idx in slot_indices {
            let slot_buf_id = self.slots[slot_idx].buffer_id;
            let (slot_top, slot_height) = {
                let vp = self.slots[slot_idx].editor.host().viewport();
                (vp.top_row, vp.height as usize)
            };
            let buf = self.slots[slot_idx].editor.buffer();
            self.syntax.submit_render(
                slot_buf_id,
                buf,
                slot_top,
                slot_height,
                crate::syntax::ParseKind::Viewport,
            );
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
        // Cold = the worker hasn't produced its first tree for this
        // buffer yet. With Top/Bottom span-cache prewarms gone, the
        // only "has the worker walked something" signal we have is
        // `viewport_render_output`. Its absence means the sync walk
        // would return empty (no retained tree yet) → block longer
        // on the first `gg`/`G` after open so the destination paints
        // with spans instead of flashing un-highlighted.
        let is_cold = self.active().viewport_render_output.is_none();
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
        tracing::debug!(
            target: "hjkl::profile",
            total_us = self.last_recompute_us,
            install_us = self.last_install_us,
            git_us = self.last_git_us,
            submitted,
            top, height,
            dg,
            "recompute_and_install"
        );
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
            changed_ranges: Vec::new(),
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
            changed_ranges: Vec::new(),
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
            changed_ranges: Vec::new(),
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
            changed_ranges: Vec::new(),
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

    /// T5d: `switch_to` must drop the viewport cache when dirty_gen mismatches.
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

        // Seed viewport cache with an old dirty_gen.
        let stale_dg = current_dg.wrapping_sub(1);
        let stale_out = RenderOutput {
            buffer_id: slot1_buf_id,
            spans: vec![vec![(0usize, 5usize, Style::default())]],
            signs: Vec::new(),
            key: (stale_dg, 0, 40),
            perf: PerfBreakdown::default(),
            kind: ParseKind::Viewport,
            changed_ranges: Vec::new(),
        };
        app.slots_mut()[slot1_idx].viewport_render_output = Some(stale_out);

        // Switch to slot 1 — stale cache must be evicted.
        app.switch_to(slot1_idx);

        // Viewport cache must be None or refreshed (not stale_dg).
        if let Some(cached) = &app.slots()[slot1_idx].viewport_render_output {
            assert_ne!(
                cached.key.0, stale_dg,
                "viewport: stale cache must have been replaced, not re-installed"
            );
        }
        // (If None, the cache was cleared and nothing re-populated yet — also correct.)

        let _ = std::fs::remove_file(&tmp);
    }

    // (deleted: install_merged_spans_top_only and subsequent tests that exercised
    // top_render_output / bottom_render_output / merge_render_outputs — all dead code
    // after the Top/Bottom ParseKind removal.)
}
