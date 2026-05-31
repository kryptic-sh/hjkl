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
        let on = !self.active().blame_column;
        self.active_mut().blame_column = on;
        if on {
            // Blame data is normally gated on settings().blame_inline; the
            // column needs it regardless, so force a refresh now.
            self.refresh_blame_force();
        }
        self.bus.info(if on {
            "git blame column: on"
        } else {
            "git blame column: off"
        });
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

/// One blame-column entry, aligned to a visible screen row. `text` is empty for
/// continuation rows (same commit as the row above) so only the FIRST line of
/// each contiguous commit run is labeled. Uncommitted rows render their own
/// label every time they start a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BlameColumnRow {
    pub text: String,
    pub is_uncommitted: bool,
}

/// Format a commit time for the blame column. Within 8 hours of `now` (both
/// unix seconds, UTC), show a coarse relative label ("just now", "5m", "3h");
/// at 8 hours or older, fall back to the absolute ISO date `YYYY-MM-DD`.
/// `now < time_unix` (clock skew / future commit) also falls back to ISO.
fn format_blame_time(time_unix: i64, now: i64) -> String {
    const EIGHT_HOURS: i64 = 8 * 3600;
    let delta = now - time_unix;
    if !(0..EIGHT_HOURS).contains(&delta) {
        return format_blame_date(time_unix);
    }
    if delta < 60 {
        "just now".to_string()
    } else if delta < 3600 {
        format!("{}m", delta / 60)
    } else {
        format!("{}h", delta / 3600)
    }
}

/// Build per-visible-row blame-column labels. `visible_doc_rows[i]` is the
/// 0-based document row shown on screen row `i` (caller computes this fold-aware;
/// pass `usize::MAX` for screen rows past EOF / blank — those get an empty entry).
/// `blame[doc_row]` is the attribution (None when unavailable). A row is labeled
/// only when its commit hash differs from the immediately preceding visible row's
/// commit (viewport-local comparison). For `i == 0` the row is always labeled.
/// Label text = `"{author} · {time}"` truncated to `width` display columns (chars).
pub(crate) fn build_blame_column(
    blame: &[Option<git::BlameInfo>],
    visible_doc_rows: &[usize],
    now: i64,
    width: usize,
) -> Vec<BlameColumnRow> {
    let mut out = Vec::with_capacity(visible_doc_rows.len());
    for (i, &dr) in visible_doc_rows.iter().enumerate() {
        if dr == usize::MAX || dr >= blame.len() || blame[dr].is_none() {
            out.push(BlameColumnRow {
                text: String::new(),
                is_uncommitted: false,
            });
            continue;
        }
        let info = blame[dr].as_ref().unwrap();
        // Compare against the immediately preceding visible row's commit.
        let above = i
            .checked_sub(1)
            .and_then(|j| visible_doc_rows.get(j))
            .and_then(|&d| blame.get(d))
            .and_then(|o| o.as_ref());
        let same = above.map(|a| a.commit == info.commit).unwrap_or(false);
        if same {
            out.push(BlameColumnRow {
                text: String::new(),
                is_uncommitted: info.is_uncommitted,
            });
        } else {
            let raw = format!(
                "{} · {}",
                info.author,
                format_blame_time(info.time_unix, now)
            );
            let text = if width == 0 {
                String::new()
            } else if raw.chars().count() > width {
                let truncated: String = raw.chars().take(width - 1).collect();
                format!("{truncated}…")
            } else {
                raw
            };
            out.push(BlameColumnRow {
                text,
                is_uncommitted: info.is_uncommitted,
            });
        }
    }
    out
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
    use super::{build_blame_column, format_blame_date, format_blame_time};
    use hjkl_app::git::BlameInfo;

    fn make_info(commit: &str, author: &str, time_unix: i64) -> BlameInfo {
        BlameInfo {
            commit: commit.to_string(),
            author: author.to_string(),
            time_unix,
            summary: "test commit".to_string(),
            is_uncommitted: false,
        }
    }

    #[test]
    fn format_blame_date_known_epochs() {
        assert_eq!(format_blame_date(0), "1970-01-01");
        assert_eq!(format_blame_date(1_700_000_000), "2023-11-14");
    }

    #[test]
    fn format_blame_time_relative_and_iso() {
        let now: i64 = 1_700_000_000;
        assert_eq!(format_blame_time(now, now), "just now");
        assert_eq!(format_blame_time(now - 120, now), "2m");
        assert_eq!(format_blame_time(now - 7200, now), "2h");
        assert_eq!(
            format_blame_time(now - 8 * 3600, now),
            format_blame_date(now - 8 * 3600)
        );
        // future commit → ISO
        assert_eq!(
            format_blame_time(now + 100, now),
            format_blame_date(now + 100)
        );
    }

    #[test]
    fn build_blame_column_collapses_runs() {
        let now: i64 = 1_700_000_000;
        let blame: Vec<Option<BlameInfo>> = vec![
            Some(make_info("aaaaaaa", "alice", now - 3600)),
            Some(make_info("aaaaaaa", "alice", now - 3600)),
            Some(make_info("bbbbbbb", "bob", now - 7200)),
            Some(make_info("bbbbbbb", "bob", now - 7200)),
        ];
        let visible = vec![0usize, 1, 2, 3];
        let result = build_blame_column(&blame, &visible, now, 40);
        assert_eq!(result.len(), 4);
        assert!(
            result[0].text.starts_with("alice"),
            "row0={:?}",
            result[0].text
        );
        assert!(
            result[1].text.is_empty(),
            "row1 should be blank, got {:?}",
            result[1].text
        );
        assert!(
            result[2].text.starts_with("bob"),
            "row2={:?}",
            result[2].text
        );
        assert!(
            result[3].text.is_empty(),
            "row3 should be blank, got {:?}",
            result[3].text
        );
    }

    #[test]
    fn build_blame_column_blank_for_eof_and_none() {
        let now: i64 = 1_700_000_000;
        let blame: Vec<Option<BlameInfo>> = vec![Some(make_info("aaaaaaa", "alice", now - 60))];
        let visible = vec![0usize, usize::MAX];
        let result = build_blame_column(&blame, &visible, now, 40);
        assert_eq!(result.len(), 2);
        assert!(!result[0].text.is_empty());
        assert!(result[1].text.is_empty());
    }

    #[test]
    fn build_blame_column_truncates() {
        let now: i64 = 1_700_000_000;
        // Author name long enough to exceed width=10 when combined with time.
        let blame: Vec<Option<BlameInfo>> =
            vec![Some(make_info("aaaaaaa", "averylongauthorname", now - 60))];
        let visible = vec![0usize];
        let result = build_blame_column(&blame, &visible, now, 10);
        assert_eq!(result.len(), 1);
        let char_count = result[0].text.chars().count();
        assert!(
            char_count <= 10,
            "expected <=10 chars, got {char_count}: {:?}",
            result[0].text
        );
        assert!(
            result[0].text.ends_with('…'),
            "expected trailing ellipsis: {:?}",
            result[0].text
        );
    }
}
