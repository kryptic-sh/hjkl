//! App-level git hunk actions (#115): stage / revert / preview the hunk under
//! the cursor.
//!
//! These build on the host-agnostic primitives in `hjkl_app::git`
//! (`hunks_for_bytes`, `hunk_at`, `stage_hunk`, `revert_hunk`). The buffer
//! bytes handed to the diff mirror the git-signs worker exactly (rope chunks +
//! a single trailing newline for non-empty content) so the hunk geometry
//! matches the gutter signs the user sees.
//!
//! Stage and revert mutate the index / worktree via `git apply`, which operates
//! on the file *on disk*. They therefore require the buffer to be saved first —
//! a dirty buffer would mean the on-disk content differs from the hunk that was
//! computed against in-memory bytes, and the patch could fail to apply or stage
//! the wrong thing. We refuse with a clear message rather than guess.

use hjkl_app::git;

use super::App;

impl App {
    /// Materialize the active buffer's bytes the same way the git-signs worker
    /// does: rope chunks concatenated, with a single trailing newline for
    /// non-empty content (matching what `:w` writes).
    fn active_buffer_git_bytes(&self) -> Vec<u8> {
        let rope = self.active().editor.buffer().rope();
        let mut bytes: Vec<u8> = Vec::with_capacity(rope.len_bytes() + 1);
        for chunk in rope.chunks() {
            bytes.extend_from_slice(chunk.as_bytes());
        }
        if !bytes.is_empty() {
            bytes.push(b'\n');
        }
        bytes
    }

    /// Compute the hunk under the cursor for the active buffer, if any.
    /// Returns `(path, hunk)` so callers can act without re-resolving.
    fn hunk_under_cursor(&self) -> Option<(std::path::PathBuf, git::Hunk)> {
        let path = self.active().filename.clone()?;
        let bytes = self.active_buffer_git_bytes();
        let hunks = git::hunks_for_bytes(&path, &bytes);
        let row = self.active().editor.cursor().0;
        git::hunk_at(&hunks, row).cloned().map(|h| (path, h))
    }

    /// `:GitDiff` / gutter "Show Diff" — preview the hunk under the cursor in an
    /// info popup. Read-only; works on dirty buffers (uses in-memory bytes).
    pub(crate) fn git_show_hunk_diff(&mut self) {
        match self.hunk_under_cursor() {
            Some((_, hunk)) => {
                let body = format!("{}\n{}", hunk.header, hunk.body);
                self.info_popup = Some(hjkl_info_popup::InfoPopup::new("git hunk", body));
            }
            None => {
                if self.active().filename.is_none() {
                    self.bus.warn("no file for this buffer");
                } else {
                    self.bus.warn("no git hunk under cursor");
                }
            }
        }
    }

    /// `:GitStage` / gutter "Stage Hunk" — stage the hunk under the cursor into
    /// the index. Requires a saved buffer (the patch applies against disk).
    pub(crate) fn git_stage_hunk_at_cursor(&mut self) {
        if self.active().dirty {
            self.bus.warn("save the buffer before staging (:w)");
            return;
        }
        let Some((path, hunk)) = self.hunk_under_cursor() else {
            self.bus.warn("no git hunk under cursor");
            return;
        };
        match git::stage_hunk(&path, &hunk) {
            Ok(()) => {
                self.bus.info("hunk staged");
                self.refresh_git_signs_force();
            }
            Err(e) => {
                self.bus.error(format!("git stage: {e}"));
            }
        }
    }

    /// `:GitRevert` / gutter "Revert Hunk" — discard the hunk under the cursor,
    /// restoring the HEAD/index version on disk, then reload the buffer so the
    /// editor reflects the reverted file. Requires a saved buffer.
    pub(crate) fn git_revert_hunk_at_cursor(&mut self) {
        if self.active().dirty {
            self.bus.warn("save the buffer before reverting (:w)");
            return;
        }
        let Some((path, hunk)) = self.hunk_under_cursor() else {
            self.bus.warn("no git hunk under cursor");
            return;
        };
        match git::revert_hunk(&path, &hunk) {
            Ok(()) => {
                // The worktree file changed underneath us — reload it so the
                // buffer matches disk. `reload_current(true)` force-reloads even
                // though the buffer is clean (it always is here — we required it).
                self.reload_current(true);
                self.bus.info("hunk reverted");
                self.refresh_git_signs_force();
            }
            Err(e) => {
                self.bus.error(format!("git revert: {e}"));
            }
        }
    }
}
