//! Pure reconcile engine for the oil.nvim-style editable file explorer.
//!
//! [`reconcile`] diffs an edited explorer buffer against a baseline snapshot
//! and returns an ordered [`Vec<FsOp>`] that, when applied in order, makes the
//! filesystem match the buffer.  **No filesystem access occurs here** — this is
//! a pure function suitable for exhaustive unit testing before the wiring phase.
//!
//! # Buffer format
//! Each non-root line is `<indent spaces><name><US><id>` where `US` = U+001F
//! (Unit Separator) and `id` is the node's decimal index in `tree.nodes` at
//! render time.  Line 0 is the root directory (no id, not an editable target).
//! Directories MAY be written with a trailing `/`.  Names may contain internal
//! slashes for nested creation (e.g. `a/b.rs`).
//!
//! # Baseline
//! An ordered `Vec<(u64, PathBuf, bool)>` — `(id, absolute path, is_dir)` per
//! line, index 0 = root.  Produced by [`crate::app::explorer::ExplorerTree`]
//! and snapshotted when the buffer is built.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Unit-separator character used to delimit the name from the id in each
/// non-root explorer buffer line.  Defined here and re-exported so `explorer.rs`
/// can import it without duplication.
pub(crate) const ID_SEP: char = '\u{1F}';

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
    /// The stable id carried in the line (`<US><id>` tail), or `None` when the
    /// line is new (no id tail) or the id couldn't be parsed.
    id: Option<u64>,
    path: PathBuf,
    is_dir: bool,
}

/// Parse `buffer` (the current explorer buffer text) into an ordered list of
/// entries, resolving absolute paths using `root`.  Line 0 (the root dir line)
/// is skipped.  Each non-root line is expected to be:
///   `<indent spaces><name><US><id>`
/// where `US` = `ID_SEP` and `<id>` is a decimal integer.  Lines without a
/// `US` (new lines typed by the user) have `id = None`.  Blank or name-empty
/// lines are skipped.
fn parse_buffer(buffer: &str, root: &Path) -> Vec<BufEntry> {
    // depth → absolute dir path for parent resolution.
    let mut stack: Vec<(usize, PathBuf)> = Vec::new();
    let mut entries: Vec<BufEntry> = Vec::new();

    for (line_idx, line) in buffer.lines().enumerate() {
        // Skip line 0 — root dir header.
        if line_idx == 0 {
            continue;
        }

        // Split on the FIRST ID_SEP to separate the name side from the id tail.
        let (left, id_opt) = if let Some(sep_pos) = line.find(ID_SEP) {
            let id_str = &line[sep_pos + ID_SEP.len_utf8()..];
            // Parse leading ASCII digits; ignore trailing garbage.
            let digits: String = id_str.chars().take_while(|c| c.is_ascii_digit()).collect();
            let id: Option<u64> = if digits.is_empty() {
                None
            } else {
                digits.parse().ok()
            };
            (&line[..sep_pos], id)
        } else {
            (line, None)
        };

        // Blank / whitespace-only name side → skip.
        if left.trim().is_empty() {
            continue;
        }

        // Count leading ASCII spaces for indent.
        let indent = left.len() - left.trim_start_matches(' ').len();

        // depth = (indent - 2) / 2, clamped to ≥ 1.
        let depth = ((indent.saturating_sub(2)) / 2).max(1);

        // Name is verbatim between indent and US — do NOT trim_end trailing spaces.
        let raw = &left[indent..];
        let is_dir = raw.ends_with('/');
        // Remove exactly one trailing '/' if it's a dir marker; else name is verbatim.
        let name = if is_dir { &raw[..raw.len() - 1] } else { raw };
        if name.is_empty() {
            continue;
        }

        // Pop stack entries that are at depth ≥ current depth.
        while stack.last().map(|(d, _)| *d >= depth).unwrap_or(false) {
            stack.pop();
        }

        // Resolve parent. depth-1 lines are children of root. A depth ≥ 2 line
        // REQUIRES an immediate parent dir (depth-1) on the stack; if it's
        // missing the line was ORPHANED by a deleted ancestor — e.g. `dd` on an
        // OPEN directory removes only the dir's own line, leaving its (deeper-
        // indented) children behind with no parent. Drop such orphans so
        // reconcile trashes them along with the deleted dir, rather than
        // reparenting them up to the root.
        let parent = if depth == 1 {
            root
        } else {
            match stack.last().filter(|(d, _)| *d == depth - 1) {
                Some((_, p)) => p.as_path(),
                None => continue, // orphan of a deleted ancestor → drop
            }
        };

        // `Path::join` handles internal slashes in `name` (e.g. "a/b.rs").
        let target = parent.join(name);

        if is_dir {
            stack.push((depth, target.clone()));
        }

        entries.push(BufEntry {
            id: id_opt,
            path: target,
            is_dir,
        });
    }

    entries
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
/// - `baseline`: `(id, abs_path, is_dir)` per line; index 0 = root (ignored by
///   the diff).  `id` is the sequence number assigned during `render_text`.
/// - `buffer`:   current buffer text (line 0 = root header).
/// - `root`:     explorer root == `baseline[0].1`.
///
/// # Algorithm
/// Walk buffer entries keyed by their embedded id:
/// - `id` in baseline and not yet seen → match: rename if path changed, else
///   no-op.  Mark id "seen".
/// - `id` is `None`, unknown, or already seen (duplicate, e.g. `yy`+`p`) →
///   CreateDir / CreateFile.
///   After the walk: every baseline entry not seen → Trash.
///
/// # Ordering
/// 1. Renames, sorted by `from` component count **ascending** (shallow → deep).
/// 2. Trashes, sorted by path component count **descending** (deep → shallow,
///    children before parents).
/// 3. Creates, sorted by path component count **ascending** (parents before
///    children).
pub(crate) fn reconcile(baseline: &[(u64, PathBuf, bool)], buffer: &str, root: &Path) -> Vec<FsOp> {
    let current = parse_buffer(buffer, root);

    // Build an id-keyed index of baseline[1..] (skip root at index 0).
    let mut base_by_id: HashMap<u64, (&PathBuf, bool)> = HashMap::new();
    for (id, path, is_dir) in baseline.iter().skip(1) {
        base_by_id.insert(*id, (path, *is_dir));
    }

    let mut renames: Vec<FsOp> = Vec::new();
    let mut trashes: Vec<FsOp> = Vec::new();
    let mut creates: Vec<FsOp> = Vec::new();

    let mut seen: HashSet<u64> = HashSet::new();

    for entry in &current {
        match entry.id {
            Some(id) if base_by_id.contains_key(&id) && !seen.contains(&id) => {
                seen.insert(id);
                let (bpath, b_is_dir) = base_by_id[&id];
                if b_is_dir == entry.is_dir {
                    // Same type.
                    if bpath != &entry.path {
                        renames.push(FsOp::Rename {
                            from: bpath.clone(),
                            to: entry.path.clone(),
                        });
                    }
                    // else: unchanged — no op needed
                } else {
                    // Type changed (file → dir or dir → file) → trash + create.
                    trashes.push(FsOp::Trash(bpath.clone()));
                    if entry.is_dir {
                        creates.push(FsOp::CreateDir(entry.path.clone()));
                    } else {
                        creates.push(FsOp::CreateFile(entry.path.clone()));
                    }
                }
            }
            // No id, unknown id, or duplicate id (yy+p) → create.
            _ => {
                if entry.is_dir {
                    creates.push(FsOp::CreateDir(entry.path.clone()));
                } else {
                    creates.push(FsOp::CreateFile(entry.path.clone()));
                }
            }
        }
    }

    // Every baseline entry (skip root at index 0) not seen in the buffer → Trash.
    for (id, path, _is_dir) in baseline.iter().skip(1) {
        if !seen.contains(id) {
            trashes.push(FsOp::Trash(path.clone()));
        }
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

    // ── Test helpers ──────────────────────────────────────────────────────────

    /// Build a baseline `Vec<(u64, PathBuf, bool)>` from a list of
    /// (relative_path, is_dir) pairs. Index 0 = root (id=0), first item id=1, …
    fn make_baseline(items: &[(&str, bool)]) -> Vec<(u64, PathBuf, bool)> {
        let r = root();
        let mut v: Vec<(u64, PathBuf, bool)> = Vec::new();
        v.push((0, r.clone(), true)); // root, id=0
        for (i, (rel, is_dir)) in items.iter().enumerate() {
            v.push(((i + 1) as u64, r.join(rel), *is_dir));
        }
        v
    }

    /// Helper to produce a single non-root line with embedded id.
    /// `depth` is the tree depth (root=0, children=1, grandchildren=2, …).
    fn idline(depth: usize, name: &str, id: u64) -> String {
        let indent = depth * 2 + 2;
        format!("{}{}{}{}", " ".repeat(indent), name, ID_SEP, id)
    }

    /// Helper for the root header line (no id).
    fn root_header() -> &'static str {
        "  project"
    }

    /// Render a baseline to the bare buffer text that `reconcile` expects.
    /// Root line (index 0) has no id. All other lines carry `<US><id>`.
    fn render_baseline(baseline: &[(u64, PathBuf, bool)]) -> String {
        let mut out = String::new();
        for (i, (id, path, is_dir)) in baseline.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            let root = &baseline[0].1;
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
            // Non-root dirs get trailing '/' to push them onto the parent stack.
            if *is_dir && depth > 0 {
                out.push('/');
            }
            // Non-root lines carry the id.
            if i > 0 {
                out.push(ID_SEP);
                out.push_str(&id.to_string());
            }
        }
        out
    }

    // Convenience root.
    fn root() -> PathBuf {
        PathBuf::from("/project")
    }

    // ── bulk_create ───────────────────────────────────────────────────────────

    /// baseline has root + 1 file (id=1); buffer adds 3 new sibling lines (no id)
    /// ⇒ 3 CreateFile ops. existing.rs carries id=1 → unchanged.
    #[test]
    fn bulk_create() {
        let baseline = make_baseline(&[("existing.rs", false)]);
        // existing.rs with id=1; 3 new lines without ids.
        let buffer = format!(
            "{}\n{}\n    new_a.rs\n    new_b.rs\n    new_c.rs",
            root_header(),
            idline(1, "existing.rs", 1),
        );
        let ops = reconcile(&baseline, &buffer, &root());
        // existing.rs is unchanged (id match, same path).
        // new_a, new_b, new_c are creates.
        assert_eq!(ops.len(), 3, "expected 3 creates, got {ops:?}");
        assert!(ops.contains(&FsOp::CreateFile(root().join("new_a.rs"))));
        assert!(ops.contains(&FsOp::CreateFile(root().join("new_b.rs"))));
        assert!(ops.contains(&FsOp::CreateFile(root().join("new_c.rs"))));
        assert!(
            ops.iter().all(|op| matches!(op, FsOp::CreateFile(_))),
            "expected only CreateFile ops, got {ops:?}"
        );
    }

    // ── create_dir_trailing_slash ─────────────────────────────────────────────

    /// New buffer line `newdir/` (no id) ⇒ CreateDir.
    #[test]
    fn create_dir_trailing_slash() {
        let baseline = make_baseline(&[]);
        let buffer = format!("{}\n    newdir/", root_header());
        let ops = reconcile(&baseline, &buffer, &root());
        assert_eq!(ops, vec![FsOp::CreateDir(root().join("newdir"))]);
    }

    // ── create_nested ─────────────────────────────────────────────────────────

    /// New line `a/b.rs` at depth-1 indent (no id) ⇒ CreateFile(root/a/b.rs).
    #[test]
    fn create_nested() {
        let baseline = make_baseline(&[]);
        let buffer = format!("{}\n    a/b.rs", root_header());
        let ops = reconcile(&baseline, &buffer, &root());
        assert_eq!(ops, vec![FsOp::CreateFile(root().join("a").join("b.rs"))]);
    }

    // ── delete_to_trash ───────────────────────────────────────────────────────

    /// baseline has a file (id=1); buffer removes that line ⇒ Trash(path).
    #[test]
    fn delete_to_trash() {
        let baseline = make_baseline(&[("to_delete.rs", false)]);
        // Buffer: root line only — file line omitted → id=1 not seen → Trash.
        let buffer = root_header().to_string();
        let ops = reconcile(&baseline, &buffer, &root());
        assert_eq!(
            ops,
            vec![FsOp::Trash(root().join("to_delete.rs"))],
            "removed file must produce exactly one Trash op"
        );
    }

    // ── rename_in_place ───────────────────────────────────────────────────────

    /// `foo.rs` (id=1) renamed to `bar.rs` in the buffer ⇒ exactly one Rename,
    /// NO Trash, NO Create.  This is the data-loss guard.
    #[test]
    fn rename_in_place_is_rename_not_trash_create() {
        let baseline = make_baseline(&[("foo.rs", false)]);
        // Same id=1 but name changed to bar.rs.
        let buffer = format!("{}\n{}", root_header(), idline(1, "bar.rs", 1));
        let ops = reconcile(&baseline, &buffer, &root());
        assert_eq!(
            ops,
            vec![FsOp::Rename {
                from: root().join("foo.rs"),
                to: root().join("bar.rs"),
            }],
            "in-place rename must produce exactly Rename{{foo.rs → bar.rs}}, got {ops:?}"
        );
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

    /// baseline `olddir/` (id=1, dir); `olddir/child.rs` (id=2, file);
    /// buffer `newdir/` id=1, `child.rs` id=2 under newdir ⇒ Rename both.
    /// Ancestor rename must be ordered before child.
    #[test]
    fn rename_dir() {
        let baseline = make_baseline(&[("olddir", true), ("olddir/child.rs", false)]);
        // newdir/ with id=1; child.rs under newdir with id=2.
        let buffer = format!(
            "{}\n{}\n{}",
            root_header(),
            idline(1, "newdir/", 1),
            idline(2, "child.rs", 2),
        );
        let ops = reconcile(&baseline, &buffer, &root());

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
        // alpha id=1, gamma id=3 present; beta id=2 missing → Trash(beta).
        let buffer = format!(
            "{}\n{}\n{}",
            root_header(),
            idline(1, "alpha.rs", 1),
            idline(1, "gamma.rs", 3),
        );
        let ops = reconcile(&baseline, &buffer, &root());
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
    /// Scenario (ids=1,2,3):
    ///   baseline:  old.rs(1), keep.rs(2), remove.rs(3)
    ///   buffer:    new.rs(id=1), keep.rs(id=2), fresh.rs(id=3), added.rs(no id)
    #[test]
    fn mixed() {
        let baseline =
            make_baseline(&[("old.rs", false), ("keep.rs", false), ("remove.rs", false)]);
        let buffer = format!(
            "{}\n{}\n{}\n{}\n    added.rs",
            root_header(),
            idline(1, "new.rs", 1),
            idline(1, "keep.rs", 2),
            idline(1, "fresh.rs", 3),
        );
        let ops = reconcile(&baseline, &buffer, &root());

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

    /// Verify a pure delete: baseline has A(1), B(2), C(3); buffer has A, C only.
    #[test]
    fn mixed_pure_delete_produces_trash_not_rename() {
        let baseline = make_baseline(&[("a.rs", false), ("b.rs", false), ("c.rs", false)]);
        let buffer = format!(
            "{}\n{}\n{}",
            root_header(),
            idline(1, "a.rs", 1),
            idline(1, "c.rs", 3),
        );
        let ops = reconcile(&baseline, &buffer, &root());
        assert_eq!(ops, vec![FsOp::Trash(root().join("b.rs"))]);
    }

    // ── rename_dir with no children ───────────────────────────────────────────

    /// Rename an empty dir (id=1).
    #[test]
    fn rename_empty_dir() {
        let baseline = make_baseline(&[("emptydir", true)]);
        let buffer = format!("{}\n{}", root_header(), idline(1, "renamed/", 1));
        let ops = reconcile(&baseline, &buffer, &root());
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
    /// With id-keyed reconcile: if id=1 maps to a file in baseline but appears as
    /// dir in buffer → Trash(old) + CreateDir(new).
    #[test]
    fn type_change_file_to_dir() {
        let baseline = make_baseline(&[("thing", false)]);
        // Same id=1 but now typed as a directory.
        let buffer = format!("{}\n{}", root_header(), idline(1, "thing/", 1));
        let ops = reconcile(&baseline, &buffer, &root());
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

    /// Symmetrical: baseline dir (id=1), buffer file at same path → Trash + CreateFile.
    #[test]
    fn type_change_dir_to_file() {
        let baseline = make_baseline(&[("thing", true)]);
        let buffer = format!("{}\n{}", root_header(), idline(1, "thing", 1));
        let ops = reconcile(&baseline, &buffer, &root());
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

    /// Removing a dir (id=1) and its child (id=2) from baseline: child must be
    /// trashed before dir.
    #[test]
    fn trash_ordering_deep_before_shallow() {
        let baseline = make_baseline(&[("parent", true), ("parent/child.rs", false)]);
        // Buffer: both ids missing → both trashed.
        let buffer = root_header().to_string();
        let ops = reconcile(&baseline, &buffer, &root());

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

    /// Adding a dir and a child file (no ids → creates): dir must come before file.
    #[test]
    fn create_ordering_shallow_before_deep() {
        let baseline = make_baseline(&[]);
        let buffer = format!("{}\n    newdir/\n      newfile.rs", root_header());
        let ops = reconcile(&baseline, &buffer, &root());

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
        let baseline = vec![(0u64, root(), true)];
        let buffer = root_header().to_string();
        let ops = reconcile(&baseline, &buffer, &root());
        assert!(ops.is_empty());
    }

    // ── blank lines in buffer are ignored ────────────────────────────────────

    #[test]
    fn blank_lines_ignored() {
        let baseline = make_baseline(&[("foo.rs", false)]);
        // Buffer has blank lines interspersed; foo.rs carries its id.
        let buffer = format!("{}\n\n{}\n\n", root_header(), idline(1, "foo.rs", 1),);
        let ops = reconcile(&baseline, &buffer, &root());
        assert!(ops.is_empty(), "blank lines must be ignored, got {ops:?}");
    }

    // ── multiple creates in order ─────────────────────────────────────────────

    /// Three sibling creates (no ids): output CreateFile order is stable.
    #[test]
    fn multiple_creates_are_all_present() {
        let baseline = make_baseline(&[]);
        let buffer = format!("{}\n    x.rs\n    y.rs\n    z.rs", root_header());
        let ops = reconcile(&baseline, &buffer, &root());
        assert_eq!(ops.len(), 3);
        assert!(ops.iter().all(|op| matches!(op, FsOp::CreateFile(_))));
    }

    // ── duplicate id → copy (yy+p semantics) ─────────────────────────────────

    /// When the same id appears twice in the buffer (yy+p), the second
    /// occurrence has no match (seen set already contains the id) → CreateFile.
    #[test]
    fn duplicate_id_creates_copy() {
        let baseline = make_baseline(&[("orig.rs", false)]);
        // id=1 appears twice → first → rename (same path → no-op), second → create.
        let buffer = format!(
            "{}\n{}\n{}",
            root_header(),
            idline(1, "orig.rs", 1), // same path → no-op
            idline(1, "orig.rs", 1), // duplicate id → create
        );
        let ops = reconcile(&baseline, &buffer, &root());
        // The duplicate line produces a CreateFile.
        assert_eq!(
            ops.len(),
            1,
            "duplicate id must yield one CreateFile, got {ops:?}"
        );
        assert!(
            ops.contains(&FsOp::CreateFile(root().join("orig.rs"))),
            "expected CreateFile(orig.rs), got {ops:?}"
        );
    }

    /// `dd` on an OPEN (unfolded) dir removes only the dir's own line, leaving
    /// its children behind at their deeper indent with no parent. Those orphans
    /// must be dropped (→ trashed with the dir), NOT reparented to root.
    #[test]
    fn dd_open_dir_orphans_are_trashed_not_reparented() {
        // baseline: mydir/(id1), mydir/a.rs(id2), mydir/b.rs(id3)
        let baseline = make_baseline(&[
            ("mydir", true),
            ("mydir/a.rs", false),
            ("mydir/b.rs", false),
        ]);
        // Buffer AFTER `dd` on the open `mydir/` line: the dir line is gone, but
        // its two children remain at depth-2 (6-space) indent with their ids.
        let buffer = format!(
            "{}\n{}\n{}",
            root_header(),
            idline(2, "a.rs", 2),
            idline(2, "b.rs", 3),
        );
        let ops = reconcile(&baseline, &buffer, &root());
        // All three originals must be trashed; nothing reparented to root.
        assert!(
            ops.contains(&FsOp::Trash(root().join("mydir"))),
            "dir must be trashed, got {ops:?}"
        );
        assert!(
            ops.contains(&FsOp::Trash(root().join("mydir").join("a.rs"))),
            "child a.rs must be trashed, got {ops:?}"
        );
        assert!(
            ops.contains(&FsOp::Trash(root().join("mydir").join("b.rs"))),
            "child b.rs must be trashed, got {ops:?}"
        );
        // NO rename (would mean a child was orphaned to root) and NO create.
        assert!(
            ops.iter().all(|op| matches!(op, FsOp::Trash(_))),
            "open-dir dd must produce only Trash ops, got {ops:?}"
        );
    }

    // ── indent corruption is safe ─────────────────────────────────────────────

    /// Mangling an unrelated line's indent but keeping ids intact must NOT
    /// produce spurious Trash ops: the intended structure is preserved.
    #[test]
    fn indent_corruption_safe() {
        // baseline: mydir/(id=1), mydir/file.rs(id=2), sibling.rs(id=3)
        let baseline = make_baseline(&[
            ("mydir", true),
            ("mydir/file.rs", false),
            ("sibling.rs", false),
        ]);
        // Buffer: mydir/ id=1, sibling.rs id=3; file.rs id=2 moved to wrong indent
        // (indent mangled from 6 to 4 spaces = depth 1, no longer under mydir).
        // IDs are intact so: id=1 → mydir (unchanged), id=2 → file.rs under root
        // (mangled indent = depth 1 = new location → Rename), id=3 → sibling.rs.
        let buffer = format!(
            "{}\n{}\n{}\n{}",
            root_header(),
            idline(1, "mydir/", 1),
            idline(1, "file.rs", 2), // depth 1 instead of 2 = reparented
            idline(1, "sibling.rs", 3),
        );
        let ops = reconcile(&baseline, &buffer, &root());
        // No spurious Trash: all 3 ids are present.
        let has_trash = ops.iter().any(|op| matches!(op, FsOp::Trash(_)));
        assert!(
            !has_trash,
            "no Trash expected when all ids are present, got {ops:?}"
        );
        // file.rs is reparented (indent change = move): Rename from mydir/file.rs to root/file.rs.
        assert!(
            ops.contains(&FsOp::Rename {
                from: root().join("mydir").join("file.rs"),
                to: root().join("file.rs"),
            }),
            "mangled indent should produce Rename (reparent), got {ops:?}"
        );
    }

    // ── whitespace names ──────────────────────────────────────────────────────

    /// Names with internal spaces and trailing spaces are preserved verbatim.
    #[test]
    fn whitespace_names_preserved() {
        let baseline = make_baseline(&[]);
        // Two new lines without ids: names with spaces.
        // "a b.txt" has an internal space; "trailing .txt" has a trailing space
        // (note: the trailing space precedes the \n, so it's part of the name).
        let buffer = format!("{}\n    a b.txt\n    trailing .txt", root_header());
        let ops = reconcile(&baseline, &buffer, &root());
        assert_eq!(ops.len(), 2, "expected 2 CreateFile ops, got {ops:?}");
        // Check that names are preserved verbatim.
        assert!(
            ops.contains(&FsOp::CreateFile(root().join("a b.txt"))),
            "must create 'a b.txt', got {ops:?}"
        );
        assert!(
            ops.contains(&FsOp::CreateFile(root().join("trailing .txt"))),
            "must create 'trailing .txt' with trailing space, got {ops:?}"
        );
    }

    // ── conceal byte math ────────────────────────────────────────────────────

    /// Verify that the US byte index + line length are what a Conceal would
    /// cover, and that the visible text (left of US) is the indent+name.
    #[test]
    fn conceal_byte_positions() {
        let line = format!("  x{}{}", ID_SEP, 5);
        // Find the US byte position.
        let us_byte = line.find(ID_SEP).expect("US must be present");
        // The visible text is everything before US.
        let visible = &line[..us_byte];
        assert_eq!(visible, "  x", "visible text must be indent+name");
        // The conceal covers [us_byte .. line.len()].
        assert_eq!(us_byte, 3, "US at byte 3 for '  x'");
        assert_eq!(line.len(), 3 + ID_SEP.len_utf8() + 1, "total line length");
        // Conceal end = line.len() in bytes.
        let conceal_end = line.len();
        // Everything from us_byte to conceal_end is the tail (US + id digits).
        let tail = &line[us_byte..conceal_end];
        assert!(
            tail.starts_with(ID_SEP),
            "tail must start with ID_SEP, got {tail:?}"
        );
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
