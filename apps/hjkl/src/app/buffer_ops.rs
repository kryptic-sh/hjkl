use hjkl_buffer::View;
use hjkl_engine::{Host, MarkJump, Settings};
use hjkl_vim::VimEditorExt;
use std::path::PathBuf;

use super::{App, DiskState, STATUS_LINE_HEIGHT};

impl App {
    /// Reset slot `idx`'s document to a fresh, empty, unnamed scratch
    /// buffer, discarding its content, undo history, and file identity.
    /// Used whenever the sole remaining slot must fall back to `[No Name]`
    /// instead of being removed (`:bdelete`/`:bwipeout` on the only
    /// buffer; aborting a stale-swap recovery prompt on a single-file
    /// launch).
    ///
    /// Only replaces the document handle + settings template and the
    /// file-identity/disk-state slot fields. Callers remain responsible for
    /// anything else that should be discarded first, for LSP
    /// detach/diagnostics, and for post-reset bookkeeping (window slot
    /// pointers, `reconcile_window_editors` — which rebuilds every window
    /// editor showing this slot from scratch, since the content `Arc`
    /// changes, discarding their marks/jumplists/undo too — `fs_watch_sync`,
    /// the status message).
    pub(crate) fn reset_slot_to_scratch(&mut self, idx: usize) {
        let old_id = self.slots[idx].buffer_id;
        self.syntax.forget(old_id);
        // The old buffer is fully discarded here (its content is gone) —
        // prune its changelist bank so it doesn't leak (audit B3), mirroring
        // `syntax.forget` right above.
        self.change_banks.remove(&old_id);
        let new_id = self.next_buffer_id;
        self.next_buffer_id += 1;
        let slot = &mut self.slots[idx];
        slot.buffer_id = new_id;
        slot.view = View::new();
        slot.settings = Settings::default();
        slot.filename = None;
        slot.dirty = false;
        slot.is_new_file = false;
        slot.is_untracked = false;
        slot.diag_signs.clear();
        slot.git_signs.clear();
        slot.last_git_dirty_gen = None;
        slot.git_repo_present = None; // re-probe on next edit
        slot.saved_hash = 0;
        slot.saved_len = 0;
        slot.disk_mtime = None;
        slot.disk_len = None;
        slot.disk_state = DiskState::Synced;
        slot.snapshot_saved();
    }

    /// Switch the focused window to display slot `idx` and refresh its
    /// viewport spans.  Records the previous slot index in `prev_active`
    /// for alt-buffer (`<C-^>` / `:b#`).
    pub(crate) fn switch_to(&mut self, idx: usize) {
        // The explorer scratch buffer is never a switch target — it's managed
        // as its own pane, not a normal buffer.
        if self.slots.get(idx).is_some_and(|s| s.is_explorer) {
            return;
        }
        // Never load a normal buffer into the explorer pane. If the explorer is
        // focused (e.g. clicking a buffer-line entry while it's focused),
        // redirect to the nearest non-explorer window first.
        if self.explorer_buf_focused()
            && let Some(win_id) = self.nearest_non_explorer_window()
        {
            self.switch_focus(win_id);
        }
        let current_slot = self.focused_slot_idx();
        if idx != current_slot {
            self.prev_active = Some(current_slot);
        }
        // Update the synthetic `%` register with the new slot's filename so
        // `"%p`, `<C-r>%`, and `:echo @%` reflect the correct path.
        let fname = self.slots[idx]
            .filename
            .as_deref()
            .map(|p| p.to_string_lossy().into_owned());
        self.registers.lock().unwrap().set_filename(fname);
        // Point the focused window at the new slot.
        let fw = self.focused_window();
        self.windows[fw].as_mut().expect("focused_window open").slot = idx;
        // Rebuild the focused window's view editor onto the new slot's Buffer
        // (#151 Phase D) so active_editor() below sees the switched buffer.
        self.reconcile_window_editors();
        if let Ok(size) = crossterm::terminal::size() {
            let vp = self.active_editor_mut().host_mut().viewport_mut();
            vp.width = size.0;
            vp.height = size.1.saturating_sub(STATUS_LINE_HEIGHT);
        }
        // recompute_and_install runs render_viewport sync (post fully-sync
        // refactor) — no need for a preview_render warm-up paint.
        self.recompute_and_install();
        self.refresh_git_signs_force();
        // Follow the new active buffer in the explorer (select its row).
        self.explorer_reveal_active();
    }

    /// `:bnext` — cycle active forward, skipping `is_explorer` slots.
    /// No-op when only one non-explorer slot.
    pub(crate) fn buffer_next(&mut self) {
        if !self.require_multi_buffer() {
            return;
        }
        let n = self.slots.len();
        let current = self.focused_slot_idx();
        // Walk forward, skipping explorer slots. Guard against all-explorer edge.
        let next = (1..=n).find_map(|i| {
            let idx = (current + i) % n;
            if !self.slots[idx].is_explorer {
                Some(idx)
            } else {
                None
            }
        });
        if let Some(next) = next {
            self.switch_to(next);
        }
    }

    /// `:bprev` — cycle active backward, skipping `is_explorer` slots.
    /// No-op when only one non-explorer slot.
    pub(crate) fn buffer_prev(&mut self) {
        if !self.require_multi_buffer() {
            return;
        }
        let n = self.slots.len();
        let current = self.focused_slot_idx();
        let prev = (1..=n).find_map(|i| {
            let idx = (current + n - i) % n;
            if !self.slots[idx].is_explorer {
                Some(idx)
            } else {
                None
            }
        });
        if let Some(prev) = prev {
            self.switch_to(prev);
        }
    }

    /// `<C-^>` / `:b#` — switch to the previously-active buffer slot.
    pub(crate) fn buffer_alt(&mut self) {
        if !self.require_multi_buffer() {
            return;
        }
        match self.prev_active {
            Some(i) if i < self.slots.len() => {
                self.switch_to(i);
            }
            _ => {
                self.bus.warn("no alternate buffer");
            }
        }
    }

    /// `:bdelete[!]` — close the active slot. With more than one slot
    /// open the slot is removed; on the last slot the buffer is reset
    /// to an empty unnamed scratch buffer (vim parity for `:bd` on the
    /// only buffer leaving an empty editor instead of quitting).
    pub(crate) fn buffer_delete(&mut self, force: bool) {
        if !force && self.active().dirty {
            self.bus
                .error("E89: No write since last change (add ! to override)");
            return;
        }
        let active_slot = self.focused_slot_idx();
        if self.slots.len() == 1 {
            self.lsp_detach_buffer(active_slot);
            self.reset_slot_to_scratch(0);
            // Keep all windows pointing at slot 0 (the only one).
            for win in self.windows.iter_mut().flatten() {
                win.slot = 0;
            }
            // Rebuild window view editors onto the replacement Buffer (#151 Phase D).
            self.reconcile_window_editors();
            // No file open in slot 0 anymore — stop watching it (#242).
            self.fs_watch_sync();
            self.bus.info("buffer closed (replaced with [No Name])");
            return;
        }
        self.lsp_detach_buffer(active_slot);
        let mut removed = self.slots.remove(active_slot);
        self.syntax.forget(removed.buffer_id);
        // The buffer is fully closed — prune its changelist bank so it
        // doesn't leak (audit B3), mirroring `syntax.forget` above.
        self.change_banks.remove(&removed.buffer_id);
        // Drop the closed buffer's swap. The owning process stays alive, so the
        // orphan scan never reaps it, and the slot is gone so cleanup_swaps_on_exit
        // can't either — leaving it makes a later open of the same file surface a
        // spurious recovery prompt.
        if let Some(p) = removed.swap_path.take() {
            let _ = hjkl_app::swap::remove_swap(&p);
        }
        // Fix up all window slot pointers that reference the removed or shifted slots.
        let slot_count = self.slots.len();
        for win in self.windows.iter_mut().flatten() {
            if win.slot == active_slot {
                // Was pointing at the removed slot — redirect to slot before it (or 0).
                win.slot = if active_slot > 0 { active_slot - 1 } else { 0 };
            } else if win.slot > active_slot {
                // Shift down due to the Vec::remove.
                win.slot -= 1;
            }
            // Clamp to valid range just in case.
            win.slot = win.slot.min(slot_count.saturating_sub(1));
        }
        let target = self.focused_slot_idx();
        self.switch_to(target);
        // Clear alt-buffer pointer after the switch: prev_active may refer
        // to a removed or re-indexed slot. Reset unconditionally.
        self.prev_active = None;
        // The removed slot's file (if any) may no longer be open — resync (#242).
        self.fs_watch_sync();
        let name = removed
            .filename
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "[No Name]".into());
        self.bus.info(format!("buffer closed: \"{name}\""));
    }

    /// Close buffer slot `idx` triggered by a mouse click on the `✕` glyph.
    ///
    /// Switches focus to the target slot first (so `buffer_delete` operates on
    /// it), then calls `buffer_delete(false)` — preserving the unsaved-changes
    /// guard: a dirty buffer emits E89 rather than silently discarding changes.
    pub(crate) fn close_buffer_slot(&mut self, idx: usize) {
        if idx != self.focused_slot_idx() {
            self.switch_to(idx);
        }
        self.buffer_delete(false);
    }

    /// `:bwipeout[!]` — completely remove the active buffer: drop marks,
    /// jumplist entries, and all per-buffer cached state.  With more than
    /// one slot open the slot is removed (same mechanics as `buffer_delete`
    /// since the slot — and its editor — vanish entirely).  On the last
    /// slot a fresh scratch buffer is installed and the old editor's marks
    /// and jumplists are explicitly discarded before replacement, ensuring
    /// no state leaks into the new session.
    pub(crate) fn buffer_wipe(&mut self, force: bool) {
        if !force && self.active().dirty {
            self.bus
                .error("E89: No write since last change (add ! to override)");
            return;
        }
        let active_slot = self.focused_slot_idx();
        if self.slots.len() == 1 {
            // No explicit mark/jumplist wipe needed here (#151 Stage 2b
            // removed it as dead weight): `reset_slot_to_scratch` below
            // installs a brand-new `View` (fresh `Arc<Mutex<Buffer>>`, so
            // the old shared marks map is simply dropped) and
            // `reconcile_window_editors` rebuilds every window editor
            // showing this slot from scratch (fresh `jump_back`/`jump_fwd`)
            // since the content `Arc` changed — old state can't leak in
            // either case.
            //
            // Also clear LSP diagnostics for the wiped buffer.
            {
                let slot = &mut self.slots[0];
                slot.lsp_diags.clear();
                slot.diag_signs_lsp.clear();
            }
            self.lsp_detach_buffer(active_slot);
            self.reset_slot_to_scratch(0);
            // Keep all windows pointing at slot 0 (the only one).
            for win in self.windows.iter_mut().flatten() {
                win.slot = 0;
            }
            // Rebuild window view editors onto the fresh scratch Buffer (#151 Phase D).
            self.reconcile_window_editors();
            // No file open in slot 0 anymore — stop watching it (#242).
            self.fs_watch_sync();
            self.bus.info("buffer wiped (replaced with [No Name])");
            return;
        }
        // Multi-slot: removing the slot entirely discards the editor (and all
        // its marks/jumps) — same mechanics as buffer_delete.
        self.lsp_detach_buffer(active_slot);
        let mut removed = self.slots.remove(active_slot);
        self.syntax.forget(removed.buffer_id);
        // The buffer is fully wiped — prune its changelist bank (audit B3),
        // mirroring `syntax.forget` above.
        self.change_banks.remove(&removed.buffer_id);
        // Drop the closed buffer's swap (see buffer_delete for rationale).
        if let Some(p) = removed.swap_path.take() {
            let _ = hjkl_app::swap::remove_swap(&p);
        }
        let slot_count = self.slots.len();
        for win in self.windows.iter_mut().flatten() {
            if win.slot == active_slot {
                win.slot = if active_slot > 0 { active_slot - 1 } else { 0 };
            } else if win.slot > active_slot {
                win.slot -= 1;
            }
            win.slot = win.slot.min(slot_count.saturating_sub(1));
        }
        let target = self.focused_slot_idx();
        self.switch_to(target);
        self.prev_active = None;
        // The removed slot's file (if any) may no longer be open — resync (#242).
        self.fs_watch_sync();
        let name = removed
            .filename
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "[No Name]".into());
        self.bus.info(format!("buffer wiped: \"{name}\""));
    }

    /// Returns `true` when multiple non-explorer slots are open; otherwise
    /// sets the "only one buffer open" status message and returns `false`.
    pub(crate) fn require_multi_buffer(&mut self) -> bool {
        let real_count = self.slots.iter().filter(|s| !s.is_explorer).count();
        if real_count <= 1 {
            self.bus.warn("only one buffer open");
            return false;
        }
        true
    }

    /// `:ls` / `:buffers` — render the buffer list to a single status
    /// line. Marks: `%` active, `+` modified. Explorer slots are excluded.
    pub(crate) fn list_buffers(&self) -> String {
        let active_slot = self.focused_slot_idx();
        let mut parts = Vec::with_capacity(self.slots.len());
        for (i, slot) in self.slots.iter().enumerate() {
            if slot.is_explorer {
                continue;
            }
            let active = if i == active_slot { '%' } else { ' ' };
            let modf = if slot.dirty { '+' } else { ' ' };
            let name = slot
                .filename
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "[No Name]".into());
            parts.push(format!("{}:{active}{modf} \"{name}\"", i + 1));
        }
        parts.join(" | ")
    }

    // ── nvim-api helpers ──────────────────────────────────────────────────────

    /// View ids of all non-explorer slots, as `u64` (nvim wire format).
    pub(crate) fn nvim_buffer_ids(&self) -> Vec<u64> {
        self.slots
            .iter()
            .filter(|s| !s.is_explorer)
            .map(|s| s.buffer_id)
            .collect()
    }

    /// View id of the currently focused slot, as `u64`.
    pub(crate) fn nvim_current_buffer_id(&self) -> u64 {
        self.active().buffer_id
    }

    /// Index into `self.slots` whose `buffer_id` matches `id`, or `None`.
    pub(crate) fn nvim_slot_index_for_buffer(&self, id: u64) -> Option<usize> {
        self.slots.iter().position(|s| s.buffer_id == id)
    }

    /// Absolute-path filename for the slot with `buffer_id == id`.
    /// Returns `""` when the slot has no filename (unnamed scratch buffer).
    pub(crate) fn nvim_buffer_name(&self, id: u64) -> Option<String> {
        let slot = self.slots.iter().find(|s| s.buffer_id == id)?;
        Some(match &slot.filename {
            None => String::new(),
            Some(p) => {
                // Try to canonicalize (resolves symlinks + relative paths);
                // fall back to whatever we have if the file doesn't exist yet.
                std::fs::canonicalize(p)
                    .unwrap_or_else(|_| {
                        if p.is_absolute() {
                            p.clone()
                        } else {
                            std::env::current_dir()
                                .map(|cwd| cwd.join(p))
                                .unwrap_or_else(|_| p.clone())
                        }
                    })
                    .display()
                    .to_string()
            }
        })
    }

    /// Set the filename for the slot with `buffer_id == id`.
    pub(crate) fn nvim_set_buffer_name(&mut self, id: u64, name: &str) {
        if let Some(slot) = self.slots.iter_mut().find(|s| s.buffer_id == id) {
            slot.filename = if name.is_empty() {
                None
            } else {
                Some(PathBuf::from(name))
            };
        }
    }

    /// Shared reference to the slot with the given buffer id — used by
    /// nvim-api handlers that need buffer content for a buffer id that may
    /// not be the currently-focused one (#151 Stage 2b: was "the slot-level
    /// editor"; slots no longer carry one, so callers now read/write
    /// through the `BufferSlot` document-handle accessors directly).
    pub(crate) fn nvim_slot(&self, id: u64) -> Option<&super::BufferSlot> {
        self.slots.iter().find(|s| s.buffer_id == id)
    }

    /// Mutable reference to the slot with the given buffer id.
    pub(crate) fn nvim_slot_mut(&mut self, id: u64) -> Option<&mut super::BufferSlot> {
        self.slots.iter_mut().find(|s| s.buffer_id == id)
    }

    /// `dirty_gen` of the slot's buffer the last time it was
    /// didChange-notified to the LSP (`None` = never sent). Test-only
    /// accessor (audit R2, fix 1) — `slots` is private to the `app` module,
    /// so nvim-api's own test module needs this to verify a non-focused
    /// slot got synced.
    #[cfg(test)]
    pub(crate) fn nvim_slot_last_lsp_dirty_gen(&self, id: u64) -> Option<u64> {
        self.slots
            .iter()
            .find(|s| s.buffer_id == id)
            .and_then(|s| s.last_lsp_dirty_gen)
    }

    /// First buffer id whose stored filename string contains `name` as a
    /// substring, or `None` if no slot matches. Used by `nvim_call_function`
    /// `bufnr("name")` semantics.
    pub(crate) fn nvim_buffer_id_for_name(&self, name: &str) -> Option<u64> {
        self.slots.iter().find_map(|s| {
            let fname = s.filename.as_ref()?.to_string_lossy();
            if fname.contains(name) {
                Some(s.buffer_id)
            } else {
                None
            }
        })
    }

    /// Allocate a fresh empty unnamed buffer slot (nvim_create_buf).
    /// The slot is appended but NOT switched to; returns the new buffer id.
    pub(crate) fn nvim_create_buffer(&mut self) -> u64 {
        use super::{BufferFeatures, BufferSlot, DiskState};
        use hjkl_buffer::View;
        use std::time::Instant;

        let buffer_id = self.next_buffer_id;
        self.next_buffer_id += 1;
        // No editor to build here (#151 Stage 2b) — the register/marks/
        // search/change-bank Arcs and the viewport are wired onto the
        // window editor whenever this slot is first shown in a window
        // (`reconcile_window_editors` / `make_view_editor`), not here.
        let mut slot = BufferSlot {
            buffer_id,
            is_explorer: false,
            features: BufferFeatures::default(),
            view: View::new(),
            settings: Settings::default(),
            filename: None,
            dirty: false,
            is_new_file: false,
            is_untracked: false,
            diag_signs: Vec::new(),
            diag_signs_lsp: Vec::new(),
            lsp_diags: Vec::new(),
            last_lsp_dirty_gen: None,
            git_signs: Vec::new(),
            last_git_dirty_gen: None,
            last_git_refresh_at: Instant::now(),
            blame: Vec::new(),
            last_blame_dirty_gen: None,
            last_blame_refresh_at: Instant::now(),
            saved_hash: 0,
            saved_len: 0,
            signature_cache: None,
            disk_mtime: None,
            disk_len: None,
            disk_state: DiskState::Synced,
            swap_path: None,
            last_swap_dirty_gen: None,
            last_fold_dirty_gen: None,
            git_repo_present: None,
            commit_ctx: None,
        };
        slot.snapshot_saved();
        self.slots.push(slot);
        buffer_id
    }

    /// Allocate a fresh `BufferId` and load `path` into a new slot.
    /// Returns the index of the newly pushed slot (does NOT switch).
    pub(crate) fn open_new_slot(&mut self, path: PathBuf) -> Result<usize, String> {
        let buffer_id = self.next_buffer_id;
        self.next_buffer_id += 1;
        let slot = super::build_slot(&mut self.syntax, buffer_id, Some(path), &self.config)?;
        self.slots.push(slot);
        let idx = self.slots.len() - 1;
        self.lsp_attach_buffer(idx);
        // Event-driven autoreload: watch this file's directory (#242).
        self.fs_watch_sync();
        Ok(idx)
    }

    /// Dispatch a buffer-navigation [`crate::keymap_actions::AppAction`].
    ///
    /// Handles variants:
    ///   - BufferNext / BufferPrev / BufferAlt
    ///   - BufferCycleH / BufferCycleL (predicate-gated: fall back to viewport motion)
    ///   - Tabnext / Tabprev (delegated through dispatch_ex)
    pub(crate) fn dispatch_buffer_action(
        &mut self,
        action: crate::keymap_actions::AppAction,
        count: usize,
    ) {
        use crate::keymap_actions::AppAction;
        match action {
            AppAction::Tabnext => {
                // `{count}gt` is an ABSOLUTE jump to tab page {count} (vim
                // 1-indexes tab pages; `:h gt`), while bare `gt` (no explicit
                // count) is RELATIVE — next tab, wrapping from last to first.
                // `count` here is already defaulted to 1 by `dispatch_action`,
                // so explicit-`1gt` and bare-`gt` are indistinguishable by
                // value alone; `g_chord_explicit_count` (captured at
                // `BeginPendingAfterG` time, before the default was applied)
                // disambiguates them.
                if self.g_chord_explicit_count {
                    let target = count.saturating_sub(1).min(self.tabs.len() - 1);
                    self.switch_tab(target);
                    let n = self.active_tab + 1;
                    let m = self.tabs.len();
                    self.bus.info(format!("tab {n}/{m}"));
                } else {
                    self.dispatch_ex("tabnext");
                }
            }
            AppAction::Tabprev => {
                for _ in 0..count {
                    self.dispatch_ex("tabprev");
                }
            }
            AppAction::BufferNext => self.buffer_next(),
            AppAction::BufferPrev => self.buffer_prev(),
            AppAction::BufferAlt => self.buffer_alt(),
            AppAction::BufferCycleH => {
                if self.slots.len() > 1 {
                    self.buffer_prev();
                } else {
                    // Single slot: fall back to viewport-top motion.
                    let n = self.pending_count.take_or(1) as usize;
                    self.active_editor_mut()
                        .apply_motion(hjkl_vim::MotionKind::ViewportTop, n);
                }
            }
            AppAction::BufferCycleL => {
                if self.slots.len() > 1 {
                    self.buffer_next();
                } else {
                    // Single slot: fall back to viewport-bottom motion.
                    let n = self.pending_count.take_or(1) as usize;
                    self.active_editor_mut()
                        .apply_motion(hjkl_vim::MotionKind::ViewportBottom, n);
                }
            }
            _ => {}
        }
    }

    /// Handle the result of `Editor::try_goto_mark_line` /
    /// `Editor::try_goto_mark_char`. Switches to the correct slot for cross-
    /// buffer marks, positions the cursor, and syncs. Emits an error toast
    /// when the referenced buffer has been closed.
    pub(crate) fn apply_mark_jump(&mut self, jump: MarkJump, linewise: bool) {
        match jump {
            MarkJump::SameBuffer => {
                self.sync_after_engine_mutation();
            }
            MarkJump::CrossBuffer {
                buffer_id,
                row,
                col,
            } => {
                let slot_idx = self.slots.iter().position(|s| s.buffer_id == buffer_id);
                match slot_idx {
                    Some(idx) => {
                        self.switch_to(idx);
                        if linewise {
                            self.active_editor_mut().jump_cursor(row, 0);
                            self.active_editor_mut()
                                .apply_motion(hjkl_vim::MotionKind::FirstNonBlank, 1);
                        } else {
                            self.active_editor_mut().jump_cursor(row, col);
                        }
                        self.sync_after_engine_mutation();
                    }
                    None => {
                        self.bus.error(format!(
                            "E474: mark references a closed buffer (id {buffer_id})"
                        ));
                    }
                }
            }
            MarkJump::Unset => { /* silent no-op — mark not set */ }
        }
    }
}
