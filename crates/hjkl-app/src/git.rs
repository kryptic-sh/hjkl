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

use git2::{BlameOptions, DiffOptions, ErrorCode, Patch, Repository, StatusOptions};

// ── Explorer git status ────────────────────────────────────────────────────────

/// Coarse git status for a single file in the explorer tree.
///
/// Only one status is assigned per path; the precedence (checked in order
/// when multiple flags are set) is: Modified > Staged > Untracked > Deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExplorerGit {
    /// Worktree-modified or type-changed (not yet staged).
    Modified,
    /// Staged in the index (new, modified, renamed, or type-changed).
    Staged,
    /// New file in the worktree not tracked by git.
    Untracked,
    /// File deleted from worktree or index but not yet committed.
    Deleted,
}

/// Build a map of **absolute path → [`ExplorerGit`]** for all dirty paths
/// under `root`.
///
/// Returns an empty map when `root` is not inside a git repository (the gate
/// — no git2 calls are made after `Repository::discover` fails) or the repo
/// has no workdir (bare repo).
pub fn explorer_status_map(
    root: &Path,
) -> std::collections::HashMap<std::path::PathBuf, ExplorerGit> {
    try_explorer_status_map(root).unwrap_or_default()
}

fn try_explorer_status_map(
    root: &Path,
) -> Result<std::collections::HashMap<std::path::PathBuf, ExplorerGit>, git2::Error> {
    let repo = Repository::discover(root)?;
    let workdir = match repo.workdir() {
        Some(w) => w.to_path_buf(),
        None => return Ok(std::collections::HashMap::new()), // bare repo
    };

    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false);

    let statuses = repo.statuses(Some(&mut opts))?;
    let mut map = std::collections::HashMap::new();

    for entry in statuses.iter() {
        let rel = match entry.path() {
            Ok(p) => std::path::PathBuf::from(p),
            Err(_) => continue, // non-UTF-8 path — skip
        };
        let abs = workdir.join(&rel);
        let flags = entry.status();

        let kind = if flags.intersects(git2::Status::WT_DELETED | git2::Status::INDEX_DELETED)
            && !flags.intersects(
                git2::Status::WT_MODIFIED
                    | git2::Status::WT_TYPECHANGE
                    | git2::Status::WT_NEW
                    | git2::Status::INDEX_NEW
                    | git2::Status::INDEX_MODIFIED
                    | git2::Status::INDEX_TYPECHANGE
                    | git2::Status::INDEX_RENAMED,
            ) {
            // Pure deletion with no higher-precedence flag.
            ExplorerGit::Deleted
        } else if flags.intersects(git2::Status::WT_MODIFIED | git2::Status::WT_TYPECHANGE) {
            ExplorerGit::Modified
        } else if flags.contains(git2::Status::WT_NEW) {
            ExplorerGit::Untracked
        } else if flags.intersects(
            git2::Status::INDEX_NEW
                | git2::Status::INDEX_MODIFIED
                | git2::Status::INDEX_TYPECHANGE
                | git2::Status::INDEX_RENAMED,
        ) {
            ExplorerGit::Staged
        } else if flags.intersects(git2::Status::WT_DELETED | git2::Status::INDEX_DELETED) {
            // Deletion alongside a higher-priority flag already handled above;
            // reaching here means deletion is the only flag.
            ExplorerGit::Deleted
        } else {
            continue; // CURRENT / clean — skip
        };

        map.insert(abs, kind);
    }

    Ok(map)
}

/// Return the key form that [`explorer_status_map`] uses for `path`
/// (`workdir.join(rel)`), or `None` when `path` is not inside a git repo or
/// resolving fails.
///
/// Reuses [`open_repo_for`] so the key form is guaranteed to match the keys
/// produced by [`explorer_status_map`], letting callers merge overlay entries
/// into the base map without key-mismatch bugs.
pub fn explorer_key_for(path: &Path) -> Option<std::path::PathBuf> {
    let (repo, rel) = open_repo_for(path).ok()?;
    let workdir = repo.workdir()?;
    Some(workdir.join(rel))
}

/// Returns `true` when `path` (or its parent directory) is inside a git
/// repository. Used to gate per-keystroke git-sign jobs: when `false`, no
/// `Repository::statuses` / diff calls are made.
///
/// The result is cheap enough to compute once and cache on [`BufferSlot`].
pub fn path_in_repo(path: &Path) -> bool {
    let canon = match path.canonicalize() {
        Ok(c) => c,
        Err(_) => return false,
    };
    let parent = canon.parent().unwrap_or(Path::new("."));
    Repository::discover(parent).is_ok()
}

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

/// Build a unified diff (`@@`-style) between two in-memory byte buffers, with
/// no git repository required. `old` is the baseline (e.g. on-disk content),
/// `new` the comparison (e.g. the editor buffer); `old_label`/`new_label`
/// become the `--- a/<>` / `+++ b/<>` header paths. Returns the full patch
/// text, or `None` on a git2 error. Identical inputs yield `Some("")` (no diff).
///
/// Used by `:DiffOrig` (buffer vs disk) and reusable for the dirty-buffer
/// reload-diff prompt.
pub fn unified_diff(old: &[u8], new: &[u8], old_label: &str, new_label: &str) -> Option<String> {
    let mut opts = DiffOptions::new();
    opts.context_lines(3);
    let mut patch = Patch::from_buffers(
        old,
        Some(Path::new(old_label)),
        new,
        Some(Path::new(new_label)),
        Some(&mut opts),
    )
    .ok()?;
    let buf = patch.to_buf().ok()?;
    Some(buf.as_str().unwrap_or_default().to_string())
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
// Explorer git path operations (#Phase2a) — stage / unstage / discard / root.
// ---------------------------------------------------------------------------

/// Return the git workdir root for `path`, or `None` when `path` is not inside
/// a repository or the repo is bare.
///
/// This is the directory that should be passed as the `-C` argument to
/// `git -C <root> …` commands so they operate in the correct repo context.
pub fn repo_root(path: &Path) -> Option<std::path::PathBuf> {
    let (repo, _rel) = open_repo_for(path).ok()?;
    repo.workdir().map(|w| w.to_path_buf())
}

/// Run `git -C <root> <args...>` with no stdin.
///
/// Returns `Ok(())` on exit-code 0 and `Err(stderr)` otherwise.
fn run_git_cmd(root: &Path, args: &[&str]) -> Result<(), String> {
    use std::process::{Command, Stdio};

    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;

    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Stage `path` into the git index (`git -C <root> add -- <path>`).
///
/// Works for files, directories (stages all changes recursively), new
/// (untracked) files, and deletions. `root` should come from [`repo_root`].
pub fn stage_path(root: &Path, path: &Path) -> Result<(), String> {
    run_git_cmd(root, &["add", "--", path.to_str().unwrap_or("")])
}

/// Unstage `path` from the index (`git -C <root> reset -q -- <path>`).
///
/// Works with or without a HEAD commit (unborn branch). Does not touch
/// the worktree — staged changes return to worktree-modified status.
pub fn unstage_path(root: &Path, path: &Path) -> Result<(), String> {
    run_git_cmd(root, &["reset", "-q", "--", path.to_str().unwrap_or("")])
}

/// Discard worktree changes to `path` (`git -C <root> checkout -- <path>`).
///
/// Restores the file(s) from the index (or HEAD when the index matches HEAD).
/// Untracked files are never touched by this operation. For directories the
/// checkout is recursive over all tracked descendants.
pub fn discard_path(root: &Path, path: &Path) -> Result<(), String> {
    run_git_cmd(root, &["checkout", "--", path.to_str().unwrap_or("")])
}

// ---------------------------------------------------------------------------
// Commit flow (Phase 2b) — gc keybinding.
// ---------------------------------------------------------------------------

/// Run `git -C <root> commit --cleanup=strip -F <msg_file>`.
///
/// On success returns `Ok(stdout trimmed)` (e.g. `"[main 1a2b3c4] feat: x"`).
/// On non-zero exit returns `Err(stderr trimmed)`, falling back to stdout when
/// stderr is empty (git sometimes writes the abort message to stdout).
pub fn commit_with_file(root: &Path, msg_file: &Path) -> Result<String, String> {
    use std::process::{Command, Stdio};

    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("commit")
        .arg("--cleanup=strip")
        .arg("-F")
        .arg(msg_file)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;

    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Err(if !stderr.is_empty() { stderr } else { stdout })
    }
}

/// Resolve the absolute path to `COMMIT_EDITMSG` inside the git dir for `root`.
///
/// Runs `git -C root rev-parse --absolute-git-dir` to handle `.git`-as-file
/// worktrees correctly. Returns `None` when not in a repo or the command fails.
pub fn commit_edit_path(root: &Path) -> Option<std::path::PathBuf> {
    use std::process::{Command, Stdio};

    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("rev-parse")
        .arg("--absolute-git-dir")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;

    if !out.status.success() {
        return None;
    }

    let git_dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if git_dir.is_empty() {
        return None;
    }
    Some(std::path::PathBuf::from(git_dir).join("COMMIT_EDITMSG"))
}

/// Build the initial commit message template written to `COMMIT_EDITMSG`.
///
/// First line is empty (cursor lands here for the commit subject), followed by
/// a standard comment block, then the output of `git status --short --branch`
/// with each line prefixed by `# `. If the status command fails the section is
/// omitted. Trailing newline included.
pub fn commit_template(root: &Path) -> String {
    use std::process::{Command, Stdio};

    let mut out = String::new();
    out.push('\n');
    out.push_str("# Please enter the commit message for your changes. Lines starting\n");
    out.push_str("# with '#' will be ignored, and an empty message aborts the commit.\n");
    out.push_str("#\n");

    if let Ok(status_out) = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("status")
        .arg("--short")
        .arg("--branch")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        && status_out.status.success()
    {
        let text = String::from_utf8_lossy(&status_out.stdout);
        for line in text.lines() {
            out.push_str("# ");
            out.push_str(line);
            out.push('\n');
        }
    }

    out
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

    // ── ExplorerGit / path_in_repo ──────────────────────────────────────────

    #[test]
    fn explorer_key_for_non_repo_returns_none() {
        // A plain temp directory (no git repo) must yield None.
        let tmp = TempDir::new_in(std::env::temp_dir()).unwrap();
        let f = tmp.path().join("loose.txt");
        std::fs::write(&f, "hello\n").unwrap();
        assert!(
            explorer_key_for(&f).is_none(),
            "expected None for file outside any repo; got {:?}",
            explorer_key_for(&f)
        );
    }

    #[test]
    fn explorer_key_for_matches_status_map_key() {
        // In a real git repo, explorer_key_for of a modified file returns
        // the same key that explorer_status_map uses for that file.
        let tmp = TempDir::new_in(std::env::temp_dir()).unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);

        let f = tmp.path().join("tracked.txt");
        std::fs::write(&f, "original\n").unwrap();
        git(tmp.path(), &["add", "tracked.txt"]);
        git(tmp.path(), &["commit", "-q", "-m", "init"]);

        // Modify on disk so it appears in explorer_status_map.
        std::fs::write(&f, "modified\n").unwrap();

        let status_map = explorer_status_map(tmp.path());
        // The status map must contain the file's key.
        assert!(
            !status_map.is_empty(),
            "status map must be non-empty after edit"
        );

        let key = explorer_key_for(&f);
        assert!(
            key.is_some(),
            "explorer_key_for must return Some inside a repo"
        );
        let key = key.unwrap();
        assert!(
            status_map.contains_key(&key),
            "explorer_key_for key {key:?} must appear in explorer_status_map; map keys: {:?}",
            status_map.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn explorer_status_map_non_repo_returns_empty() {
        // A plain temp directory (no git repo) must return an empty map —
        // this is the no-repo gate.
        let tmp = TempDir::new_in(std::env::temp_dir()).unwrap();
        let map = explorer_status_map(tmp.path());
        assert!(
            map.is_empty(),
            "expected empty map for non-repo dir; got {map:?}"
        );
    }

    #[test]
    fn path_in_repo_non_repo_returns_false() {
        let tmp = TempDir::new_in(std::env::temp_dir()).unwrap();
        let f = tmp.path().join("loose.txt");
        std::fs::write(&f, "hello\n").unwrap();
        assert!(!path_in_repo(&f), "path outside any repo must return false");
    }

    #[test]
    fn explorer_status_map_classifies_files() {
        let tmp = TempDir::new_in(std::env::temp_dir()).unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);

        // committed file — will be modified
        let committed = tmp.path().join("committed.txt");
        std::fs::write(&committed, "original\n").unwrap();
        git(tmp.path(), &["add", "committed.txt"]);
        git(tmp.path(), &["commit", "-q", "-m", "init"]);

        // modify the committed file (worktree change)
        std::fs::write(&committed, "changed\n").unwrap();

        // untracked file
        let untracked = tmp.path().join("untracked.txt");
        std::fs::write(&untracked, "new\n").unwrap();

        // staged new file
        let staged = tmp.path().join("staged.txt");
        std::fs::write(&staged, "staged\n").unwrap();
        git(tmp.path(), &["add", "staged.txt"]);

        let map = explorer_status_map(tmp.path());

        // Look up by file name, not absolute path: `explorer_status_map` keys on
        // git2's `workdir` form, which is canonicalized (macOS `/var`→`/private`,
        // Windows separators/UNC) and so won't equal a raw `tmp.path().join(...)`.
        let _ = (&committed, &untracked, &staged);
        let by_name = |name: &str| -> Option<ExplorerGit> {
            map.iter().find_map(|(p, s)| {
                (p.file_name().and_then(|n| n.to_str()) == Some(name)).then_some(*s)
            })
        };
        assert_eq!(
            by_name("committed.txt"),
            Some(ExplorerGit::Modified),
            "worktree-modified file must be Modified; map: {map:?}"
        );
        assert_eq!(
            by_name("untracked.txt"),
            Some(ExplorerGit::Untracked),
            "new untracked file must be Untracked; map: {map:?}"
        );
        assert_eq!(
            by_name("staged.txt"),
            Some(ExplorerGit::Staged),
            "index-added file must be Staged; map: {map:?}"
        );
    }

    #[test]
    fn path_in_repo_returns_true_inside_repo() {
        let tmp = TempDir::new_in(std::env::temp_dir()).unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        let f = tmp.path().join("file.txt");
        std::fs::write(&f, "hello\n").unwrap();
        assert!(path_in_repo(&f), "file inside a repo must return true");
    }

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

    // ── stage_path / unstage_path / discard_path / repo_root ──────────────────

    #[test]
    fn stage_path_stages_modification() {
        let tmp = TempDir::new_in(std::env::temp_dir()).unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        let f = tmp.path().join("tracked.txt");
        std::fs::write(&f, "original\n").unwrap();
        git(tmp.path(), &["add", "tracked.txt"]);
        git(tmp.path(), &["commit", "-q", "-m", "init"]);

        // Worktree modification.
        std::fs::write(&f, "modified\n").unwrap();

        let root = repo_root(&f).expect("must be in repo");
        stage_path(&root, &f).expect("stage_path must succeed");

        // After staging, explorer_status_map must classify it as Staged.
        // Look up by file name (keys are git2's canonicalized workdir form).
        let map = explorer_status_map(tmp.path());
        let st = map.iter().find_map(|(p, s)| {
            (p.file_name().and_then(|n| n.to_str()) == Some("tracked.txt")).then_some(*s)
        });
        assert_eq!(
            st,
            Some(ExplorerGit::Staged),
            "file must be Staged after stage_path; map: {map:?}"
        );
    }

    #[test]
    fn unstage_path_unstages() {
        let tmp = TempDir::new_in(std::env::temp_dir()).unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        let f = tmp.path().join("tracked.txt");
        std::fs::write(&f, "original\n").unwrap();
        git(tmp.path(), &["add", "tracked.txt"]);
        git(tmp.path(), &["commit", "-q", "-m", "init"]);

        // Modify then stage.
        std::fs::write(&f, "modified\n").unwrap();
        git(tmp.path(), &["add", "tracked.txt"]);

        let root = repo_root(&f).expect("must be in repo");
        unstage_path(&root, &f).expect("unstage_path must succeed");

        // After unstaging, file must be worktree-Modified (not Staged).
        // Look up by file name (keys are git2's canonicalized workdir form).
        let map = explorer_status_map(tmp.path());
        let st = map.iter().find_map(|(p, s)| {
            (p.file_name().and_then(|n| n.to_str()) == Some("tracked.txt")).then_some(*s)
        });
        assert_eq!(
            st,
            Some(ExplorerGit::Modified),
            "file must be Modified after unstage_path; map: {map:?}"
        );
    }

    #[test]
    fn discard_path_restores_worktree() {
        let tmp = TempDir::new_in(std::env::temp_dir()).unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        // Disable autocrlf so Windows checkout doesn't rewrite `\n` → `\r\n`.
        git(tmp.path(), &["config", "core.autocrlf", "false"]);
        let f = tmp.path().join("tracked.txt");
        std::fs::write(&f, "original\n").unwrap();
        git(tmp.path(), &["add", "tracked.txt"]);
        git(tmp.path(), &["commit", "-q", "-m", "init"]);

        // Dirty the worktree.
        std::fs::write(&f, "dirty\n").unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "dirty\n");

        let root = repo_root(&f).expect("must be in repo");
        discard_path(&root, &f).expect("discard_path must succeed");

        // File content must be restored to the committed version.
        let content = std::fs::read_to_string(&f).unwrap();
        assert_eq!(
            content, "original\n",
            "discard_path must restore committed content; got {content:?}"
        );
    }

    #[test]
    fn stage_path_outside_repo_errs() {
        let tmp = TempDir::new_in(std::env::temp_dir()).unwrap();
        // Not a git repo — repo_root returns None, but we can still call
        // stage_path with the tmp dir as a fake root and confirm Err.
        let fake_root = tmp.path().to_path_buf();
        let fake_file = tmp.path().join("nope.txt");
        std::fs::write(&fake_file, "x\n").unwrap();
        let result = stage_path(&fake_root, &fake_file);
        assert!(
            result.is_err(),
            "stage_path outside a repo must return Err; got Ok(())"
        );
    }

    // ── commit_with_file / commit_edit_path / commit_template ────────────────

    #[test]
    fn commit_with_file_real_message_commits() {
        let tmp = TempDir::new_in(std::env::temp_dir()).unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        // Repo-local identity so commit_with_file's plain `git commit` succeeds
        // on CI runners that have no global git config.
        git(tmp.path(), &["config", "user.email", "t@t.com"]);
        git(tmp.path(), &["config", "user.name", "T"]);
        // Need at least one commit so HEAD exists.
        let f = tmp.path().join("a.txt");
        std::fs::write(&f, "original\n").unwrap();
        git(tmp.path(), &["add", "a.txt"]);
        git(tmp.path(), &["commit", "-q", "-m", "init"]);

        // Stage a change.
        std::fs::write(&f, "modified\n").unwrap();
        git(tmp.path(), &["add", "a.txt"]);

        // Write a real commit message to a temp file.
        let msg_file = tmp.path().join("COMMIT_EDITMSG");
        std::fs::write(&msg_file, "feat: x\n").unwrap();

        let root = repo_root(&f).expect("must be in repo");
        let result = commit_with_file(&root, &msg_file);
        assert!(result.is_ok(), "commit_with_file must succeed: {result:?}");

        // Verify the commit subject.
        let log_out = Command::new("git")
            .args(["log", "-1", "--format=%s"])
            .current_dir(tmp.path())
            .output()
            .expect("git log");
        let subject = String::from_utf8_lossy(&log_out.stdout).trim().to_string();
        assert_eq!(
            subject, "feat: x",
            "commit subject must match; got {subject:?}"
        );
    }

    #[test]
    fn commit_with_file_empty_message_aborts() {
        let tmp = TempDir::new_in(std::env::temp_dir()).unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        let f = tmp.path().join("a.txt");
        std::fs::write(&f, "original\n").unwrap();
        git(tmp.path(), &["add", "a.txt"]);
        git(
            tmp.path(),
            &[
                "-c",
                "user.email=t@t.com",
                "-c",
                "user.name=T",
                "commit",
                "-q",
                "-m",
                "init",
            ],
        );

        // Stage a change.
        std::fs::write(&f, "changed\n").unwrap();
        git(tmp.path(), &["add", "a.txt"]);

        // Write only comment / blank lines — git strips these → empty message → abort.
        let msg_file = tmp.path().join("COMMIT_EDITMSG");
        std::fs::write(&msg_file, "# this is a comment\n\n# another comment\n").unwrap();

        let root = repo_root(&f).expect("must be in repo");
        let result = commit_with_file(&root, &msg_file);
        assert!(
            result.is_err(),
            "commit_with_file with blank message must return Err; got Ok({:?})",
            result.ok()
        );
    }

    #[test]
    fn commit_edit_path_in_repo_returns_some() {
        let tmp = TempDir::new_in(std::env::temp_dir()).unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        let root = tmp.path().to_path_buf();
        let path = commit_edit_path(&root);
        assert!(
            path.is_some(),
            "commit_edit_path must return Some inside a repo"
        );
        let p = path.unwrap();
        assert!(
            p.file_name()
                .map(|n| n == "COMMIT_EDITMSG")
                .unwrap_or(false),
            "path must end with COMMIT_EDITMSG; got {p:?}"
        );
    }

    #[test]
    fn commit_edit_path_outside_repo_returns_none() {
        let tmp = TempDir::new_in(std::env::temp_dir()).unwrap();
        // Not a git repo.
        let path = commit_edit_path(tmp.path());
        assert!(
            path.is_none(),
            "commit_edit_path must return None outside a repo; got {path:?}"
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

    // ── unified_diff (#208) ─────────────────────────────────────────────────

    #[test]
    fn unified_diff_emits_hunk_for_changed_line() {
        let old = b"alpha\nbeta\ngamma\n";
        let new = b"alpha\nBETA\ngamma\n";
        let d = unified_diff(old, new, "a/f.txt", "b/f.txt").expect("diff");
        assert!(d.contains("@@"), "expected hunk header: {d}");
        assert!(d.contains("-beta"), "expected removed line: {d}");
        assert!(d.contains("+BETA"), "expected added line: {d}");
    }

    #[test]
    fn unified_diff_empty_for_identical_buffers() {
        let buf = b"same\ncontent\n";
        let d = unified_diff(buf, buf, "a/f.txt", "b/f.txt").expect("diff");
        assert!(
            d.trim().is_empty(),
            "identical buffers must produce an empty diff, got: {d:?}"
        );
    }

    #[test]
    fn unified_diff_handles_creation_from_empty() {
        let d = unified_diff(b"", b"new\nfile\n", "a/f.txt", "b/f.txt").expect("diff");
        assert!(d.contains("+new"), "expected added content: {d}");
        assert!(d.contains("+file"), "expected added content: {d}");
    }
}
