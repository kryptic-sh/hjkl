use hjkl_buffer::Buffer;
use hjkl_engine::{Editor, Host, MarkJump, Options};
use std::path::PathBuf;

use super::{App, DiskState, STATUS_LINE_HEIGHT};
use crate::host::TuiHost;

impl App {
    /// Switch the focused window to display slot `idx` and refresh its
    /// viewport spans.  Records the previous slot index in `prev_active`
    /// for alt-buffer (`<C-^>` / `:b#`).
    pub(crate) fn switch_to(&mut self, idx: usize) {
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
            // Carry the register bank across slots so vim's yank/paste
            // works cross-buffer (`yy` in slot 0, `p` in slot 1).
            let regs = self.slots[current_slot].editor.registers().clone();
            *self.slots[idx].editor.registers_mut() = regs;
        }
        // Update the synthetic `%` register with the new slot's filename so
        // `"%p`, `<C-r>%`, and `:echo @%` reflect the correct path.
        let fname = self.slots[idx]
            .filename
            .as_deref()
            .map(|p| p.to_string_lossy().into_owned());
        self.slots[idx].editor.registers_mut().set_filename(fname);
        // Keep the engine's current_buffer_id in sync so `mA`–`mZ` global
        // marks tag new marks with the correct slot id.
        let new_bid = self.slots[idx].buffer_id;
        self.slots[idx].editor.set_current_buffer_id(new_bid);
        // Point the focused window at the new slot.
        let fw = self.focused_window();
        self.windows[fw].as_mut().expect("focused_window open").slot = idx;
        if let Ok(size) = crossterm::terminal::size() {
            let vp = self.active_mut().editor.host_mut().viewport_mut();
            vp.width = size.0;
            vp.height = size.1.saturating_sub(STATUS_LINE_HEIGHT);
        }
        // recompute_and_install runs render_viewport sync (post fully-sync
        // refactor) — no need for a preview_render warm-up paint.
        self.recompute_and_install();
        self.refresh_git_signs_force();
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
            let old_id = self.active().buffer_id;
            self.syntax.forget(old_id);
            let new_id = self.next_buffer_id;
            self.next_buffer_id += 1;
            let host = TuiHost::new();
            let mut editor = Editor::new(Buffer::new(), host, Options::default());
            editor.set_current_buffer_id(new_id);
            if let Ok(size) = crossterm::terminal::size() {
                let vp = editor.host_mut().viewport_mut();
                vp.width = size.0;
                vp.height = size.1.saturating_sub(STATUS_LINE_HEIGHT);
            }
            let _ = editor.take_content_edits();
            let _ = editor.take_content_reset();
            let slot = &mut self.slots[0];
            slot.buffer_id = new_id;
            slot.editor = editor;
            slot.filename = None;
            slot.dirty = false;
            slot.is_new_file = false;
            slot.is_untracked = false;
            slot.diag_signs.clear();
            slot.git_signs.clear();
            slot.last_git_dirty_gen = None;
            slot.saved_hash = 0;
            slot.saved_len = 0;
            slot.disk_mtime = None;
            slot.disk_len = None;
            slot.disk_state = DiskState::Synced;
            slot.snapshot_saved();
            // Keep all windows pointing at slot 0 (the only one).
            for win in self.windows.iter_mut().flatten() {
                win.slot = 0;
            }
            self.bus.info("buffer closed (replaced with [No Name])");
            return;
        }
        self.lsp_detach_buffer(active_slot);
        let mut removed = self.slots.remove(active_slot);
        self.syntax.forget(removed.buffer_id);
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
        let name = removed
            .filename
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "[No Name]".into());
        self.bus.info(format!("buffer closed: \"{name}\""));
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
            // Explicitly wipe marks and jumplists before discarding the editor
            // so no state leaks into the replacement scratch buffer.
            {
                let editor = &mut self.slots[0].editor;
                let mark_chars: Vec<char> = editor.marks().map(|(c, _)| c).collect();
                for c in mark_chars {
                    editor.clear_mark(c);
                }
                editor.jump_back_list_mut().clear();
                editor.jump_fwd_list_mut().clear();
            }
            // Also clear LSP diagnostics for the wiped buffer.
            {
                let slot = &mut self.slots[0];
                slot.lsp_diags.clear();
                slot.diag_signs_lsp.clear();
            }
            self.lsp_detach_buffer(active_slot);
            let old_id = self.active().buffer_id;
            self.syntax.forget(old_id);
            let new_id = self.next_buffer_id;
            self.next_buffer_id += 1;
            let host = TuiHost::new();
            let mut editor = Editor::new(Buffer::new(), host, Options::default());
            editor.set_current_buffer_id(new_id);
            if let Ok(size) = crossterm::terminal::size() {
                let vp = editor.host_mut().viewport_mut();
                vp.width = size.0;
                vp.height = size.1.saturating_sub(STATUS_LINE_HEIGHT);
            }
            let _ = editor.take_content_edits();
            let _ = editor.take_content_reset();
            let slot = &mut self.slots[0];
            slot.buffer_id = new_id;
            slot.editor = editor;
            slot.filename = None;
            slot.dirty = false;
            slot.is_new_file = false;
            slot.is_untracked = false;
            slot.diag_signs.clear();
            slot.git_signs.clear();
            slot.last_git_dirty_gen = None;
            slot.saved_hash = 0;
            slot.saved_len = 0;
            slot.disk_mtime = None;
            slot.disk_len = None;
            slot.disk_state = DiskState::Synced;
            slot.snapshot_saved();
            // Keep all windows pointing at slot 0 (the only one).
            for win in self.windows.iter_mut().flatten() {
                win.slot = 0;
            }
            self.bus.info("buffer wiped (replaced with [No Name])");
            return;
        }
        // Multi-slot: removing the slot entirely discards the editor (and all
        // its marks/jumps) — same mechanics as buffer_delete.
        self.lsp_detach_buffer(active_slot);
        let mut removed = self.slots.remove(active_slot);
        self.syntax.forget(removed.buffer_id);
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

    /// Allocate a fresh `BufferId` and load `path` into a new slot.
    /// Returns the index of the newly pushed slot (does NOT switch).
    pub(crate) fn open_new_slot(&mut self, path: PathBuf) -> Result<usize, String> {
        let buffer_id = self.next_buffer_id;
        self.next_buffer_id += 1;
        let slot = super::build_slot(&mut self.syntax, buffer_id, Some(path), &self.config)?;
        self.slots.push(slot);
        let idx = self.slots.len() - 1;
        self.lsp_attach_buffer(idx);
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
                for _ in 0..count {
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
                    self.active_mut()
                        .editor
                        .apply_motion(hjkl_vim::MotionKind::ViewportTop, n);
                }
            }
            AppAction::BufferCycleL => {
                if self.slots.len() > 1 {
                    self.buffer_next();
                } else {
                    // Single slot: fall back to viewport-bottom motion.
                    let n = self.pending_count.take_or(1) as usize;
                    self.active_mut()
                        .editor
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
                            self.active_mut().editor.jump_cursor(row, 0);
                            self.active_mut()
                                .editor
                                .apply_motion(hjkl_vim::MotionKind::FirstNonBlank, 1);
                        } else {
                            self.active_mut().editor.jump_cursor(row, col);
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
