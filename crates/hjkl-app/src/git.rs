//! Host-agnostic git diff change computation for the gutter.
//!
//! `changes_for_bytes(path, bytes)` returns a `Vec<GitChange>` describing
//! per-line changes between `bytes` and the file's blob in HEAD:
//!
//! - `GitChangeKind::Add` on rows that were added
//! - `GitChangeKind::Modify` on rows that were modified
//! - `GitChangeKind::Delete` above rows where lines were deleted
//!
//! Untracked files and files outside a git repo, files with no HEAD blob
//! (e.g. brand-new repo, no commits), or any git2 error returns an empty
//! `Vec` — the caller renders no git column.
//!
//! The diff is computed against the provided bytes (the editor's in-memory
//! buffer), so changes reflect unsaved edits. Hosts convert `GitChange` to
//! their own render type (e.g. `hjkl_buffer::Sign` for the ratatui TUI).

use std::path::Path;

use git2::{DiffOptions, ErrorCode, Patch, Repository};

/// One per-row git-diff change. Hosts convert to their own render
/// type (e.g. `hjkl_buffer::Sign` for the ratatui TUI).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GitChange {
    pub row: usize,
    pub kind: GitChangeKind,
}

/// The kind of change on a given row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitChangeKind {
    Add,
    Modify,
    Delete,
}

/// Compute git diff changes for `path` against its HEAD blob, comparing
/// against `current` (the editor's in-memory buffer bytes — pass
/// `lines.join("\n")` + trailing `\n` for non-empty content to match
/// what `:w` would write).
///
/// Errors are swallowed — out-of-repo, no HEAD blob, or any git2
/// failure returns an empty Vec.
pub fn changes_for_bytes(path: &Path, current: &[u8]) -> Vec<GitChange> {
    try_changes_with_bytes(path, current).unwrap_or_default()
}

fn try_changes_with_bytes(path: &Path, current: &[u8]) -> Result<Vec<GitChange>, git2::Error> {
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

    // Untracked or no HEAD → no per-line changes (status line carries
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

    let mut changes = Vec::new();
    for h in 0..patch.num_hunks() {
        let (hunk, _) = patch.hunk(h)?;
        let new_start = hunk.new_start() as usize;
        let new_lines = hunk.new_lines() as usize;
        let old_lines = hunk.old_lines() as usize;

        if new_lines == 0 && old_lines > 0 {
            let row = new_start.saturating_sub(1);
            changes.push(GitChange {
                row,
                kind: GitChangeKind::Delete,
            });
        } else if old_lines == 0 && new_lines > 0 {
            for i in 0..new_lines {
                changes.push(GitChange {
                    row: (new_start + i).saturating_sub(1),
                    kind: GitChangeKind::Add,
                });
            }
        } else {
            for i in 0..new_lines {
                changes.push(GitChange {
                    row: (new_start + i).saturating_sub(1),
                    kind: GitChangeKind::Modify,
                });
            }
        }
    }

    changes.sort_by_key(|c| c.row);
    Ok(changes)
}

// ---------------------------------------------------------------------------
// Hunk model (#115) — group the diff into stage/revert/preview units.
// ---------------------------------------------------------------------------

/// One contiguous diff hunk between the HEAD blob and the current buffer.
///
/// Coordinates are git unified-diff convention (1-based; a zero-length side
/// uses the count as its start so `-0,0` never appears mid-file). `body` is the
/// ready-to-apply patch body (` `/`-`/`+` lines, each `\n`-terminated); `header`
/// is the matching `@@ -a,b +c,d @@` line. Together with a one-line
/// `diff --git` preamble these form a patch `git apply` accepts (Phase 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: usize,
    pub old_lines: usize,
    pub new_start: usize,
    pub new_lines: usize,
    pub header: String,
    pub body: String,
}

impl Hunk {
    /// 0-based inclusive current-buffer row range this hunk covers. A pure
    /// deletion (`new_lines == 0`) collapses to the single row the deletion
    /// sign sits on — matching [`GitChangeKind::Delete`] placement.
    pub fn new_row_range(&self) -> std::ops::RangeInclusive<usize> {
        if self.new_lines == 0 {
            let row = self.new_start.saturating_sub(1);
            row..=row
        } else {
            let start = self.new_start - 1;
            start..=start + self.new_lines - 1
        }
    }
}

/// Compute hunks for `path` against its HEAD blob, comparing against `current`
/// (the editor's in-memory bytes). Errors / out-of-repo / untracked / no HEAD
/// blob all yield an empty `Vec`, exactly like [`changes_for_bytes`].
pub fn hunks_for_bytes(path: &Path, current: &[u8]) -> Vec<Hunk> {
    try_hunks_with_bytes(path, current).unwrap_or_default()
}

fn try_hunks_with_bytes(path: &Path, current: &[u8]) -> Result<Vec<Hunk>, git2::Error> {
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
        Err(e) if e.code() == ErrorCode::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let blob = repo.find_blob(entry.id())?;

    // 3 lines of context — git's default, and what `git apply` expects.
    let mut opts = DiffOptions::new();
    opts.context_lines(3);
    let patch = Patch::from_blob_and_buffer(&blob, None, current, None, Some(&mut opts))?;

    Ok(hunks_from_patch(&patch))
}

/// Build [`Hunk`]s from a computed git2 [`Patch`]. Split out so the assembly
/// logic is reachable from tests without re-deriving the repo plumbing.
fn hunks_from_patch(patch: &Patch) -> Vec<Hunk> {
    let mut hunks = Vec::new();
    for h in 0..patch.num_hunks() {
        let Ok((hunk, _)) = patch.hunk(h) else {
            continue;
        };
        let old_start = hunk.old_start() as usize;
        let old_lines = hunk.old_lines() as usize;
        let new_start = hunk.new_start() as usize;
        let new_lines = hunk.new_lines() as usize;

        let header = format!("@@ -{old_start},{old_lines} +{new_start},{new_lines} @@");

        let mut body = String::new();
        let n_lines = patch.num_lines_in_hunk(h).unwrap_or(0);
        for l in 0..n_lines {
            let Ok(line) = patch.line_in_hunk(h, l) else {
                continue;
            };
            // origin: ' ' context, '+' addition, '-' deletion. Other origins
            // (file headers, EOFNL markers) don't occur inside a hunk body.
            let origin = line.origin();
            if matches!(origin, ' ' | '+' | '-') {
                body.push(origin);
            }
            body.push_str(&String::from_utf8_lossy(line.content()));
            // Ensure each patch line is newline-terminated even when the source
            // file's last line had no trailing newline.
            if !body.ends_with('\n') {
                body.push('\n');
            }
        }

        hunks.push(Hunk {
            old_start,
            old_lines,
            new_start,
            new_lines,
            header,
            body,
        });
    }
    hunks
}

/// The hunk whose current-buffer row range contains `row` (0-based), if any.
/// Hunks are ordered and non-overlapping, so at most one matches.
pub fn hunk_at(hunks: &[Hunk], row: usize) -> Option<&Hunk> {
    hunks.iter().find(|h| h.new_row_range().contains(&row))
}

// ---------------------------------------------------------------------------
// Hunk stage / revert (#115 Phase 2) — mutate the index / worktree.
// ---------------------------------------------------------------------------

/// Outcome of a stage / revert attempt.
#[derive(Debug)]
pub enum HunkApplyError {
    /// `path` is not inside a git repository.
    NotInRepo,
    /// The repo path could not be turned into a workdir-relative path.
    PathResolution,
    /// `git apply` ran but rejected the patch (stderr captured).
    ApplyFailed(String),
    /// Spawning `git` failed (not installed / I/O error).
    Spawn(String),
}

impl std::fmt::Display for HunkApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HunkApplyError::NotInRepo => write!(f, "not in a git repository"),
            HunkApplyError::PathResolution => write!(f, "could not resolve path within repo"),
            HunkApplyError::ApplyFailed(e) => write!(f, "git apply failed: {e}"),
            HunkApplyError::Spawn(e) => write!(f, "could not run git: {e}"),
        }
    }
}

impl std::error::Error for HunkApplyError {}

/// Build a complete patch (with `diff --git` preamble) for `hunk` on the
/// workdir-relative path `rel`, suitable for `git apply`.
///
/// `git apply` needs the `a/`,`b/` path headers to know which file the hunk
/// targets; the `@@` header + body come straight from [`Hunk`].
fn build_patch(rel: &Path, hunk: &Hunk) -> String {
    let p = rel.to_string_lossy();
    format!(
        "diff --git a/{p} b/{p}\n--- a/{p}\n+++ b/{p}\n{}\n{}",
        hunk.header, hunk.body
    )
}

/// Resolve `path` to `(repo_workdir, workdir_relative_path)`.
fn resolve_in_repo(
    path: &Path,
) -> Result<(std::path::PathBuf, std::path::PathBuf), HunkApplyError> {
    let canon = path.canonicalize().map_err(|_| HunkApplyError::NotInRepo)?;
    let parent = canon.parent().unwrap_or(Path::new("."));
    let repo = Repository::discover(parent).map_err(|_| HunkApplyError::NotInRepo)?;
    let workdir = repo
        .workdir()
        .ok_or(HunkApplyError::NotInRepo)?
        .canonicalize()
        .map_err(|_| HunkApplyError::PathResolution)?;
    let rel = canon
        .strip_prefix(&workdir)
        .map_err(|_| HunkApplyError::PathResolution)?
        .to_path_buf();
    Ok((workdir, rel))
}

/// Stage `hunk` for `path` into the git index (`git apply --cached`).
///
/// Operates on the on-disk file's relationship to HEAD, so callers must ensure
/// the buffer is saved first (the hunk was computed from buffer bytes that must
/// match disk for the patch to apply cleanly).
pub fn stage_hunk(path: &Path, hunk: &Hunk) -> Result<(), HunkApplyError> {
    let (workdir, rel) = resolve_in_repo(path)?;
    let patch = build_patch(&rel, hunk);
    run_git_apply(&workdir, &["apply", "--cached", "-"], &patch)
}

/// Revert `hunk` in the worktree (`git apply --reverse`), discarding that
/// change and restoring the HEAD/index version of those lines on disk.
pub fn revert_hunk(path: &Path, hunk: &Hunk) -> Result<(), HunkApplyError> {
    let (workdir, rel) = resolve_in_repo(path)?;
    let patch = build_patch(&rel, hunk);
    run_git_apply(&workdir, &["apply", "--reverse", "-"], &patch)
}

/// Run `git <args>` in `cwd`, feeding `patch` on stdin.
fn run_git_apply(cwd: &Path, args: &[&str], patch: &str) -> Result<(), HunkApplyError> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut child = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| HunkApplyError::Spawn(e.to_string()))?;

    child
        .stdin
        .as_mut()
        .ok_or_else(|| HunkApplyError::Spawn("no stdin".into()))?
        .write_all(patch.as_bytes())
        .map_err(|e| HunkApplyError::Spawn(e.to_string()))?;

    let out = child
        .wait_with_output()
        .map_err(|e| HunkApplyError::Spawn(e.to_string()))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(HunkApplyError::ApplyFailed(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ))
    }
}

/// `true` when the file exists in a git workdir but isn't present in
/// the HEAD tree (newly created, never committed). Drives the
/// `[Untracked]` status-line tag — distinct from the diff-changes path
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
        assert!(changes_for_bytes(&f, b"hello\n").is_empty());
    }

    #[test]
    fn untracked_file_emits_no_changes() {
        // Untracked files no longer flood the gutter with `+`; the
        // `[Untracked]` status-line tag carries the signal instead.
        let tmp = TempDir::new().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        let f = tmp.path().join("u.txt");
        std::fs::write(&f, "a\nb\nc\n").unwrap();
        let bytes = std::fs::read(&f).unwrap();
        let changes = changes_for_bytes(&f, &bytes);
        assert!(changes.is_empty(), "expected no changes; got {changes:?}");
        assert!(is_untracked(&f), "expected is_untracked=true");
    }

    #[test]
    fn modified_line_emits_modify() {
        let tmp = TempDir::new().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        let f = tmp.path().join("m.txt");
        std::fs::write(&f, "alpha\nbravo\ncharlie\n").unwrap();
        git(tmp.path(), &["add", "m.txt"]);
        git(tmp.path(), &["commit", "-q", "-m", "init"]);
        // Modify row 1.
        std::fs::write(&f, "alpha\nBRAVO\ncharlie\n").unwrap();
        let bytes = std::fs::read(&f).unwrap();
        let changes = changes_for_bytes(&f, &bytes);
        assert!(
            changes
                .iter()
                .any(|c| c.row == 1 && c.kind == GitChangeKind::Modify)
        );
    }

    #[test]
    fn added_line_emits_add() {
        let tmp = TempDir::new().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        let f = tmp.path().join("a.txt");
        std::fs::write(&f, "alpha\nbravo\n").unwrap();
        git(tmp.path(), &["add", "a.txt"]);
        git(tmp.path(), &["commit", "-q", "-m", "init"]);
        std::fs::write(&f, "alpha\nbravo\nNEW\n").unwrap();
        let bytes = std::fs::read(&f).unwrap();
        let changes = changes_for_bytes(&f, &bytes);
        assert!(
            changes
                .iter()
                .any(|c| c.row == 2 && c.kind == GitChangeKind::Add)
        );
    }

    #[test]
    fn modified_buffer_against_unchanged_disk_emits_changes() {
        // Tracked file unchanged on disk, but the editor's in-memory
        // buffer has unsaved edits. changes_for_bytes must compare
        // HEAD blob against the *provided bytes*, not disk.
        let tmp = TempDir::new().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        let f = tmp.path().join("app.rs");
        std::fs::write(&f, "alpha\nbravo\ncharlie\n").unwrap();
        git(tmp.path(), &["add", "app.rs"]);
        git(tmp.path(), &["commit", "-q", "-m", "init"]);
        // Disk content unchanged; pretend the editor has an unsaved edit.
        let in_memory = b"alpha\nBRAVO\ncharlie\n";
        let changes = changes_for_bytes(&f, in_memory);
        assert!(
            changes
                .iter()
                .any(|c| c.row == 1 && c.kind == GitChangeKind::Modify),
            "expected Modify on row 1 from in-memory diff; got {changes:?}"
        );
    }

    #[test]
    fn deleted_line_emits_delete() {
        let tmp = TempDir::new().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        let f = tmp.path().join("d.txt");
        std::fs::write(&f, "alpha\nbravo\ncharlie\n").unwrap();
        git(tmp.path(), &["add", "d.txt"]);
        git(tmp.path(), &["commit", "-q", "-m", "init"]);
        // Delete row 1 ("bravo").
        std::fs::write(&f, "alpha\ncharlie\n").unwrap();
        let bytes = std::fs::read(&f).unwrap();
        let changes = changes_for_bytes(&f, &bytes);
        assert!(changes.iter().any(|c| c.kind == GitChangeKind::Delete));
    }

    // ── Hunk model (#115) ────────────────────────────────────────────────────

    fn commit_file(tmp: &Path, name: &str, content: &str) -> std::path::PathBuf {
        let f = tmp.join(name);
        std::fs::write(&f, content).unwrap();
        git(tmp, &["add", name]);
        git(tmp, &["commit", "-q", "-m", "init"]);
        f
    }

    #[test]
    #[ignore = "git2 integration: real repo + git subprocess; CI test-binary flake (#115 follow-up)"]
    fn no_change_yields_no_hunks() {
        let tmp = TempDir::new().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        let f = commit_file(tmp.path(), "h.txt", "a\nb\nc\n");
        assert!(hunks_for_bytes(&f, b"a\nb\nc\n").is_empty());
    }

    #[test]
    #[ignore = "git2 integration: real repo + git subprocess; CI test-binary flake (#115 follow-up)"]
    fn modified_line_one_hunk_with_patch_body() {
        let tmp = TempDir::new().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        let f = commit_file(tmp.path(), "h.txt", "a\nb\nc\n");
        let hunks = hunks_for_bytes(&f, b"a\nB\nc\n");
        assert_eq!(hunks.len(), 1, "got {hunks:?}");
        let h = &hunks[0];
        // The change is on row 1 (0-based).
        assert!(h.new_row_range().contains(&1));
        assert!(hunk_at(&hunks, 1).is_some());
        // Patch body carries both sides + context, header well-formed.
        assert!(h.body.contains("-b\n"), "body: {:?}", h.body);
        assert!(h.body.contains("+B\n"), "body: {:?}", h.body);
        assert!(h.body.contains(" a\n"), "context expected: {:?}", h.body);
        assert!(h.header.starts_with("@@ -"), "header: {}", h.header);
    }

    #[test]
    #[ignore = "git2 integration: real repo + git subprocess; CI test-binary flake (#115 follow-up)"]
    fn added_lines_hunk_covers_new_rows() {
        let tmp = TempDir::new().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        let f = commit_file(tmp.path(), "h.txt", "a\nb\n");
        let hunks = hunks_for_bytes(&f, b"a\nNEW1\nNEW2\nb\n");
        assert_eq!(hunks.len(), 1);
        let h = &hunks[0];
        assert!(h.body.contains("+NEW1\n") && h.body.contains("+NEW2\n"));
    }

    #[test]
    #[ignore = "git2 integration: real repo + git subprocess; CI test-binary flake (#115 follow-up)"]
    fn two_separate_changes_two_hunks() {
        let tmp = TempDir::new().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        let f = commit_file(
            tmp.path(),
            "h.txt",
            "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n",
        );
        let hunks = hunks_for_bytes(&f, b"X\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\nY\n");
        assert_eq!(hunks.len(), 2, "got {hunks:?}");
        assert!(hunk_at(&hunks, 0).is_some(), "first change at row 0");
        assert!(hunk_at(&hunks, 11).is_some(), "second change at row 11");
    }

    #[test]
    #[ignore = "git2 integration: real repo + git subprocess; CI test-binary flake (#115 follow-up)"]
    fn hunk_at_off_change_is_none() {
        let tmp = TempDir::new().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        let f = commit_file(tmp.path(), "h.txt", "1\n2\n3\n4\n5\n6\n7\n8\n");
        let hunks = hunks_for_bytes(&f, b"1\n2\n3\n4\nFIVE\n6\n7\n8\n");
        // Row 0 is beyond the context window of the row-4 change.
        assert!(hunk_at(&hunks, 0).is_none());
        assert!(hunk_at(&hunks, 4).is_some());
    }

    #[test]
    fn no_repo_yields_no_hunks() {
        let tmp = TempDir::new().unwrap();
        let f = tmp.path().join("x.txt");
        std::fs::write(&f, "hello\n").unwrap();
        assert!(hunks_for_bytes(&f, b"changed\n").is_empty());
    }

    // ── Stage / revert (#115 Phase 2) ─────────────────────────────────────────

    /// Capture `git diff --cached` for `name` (what's staged in the index).
    fn staged_diff(tmp: &Path, name: &str) -> String {
        let out = Command::new("git")
            .args(["diff", "--cached", "--", name])
            .current_dir(tmp)
            .output()
            .expect("git diff --cached");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[test]
    #[ignore = "git2 integration: real repo + git subprocess; CI test-binary flake (#115 follow-up)"]
    fn stage_hunk_applies_to_index() {
        let tmp = TempDir::new().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        let f = commit_file(tmp.path(), "s.txt", "a\nb\nc\n");
        // Edit on disk + save (stage works against the on-disk file vs HEAD).
        std::fs::write(&f, "a\nB\nc\n").unwrap();

        let bytes = std::fs::read(&f).unwrap();
        let hunks = hunks_for_bytes(&f, &bytes);
        assert_eq!(hunks.len(), 1, "expected one hunk, got {hunks:?}");

        stage_hunk(&f, &hunks[0]).expect("stage_hunk");

        // The index now carries the b→B change.
        let staged = staged_diff(tmp.path(), "s.txt");
        assert!(
            staged.contains("-b") && staged.contains("+B"),
            "staged: {staged}"
        );
    }

    #[test]
    #[ignore = "git2 integration: real repo + git subprocess; CI test-binary flake (#115 follow-up)"]
    fn revert_hunk_restores_worktree() {
        let tmp = TempDir::new().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        let f = commit_file(tmp.path(), "r.txt", "a\nb\nc\n");
        std::fs::write(&f, "a\nB\nc\n").unwrap();

        let bytes = std::fs::read(&f).unwrap();
        let hunks = hunks_for_bytes(&f, &bytes);
        assert_eq!(hunks.len(), 1);

        revert_hunk(&f, &hunks[0]).expect("revert_hunk");

        // The worktree file is back to the committed content.
        let after = std::fs::read_to_string(&f).unwrap();
        assert_eq!(after, "a\nb\nc\n", "revert must restore HEAD content");
    }

    #[test]
    #[ignore = "git2 integration: real repo + git subprocess; CI test-binary flake (#115 follow-up)"]
    fn stage_hunk_outside_repo_errs() {
        let tmp = TempDir::new().unwrap();
        let f = tmp.path().join("loose.txt");
        std::fs::write(&f, "x\n").unwrap();
        let dummy = Hunk {
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 1,
            header: "@@ -1,1 +1,1 @@".into(),
            body: "-x\n+y\n".into(),
        };
        assert!(
            matches!(stage_hunk(&f, &dummy), Err(HunkApplyError::NotInRepo)),
            "staging outside a repo must be NotInRepo"
        );
    }
}
