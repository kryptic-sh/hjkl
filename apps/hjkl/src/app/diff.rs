//! Diff mode (#208) — Phase 1: `:DiffOrig`.
//!
//! Shows a unified diff of the current buffer (in-memory, possibly unsaved)
//! against its on-disk version, in a read-only `filetype=diff` scratch buffer
//! opened in a vertical split. Closes with `:q` like any split.
//!
//! The diff itself is computed by [`hjkl_app::git::unified_diff`] (git2
//! `Patch::from_buffers`, no repository required), so this is reusable for the
//! dirty-buffer reload-diff prompt (#241).

use super::App;

impl App {
    /// `:DiffOrig` — open a read-only split with the unified diff of the active
    /// buffer vs its on-disk file.
    pub(crate) fn diff_orig(&mut self) {
        use crate::app::STATUS_LINE_HEIGHT;
        use crate::app::window::{LayoutTree, SplitDir, Window};
        use crate::host::TuiHost;
        use hjkl_buffer::Buffer;
        use hjkl_engine::{BufferEdit, Editor, Host, Options};

        // Must be a real on-disk file (not a scratch / explorer buffer).
        let Some(path) = self.active().filename.clone() else {
            self.bus.error("E32: No file name");
            return;
        };

        // On-disk bytes (the "old" side). A missing file diffs against empty.
        let disk = std::fs::read(&path).unwrap_or_default();

        // Buffer bytes (the "new" side), produced exactly as `:w` would write
        // them (trailing newline) so an unmodified buffer yields an empty diff.
        let buf_bytes: Vec<u8> = {
            let rope = self.active_editor().buffer().rope();
            let mut b = Vec::with_capacity(rope.len_bytes() + 1);
            for chunk in rope.chunks() {
                b.extend_from_slice(chunk.as_bytes());
            }
            if !b.is_empty() {
                b.push(b'\n');
            }
            b
        };

        let label = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".to_string());
        let diff = hjkl_app::git::unified_diff(
            &disk,
            &buf_bytes,
            &format!("a/{label}"),
            &format!("b/{label}"),
        )
        .unwrap_or_default();

        let body = if diff.trim().is_empty() {
            format!("# :DiffOrig — no differences between buffer and {label}\n")
        } else {
            diff
        };

        // Build a read-only scratch slot holding the diff (mirrors `do_vnew`).
        let focused = self.focused_window();
        // Inherit the focused window's scroll from its own editor (#151 Phase D).
        let (top_row, top_col) = self.window_scroll(focused);

        let new_slot_idx = {
            let buffer_id = self.next_buffer_id;
            self.next_buffer_id += 1;
            let host = TuiHost::new();
            let mut editor = Editor::new(Buffer::new(), host, Options::default());
            if let Ok(size) = crossterm::terminal::size() {
                let vp = editor.host_mut().viewport_mut();
                vp.width = size.0;
                vp.height = size.1.saturating_sub(STATUS_LINE_HEIGHT);
            }
            editor.set_current_buffer_id(buffer_id);
            BufferEdit::replace_all(editor.buffer_mut(), &body);
            editor.set_filetype("diff");
            // Read-only view: block edits/insert-mode entry.
            editor.settings_mut().modifiable = false;
            let _ = editor.take_content_edits();
            let _ = editor.take_content_reset();
            let mut slot = super::BufferSlot {
                buffer_id,
                is_explorer: false,
                features: super::BufferFeatures::default(),
                editor,
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
                last_git_refresh_at: std::time::Instant::now(),
                blame: Vec::new(),
                last_blame_dirty_gen: None,
                last_blame_refresh_at: std::time::Instant::now(),
                saved_hash: 0,
                saved_len: 0,
                signature_cache: None,
                disk_mtime: None,
                disk_len: None,
                disk_state: super::DiskState::Synced,
                swap_path: None,
                last_swap_dirty_gen: None,
                last_fold_dirty_gen: None,
                git_repo_present: None,
                commit_ctx: None,
            };
            // Mark clean so `:q` doesn't trip the unsaved-changes guard and no
            // swap is written for this transient view.
            slot.snapshot_saved();
            self.slots.push(slot);
            self.slots.len() - 1
        };

        let new_win_id = self.next_window_id;
        self.next_window_id += 1;
        self.windows.push(Some(Window::new(new_slot_idx)));
        self.reconcile_window_editors();
        self.seed_window_editor(new_win_id, 0, 0, top_row, top_col);
        // New diff window on the left (a), original on the right (b).
        self.layout_mut()
            .replace_leaf(focused, move |id| LayoutTree::Split {
                dir: SplitDir::Vertical,
                ratio: 0.5,
                a: Box::new(LayoutTree::Leaf(new_win_id)),
                b: Box::new(LayoutTree::Leaf(id)),
                last_rect: None,
            });
        self.set_focused_window(new_win_id);
    }
}
