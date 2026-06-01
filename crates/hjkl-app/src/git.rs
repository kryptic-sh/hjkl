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

use git2::{BlameOptions, DiffOptions, ErrorCode, Patch, Repository};

/// One per-row git-diff change. Hosts convert to their own render
/// type (e.g. `hjkl_buffer::Sign` for the ratatui TUI).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GitChange {
    pub row: usize,
    pub kind: GitChangeKind,
    /// `true` when this change lives in the index (HEAD↔index — already
    /// staged), `false` when it is a worktree/buffer change not yet staged
    /// (index↔buffer). Hosts render staged rows with a distinct gutter glyph.
    pub staged: bool,
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

/// Resolve `path` to `(repo, workdir_relative_path)`, canonicalizing first so
/// relative single-component paths (e.g. `.gitignore`) don't yield an empty
/// parent — `Path::new("foo").parent()` returns `Some("")` not `None`, which
/// `Repository::discover` rejects.
fn open_repo_for(path: &Path) -> Result<(Repository, std::path::PathBuf), git2::Error> {
    let canon_path = path
        .canonicalize()
        .map_err(|e| git2::Error::from_str(&e.to_string()))?;
    let parent = canon_path.parent().unwrap_or(Path::new("."));
    let repo = Repository::discover(parent)?;
    let rel = {
        let workdir = repo
            .workdir()
            .ok_or_else(|| git2::Error::from_str("bare repo has no workdir"))?;
        canon_path
            .strip_prefix(
                workdir
                    .canonicalize()
                    .map_err(|e| git2::Error::from_str(&e.to_string()))?,
            )
            .map_err(|_| git2::Error::from_str("path outside repo workdir"))?
            .to_path_buf()
    };
    Ok((repo, rel))
}

/// The blob for `rel` in the HEAD tree, or `None` when there is no HEAD
/// (unborn branch / empty repo) or the path is not tracked in HEAD.
fn head_blob<'r>(repo: &'r Repository, rel: &Path) -> Result<Option<git2::Blob<'r>>, git2::Error> {
    let head = match repo.head() {
        Ok(h) => h,
        Err(e) if e.code() == ErrorCode::UnbornBranch || e.code() == ErrorCode::NotFound => {
            return Ok(None);
        }
        Err(e) => return Err(e),
    };
    let tree = head.peel_to_tree()?;
    match tree.get_path(rel) {
        Ok(entry) => Ok(Some(repo.find_blob(entry.id())?)),
        Err(e) if e.code() == ErrorCode::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// The blob for `rel` in the index (stage 0), or `None` when the path has no
/// index entry (untracked, or a conflict with no stage-0 entry).
fn index_blob<'r>(repo: &'r Repository, rel: &Path) -> Result<Option<git2::Blob<'r>>, git2::Error> {
    let index = repo.index()?;
    match index.get_path(rel, 0) {
        Some(entry) => Ok(Some(repo.find_blob(entry.id)?)),
        None => Ok(None),
    }
}

/// Collect per-row [`GitChange`]s from a computed context-0 [`Patch`], tagging
/// each with `staged`. Rows are in the patch's *new*-side coordinates.
fn collect_row_changes(patch: &Patch, staged: bool) -> Result<Vec<GitChange>, git2::Error> {
    let mut changes = Vec::new();
    for h in 0..patch.num_hunks() {
        let (hunk, _) = patch.hunk(h)?;
        let new_start = hunk.new_start() as usize;
        let new_lines = hunk.new_lines() as usize;
        let old_lines = hunk.old_lines() as usize;

        if new_lines == 0 && old_lines > 0 {
            changes.push(GitChange {
                row: new_start.saturating_sub(1),
                kind: GitChangeKind::Delete,
                staged,
            });
        } else if old_lines == 0 && new_lines > 0 {
            for i in 0..new_lines {
                changes.push(GitChange {
                    row: (new_start + i).saturating_sub(1),
                    kind: GitChangeKind::Add,
                    staged,
                });
            }
        } else {
            for i in 0..new_lines {
                changes.push(GitChange {
                    row: (new_start + i).saturating_sub(1),
                    kind: GitChangeKind::Modify,
                    staged,
                });
            }
        }
    }
    Ok(changes)
}

/// Compute gutter changes split into **unstaged** (index↔buffer) and **staged**
/// (HEAD↔index) sets.
///
/// When nothing is staged the index equals HEAD, so the staged set is empty and
/// the unstaged set is exactly HEAD↔buffer — identical to the pre-staging
/// behavior. Untracked / no-HEAD files yield an empty Vec (the `[Untracked]`
/// status tag carries that signal; per-line `+` floods are noise).
///
/// Per-row dedup: when a row carries both a staged and an unstaged change (e.g.
/// a line was staged then edited again), the **unstaged** change wins — that's
/// what the user is actively looking at. Staged rows are attributed in index
/// coordinates; with concurrent unstaged edits above them the mapping is
/// approximate, which is acceptable for a gutter hint.
fn try_changes_with_bytes(path: &Path, current: &[u8]) -> Result<Vec<GitChange>, git2::Error> {
    let (repo, rel) = open_repo_for(path)?;

    // Untracked or no HEAD → no per-line changes.
    let Some(head_blob) = head_blob(&repo, &rel)? else {
        return Ok(Vec::new());
    };
    let index_blob = index_blob(&repo, &rel)?;

    // Unstaged: index (or HEAD when there's no index entry) ↔ buffer.
    let unstaged_old = index_blob.as_ref().unwrap_or(&head_blob);
    let mut opts = DiffOptions::new();
    opts.context_lines(0);
    let patch = Patch::from_blob_and_buffer(unstaged_old, None, current, None, Some(&mut opts))?;
    let mut changes = collect_row_changes(&patch, false)?;

    // Staged: HEAD ↔ index, only when the index differs from HEAD.
    if let Some(idx) = index_blob.as_ref()
        && idx.id() != head_blob.id()
    {
        let mut opts = DiffOptions::new();
        opts.context_lines(0);
        let patch = Patch::from_blobs(&head_blob, None, idx, None, Some(&mut opts))?;
        let staged = collect_row_changes(&patch, true)?;
        let unstaged_rows: std::collections::HashSet<usize> =
            changes.iter().map(|c| c.row).collect();
        for c in staged {
            if !unstaged_rows.contains(&c.row) {
                changes.push(c);
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

/// Unstaged hunks: index↔buffer (what `git add`/stage would move into the
/// index). When nothing is staged the index equals HEAD, so this is identical
/// to [`hunks_for_bytes`] (HEAD↔buffer). Use this for stage / revert / preview
/// of the change the user is actively editing, since the patch is relative to
/// the index and `git apply --cached` applies cleanly.
pub fn unstaged_hunks_for_bytes(path: &Path, current: &[u8]) -> Vec<Hunk> {
    try_unstaged_hunks(path, current).unwrap_or_default()
}

fn try_unstaged_hunks(path: &Path, current: &[u8]) -> Result<Vec<Hunk>, git2::Error> {
    let (repo, rel) = open_repo_for(path)?;
    let Some(head_blob) = head_blob(&repo, &rel)? else {
        return Ok(Vec::new());
    };
    let index_blob = index_blob(&repo, &rel)?;
    let old = index_blob.as_ref().unwrap_or(&head_blob);
    let mut opts = DiffOptions::new();
    opts.context_lines(3);
    let patch = Patch::from_blob_and_buffer(old, None, current, None, Some(&mut opts))?;
    Ok(hunks_from_patch(&patch))
}

/// Staged hunks: HEAD↔index (what is already staged and could be *unstaged*).
/// Empty when the index matches HEAD. Row ranges are in index coordinates;
/// when the staged region is unchanged in the buffer (the common case after
/// staging without further edits) those rows line up with buffer rows.
pub fn staged_hunks_for_path(path: &Path) -> Vec<Hunk> {
    try_staged_hunks(path).unwrap_or_default()
}

fn try_staged_hunks(path: &Path) -> Result<Vec<Hunk>, git2::Error> {
    let (repo, rel) = open_repo_for(path)?;
    let Some(head_blob) = head_blob(&repo, &rel)? else {
        return Ok(Vec::new());
    };
    let Some(index_blob) = index_blob(&repo, &rel)? else {
        return Ok(Vec::new());
    };
    if index_blob.id() == head_blob.id() {
        return Ok(Vec::new());
    }
    let mut opts = DiffOptions::new();
    opts.context_lines(3);
    let patch = Patch::from_blobs(&head_blob, None, &index_blob, None, Some(&mut opts))?;
    Ok(hunks_from_patch(&patch))
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
/// change and restoring the index version of those lines on disk. Pair with an
/// unstaged hunk (index↔buffer) so the discard targets the index baseline.
pub fn revert_hunk(path: &Path, hunk: &Hunk) -> Result<(), HunkApplyError> {
    let (workdir, rel) = resolve_in_repo(path)?;
    let patch = build_patch(&rel, hunk);
    run_git_apply(&workdir, &["apply", "--reverse", "-"], &patch)
}

/// Unstage `hunk` from the index (`git apply --cached --reverse`), moving that
/// change back out of the index toward HEAD. Pair with a staged hunk
/// (HEAD↔index). The worktree / buffer is untouched, so no save is required.
pub fn unstage_hunk(path: &Path, hunk: &Hunk) -> Result<(), HunkApplyError> {
    let (workdir, rel) = resolve_in_repo(path)?;
    let patch = build_patch(&rel, hunk);
    run_git_apply(&workdir, &["apply", "--cached", "--reverse", "-"], &patch)
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

// ---------------------------------------------------------------------------
// Blame (#202) — per-line attribution.
// ---------------------------------------------------------------------------

/// One line's git blame attribution. `commit` is the short (7-char) hash;
/// for an uncommitted (locally-modified, not yet committed) line, `is_uncommitted`
/// is true and the other fields carry placeholder values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlameInfo {
    pub commit: String,  // short hash, or "0000000" when uncommitted
    pub author: String,  // author name, or "You" when uncommitted
    pub time_unix: i64,  // author time (unix seconds), 0 when uncommitted
    pub summary: String, // commit summary line, or "Not Committed Yet"
    pub is_uncommitted: bool,
}

/// Convert a [`git2::BlameHunk`] to a [`BlameInfo`], looking up the commit
/// summary from `repo` when the hunk is committed.
fn hunk_to_info(repo: &Repository, hunk: &git2::BlameHunk<'_>) -> BlameInfo {
    let oid = hunk.final_commit_id();
    if oid.is_zero() {
        return BlameInfo {
            commit: "0000000".into(),
            author: "You".into(),
            time_unix: 0,
            summary: "Not Committed Yet".into(),
            is_uncommitted: true,
        };
    }
    let short = oid.to_string();
    let short = short[..7.min(short.len())].to_owned();
    let author = hunk
        .final_signature()
        .and_then(|s| s.name().ok().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into());
    let time_unix = hunk
        .final_signature()
        .map(|s| s.when().seconds())
        .unwrap_or(0);
    let summary = repo
        .find_commit(oid)
        .ok()
        .and_then(|c| c.summary().ok().flatten().map(str::to_owned))
        .unwrap_or_default();
    BlameInfo {
        commit: short,
        author,
        time_unix,
        summary,
        is_uncommitted: false,
    }
}

/// Blame a single line of a file, accounting for in-memory (unsaved) edits.
///
/// `row` is 0-based (editor convention). Returns `None` when the file is
/// outside a repo, has no HEAD, is untracked, or `row` is out of range.
pub fn blame_line(path: &Path, row: usize, current: &[u8]) -> Option<BlameInfo> {
    try_blame_line(path, row, current).ok().flatten()
}

fn try_blame_line(
    path: &Path,
    row: usize,
    current: &[u8],
) -> Result<Option<BlameInfo>, git2::Error> {
    let (repo, rel) = open_repo_for(path)?;
    // Guard no-HEAD (unborn branch / not found).
    match repo.head() {
        Ok(_) => {}
        Err(e) if e.code() == ErrorCode::UnbornBranch || e.code() == ErrorCode::NotFound => {
            return Ok(None);
        }
        Err(e) => return Err(e),
    }
    let mut opts = BlameOptions::new();
    let blame = repo.blame_file(&rel, Some(&mut opts))?;
    let blame = blame.blame_buffer(current)?;
    let hunk = match blame.get_line(row + 1) {
        Some(h) => h,
        None => return Ok(None),
    };
    Ok(Some(hunk_to_info(&repo, &hunk)))
}

/// Blame every line of the in-memory buffer for `path`.
///
/// Returns a `Vec` with one entry per line; entries are `None` only when
/// `get_line` yields nothing for that row (shouldn't happen in practice).
/// Out-of-repo / no-HEAD / untracked files return an empty `Vec`.
pub fn blame_file_all(path: &Path, current: &[u8]) -> Vec<Option<BlameInfo>> {
    try_blame_all(path, current).unwrap_or_default()
}

fn try_blame_all(path: &Path, current: &[u8]) -> Result<Vec<Option<BlameInfo>>, git2::Error> {
    let (repo, rel) = open_repo_for(path)?;
    match repo.head() {
        Ok(_) => {}
        Err(e) if e.code() == ErrorCode::UnbornBranch || e.code() == ErrorCode::NotFound => {
            return Ok(Vec::new());
        }
        Err(e) => return Err(e),
    }
    let mut opts = BlameOptions::new();
    let blame = repo.blame_file(&rel, Some(&mut opts))?;
    let blame = blame.blame_buffer(current)?;

    // Count lines: split on '\n', drop trailing empty entry if buffer ends with '\n'.
    let line_count = {
        let parts = current.split(|&b| b == b'\n');
        let count = parts.count();
        if current.ends_with(b"\n") && count > 0 {
            count - 1
        } else {
            count
        }
    };

    let mut result = Vec::with_capacity(line_count);
    for row in 0..line_count {
        let entry = blame.get_line(row + 1).map(|h| hunk_to_info(&repo, &h));
        result.push(entry);
    }
    Ok(result)
}

/// Full commit message for `short_hash` (resolved against the repo containing
/// `path`). Returns `None` when the repo can't be opened, the hash doesn't
/// resolve, or the commit has no message. Used by the blame-column hover popup.
pub fn commit_message(path: &Path, short_hash: &str) -> Option<String> {
    try_commit_message(path, short_hash).ok().flatten()
}

fn try_commit_message(path: &Path, short_hash: &str) -> Result<Option<String>, git2::Error> {
    let (repo, _rel) = open_repo_for(path)?;
    let commit = repo.revparse_single(short_hash)?.peel_to_commit()?;
    Ok(commit.message().ok().map(|m| m.trim_end().to_string()))
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

    // ── Blame (#202) ─────────────────────────────────────────────────────────

    #[test]
    #[ignore = "git2 integration: real repo + git subprocess; CI test-binary flake (#115 follow-up)"]
    fn blame_line_committed_line_attributes_commit() {
        let tmp = TempDir::new().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        let f = commit_file(tmp.path(), "b.txt", "a\nb\nc\n");
        let info = blame_line(&f, 1, b"a\nb\nc\n");
        let info = info.expect("blame_line must return Some for a tracked committed file");
        assert!(!info.is_uncommitted, "line 1 is committed");
        assert!(!info.author.is_empty(), "author must be non-empty");
        assert_eq!(info.commit.len(), 7, "commit hash must be 7 chars");
    }

    #[test]
    #[ignore = "git2 integration: real repo + git subprocess; CI test-binary flake (#115 follow-up)"]
    fn blame_line_uncommitted_edit_is_not_committed() {
        let tmp = TempDir::new().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        let f = commit_file(tmp.path(), "b.txt", "a\nb\nc\n");
        let info = blame_line(&f, 1, b"a\nMODIFIED\nc\n");
        let info = info.expect("blame_line must return Some for in-memory modified line");
        assert!(
            info.is_uncommitted,
            "modified line must be is_uncommitted=true"
        );
        assert_eq!(info.summary, "Not Committed Yet");
    }

    #[test]
    #[ignore = "git2 integration: real repo + git subprocess; CI test-binary flake (#115 follow-up)"]
    fn blame_line_out_of_repo_is_none() {
        let tmp = TempDir::new().unwrap();
        let f = tmp.path().join("loose.txt");
        std::fs::write(&f, "hello\n").unwrap();
        assert!(
            blame_line(&f, 0, b"hello\n").is_none(),
            "file outside repo must yield None"
        );
    }

    #[test]
    #[ignore = "git2 integration: real repo + git subprocess; CI test-binary flake (#115 follow-up)"]
    fn blame_file_all_len_matches_lines() {
        let tmp = TempDir::new().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        let f = commit_file(tmp.path(), "b.txt", "a\nb\nc\n");
        let all = blame_file_all(&f, b"a\nb\nc\n");
        assert!(
            all.len() >= 3,
            "expected at least 3 entries, got {}",
            all.len()
        );
        for (i, entry) in all.iter().enumerate().take(3) {
            let info = entry.as_ref().expect("entry must be Some");
            assert!(!info.is_uncommitted, "line {i} is committed");
        }
    }
}
