//! Pure reconcile engine for the oil.nvim-style editable file explorer.
//!
//! [`reconcile`] diffs an edited explorer buffer against a baseline snapshot
//! and returns an ordered [`Vec<FsOp>`] that, when applied in order, makes the
//! filesystem match the buffer.  **No filesystem access occurs here** — this is
//! a pure function suitable for exhaustive unit testing before the wiring phase.
//!
//! # Buffer format
//! Each line is `<indent spaces><name>` where `indent = depth * 2 + 2`.
//! Line 0 is the root directory (not an editable target).  Directories MAY be
//! written with a trailing `/`.  Names may contain internal slashes for nested
//! creation (e.g. `a/b.rs`).
//!
//! # Baseline
//! An ordered `Vec<(PathBuf, bool)>` — `(absolute path, is_dir)` per line,
//! index 0 = root.  Produced by [`crate::app::explorer::ExplorerTree`] and
//! snapshotted when the buffer is built.

use std::path::{Path, PathBuf};

// ── Op model ──────────────────────────────────────────────────────────────────

/// A single filesystem operation produced by [`reconcile`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FsOp {
    /// Create a directory (and any intermediate parents — the wiring phase uses
    /// `create_dir_all`).
    CreateDir(PathBuf),
    /// Create an empty file (the wiring phase uses `mkdir -p` on the parent
    /// first so that `a/b.rs` works when `a/` is new).
    CreateFile(PathBuf),
    /// Move the entry at `from` into the trash directory (see
    /// `crate::app::trash`).  Never a physical delete.
    Trash(PathBuf),
    /// Rename / move `from` to `to`.  Only emitted when `from` and `to` are
    /// the same node-type (both dirs or both files).
    Rename { from: PathBuf, to: PathBuf },
}

// ── Buffer parser ─────────────────────────────────────────────────────────────

/// One parsed entry from the buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BufEntry {
    path: PathBuf,
    is_dir: bool,
}

/// Parse `buffer` (the current bare explorer buffer text) into an ordered list
/// of entries, resolving absolute paths using `root`.  Line 0 (the root dir
/// line) is skipped.
fn parse_buffer(buffer: &str, root: &Path) -> Vec<BufEntry> {
    // depth → absolute dir path for parent resolution.
    // We store (depth, path) pairs; the stack grows as we descend into dirs.
    let mut stack: Vec<(usize, PathBuf)> = Vec::new();
    let mut entries: Vec<BufEntry> = Vec::new();

    for (line_idx, line) in buffer.lines().enumerate() {
        // Skip line 0 — root dir header.
        if line_idx == 0 {
            continue;
        }

        // Skip blank / whitespace-only lines.
        let trimmed = line.trim_end();
        if trimmed.trim_start().is_empty() {
            continue;
        }

        // Count leading ASCII spaces for indent.
        let indent = trimmed.len() - trimmed.trim_start_matches(' ').len();

        // depth = (indent - 2) / 2, clamped to ≥ 1.
        // name_col = depth * 2 + 2  ⇒  depth = (indent - 2) / 2
        let depth = ((indent.saturating_sub(2)) / 2).max(1);

        let raw = trimmed[indent..].trim_end();
        let is_dir = raw.ends_with('/');
        let name = raw.trim_end_matches('/');
        if name.is_empty() {
            continue;
        }

        // Pop stack entries that are at depth ≥ current depth (they are
        // siblings or ancestors-once-removed, not parents of this entry).
        while stack.last().map(|(d, _)| *d >= depth).unwrap_or(false) {
            stack.pop();
        }

        // Parent: top of stack if its depth == depth - 1, else root.
        let parent = stack
            .last()
            .filter(|(d, _)| *d == depth - 1)
            .map(|(_, p)| p.as_path())
            .unwrap_or(root);

        // `Path::join` handles internal slashes in `name` (e.g. "a/b.rs").
        let target = parent.join(name);

        if is_dir {
            stack.push((depth, target.clone()));
        }

        entries.push(BufEntry {
            path: target,
            is_dir,
        });
    }

    entries
}

// ── LCS alignment ─────────────────────────────────────────────────────────────

/// Standard O(m*n) LCS over (path, is_dir) sequences.  Two entries match only
/// when both the path AND the is_dir flag are equal — a type change (file → dir
/// or vice versa) at the same path is treated as a deletion + creation, not an
/// unchanged entry.
///
/// Returns the list of matched index-pairs `(baseline_idx, current_idx)` in
/// ascending order.
fn lcs_paths(baseline: &[(PathBuf, bool)], current: &[BufEntry]) -> Vec<(usize, usize)> {
    let m = baseline.len();
    let n = current.len();

    if m == 0 || n == 0 {
        return Vec::new();
    }

    // dp[i][j] = LCS length for baseline[..i] vs current[..j]
    let mut dp = vec![0u32; (m + 1) * (n + 1)];

    for i in 1..=m {
        for j in 1..=n {
            // Match only when BOTH path and is_dir agree.
            if baseline[i - 1].0 == current[j - 1].path
                && baseline[i - 1].1 == current[j - 1].is_dir
            {
                dp[i * (n + 1) + j] = dp[(i - 1) * (n + 1) + (j - 1)] + 1;
            } else {
                let up = dp[(i - 1) * (n + 1) + j];
                let left = dp[i * (n + 1) + (j - 1)];
                dp[i * (n + 1) + j] = up.max(left);
            }
        }
    }

    // Back-trace to recover the matched pairs.
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    let mut i = m;
    let mut j = n;
    while i > 0 && j > 0 {
        if baseline[i - 1].0 == current[j - 1].path && baseline[i - 1].1 == current[j - 1].is_dir {
            pairs.push((i - 1, j - 1));
            i -= 1;
            j -= 1;
        } else if dp[(i - 1) * (n + 1) + j] >= dp[i * (n + 1) + (j - 1)] {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    pairs.reverse();
    pairs
}

// ── Gap processor ─────────────────────────────────────────────────────────────

/// Process one alignment gap: `bgap` = baseline-only entries, `cgap` =
/// current-only entries.  Appends ops to the output collections.
fn process_gap(
    bgap: &[(PathBuf, bool)],
    cgap: &[BufEntry],
    renames: &mut Vec<FsOp>,
    trashes: &mut Vec<FsOp>,
    creates: &mut Vec<FsOp>,
) {
    let paired = bgap.len().min(cgap.len());

    for i in 0..paired {
        let (bpath, b_is_dir) = &bgap[i];
        let centry = &cgap[i];

        if *b_is_dir == centry.is_dir {
            // Same type → rename (paths differ; identical paths caught by LCS).
            if bpath != &centry.path {
                renames.push(FsOp::Rename {
                    from: bpath.clone(),
                    to: centry.path.clone(),
                });
            }
        } else {
            // Type mismatch → trash + create.
            trashes.push(FsOp::Trash(bpath.clone()));
            if centry.is_dir {
                creates.push(FsOp::CreateDir(centry.path.clone()));
            } else {
                creates.push(FsOp::CreateFile(centry.path.clone()));
            }
        }
    }

    // Leftover baseline entries → trash.
    for (bpath, _) in bgap.iter().skip(paired) {
        trashes.push(FsOp::Trash(bpath.clone()));
    }

    // Leftover current entries → create.
    for centry in cgap.iter().skip(paired) {
        if centry.is_dir {
            creates.push(FsOp::CreateDir(centry.path.clone()));
        } else {
            creates.push(FsOp::CreateFile(centry.path.clone()));
        }
    }
}

// ── Component count helpers ───────────────────────────────────────────────────

fn component_count(p: &Path) -> usize {
    p.components().count()
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Diff the edited explorer buffer against `baseline` and return the ordered
/// filesystem ops required to make disk match the buffer.
///
/// **Pure** — no filesystem access.  Suitable for testing in isolation.
///
/// # Arguments
/// - `baseline`: `(abs_path, is_dir)` per line; index 0 = root (ignored by the diff).
/// - `buffer`:   current bare buffer text (line 0 = root header).
/// - `root`:     explorer root == `baseline[0].0`.
///
/// # Ordering
/// 1. Renames, sorted by `from` component count **ascending** (shallow → deep).
/// 2. Trashes, sorted by path component count **descending** (deep → shallow,
///    children before parents).
/// 3. Creates, sorted by path component count **ascending** (parents before
///    children).
pub(crate) fn reconcile(baseline: &[(PathBuf, bool)], buffer: &str, root: &Path) -> Vec<FsOp> {
    // baseline[1..] is the diffable slice (skip root).
    let b_slice: &[(PathBuf, bool)] = if baseline.is_empty() {
        &[]
    } else {
        &baseline[1..]
    };

    let current = parse_buffer(buffer, root);

    // Compute LCS between baseline[1..] and current.
    let matched_pairs = lcs_paths(b_slice, &current);

    // Walk the alignment, extracting gaps between consecutive matched pairs.
    let mut renames: Vec<FsOp> = Vec::new();
    let mut trashes: Vec<FsOp> = Vec::new();
    let mut creates: Vec<FsOp> = Vec::new();

    let mut prev_b: usize = 0; // exclusive start in b_slice
    let mut prev_c: usize = 0; // exclusive start in current

    for &(bi, ci) in matched_pairs.iter() {
        // Gap: b_slice[prev_b..bi] vs current[prev_c..ci]
        let bgap: Vec<(PathBuf, bool)> = b_slice[prev_b..bi]
            .iter()
            .map(|(p, d)| (p.clone(), *d))
            .collect();
        let cgap = &current[prev_c..ci];
        process_gap(&bgap, cgap, &mut renames, &mut trashes, &mut creates);

        prev_b = bi + 1;
        prev_c = ci + 1;
    }

    // Trailing gap after last anchor.
    {
        let bgap: Vec<(PathBuf, bool)> = b_slice[prev_b..]
            .iter()
            .map(|(p, d)| (p.clone(), *d))
            .collect();
        let cgap = &current[prev_c..];
        process_gap(&bgap, cgap, &mut renames, &mut trashes, &mut creates);
    }

    // ── Sort per ordering rules ───────────────────────────────────────────────

    // Renames: ascending by `from` component count (shallow → deep).
    renames.sort_by_key(|op| {
        if let FsOp::Rename { from, .. } = op {
            component_count(from)
        } else {
            0
        }
    });

    // Trashes: descending by component count (deep → shallow).
    trashes.sort_by_key(|op| {
        if let FsOp::Trash(p) = op {
            std::cmp::Reverse(component_count(p))
        } else {
            std::cmp::Reverse(0)
        }
    });

    // Creates: ascending by component count (parents before children).
    creates.sort_by_key(|op| match op {
        FsOp::CreateDir(p) | FsOp::CreateFile(p) => component_count(p),
        _ => 0,
    });

    // Final order: renames, trashes, creates.
    let mut ops = renames;
    ops.extend(trashes);
    ops.extend(creates);
    ops
}

// ── Applied-op journal ────────────────────────────────────────────────────────

/// A concrete action that was carried out by [`apply_ops`].
///
/// Carries enough information to reverse the action (undo) and, from the
/// undo side, to re-apply it (redo).  The undo / redo path is implemented by
/// [`revert_ops`].
#[derive(Debug, Clone)]
pub(crate) enum AppliedOp {
    /// A new file or directory was created at `path`.
    /// Reverse: trash it.
    Created(PathBuf),
    /// A file or directory at `original` was moved to the trash at `dest`.
    /// Reverse: move `dest` back to `original`.
    Trashed { original: PathBuf, dest: PathBuf },
    /// A file or directory was renamed / moved from `from` to `to`.
    /// Reverse: rename `to` back to `from`.
    Renamed { from: PathBuf, to: PathBuf },
    /// A trashed entry at `from_trash` was restored to `to` (the
    /// trash-restore branch of CreateFile).
    /// Reverse: trash `to` again.
    Restored { from_trash: PathBuf, to: PathBuf },
}

// ── Filesystem application ────────────────────────────────────────────────────

/// Move a file across device boundaries: `fs::rename` first; on `CrossesDevices`
/// fall back to `fs::copy` + `fs::remove_file`.
fn move_file(src: &Path, dst: &Path) -> std::io::Result<()> {
    match std::fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
            std::fs::copy(src, dst)?;
            std::fs::remove_file(src)?;
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Move a directory tree across filesystem boundaries: `fs::rename` first; on
/// `CrossesDevices` fall back to a recursive copy followed by `remove_dir_all`.
fn move_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    match std::fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
            let mut stack = vec![src.to_path_buf()];
            while let Some(dir) = stack.pop() {
                let rel = dir
                    .strip_prefix(src)
                    .map_err(|_| std::io::Error::other("strip_prefix failed"))?;
                let dst_dir = dst.join(rel);
                std::fs::create_dir_all(&dst_dir)?;
                for entry in std::fs::read_dir(&dir)?.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        stack.push(path);
                    } else {
                        let rel_file = path
                            .strip_prefix(src)
                            .map_err(|_| std::io::Error::other("strip_prefix failed"))?;
                        std::fs::copy(&path, dst.join(rel_file))?;
                    }
                }
            }
            std::fs::remove_dir_all(src)?;
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Apply reconcile ops to disk. Deletions go to the trash (recoverable);
/// a `CreateFile` whose basename matches a pending trashed entry is **restored**
/// from trash instead of created empty (this is how `dd` then `p` becomes a
/// move). Returns the paths of genuinely newly-created FILES (to open), the
/// concrete [`AppliedOp`] journal entries (for undo/redo), and a list of error
/// strings from non-fatal op failures (best-effort).
///
/// # Arguments
/// - `ops`: output of [`reconcile`], already in renames→trashes→creates order.
/// - `trashed`: mutable registry of `(original_file_name, trash_dest)` pairs
///   built up by this call and carried across reconcile cycles so that a
///   `Trash` on tick N and a `CreateFile` on tick N+1 correctly restores.
pub(crate) fn apply_ops(
    ops: &[FsOp],
    trashed: &mut Vec<(String, PathBuf)>,
) -> (Vec<PathBuf>, Vec<AppliedOp>, Vec<String>) {
    let mut created: Vec<PathBuf> = Vec::new();
    let mut applied: Vec<AppliedOp> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    for op in ops {
        match op {
            FsOp::Rename { from, to } => {
                // Skip if the source is already gone AND the dest already
                // exists — an ancestor directory rename already moved it.
                if !from.exists() && to.exists() {
                    continue;
                }
                if let Some(parent) = to.parent()
                    && let Err(e) = std::fs::create_dir_all(parent)
                {
                    errors.push(format!("rename: create_dir_all({parent:?}): {e}"));
                    continue;
                }
                let result = if from.is_dir() {
                    move_dir(from, to)
                } else {
                    move_file(from, to)
                };
                match result {
                    Ok(()) => {
                        applied.push(AppliedOp::Renamed {
                            from: from.clone(),
                            to: to.clone(),
                        });
                    }
                    Err(e) => {
                        errors.push(format!("rename {from:?} → {to:?}: {e}"));
                    }
                }
            }

            FsOp::Trash(path) => {
                let dest = match hjkl_app::trash::trash_path(path) {
                    Ok(d) => d,
                    Err(e) => {
                        errors.push(format!("trash_path({path:?}): {e}"));
                        continue;
                    }
                };
                let result = if path.is_dir() {
                    move_dir(path, &dest)
                } else {
                    move_file(path, &dest)
                };
                match result {
                    Ok(()) => {
                        let name = path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        trashed.push((name, dest.clone()));
                        applied.push(AppliedOp::Trashed {
                            original: path.clone(),
                            dest,
                        });
                    }
                    Err(e) => {
                        errors.push(format!("trash {path:?}: {e}"));
                    }
                }
            }

            FsOp::CreateDir(path) => {
                if let Err(e) = std::fs::create_dir_all(path) {
                    errors.push(format!("create_dir_all({path:?}): {e}"));
                } else {
                    applied.push(AppliedOp::Created(path.clone()));
                }
            }

            FsOp::CreateFile(path) => {
                // Check whether a trashed entry can be restored here.
                let file_name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();

                // Find the most-recent trashed entry whose name matches.
                let restore_idx = trashed
                    .iter()
                    .enumerate()
                    .rev()
                    .find(|(_, (name, _))| name == &file_name)
                    .map(|(i, _)| i);

                if let Some(parent) = path.parent()
                    && let Err(e) = std::fs::create_dir_all(parent)
                {
                    errors.push(format!("create_file: create_dir_all({parent:?}): {e}"));
                    continue;
                }

                if let Some(idx) = restore_idx {
                    let (_, trash_dest) = trashed.remove(idx);
                    // Restore from trash.
                    match move_file(&trash_dest, path) {
                        Ok(()) => {
                            applied.push(AppliedOp::Restored {
                                from_trash: trash_dest,
                                to: path.clone(),
                            });
                        }
                        Err(e) => {
                            errors.push(format!("restore {trash_dest:?} → {path:?}: {e}"));
                        }
                    }
                    // Not a new file — do NOT add to `created`.
                } else {
                    match std::fs::File::create(path) {
                        Ok(_) => {
                            created.push(path.clone());
                            applied.push(AppliedOp::Created(path.clone()));
                        }
                        Err(e) => {
                            errors.push(format!("create_file({path:?}): {e}"));
                        }
                    }
                }
            }
        }
    }

    (created, applied, errors)
}

/// Reverse a slice of [`AppliedOp`]s **in reverse order** (last op undone first).
///
/// Each reversal is applied to the filesystem immediately.  The function
/// returns a new `Vec<AppliedOp>` that, when passed to [`apply_applied`],
/// re-performs the original forward actions (i.e. the redo journal).
///
/// `trashed` is the pane's trash registry; it is updated as new trash
/// destinations are created during the reversal.
pub(crate) fn revert_ops(
    applied: &[AppliedOp],
    trashed: &mut Vec<(String, PathBuf)>,
) -> (Vec<AppliedOp>, Vec<String>) {
    let mut redo_journal: Vec<AppliedOp> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    // Reverse-iterate so the last op is undone first (symmetrical with apply
    // order: if we created dir/file in order A→B, undo is B→A).
    for op in applied.iter().rev() {
        match op {
            // A file/dir was created → trash it to undo.
            AppliedOp::Created(path) => {
                let dest = match hjkl_app::trash::trash_path(path) {
                    Ok(d) => d,
                    Err(e) => {
                        errors.push(format!("revert created: trash_path({path:?}): {e}"));
                        continue;
                    }
                };
                let result = if path.is_dir() {
                    move_dir(path, &dest)
                } else {
                    move_file(path, &dest)
                };
                match result {
                    Ok(()) => {
                        let name = path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        trashed.push((name, dest.clone()));
                        // Redo = restore from this new trash dest back to path.
                        redo_journal.push(AppliedOp::Restored {
                            from_trash: dest,
                            to: path.clone(),
                        });
                    }
                    Err(e) => {
                        errors.push(format!("revert created: trash {path:?}: {e}"));
                    }
                }
            }

            // A file/dir was trashed → move it back from trash to original.
            AppliedOp::Trashed { original, dest } => {
                if let Some(parent) = original.parent()
                    && let Err(e) = std::fs::create_dir_all(parent)
                {
                    errors.push(format!("revert trashed: create_dir_all({parent:?}): {e}"));
                    continue;
                }
                let result = if dest.is_dir() {
                    move_dir(dest, original)
                } else {
                    move_file(dest, original)
                };
                match result {
                    Ok(()) => {
                        // Remove the now-restored entry from the trashed registry
                        // (it's back on disk).
                        if let Some(pos) = trashed.iter().position(|(_, d)| d == dest) {
                            trashed.remove(pos);
                        }
                        // Redo = trash original again (fresh dest computed at redo time).
                        redo_journal.push(AppliedOp::Trashed {
                            original: original.clone(),
                            dest: dest.clone(),
                        });
                    }
                    Err(e) => {
                        errors.push(format!(
                            "revert trashed: restore {dest:?} → {original:?}: {e}"
                        ));
                    }
                }
            }

            // A rename from→to → rename back to→from.
            AppliedOp::Renamed { from, to } => {
                if let Some(parent) = from.parent()
                    && let Err(e) = std::fs::create_dir_all(parent)
                {
                    errors.push(format!("revert renamed: create_dir_all({parent:?}): {e}"));
                    continue;
                }
                let result = if to.is_dir() {
                    move_dir(to, from)
                } else {
                    move_file(to, from)
                };
                match result {
                    Ok(()) => {
                        // Redo = rename from→to again.
                        redo_journal.push(AppliedOp::Renamed {
                            from: from.clone(),
                            to: to.clone(),
                        });
                    }
                    Err(e) => {
                        errors.push(format!("revert renamed: {to:?} → {from:?}: {e}"));
                    }
                }
            }

            // A trashed entry was restored to `to` → trash `to` again.
            AppliedOp::Restored { from_trash: _, to } => {
                let new_dest = match hjkl_app::trash::trash_path(to) {
                    Ok(d) => d,
                    Err(e) => {
                        errors.push(format!("revert restored: trash_path({to:?}): {e}"));
                        continue;
                    }
                };
                let result = if to.is_dir() {
                    move_dir(to, &new_dest)
                } else {
                    move_file(to, &new_dest)
                };
                match result {
                    Ok(()) => {
                        let name = to
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        trashed.push((name, new_dest.clone()));
                        // Redo = restore from this new trash dest back to `to`.
                        redo_journal.push(AppliedOp::Restored {
                            from_trash: new_dest,
                            to: to.clone(),
                        });
                    }
                    Err(e) => {
                        errors.push(format!("revert restored: trash {to:?}: {e}"));
                    }
                }
            }
        }
    }

    // The redo journal was built in undo order (reversed). Reverse it once more
    // so that re-applying it (via apply_applied) repeats the original forward
    // order.
    redo_journal.reverse();
    (redo_journal, errors)
}

/// Re-apply a set of [`AppliedOp`]s that were produced by [`revert_ops`] as the
/// "redo" journal. This is the forward direction of redo.
///
/// Returns the newly-created file paths (for opening) and any errors.
pub(crate) fn apply_applied(
    ops: &[AppliedOp],
    trashed: &mut Vec<(String, PathBuf)>,
) -> (Vec<PathBuf>, Vec<AppliedOp>, Vec<String>) {
    let mut created: Vec<PathBuf> = Vec::new();
    let mut new_applied: Vec<AppliedOp> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    for op in ops {
        match op {
            AppliedOp::Created(path) => {
                // Re-create the file/dir.
                let result = if path.extension().is_none() && !path.to_string_lossy().contains('.')
                {
                    // Heuristic: no extension → treat as dir.  But the journal
                    // knows exactly what was created; we don't have is_dir info
                    // here.  Safe fallback: try create_file first; if the path
                    // was a dir the Restored/Trashed variant would have been used.
                    std::fs::File::create(path).map(|_| ())
                } else {
                    std::fs::File::create(path).map(|_| ())
                };
                match result {
                    Ok(()) => {
                        created.push(path.clone());
                        new_applied.push(AppliedOp::Created(path.clone()));
                    }
                    Err(e) => {
                        errors.push(format!("redo created: create {path:?}: {e}"));
                    }
                }
            }

            AppliedOp::Trashed { original, dest: _ } => {
                // Re-trash: original should be back on disk (from the undo).
                // Compute a fresh trash dest.
                let new_dest = match hjkl_app::trash::trash_path(original) {
                    Ok(d) => d,
                    Err(e) => {
                        errors.push(format!("redo trashed: trash_path({original:?}): {e}"));
                        continue;
                    }
                };
                let result = if original.is_dir() {
                    move_dir(original, &new_dest)
                } else {
                    move_file(original, &new_dest)
                };
                match result {
                    Ok(()) => {
                        let name = original
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        trashed.push((name, new_dest.clone()));
                        new_applied.push(AppliedOp::Trashed {
                            original: original.clone(),
                            dest: new_dest,
                        });
                    }
                    Err(e) => {
                        errors.push(format!("redo trashed: trash {original:?}: {e}"));
                    }
                }
            }

            AppliedOp::Renamed { from, to } => {
                if let Some(parent) = to.parent()
                    && let Err(e) = std::fs::create_dir_all(parent)
                {
                    errors.push(format!("redo renamed: create_dir_all({parent:?}): {e}"));
                    continue;
                }
                let result = if from.is_dir() {
                    move_dir(from, to)
                } else {
                    move_file(from, to)
                };
                match result {
                    Ok(()) => {
                        new_applied.push(AppliedOp::Renamed {
                            from: from.clone(),
                            to: to.clone(),
                        });
                    }
                    Err(e) => {
                        errors.push(format!("redo renamed: {from:?} → {to:?}: {e}"));
                    }
                }
            }

            AppliedOp::Restored { from_trash, to } => {
                if let Some(parent) = to.parent()
                    && let Err(e) = std::fs::create_dir_all(parent)
                {
                    errors.push(format!("redo restored: create_dir_all({parent:?}): {e}"));
                    continue;
                }
                match move_file(from_trash, to) {
                    Ok(()) => {
                        // Remove from the trashed registry.
                        if let Some(pos) = trashed.iter().position(|(_, d)| d == from_trash) {
                            trashed.remove(pos);
                        }
                        new_applied.push(AppliedOp::Restored {
                            from_trash: from_trash.clone(),
                            to: to.clone(),
                        });
                    }
                    Err(e) => {
                        errors.push(format!("redo restored: {from_trash:?} → {to:?}: {e}"));
                    }
                }
            }
        }
    }

    (created, new_applied, errors)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Helper: render a baseline (index 0 = root) to the bare buffer text that
    /// `reconcile` expects.  The root line has depth 0 → indent = 0*2+2 = 2
    /// spaces.  Dirs are rendered WITH a trailing `/` so the parser pushes them
    /// onto the directory stack and children resolve under them correctly.
    fn render_baseline(baseline: &[(PathBuf, bool)]) -> String {
        let mut out = String::new();
        for (i, (path, is_dir)) in baseline.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            // Infer depth from path component count relative to root (baseline[0]).
            let root = &baseline[0].0;
            let depth = match path.strip_prefix(root) {
                Ok(rel) => rel.components().count(),
                Err(_) => 0,
            };
            let indent = depth * 2 + 2;
            let name = if depth == 0 {
                path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_string_lossy().into_owned())
            } else {
                path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            };
            out.push_str(&" ".repeat(indent));
            out.push_str(&name);
            // Append `/` for non-root dirs so the parser pushes them onto the
            // directory stack and children resolve correctly.
            if *is_dir && depth > 0 {
                out.push('/');
            }
        }
        out
    }

    // Convenience root.
    fn root() -> PathBuf {
        PathBuf::from("/project")
    }

    // Build a baseline from a list of (relative_path, is_dir) pairs.
    // First entry is always the root.
    fn make_baseline(items: &[(&str, bool)]) -> Vec<(PathBuf, bool)> {
        let r = root();
        let mut v: Vec<(PathBuf, bool)> = Vec::new();
        v.push((r.clone(), true)); // root
        for (rel, is_dir) in items {
            v.push((r.join(rel), *is_dir));
        }
        v
    }

    // ── bulk_create ───────────────────────────────────────────────────────────

    /// baseline has root + 1 file; buffer adds 3 new sibling lines ⇒ 3 ops.
    /// The existing file is at position 0 in baseline and position 0 in the
    /// buffer.  The 3 new files are appended at positions 1–3; they are all
    /// cgap entries and become CreateFile ops.
    #[test]
    fn bulk_create() {
        let baseline = make_baseline(&[("existing.rs", false)]);
        // Buffer: root line + existing.rs + 3 new files
        let buffer = "  project\n    existing.rs\n    new_a.rs\n    new_b.rs\n    new_c.rs";
        let ops = reconcile(&baseline, buffer, &root());
        // existing.rs is unchanged (LCS match).
        // new_a, new_b, new_c are creates.
        assert_eq!(ops.len(), 3, "expected 3 creates, got {ops:?}");
        assert!(ops.contains(&FsOp::CreateFile(root().join("new_a.rs"))));
        assert!(ops.contains(&FsOp::CreateFile(root().join("new_b.rs"))));
        assert!(ops.contains(&FsOp::CreateFile(root().join("new_c.rs"))));
        // No trashes or renames.
        assert!(
            ops.iter().all(|op| matches!(op, FsOp::CreateFile(_))),
            "expected only CreateFile ops, got {ops:?}"
        );
    }

    // ── create_dir_trailing_slash ─────────────────────────────────────────────

    /// New buffer line `newdir/` ⇒ CreateDir.
    #[test]
    fn create_dir_trailing_slash() {
        let baseline = make_baseline(&[]);
        // Buffer: root line + one new dir with trailing slash.
        let buffer = "  project\n    newdir/";
        let ops = reconcile(&baseline, buffer, &root());
        assert_eq!(ops, vec![FsOp::CreateDir(root().join("newdir"))]);
    }

    // ── create_nested ─────────────────────────────────────────────────────────

    /// New line `a/b.rs` at depth-1 indent ⇒ CreateFile(root/a/b.rs).
    #[test]
    fn create_nested() {
        let baseline = make_baseline(&[]);
        // Buffer: root line + nested path via internal slash at depth-1 indent (4 spaces).
        let buffer = "  project\n    a/b.rs";
        let ops = reconcile(&baseline, buffer, &root());
        assert_eq!(ops, vec![FsOp::CreateFile(root().join("a").join("b.rs"))]);
    }

    // ── delete_to_trash ───────────────────────────────────────────────────────

    /// baseline has a file; buffer removes that line ⇒ Trash(path).
    #[test]
    fn delete_to_trash() {
        let baseline = make_baseline(&[("to_delete.rs", false)]);
        // Buffer: root line only — file removed.
        let buffer = "  project";
        let ops = reconcile(&baseline, buffer, &root());
        assert_eq!(
            ops,
            vec![FsOp::Trash(root().join("to_delete.rs"))],
            "removed file must produce exactly one Trash op"
        );
    }

    // ── rename_in_place ───────────────────────────────────────────────────────

    /// `foo.rs` renamed to `bar.rs` at same position/indent ⇒ exactly one Rename,
    /// NO Trash, NO Create.  This is the data-loss guard.
    #[test]
    fn rename_in_place_is_rename_not_trash_create() {
        let baseline = make_baseline(&[("foo.rs", false)]);
        // Edit: same position, name changed.
        let buffer = "  project\n    bar.rs";
        let ops = reconcile(&baseline, buffer, &root());
        assert_eq!(
            ops,
            vec![FsOp::Rename {
                from: root().join("foo.rs"),
                to: root().join("bar.rs"),
            }],
            "in-place rename must produce exactly Rename{{foo.rs → bar.rs}}, got {ops:?}"
        );
        // Explicitly assert no Trash or Create ops — the data-loss guard.
        for op in &ops {
            assert!(
                !matches!(op, FsOp::Trash(_)),
                "Trash must not be emitted for an in-place rename"
            );
            assert!(
                !matches!(op, FsOp::CreateFile(_) | FsOp::CreateDir(_)),
                "Create must not be emitted for an in-place rename"
            );
        }
    }

    // ── rename_dir ────────────────────────────────────────────────────────────

    /// baseline `olddir/` (dir); buffer `newdir/` ⇒ Rename (dir).
    /// When children are present, ancestor rename must be ordered before child.
    #[test]
    fn rename_dir() {
        let baseline = make_baseline(&[("olddir", true), ("olddir/child.rs", false)]);
        // Buffer: root line + newdir/ + child under newdir (at depth 2 = 6 spaces).
        let buffer = "  project\n    newdir/\n      child.rs";
        let ops = reconcile(&baseline, buffer, &root());

        // Must contain Rename{olddir → newdir}.
        let rename_dir_op = FsOp::Rename {
            from: root().join("olddir"),
            to: root().join("newdir"),
        };
        assert!(
            ops.contains(&rename_dir_op),
            "must contain Rename{{olddir → newdir}}, got {ops:?}"
        );

        // Must contain Rename{olddir/child.rs → newdir/child.rs}.
        let rename_child_op = FsOp::Rename {
            from: root().join("olddir").join("child.rs"),
            to: root().join("newdir").join("child.rs"),
        };
        assert!(
            ops.contains(&rename_child_op),
            "must contain child rename, got {ops:?}"
        );

        // The parent rename must come before the child rename (shallower first).
        let dir_pos = ops.iter().position(|op| op == &rename_dir_op).unwrap();
        let child_pos = ops.iter().position(|op| op == &rename_child_op).unwrap();
        assert!(
            dir_pos < child_pos,
            "ancestor rename must precede child rename: dir={dir_pos} child={child_pos}"
        );
    }

    // ── unchanged_no_ops ─────────────────────────────────────────────────────

    /// buffer == rendered baseline ⇒ empty Vec.
    #[test]
    fn unchanged_no_ops() {
        let baseline =
            make_baseline(&[("src", true), ("src/main.rs", false), ("Cargo.toml", false)]);
        let buffer = render_baseline(&baseline);
        let ops = reconcile(&baseline, &buffer, &root());
        assert!(
            ops.is_empty(),
            "unchanged buffer must produce no ops, got {ops:?}\nbuffer:\n{buffer}"
        );
    }

    // ── delete_one_keep_rest ──────────────────────────────────────────────────

    /// Middle line removed ⇒ single Trash, others unchanged.
    #[test]
    fn delete_one_keep_rest() {
        let baseline =
            make_baseline(&[("alpha.rs", false), ("beta.rs", false), ("gamma.rs", false)]);
        // Remove beta.rs from the middle.
        let buffer = "  project\n    alpha.rs\n    gamma.rs";
        let ops = reconcile(&baseline, buffer, &root());
        assert_eq!(
            ops,
            vec![FsOp::Trash(root().join("beta.rs"))],
            "only beta.rs must be trashed, got {ops:?}"
        );
    }

    // ── mixed ─────────────────────────────────────────────────────────────────

    /// A rename + a create + a delete in one buffer ⇒ ordered Vec:
    /// renames first, then trashes, then creates.
    ///
    /// Scenario:
    ///   baseline:  old.rs, keep.rs, remove.rs
    ///   buffer:    new.rs, keep.rs, fresh.rs, added.rs
    ///
    /// LCS = [keep.rs].
    /// Gap before keep.rs: bgap=[old.rs], cgap=[new.rs] → Rename{old→new}.
    /// Gap after keep.rs:  bgap=[remove.rs], cgap=[fresh.rs, added.rs].
    ///   - Pair 1: remove.rs ↔ fresh.rs → Rename{remove→fresh}.
    ///   - Leftover cgap: added.rs → CreateFile.
    ///
    /// Final order: Rename(old→new), Rename(remove→fresh), CreateFile(added).
    #[test]
    fn mixed() {
        let baseline =
            make_baseline(&[("old.rs", false), ("keep.rs", false), ("remove.rs", false)]);
        // Buffer: new.rs (rename of old.rs), keep.rs, fresh.rs (rename of remove.rs),
        // added.rs (pure create).
        let buffer = "  project\n    new.rs\n    keep.rs\n    fresh.rs\n    added.rs";
        let ops = reconcile(&baseline, buffer, &root());

        let has_rename_old = ops.contains(&FsOp::Rename {
            from: root().join("old.rs"),
            to: root().join("new.rs"),
        });
        let has_rename_remove = ops.contains(&FsOp::Rename {
            from: root().join("remove.rs"),
            to: root().join("fresh.rs"),
        });
        let has_create = ops.contains(&FsOp::CreateFile(root().join("added.rs")));

        assert!(
            has_rename_old,
            "must have Rename{{old.rs → new.rs}}, got {ops:?}"
        );
        assert!(
            has_rename_remove,
            "must have Rename{{remove.rs → fresh.rs}}, got {ops:?}"
        );
        assert!(has_create, "must have CreateFile(added.rs), got {ops:?}");

        // Ordering: all renames before creates.
        let rename_old_pos = ops
            .iter()
            .position(|op| {
                matches!(op, FsOp::Rename { from, to }
                    if from == &root().join("old.rs") && to == &root().join("new.rs"))
            })
            .unwrap();
        let rename_remove_pos = ops
            .iter()
            .position(|op| {
                matches!(op, FsOp::Rename { from, to }
                    if from == &root().join("remove.rs") && to == &root().join("fresh.rs"))
            })
            .unwrap();
        let create_pos = ops
            .iter()
            .position(|op| matches!(op, FsOp::CreateFile(p) if p == &root().join("added.rs")))
            .unwrap();

        assert!(
            rename_old_pos < create_pos,
            "renames must precede creates: rename={rename_old_pos} create={create_pos}"
        );
        assert!(
            rename_remove_pos < create_pos,
            "renames must precede creates: rename={rename_remove_pos} create={create_pos}"
        );
    }

    // ── mixed with explicit trash ─────────────────────────────────────────────

    /// Verify a pure delete: baseline has A, B, C; buffer has A, C only.
    /// B is deleted (bgap=[B], cgap=[] → Trash(B)).
    #[test]
    fn mixed_pure_delete_produces_trash_not_rename() {
        let baseline = make_baseline(&[("a.rs", false), ("b.rs", false), ("c.rs", false)]);
        let buffer = "  project\n    a.rs\n    c.rs";
        let ops = reconcile(&baseline, buffer, &root());
        // LCS = [a.rs, c.rs]. Gap in middle: bgap=[b.rs], cgap=[] → Trash.
        assert_eq!(ops, vec![FsOp::Trash(root().join("b.rs"))]);
    }

    // ── rename_dir with no children ───────────────────────────────────────────

    /// Rename an empty dir.
    #[test]
    fn rename_empty_dir() {
        let baseline = make_baseline(&[("emptydir", true)]);
        let buffer = "  project\n    renamed/";
        let ops = reconcile(&baseline, buffer, &root());
        assert_eq!(
            ops,
            vec![FsOp::Rename {
                from: root().join("emptydir"),
                to: root().join("renamed"),
            }]
        );
    }

    // ── type change: file → dir ───────────────────────────────────────────────

    /// When the type changes at the same position (file → dir), emit Trash + CreateDir.
    /// The LCS keys on (path, is_dir) so a same-path different-type entry is
    /// NOT treated as unchanged.
    #[test]
    fn type_change_file_to_dir() {
        let baseline = make_baseline(&[("thing", false)]);
        // Same name, but now typed as a directory.
        let buffer = "  project\n    thing/";
        let ops = reconcile(&baseline, buffer, &root());
        // bgap=[thing(file)], cgap=[thing/(dir)]: type mismatch → Trash + CreateDir.
        assert!(
            ops.contains(&FsOp::Trash(root().join("thing"))),
            "must trash old file, got {ops:?}"
        );
        assert!(
            ops.contains(&FsOp::CreateDir(root().join("thing"))),
            "must create new dir, got {ops:?}"
        );
        assert_eq!(ops.len(), 2, "exactly Trash + CreateDir, got {ops:?}");
    }

    // ── type change: dir → file ───────────────────────────────────────────────

    /// Symmetrical: baseline dir, buffer file at same path → Trash + CreateFile.
    #[test]
    fn type_change_dir_to_file() {
        let baseline = make_baseline(&[("thing", true)]);
        let buffer = "  project\n    thing";
        let ops = reconcile(&baseline, buffer, &root());
        assert!(
            ops.contains(&FsOp::Trash(root().join("thing"))),
            "must trash old dir"
        );
        assert!(
            ops.contains(&FsOp::CreateFile(root().join("thing"))),
            "must create new file"
        );
        assert_eq!(ops.len(), 2, "exactly Trash + CreateFile, got {ops:?}");
    }

    // ── ordering: trashes deep before shallow ────────────────────────────────

    /// Removing a dir and its child from baseline: child must be trashed before dir.
    #[test]
    fn trash_ordering_deep_before_shallow() {
        let baseline = make_baseline(&[("parent", true), ("parent/child.rs", false)]);
        // Buffer: both removed.
        let buffer = "  project";
        let ops = reconcile(&baseline, buffer, &root());

        let child_pos = ops
            .iter()
            .position(
                |op| matches!(op, FsOp::Trash(p) if p == &root().join("parent").join("child.rs")),
            )
            .unwrap();
        let parent_pos = ops
            .iter()
            .position(|op| matches!(op, FsOp::Trash(p) if p == &root().join("parent")))
            .unwrap();

        assert!(
            child_pos < parent_pos,
            "child must be trashed before parent: child={child_pos} parent={parent_pos}"
        );
    }

    // ── ordering: creates shallow before deep ────────────────────────────────

    /// Adding a dir and a child file: dir must come before file in the ops list.
    #[test]
    fn create_ordering_shallow_before_deep() {
        let baseline = make_baseline(&[]);
        // Two new entries: depth-1 dir and depth-2 file inside it.
        // Buffer line 1: depth 1 → indent 4 → "    newdir/"
        // Buffer line 2: depth 2 → indent 6 → "      newfile.rs"
        let buffer = "  project\n    newdir/\n      newfile.rs";
        let ops = reconcile(&baseline, buffer, &root());

        let dir_pos = ops
            .iter()
            .position(|op| matches!(op, FsOp::CreateDir(p) if p == &root().join("newdir")))
            .expect("CreateDir(newdir) must be present");
        let file_pos = ops
            .iter()
            .position(|op| matches!(op, FsOp::CreateFile(p) if p == &root().join("newdir").join("newfile.rs")))
            .expect("CreateFile(newdir/newfile.rs) must be present");

        assert!(
            dir_pos < file_pos,
            "CreateDir must precede CreateFile: dir={dir_pos} file={file_pos}"
        );
    }

    // ── render_baseline helper roundtrip ─────────────────────────────────────

    /// render_baseline of a non-trivial tree produces a string that reconcile
    /// treats as unchanged.
    #[test]
    fn render_baseline_roundtrips() {
        let baseline = make_baseline(&[
            ("docs", true),
            ("docs/README.md", false),
            ("src", true),
            ("src/main.rs", false),
            ("src/lib.rs", false),
            ("Cargo.toml", false),
        ]);
        let buf = render_baseline(&baseline);
        let ops = reconcile(&baseline, &buf, &root());
        assert!(
            ops.is_empty(),
            "render_baseline must produce an unchanged buffer, got {ops:?}\nbuffer:\n{buf}"
        );
    }

    // ── empty baseline + empty buffer ────────────────────────────────────────

    #[test]
    fn empty_baseline_empty_buffer() {
        let baseline = vec![(root(), true)];
        let buffer = "  project";
        let ops = reconcile(&baseline, buffer, &root());
        assert!(ops.is_empty());
    }

    // ── blank lines in buffer are ignored ────────────────────────────────────

    #[test]
    fn blank_lines_ignored() {
        let baseline = make_baseline(&[("foo.rs", false)]);
        // Buffer has blank lines interspersed.
        let buffer = "  project\n\n    foo.rs\n\n";
        let ops = reconcile(&baseline, buffer, &root());
        assert!(ops.is_empty(), "blank lines must be ignored, got {ops:?}");
    }

    // ── multiple creates in order ─────────────────────────────────────────────

    /// Three sibling creates: output CreateFile order must be stable / ascending
    /// by component count (all equal here, so insertion order is preserved).
    #[test]
    fn multiple_creates_are_all_present() {
        let baseline = make_baseline(&[]);
        let buffer = "  project\n    x.rs\n    y.rs\n    z.rs";
        let ops = reconcile(&baseline, buffer, &root());
        assert_eq!(ops.len(), 3);
        assert!(ops.iter().all(|op| matches!(op, FsOp::CreateFile(_))));
    }

    // ── apply_ops integration tests ───────────────────────────────────────────

    /// Override `XDG_CACHE_HOME` to a temp dir for trash isolation.
    fn isolated_trash(td: &tempfile::TempDir) {
        // SAFETY: nextest runs each test in its own process so no data-race.
        unsafe { std::env::set_var("XDG_CACHE_HOME", td.path()) };
    }

    #[test]
    fn apply_create_file_makes_empty_file() {
        let td = tempfile::tempdir().unwrap();
        isolated_trash(&td);
        let target = td.path().join("new.rs");
        let ops = vec![FsOp::CreateFile(target.clone())];
        let (created, applied, errors) = apply_ops(&ops, &mut Vec::new());
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(created, vec![target.clone()]);
        assert_eq!(applied.len(), 1, "one AppliedOp must be recorded");
        assert!(
            matches!(&applied[0], AppliedOp::Created(p) if p == &target),
            "must record Created AppliedOp"
        );
        assert!(target.exists(), "file must exist after CreateFile");
        assert_eq!(
            std::fs::metadata(&target).unwrap().len(),
            0,
            "created file must be empty"
        );
    }

    #[test]
    fn apply_create_nested_makes_parents() {
        let td = tempfile::tempdir().unwrap();
        isolated_trash(&td);
        let target = td.path().join("a").join("b").join("c.rs");
        let ops = vec![FsOp::CreateFile(target.clone())];
        let (created, applied, errors) = apply_ops(&ops, &mut Vec::new());
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(created, vec![target.clone()]);
        assert_eq!(applied.len(), 1);
        assert!(target.exists(), "nested file must exist");
    }

    #[test]
    fn apply_trash_moves_into_trash_not_deleted() {
        let td = tempfile::tempdir().unwrap();
        isolated_trash(&td);
        // Create the source file.
        let src = td.path().join("source.txt");
        std::fs::write(&src, b"hello").unwrap();
        let ops = vec![FsOp::Trash(src.clone())];
        let mut trashed = Vec::new();
        let (created, applied, errors) = apply_ops(&ops, &mut trashed);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert!(created.is_empty());
        assert_eq!(applied.len(), 1, "one AppliedOp must be recorded");
        // Source must be gone.
        assert!(!src.exists(), "source must be gone after Trash");
        // A file must exist inside the trash dir.
        assert_eq!(trashed.len(), 1, "one entry must be in trashed registry");
        let (name, dest) = &trashed[0];
        assert_eq!(name, "source.txt");
        assert!(dest.exists(), "trash destination must exist: {dest:?}");
        // Verify content survived.
        assert_eq!(std::fs::read(dest).unwrap(), b"hello");
        // AppliedOp::Trashed must record the correct original + dest.
        assert!(
            matches!(&applied[0], AppliedOp::Trashed { original, dest: d }
                if original == &src && d.exists()),
            "AppliedOp::Trashed must record original + dest"
        );
    }

    #[test]
    fn apply_rename_preserves_content() {
        let td = tempfile::tempdir().unwrap();
        isolated_trash(&td);
        let foo = td.path().join("foo.rs");
        std::fs::write(&foo, b"hi").unwrap();
        let bar = td.path().join("bar.rs");
        let ops = vec![FsOp::Rename {
            from: foo.clone(),
            to: bar.clone(),
        }];
        let (created, applied, errors) = apply_ops(&ops, &mut Vec::new());
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert!(created.is_empty());
        assert_eq!(applied.len(), 1);
        assert!(
            matches!(&applied[0], AppliedOp::Renamed { from, to }
                if from == &foo && to == &bar),
            "must record Renamed AppliedOp"
        );
        assert!(!foo.exists(), "source must be gone after rename");
        assert!(bar.exists(), "destination must exist after rename");
        assert_eq!(
            std::fs::read(&bar).unwrap(),
            b"hi",
            "content must be preserved"
        );
    }

    #[test]
    fn apply_move_via_trash_then_create_restores_content() {
        let td = tempfile::tempdir().unwrap();
        isolated_trash(&td);
        // Set up: foo.rs in src_dir with content "x".
        let src_dir = td.path().join("src_dir");
        std::fs::create_dir_all(&src_dir).unwrap();
        let foo = src_dir.join("foo.rs");
        std::fs::write(&foo, b"x").unwrap();

        // Step 1: Trash foo.rs.
        let mut trashed: Vec<(String, PathBuf)> = Vec::new();
        let trash_ops = vec![FsOp::Trash(foo.clone())];
        let (c, _a1, e) = apply_ops(&trash_ops, &mut trashed);
        assert!(e.is_empty(), "trash must succeed: {e:?}");
        assert!(c.is_empty());
        assert_eq!(trashed.len(), 1);
        assert!(!foo.exists());

        // Step 2: CreateFile at dir2/foo.rs — should restore from trash.
        let dir2 = td.path().join("dir2");
        let dest = dir2.join("foo.rs");
        let create_ops = vec![FsOp::CreateFile(dest.clone())];
        let (created, applied, errors) = apply_ops(&create_ops, &mut trashed);
        assert!(errors.is_empty(), "restore must succeed: {errors:?}");
        // Restored from trash → NOT in the "created" list.
        assert!(
            created.is_empty(),
            "restored file must NOT appear in created list"
        );
        // trashed registry must be emptied.
        assert!(trashed.is_empty(), "trashed registry must be drained");
        // dest must exist with original content.
        assert!(dest.exists(), "destination must exist after restore");
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            b"x",
            "content must be preserved through trash-restore cycle"
        );
        // AppliedOp::Restored must be recorded.
        assert!(
            matches!(&applied[0], AppliedOp::Restored { to, .. } if to == &dest),
            "must record Restored AppliedOp"
        );
    }

    // ── revert_ops round-trip tests ───────────────────────────────────────────

    #[test]
    fn revert_create_removes_file_redo_recreates() {
        let td = tempfile::tempdir().unwrap();
        isolated_trash(&td);
        let target = td.path().join("round.rs");

        // Apply: create
        let ops = vec![FsOp::CreateFile(target.clone())];
        let mut trashed = Vec::new();
        let (_, applied, errors) = apply_ops(&ops, &mut trashed);
        assert!(errors.is_empty());
        assert!(target.exists(), "file must exist before revert");

        // Revert (undo)
        let (redo_journal, errs) = revert_ops(&applied, &mut trashed);
        assert!(errs.is_empty(), "revert errors: {errs:?}");
        assert!(!target.exists(), "file must be gone after revert");

        // Redo: re-apply the redo journal
        let (_, _redo_applied, errs2) = apply_applied(&redo_journal, &mut trashed);
        assert!(errs2.is_empty(), "redo errors: {errs2:?}");
        assert!(target.exists(), "file must exist again after redo");
    }

    #[test]
    fn revert_trash_restores_file_redo_retrashes() {
        let td = tempfile::tempdir().unwrap();
        isolated_trash(&td);
        let src = td.path().join("restore_me.txt");
        std::fs::write(&src, b"data").unwrap();

        // Apply: trash
        let ops = vec![FsOp::Trash(src.clone())];
        let mut trashed = Vec::new();
        let (_, applied, errors) = apply_ops(&ops, &mut trashed);
        assert!(errors.is_empty());
        assert!(!src.exists(), "must be trashed");

        // Revert (undo): restore from trash
        let (redo_journal, errs) = revert_ops(&applied, &mut trashed);
        assert!(errs.is_empty(), "revert errors: {errs:?}");
        assert!(src.exists(), "file must be back on disk after revert");
        assert_eq!(
            std::fs::read(&src).unwrap(),
            b"data",
            "content must survive"
        );

        // Redo: re-trash
        let (_, _redo_applied, errs2) = apply_applied(&redo_journal, &mut trashed);
        assert!(errs2.is_empty(), "redo errors: {errs2:?}");
        assert!(!src.exists(), "file must be trashed again after redo");
    }

    #[test]
    fn revert_rename_swaps_back_redo_renames_again() {
        let td = tempfile::tempdir().unwrap();
        isolated_trash(&td);
        let foo = td.path().join("orig.rs");
        let bar = td.path().join("renamed.rs");
        std::fs::write(&foo, b"content").unwrap();

        // Apply: rename
        let ops = vec![FsOp::Rename {
            from: foo.clone(),
            to: bar.clone(),
        }];
        let mut trashed = Vec::new();
        let (_, applied, errors) = apply_ops(&ops, &mut trashed);
        assert!(errors.is_empty());
        assert!(!foo.exists() && bar.exists(), "rename must have happened");

        // Revert (undo)
        let (redo_journal, errs) = revert_ops(&applied, &mut trashed);
        assert!(errs.is_empty(), "revert errors: {errs:?}");
        assert!(
            foo.exists() && !bar.exists(),
            "must be back to orig after revert"
        );

        // Redo
        let (_, _, errs2) = apply_applied(&redo_journal, &mut trashed);
        assert!(errs2.is_empty(), "redo errors: {errs2:?}");
        assert!(!foo.exists() && bar.exists(), "redo must rename again");
    }

    #[test]
    fn revert_restore_retrashes_redo_restores() {
        let td = tempfile::tempdir().unwrap();
        isolated_trash(&td);
        let src = td.path().join("moved.txt");
        std::fs::write(&src, b"hello").unwrap();

        // Step 1: trash it
        let mut trashed: Vec<(String, PathBuf)> = Vec::new();
        let (_, applied_trash, e) = apply_ops(&[FsOp::Trash(src.clone())], &mut trashed);
        assert!(e.is_empty());
        assert!(!src.exists());

        // Step 2: restore to a new location (simulate dd + p move)
        let dest = td.path().join("dest_dir").join("moved.txt");
        let (_, applied_restore, e2) = apply_ops(&[FsOp::CreateFile(dest.clone())], &mut trashed);
        assert!(e2.is_empty());
        assert!(dest.exists(), "restored file must exist at dest");

        // Combined applied journal for the move
        let mut all_applied = applied_trash;
        all_applied.extend(applied_restore);

        // Revert (undo): dest must be trashed, src does NOT come back (only dest→trash)
        let (redo_journal, errs) = revert_ops(&all_applied, &mut trashed);
        assert!(errs.is_empty(), "revert errors: {errs:?}");
        assert!(!dest.exists(), "dest must be trashed after revert");

        // Redo: restore dest from trash again
        let (_, _, errs2) = apply_applied(&redo_journal, &mut trashed);
        assert!(errs2.is_empty(), "redo errors: {errs2:?}");
        assert!(dest.exists(), "dest must be restored again after redo");
    }
}
