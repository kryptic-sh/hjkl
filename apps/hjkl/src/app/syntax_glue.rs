use hjkl_engine::Host;
use hjkl_engine_tui::EditorRatatuiExt;
use std::path::Path;

use hjkl_bonsai::{CommentMarkerPass, Highlighter, Theme};
use hjkl_engine::types::{Attrs, Color as EngineColor, Style as EngineStyle};
use hjkl_picker::PreviewSpans;

use hjkl_app::git::{GitChange, GitChangeKind};
use hjkl_app::git_worker::{BlameJob, GitJob};
use hjkl_buffer_tui::Sign;
use hjkl_lang::GrammarRequest;
use ratatui::style::{Color, Style};

use super::App;

/// Convert a host-agnostic [`GitChange`] into a ratatui-flavored [`Sign`].
///
/// The glyph encodes the change *kind* (`+` add, `~` modify, `_` delete); the
/// colour encodes *staged state*. Unstaged changes keep the per-kind colours
/// (green / yellow / red). Staged changes (already in the index) render in
/// **blue** regardless of kind, so the gutter shows at a glance which changes
/// are staged vs not. Unstaged signs sit at a slightly higher priority so an
/// overlapping row reads as "still has unstaged work".
fn change_to_sign(c: GitChange) -> Sign {
    let ch = match c.kind {
        GitChangeKind::Add => '+',
        GitChangeKind::Modify => '~',
        GitChangeKind::Delete => '_',
    };
    let (style, priority) = if c.staged {
        (Style::default().fg(Color::Blue), 49)
    } else {
        let color = match c.kind {
            GitChangeKind::Add => Color::Green,
            GitChangeKind::Modify => Color::Yellow,
            GitChangeKind::Delete => Color::Red,
        };
        (Style::default().fg(color), 50)
    };
    Sign {
        row: c.row,
        ch,
        style,
        priority,
    }
}

impl App {
    /// Queue a git diff-sign refresh for the current buffer (throttled).
    pub(crate) fn refresh_git_signs(&mut self) {
        self.refresh_git_signs_inner(false);
    }

    /// Queue a git diff-sign refresh for the current buffer, bypassing
    /// the 250 ms throttle. Also recomputes the explorer's on-disk git base so
    /// save / stage / checkout sites all get fresh sidebar colors.
    pub(crate) fn refresh_git_signs_force(&mut self) {
        self.refresh_git_signs_inner(true);
        self.recompute_explorer_git_base();
    }

    pub(crate) fn refresh_git_signs_inner(&mut self, force: bool) {
        use std::time::{Duration, Instant};
        const REFRESH_MIN_INTERVAL: Duration = Duration::from_millis(250);

        let path = match self.active().filename.as_deref() {
            Some(p) => p.to_path_buf(),
            None => {
                let slot = self.active_mut();
                slot.git_signs.clear();
                slot.last_git_dirty_gen = None;
                return;
            }
        };

        // Lazily probe and cache whether the file's directory is inside a git
        // repo. This probe runs once per slot and is cached on `git_repo_present`.
        // When the result is `false`, skip all git logic (no libgit2 diff calls).
        if self.active().git_repo_present.is_none() {
            let present = hjkl_app::git::path_in_repo(&path);
            self.active_mut().git_repo_present = Some(present);
        }
        if self.active().git_repo_present == Some(false) {
            let slot = self.active_mut();
            slot.git_signs.clear();
            slot.last_git_dirty_gen = None;
            return; // gate: no-repo → skip all git work
        }

        let dg = self.active_editor().buffer().dirty_gen();
        if !force && self.active().last_git_dirty_gen == Some(dg) {
            return;
        }
        let now = Instant::now();
        if !force && now.duration_since(self.active().last_git_refresh_at) < REFRESH_MIN_INTERVAL {
            return;
        }

        // O(1) rope clone — Arc-clone of the root node. Worker thread
        // materializes the byte buffer; main thread pays nothing here.
        let rope = self.active_editor().buffer().rope();
        let buffer_id = self.active().buffer_id;
        self.active_mut().last_git_refresh_at = now;

        self.git_worker.submit(GitJob {
            buffer_id,
            path,
            rope,
            dirty_gen: dg,
        });
    }

    /// Drain completed git-sign results from the worker and install them.
    pub(crate) fn poll_git_signs(&mut self) -> bool {
        let mut redraw = false;
        while let Some(result) = self.git_worker.try_recv() {
            if let Some(slot) = self
                .slots
                .iter_mut()
                .find(|s| s.buffer_id == result.buffer_id)
                && slot
                    .last_git_dirty_gen
                    .is_none_or(|dg| dg <= result.dirty_gen)
            {
                slot.git_signs = result.changes.into_iter().map(change_to_sign).collect();
                slot.is_untracked = result.is_untracked;
                slot.last_git_dirty_gen = Some(result.dirty_gen);
                redraw = true;
            }
        }
        if redraw {
            self.refresh_explorer_git();
        }
        redraw
    }

    /// Overlay open dirty buffers' git state onto the cached disk base and
    /// re-tag the explorer tree. Cheap: no git syscalls. No-op when the
    /// explorer is closed or its root isn't in a git repo.
    ///
    /// The merged map starts from `git_base` (on-disk state), then upgrades
    /// any entry whose buffer is open and dirty (has git signs or is
    /// untracked). When a buffer's edits are undone so it matches HEAD,
    /// `git_signs` is empty → no overlay → the node falls back to
    /// `git_base` (clean). Returns `true` when the tree was re-tagged.
    pub(crate) fn refresh_explorer_git(&mut self) -> bool {
        let Some(pane) = self.explorer.as_ref() else {
            return false;
        };
        if !pane.tree.repo_present {
            return false;
        }
        let mut map = pane.tree.git_base.clone();
        for slot in &self.slots {
            let Some(fname) = slot.filename.as_ref() else {
                continue;
            };
            // Overlay only reflects UNSAVED buffer state on top of the disk base.
            // Use `slot.dirty` (buffer differs from the on-disk file), NOT
            // `git_signs` (buffer-vs-HEAD): after `git add` the file is saved
            // (not dirty) but still differs from HEAD, so keying on git_signs
            // would mask the freshly-staged status. The disk base already holds
            // the correct staged/modified/untracked for the saved-on-disk file.
            let cand = if slot.is_untracked {
                hjkl_app::git::ExplorerGit::Untracked
            } else if slot.dirty {
                hjkl_app::git::ExplorerGit::Modified
            } else {
                continue;
            };
            if let Some(key) = hjkl_app::git::explorer_key_for(fname) {
                // Merge with precedence: keep the higher-priority status.
                let entry = map.entry(key).or_insert(cand);
                if crate::app::explorer::git_status_priority(cand)
                    > crate::app::explorer::git_status_priority(*entry)
                {
                    *entry = cand;
                }
            }
        }
        let pane = self.explorer.as_mut().unwrap();
        pane.tree.retag_git(&map);
        true
    }

    /// Recompute the explorer's on-disk git status base via git, then
    /// re-apply the live overlay. Call when on-disk git state may have
    /// changed (save / stage / checkout). No-op when explorer is closed or
    /// root is not in a git repo.
    pub(crate) fn recompute_explorer_git_base(&mut self) -> bool {
        let Some(pane) = self.explorer.as_ref() else {
            return false;
        };
        if !pane.tree.repo_present {
            return false;
        }
        let root = pane.tree.root.clone();
        let base = hjkl_app::git::explorer_status_map(&root);
        if let Some(pane) = self.explorer.as_mut() {
            pane.tree.git_base = base;
        }
        self.refresh_explorer_git()
    }

    /// Queue a git blame refresh for the current buffer (throttled).
    ///
    /// No-op when neither `blame_inline` nor the BLAME view is on (and
    /// clears stale data in that case). `force = true` bypasses the dirty-gen
    /// dedup and the 250 ms throttle so the column populates immediately on
    /// toggle.
    pub(crate) fn refresh_blame(&mut self) {
        self.refresh_blame_inner(false);
    }

    /// Force-refresh blame, bypassing the throttle and dirty-gen dedup.
    /// Used by `toggle_blame_column` so data populates immediately on enable.
    pub(crate) fn refresh_blame_force(&mut self) {
        self.refresh_blame_inner(true);
    }

    fn refresh_blame_inner(&mut self, force: bool) {
        use std::time::{Duration, Instant};
        const BLAME_MIN_INTERVAL: Duration = Duration::from_millis(250);

        // When neither inline nor blame-view is on, clear stale data and bail.
        let blame_inline = self.active_editor().settings().blame_inline;
        let blame_view = self.active_editor().is_blame();
        if !blame_inline && !blame_view {
            let slot = self.active_mut();
            slot.blame.clear();
            slot.last_blame_dirty_gen = None;
            return;
        }

        let path = match self.active().filename.as_deref() {
            Some(p) => p.to_path_buf(),
            None => {
                let slot = self.active_mut();
                slot.blame.clear();
                slot.last_blame_dirty_gen = None;
                return;
            }
        };

        let dg = self.active_editor().buffer().dirty_gen();
        if !force && self.active().last_blame_dirty_gen == Some(dg) {
            return;
        }

        let now = Instant::now();
        if !force && now.duration_since(self.active().last_blame_refresh_at) < BLAME_MIN_INTERVAL {
            return;
        }

        let rope = self.active_editor().buffer().rope();
        let buffer_id = self.active().buffer_id;
        self.active_mut().last_blame_refresh_at = now;

        self.blame_worker.submit(BlameJob {
            buffer_id,
            path,
            rope,
            dirty_gen: dg,
        });
    }

    /// Drain completed blame results from the worker and install them.
    pub(crate) fn poll_blame(&mut self) -> bool {
        let mut redraw = false;
        while let Some(result) = self.blame_worker.try_recv() {
            if let Some(slot) = self
                .slots
                .iter_mut()
                .find(|s| s.buffer_id == result.buffer_id)
                && slot
                    .last_blame_dirty_gen
                    .is_none_or(|dg| dg <= result.dirty_gen)
            {
                slot.blame = result.blame;
                slot.last_blame_dirty_gen = Some(result.dirty_gen);
                redraw = true;
            }
        }
        redraw
    }

    /// Poll in-flight async grammar loads and wire any that completed.
    ///
    /// Returns `true` when at least one load resolved and a redraw is needed.
    pub(crate) fn poll_grammar_loads(&mut self) -> bool {
        let events = self.syntax.poll_pending_loads();
        if events.is_empty() {
            return false;
        }
        for event in &events {
            use crate::syntax::LoadEventKind;
            hjkl_syntax::SyntaxLayer::dispatch_load_event(event, |kind| match kind {
                LoadEventKind::Ready { id, name } => {
                    tracing::debug!("grammar load complete: {name} (buffer {id})");
                    // Re-attach the grammar now that it's ready.
                    if let Some(slot) = self.slots.iter().find(|s| s.buffer_id == id)
                        && let Some(ref p) = slot.filename.clone()
                    {
                        let _ = self.syntax.set_language_for_path(id, p);
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

    /// Poll in-flight anvil install handles each tick.
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
                    InstallStatus::Queued => {}
                    InstallStatus::TofuRecorded { triple, sha256 } => {
                        self.bus
                            .info(format!("anvil: {name} TOFU hash recorded for {triple}"));
                        let _ = sha256;
                    }
                }
            }
        }

        for name in to_remove {
            self.anvil_handles.remove(&name);
        }

        redraw
    }

    /// Handle a `take_content_reset` event on the active buffer.
    pub(crate) fn handle_active_content_reset(&mut self, buffer_id: crate::syntax::BufferId) {
        self.syntax.reset(buffer_id);
        let active_idx = self.focused_slot_idx();
        self.slots[active_idx]
            .editor
            .install_ratatui_syntax_spans(Vec::new());
    }

    /// Run `render_viewport` for the active buffer and install the result.
    /// Returns empty spans when no grammar is attached or grammar is loading.
    pub(crate) fn recompute_and_install(&mut self) {
        if !self.syntax_enabled || !self.active().features.syntax {
            return;
        }
        let buffer_id = self.active().buffer_id;
        let (top, height) = {
            // Compute union viewport across all windows showing the same slot.
            let focused_slot = self.focused_slot_idx();
            let (focused_top, focused_height) = {
                let vp = self.active_editor().host().viewport();
                (vp.top_row, vp.height as usize)
            };
            let mut union_top = focused_top;
            let mut union_bot = focused_top + focused_height;
            // Each window's scroll comes from its own editor (#151 Phase D);
            // collect (win_id, rect) first to avoid borrowing self twice.
            let wins: Vec<(crate::app::window::WindowId, crate::app::window::LayoutRect)> = self
                .windows
                .iter()
                .enumerate()
                .filter_map(|(i, w)| {
                    let w = w.as_ref()?;
                    if w.slot == focused_slot {
                        w.last_rect.map(|r| (i, r))
                    } else {
                        None
                    }
                })
                .collect();
            for (wid, rect) in wins {
                let top = self.window_scroll(wid).0;
                union_top = union_top.min(top);
                union_bot = union_bot.max(top + rect.h as usize);
            }
            (union_top, union_bot - union_top)
        };

        let active_idx = self.focused_slot_idx();
        // Push colorizer option state from the active editor's settings before
        // rendering — Options own the source of truth, SyntaxLayer mirrors so
        // walk_rows can gate without a re-borrow into the editor.
        let (clz, clz_ft, rb) = {
            let s = self.slots[active_idx].editor.settings();
            (
                s.colorizer,
                s.colorizer_filetypes.clone(),
                s.rainbow_brackets,
            )
        };
        self.syntax.set_colorizer(clz, clz_ft);
        self.syntax.set_rainbow_brackets(rb);
        let buf = self.slots[active_idx].editor.buffer();

        // BUG 1 fix: `height` is measured in SCREEN rows, but closed folds
        // hide doc rows without consuming a screen row.  The renderer walks
        // more doc rows than `height` to fill the screen; rows beyond
        // `top + height` would miss syntax spans.  Expand the requested
        // doc-row range by counting hidden rows inside it so the syntax
        // layer covers the full visible doc range.
        let effective_height = {
            let row_count = buf.row_count();
            let mut screen = 0usize;
            let mut doc = top;
            while screen < height && doc < row_count {
                if buf.is_row_hidden(doc) {
                    // Hidden rows cost no screen rows but advance doc.
                    doc += 1;
                } else {
                    screen += 1;
                    doc += 1;
                }
            }
            // `doc` is now the first doc row beyond the visible window.
            // The span range must cover `top..doc`, i.e. `doc - top` rows.
            doc.saturating_sub(top).max(height)
        };

        let out = self
            .syntax
            .render_viewport(buffer_id, buf, top, effective_height);

        // When a language server owns this buffer's diagnostics, drop the
        // tree-sitter parse-error gutter signs: one syntax error makes TS
        // error-recovery flag a cascade of downstream lines, which conflicts
        // with the server's precise per-line diagnostics. Keep TS signs only
        // as a fallback for buffers with no LSP attached.
        let suppress_ts_diag_signs = self.slot_has_lsp(active_idx);

        if let Some(out) = out {
            let start = out.key.1;
            let end = start + out.spans.len();
            self.slots[active_idx]
                .editor
                .patch_ratatui_syntax_spans_range(start..end, &out.spans);
            self.slots[active_idx].diag_signs = if suppress_ts_diag_signs {
                Vec::new()
            } else {
                out.signs
            };
        } else {
            // No spans available (no language or grammar still loading).
            // Clear stale spans and let the renderer draw plain text.
            self.slots[active_idx]
                .editor
                .install_ratatui_syntax_spans(Vec::new());
        }

        self.refresh_git_signs();
        self.refresh_blame();

        // --- Auto-folds (foldmethod=expr / marker) ---
        // After render_viewport triggers a reparse, extract fold ranges and
        // apply them to the buffer. Runs only when dirty_gen has advanced
        // since the last fold pass — once per edit, never per-frame.
        let (fdm, fen, fls) = {
            // Settings are per-window (#151 Phase D): read the focused window's
            // editor, where `:set foldmethod=…` lands.
            let s = self.active_editor().settings();
            (s.foldmethod, s.foldenable, s.foldlevelstart)
        };
        let dg = self.slots[active_idx].editor.buffer().dirty_gen();
        let last_fold_dg = self.slots[active_idx].last_fold_dirty_gen;

        if fen && last_fold_dg != Some(dg) {
            use hjkl_engine::types::FoldMethod;
            let default_closed = fls == 0;
            match fdm {
                FoldMethod::Expr => {
                    // Extract fold ranges from the (now fresh) tree.
                    // `extract_fold_ranges` returns `None` when the grammar is
                    // not yet ready (still loading or unknown extension). In
                    // that case we must NOT record the dirty_gen as processed —
                    // the next recompute after the grammar finishes loading will
                    // see `last_fold_dg != Some(dg)` and re-run against the
                    // now-ready tree.
                    let ranges_opt = {
                        let buf = self.slots[active_idx].editor.buffer();
                        self.syntax.extract_fold_ranges(buffer_id, buf)
                    };
                    if let Some(ranges) = ranges_opt {
                        if !ranges.is_empty() {
                            // set_auto_folds preserves open/closed state for
                            // folds at the same start_row; new folds default
                            // open (fls >= 99) or closed (fls == 0); manual
                            // folds are never touched.
                            self.slots[active_idx]
                                .editor
                                .buffer_mut()
                                .set_auto_folds(&ranges, default_closed);
                        }
                        // Grammar was ready and extraction ran (even if no
                        // folds found). Mark this dirty_gen as processed.
                        self.slots[active_idx].last_fold_dirty_gen = Some(dg);
                    }
                    // If ranges_opt is None (grammar not ready),
                    // last_fold_dirty_gen stays at the old value — retried on
                    // the next recompute_and_install when the load completes.
                }
                FoldMethod::Marker => {
                    // Marker folds are grammar-independent: a pure text scan over
                    // the rope. They work on any file, so there is no "not ready"
                    // state — always record dirty_gen. We recognize TWO marker
                    // pairs: the configured `:set foldmarker=open,close` (default
                    // vim `{{{` / `}}}`, used when unset or malformed) AND the
                    // universal `#region` / `#endregion` convention.
                    let fmr = self.active_editor().settings().foldmarker.clone();
                    let (open, close) = match fmr.split_once(',') {
                        Some((o, c)) if !o.is_empty() && !c.is_empty() => {
                            (o.to_string(), c.to_string())
                        }
                        _ => (
                            hjkl_bonsai::DEFAULT_FOLD_MARKER_OPEN.to_string(),
                            hjkl_bonsai::DEFAULT_FOLD_MARKER_CLOSE.to_string(),
                        ),
                    };
                    // Always call set_auto_folds (even with an empty range list)
                    // so that removing the last marker also removes its fold.
                    let ranges = {
                        let buf = self.slots[active_idx].editor.buffer();
                        hjkl_bonsai::extract_marker_fold_ranges_rope_multi(
                            &buf.rope(),
                            &[
                                (open.as_str(), close.as_str()),
                                (
                                    hjkl_bonsai::DEFAULT_REGION_MARKER_OPEN,
                                    hjkl_bonsai::DEFAULT_REGION_MARKER_CLOSE,
                                ),
                            ],
                        )
                    };
                    self.slots[active_idx]
                        .editor
                        .buffer_mut()
                        .set_auto_folds(&ranges, default_closed);
                    self.slots[active_idx].last_fold_dirty_gen = Some(dg);
                }
                FoldMethod::Manual => {}
            }
        }
    }

    /// Compute syntax highlight spans for a one-off preview snippet.
    pub fn preview_spans_for(&self, path: &Path, bytes: &[u8]) -> PreviewSpans {
        self.preview_spans_for_range(path, bytes, 0..bytes.len())
    }

    /// Viewport-clipped variant of [`Self::preview_spans_for`].
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

    /// `:syntax on` / `:syntax off` — toggle bonsai highlighting app-wide.
    pub(crate) fn set_syntax_enabled(&mut self, enabled: bool) {
        if self.syntax_enabled == enabled {
            return;
        }
        self.syntax_enabled = enabled;
        if !enabled {
            for slot in &mut self.slots {
                slot.editor.install_ratatui_syntax_spans(Vec::new());
                slot.diag_signs.clear();
            }
            // Window editors hold their own span cache (#151 Phase D) — clear too.
            for ed in self.window_editors.values_mut() {
                ed.install_ratatui_syntax_spans(Vec::new());
            }
        } else {
            for i in 0..self.slots.len() {
                let buffer_id = self.slots[i].buffer_id;
                if let Some(p) = self.slots[i].filename.clone() {
                    let _ = self.syntax.set_language_for_path(buffer_id, &p);
                }
            }
            self.recompute_and_install();
        }
    }
}

/// Number of off-screen rows above/below the visible window to include in the
/// highlighter's byte range for picker preview injection resolution.
const VIEWPORT_SLACK_ROWS: usize = 50;

/// Find the byte offset where row `target_row` begins (row 0 = byte 0). For
/// `target_row` past the end, returns `bytes.len()`.
///
/// Picker preview scroll/keystroke handling calls this on every frame, so a
/// per-byte linear scan re-walked the whole prefix from offset 0 each time
/// (audit D5). `memchr::memchr_iter` gives the same result via a SIMD
/// newline scan — same pattern `hjkl-bonsai`'s comment_markers.rs already
/// uses for the same reason.
fn byte_offset_of_row(bytes: &[u8], target_row: usize) -> usize {
    if target_row == 0 {
        return 0;
    }
    memchr::memchr_iter(b'\n', bytes)
        .nth(target_row - 1)
        .map(|i| i + 1)
        .unwrap_or(bytes.len())
}

/// Bridge: route `hjkl-picker`'s preview-pane highlighter through the
/// editor's bonsai pipeline.
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

    /// Regression: install_render_result no longer exists; the equivalent is
    /// recompute_and_install. This test verifies the picker preview injection
    /// wiring still works.
    #[test]
    #[ignore = "network + compiler: fetches markdown + rust grammars"]
    fn preview_spans_for_markdown_includes_rust_injection() {
        let app = App::new(None, false, None, None).unwrap();

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

        const RUST_ROW: usize = 3;
        assert!(
            spans.by_row.len() > RUST_ROW,
            "expected at least {} rows, got {}",
            RUST_ROW + 1,
            spans.by_row.len()
        );
        let rust_row = &spans.by_row[RUST_ROW];
        assert!(
            rust_row.len() >= 3,
            "expected ≥3 styled spans on the rust row (keyword/function/punct from injection); \
             got {} spans: {:?}",
            rust_row.len(),
            rust_row
        );
    }

    /// Reference oracle: the original per-byte linear scan `byte_offset_of_row`
    /// used before the audit D5 fix switched it to `memchr::memchr_iter`.
    /// Kept here only to pin the new implementation's output against the old
    /// one across the row/content combinations that would expose a
    /// mismatch — off-by-one boundaries, row 0, and multibyte content.
    fn byte_offset_of_row_linear_scan(bytes: &[u8], target_row: usize) -> usize {
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

    #[test]
    fn byte_offset_of_row_matches_the_old_linear_scan() {
        // Includes multibyte UTF-8 content (emoji + accented text), a blank
        // line, and no trailing newline on the last line — none of which
        // should affect a byte-oriented `\n` scan, but pin them anyway.
        let text = "fn café() {\n    🦀\n}\n\nlast line, no trailing newline";
        let bytes = text.as_bytes();
        let row_count = bytes.iter().filter(|&&b| b == b'\n').count() + 1;

        // Row 0, every real row, and a couple of past-end rows.
        for target_row in 0..=(row_count + 3) {
            assert_eq!(
                byte_offset_of_row(bytes, target_row),
                byte_offset_of_row_linear_scan(bytes, target_row),
                "mismatch at target_row={target_row}"
            );
        }
    }

    #[test]
    fn byte_offset_of_row_past_end_returns_len() {
        let bytes = b"a\nb\nc";
        assert_eq!(byte_offset_of_row(bytes, 100), bytes.len());
    }

    #[test]
    fn byte_offset_of_row_zero_is_always_zero() {
        assert_eq!(byte_offset_of_row(b"", 0), 0);
        assert_eq!(byte_offset_of_row(b"abc\ndef", 0), 0);
    }
}
