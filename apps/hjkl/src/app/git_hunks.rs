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

    /// Compute the **unstaged** hunk (index↔buffer) covering `row`, if any.
    /// Returns `(path, hunk)` so callers can act without re-resolving.
    fn unstaged_hunk_at_row(&self, row: usize) -> Option<(std::path::PathBuf, git::Hunk)> {
        let path = self.active().filename.clone()?;
        let bytes = self.active_buffer_git_bytes();
        let hunks = git::unstaged_hunks_for_bytes(&path, &bytes);
        git::hunk_at(&hunks, row).cloned().map(|h| (path, h))
    }

    /// Compute the **staged** hunk (HEAD↔index) covering `row`, if any.
    fn staged_hunk_at_row(&self, row: usize) -> Option<(std::path::PathBuf, git::Hunk)> {
        let path = self.active().filename.clone()?;
        let hunks = git::staged_hunks_for_path(&path);
        git::hunk_at(&hunks, row).cloned().map(|h| (path, h))
    }

    /// The unstaged hunk under the cursor, if any.
    fn unstaged_hunk_under_cursor(&self) -> Option<(std::path::PathBuf, git::Hunk)> {
        self.unstaged_hunk_at_row(self.active().editor.cursor().0)
    }

    /// The staged hunk under the cursor, if any.
    fn staged_hunk_under_cursor(&self) -> Option<(std::path::PathBuf, git::Hunk)> {
        self.staged_hunk_at_row(self.active().editor.cursor().0)
    }

    /// Classify the git change covering `row` for the active buffer. Unstaged
    /// (index↔buffer) takes priority over staged (HEAD↔index) so a row with
    /// fresh edits reads as still-unstaged. Drives the adaptive gutter menu.
    pub(crate) fn git_hunk_kind_at_row(&self, row: usize) -> crate::menu::GitHunkKind {
        if self.unstaged_hunk_at_row(row).is_some() {
            crate::menu::GitHunkKind::Unstaged
        } else if self.staged_hunk_at_row(row).is_some() {
            crate::menu::GitHunkKind::Staged
        } else {
            crate::menu::GitHunkKind::None
        }
    }

    /// Show `hunk` in a read-only info popup, titled by staged state.
    fn show_hunk_popup(&mut self, hunk: &git::Hunk, staged: bool) {
        let title = if staged {
            "git hunk (staged)"
        } else {
            "git hunk"
        };
        let body = format!("{}\n{}", hunk.header, hunk.body);
        self.info_popup = Some(hjkl_info_popup::InfoPopup::new(title, body));
    }

    /// `:GitDiff` / gutter "Show Diff" — preview the hunk under the cursor in an
    /// info popup. Read-only; works on dirty buffers. Prefers the unstaged hunk
    /// (what the user is editing), falling back to the staged hunk.
    pub(crate) fn git_show_hunk_diff(&mut self) {
        if let Some((_, hunk)) = self.unstaged_hunk_under_cursor() {
            self.show_hunk_popup(&hunk, false);
        } else if let Some((_, hunk)) = self.staged_hunk_under_cursor() {
            self.show_hunk_popup(&hunk, true);
        } else if self.active().filename.is_none() {
            self.bus.warn("no file for this buffer");
        } else {
            self.bus.warn("no git hunk under cursor");
        }
    }

    /// P10 gutter left-click on a git sign — preview the hunk covering `row`
    /// without moving the cursor (gutter clicks never move the cursor; see
    /// `gutter_click_no_cursor_move`). Silent no-op when no hunk covers the row.
    pub(crate) fn git_show_hunk_diff_at_row(&mut self, row: usize) {
        if let Some((_, hunk)) = self.unstaged_hunk_at_row(row) {
            self.show_hunk_popup(&hunk, false);
        } else if let Some((_, hunk)) = self.staged_hunk_at_row(row) {
            self.show_hunk_popup(&hunk, true);
        }
    }

    /// `:GitBlame` / `<leader>gm` — toggle the left-side git blame column for
    /// the active buffer. When turned on, also kicks a blame refresh so the
    /// column populates immediately; auto-hides the inline EOL blame.
    pub(crate) fn toggle_blame_column(&mut self) {
        // BLAME is an FSM-owned read-only view (`Editor::view_mode`): the engine
        // tracks it, blocks edits while active, and auto-exits on any mode
        // change (keyboard, mouse drag, Esc). The host only flips it on/off and
        // loads the git data — it never has to police read-only or transitions.
        let on = !self.active().editor.is_blame();
        if on {
            self.active_mut().editor.enter_blame();
            // Refresh the blame data (normally gated on `blame_inline`).
            self.refresh_blame_force();
        } else {
            self.active_mut().editor.exit_blame();
        }
        self.bus.info(if on {
            "BLAME mode (read-only) — gb or Esc to exit"
        } else {
            "BLAME mode off"
        });
    }

    /// Reset the inline git-blame idle debounce whenever the focused cursor
    /// moves — from *any* source (keyboard motion, mouse click/drag, macro
    /// replay, LSP jump, …). Called once per event-loop tick before drawing.
    /// Debouncing on the cursor position (rather than stamping each input
    /// path) keeps the delay source- and mode-agnostic with no per-path
    /// bookkeeping to maintain.
    pub(crate) fn note_blame_cursor_motion(&mut self) {
        let cur = (self.focused_slot_idx(), self.active().editor.cursor());
        if self.blame_prev_cursor != Some(cur) {
            self.blame_prev_cursor = Some(cur);
            self.blame_cursor_moved_at = std::time::Instant::now();
        }
    }

    /// Mouse-hover over the blame column at `doc_row` — show the full commit
    /// message for that line's commit in the markdown hover popup (the same
    /// widget LSP hovers use), anchored at `cell`. No-op when the row has no
    /// blame attribution; uncommitted rows show a short placeholder.
    pub(crate) fn show_blame_commit_hover(&mut self, doc_row: usize, cell: (u16, u16)) {
        let info = match self.active().blame.get(doc_row) {
            Some(Some(i)) => i.clone(),
            _ => return,
        };
        let content = if info.is_uncommitted {
            "**Not Committed Yet**".to_string()
        } else {
            let header = format!(
                "**{}**  {} · {}",
                info.commit,
                info.author,
                format_blame_date(info.time_unix)
            );
            // Full commit message (resolved from the repo); fall back to the
            // header alone when it can't be fetched.
            let msg = self
                .active()
                .filename
                .clone()
                .and_then(|p| git::commit_message(&p, &info.commit));
            match msg {
                Some(m) => format!("{header}\n\n{m}"),
                None => header,
            }
        };
        self.hover_popup = Some(crate::hover_popup::new(content, cell));
    }

    /// `:GitStage` / gutter "Stage Hunk" — stage the unstaged hunk under the
    /// cursor into the index. Requires a saved buffer (the patch applies against
    /// disk).
    pub(crate) fn git_stage_hunk_at_cursor(&mut self) {
        if self.active().dirty {
            self.bus.warn("save the buffer before staging (:w)");
            return;
        }
        let Some((path, hunk)) = self.unstaged_hunk_under_cursor() else {
            self.bus.warn("no unstaged hunk under cursor");
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

    /// `:GitUnstage` / gutter "Unstage Hunk" — move the staged hunk under the
    /// cursor back out of the index toward HEAD. Touches only the index, so no
    /// buffer save is required.
    pub(crate) fn git_unstage_hunk_at_cursor(&mut self) {
        let Some((path, hunk)) = self.staged_hunk_under_cursor() else {
            self.bus.warn("no staged hunk under cursor");
            return;
        };
        match git::unstage_hunk(&path, &hunk) {
            Ok(()) => {
                self.bus.info("hunk unstaged");
                self.refresh_git_signs_force();
            }
            Err(e) => {
                self.bus.error(format!("git unstage: {e}"));
            }
        }
    }

    /// `:GitRevert` / gutter "Revert Hunk" — discard the unstaged hunk under the
    /// cursor, restoring the index baseline on disk, then reload the buffer so
    /// the editor reflects the reverted file. Requires a saved buffer.
    pub(crate) fn git_revert_hunk_at_cursor(&mut self) {
        if self.active().dirty {
            self.bus.warn("save the buffer before reverting (:w)");
            return;
        }
        let Some((path, hunk)) = self.unstaged_hunk_under_cursor() else {
            self.bus.warn("no unstaged hunk under cursor");
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

/// Relative time within one day (`now`, `t` unix seconds, UTC) as `now`/`{m}m`/
/// `{h}h`; the absolute ISO date otherwise (or for clock-skew/future commits).
fn format_blame_reltime(t: i64, now: i64) -> String {
    let d = now - t;
    if !(0..86_400).contains(&d) {
        return format_blame_date(t);
    }
    if d < 60 {
        "now".to_string()
    } else if d < 3600 {
        format!("{}m", d / 60)
    } else {
        format!("{}h", d / 3600)
    }
}

/// Build the boxed-blame render plan for the viewport: walk visible document
/// rows from `top_row`, inserting a titled top border at each commit run's
/// first row and a bottom border after its last row, capped to `height` screen
/// rows. `is_hidden(row)` reports fold-hidden rows (skipped). The title is
/// `<hash> <author> · <reltime> · <summary>`.
pub(crate) fn build_blame_box_plan(
    blame: &[Option<git::BlameInfo>],
    line_count: usize,
    is_hidden: impl Fn(usize) -> bool,
    top_row: usize,
    height: usize,
    now: i64,
) -> Vec<hjkl_buffer_tui::render::BlameRow> {
    use hjkl_buffer_tui::render::BlameRow;
    let commit_of = |r: usize| blame.get(r).and_then(|o| o.as_ref());
    let mut plan: Vec<BlameRow> = Vec::with_capacity(height);
    let mut dr = top_row;
    while dr < line_count && plan.len() < height {
        if is_hidden(dr) {
            dr += 1;
            continue;
        }
        let Some(info) = commit_of(dr) else {
            plan.push(BlameRow::Content(dr));
            dr += 1;
            continue;
        };
        let run_start = dr == 0 || commit_of(dr - 1).is_none_or(|p| p.commit != info.commit);
        if run_start {
            let hash: String = info.commit.chars().take(8).collect();
            let when = if info.is_uncommitted {
                "now".to_string()
            } else {
                format_blame_reltime(info.time_unix, now)
            };
            plan.push(BlameRow::BorderTop(format!(
                "{hash} {} · {when} · {}",
                info.author, info.summary
            )));
            if plan.len() >= height {
                break;
            }
        }
        plan.push(BlameRow::Content(dr));
        if plan.len() >= height {
            break;
        }
        let run_end = commit_of(dr + 1).is_none_or(|n| n.commit != info.commit);
        if run_end {
            plan.push(BlameRow::BorderBottom);
        }
        dr += 1;
    }
    plan
}

/// Format a unix timestamp (seconds, UTC) as `YYYY-MM-DD`. Self-contained
/// civil-date conversion (Howard Hinnant's algorithm) — no chrono/time dep.
fn format_blame_date(time_unix: i64) -> String {
    let days = time_unix.div_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::{format_blame_date, format_blame_reltime};

    #[test]
    fn format_blame_date_known_epochs() {
        assert_eq!(format_blame_date(0), "1970-01-01");
        assert_eq!(format_blame_date(1_700_000_000), "2023-11-14");
    }

    #[test]
    fn format_blame_reltime_relative_and_iso() {
        let now: i64 = 1_700_000_000;
        assert_eq!(format_blame_reltime(now, now), "now");
        assert_eq!(format_blame_reltime(now - 120, now), "2m");
        assert_eq!(format_blame_reltime(now - 7200, now), "2h");
        // ≥ 1 day → ISO date.
        assert_eq!(
            format_blame_reltime(now - 86_400, now),
            format_blame_date(now - 86_400)
        );
        // future commit → ISO.
        assert_eq!(
            format_blame_reltime(now + 100, now),
            format_blame_date(now + 100)
        );
    }
}
