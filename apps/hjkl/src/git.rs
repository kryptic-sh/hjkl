//! Git diff signs for the gutter.
//!
//! `signs_for(path)` returns a `Vec<Sign>` describing per-line changes
//! between the file at `path` and its blob in HEAD:
//!
//! - `+` (green) on rows that were added
//! - `~` (yellow) on rows that were modified
//! - `_` (red) above rows where lines were deleted
//!
//! Untracked files mark every row as added. Files outside a git repo,
//! files with no HEAD blob (e.g. brand-new repo, no commits), or any
//! git2 error returns an empty `Vec` — the caller renders no git column.
//!
//! The diff is computed against on-disk content (not the editor's
//! in-memory buffer), so signs reflect what `git status` would show.
//! Refresh after `:w` to pick up the user's saved state.

use std::path::Path;

use git2::{DiffOptions, ErrorCode, Patch, Repository};
use hjkl_buffer::Sign;
use ratatui::style::{Color, Style};

const PRIORITY: u8 = 50;

/// Compute git diff signs for `path` against its HEAD blob, comparing
/// against `current` (the editor's in-memory buffer bytes — pass
/// `lines.join("\n")` + trailing `\n` for non-empty content to match
/// what `:w` would write).
///
/// Errors are swallowed — out-of-repo, no HEAD blob, or any git2
/// failure returns an empty Vec or untracked-style "every row added".
pub fn signs_for_bytes(path: &Path, current: &[u8]) -> Vec<Sign> {
    try_signs_with_bytes(path, current).unwrap_or_default()
}

fn try_signs_with_bytes(path: &Path, current: &[u8]) -> Result<Vec<Sign>, git2::Error> {
    // Canonicalize first so relative single-component paths (e.g.
    // `.gitignore`) don't yield an empty parent — Path::new("foo").parent()
    // returns Some("") not None, which Repository::discover rejects.
    let canon_path = path
        .canonicalize()
        .map_err(|e| git2::Error::from_str(&e.to_string()))?;
    let parent = canon_path.parent().unwrap_or(Path::new("."));
    let repo = Repository::discover(parent)?;
    let workdir = repo
        .workdir()
        .ok_or_else(|| git2::Error::from_str("bare repo has no workdir"))?;
    let rel = canon_path
        .strip_prefix(
            workdir
                .canonicalize()
                .map_err(|e| git2::Error::from_str(&e.to_string()))?,
        )
        .map_err(|_| git2::Error::from_str("path outside repo workdir"))?
        .to_path_buf();

    let style_add = Style::default().fg(Color::Green);
    let style_mod = Style::default().fg(Color::Yellow);
    let style_del = Style::default().fg(Color::Red);

    // Untracked or no HEAD → no per-line signs (status line carries
    // the `[Untracked]` tag instead; per-line `+` floods are noise).
    let head = match repo.head() {
        Ok(h) => h,
        Err(e) if e.code() == ErrorCode::UnbornBranch || e.code() == ErrorCode::NotFound => {
            return Ok(Vec::new());
        }
        Err(e) => return Err(e),
    };
    let tree = head.peel_to_tree()?;
    let entry = match tree.get_path(&rel) {
        Ok(e) => e,
        Err(e) if e.code() == ErrorCode::NotFound => {
            return Ok(Vec::new());
        }
        Err(e) => return Err(e),
    };
    let blob = repo.find_blob(entry.id())?;

    let mut opts = DiffOptions::new();
    opts.context_lines(0);
    let patch = Patch::from_blob_and_buffer(&blob, None, current, None, Some(&mut opts))?;

    let mut signs = Vec::new();
    for h in 0..patch.num_hunks() {
        let (hunk, _) = patch.hunk(h)?;
        let new_start = hunk.new_start() as usize;
        let new_lines = hunk.new_lines() as usize;
        let old_lines = hunk.old_lines() as usize;

        if new_lines == 0 && old_lines > 0 {
            let row = new_start.saturating_sub(1);
            signs.push(Sign {
                row,
                ch: '_',
                style: style_del,
                priority: PRIORITY,
            });
        } else if old_lines == 0 && new_lines > 0 {
            for i in 0..new_lines {
                signs.push(Sign {
                    row: (new_start + i).saturating_sub(1),
                    ch: '+',
                    style: style_add,
                    priority: PRIORITY,
                });
            }
        } else {
            for i in 0..new_lines {
                signs.push(Sign {
                    row: (new_start + i).saturating_sub(1),
                    ch: '~',
                    style: style_mod,
                    priority: PRIORITY,
                });
            }
        }
    }

    signs.sort_by_key(|s| s.row);
    Ok(signs)
}

/// `true` when the file exists in a git workdir but isn't present in
/// the HEAD tree (newly created, never committed). Drives the
/// `[Untracked]` status-line tag — distinct from the diff-signs path
/// which returns empty for untracked files (no per-line `+` flood).
pub fn is_untracked(path: &Path) -> bool {
    try_is_untracked(path).unwrap_or(false)
}

fn try_is_untracked(path: &Path) -> Result<bool, git2::Error> {
    let canon_path = path
        .canonicalize()
        .map_err(|e| git2::Error::from_str(&e.to_string()))?;
    let parent = canon_path.parent().unwrap_or(Path::new("."));
    let repo = Repository::discover(parent)?;
    let workdir = repo
        .workdir()
        .ok_or_else(|| git2::Error::from_str("bare repo has no workdir"))?;
    let rel = canon_path
        .strip_prefix(
            workdir
                .canonicalize()
                .map_err(|e| git2::Error::from_str(&e.to_string()))?,
        )
        .map_err(|_| git2::Error::from_str("path outside repo workdir"))?;
    let head = match repo.head() {
        Ok(h) => h,
        Err(e) if e.code() == ErrorCode::UnbornBranch || e.code() == ErrorCode::NotFound => {
            return Ok(true);
        }
        Err(e) => return Err(e),
    };
    let tree = head.peel_to_tree()?;
    match tree.get_path(rel) {
        Ok(_) => Ok(false),
        Err(e) if e.code() == ErrorCode::NotFound => Ok(true),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .expect("git command");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn no_repo_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let f = tmp.path().join("a.txt");
        std::fs::write(&f, "hello\n").unwrap();
        assert!(signs_for_bytes(&f, b"hello\n").is_empty());
    }

    #[test]
    fn untracked_file_marks_every_row_added() {
        let tmp = TempDir::new().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        let f = tmp.path().join("u.txt");
        std::fs::write(&f, "a\nb\nc\n").unwrap();
        let bytes = std::fs::read(&f).unwrap();
        let signs = signs_for_bytes(&f, &bytes);
        // 3 rows, all '+'.
        assert_eq!(signs.len(), 3);
        assert!(signs.iter().all(|s| s.ch == '+'));
    }

    #[test]
    fn modified_line_emits_tilde() {
        let tmp = TempDir::new().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        let f = tmp.path().join("m.txt");
        std::fs::write(&f, "alpha\nbravo\ncharlie\n").unwrap();
        git(tmp.path(), &["add", "m.txt"]);
        git(tmp.path(), &["commit", "-q", "-m", "init"]);
        // Modify row 1.
        std::fs::write(&f, "alpha\nBRAVO\ncharlie\n").unwrap();
        let bytes = std::fs::read(&f).unwrap();
        let signs = signs_for_bytes(&f, &bytes);
        assert!(signs.iter().any(|s| s.row == 1 && s.ch == '~'));
    }

    #[test]
    fn added_line_emits_plus() {
        let tmp = TempDir::new().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        let f = tmp.path().join("a.txt");
        std::fs::write(&f, "alpha\nbravo\n").unwrap();
        git(tmp.path(), &["add", "a.txt"]);
        git(tmp.path(), &["commit", "-q", "-m", "init"]);
        std::fs::write(&f, "alpha\nbravo\nNEW\n").unwrap();
        let bytes = std::fs::read(&f).unwrap();
        let signs = signs_for_bytes(&f, &bytes);
        assert!(signs.iter().any(|s| s.row == 2 && s.ch == '+'));
    }

    #[test]
    fn modified_buffer_against_unchanged_disk_emits_signs() {
        // The bug we're chasing: tracked file unchanged on disk, but the
        // editor's in-memory buffer has unsaved edits. signs_for_bytes
        // must compare HEAD blob against the *provided bytes*, not disk.
        let tmp = TempDir::new().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        let f = tmp.path().join("app.rs");
        std::fs::write(&f, "alpha\nbravo\ncharlie\n").unwrap();
        git(tmp.path(), &["add", "app.rs"]);
        git(tmp.path(), &["commit", "-q", "-m", "init"]);
        // Disk content unchanged; pretend the editor has an unsaved edit.
        let in_memory = b"alpha\nBRAVO\ncharlie\n";
        let signs = signs_for_bytes(&f, in_memory);
        assert!(
            signs.iter().any(|s| s.row == 1 && s.ch == '~'),
            "expected '~' on row 1 from in-memory diff; got {signs:?}"
        );
    }

    #[test]
    fn deleted_line_emits_underscore() {
        let tmp = TempDir::new().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        let f = tmp.path().join("d.txt");
        std::fs::write(&f, "alpha\nbravo\ncharlie\n").unwrap();
        git(tmp.path(), &["add", "d.txt"]);
        git(tmp.path(), &["commit", "-q", "-m", "init"]);
        // Delete row 1 ("bravo").
        std::fs::write(&f, "alpha\ncharlie\n").unwrap();
        let bytes = std::fs::read(&f).unwrap();
        let signs = signs_for_bytes(&f, &bytes);
        assert!(signs.iter().any(|s| s.ch == '_'));
    }
}
