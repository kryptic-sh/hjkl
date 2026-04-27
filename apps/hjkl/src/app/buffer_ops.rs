use hjkl_buffer::Buffer;
use hjkl_engine::{BufferEdit, Editor, Host, Options};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::{App, BufferSlot, STATUS_LINE_HEIGHT};
use crate::host::TuiHost;

impl App {
    /// Switch active to `idx` and refresh its viewport spans.
    /// Records the previous active index in `prev_active` for alt-buffer.
    pub(crate) fn switch_to(&mut self, idx: usize) {
        if idx != self.active {
            self.prev_active = Some(self.active);
        }
        self.active = idx;
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
        let next = (self.active + 1) % self.slots.len();
        self.switch_to(next);
    }

    /// `:bprev` — cycle active backward. No-op when only one slot.
    pub(crate) fn buffer_prev(&mut self) {
        if !self.require_multi_buffer() {
            return;
        }
        let prev = (self.active + self.slots.len() - 1) % self.slots.len();
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
            slot.snapshot_saved();
            self.status_message = Some("buffer closed (replaced with [No Name])".into());
            return;
        }
        let removed = self.slots.remove(self.active);
        self.syntax.forget(removed.buffer_id);
        if self.active >= self.slots.len() {
            self.active = self.slots.len() - 1;
        }
        let target = self.active;
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
        let mut parts = Vec::with_capacity(self.slots.len());
        for (i, slot) in self.slots.iter().enumerate() {
            let active = if i == self.active { '%' } else { ' ' };
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
        let mut buffer = Buffer::new();
        let mut is_new_file = false;
        match std::fs::read_to_string(&path) {
            Ok(content) => {
                let content = content.strip_suffix('\n').unwrap_or(&content);
                BufferEdit::replace_all(&mut buffer, content);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                is_new_file = true;
            }
            Err(e) => return Err(format!("E484: Can't open file {}: {e}", path.display())),
        }
        let host = TuiHost::new();
        let mut editor = Editor::new(buffer, host, Options::default());
        if let Ok(size) = crossterm::terminal::size() {
            let vp = editor.host_mut().viewport_mut();
            vp.width = size.0;
            vp.height = size.1.saturating_sub(STATUS_LINE_HEIGHT);
        }
        let buffer_id = self.next_buffer_id;
        self.next_buffer_id += 1;
        self.syntax.set_language_for_path(buffer_id, &path);
        let (vp_top, vp_height) = {
            let vp = editor.host().viewport();
            (vp.top_row, vp.height as usize)
        };
        if let Some(out) = self
            .syntax
            .preview_render(buffer_id, editor.buffer(), vp_top, vp_height)
        {
            editor.install_ratatui_syntax_spans(out.spans);
        }
        self.syntax
            .submit_render(buffer_id, editor.buffer(), vp_top, vp_height);
        let initial_dg = editor.buffer().dirty_gen();
        let (key, signs) = if let Some(out) = self
            .syntax
            .wait_for_initial_result(Duration::from_millis(150))
        {
            let k = out.key;
            editor.install_ratatui_syntax_spans(out.spans);
            (Some(k), out.signs)
        } else {
            (Some((initial_dg, vp_top, vp_height)), Vec::new())
        };
        let _ = editor.take_content_edits();
        let _ = editor.take_content_reset();
        let mut slot = BufferSlot {
            buffer_id,
            editor,
            filename: Some(path),
            dirty: false,
            is_new_file,
            is_untracked: false,
            diag_signs: signs,
            git_signs: Vec::new(),
            last_git_dirty_gen: None,
            last_git_refresh_at: Instant::now(),
            last_recompute_at: Instant::now() - Duration::from_secs(1),
            last_recompute_key: key,
            saved_hash: 0,
            saved_len: 0,
        };
        slot.snapshot_saved();
        self.slots.push(slot);
        Ok(self.slots.len() - 1)
    }
}
