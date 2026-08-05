//! Pure reconcile engine for the oil.nvim-style editable file explorer.
//!
//! [`reconcile`] diffs an edited explorer buffer against a baseline snapshot
//! and returns an ordered [`Vec<FsOp>`] that, when applied in order, makes the
//! filesystem match the buffer.  **No filesystem access occurs here** — this is
//! a pure function suitable for exhaustive unit testing before the wiring phase.
//!
//! # View format
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
pub const ID_SEP: char = '\u{1F}';

// ── Op model ──────────────────────────────────────────────────────────────────

/// A single filesystem operation produced by [`reconcile`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsOp {
    /// Create a directory (and any intermediate parents — the wiring phase uses
    /// `create_dir_all`). The bool is `restore`: when `true`, `apply_ops` may
    /// satisfy the create by restoring a same-basename trashed entry (`dd` +
    /// `p` move); when `false` it must always create fresh — a type change at
    /// an unchanged path, where a restore would move the just-trashed entry
    /// back over the path and silently undo the type change.
    CreateDir(PathBuf, bool),
    /// Create an empty file (the wiring phase uses `mkdir -p` on the parent
    /// first so that `a/b.rs` works when `a/` is new). The bool is `restore` —
    /// same contract as [`FsOp::CreateDir`].
    CreateFile(PathBuf, bool),
    /// Move the entry at `from` into the trash directory (see
    /// `crate::app::trash`).  Never a physical delete.
    Trash(PathBuf),
    /// Rename / move `from` to `to`.  Only emitted when `from` and `to` are
    /// the same node-type (both dirs or both files).
    Rename { from: PathBuf, to: PathBuf },
}

// ── View parser ─────────────────────────────────────────────────────────────

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
        // Reject names that would escape the explorer root — `..`, an absolute
        // path, or a drive/root prefix — which `Path::join` would otherwise
        // honor, letting a crafted buffer line create/rename/trash outside the
        // tree. Internal `/` for nested creation (`a/b.rs`) stays allowed.
        if Path::new(name).components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        }) {
            continue;
        }

        // Pop stack entries that are at depth ≥ current depth.
        while stack.last().is_some_and(|(d, _)| *d >= depth) {
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
pub fn reconcile(baseline: &[(u64, PathBuf, bool)], buffer: &str, root: &Path) -> Vec<FsOp> {
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
                    //
                    // The create carries `restore: false`: the Trash above
                    // pushes this same basename into the `trashed` registry,
                    // and restoring would move the OLD entry back over the
                    // path — silently undoing the type change and discarding
                    // the user's edit. A fresh create leaves the old entry in
                    // the trash (a later `p` can still restore it) and the
                    // disk holding the user's edit.
                    trashes.push(FsOp::Trash(bpath.clone()));
                    if entry.is_dir {
                        creates.push(FsOp::CreateDir(entry.path.clone(), false));
                    } else {
                        creates.push(FsOp::CreateFile(entry.path.clone(), false));
                    }
                }
            }
            // No id, unknown id, or duplicate id (yy+p) → create.
            _ => {
                if entry.is_dir {
                    creates.push(FsOp::CreateDir(entry.path.clone(), true));
                } else {
                    creates.push(FsOp::CreateFile(entry.path.clone(), true));
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
        FsOp::CreateDir(p, _) | FsOp::CreateFile(p, _) => component_count(p),
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
pub enum AppliedOp {
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

/// Move whatever is at `src` to `dst`, through the disk-I/O seam
/// ([`hjkl_fs::move_atomic`]: `rename`, and across a filesystem boundary a
/// staged copy-then-delete).
///
/// This replaces the explorer's own pair of movers — `move_file` / `move_dir`,
/// picked at each call site from `from.is_dir()`, with a hand-rolled recursive
/// copy behind `move_dir`'s `CrossesDevices` arm. Every difference bites on
/// that fallback: the path that runs when the entry being moved and the trash
/// under `$XDG_CACHE_HOME` (or the destination of a rename) sit on different
/// filesystems.
///
/// - The fallback copy is **staged** beside its destination and swapped in only
///   once it is complete, so a move that dies partway leaves the destination
///   exactly as it was. The old walk copied in place, so a failure left a
///   half-populated directory that reads as a finished one.
/// - A **fifo, socket or device node** anywhere in the tree is an error rather
///   than a hang. The old walk classified with `symlink_metadata` but had only
///   three branches: anything that was not a symlink or a directory went to
///   `std::fs::copy`, which blocks forever on a fifo waiting for a writer — the
///   editor wedged with no way to cancel it.
/// - **All three shapes** — file, directory tree, symlink — are handled here,
///   so the call sites no longer choose a mover from `is_dir()`, which follows
///   symlinks. A symlink to a directory used to be walked as if it were the
///   tree it points at and then `remove_dir_all`-ed, which copied the target's
///   contents and then failed on the removal. Now the link's *target string* is
///   moved and the data it points at is neither copied nor deleted.
///
/// Destination semantics are deliberately unchanged: the fast path is literally
/// `std::fs::rename`, so exactly the same moves succeed and fail as before. The
/// refusal to overwrite an occupied destination lives in [`apply_ops`], above
/// this function, and stays the only thing standing between a rename and a
/// clobber.
fn move_entry(src: &Path, dst: &Path) -> std::io::Result<()> {
    // `WriteOptions::default()`: durable, and a copied file carries the
    // source's mode. Durability for the same reason `hjkl_app::trash` uses the
    // default — this is the inverse of `move_to_trash`, and an entry the user
    // renamed, or pulled back out of the trash, must still be where they put it
    // after a crash.
    //
    // Not `preserve_mode`: for a file that means "keep the DESTINATION's mode",
    // which is the wrong answer for a move — the entry must keep its own bits.
    hjkl_fs::move_atomic(src, dst, &hjkl_fs::WriteOptions::default())
}

/// True when `a` and `b` resolve to the same existing file — e.g. a pure
/// case-change rename on a case-insensitive filesystem, where the destination
/// "exists" but IS the source. Both sides must exist for a `true`.
fn is_same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => false,
    }
}

/// Pick a non-existing temp sibling path next to `from` for the two-step
/// rename used to break same-batch rename cycles (swap `a↔b`).
fn temp_sibling_path(from: &Path) -> PathBuf {
    let parent = from.parent().unwrap_or_else(|| Path::new("."));
    let base = from
        .file_name()
        .map_or_else(|| "file".to_string(), |n| n.to_string_lossy().into_owned());
    let pid = std::process::id();
    (0u64..)
        .map(|n| parent.join(format!(".hjkl-rename-{pid}-{n}-{base}")))
        .find(|cand| cand.symlink_metadata().is_err())
        .expect("some counter yields a fresh temp name")
}

/// Apply reconcile ops to disk. Deletions go to the trash (recoverable);
/// a `CreateFile` / `CreateDir` whose basename matches a pending trashed entry
/// is **restored** from trash instead of created empty (this is how `dd` then
/// `p` becomes a move) — except when the op carries `restore: false`, which a
/// type change at an unchanged path does, so the just-trashed entry of the same
/// name is not moved back over the user's edit. Returns the paths of genuinely
/// newly-created FILES (to open), the
/// concrete [`AppliedOp`] journal entries (for undo/redo), and a list of error
/// strings from non-fatal op failures (best-effort).
///
/// A `Rename` whose destination already exists (and is not the same file, so
/// case-change renames still work) is **refused** with an error — never a
/// silent clobber — unless a *later op in the same batch* vacates the
/// destination (name swap `a↔b`, or rename-onto-trashed). Those are routed
/// through a unique temp sibling name: `from → temp` now, `temp → to` after
/// the batch. The temp hop is journaled as two `Renamed` entries so undo/redo
/// replay it without clobbering.
///
/// # Arguments
/// - `ops`: output of [`reconcile`], already in renames→trashes→creates order.
/// - `trashed`: mutable registry of `(original_file_name, trash_dest)` pairs
///   built up by this call and carried across reconcile cycles so that a
///   `Trash` on tick N and a `CreateFile` on tick N+1 correctly restores.
/// - `trash_root`: where trashed entries go. Production passes
///   [`hjkl_app::trash::TrashRoot::Xdg`] (via `ExplorerPane::trash_root`);
///   tests pass an explicit directory so they never have to override
///   `XDG_CACHE_HOME`, which is process-global and therefore visible to every
///   other thread in the test binary.
pub fn apply_ops(
    ops: &[FsOp],
    trashed: &mut Vec<(String, PathBuf)>,
    trash_root: &hjkl_app::trash::TrashRoot,
) -> (Vec<PathBuf>, Vec<AppliedOp>, Vec<String>) {
    let mut created: Vec<PathBuf> = Vec::new();
    let mut applied: Vec<AppliedOp> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    // Paths this batch will vacate later (rename sources + trash targets).
    // A rename whose occupied destination is in this set is a same-batch
    // swap/cycle — parked at a temp name and finalized after the loop.
    let will_vacate: HashSet<&Path> = ops
        .iter()
        .filter_map(|op| match op {
            FsOp::Rename { from, .. } => Some(from.as_path()),
            FsOp::Trash(p) => Some(p.as_path()),
            _ => None,
        })
        .collect();
    // Parked temp hops: (original from, temp holding it, final destination).
    let mut deferred: Vec<(PathBuf, PathBuf, PathBuf)> = Vec::new();

    for op in ops {
        match op {
            FsOp::Rename { from, to } => {
                // Skip if the source is already gone AND the dest already
                // exists — an ancestor directory rename already moved it.
                if !from.exists() && to.exists() {
                    continue;
                }
                // Occupied destination (`symlink_metadata` also catches a
                // dangling symlink squatting on the name). A pure case-change
                // rename resolves to the same file — let it through.
                if to.symlink_metadata().is_ok() && !is_same_file(from, to) {
                    if will_vacate.contains(to.as_path()) {
                        // Same-batch swap/cycle: a later op moves `to` away.
                        // Park `from` under a temp sibling; finish after the
                        // batch. Journal the hop so undo replays it exactly.
                        let temp = temp_sibling_path(from);
                        match move_entry(from, &temp) {
                            Ok(()) => {
                                applied.push(AppliedOp::Renamed {
                                    from: from.clone(),
                                    to: temp.clone(),
                                });
                                deferred.push((from.clone(), temp, to.clone()));
                            }
                            Err(e) => {
                                errors.push(format!(
                                    "rename {from:?} → {to:?}: park at {temp:?} failed: {e}"
                                ));
                            }
                        }
                    } else {
                        errors.push(format!(
                            "rename {from:?} → {to:?}: destination exists — refusing to overwrite"
                        ));
                    }
                    continue;
                }
                if let Some(parent) = to.parent()
                    && let Err(e) = std::fs::create_dir_all(parent)
                {
                    errors.push(format!("rename: create_dir_all({parent:?}): {e}"));
                    continue;
                }
                match move_entry(from, to) {
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
                let is_dir = path.is_dir();
                let dest = match hjkl_app::trash::trash_path_in(trash_root, path, is_dir) {
                    Ok(d) => d,
                    Err(e) => {
                        errors.push(format!("trash_path({path:?}): {e}"));
                        continue;
                    }
                };
                // The trash is under `$XDG_CACHE_HOME` and `path` can be on any
                // filesystem, so this move can cross a device boundary; the seam
                // is what keeps the entry from being lost when it does.
                let result = hjkl_app::trash::move_to_trash(path, &dest);
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

            FsOp::CreateDir(path, restore) => {
                // Like CreateFile, restore a trashed entry of the same name —
                // restoring the WHOLE subtree. This is what makes moving a
                // *collapsed* directory lossless: with the lazy explorer the
                // dir's children aren't in the buffer, so `dd` emits only
                // `Trash(dir)` and `p` only `CreateDir(dir)`; restoring the
                // trashed dir wholesale brings its contents along.
                let dir_name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                // `restore: false` (type change at an unchanged path) skips
                // the trash lookup entirely and creates fresh: a restore would
                // move the just-trashed entry of the same basename back over
                // the path, silently undoing the type change. The entry stays
                // in the registry so a later `p` can still restore it.
                let restore_idx = if *restore {
                    trashed
                        .iter()
                        .enumerate()
                        .rev()
                        .find(|(_, (name, _))| name == &dir_name)
                        .map(|(i, _)| i)
                } else {
                    None
                };

                if let Some(parent) = path.parent()
                    && let Err(e) = std::fs::create_dir_all(parent)
                {
                    errors.push(format!("create_dir: create_dir_all({parent:?}): {e}"));
                    continue;
                }

                if let Some(idx) = restore_idx {
                    let (_, trash_dest) = trashed.remove(idx);
                    match move_entry(&trash_dest, path) {
                        Ok(()) => {
                            applied.push(AppliedOp::Restored {
                                from_trash: trash_dest,
                                to: path.clone(),
                            });
                        }
                        Err(e) => {
                            errors.push(format!("restore dir {trash_dest:?} → {path:?}: {e}"));
                        }
                    }
                } else if let Err(e) = std::fs::create_dir_all(path) {
                    errors.push(format!("create_dir_all({path:?}): {e}"));
                } else {
                    applied.push(AppliedOp::Created(path.clone()));
                }
            }

            FsOp::CreateFile(path, restore) => {
                // Check whether a trashed entry can be restored here.
                let file_name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();

                // `restore: false` (type change at an unchanged path) skips
                // the trash lookup entirely and creates fresh — see the
                // CreateDir arm.
                let restore_idx = if *restore {
                    trashed
                        .iter()
                        .enumerate()
                        .rev()
                        .find(|(_, (name, _))| name == &file_name)
                        .map(|(i, _)| i)
                } else {
                    None
                };

                if let Some(parent) = path.parent()
                    && let Err(e) = std::fs::create_dir_all(parent)
                {
                    errors.push(format!("create_file: create_dir_all({parent:?}): {e}"));
                    continue;
                }

                if let Some(idx) = restore_idx {
                    let (_, trash_dest) = trashed.remove(idx);
                    // Restore from trash.
                    match move_entry(&trash_dest, path) {
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

    // Finalize parked temp hops: the vacating op has run, so `to` should be
    // free now. If it is not (the vacating op failed), move the parked file
    // back to its original name — never clobber, never leave a temp behind.
    for (orig_from, temp, to) in deferred {
        if to.symlink_metadata().is_ok() {
            errors.push(format!(
                "rename {orig_from:?} → {to:?}: destination still exists — keeping original name"
            ));
            match move_entry(&temp, &orig_from) {
                Ok(()) => {
                    // Drop the park journal entry — the disk is back to where
                    // it started, so undo must not replay the hop.
                    if let Some(pos) = applied.iter().rposition(|a| {
                        matches!(a, AppliedOp::Renamed { from, to }
                            if from == &orig_from && to == &temp)
                    }) {
                        applied.remove(pos);
                    }
                }
                Err(e) => {
                    errors.push(format!("restore {temp:?} → {orig_from:?}: {e}"));
                }
            }
            continue;
        }
        match move_entry(&temp, &to) {
            Ok(()) => {
                applied.push(AppliedOp::Renamed {
                    from: temp.clone(),
                    to: to.clone(),
                });
            }
            Err(e) => {
                errors.push(format!("rename {temp:?} → {to:?}: {e}"));
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
/// destinations are created during the reversal. `trash_root` is where new
/// trash destinations are reserved — see [`apply_ops`].
pub fn revert_ops(
    applied: &[AppliedOp],
    trashed: &mut Vec<(String, PathBuf)>,
    trash_root: &hjkl_app::trash::TrashRoot,
) -> (Vec<AppliedOp>, Vec<String>) {
    let mut redo_journal: Vec<AppliedOp> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    // Reverse-iterate so the last op is undone first (symmetrical with apply
    // order: if we created dir/file in order A→B, undo is B→A).
    for op in applied.iter().rev() {
        match op {
            // A file/dir was created → trash it to undo.
            AppliedOp::Created(path) => {
                let is_dir = path.is_dir();
                let dest = match hjkl_app::trash::trash_path_in(trash_root, path, is_dir) {
                    Ok(d) => d,
                    Err(e) => {
                        errors.push(format!("revert created: trash_path({path:?}): {e}"));
                        continue;
                    }
                };
                let result = hjkl_app::trash::move_to_trash(path, &dest);
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
                match move_entry(dest, original) {
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
                match move_entry(to, from) {
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
                let is_dir = to.is_dir();
                let new_dest = match hjkl_app::trash::trash_path_in(trash_root, to, is_dir) {
                    Ok(d) => d,
                    Err(e) => {
                        errors.push(format!("revert restored: trash_path({to:?}): {e}"));
                        continue;
                    }
                };
                let result = hjkl_app::trash::move_to_trash(to, &new_dest);
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
/// `trash_root` is where re-trashed entries are reserved — see [`apply_ops`].
pub fn apply_applied(
    ops: &[AppliedOp],
    trashed: &mut Vec<(String, PathBuf)>,
    trash_root: &hjkl_app::trash::TrashRoot,
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
                let is_dir = original.is_dir();
                let new_dest = match hjkl_app::trash::trash_path_in(trash_root, original, is_dir) {
                    Ok(d) => d,
                    Err(e) => {
                        errors.push(format!("redo trashed: trash_path({original:?}): {e}"));
                        continue;
                    }
                };
                let result = hjkl_app::trash::move_to_trash(original, &new_dest);
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
                match move_entry(from, to) {
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
                match move_entry(from_trash, to) {
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
                path.file_name().map_or_else(
                    || path.to_string_lossy().into_owned(),
                    |n| n.to_string_lossy().into_owned(),
                )
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

    // ── path-escape guard ─────────────────────────────────────────────────────

    /// A crafted buffer line with a `..` (or absolute) name must never emit an
    /// op whose target escapes the explorer root.
    #[test]
    fn reconcile_rejects_root_escape() {
        let baseline = make_baseline(&[("keep.rs", false)]);
        // root + existing file (id 1) + two malicious new (no-id) lines.
        let evil_rel = format!("{}../evil", " ".repeat(4)); // depth-1, `..`
        let evil_abs = format!("{}/etc/passwd", " ".repeat(4)); // depth-1, absolute
        let buffer = format!(
            "{}\n{}\n{}\n{}",
            root_header(),
            idline(1, "keep.rs", 1),
            evil_rel,
            evil_abs,
        );
        let ops = reconcile(&baseline, &buffer, &root());
        for op in &ops {
            let paths: Vec<&PathBuf> = match op {
                FsOp::CreateFile(p, _) | FsOp::CreateDir(p, _) | FsOp::Trash(p) => vec![p],
                FsOp::Rename { from, to } => vec![from, to],
            };
            for p in paths {
                assert!(
                    p.starts_with(root()),
                    "op escaped explorer root: {op:?} (path {p:?})"
                );
            }
        }
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
        assert!(ops.contains(&FsOp::CreateFile(root().join("new_a.rs"), true)));
        assert!(ops.contains(&FsOp::CreateFile(root().join("new_b.rs"), true)));
        assert!(ops.contains(&FsOp::CreateFile(root().join("new_c.rs"), true)));
        assert!(
            ops.iter().all(|op| matches!(op, FsOp::CreateFile(_, _))),
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
        assert_eq!(ops, vec![FsOp::CreateDir(root().join("newdir"), true)]);
    }

    // ── create_nested ─────────────────────────────────────────────────────────

    /// New line `a/b.rs` at depth-1 indent (no id) ⇒ CreateFile(root/a/b.rs).
    #[test]
    fn create_nested() {
        let baseline = make_baseline(&[]);
        let buffer = format!("{}\n    a/b.rs", root_header());
        let ops = reconcile(&baseline, &buffer, &root());
        assert_eq!(
            ops,
            vec![FsOp::CreateFile(root().join("a").join("b.rs"), true)]
        );
    }

    // ── delete_to_trash ───────────────────────────────────────────────────────

    /// baseline has a file (id=1); buffer removes that line ⇒ Trash(path).
    #[test]
    fn delete_to_trash() {
        let baseline = make_baseline(&[("to_delete.rs", false)]);
        // View: root line only — file line omitted → id=1 not seen → Trash.
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
                !matches!(op, FsOp::CreateFile(_, _) | FsOp::CreateDir(_, _)),
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
        let has_create = ops.contains(&FsOp::CreateFile(root().join("added.rs"), true));

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
            .position(|op| matches!(op, FsOp::CreateFile(p, _) if p == &root().join("added.rs")))
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
            ops.contains(&FsOp::CreateDir(root().join("thing"), false)),
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
            ops.contains(&FsOp::CreateFile(root().join("thing"), false)),
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
        // View: both ids missing → both trashed.
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
            .position(|op| matches!(op, FsOp::CreateDir(p, _) if p == &root().join("newdir")))
            .expect("CreateDir(newdir) must be present");
        let file_pos = ops
            .iter()
            .position(|op| matches!(op, FsOp::CreateFile(p, _) if p == &root().join("newdir").join("newfile.rs")))
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
        // View has blank lines interspersed; foo.rs carries its id.
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
        assert!(ops.iter().all(|op| matches!(op, FsOp::CreateFile(_, _))));
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
            ops.contains(&FsOp::CreateFile(root().join("orig.rs"), true)),
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
        // View AFTER `dd` on the open `mydir/` line: the dir line is gone, but
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
        // View: mydir/ id=1, sibling.rs id=3; file.rs id=2 moved to wrong indent
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
            ops.contains(&FsOp::CreateFile(root().join("a b.txt"), true)),
            "must create 'a b.txt', got {ops:?}"
        );
        assert!(
            ops.contains(&FsOp::CreateFile(root().join("trailing .txt"), true)),
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

    /// A trash root under `td`, for the `apply_ops` / `revert_ops` /
    /// `apply_applied` calls below.
    ///
    /// This used to override `XDG_CACHE_HOME` behind a mutex. The variable is
    /// process-global, so the override was visible to every other thread in the
    /// binary — including all the tests that never took that mutex and merely
    /// *read* the environment (any that construct an `App` resolve a swap
    /// directory under it). Naming the directory outright removes the shared
    /// resource instead of taking a lock on it, and leaves the environment
    /// untouched. Same location as before: `<td>/hjkl/trash`.
    fn isolated_trash(td: &tempfile::TempDir) -> hjkl_app::trash::TrashRoot {
        hjkl_app::trash::TrashRoot::At(td.path().join("hjkl").join("trash"))
    }

    #[test]
    fn apply_create_file_makes_empty_file() {
        let td = tempfile::tempdir().unwrap();
        let trash = isolated_trash(&td);
        let target = td.path().join("new.rs");
        let ops = vec![FsOp::CreateFile(target.clone(), true)];
        let (created, applied, errors) = apply_ops(&ops, &mut Vec::new(), &trash);
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
        let trash = isolated_trash(&td);
        let target = td.path().join("a").join("b").join("c.rs");
        let ops = vec![FsOp::CreateFile(target.clone(), true)];
        let (created, applied, errors) = apply_ops(&ops, &mut Vec::new(), &trash);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(created, vec![target.clone()]);
        assert_eq!(applied.len(), 1);
        assert!(target.exists(), "nested file must exist");
    }

    #[test]
    fn apply_trash_moves_into_trash_not_deleted() {
        let td = tempfile::tempdir().unwrap();
        let trash = isolated_trash(&td);
        // Create the source file.
        let src = td.path().join("source.txt");
        std::fs::write(&src, b"hello").unwrap();
        let ops = vec![FsOp::Trash(src.clone())];
        let mut trashed = Vec::new();
        let (created, applied, errors) = apply_ops(&ops, &mut trashed, &trash);
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
        let trash = isolated_trash(&td);
        let foo = td.path().join("foo.rs");
        std::fs::write(&foo, b"hi").unwrap();
        let bar = td.path().join("bar.rs");
        let ops = vec![FsOp::Rename {
            from: foo.clone(),
            to: bar.clone(),
        }];
        let (created, applied, errors) = apply_ops(&ops, &mut Vec::new(), &trash);
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

    /// A rename whose destination is an existing, different file must be
    /// REFUSED (error reported, nothing journaled) — never a silent clobber.
    /// Pre-fix this destroyed the target's content unrecoverably.
    #[test]
    fn apply_rename_onto_existing_file_is_refused() {
        let td = tempfile::tempdir().unwrap();
        let trash = isolated_trash(&td);
        let a = td.path().join("a.txt");
        let b = td.path().join("b.txt");
        std::fs::write(&a, b"AAA").unwrap();
        std::fs::write(&b, b"BBB").unwrap();
        let ops = vec![FsOp::Rename {
            from: a.clone(),
            to: b.clone(),
        }];
        let (created, applied, errors) = apply_ops(&ops, &mut Vec::new(), &trash);
        assert!(created.is_empty());
        assert!(
            applied.is_empty(),
            "refused rename must journal nothing, got {applied:?}"
        );
        assert_eq!(
            errors.len(),
            1,
            "refusal must be reported to the user, got {errors:?}"
        );
        assert_eq!(std::fs::read(&a).unwrap(), b"AAA", "source must be intact");
        assert_eq!(
            std::fs::read(&b).unwrap(),
            b"BBB",
            "target content must NOT be clobbered"
        );
    }

    /// Swapping two filenames in one explorer edit emits `Rename a→b` +
    /// `Rename b→a` in one batch. Both must succeed via a temp hop, with each
    /// file's CONTENT following its new name. Pre-fix the first rename
    /// clobbered b, then the second propagated a's content into both names —
    /// b's content was permanently lost.
    #[test]
    fn apply_swap_two_files_in_one_batch() {
        let td = tempfile::tempdir().unwrap();
        let trash = isolated_trash(&td);
        let a = td.path().join("a.txt");
        let b = td.path().join("b.txt");
        std::fs::write(&a, b"AAA").unwrap();
        std::fs::write(&b, b"BBB").unwrap();
        let ops = vec![
            FsOp::Rename {
                from: a.clone(),
                to: b.clone(),
            },
            FsOp::Rename {
                from: b.clone(),
                to: a.clone(),
            },
        ];
        let mut trashed = Vec::new();
        let (created, applied, errors) = apply_ops(&ops, &mut trashed, &trash);
        assert!(errors.is_empty(), "swap must succeed, got {errors:?}");
        assert!(created.is_empty());
        assert_eq!(
            std::fs::read(&a).unwrap(),
            b"BBB",
            "a must now hold b's old content"
        );
        assert_eq!(
            std::fs::read(&b).unwrap(),
            b"AAA",
            "b must now hold a's old content"
        );
        // No stray temp-hop files left behind. (Other concurrent tests may
        // drop unrelated entries — e.g. the XDG cache dir — into this tempdir,
        // so only check for our `.hjkl-rename-*` names.)
        let names: Vec<String> = std::fs::read_dir(td.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            names.iter().all(|n| !n.starts_with(".hjkl-rename-")),
            "no temp-hop files may remain, got {names:?}"
        );
        // Undo must restore the original contents.
        let (_redo, errs) = revert_ops(&applied, &mut trashed, &trash);
        assert!(errs.is_empty(), "undo of swap: {errs:?}");
        assert_eq!(std::fs::read(&a).unwrap(), b"AAA", "undo restores a");
        assert_eq!(std::fs::read(&b).unwrap(), b"BBB", "undo restores b");
    }

    /// Rename `a→b` while `b` is trashed in the same batch (user deleted b's
    /// line and renamed a to b). Reconcile orders renames before trashes, so
    /// the rename's target is still occupied — it must defer through the temp
    /// hop until the Trash vacates `b`, not clobber and not refuse.
    #[test]
    fn apply_rename_onto_trashed_target_in_one_batch() {
        let td = tempfile::tempdir().unwrap();
        let trash = isolated_trash(&td);
        let a = td.path().join("a.txt");
        let b = td.path().join("b.txt");
        std::fs::write(&a, b"AAA").unwrap();
        std::fs::write(&b, b"BBB").unwrap();
        let ops = vec![
            FsOp::Rename {
                from: a.clone(),
                to: b.clone(),
            },
            FsOp::Trash(b.clone()),
        ];
        let mut trashed = Vec::new();
        let (_, _, errors) = apply_ops(&ops, &mut trashed, &trash);
        assert!(errors.is_empty(), "must succeed, got {errors:?}");
        assert!(!a.exists(), "a was renamed away");
        assert_eq!(
            std::fs::read(&b).unwrap(),
            b"AAA",
            "b must hold a's content after the trash vacated it"
        );
        assert_eq!(trashed.len(), 1, "old b must be in the trash");
        assert_eq!(
            std::fs::read(&trashed[0].1).unwrap(),
            b"BBB",
            "old b content must be recoverable from trash"
        );
    }

    /// The ancestor-rename skip must survive the clobber guard: when a dir
    /// rename already moved a child, the child's own Rename op (source gone,
    /// dest present) is silently skipped — not refused, not an error.
    #[test]
    fn apply_ancestor_rename_skip_still_works() {
        let td = tempfile::tempdir().unwrap();
        let trash = isolated_trash(&td);
        let olddir = td.path().join("olddir");
        std::fs::create_dir_all(&olddir).unwrap();
        std::fs::write(olddir.join("child.txt"), b"C").unwrap();
        let newdir = td.path().join("newdir");
        let ops = vec![
            FsOp::Rename {
                from: olddir.clone(),
                to: newdir.clone(),
            },
            FsOp::Rename {
                from: olddir.join("child.txt"),
                to: newdir.join("child.txt"),
            },
        ];
        let (_, applied, errors) = apply_ops(&ops, &mut Vec::new(), &trash);
        assert!(
            errors.is_empty(),
            "ancestor skip must not error: {errors:?}"
        );
        assert_eq!(
            applied.len(),
            1,
            "only the dir rename is journaled (child op skipped), got {applied:?}"
        );
        assert_eq!(
            std::fs::read(newdir.join("child.txt")).unwrap(),
            b"C",
            "child moved with its dir"
        );
    }

    #[test]
    fn apply_move_via_trash_then_create_restores_content() {
        let td = tempfile::tempdir().unwrap();
        let trash = isolated_trash(&td);
        // Set up: foo.rs in src_dir with content "x".
        let src_dir = td.path().join("src_dir");
        std::fs::create_dir_all(&src_dir).unwrap();
        let foo = src_dir.join("foo.rs");
        std::fs::write(&foo, b"x").unwrap();

        // Step 1: Trash foo.rs.
        let mut trashed: Vec<(String, PathBuf)> = Vec::new();
        let trash_ops = vec![FsOp::Trash(foo.clone())];
        let (c, _a1, e) = apply_ops(&trash_ops, &mut trashed, &trash);
        assert!(e.is_empty(), "trash must succeed: {e:?}");
        assert!(c.is_empty());
        assert_eq!(trashed.len(), 1);
        assert!(!foo.exists());

        // Step 2: CreateFile at dir2/foo.rs — should restore from trash.
        let dir2 = td.path().join("dir2");
        let dest = dir2.join("foo.rs");
        let create_ops = vec![FsOp::CreateFile(dest.clone(), true)];
        let (created, applied, errors) = apply_ops(&create_ops, &mut trashed, &trash);
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

    /// Moving a *collapsed* directory (`Trash(dir)` then `CreateDir(dir)` with no
    /// per-child ops, as the lazy explorer emits) restores the WHOLE subtree from
    /// trash — contents preserved — rather than creating an empty directory.
    #[test]
    fn apply_create_dir_restores_trashed_subtree() {
        let td = tempfile::tempdir().unwrap();
        let trash = isolated_trash(&td);
        // src/mover/inner.txt — `mover` is the dir being moved while collapsed.
        let mover = td.path().join("mover");
        std::fs::create_dir_all(&mover).unwrap();
        std::fs::write(mover.join("inner.txt"), b"deep").unwrap();

        let mut trashed: Vec<(String, PathBuf)> = Vec::new();
        // Trash the dir (its contents go to trash with it).
        let (_, _, e) = apply_ops(&[FsOp::Trash(mover.clone())], &mut trashed, &trash);
        assert!(e.is_empty(), "trash dir: {e:?}");
        assert!(!mover.exists());
        assert_eq!(trashed.len(), 1);

        // CreateDir at a new parent — must restore the trashed `mover` wholesale.
        let dest = td.path().join("target").join("mover");
        let (created, applied, errors) =
            apply_ops(&[FsOp::CreateDir(dest.clone(), true)], &mut trashed, &trash);
        assert!(errors.is_empty(), "restore dir: {errors:?}");
        assert!(created.is_empty(), "restored dir is not a fresh create");
        assert!(trashed.is_empty(), "trashed registry drained");
        assert!(dest.is_dir(), "destination dir must exist");
        assert_eq!(
            std::fs::read(dest.join("inner.txt")).unwrap(),
            b"deep",
            "the dir's contents must survive the move (collapsed-dir move is lossless)"
        );
        assert!(
            matches!(&applied[0], AppliedOp::Restored { to, .. } if to == &dest),
            "must record Restored AppliedOp for the dir"
        );
    }

    /// A type change at an UNCHANGED path must not be silently undone by the
    /// trash registry. Reconcile emits `Trash(thing)` + `CreateDir(thing)`
    /// (restore=false) in one batch; `apply_ops` runs the trash first, so the
    /// create's basename matches a fresh trashed entry — but with `restore:
    /// false` it must create a NEW EMPTY directory instead of moving the old
    /// file back over the path. The old file stays in the trash, recoverable.
    #[test]
    fn apply_type_change_file_to_dir_creates_fresh_not_restores() {
        let td = tempfile::tempdir().unwrap();
        let trash = isolated_trash(&td);
        let root = td.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        // Baseline: a file at `thing` (id 1) with content.
        let p = root.join("thing");
        std::fs::write(&p, b"old file contents").unwrap();
        let baseline = vec![(0u64, root.clone(), true), (1u64, p.clone(), false)];
        // Buffer: the same id, same path, but with a dir marker.
        let buffer = format!("{}\n{}", root_header(), idline(1, "thing/", 1));

        let ops = reconcile(&baseline, &buffer, &root);
        // The type-change create must carry `restore: false`.
        assert!(ops.contains(&FsOp::Trash(p.clone())));
        assert!(
            ops.contains(&FsOp::CreateDir(p.clone(), false)),
            "type-change create must be a fresh CreateDir, got {ops:?}"
        );
        assert_eq!(ops.len(), 2, "exactly Trash + CreateDir, got {ops:?}");

        let mut trashed = Vec::new();
        let (created, applied, errors) = apply_ops(&ops, &mut trashed, &trash);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert!(created.is_empty(), "a dir create is not a file create");
        // P is a fresh EMPTY directory — the old file did not come back.
        let meta = std::fs::metadata(&p).expect("P must exist after the type change");
        assert!(
            meta.is_dir(),
            "P must be a directory, not the restored file"
        );
        assert_eq!(
            std::fs::read_dir(&p).unwrap().count(),
            0,
            "the new directory must be empty"
        );
        // The old file is still in the trash, intact and recoverable.
        assert_eq!(trashed.len(), 1, "the old file must stay in the trash");
        let (name, dest) = &trashed[0];
        assert_eq!(name, "thing");
        assert_eq!(
            std::fs::read(dest).unwrap(),
            b"old file contents",
            "trashed file must be recoverable"
        );
        // Journal: Trashed first, then a FRESH Created (never Restored).
        assert!(
            matches!(&applied[0], AppliedOp::Trashed { original, .. } if original == &p),
            "Trash must be journaled first, got {applied:?}"
        );
        assert!(
            matches!(&applied[1], AppliedOp::Created(c) if c == &p),
            "the dir create must be a fresh Created (not Restored), got {applied:?}"
        );
    }

    /// The `dd` + `p` flow emits `Trash(thing.rs)` + `CreateFile(thing.rs)` in
    /// ONE batch (same path, restore=true) — the create must still be satisfied
    /// from the trash registry, restoring the entry wholesale instead of
    /// creating an empty file. This is the constraint the type-change fix must
    /// not break.
    #[test]
    fn apply_trash_then_create_same_batch_restores_dd_p() {
        let td = tempfile::tempdir().unwrap();
        let trash = isolated_trash(&td);
        let root = td.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        let p = root.join("thing.rs");
        std::fs::write(&p, b"move me").unwrap();
        // Baseline: file at `thing.rs` (id 1); buffer pastes it back with NO id
        // (dd then p) — reconcile emits Trash + CreateFile(restore=true).
        let baseline = vec![(0u64, root.clone(), true), (1u64, p.clone(), false)];
        let buffer = format!("{}\n    thing.rs", root_header());
        let ops = reconcile(&baseline, &buffer, &root);
        assert!(ops.contains(&FsOp::Trash(p.clone())));
        assert!(
            ops.contains(&FsOp::CreateFile(p.clone(), true)),
            "dd+p create must carry restore=true, got {ops:?}"
        );

        let mut trashed = Vec::new();
        let (created, applied, errors) = apply_ops(&ops, &mut trashed, &trash);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert!(
            created.is_empty(),
            "restored file must NOT appear in created"
        );
        assert!(
            trashed.is_empty(),
            "trash registry must be drained by the restore"
        );
        assert_eq!(
            std::fs::read(&p).unwrap(),
            b"move me",
            "dd+p must restore the entry, not create an empty file"
        );
        assert!(
            matches!(&applied[1], AppliedOp::Restored { to, .. } if to == &p),
            "the create must be journaled as Restored, got {applied:?}"
        );
    }

    // ── revert_ops round-trip tests ───────────────────────────────────────────

    #[test]
    fn revert_create_removes_file_redo_recreates() {
        let td = tempfile::tempdir().unwrap();
        let trash = isolated_trash(&td);
        let target = td.path().join("round.rs");

        // Apply: create
        let ops = vec![FsOp::CreateFile(target.clone(), true)];
        let mut trashed = Vec::new();
        let (_, applied, errors) = apply_ops(&ops, &mut trashed, &trash);
        assert!(errors.is_empty());
        assert!(target.exists(), "file must exist before revert");

        // Revert (undo)
        let (redo_journal, errs) = revert_ops(&applied, &mut trashed, &trash);
        assert!(errs.is_empty(), "revert errors: {errs:?}");
        assert!(!target.exists(), "file must be gone after revert");

        // Redo: re-apply the redo journal
        let (_, _redo_applied, errs2) = apply_applied(&redo_journal, &mut trashed, &trash);
        assert!(errs2.is_empty(), "redo errors: {errs2:?}");
        assert!(target.exists(), "file must exist again after redo");
    }

    #[test]
    fn revert_trash_restores_file_redo_retrashes() {
        let td = tempfile::tempdir().unwrap();
        let trash = isolated_trash(&td);
        let src = td.path().join("restore_me.txt");
        std::fs::write(&src, b"data").unwrap();

        // Apply: trash
        let ops = vec![FsOp::Trash(src.clone())];
        let mut trashed = Vec::new();
        let (_, applied, errors) = apply_ops(&ops, &mut trashed, &trash);
        assert!(errors.is_empty());
        assert!(!src.exists(), "must be trashed");

        // Revert (undo): restore from trash
        let (redo_journal, errs) = revert_ops(&applied, &mut trashed, &trash);
        assert!(errs.is_empty(), "revert errors: {errs:?}");
        assert!(src.exists(), "file must be back on disk after revert");
        assert_eq!(
            std::fs::read(&src).unwrap(),
            b"data",
            "content must survive"
        );

        // Redo: re-trash
        let (_, _redo_applied, errs2) = apply_applied(&redo_journal, &mut trashed, &trash);
        assert!(errs2.is_empty(), "redo errors: {errs2:?}");
        assert!(!src.exists(), "file must be trashed again after redo");
    }

    #[test]
    fn revert_rename_swaps_back_redo_renames_again() {
        let td = tempfile::tempdir().unwrap();
        let trash = isolated_trash(&td);
        let foo = td.path().join("orig.rs");
        let bar = td.path().join("renamed.rs");
        std::fs::write(&foo, b"content").unwrap();

        // Apply: rename
        let ops = vec![FsOp::Rename {
            from: foo.clone(),
            to: bar.clone(),
        }];
        let mut trashed = Vec::new();
        let (_, applied, errors) = apply_ops(&ops, &mut trashed, &trash);
        assert!(errors.is_empty());
        assert!(!foo.exists() && bar.exists(), "rename must have happened");

        // Revert (undo)
        let (redo_journal, errs) = revert_ops(&applied, &mut trashed, &trash);
        assert!(errs.is_empty(), "revert errors: {errs:?}");
        assert!(
            foo.exists() && !bar.exists(),
            "must be back to orig after revert"
        );

        // Redo
        let (_, _, errs2) = apply_applied(&redo_journal, &mut trashed, &trash);
        assert!(errs2.is_empty(), "redo errors: {errs2:?}");
        assert!(!foo.exists() && bar.exists(), "redo must rename again");
    }

    #[test]
    fn revert_restore_retrashes_redo_restores() {
        let td = tempfile::tempdir().unwrap();
        let trash = isolated_trash(&td);
        let src = td.path().join("moved.txt");
        std::fs::write(&src, b"hello").unwrap();

        // Step 1: trash it
        let mut trashed: Vec<(String, PathBuf)> = Vec::new();
        let (_, applied_trash, e) = apply_ops(&[FsOp::Trash(src.clone())], &mut trashed, &trash);
        assert!(e.is_empty());
        assert!(!src.exists());

        // Step 2: restore to a new location (simulate dd + p move)
        let dest = td.path().join("dest_dir").join("moved.txt");
        let (_, applied_restore, e2) = apply_ops(
            &[FsOp::CreateFile(dest.clone(), true)],
            &mut trashed,
            &trash,
        );
        assert!(e2.is_empty());
        assert!(dest.exists(), "restored file must exist at dest");

        // Combined applied journal for the move
        let mut all_applied = applied_trash;
        all_applied.extend(applied_restore);

        // Revert (undo): dest must be trashed, src does NOT come back (only dest→trash)
        let (redo_journal, errs) = revert_ops(&all_applied, &mut trashed, &trash);
        assert!(errs.is_empty(), "revert errors: {errs:?}");
        assert!(!dest.exists(), "dest must be trashed after revert");

        // Redo: restore dest from trash again
        let (_, _, errs2) = apply_applied(&redo_journal, &mut trashed, &trash);
        assert!(errs2.is_empty(), "redo errors: {errs2:?}");
        assert!(dest.exists(), "dest must be restored again after redo");
    }

    #[test]
    fn revert_restore_dir_retrashes_redo_restores() {
        let td = tempfile::tempdir().unwrap();
        let trash = isolated_trash(&td);
        let dir = td.path().join("mydir");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("inner.txt"), b"deep").unwrap();

        // Step 1: trash the dir.
        let mut trashed: Vec<(String, PathBuf)> = Vec::new();
        let (_, _trash_applied, e) = apply_ops(&[FsOp::Trash(dir.clone())], &mut trashed, &trash);
        assert!(e.is_empty(), "trash dir: {e:?}");
        assert!(!dir.exists());
        assert_eq!(trashed.len(), 1);
        let trash_path = trashed[0].1.clone();

        // Build a Restored op the same way apply_ops would.
        let restored = AppliedOp::Restored {
            from_trash: trash_path,
            to: dir.clone(),
        };

        // Step 2: apply_applied (redo) — restore dir from trash.
        let (_, redo_applied, errs) = apply_applied(&[restored], &mut trashed, &trash);
        assert!(errs.is_empty(), "apply_applied dir: {errs:?}");
        assert!(dir.is_dir(), "dir must be restored after apply_applied");
        assert!(dir.join("inner.txt").exists(), "contents must survive");

        // Step 3: revert_ops (undo) — trash dir again.
        let (redo_journal, errs) = revert_ops(&redo_applied, &mut trashed, &trash);
        assert!(errs.is_empty(), "revert dir: {errs:?}");
        assert!(!dir.exists(), "dir must be trashed again after revert");

        // Step 4: apply_applied (redo again) — restore from the new trash path.
        let (_, _, errs) = apply_applied(&redo_journal, &mut trashed, &trash);
        assert!(errs.is_empty(), "redo dir: {errs:?}");
        assert!(dir.is_dir(), "dir must be restored again after redo");
        assert!(dir.join("inner.txt").exists(), "contents must survive redo");
    }

    /// The cross-device fallback of [`move_entry`] for a directory restore must
    /// preserve the full subtree including nested files.  The existing redo
    /// test above only exercises the same-device `rename` path; this test
    /// directly exercises the copy+remove fallback body on a directory with
    /// content.
    #[test]
    fn restore_dir_cross_device_fallback_preserves_contents() {
        // No trash root: this test calls the `hjkl_fs` move primitives directly
        // and never reaches `trash_path_in`.
        let td = tempfile::tempdir().unwrap();

        // Create a directory tree (simulating a trashed dir).
        let src = td.path().join("trashed_dir");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("sub/deep.txt"), b"nested").unwrap();
        std::fs::write(src.join("top.txt"), b"top-level").unwrap();

        // Restore target (simulating the original path being re-created).
        let restored = td.path().join("restored_dir");

        // Exercise the cross-device fallback body directly: `move_entry` does
        // `rename` first, and on `CrossesDevices` hands a directory to
        // `hjkl_fs::copy_dir_atomic` followed by the removal of the source.  We
        // can't force a `CrossesDevices` error inside one tempdir, so call that
        // body — which is the path that was previously untested for directory
        // restores.
        hjkl_fs::copy_dir_atomic(&src, &restored, &hjkl_fs::WriteOptions::default()).unwrap();
        hjkl_fs::remove_path_all(&src).unwrap();

        // Verify the restored directory has the full subtree.
        assert!(restored.is_dir(), "restored dir must exist");
        assert!(restored.join("sub").is_dir(), "subdir must be restored");
        assert_eq!(
            std::fs::read_to_string(restored.join("sub/deep.txt")).unwrap(),
            "nested"
        );
        assert_eq!(
            std::fs::read_to_string(restored.join("top.txt")).unwrap(),
            "top-level"
        );
        // Original is gone (removed by remove_dir_all).
        assert!(
            !src.exists(),
            "source must be removed after cross-device move"
        );
    }

    // ── cross-device copy fallback: symlink safety ────────────────────────────

    /// The copy fallback of [`move_entry`] must NOT follow symlinks: a
    /// directory symlink inside the moved tree is recreated as a symlink at
    /// the destination, and the tree it points at is left untouched (the old
    /// `is_dir()`-based walk recursed into it and later deleted through it).
    #[cfg(unix)]
    #[test]
    fn cross_device_copy_preserves_symlinks_without_following() {
        let td = tempfile::tempdir().unwrap();

        // A "real" tree that the symlink points at — must survive untouched.
        let real = td.path().join("real");
        std::fs::create_dir_all(real.join("inner")).unwrap();
        std::fs::write(real.join("inner/keep.txt"), "keep").unwrap();

        // The tree being moved: a regular file, a subdir, a dir symlink to
        // `real`, and a self-referential symlink loop.
        let src = td.path().join("tree");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("sub/file.txt"), "data").unwrap();
        std::os::unix::fs::symlink(&real, src.join("link_to_real")).unwrap();
        std::os::unix::fs::symlink(&src, src.join("loop")).unwrap();

        // Exercise the fallback body directly (rename can't be forced to
        // fail with CrossesDevices inside one tempdir), then the removal
        // step exactly as the seam performs it.
        let dst = td.path().join("moved");
        hjkl_fs::copy_dir_atomic(&src, &dst, &hjkl_fs::WriteOptions::default()).unwrap();
        hjkl_fs::remove_path_all(&src).unwrap();

        // Regular content copied.
        assert_eq!(
            std::fs::read_to_string(dst.join("sub/file.txt")).unwrap(),
            "data"
        );
        // The dir symlink was recreated as a symlink — not expanded.
        let meta = std::fs::symlink_metadata(dst.join("link_to_real")).unwrap();
        assert!(meta.file_type().is_symlink(), "symlink was materialized");
        assert_eq!(std::fs::read_link(dst.join("link_to_real")).unwrap(), real);
        // The loop symlink did not cause unbounded recursion.
        assert!(
            std::fs::symlink_metadata(dst.join("loop"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        // The real tree the link pointed at is fully intact after the
        // source removal.
        assert_eq!(
            std::fs::read_to_string(real.join("inner/keep.txt")).unwrap(),
            "keep"
        );
    }

    // ── cross-device copy fallback: fifo must error, never hang ───────────────

    /// A fifo in a tree the explorer moves across a filesystem boundary must
    /// **error**. The hand-rolled walk this replaced had three branches —
    /// symlink, directory, everything-else — so a fifo went to `std::fs::copy`,
    /// which opens it and blocks forever waiting for a writer. That hung the
    /// editor inside a `dd` or a rename, with no way to cancel.
    ///
    /// A genuine `EXDEV` needs two real filesystems and a unit test cannot mount
    /// one, so this follows what `hjkl-fs` does for its own cross-device tests:
    /// take the real thing via `/dev/shm` when the machine offers it (a separate
    /// tmpfs on essentially every Linux box), and otherwise call the staged copy
    /// that `move_atomic`'s `CrossesDevices` arm delegates to. The fifo is
    /// classified by the same code on both routes.
    ///
    /// The move runs on a worker thread behind a `recv_timeout`, so a regression
    /// **fails** this test rather than wedging the whole suite.
    #[cfg(unix)]
    #[test]
    fn cross_device_move_of_a_tree_with_a_fifo_errors_instead_of_hanging() {
        let td = tempfile::tempdir().unwrap();
        let src = td.path().join("tree");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("sub/file.txt"), "data").unwrap();
        let fifo = std::ffi::CString::new(src.join("pipe").to_str().unwrap()).unwrap();
        assert_eq!(
            unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) },
            0,
            "mkfifo failed"
        );

        // A second filesystem, when there is one to be had.
        #[cfg(target_os = "linux")]
        let shm: Option<tempfile::TempDir> = {
            use std::os::unix::fs::MetadataExt;
            let same_dev =
                |m: &std::fs::Metadata| m.dev() == std::fs::metadata(td.path()).unwrap().dev();
            match std::fs::metadata("/dev/shm") {
                Ok(m) if !same_dev(&m) => tempfile::tempdir_in("/dev/shm").ok(),
                _ => None,
            }
        };
        #[cfg(not(target_os = "linux"))]
        let shm: Option<tempfile::TempDir> = None;

        let real_exdev = shm.is_some();
        let dst = match &shm {
            Some(d) => d.path().join("moved"),
            None => td.path().join("moved"),
        };

        if real_exdev {
            // Proof that this run exercises the fallback and does not silently
            // take the `rename` fast path. A failed rename changes nothing.
            assert_eq!(
                std::fs::rename(&src, &dst).unwrap_err().kind(),
                std::io::ErrorKind::CrossesDevices,
                "expected a genuine cross-device boundary"
            );
        }

        let (tx, rx) = std::sync::mpsc::channel();
        let (thread_src, thread_dst) = (src.clone(), dst.clone());
        std::thread::spawn(move || {
            let opts = hjkl_fs::WriteOptions::default();
            let result = if real_exdev {
                // `rename` fails with `CrossesDevices` here, so this really does
                // take the fallback arm.
                hjkl_fs::move_atomic(&thread_src, &thread_dst, &opts)
            } else {
                hjkl_fs::copy_dir_atomic(&thread_src, &thread_dst, &opts)
            };
            let _ = tx.send(result.map_err(|e| e.kind()));
        });

        let outcome = rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("moving a tree containing a fifo hung — `fs::copy` blocks on a fifo forever");
        assert_eq!(
            outcome,
            Err(std::io::ErrorKind::Unsupported),
            "a fifo in the tree must be refused, not copied"
        );

        // A refused move loses nothing and leaves no partial destination.
        assert_eq!(
            std::fs::read_to_string(src.join("sub/file.txt")).unwrap(),
            "data",
            "source must survive a refused move"
        );
        assert!(
            std::fs::symlink_metadata(src.join("pipe")).is_ok(),
            "the fifo itself must still be there"
        );
        assert!(!dst.exists(), "no partial destination may be left behind");
    }
}
