use hjkl_buffer::Buffer;
use hjkl_engine::{Editor, Host, Options};
use std::path::PathBuf;

use super::{App, DiskState, STATUS_LINE_HEIGHT};
use crate::host::TuiHost;

impl App {
    /// Switch the focused window to display slot `idx` and refresh its
    /// viewport spans.  Records the previous slot index in `prev_active`
    /// for alt-buffer (`<C-^>` / `:b#`).
    pub(crate) fn switch_to(&mut self, idx: usize) {
        let current_slot = self.focused_slot_idx();
        if idx != current_slot {
            self.prev_active = Some(current_slot);
            // Carry the register bank across slots so vim's yank/paste
            // works cross-buffer (`yy` in slot 0, `p` in slot 1).
            let regs = self.slots[current_slot].editor.registers().clone();
            *self.slots[idx].editor.registers_mut() = regs;
        }
        // Point the focused window at the new slot.
        self.windows[self.focused_window]
            .as_mut()
            .expect("focused_window open")
            .slot = idx;
        if let Ok(size) = crossterm::terminal::size() {
            let vp = self.active_mut().editor.host_mut().viewport_mut();
            vp.width = size.0;
            vp.height = size.1.saturating_sub(STATUS_LINE_HEIGHT);
        }
        let buffer_id = self.active().buffer_id;
        let (vp_top, vp_height) = {
            let vp = self.active().editor.host().viewport();
            (vp.top_row, vp.height as usize)
        };
        if let Some(out) =
            self.syntax
                .preview_render(buffer_id, self.active().editor.buffer(), vp_top, vp_height)
        {
            self.active_mut()
                .editor
                .install_ratatui_syntax_spans(out.spans);
        }
        self.active_mut().last_recompute_key = None;
        self.recompute_and_install();
        self.refresh_git_signs_force();
    }

    /// `:bnext` — cycle active forward. No-op when only one slot.
    pub(crate) fn buffer_next(&mut self) {
        if !self.require_multi_buffer() {
            return;
        }
        let next = (self.focused_slot_idx() + 1) % self.slots.len();
        self.switch_to(next);
    }

    /// `:bprev` — cycle active backward. No-op when only one slot.
    pub(crate) fn buffer_prev(&mut self) {
        if !self.require_multi_buffer() {
            return;
        }
        let prev = (self.focused_slot_idx() + self.slots.len() - 1) % self.slots.len();
        self.switch_to(prev);
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
                self.status_message = Some("no alternate buffer".into());
            }
        }
    }

    /// `:bdelete[!]` — close the active slot. With more than one slot
    /// open the slot is removed; on the last slot the buffer is reset
    /// to an empty unnamed scratch buffer (vim parity for `:bd` on the
    /// only buffer leaving an empty editor instead of quitting).
    pub(crate) fn buffer_delete(&mut self, force: bool) {
        if !force && self.active().dirty {
            self.status_message =
                Some("E89: No write since last change (add ! to override)".into());
            return;
        }
        let active_slot = self.focused_slot_idx();
        if self.slots.len() == 1 {
            let old_id = self.active().buffer_id;
            self.syntax.forget(old_id);
            let new_id = self.next_buffer_id;
            self.next_buffer_id += 1;
            let host = TuiHost::new();
            let mut editor = Editor::new(Buffer::new(), host, Options::default());
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
            slot.last_recompute_key = None;
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
            self.status_message = Some("buffer closed (replaced with [No Name])".into());
            return;
        }
        let removed = self.slots.remove(active_slot);
        self.syntax.forget(removed.buffer_id);
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
        self.status_message = Some(format!("buffer closed: \"{name}\""));
    }

    /// Returns `true` when multiple slots are open; otherwise sets the
    /// "only one buffer open" status message and returns `false`.
    pub(crate) fn require_multi_buffer(&mut self) -> bool {
        if self.slots.len() <= 1 {
            self.status_message = Some("only one buffer open".into());
            return false;
        }
        true
    }

    /// `:ls` / `:buffers` — render the buffer list to a single status
    /// line. Marks: `%` active, `+` modified.
    pub(crate) fn list_buffers(&self) -> String {
        let active_slot = self.focused_slot_idx();
        let mut parts = Vec::with_capacity(self.slots.len());
        for (i, slot) in self.slots.iter().enumerate() {
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
        Ok(self.slots.len() - 1)
    }
}
