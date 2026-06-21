//! File-explorer as a real read-only buffer window (#55) — Neovim neo-tree/oil style.
//!
//! The tree **model** (`ExplorerTree`: root, expanded set, flat node list) is
//! kept from the previous implementation. `render_text` produces the buffer
//! text and a line→node map. Everything else (custom render, key handler,
//! focus flag, scroll, selection, mouse zone) is gone — the engine provides
//! those for free because the explorer is now a real left window in the layout
//! tree.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

// ── Tree model ─────────────────────────────────────────────────────────────────

/// One visible row in the flattened tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExplorerNode {
    pub path: PathBuf,
    /// Nesting depth: root is depth 0, its children depth 1, …
    pub depth: usize,
    pub is_dir: bool,
    /// True when this node is the last child of its parent (`└╴` vs `├╴`).
    pub is_last: bool,
    /// For each ancestor level above this node (excluding the root): whether
    /// that ancestor has a following sibling (draw a `│` in that column).
    /// Length `depth - 1` for `depth >= 1`; empty for the root.
    pub branches: Vec<bool>,
    /// Git status for this path (from the workdir status map). `None` when the
    /// path is clean or the explorer root is outside a git repo.
    pub git: Option<hjkl_app::git::ExplorerGit>,
}

/// Pure tree model. Owned by `ExplorerPane` on `App`.
#[derive(Debug, Clone)]
pub(crate) struct ExplorerTree {
    /// Root of the tree (cwd when the explorer was opened).
    pub(crate) root: PathBuf,
    /// Directories whose fold is OPEN (absolute paths).
    /// The full tree is always in `nodes`; this set drives fold closed/open state.
    /// The root is always open by default.
    expanded: HashSet<PathBuf>,
    /// Flattened depth-first list of ALL nodes (entire tree, regardless of
    /// expansion). Collapsed dirs are present but their subtrees are hidden via
    /// buffer folds. Indexed 1:1 with buffer lines after `render_text`.
    pub(crate) nodes: Vec<ExplorerNode>,
    /// When `false`, entries whose name starts with `.` are hidden. Defaults to
    /// `true` (dotfiles shown). `H` toggles this. The `.git` dir is always
    /// skipped regardless.
    pub(crate) show_hidden: bool,
    /// When `true` (default), entries matched by the repo's git ignore rules
    /// (`.gitignore`, `.git/info/exclude`, core.excludesfile) are hidden — this
    /// also prunes ignored dirs (e.g. `target/`, `node_modules/`) from the walk,
    /// keeping it fast. `I` toggles this. No effect outside a repo.
    pub(crate) respect_gitignore: bool,
    /// Last-known on-disk git status map (keyed by the same absolute paths as
    /// [`hjkl_app::git::explorer_status_map`] produces). Populated on each
    /// full rebuild; used by [`Self::retag_git`] for cheap live overlay.
    pub(crate) git_base: HashMap<PathBuf, hjkl_app::git::ExplorerGit>,
    /// `true` when `root` is inside a git repository (cached from the last
    /// rebuild). Gates `retag_git` so no git syscalls occur per keystroke
    /// outside a repo.
    pub(crate) repo_present: bool,
}

impl ExplorerTree {
    /// Create a fresh tree rooted at `root`. The root starts expanded so its
    /// children are visible immediately. Dotfiles are shown by default;
    /// git-ignored entries are hidden by default.
    pub(crate) fn new(root: PathBuf) -> Self {
        let mut expanded = HashSet::new();
        expanded.insert(root.clone());
        let mut tree = Self {
            root,
            expanded,
            nodes: Vec::new(),
            show_hidden: true,
            respect_gitignore: true,
            git_base: HashMap::new(),
            repo_present: false,
        };
        tree.rebuild();
        tree
    }

    /// Read one directory's children, sorted directories-first then by
    /// case-insensitive name. The `.git` dir is always skipped; dotfiles are
    /// filtered unless `show_hidden`; git-ignored entries are filtered when
    /// `respect_gitignore` and a repo is available (`repo`).
    fn read_children(&self, dir: &Path, repo: Option<&git2::Repository>) -> Vec<(PathBuf, bool)> {
        let mut entries: Vec<(PathBuf, bool)> = match std::fs::read_dir(dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .filter_map(|e| {
                    let p = e.path();
                    let name = p.file_name().map(|n| n.to_string_lossy().into_owned());
                    // Never show the repo internals dir.
                    if name.as_deref() == Some(".git") {
                        return None;
                    }
                    // Skip dotfiles unless show_hidden.
                    if !self.show_hidden && name.as_deref().is_some_and(|n| n.starts_with('.')) {
                        return None;
                    }
                    // Skip git-ignored entries (prunes ignored dirs from the walk).
                    if self.respect_gitignore
                        && let Some(r) = repo
                        && r.is_path_ignored(&p).unwrap_or(false)
                    {
                        return None;
                    }
                    let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                    Some((p, is_dir))
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        entries.sort_by(|(a, a_dir), (b, b_dir)| {
            b_dir.cmp(a_dir).then_with(|| {
                let an = a.file_name().map(|n| n.to_string_lossy().to_lowercase());
                let bn = b.file_name().map(|n| n.to_string_lossy().to_lowercase());
                an.cmp(&bn)
            })
        });
        entries
    }

    /// Open the git repo enclosing `root` for ignore checks. `None` when not in
    /// a repo or when ignore-honoring is disabled.
    fn open_repo(&self) -> Option<git2::Repository> {
        if !self.respect_gitignore {
            return None;
        }
        git2::Repository::discover(&self.root).ok()
    }

    /// Recursively append `dir`'s expanded children to `out`.
    ///
    /// `status` carries the git workdir status map built once per rebuild.
    /// Each file/dir node gets its `git` field set from the map. Deleted files
    /// (present in `status` with `ExplorerGit::Deleted` but absent from the
    /// filesystem) are injected at the end of the directory's child list and
    /// re-sorted so they appear in tree order.
    fn push_children(
        &self,
        dir: &Path,
        depth: usize,
        prefix: &[bool],
        out: &mut Vec<ExplorerNode>,
        repo: Option<&git2::Repository>,
        status: &HashMap<PathBuf, hjkl_app::git::ExplorerGit>,
    ) {
        let mut children = self.read_children(dir, repo);

        // Inject deleted files: paths in the status map whose parent is `dir`
        // and whose status is Deleted, that are not already present on disk.
        // Collect disk paths into owned set first to avoid borrow conflicts.
        let disk_paths: HashSet<PathBuf> = children.iter().map(|(p, _)| p.clone()).collect();
        for (abs_path, &git_status) in status.iter() {
            if git_status != hjkl_app::git::ExplorerGit::Deleted {
                continue;
            }
            if abs_path.parent() != Some(dir) {
                continue;
            }
            if disk_paths.contains(abs_path) {
                continue;
            }
            // Not on disk — inject as a file entry.
            children.push((abs_path.clone(), false));
        }

        // Re-sort: dirs first, then case-insensitive name (same order as read_children).
        children.sort_by(|(a, a_dir), (b, b_dir)| {
            b_dir.cmp(a_dir).then_with(|| {
                let an = a.file_name().map(|n| n.to_string_lossy().to_lowercase());
                let bn = b.file_name().map(|n| n.to_string_lossy().to_lowercase());
                an.cmp(&bn)
            })
        });

        let n = children.len();
        for (i, (path, is_dir)) in children.into_iter().enumerate() {
            let is_last = i + 1 == n;
            let git = status.get(&path).copied();
            out.push(ExplorerNode {
                path: path.clone(),
                depth,
                is_dir,
                is_last,
                branches: prefix.to_vec(),
                git,
            });
            // Lazy walk: descend ONLY into directories the user has expanded.
            // A collapsed dir contributes a single node and its subtree is left
            // unread — so opening the explorer in a huge tree (a home dir) only
            // reads the visible levels, never the whole disk. Re-expanding a dir
            // re-reads its one level (cheap); search reaches the rest via the
            // fuzzy file finder, not by walking everything up front.
            if is_dir && self.expanded.contains(&path) {
                let mut child_prefix = prefix.to_vec();
                child_prefix.push(!is_last);
                self.push_children(&path, depth + 1, &child_prefix, out, repo, status);
            }
        }
    }

    /// Rebuild the flattened node list from the current expansion state.
    pub(crate) fn rebuild(&mut self) {
        // Build the git status map once for the whole walk. Empty when not in a
        // repo — no git2 calls are made inside push_children in that case.
        let status = hjkl_app::git::explorer_status_map(&self.root);
        self.repo_present = git2::Repository::discover(&self.root).is_ok();
        self.git_base = status.clone();

        let mut out = Vec::new();
        let root = self.root.clone();
        out.push(ExplorerNode {
            path: root.clone(),
            depth: 0,
            is_dir: true,
            is_last: true,
            branches: Vec::new(),
            git: None, // root dir rollup applied below
        });
        // Always walk the full tree — every node (including children of collapsed
        // dirs) is added to `nodes`. Fold state is managed via `compute_folds`.
        let repo = self.open_repo();
        self.push_children(&root, 1, &[], &mut out, repo.as_ref(), &status);
        roll_up_dir_status(&mut out);
        self.nodes = out;
    }

    /// The explorer no longer uses buffer folds: with the lazy walk a collapsed
    /// directory's children are simply absent from `nodes`, so the buffer text
    /// already *is* the visible tree (no hidden lines to fold away). Expand /
    /// collapse adds or removes nodes via `set_expanded` + `rebuild`, and the
    /// open/closed icon is driven directly by the `expanded` set in the render
    /// overlay. Kept returning an empty list so the existing `set_folds`
    /// call-sites (`open_explorer`, `explorer_rebuild_buffer`) stay unchanged.
    pub(crate) fn compute_folds(&self) -> Vec<hjkl_buffer::Fold> {
        Vec::new()
    }

    /// Re-tag every node's `git` field from `status` without a filesystem walk
    /// or tree-structure change, then recompute directory rollups.
    ///
    /// Used for live refresh on buffer edits: the caller merges the on-disk
    /// `git_base` with any in-memory overlay (open dirty buffers) and passes
    /// the result here. Cheap — O(nodes) with no git2 calls.
    pub(crate) fn retag_git(&mut self, status: &HashMap<PathBuf, hjkl_app::git::ExplorerGit>) {
        for node in &mut self.nodes {
            node.git = status.get(&node.path).copied();
        }
        roll_up_dir_status(&mut self.nodes);
    }

    /// Toggle the expansion of the directory at `path` in the `expanded` set.
    /// Test-only helper — runtime toggling flips the buffer fold directly via
    /// `toggle_fold_at` and syncs `expanded` with `set_expanded`.
    #[cfg(test)]
    pub(crate) fn toggle(&mut self, path: &Path) -> bool {
        if self.expanded.contains(path) {
            self.expanded.remove(path);
        } else {
            self.expanded.insert(path.to_path_buf());
        }
        true
    }

    /// `true` when `path` is an expanded directory. Drives the open/closed
    /// folder icon in the render overlay (the lazy explorer has no buffer folds,
    /// so the icon reads the `expanded` set directly).
    pub(crate) fn is_expanded(&self, path: &Path) -> bool {
        self.expanded.contains(path)
    }

    /// Set a directory's expanded (fold-open) state explicitly. Used to keep
    /// `expanded` in sync with the buffer's actual fold state after a toggle.
    pub(crate) fn set_expanded(&mut self, path: &Path, expanded: bool) {
        if expanded {
            self.expanded.insert(path.to_path_buf());
        } else {
            self.expanded.remove(path);
        }
    }

    /// Flip `show_hidden` and rebuild.
    pub(crate) fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        self.rebuild();
    }

    /// Flip `respect_gitignore` (show/hide git-ignored entries) and rebuild.
    pub(crate) fn toggle_gitignore(&mut self) {
        self.respect_gitignore = !self.respect_gitignore;
        self.rebuild();
    }

    /// Re-root the tree at `new_root`, preserving the existing `expanded` set.
    /// The new root is automatically added to `expanded`.
    pub(crate) fn set_root(&mut self, new_root: PathBuf) {
        self.root = new_root.clone();
        self.expanded.insert(new_root);
        self.rebuild();
    }

    /// Open all ancestor dirs of `path` (add them to `expanded`) and return the
    /// row of `path` in `self.nodes`, or `None` if `path` is not under the root.
    ///
    /// With the full-tree model the node is always present in `nodes` (the full
    /// walk includes it regardless of expansion). This method only needs to open
    /// the ancestor folds via `expanded` — no filesystem re-walk needed. However
    /// when called from `open_explorer` (tree was just built) or externally a
    /// `rebuild` is sometimes needed; callers decide. Here we only update
    /// `expanded` and find the row in the already-built node list.
    ///
    /// Robust to path-form differences (relative vs absolute, symlinked cwd).
    pub(crate) fn reveal(&mut self, path: &Path) -> Option<usize> {
        // Determine `path` relative to root, tolerating canonicalization diffs.
        let rel = path
            .strip_prefix(&self.root)
            .ok()
            .map(|p| p.to_path_buf())
            .or_else(|| {
                let rc = std::fs::canonicalize(&self.root).ok()?;
                let pc = std::fs::canonicalize(path).ok()?;
                pc.strip_prefix(&rc).ok().map(|p| p.to_path_buf())
            })?;

        // Reconstruct the node path from the root + relative components.
        let mut target = self.root.clone();
        for comp in rel.components() {
            target = target.join(comp);
        }

        // Open every ancestor directory (add to `expanded` so folds open).
        self.expanded.insert(self.root.clone());
        let mut anc = target.parent();
        while let Some(p) = anc {
            self.expanded.insert(p.to_path_buf());
            if p == self.root {
                break;
            }
            anc = p.parent();
        }

        // With the full-tree model the node is always present. If it's not found
        // (e.g. path doesn't exist yet), fall back to a rebuild so new nodes are
        // picked up from disk.
        if let Some(row) = self.nodes.iter().position(|n| n.path == target) {
            return Some(row);
        }
        // Node not found — rebuild from disk (handles the initial open case where
        // the tree may not have been walked yet for a freshly-created path).
        self.rebuild();
        self.nodes.iter().position(|n| n.path == target)
    }

    /// Build the buffer text and line→node map for the current tree state.
    ///
    /// Each line in the returned `String` corresponds to `nodes[i]`, so
    /// `cursor_row` in the editor maps directly to `nodes[cursor_row]`.
    ///
    /// The buffer contains indentation spaces + the bare name + a hidden id
    /// tail (`<US><idx>`) for all non-root lines.  The root line (index 0) has
    /// no id tail.  All glyphs are painted as a render overlay in `render.rs`.
    ///
    /// Column layout (identical to what was emitted before; the overlay paints
    /// the leading cells):
    ///   depth 0  : `"  " + name`               (no id tail)
    ///   depth ≥ 1: `" ".repeat(depth*2+2) + name[/] + US + idx`
    pub(crate) fn render_text(&self) -> String {
        use super::explorer_reconcile::ID_SEP;
        let mut out = String::new();
        for (i, node) in self.nodes.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            let name_col = node.depth * 2 + 2;
            let name = if node.depth == 0 {
                node.path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| node.path.to_string_lossy().into_owned())
            } else {
                let base = node
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if node.is_dir {
                    format!("{base}/")
                } else {
                    base
                }
            };
            out.push_str(&" ".repeat(name_col));
            out.push_str(&name);
            // Non-root lines carry the stable id so reconcile can key on it.
            if node.depth > 0 {
                out.push(ID_SEP);
                out.push_str(&i.to_string());
            }
        }
        out
    }
}

// ── App-level explorer state ───────────────────────────────────────────────────

/// Tracks the open explorer window + slot so `toggle_explorer` can close it.
#[derive(Debug, Clone)]
pub(crate) struct ExplorerPane {
    /// WindowId of the explorer window in the active tab's layout.
    pub win_id: super::window::WindowId,
    /// The tree model.
    pub tree: ExplorerTree,
    /// The `dirty_gen` of the explorer buffer at the last successful reconcile.
    /// Prevents re-running the diff on an unchanged buffer.
    pub last_reconcile_gen: u64,
    /// Registry of files moved to trash during this pane's lifetime.
    /// Each entry is `(original_file_name, trash_destination_path)`.
    /// Used to restore ("un-trash") when a `CreateFile` op targets a name
    /// matching a trashed entry — making `dd` + `p` a lossless move.
    pub trashed: Vec<(String, std::path::PathBuf)>,
    /// The last-reconciled buffer state: `(id, absolute_path, is_dir)` per
    /// line, index 0 = root (id=0). This is the baseline for `reconcile()`.
    /// Kept in sync with every reconcile cycle and every
    /// `explorer_rebuild_buffer` call so the diff always starts from the
    /// correct "last-known-good" state. The `id` is the node's index in
    /// `tree.nodes` at the time the buffer was last rendered.
    pub baseline: Vec<(u64, PathBuf, bool)>,
    /// Undo stack: each entry is one reconcile transaction
    /// (`Vec<AppliedOp>`) that can be reverted by [`App::explorer_undo`].
    pub undo_stack: Vec<Vec<super::explorer_reconcile::AppliedOp>>,
    /// Redo stack: each entry is the redo journal produced by
    /// [`super::explorer_reconcile::revert_ops`], ready to be re-applied by
    /// [`App::explorer_redo`].
    pub redo_stack: Vec<Vec<super::explorer_reconcile::AppliedOp>>,
}

// ── nodes_from_buffer ──────────────────────────────────────────────────────────

/// Derive the render-tree (`Vec<ExplorerNode>`) from the current buffer text.
/// Used only in tests (round-trip assertions); production code now uses
/// `ExplorerTree::rebuild` for all render-tree updates.
///
/// Parses the buffer using the same indent→depth rules as
/// `explorer_reconcile::parse_buffer` (which `reconcile()` calls internally),
/// then fills in `is_last` and `branches` by scanning forward in the depth
/// sequence, mirroring the logic used by `ExplorerTree::push_children` so the
/// guide overlay produced by `render.rs` stays aligned.
///
/// The root line (line 0) becomes a depth-0 node for `path = root`.
/// Subsequent lines: `depth = ((indent - 2) / 2).max(1)`.
///
/// `is_last[i]` — scanning forward from `i`, if the first node whose
/// `depth < nodes[i].depth` is reached before finding a node with
/// `depth == nodes[i].depth`, then `is_last = true`.
///
/// `branches[i]` (length `depth - 1` for `depth >= 1`; empty for depth 0):
/// for each ancestor level `a` in `1..depth`, the entry is `!is_last` of the
/// most-recent earlier node at depth `a`. I.e. draw a vertical bar at an
/// ancestor column iff that ancestor still has a following sibling.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn nodes_from_buffer(text: &str, root: &Path) -> Vec<ExplorerNode> {
    overlay_nodes_from_buffer(text, root)
        .into_iter()
        .flatten()
        .collect()
}

/// Like [`nodes_from_buffer`], but returns ONE entry per buffer line — `None`
/// for blank / nameless lines — so the result indexes **1:1 with buffer rows**.
///
/// This is what the `render.rs` glyph/color overlay walks: deriving the tree
/// layout from the LIVE buffer text (rather than the last-reconciled
/// `ExplorerTree::nodes`) keeps icons, guides, and git colors aligned **while
/// the buffer is being edited** — before the Normal-mode reconcile rebuilds the
/// tree. A mid-edit `o`/`O` inserts a blank indented line; mapping the overlay
/// off the stale node list shifted every glyph below the edit down a row. Here
/// that blank line is a `None` slot, so everything stays put.
///
/// `git` is left `None` on every node — the overlay resolves git status by
/// path against the reconciled tree's status map (so a row's color follows its
/// path, not its row index). See `render.rs`.
pub(crate) fn overlay_nodes_from_buffer(text: &str, root: &Path) -> Vec<Option<ExplorerNode>> {
    // ── Pass 1: parse path / depth / is_dir, one slot per buffer line ────────
    struct RawNode {
        path: PathBuf,
        depth: usize,
        is_dir: bool,
    }

    // `split('\n')` (not `lines()`) so a trailing newline yields a final empty
    // slot, matching the buffer's row count exactly.
    let mut raw: Vec<Option<RawNode>> = Vec::new();
    let mut dir_stack: Vec<(usize, PathBuf)> = Vec::new(); // (depth, abs_path)

    for (line_idx, line) in text.split('\n').enumerate() {
        use super::explorer_reconcile::ID_SEP;
        if line_idx == 0 {
            // Root line — always depth 0, no id tail.
            raw.push(Some(RawNode {
                path: root.to_path_buf(),
                depth: 0,
                is_dir: true,
            }));
            dir_stack.push((0, root.to_path_buf()));
            continue;
        }
        // Strip the id tail (everything from the first ID_SEP onward) before
        // parsing indent/name. This leaves the name side verbatim.
        let left = if let Some(sep_pos) = line.find(ID_SEP) {
            &line[..sep_pos]
        } else {
            line
        };
        if left.trim().is_empty() {
            raw.push(None);
            continue;
        }
        let indent = left.len() - left.trim_start_matches(' ').len();
        let depth = ((indent.saturating_sub(2)) / 2).max(1);
        // Name is verbatim between indent and the stripped id tail.
        // Do NOT trim_end — whitespace in names must be preserved.
        let name_part = &left[indent..];
        let is_dir = name_part.ends_with('/');
        let name = if is_dir {
            &name_part[..name_part.len() - 1]
        } else {
            name_part
        };
        if name.is_empty() {
            raw.push(None);
            continue;
        }
        // Pop dir_stack entries at depth >= current.
        while dir_stack.last().map(|(d, _)| *d >= depth).unwrap_or(false) {
            dir_stack.pop();
        }
        let parent = dir_stack
            .last()
            .filter(|(d, _)| *d == depth - 1)
            .map(|(_, p)| p.as_path())
            .unwrap_or(root);
        let path = parent.join(name);
        if is_dir {
            dir_stack.push((depth, path.clone()));
        }
        raw.push(Some(RawNode {
            path,
            depth,
            is_dir,
        }));
    }

    let n = raw.len();
    if n == 0 {
        return Vec::new();
    }

    // ── Pass 2: compute is_last (per real node; blanks are transparent) ──────
    let mut is_last_flags: Vec<bool> = vec![true; n];
    for i in 0..n {
        let Some(ref cur) = raw[i] else { continue };
        let d = cur.depth;
        for slot in raw.iter().skip(i + 1) {
            let Some(node) = slot else { continue };
            if node.depth < d {
                break; // left the parent's scope
            }
            if node.depth == d {
                is_last_flags[i] = false;
                break;
            }
        }
    }

    // ── Pass 3: compute branches, scattering into a per-line Option vec ──────
    // For each node at depth d (d >= 1), branches[a-1] for a in 1..d is
    // `!is_last` of the most-recent ancestor at depth a.
    let mut ancestor_is_last: Vec<Option<bool>> = vec![None; n + 2];

    let mut out: Vec<Option<ExplorerNode>> = Vec::with_capacity(n);
    for i in 0..n {
        let Some(ref cur) = raw[i] else {
            out.push(None);
            continue;
        };
        let d = cur.depth;
        // Update the ancestor table for this depth.
        if d < ancestor_is_last.len() {
            ancestor_is_last[d] = Some(is_last_flags[i]);
        }

        let branches: Vec<bool> = if d == 0 {
            Vec::new()
        } else {
            (1..d)
                .map(|a| {
                    // draw a bar if the ancestor at depth `a` is NOT last
                    ancestor_is_last
                        .get(a)
                        .and_then(|v| *v)
                        .map(|last| !last)
                        .unwrap_or(false)
                })
                .collect()
        };

        out.push(Some(ExplorerNode {
            path: cur.path.clone(),
            depth: d,
            is_dir: cur.is_dir,
            is_last: is_last_flags[i],
            branches,
            git: None, // resolved by path in the render overlay
        }));
    }

    out
}

// ── Git status rollup ─────────────────────────────────────────────────────────

/// Precedence order for directory rollup: higher index = higher priority.
pub(crate) fn git_status_priority(s: hjkl_app::git::ExplorerGit) -> u8 {
    use hjkl_app::git::ExplorerGit::*;
    match s {
        Deleted => 1,
        Untracked => 2,
        Staged => 3,
        Modified => 4,
    }
}

/// Roll up git status from descendant files onto ancestor directory nodes.
///
/// After `push_children` has populated the flat DFS-ordered list, this pass
/// sets each dir node's `git` to the highest-priority status among its
/// descendants. Relies on DFS order: every node at index `i+1..` whose depth
/// is greater than `nodes[i].depth` belongs to the subtree of `nodes[i]`.
fn roll_up_dir_status(nodes: &mut [ExplorerNode]) {
    for i in 0..nodes.len() {
        if !nodes[i].is_dir {
            continue;
        }
        let dir_depth = nodes[i].depth;
        let mut best: Option<hjkl_app::git::ExplorerGit> = nodes[i].git;
        // Split the slice so we can read `tail[j]` while holding a mutable ref
        // to `head[i]` later. `tail` starts at i+1.
        let tail = &nodes[i + 1..];
        for desc in tail {
            if desc.depth <= dir_depth {
                break; // left the subtree
            }
            if let Some(s) = desc.git {
                best = Some(match best {
                    None => s,
                    Some(b) => {
                        if git_status_priority(s) > git_status_priority(b) {
                            s
                        } else {
                            b
                        }
                    }
                });
            }
        }
        nodes[i].git = best;
    }
}

// ── App methods ────────────────────────────────────────────────────────────────

use hjkl_engine::Host;

impl super::App {
    /// `true` when the focused window's slot is the explorer buffer.
    pub(crate) fn explorer_buf_focused(&self) -> bool {
        let fw = self.focused_window();
        self.windows
            .get(fw)
            .and_then(|w| w.as_ref())
            .map(|w| self.slots.get(w.slot).is_some_and(|s| s.is_explorer))
            .unwrap_or(false)
    }

    /// `<leader>e` toggle: closed → open + focus; open → close.
    pub(crate) fn toggle_explorer(&mut self) {
        if self.explorer.is_some() {
            self.close_explorer();
        } else {
            self.open_explorer();
        }
    }

    /// Open the explorer window (left vertical split of the current tab).
    fn open_explorer(&mut self) {
        use super::STATUS_LINE_HEIGHT;
        use super::window::{LayoutTree, SplitDir, Window};
        use crate::host::TuiHost;
        use hjkl_buffer::Buffer;
        use hjkl_engine::{BufferEdit, Editor, Host, Options};
        use std::time::Instant;

        // Capture the file the user was editing so we can reveal it.
        let active_file = self.active().filename.clone();

        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let mut tree = ExplorerTree::new(cwd.clone());

        // Reveal the active file's path before rendering, so the initial cursor
        // lands on it and its ancestor dirs are expanded. `reveal` is robust to
        // relative-vs-absolute path forms (it canonicalizes), and returns `None`
        // when the file isn't under the root — so no `starts_with` pre-gate (that
        // wrongly rejected files opened with a relative path, since cwd is
        // absolute).
        let reveal_row: Option<usize> = active_file.as_deref().and_then(|p| tree.reveal(p));

        let text = tree.render_text();
        // Compute initial folds from the `expanded` set (only root is open).
        let initial_folds = tree.compute_folds();
        // Nodes are rebuilt by new() above; no extra rebuild needed.

        let buffer_id = self.next_buffer_id;
        self.next_buffer_id += 1;

        let host = TuiHost::new();
        let mut editor = Editor::new(Buffer::new(), host, Options::default());
        if let Ok(size) = crossterm::terminal::size() {
            let h = size.1.saturating_sub(STATUS_LINE_HEIGHT);
            let vp = editor.host_mut().viewport_mut();
            vp.width = super::explorer::EXPLORER_WINDOW_WIDTH;
            vp.height = h;
        }
        editor.set_current_buffer_id(buffer_id);
        if !text.is_empty() {
            BufferEdit::replace_all(editor.buffer_mut(), &text);
        }
        // Apply initial folds so collapsed dirs hide their subtrees on open.
        editor.buffer_mut().set_folds(&initial_folds);
        editor.set_filetype("explorer");
        // Settings for the explorer: no line numbers, no sign column, cursorline on.
        {
            let s = editor.settings_mut();
            s.number = false;
            s.relativenumber = false;
            s.signcolumn = hjkl_engine::types::SignColumnMode::No;
            s.cursorline = true;
            s.foldcolumn = 0;
        }
        let _ = editor.take_content_edits();
        let _ = editor.take_content_reset();

        let slot = super::BufferSlot {
            buffer_id,
            is_explorer: true,
            features: super::BufferFeatures {
                syntax: false,
                lsp: false,
                hover: false,
                end_of_buffer: false,
            },
            editor,
            filename: None,
            dirty: false,
            is_new_file: false,
            is_untracked: false,
            diag_signs: Vec::new(),
            diag_signs_lsp: Vec::new(),
            lsp_diags: Vec::new(),
            last_lsp_dirty_gen: None,
            git_signs: Vec::new(),
            last_git_dirty_gen: None,
            last_git_refresh_at: Instant::now(),
            blame: Vec::new(),
            last_blame_dirty_gen: None,
            last_blame_refresh_at: Instant::now(),
            saved_hash: 0,
            saved_len: 0,
            signature_cache: None,
            disk_mtime: None,
            disk_len: None,
            disk_state: super::DiskState::Synced,
            swap_path: None,
            last_swap_dirty_gen: None,
            last_fold_dirty_gen: None,
            git_repo_present: None,
            commit_ctx: None,
        };
        self.slots.push(slot);
        let slot_idx = self.slots.len() - 1;

        let new_win_id = self.next_window_id;
        self.next_window_id += 1;
        self.windows.push(Some(Window::new(slot_idx)));

        // Splice a left vertical split: new explorer window on the left,
        // existing layout on the right.
        let total_w = crossterm::terminal::size()
            .map(|(w, _)| w as usize)
            .unwrap_or(80);
        let ratio_a = (EXPLORER_WINDOW_WIDTH as f32 / total_w as f32).clamp(0.05, 0.45);
        let ratio_b = 1.0 - ratio_a;
        let _ = ratio_b; // ratio_a is the left side

        // Save the outgoing window's cursor/scroll before changing the layout.
        self.sync_viewport_from_editor();

        let old_layout = self.take_layout();
        let new_layout = LayoutTree::Split {
            dir: SplitDir::Vertical,
            ratio: ratio_a,
            a: Box::new(LayoutTree::Leaf(new_win_id)),
            b: Box::new(old_layout),
            last_rect: None,
        };
        self.restore_layout(new_layout);

        // Focus the new explorer window.
        self.set_focused_window(new_win_id);
        self.sync_viewport_to_editor();

        // Build the initial baseline from the freshly-built tree nodes.
        // Each entry gets its sequence id = its index in tree.nodes.
        let initial_baseline: Vec<(u64, PathBuf, bool)> = tree
            .nodes
            .iter()
            .enumerate()
            .map(|(idx, n)| (idx as u64, n.path.clone(), n.is_dir))
            .collect();

        // Record the dirty_gen AFTER set_content so the first reconcile check
        // starts from the correct generation and doesn't fire immediately.
        let initial_gen = self
            .slots
            .iter()
            .rev()
            .find(|s| s.is_explorer)
            .map(|s| s.editor.buffer().dirty_gen())
            .unwrap_or(0);

        self.explorer = Some(ExplorerPane {
            win_id: new_win_id,
            tree,
            last_reconcile_gen: initial_gen,
            trashed: Vec::new(),
            baseline: initial_baseline,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        });

        // Overlay any already-dirty open buffers onto the freshly-built tree
        // so an edited buffer is immediately colored without waiting for the
        // next git-sign poll cycle.
        self.refresh_explorer_git();

        // Apply the reveal cursor position if we found the active file.
        if let Some(row) = reveal_row {
            if let Some(Some(win)) = self.windows.get_mut(new_win_id) {
                win.cursor_row = row;
                win.cursor_col = 0;
            }
            self.sync_viewport_to_explorer_editor();
        }
    }

    /// Current slot index of the explorer's scratch buffer, found by its
    /// `is_explorer` flag (robust to slot re-indexing from `:bd`/`:bn`).
    fn explorer_slot_idx(&self) -> Option<usize> {
        self.slots.iter().position(|s| s.is_explorer)
    }

    /// Close the explorer window and remove its slot.
    fn close_explorer(&mut self) {
        let Some(ep) = self.explorer.take() else {
            return;
        };
        let new_focus = match self.layout_mut().remove_leaf(ep.win_id) {
            Ok(f) => f,
            Err(_) => return,
        };
        self.windows[ep.win_id] = None;
        if let Some(slot_idx) = self.explorer_slot_idx() {
            self.slots.remove(slot_idx);
            let slot_count = self.slots.len();
            for win in self.windows.iter_mut().flatten() {
                if win.slot == slot_idx {
                    win.slot = 0;
                } else if win.slot > slot_idx {
                    win.slot -= 1;
                }
                win.slot = win.slot.min(slot_count.saturating_sub(1));
            }
        }
        self.set_focused_window(new_focus);
        self.sync_viewport_to_editor();
    }

    /// Rebuild the explorer buffer text (after expand/collapse). Keeps the
    /// cursor row on the same path when possible.
    ///
    /// This is a full structural reset: it calls `set_content` (which clears
    /// undo history) and then syncs `baseline` and `last_reconcile_gen` so
    /// the next reconcile starts from the new content — not from the
    /// pre-toggle state. Undo across a fold-toggle is not expected.
    pub(crate) fn explorer_rebuild_buffer(&mut self) {
        let Some(slot_idx) = self.explorer_slot_idx() else {
            return;
        };
        let (text, win_id) = match self.explorer.as_ref() {
            Some(ep) => (ep.tree.render_text(), ep.win_id),
            None => return,
        };
        // Stash the path currently under cursor before we rebuild.
        let prev_row = self
            .windows
            .get(win_id)
            .and_then(|w| w.as_ref())
            .map(|w| w.cursor_row)
            .unwrap_or(0);
        let prev_path = self
            .explorer
            .as_ref()
            .and_then(|ep| ep.tree.nodes.get(prev_row))
            .map(|n| n.path.clone());

        // Write new content directly (bypasses readonly guard intentionally).
        // set_content clears undo/redo — correct for structural nav (toggle/root).
        self.slots[slot_idx].editor.set_content(&text);
        let _ = self.slots[slot_idx].editor.take_content_edits();
        let _ = self.slots[slot_idx].editor.take_content_reset();

        // Apply folds derived from the current `expanded` set so collapsed dirs
        // hide their subtrees. Must happen after set_content so row indices are
        // correct for the new text. `set_folds` is idempotent when unchanged.
        let folds = self
            .explorer
            .as_ref()
            .map(|ep| ep.tree.compute_folds())
            .unwrap_or_default();
        self.slots[slot_idx].editor.buffer_mut().set_folds(&folds);
        self.sync_explorer_window_folds();

        // Sync the baseline from the freshly-rendered tree so the next
        // reconcile diffs against the post-toggle state. Also update
        // last_reconcile_gen to the new dirty_gen so the structural reset
        // doesn't trigger a spurious reconcile.
        let new_baseline: Vec<(u64, PathBuf, bool)> = self
            .explorer
            .as_ref()
            .map(|ep| {
                ep.tree
                    .nodes
                    .iter()
                    .enumerate()
                    .map(|(idx, n)| (idx as u64, n.path.clone(), n.is_dir))
                    .collect()
            })
            .unwrap_or_default();
        let new_gen = self.slots[slot_idx].editor.buffer().dirty_gen();
        if let Some(ep) = self.explorer.as_mut() {
            ep.baseline = new_baseline;
            ep.last_reconcile_gen = new_gen;
        }

        // Try to keep cursor on the same path.
        let new_row = if let Some(ref p) = prev_path {
            self.explorer
                .as_ref()
                .and_then(|ep| ep.tree.nodes.iter().position(|n| &n.path == p))
                .unwrap_or(prev_row)
        } else {
            prev_row
        };
        if let Some(Some(win)) = self.windows.get_mut(win_id) {
            win.cursor_row = new_row.min(
                self.explorer
                    .as_ref()
                    .map(|ep| ep.tree.nodes.len().saturating_sub(1))
                    .unwrap_or(0),
            );
            win.cursor_col = 0;
        }
        // Sync the editor cursor to match the window snapshot.
        let fw = self.focused_window();
        if fw == win_id {
            self.sync_viewport_to_explorer_editor();
        }
    }

    /// When the explorer is open and in Normal mode and the buffer has changed
    /// since the last reconcile, diff the buffer against the pane's explicit
    /// `baseline` and apply the resulting filesystem ops.
    ///
    /// **Git-aware rebuild model**: after applying ops the tree is rebuilt from
    /// disk + git so that tracked-but-deleted files reappear in red
    /// (git WT_DELETED injection). The buffer is fully reset via
    /// `explorer_rebuild_buffer` → `set_content`, which clears vim's undo
    /// stack. Explicit undo/redo is provided by the pane's `undo_stack` /
    /// `redo_stack` journal (see [`App::explorer_undo`] /
    /// [`App::explorer_redo`]).
    pub(crate) fn maybe_reconcile_explorer(&mut self) {
        use hjkl_engine::VimMode;

        // Nothing to do if the explorer is not open.
        if self.explorer.is_none() {
            return;
        }

        // Find the explorer slot index.
        let Some(slot_idx) = self.explorer_slot_idx() else {
            return;
        };

        // Must be in Normal mode — don't reconcile mid-edit.
        let vim_mode = self.slots[slot_idx].editor.vim_mode();
        if vim_mode != VimMode::Normal {
            return;
        }

        // Read the buffer's current dirty_gen.
        let cur_gen = self.slots[slot_idx].editor.buffer().dirty_gen();

        // Guard: nothing changed since last reconcile.
        let last_gen = self
            .explorer
            .as_ref()
            .map(|ep| ep.last_reconcile_gen)
            .unwrap_or(0);
        if cur_gen == last_gen {
            return;
        }

        // Clone what we need out of `self.explorer` before borrowing self mutably.
        let (baseline, text, root) = {
            let ep = self.explorer.as_ref().unwrap();
            let baseline = ep.baseline.clone();
            let text = self.slots[slot_idx].editor.buffer().as_string();
            let root = ep.tree.root.clone();
            (baseline, text, root)
        };

        let ops = super::explorer_reconcile::reconcile(&baseline, &text, &root);

        if ops.is_empty() {
            // No fs changes. But the buffer may still hold cosmetic cruft that
            // parsed to nothing — most commonly a stray blank line left by
            // `o<Esc>` / `O<Esc>` (open-line with no name typed). If the text no
            // longer matches the tree's canonical render, rebuild to normalize
            // it away; otherwise just advance the gen (a pure cursor move on
            // unchanged content).
            let canonical = self
                .explorer
                .as_ref()
                .map(|ep| ep.tree.render_text())
                .unwrap_or_default();
            if text != canonical {
                self.explorer_rebuild_buffer(); // also syncs baseline + gen
            } else if let Some(ep) = self.explorer.as_mut() {
                ep.last_reconcile_gen = cur_gen;
            }
            return;
        }

        // Take the trashed registry out of the pane so we can mutate it.
        let mut trashed = self
            .explorer
            .as_mut()
            .map(|ep| std::mem::take(&mut ep.trashed))
            .unwrap_or_default();

        let (newly_created, applied, errors) =
            super::explorer_reconcile::apply_ops(&ops, &mut trashed);

        // Paths produced by this transaction (created / pasted-moved / renamed
        // destinations) — used below to land the cursor on the result (the
        // topmost when several), e.g. moving onto a pasted file.
        let result_paths: Vec<PathBuf> = {
            use super::explorer_reconcile::AppliedOp;
            applied
                .iter()
                .filter_map(|op| match op {
                    AppliedOp::Created(p) => Some(p.clone()),
                    AppliedOp::Restored { to, .. } => Some(to.clone()),
                    AppliedOp::Renamed { to, .. } => Some(to.clone()),
                    AppliedOp::Trashed { .. } => None,
                })
                .collect()
        };

        // Put the trashed registry back.
        if let Some(ep) = self.explorer.as_mut() {
            ep.trashed = trashed;
        }

        // Toast any errors.
        for err in &errors {
            self.bus.error(format!("explorer: {err}"));
        }

        // Push to undo stack if ops actually happened; clear redo stack.
        if !applied.is_empty()
            && let Some(ep) = self.explorer.as_mut()
        {
            ep.undo_stack.push(applied);
            ep.redo_stack.clear();
        }

        // Reveal newly-created nested items: expand the ancestor directories of
        // every result path so a multi-level create (`somedir/test.txt`) shows
        // the new leaf rather than leaving `somedir` collapsed. Walk each result
        // path's parents up to (and including) the root.
        if let Some(ep) = self.explorer.as_mut() {
            let root = ep.tree.root.clone();
            for p in &result_paths {
                if !p.starts_with(&root) {
                    continue;
                }
                let mut dir = p.parent();
                while let Some(d) = dir {
                    ep.tree.set_expanded(d, true);
                    if d == root {
                        break;
                    }
                    dir = d.parent();
                }
            }
        }

        // Rebuild the tree from disk + git so that tracked-but-deleted files
        // appear as red nodes (WT_DELETED injection in push_children).
        if let Some(ep) = self.explorer.as_mut() {
            ep.tree.rebuild();
        }
        // Reset the buffer content from the freshly-rebuilt tree and sync
        // baseline + last_reconcile_gen so the next tick is a no-op.
        self.explorer_rebuild_buffer();
        // Refresh git colors after rebuild.
        self.recompute_explorer_git_base();

        // Land the cursor on the transaction result (topmost created/pasted/
        // renamed path), overriding the sticky-cursor restore — so after a
        // paste you're on the pasted file.
        if !result_paths.is_empty() {
            let target = self.explorer.as_ref().and_then(|ep| {
                ep.tree
                    .nodes
                    .iter()
                    .position(|n| result_paths.contains(&n.path))
                    .map(|row| (ep.win_id, row))
            });
            if let Some((win_id, row)) = target {
                if let Some(Some(win)) = self.windows.get_mut(win_id) {
                    win.cursor_row = row;
                    win.cursor_col = 0;
                }
                if self.focused_window() == win_id {
                    self.sync_viewport_to_explorer_editor();
                }
            }
        }

        // Spawn a background buffer for each newly-created file WITHOUT focusing
        // it — `open_new_slot` just builds the slot (no window/focus change), so
        // a long restructure session stays in the explorer. Skip files already
        // open in a slot.
        for path in newly_created {
            let already = self
                .slots
                .iter()
                .any(|s| s.filename.as_deref() == Some(path.as_path()));
            if !already {
                let _ = self.open_new_slot(path);
            }
        }
    }

    /// Undo the last explorer filesystem transaction.
    ///
    /// Pops one entry from `undo_stack`, reverses it on disk via
    /// [`super::explorer_reconcile::revert_ops`], pushes the redo journal onto
    /// `redo_stack`, and rebuilds the buffer so the tree reflects the reverted
    /// state.  Returns `true` when something was undone.
    ///
    /// After this call `last_reconcile_gen` matches the new buffer gen so the
    /// subsequent `maybe_reconcile_explorer` tick is a no-op.
    pub(crate) fn explorer_undo(&mut self) -> bool {
        // Pop the top transaction from the undo stack.
        let txn = {
            let ep = match self.explorer.as_mut() {
                Some(ep) => ep,
                None => return false,
            };
            match ep.undo_stack.pop() {
                Some(t) => t,
                None => return false,
            }
        };

        // Take the trashed registry.
        let mut trashed = self
            .explorer
            .as_mut()
            .map(|ep| std::mem::take(&mut ep.trashed))
            .unwrap_or_default();

        let (redo_journal, errors) = super::explorer_reconcile::revert_ops(&txn, &mut trashed);

        // Put the trashed registry back and push to redo stack.
        if let Some(ep) = self.explorer.as_mut() {
            ep.trashed = trashed;
            ep.redo_stack.push(redo_journal);
        }

        for err in &errors {
            self.bus.error(format!("explorer undo: {err}"));
        }

        // Rebuild tree + buffer from disk + git.
        if let Some(ep) = self.explorer.as_mut() {
            ep.tree.rebuild();
        }
        self.explorer_rebuild_buffer();
        self.recompute_explorer_git_base();

        true
    }

    /// Redo the last undone explorer filesystem transaction.
    ///
    /// Pops one entry from `redo_stack`, re-applies it via
    /// [`super::explorer_reconcile::apply_applied`], pushes the new applied
    /// journal back onto `undo_stack`, and rebuilds the buffer.
    pub(crate) fn explorer_redo(&mut self) {
        // Pop the top transaction from the redo stack.
        let redo_txn = {
            let ep = match self.explorer.as_mut() {
                Some(ep) => ep,
                None => return,
            };
            match ep.redo_stack.pop() {
                Some(t) => t,
                None => return,
            }
        };

        // Take the trashed registry.
        let mut trashed = self
            .explorer
            .as_mut()
            .map(|ep| std::mem::take(&mut ep.trashed))
            .unwrap_or_default();

        let (_newly_created, new_applied, errors) =
            super::explorer_reconcile::apply_applied(&redo_txn, &mut trashed);

        // Put the trashed registry back and push to undo stack.
        if let Some(ep) = self.explorer.as_mut() {
            ep.trashed = trashed;
            if !new_applied.is_empty() {
                ep.undo_stack.push(new_applied);
            }
        }

        for err in &errors {
            self.bus.error(format!("explorer redo: {err}"));
        }

        // Rebuild tree + buffer from disk + git.
        if let Some(ep) = self.explorer.as_mut() {
            ep.tree.rebuild();
        }
        self.explorer_rebuild_buffer();
        self.recompute_explorer_git_base();
    }

    /// Copy the explorer buffer's CURRENT fold state into its per-window
    /// `window_folds` snapshot.
    ///
    /// Window-level folds (the multi-split feature) render an UNFOCUSED window
    /// from `app.window_folds[win]`, NOT from the live buffer — but the explorer
    /// mutates its buffer folds directly (`toggle_fold_at`, `reveal_row`,
    /// `set_folds`) while it is usually unfocused. Without this the snapshot
    /// goes stale: `BufferView` keeps drawing the old fold layout while the
    /// glyph/color overlay (which reads the live buffer) paints the new one, so
    /// the two desync and the tree renders garbled. Call after every explorer
    /// fold mutation.
    pub(crate) fn sync_explorer_window_folds(&mut self) {
        let Some(win_id) = self.explorer.as_ref().map(|ep| ep.win_id) else {
            return;
        };
        let Some(slot_idx) = self.explorer_slot_idx() else {
            return;
        };
        let folds = self.slots[slot_idx].editor.buffer().folds();
        self.window_folds.insert(win_id, folds);
    }

    /// Sync the explorer editor's cursor from the explorer window's snapshot.
    /// Like `sync_viewport_to_editor` but only for the explorer slot.
    pub(crate) fn sync_viewport_to_explorer_editor(&mut self) {
        let Some(ref ep) = self.explorer else { return };
        let win_id = ep.win_id;
        let Some(slot_idx) = self.slots.iter().position(|s| s.is_explorer) else {
            return;
        };
        let (row, col, top) = {
            let win = self.windows.get(win_id).and_then(|w| w.as_ref());
            match win {
                Some(w) => (w.cursor_row, w.cursor_col, w.top_row),
                None => return,
            }
        };
        let editor = &mut self.slots[slot_idx].editor;
        editor.jump_cursor(row, col);
        let vp = editor.host_mut().viewport_mut();
        vp.top_row = top;
    }

    /// "Follow" the active buffer in the explorer: when the explorer is open,
    /// reveal the active buffer's file (expand its ancestors) and move the
    /// explorer's selection to it — so the buffer you're editing is the
    /// highlighted row in the tree. Does NOT change window focus. No-op for
    /// scratch buffers or files outside the tree root.
    pub(crate) fn explorer_reveal_active(&mut self) {
        if self.explorer.is_none() {
            return;
        }
        let Some(fname) = self.active().filename.clone() else {
            return;
        };
        // Tree nodes are absolute (root = cwd); the active filename may be cwd-
        // relative. Resolve to absolute before matching.
        let abs = if fname.is_absolute() {
            fname
        } else {
            std::env::current_dir()
                .map(|c| c.join(&fname))
                .unwrap_or(fname)
        };

        let Some(slot_idx) = self.explorer_slot_idx() else {
            return;
        };

        // Locate the target row, syncing ancestor `expanded` state. `reveal`
        // rebuilds the tree from disk ONLY when the node is missing (e.g. a
        // freshly-created file), which changes the node count.
        let (win_id, row, structural) = {
            let Some(ep) = self.explorer.as_mut() else {
                return;
            };
            if !abs.starts_with(&ep.tree.root) {
                return; // file isn't under the explorer's root
            }
            let before = ep.tree.nodes.len();
            let row = ep.tree.reveal(&abs);
            let structural = ep.tree.nodes.len() != before;
            (ep.win_id, row, structural)
        };

        if structural {
            // The node list changed (new file appeared): rebuild the buffer so
            // it matches, applying folds from the now-updated `expanded` set
            // (ancestors opened by `reveal`). Only paid on the rare new-file
            // path — NOT on every buffer switch.
            self.explorer_rebuild_buffer();
            self.recompute_explorer_git_base();
        }

        let Some(row) = row else {
            return;
        };

        if !structural {
            // Common case: open ONLY the folds enclosing the target row in the
            // buffer, leaving every other dir's fold state intact. This avoids
            // the full `set_content` + fold-recompute reset that collapsed
            // search-revealed dirs, hid the target row (cursorline landed on
            // the wrong line), and reshuffled the scroll position.
            self.slots[slot_idx].editor.buffer_mut().reveal_row(row);
            self.sync_explorer_window_folds();
        }

        // Move the explorer selection to the revealed row and scroll it into
        // view ONLY if it isn't already visible. The check must be FOLD-AWARE:
        // collapsed dirs above the cursor make the raw row index much larger
        // than its on-screen position, so a naive `row >= top + height` compare
        // wrongly scrolled the pane every time a (clearly visible) file was
        // clicked. Walk visible rows from `top_row` instead.
        let cur_top = self
            .windows
            .get(win_id)
            .and_then(|w| w.as_ref())
            .map(|w| w.top_row)
            .unwrap_or(0);
        let height = self
            .windows
            .get(win_id)
            .and_then(|w| w.as_ref())
            .and_then(|w| w.last_rect)
            .map(|rc| rc.h as usize)
            .unwrap_or(0);
        let new_top = if height == 0 {
            cur_top
        } else if row < cur_top {
            row
        } else {
            // Is `row` among the `height` visible rows starting at `cur_top`?
            let buf = self.slots[slot_idx].editor.buffer();
            let mut r = cur_top;
            let mut visible = false;
            for _ in 0..height {
                if r == row {
                    visible = true;
                    break;
                }
                match buf.next_visible_row(r) {
                    Some(n) => r = n,
                    None => break,
                }
            }
            if visible {
                cur_top
            } else {
                // Off the bottom: make `row` the last visible line by walking
                // back `height - 1` visible rows from it.
                let mut t = row;
                for _ in 0..height.saturating_sub(1) {
                    match buf.prev_visible_row(t) {
                        Some(p) => t = p,
                        None => break,
                    }
                }
                t
            }
        };
        if let Some(Some(win)) = self.windows.get_mut(win_id) {
            win.cursor_row = row;
            win.cursor_col = 0;
            win.top_row = new_top;
        }
        // Sync the explorer editor cursor so the (usually unfocused) cursorline
        // highlights the revealed row.
        self.sync_viewport_to_explorer_editor();
    }

    /// Enter/l/o on the explorer: toggle dir or open file.
    pub(crate) fn explorer_activate(&mut self) {
        // Determine the cursor row in the explorer window.
        let cursor_row = {
            let ep = self.explorer.as_ref().unwrap();
            let win = self.windows.get(ep.win_id).and_then(|w| w.as_ref());
            win.map(|w| w.cursor_row).unwrap_or(0)
        };

        // Get the node at cursor.
        let node = self
            .explorer
            .as_ref()
            .and_then(|ep| ep.tree.nodes.get(cursor_row))
            .cloned();

        let Some(node) = node else { return };

        if node.is_dir {
            // Lazy expand/collapse: flip the dir's `expanded` state and rebuild.
            // Expanding reads that dir's one level from disk (instant); collapsing
            // drops its subtree from `nodes`. `explorer_rebuild_buffer` re-renders
            // and keeps the cursor on this dir's path.
            let path = node.path.clone();
            if let Some(ref mut ep) = self.explorer {
                let now = ep.tree.is_expanded(&path);
                ep.tree.set_expanded(&path, !now);
                ep.tree.rebuild();
            }
            self.explorer_rebuild_buffer();
        } else {
            // File: open in the nearest non-explorer window.
            let target_win = self.nearest_non_explorer_window();
            if let Some(win_id) = target_win {
                self.switch_focus(win_id);
            }
            let s = Self::explorer_open_arg(&node.path);
            // Suppress the `"file" NL` open toast — opening from the tree is an
            // explicit action and the buffer visibly changes.
            self.suppress_open_notice = true;
            self.dispatch_ex(&format!("edit {s}"));
            self.suppress_open_notice = false;
        }
    }

    /// `o`/`O` when the cursor is on a DIRECTORY: create the new entry as a
    /// CHILD of that dir (not a sibling). Opens the dir's fold so the new line
    /// sits right under it, runs the normal open-line-below, then indents the
    /// fresh line to child depth so reconcile resolves the parent to this dir.
    /// Returns `false` (not handled) when the cursor isn't on a directory — the
    /// caller then lets the normal `o`/`O` create a sibling.
    pub(crate) fn explorer_open_in_dir(&mut self) -> bool {
        let node = match self.explorer_cursor_node() {
            Some(n) if n.is_dir => n,
            _ => return false,
        };
        let Some(slot_idx) = self.explorer_slot_idx() else {
            return false;
        };
        // Expand the dir (lazy-load its one level) so the new line lands as its
        // first child, then re-render. If already expanded this is a cheap
        // re-read. `explorer_rebuild_buffer` keeps the cursor on the dir row.
        if let Some(ref mut ep) = self.explorer
            && !ep.tree.is_expanded(&node.path)
        {
            ep.tree.set_expanded(&node.path, true);
            ep.tree.rebuild();
        }
        self.explorer_rebuild_buffer();
        // Open a line below + enter insert (autoindent copies the dir's indent).
        hjkl_vim_tui::handle_key(
            &mut self.slots[slot_idx].editor,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('o'),
                crossterm::event::KeyModifiers::NONE,
            ),
        );
        // Indent the new line to CHILD depth (dir.depth + 1) so reconcile nests
        // it under this dir. Pad from the current cursor column (robust whether
        // or not autoindent already copied the dir's indent).
        let target_col = (node.depth + 1) * 2 + 2;
        let col = self.slots[slot_idx].editor.cursor().1;
        if col < target_col {
            let pad = " ".repeat(target_col - col);
            self.slots[slot_idx].editor.insert_str(&pad);
        }
        true
    }

    /// `p` when the cursor is on a DIRECTORY: paste the cut/yanked tree lines
    /// INSIDE that dir (re-indented as its children) so `dd` a dir/file then
    /// `p` on another dir MOVES the entry into it. The pasted lines have their
    /// `<US><id>` tails stripped, so reconcile treats them as fresh creates —
    /// files are restored from trash by name (contents preserved), making the
    /// whole thing a real move.
    ///
    /// Returns `false` (not handled) when the cursor isn't on a directory or the
    /// register isn't a linewise tree block — the caller then runs the normal
    /// sibling paste.
    pub(crate) fn explorer_paste_in_dir(&mut self) -> bool {
        use super::explorer_reconcile::ID_SEP;
        let Some(slot_idx) = self.explorer_slot_idx() else {
            return false;
        };
        let cursor_row = self.slots[slot_idx].editor.cursor().0;
        // Node under the editor cursor must be a directory.
        let node = match self
            .explorer
            .as_ref()
            .and_then(|ep| ep.tree.nodes.get(cursor_row))
            .cloned()
        {
            Some(n) if n.is_dir => n,
            _ => return false,
        };

        // Register must hold a linewise tree block (from `dd`/`yy` of rows).
        let slot = self.slots[slot_idx].editor.registers().unnamed.clone();
        if !slot.linewise || slot.text.trim().is_empty() {
            return false;
        }

        // Strip the id tail from each line + drop blanks.
        let lines: Vec<String> = slot
            .text
            .trim_end_matches('\n')
            .split('\n')
            .map(|l| l.split(ID_SEP).next().unwrap_or(l).to_string())
            .filter(|l| !l.trim().is_empty())
            .collect();
        if lines.is_empty() {
            return false;
        }

        // Re-indent so the block's shallowest line becomes a child of this dir
        // (depth+1); deeper lines keep their relative offset.
        let min_indent = lines
            .iter()
            .map(|l| l.len() - l.trim_start_matches(' ').len())
            .min()
            .unwrap_or(0);
        let target_indent = (node.depth + 1) * 2 + 2;
        let delta = target_indent as isize - min_indent as isize;
        let block: String = lines
            .iter()
            .map(|l| {
                let cur = l.len() - l.trim_start_matches(' ').len();
                let new_indent = (cur as isize + delta).max(0) as usize;
                format!("{}{}", " ".repeat(new_indent), &l[cur..])
            })
            .collect::<Vec<_>>()
            .join("\n");

        // Expand the dir (lazy-load its one level) so the paste lands as visible
        // children right after the dir line, not after a collapsed subtree.
        if let Some(ref mut ep) = self.explorer
            && !ep.tree.is_expanded(&node.path)
        {
            ep.tree.set_expanded(&node.path, true);
            ep.tree.rebuild();
        }
        // `explorer_rebuild_buffer` keeps the editor cursor on the dir's row, so
        // `paste_after` below targets the dir's first-child position.
        self.explorer_rebuild_buffer();

        // Install the re-indented, id-less block as a linewise register and
        // paste below the dir line.
        self.slots[slot_idx].editor.registers_mut().unnamed = hjkl_engine::Slot {
            text: block,
            linewise: true,
        };
        self.slots[slot_idx].editor.paste_after(1);
        true
    }

    /// Find the nearest non-explorer window in the active tab's layout.
    pub(crate) fn nearest_non_explorer_window(&self) -> Option<super::window::WindowId> {
        let leaves = self.layout().leaves();
        let explorer_win = self.explorer.as_ref().map(|ep| ep.win_id);
        // Prefer the currently focused non-explorer window.
        let fw = self.focused_window();
        if Some(fw) != explorer_win {
            return Some(fw);
        }
        // Fall back to the first non-explorer leaf.
        leaves
            .into_iter()
            .find(|&win_id| Some(win_id) != explorer_win)
    }

    // ── Cursor-node helpers ───────────────────────────────────────────────────

    /// Return the node currently under the explorer cursor.
    fn explorer_cursor_node(&self) -> Option<ExplorerNode> {
        let ep = self.explorer.as_ref()?;
        let win = self.windows.get(ep.win_id)?.as_ref()?;
        ep.tree.nodes.get(win.cursor_row).cloned()
    }

    /// Path string for an `:edit`/`:split`/… open command. Relative to the cwd
    /// when the file is under it (so buffer names match normally-opened files
    /// instead of showing absolute paths in the picker / buffer line), else
    /// absolute.
    fn explorer_open_arg(path: &Path) -> String {
        if let Ok(cwd) = std::env::current_dir()
            && let Ok(rel) = path.strip_prefix(&cwd)
        {
            return rel.to_string_lossy().into_owned();
        }
        path.to_string_lossy().into_owned()
    }

    // ── Refresh / hidden / root ───────────────────────────────────────────────

    /// Toggle dotfile visibility and rebuild.
    pub(crate) fn explorer_toggle_hidden(&mut self) {
        if let Some(ref mut ep) = self.explorer {
            ep.tree.toggle_hidden();
        }
        self.explorer_rebuild_buffer();
    }

    /// Toggle git-ignore honoring (show/hide ignored entries) and rebuild.
    pub(crate) fn explorer_toggle_gitignore(&mut self) {
        if let Some(ref mut ep) = self.explorer {
            ep.tree.toggle_gitignore();
        }
        self.explorer_rebuild_buffer();
    }

    /// Move the tree root up to its parent directory.
    pub(crate) fn explorer_root_up(&mut self) {
        let parent = self
            .explorer
            .as_ref()
            .and_then(|ep| ep.tree.root.parent().map(|p| p.to_path_buf()));
        if let Some(parent) = parent {
            if let Some(ref mut ep) = self.explorer {
                ep.tree.set_root(parent);
            }
            self.explorer_rebuild_buffer();
        }
    }

    // ── Open modes ───────────────────────────────────────────────────────────

    /// Open the file under cursor in a horizontal split.
    pub(crate) fn explorer_open_split(&mut self) {
        let node = match self.explorer_cursor_node() {
            Some(n) if !n.is_dir => n,
            _ => return,
        };
        if let Some(win_id) = self.nearest_non_explorer_window() {
            self.switch_focus(win_id);
        }
        let s = Self::explorer_open_arg(&node.path);
        self.dispatch_ex(&format!("split {s}"));
    }

    /// Open the file under cursor in a vertical split.
    pub(crate) fn explorer_open_vsplit(&mut self) {
        let node = match self.explorer_cursor_node() {
            Some(n) if !n.is_dir => n,
            _ => return,
        };
        if let Some(win_id) = self.nearest_non_explorer_window() {
            self.switch_focus(win_id);
        }
        let s = Self::explorer_open_arg(&node.path);
        self.dispatch_ex(&format!("vsplit {s}"));
    }

    /// Open the file under cursor in a new tab.
    pub(crate) fn explorer_open_tab(&mut self) {
        let node = match self.explorer_cursor_node() {
            Some(n) if !n.is_dir => n,
            _ => return,
        };
        let s = Self::explorer_open_arg(&node.path);
        self.dispatch_ex(&format!("tabnew {s}"));
    }

    /// Route an explorer [`AppAction`](crate::keymap_actions::AppAction) to the
    /// corresponding `explorer_*` method. Called from `dispatch_action` when the
    /// match arm groups all `Explorer*` variants together.
    pub(crate) fn dispatch_explorer_action(&mut self, action: crate::keymap_actions::AppAction) {
        use crate::keymap_actions::AppAction;
        match action {
            AppAction::ExplorerActivate => self.explorer_activate(),
            AppAction::ExplorerOpenSplit => self.explorer_open_split(),
            AppAction::ExplorerOpenVsplit => self.explorer_open_vsplit(),
            AppAction::ExplorerOpenTab => self.explorer_open_tab(),
            AppAction::ExplorerRootUp => self.explorer_root_up(),
            AppAction::ExplorerToggleHidden => self.explorer_toggle_hidden(),
            AppAction::ExplorerToggleGitignore => self.explorer_toggle_gitignore(),
            AppAction::ExplorerGitStageToggle => self.explorer_git_stage_toggle(),
            AppAction::ExplorerGitDiscard => self.explorer_git_discard(),
            AppAction::ExplorerGitCommit => self.explorer_git_commit(),
            _ => {}
        }
    }

    // ── Git path operations ───────────────────────────────────────────────────

    /// `ga` — stage or unstage the node under the cursor.
    ///
    /// If the node's git status is [`ExplorerGit::Staged`] the path is
    /// unstaged; otherwise it is staged. After success the explorer git base is
    /// recomputed and the buffer is rebuilt so the gutter colours update
    /// immediately.
    pub(crate) fn explorer_git_stage_toggle(&mut self) {
        let node = match self.explorer_cursor_node() {
            Some(n) => n,
            None => return,
        };
        let ep = match self.explorer.as_ref() {
            Some(ep) => ep,
            None => return,
        };
        if !ep.tree.repo_present {
            return;
        }
        let root = match hjkl_app::git::repo_root(&node.path) {
            Some(r) => r,
            None => return,
        };
        let result = if node.git == Some(hjkl_app::git::ExplorerGit::Staged) {
            hjkl_app::git::unstage_path(&root, &node.path)
        } else {
            hjkl_app::git::stage_path(&root, &node.path)
        };
        match result {
            Err(e) => {
                self.bus.error(format!("Git stage failed: {e}"));
            }
            Ok(()) => {
                self.recompute_explorer_git_base();
                self.explorer_rebuild_buffer();
                self.refresh_git_signs_force();
            }
        }
    }

    /// `gr` — open a confirm overlay to discard worktree changes for the node
    /// under the cursor.
    ///
    /// Only opens the confirm when the node has tracked worktree changes
    /// (Modified or Deleted). Untracked files are skipped — `git checkout`
    /// does not affect them and it would be a surprising no-op. The confirm
    /// prompt reads "Discard changes to <name>? (y/n)".
    pub(crate) fn explorer_git_discard(&mut self) {
        let node = match self.explorer_cursor_node() {
            Some(n) => n,
            None => return,
        };
        let ep = match self.explorer.as_ref() {
            Some(ep) => ep,
            None => return,
        };
        if !ep.tree.repo_present {
            return;
        }
        // Only offer discard for paths that have tracked worktree changes.
        // Untracked → `git checkout` would fail / be a no-op; Staged → use `ga`
        // to unstage first. Directories with a rolled-up status (git == None)
        // also proceed — `git checkout -- <dir>` is recursive over tracked files.
        let allow = match node.git {
            Some(hjkl_app::git::ExplorerGit::Modified | hjkl_app::git::ExplorerGit::Deleted) => {
                true
            }
            None if node.is_dir => true,
            _ => false,
        };
        if !allow {
            return;
        }
        self.explorer_git_discard_confirm = Some(node.path.clone());
    }

    /// Called when the user confirms a git-discard with `y`.
    pub(crate) fn explorer_commit_git_discard(&mut self) {
        let path = match self.explorer_git_discard_confirm.take() {
            Some(p) => p,
            None => return,
        };
        let root = match hjkl_app::git::repo_root(&path) {
            Some(r) => r,
            None => {
                self.bus.error("Git discard failed: path not in a repo");
                return;
            }
        };
        match hjkl_app::git::discard_path(&root, &path) {
            Ok(()) => {
                self.recompute_explorer_git_base();
                self.explorer_rebuild_buffer();
                self.refresh_git_signs_force();
            }
            Err(e) => {
                self.bus.error(format!("Git discard failed: {e}"));
            }
        }
    }

    /// `gc` — open COMMIT_EDITMSG in a split for committing staged changes.
    ///
    /// Resolves the explorer tree root as the git root. Opens the repo's
    /// `COMMIT_EDITMSG` file pre-filled with a comment template and
    /// `git status --short --branch`. On window close the commit hook in
    /// `close_focused_window` runs `git commit --cleanup=strip -F <msg_file>`.
    pub(crate) fn explorer_git_commit(&mut self) {
        // Resolve root from the explorer tree.
        let root = match self.explorer.as_ref() {
            Some(ep) => {
                if !ep.tree.repo_present {
                    self.bus.warn("not a git repository");
                    return;
                }
                match hjkl_app::git::repo_root(&ep.tree.root) {
                    Some(r) => r,
                    None => {
                        self.bus.warn("not a git repository");
                        return;
                    }
                }
            }
            None => {
                self.bus.warn("not a git repository");
                return;
            }
        };

        let msg_file = match hjkl_app::git::commit_edit_path(&root) {
            Some(p) => p,
            None => {
                self.bus.error("could not resolve COMMIT_EDITMSG path");
                return;
            }
        };

        let template = hjkl_app::git::commit_template(&root);
        if let Err(e) = std::fs::write(&msg_file, &template) {
            self.bus
                .error(format!("could not write commit template: {e}"));
            return;
        }

        // Focus a non-explorer window first (same as explorer_open_split).
        if let Some(win_id) = self.nearest_non_explorer_window() {
            self.switch_focus(win_id);
        }

        // Open the COMMIT_EDITMSG in a horizontal split.
        let s = Self::explorer_open_arg(&msg_file);
        self.dispatch_ex(&format!("split {s}"));

        // Find the newly focused slot (the split just opened) and attach ctx.
        let slot_idx = self.focused_slot_idx();
        self.slots[slot_idx].commit_ctx = Some(super::types::CommitCtx { root, msg_file });
    }

    /// Route a key when `explorer_git_discard_confirm` is active.
    pub(crate) fn handle_explorer_git_discard_confirm_key(
        &mut self,
        key: crossterm::event::KeyEvent,
    ) {
        use crossterm::event::KeyCode;
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.explorer_commit_git_discard();
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.explorer_git_discard_confirm = None;
            }
            _ => {} // consume but do nothing
        }
    }

    /// Text shown in the confirm bar when a git-discard confirmation is pending.
    #[allow(dead_code)]
    pub(crate) fn explorer_git_discard_confirm_prompt(&self) -> Option<String> {
        let path = self.explorer_git_discard_confirm.as_ref()?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        Some(format!("Discard changes to {name}? (y/n)"))
    }
}

/// Width of the explorer window in columns.
pub(crate) const EXPLORER_WINDOW_WIDTH: u16 = 36;

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Build a unique temp dir tree:
    /// root/{ a_dir/{ inner.txt }, b_dir/, m_file.txt, z_file.txt }
    fn make_tree() -> PathBuf {
        let base = std::env::temp_dir().join(format!("hjkl_explorer_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("a_dir")).unwrap();
        fs::create_dir_all(base.join("b_dir")).unwrap();
        fs::write(base.join("a_dir").join("inner.txt"), "x").unwrap();
        fs::write(base.join("m_file.txt"), "x").unwrap();
        fs::write(base.join("z_file.txt"), "x").unwrap();
        base
    }

    /// Names of the non-root nodes.
    fn child_names(tree: &ExplorerTree) -> Vec<String> {
        tree.nodes[1..]
            .iter()
            .map(|n| n.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn root_first_then_dirs_first_then_name() {
        let root = make_tree();
        let mut tree = ExplorerTree::new(root.clone());
        assert_eq!(tree.nodes[0].path, root);
        assert!(tree.nodes[0].is_dir && tree.nodes[0].depth == 0);
        // Lazy walk: only the top level is present (a_dir's `inner.txt` absent).
        let names = child_names(&tree);
        for top in ["a_dir", "b_dir", "m_file.txt", "z_file.txt"] {
            assert!(names.contains(&top.to_string()), "{top} must be present");
        }
        assert!(
            !names.contains(&"inner.txt".to_string()),
            "inner.txt is under a collapsed dir — must NOT be present yet; got {names:?}"
        );
        // Top-level order: dirs first (a_dir, b_dir), then files (m, z).
        let a_idx = names.iter().position(|n| n == "a_dir").unwrap();
        let b_idx = names.iter().position(|n| n == "b_dir").unwrap();
        let m_idx = names.iter().position(|n| n == "m_file.txt").unwrap();
        assert!(
            a_idx < b_idx && b_idx < m_idx,
            "dirs-first order, got {names:?}"
        );

        // Expand a_dir → its child appears in DFS order, right after a_dir.
        let a_path = tree.nodes[1].path.clone();
        tree.set_expanded(&a_path, true);
        tree.rebuild();
        let names = child_names(&tree);
        let a_idx = names.iter().position(|n| n == "a_dir").unwrap();
        let i_idx = names.iter().position(|n| n == "inner.txt").unwrap();
        let b_idx = names.iter().position(|n| n == "b_dir").unwrap();
        assert!(
            a_idx < i_idx && i_idx < b_idx,
            "DFS after expand: a_dir < inner.txt < b_dir, got {names:?}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn expand_inserts_children_at_depth_with_guide() {
        let root = make_tree();
        let mut tree = ExplorerTree::new(root.clone());
        // Lazy: inner.txt absent until a_dir is expanded.
        assert!(!tree.nodes.iter().any(|n| n.path.ends_with("inner.txt")));
        let a_path = tree.nodes[1].path.clone();
        tree.set_expanded(&a_path, true);
        tree.rebuild();
        // a_dir = nodes[1], inner.txt = nodes[2] at depth 2 with a guide.
        let inner = &tree.nodes[2];
        assert_eq!(inner.depth, 2);
        assert!(!inner.is_dir);
        assert_eq!(inner.branches, vec![true]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn last_child_flag() {
        let root = make_tree();
        let tree = ExplorerTree::new(root.clone());
        assert!(!tree.nodes[1].is_last); // a_dir is NOT last (b_dir, files follow)
        let z = tree.nodes.last().unwrap();
        assert_eq!(z.path.file_name().unwrap(), "z_file.txt");
        assert!(z.is_last);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn collapse_removes_children_from_nodes() {
        // Lazy model: expand/collapse adds/removes the dir's children from
        // `nodes` (no buffer folds — the node list IS the visible tree).
        let root = make_tree();
        let mut tree = ExplorerTree::new(root.clone());
        let a_dir_path = tree.nodes[1].path.clone();
        let has_inner = |t: &ExplorerTree| t.nodes.iter().any(|n| n.path.ends_with("inner.txt"));

        // a_dir starts collapsed → inner.txt absent.
        assert!(!has_inner(&tree), "child of collapsed dir must be absent");

        // Expand → child node appears.
        tree.toggle(&a_dir_path);
        tree.rebuild();
        assert!(has_inner(&tree), "expand must add the child node");

        // Collapse → child node removed.
        tree.toggle(&a_dir_path);
        tree.rebuild();
        assert!(!has_inner(&tree), "collapse must remove the child node");

        // The lazy explorer produces no folds.
        assert!(
            tree.compute_folds().is_empty(),
            "lazy explorer has no folds"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn render_text_line_count_matches_nodes() {
        let root = make_tree();
        let tree = ExplorerTree::new(root.clone());
        let text = tree.render_text();
        let line_count = text.lines().count();
        assert_eq!(
            line_count,
            tree.nodes.len(),
            "render_text line count must equal node count"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn render_text_line_count_after_toggle() {
        // Lazy model: render_text always equals nodes.len() (no hidden lines),
        // and expanding a dir adds its children, growing the line count.
        let root = make_tree();
        let mut tree = ExplorerTree::new(root.clone());
        let line_count_before = tree.render_text().lines().count();
        assert_eq!(line_count_before, tree.nodes.len());

        let a_dir_path = tree.nodes[1].path.clone();
        tree.set_expanded(&a_dir_path, true);
        tree.rebuild();
        let line_count_after = tree.render_text().lines().count();
        assert_eq!(line_count_after, tree.nodes.len());
        assert!(
            line_count_after > line_count_before,
            "expanding a_dir must add inner.txt's line"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// Whole-tree search integration: a deep file in a collapsed dir is absent
    /// from the lazy tree, but opening it (what selecting it in the fuzzy finder
    /// does) reveals it — expanding its ancestors and selecting its row. This is
    /// the Snacks-style "find file → reveal in tree" path.
    #[test]
    fn opening_deep_file_reveals_it_in_lazy_tree() {
        use crate::keymap_actions::AppAction;
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("a").join("b")).unwrap();
        std::fs::write(tmp.path().join("a").join("b").join("deep.txt"), b"x").unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let mut app = super::super::App::new(None, false, None, None).unwrap();
        app.dispatch_action(AppAction::ToggleExplorer, 1);

        let has_deep = |app: &super::super::App| {
            app.explorer
                .as_ref()
                .unwrap()
                .tree
                .nodes
                .iter()
                .any(|n| n.path.file_name().map(|f| f == "deep.txt").unwrap_or(false))
        };
        // Lazy: deep.txt is under collapsed dirs → absent.
        assert!(!has_deep(&app), "deep.txt must be absent before reveal");

        // Open it (the fuzzy finder's select path does exactly this).
        app.dispatch_action(AppAction::FocusRight, 1);
        app.dispatch_ex("edit a/b/deep.txt");

        // The explorer followed the active buffer: deep.txt now in the tree, and
        // its ancestor dirs were lazily expanded to reach it.
        let revealed = has_deep(&app);
        let a_expanded = app
            .explorer
            .as_ref()
            .unwrap()
            .tree
            .is_expanded(&std::env::current_dir().unwrap().join("a"));
        std::env::set_current_dir(prev).unwrap();
        assert!(revealed, "opening deep.txt must reveal it in the lazy tree");
        assert!(a_expanded, "reveal must lazily expand ancestor dir `a`");
    }

    /// `/` in the explorer opens the whole-tree fuzzy finder (not a plain buffer
    /// search over the visible lazy rows).
    #[test]
    fn slash_in_explorer_opens_fuzzy_finder() {
        use crate::keymap_actions::AppAction;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.txt"), b"x").unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let mut app = super::super::App::new(None, false, None, None).unwrap();
        app.dispatch_action(AppAction::ToggleExplorer, 1);
        assert!(app.explorer_buf_focused(), "explorer must be focused");
        assert!(app.picker.is_none() && app.search_field.is_none());

        // Press `/` through the production keypress path.
        let _ = app.handle_keypress(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));

        let picker_open = app.picker.is_some();
        let search_open = app.search_field.is_some();
        std::env::set_current_dir(prev).unwrap();
        assert!(picker_open, "/ in the explorer must open the fuzzy finder");
        assert!(
            !search_open,
            "/ in the explorer must NOT open the plain buffer search prompt"
        );
    }

    #[test]
    fn toggle_explorer_creates_window_and_is_explorer_slot() {
        use crate::keymap_actions::AppAction;
        let mut app = super::super::App::new(None, false, None, None).unwrap();
        assert!(app.explorer.is_none());
        // Open explorer.
        app.dispatch_action(AppAction::ToggleExplorer, 1);
        assert!(app.explorer.is_some(), "explorer should be open");
        assert!(
            app.slots.iter().any(|s| s.is_explorer),
            "explorer slot must have is_explorer = true"
        );
        // Close explorer.
        app.dispatch_action(AppAction::ToggleExplorer, 1);
        assert!(app.explorer.is_none(), "explorer should be closed");
    }

    #[test]
    fn explorer_reconcile_creates_file_on_disk() {
        use crate::keymap_actions::AppAction;
        use hjkl_engine::BufferEdit;
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("existing.txt"), "hi").unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let mut app = super::super::App::new(None, false, None, None).unwrap();
        app.dispatch_action(AppAction::ToggleExplorer, 1);
        let idx = app.explorer_slot_idx().expect("explorer slot");
        // Append a new file line at the root depth (name_col = 2).
        let cur = app.slots[idx].editor.buffer().as_string();
        let newtext = format!("{cur}\n  newfile.rs");
        BufferEdit::replace_all(app.slots[idx].editor.buffer_mut(), &newtext);
        // Explorer is in Normal mode by default; run the reconcile.
        app.maybe_reconcile_explorer();

        let created = tmp.path().join("newfile.rs");
        let exists = created.exists();
        std::env::set_current_dir(prev).unwrap();
        assert!(exists, "reconcile must create newfile.rs on disk");
    }

    #[test]
    fn explorer_o_type_esc_creates_file_real_flow() {
        use crate::keymap_actions::AppAction;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("existing.txt"), "hi").unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let mut app = super::super::App::new(None, false, None, None).unwrap();
        app.dispatch_action(AppAction::ToggleExplorer, 1);

        // Replicate the event loop: handle each key; on FallThrough, route to
        // the insert dispatcher (insert mode) or the engine (otherwise); then
        // run the post-key reconcile hook exactly as run() does.
        use crate::app::event_loop::KeyOutcome;
        use hjkl_engine::VimMode;
        let press = |app: &mut super::super::App, code: KeyCode| {
            let key = KeyEvent::new(code, KeyModifiers::NONE);
            let consumed = matches!(
                app.handle_keypress(key),
                KeyOutcome::Continue | KeyOutcome::Break
            );
            if !consumed {
                if app.active_editor().vim_mode() == VimMode::Insert {
                    app.dispatch_insert_key(key);
                } else {
                    hjkl_vim_tui::handle_key(app.active_editor_mut(), key);
                }
            }
            app.maybe_reconcile_explorer();
        };
        press(&mut app, KeyCode::Char('o'));
        for c in "made.rs".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Esc);

        let created = tmp.path().join("made.rs");
        let exists = created.exists();
        // The list must reflect disk: the created file appears in the buffer.
        let idx = app.explorer_slot_idx().unwrap();
        let buf = app.slots[idx].editor.buffer().as_string();
        std::env::set_current_dir(prev).unwrap();
        assert!(exists, "o + type + Esc must create the file on disk");
        assert!(
            buf.contains("made.rs"),
            "created file must appear in the explorer list; buf=<<<{buf}>>>"
        );
    }

    /// Creating a multi-level item (`somedir/test.txt`) must auto-expand the new
    /// directory so the freshly-created leaf is visible in the lazy tree.
    #[test]
    fn create_multilevel_item_expands_new_dir() {
        use crate::keymap_actions::AppAction;
        use hjkl_engine::BufferEdit;
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("existing.txt"), "hi").unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let mut app = super::super::App::new(None, false, None, None).unwrap();
        app.dispatch_action(AppAction::ToggleExplorer, 1);
        let idx = app.explorer_slot_idx().unwrap();

        // Append a root-level line for a nested create (2-space indent = depth 1).
        let cur = app.slots[idx].editor.buffer().as_string();
        let newtext = format!("{cur}\n  somedir/test.txt");
        BufferEdit::replace_all(app.slots[idx].editor.buffer_mut(), &newtext);
        app.maybe_reconcile_explorer();

        let cwd = std::env::current_dir().unwrap();
        let dir_exists = cwd.join("somedir").is_dir();
        let file_exists = cwd.join("somedir").join("test.txt").exists();
        let ep = app.explorer.as_ref().unwrap();
        let expanded = ep.tree.is_expanded(&cwd.join("somedir"));
        let test_visible = ep
            .tree
            .nodes
            .iter()
            .any(|n| n.path.file_name().map(|f| f == "test.txt").unwrap_or(false));
        std::env::set_current_dir(prev).unwrap();

        assert!(dir_exists, "somedir/ must be created on disk");
        assert!(file_exists, "somedir/test.txt must be created on disk");
        assert!(
            expanded,
            "the new dir must be expanded after creating a child in it"
        );
        assert!(
            test_visible,
            "test.txt must be visible in the tree after the multi-level create"
        );
    }

    #[test]
    fn explorer_dd_trashes_file_and_drops_it_from_list() {
        use crate::app::event_loop::KeyOutcome;
        use crate::keymap_actions::AppAction;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use hjkl_engine::VimMode;
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("victim.txt"), "bye").unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let mut app = super::super::App::new(None, false, None, None).unwrap();
        app.dispatch_action(AppAction::ToggleExplorer, 1);

        let press = |app: &mut super::super::App, code: KeyCode| {
            let key = KeyEvent::new(code, KeyModifiers::NONE);
            let consumed = matches!(
                app.handle_keypress(key),
                KeyOutcome::Continue | KeyOutcome::Break
            );
            if !consumed {
                if app.active_editor().vim_mode() == VimMode::Insert {
                    app.dispatch_insert_key(key);
                } else {
                    hjkl_vim_tui::handle_key(app.active_editor_mut(), key);
                }
            }
            app.maybe_reconcile_explorer();
        };
        // Move onto the victim.txt line (line 1, below the root) and delete it.
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Char('d'));

        let on_disk = tmp.path().join("victim.txt").exists();
        let idx = app.explorer_slot_idx().unwrap();
        let buf = app.slots[idx].editor.buffer().as_string();
        std::env::set_current_dir(prev).unwrap();
        assert!(!on_disk, "dd must remove victim.txt from disk (to trash)");
        assert!(
            !buf.contains("victim.txt"),
            "dd'd file must drop from the list; buf=<<<{buf}>>>"
        );
    }

    #[test]
    fn create_keeps_focus_in_explorer() {
        use crate::app::event_loop::KeyOutcome;
        use crate::keymap_actions::AppAction;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use hjkl_engine::VimMode;
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("existing.txt"), "hi").unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let mut app = super::super::App::new(None, false, None, None).unwrap();
        app.dispatch_action(AppAction::ToggleExplorer, 1);
        let press = |app: &mut super::super::App, code: KeyCode| {
            let key = KeyEvent::new(code, KeyModifiers::NONE);
            let consumed = matches!(
                app.handle_keypress(key),
                KeyOutcome::Continue | KeyOutcome::Break
            );
            if !consumed {
                if app.active_editor().vim_mode() == VimMode::Insert {
                    app.dispatch_insert_key(key);
                } else {
                    hjkl_vim_tui::handle_key(app.active_editor_mut(), key);
                }
            }
            app.maybe_reconcile_explorer();
        };
        press(&mut app, KeyCode::Char('o'));
        for c in "newone.rs".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Esc);

        let focused_explorer = app.explorer_buf_focused();
        let on_disk = tmp.path().join("newone.rs").exists();
        std::env::set_current_dir(prev).unwrap();
        assert!(on_disk, "file should be created");
        assert!(
            focused_explorer,
            "focus must stay in the explorer after creating a file"
        );
    }

    #[test]
    fn paste_moves_cursor_to_pasted_file() {
        use crate::app::event_loop::KeyOutcome;
        use crate::keymap_actions::AppAction;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use hjkl_engine::VimMode;
        let tmp = tempfile::tempdir().unwrap();
        // Use a separate cache dir so trash files don't appear in the explorer
        // tree (full-tree walk now includes the hjkl/trash/ subtree).
        let cache_tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "a").unwrap();
        std::fs::write(tmp.path().join("z.txt"), "z").unwrap();
        unsafe { std::env::set_var("XDG_CACHE_HOME", cache_tmp.path()) };
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let mut app = super::super::App::new(None, false, None, None).unwrap();
        app.dispatch_action(AppAction::ToggleExplorer, 1);
        let press = |app: &mut super::super::App, code: KeyCode| {
            let key = KeyEvent::new(code, KeyModifiers::NONE);
            let consumed = matches!(
                app.handle_keypress(key),
                KeyOutcome::Continue | KeyOutcome::Break
            );
            if !consumed {
                if app.active_editor().vim_mode() == VimMode::Insert {
                    app.dispatch_insert_key(key);
                } else {
                    hjkl_vim_tui::handle_key(app.active_editor_mut(), key);
                }
            }
            app.maybe_reconcile_explorer();
        };
        // Cursor onto a.txt (line 1), delete it (trash), then paste it back.
        press(&mut app, KeyCode::Char('j')); // onto a.txt
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Char('d')); // a.txt trashed
        press(&mut app, KeyCode::Char('p')); // paste -> restore a.txt

        let on_disk = tmp.path().join("a.txt").exists();
        let cursor_path = {
            let ep = app.explorer.as_ref().unwrap();
            let row = app
                .windows
                .get(ep.win_id)
                .and_then(|w| w.as_ref())
                .map(|w| w.cursor_row)
                .unwrap();
            ep.tree.nodes.get(row).map(|n| n.path.clone())
        };
        std::env::set_current_dir(prev).unwrap();
        assert!(on_disk, "a.txt should be restored on disk");
        assert_eq!(
            cursor_path.and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())),
            Some("a.txt".to_string()),
            "cursor should land on the pasted file"
        );
    }

    #[test]
    fn o_on_dir_creates_child_inside() {
        use crate::app::event_loop::KeyOutcome;
        use crate::keymap_actions::AppAction;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use hjkl_engine::VimMode;
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("sub")).unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let mut app = super::super::App::new(None, false, None, None).unwrap();
        app.dispatch_action(AppAction::ToggleExplorer, 1);
        let press = |app: &mut super::super::App, code: KeyCode| {
            let key = KeyEvent::new(code, KeyModifiers::NONE);
            let consumed = matches!(
                app.handle_keypress(key),
                KeyOutcome::Continue | KeyOutcome::Break
            );
            if !consumed {
                if app.active_editor().vim_mode() == VimMode::Insert {
                    app.dispatch_insert_key(key);
                } else {
                    hjkl_vim_tui::handle_key(app.active_editor_mut(), key);
                }
            }
            app.maybe_reconcile_explorer();
        };
        // Cursor onto the `sub/` dir (line 1), then `o` + name + Esc.
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('o'));
        for c in "kid.txt".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Esc);

        let inside = tmp.path().join("sub").join("kid.txt").exists();
        let sibling = tmp.path().join("kid.txt").exists();
        std::env::set_current_dir(prev).unwrap();
        assert!(
            inside,
            "o on a dir must create the file INSIDE it (sub/kid.txt)"
        );
        assert!(!sibling, "must NOT create a sibling at root (kid.txt)");
    }

    #[test]
    fn cap_o_on_dir_creates_sibling_not_deeper_neighbor() {
        // Real bug repro: `O` on a sibling dir whose *preceding buffer row* is
        // a DEEPER child of the previous dir must copy the cursor line's indent
        // (sibling level), NOT the deeper neighbour's.
        //   <root>
        //     aaa/
        //       kid.txt   <- deeper row, physically above zzz/
        //     zzz/        <- cursor here, press O
        // Expect `twin.txt` at root, NOT inside aaa/.
        use crate::app::event_loop::KeyOutcome;
        use crate::keymap_actions::AppAction;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use hjkl_engine::VimMode;
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("aaa")).unwrap();
        std::fs::write(tmp.path().join("aaa").join("kid.txt"), b"").unwrap();
        std::fs::create_dir(tmp.path().join("zzz")).unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let mut app = super::super::App::new(None, false, None, None).unwrap();
        app.dispatch_action(AppAction::ToggleExplorer, 1);
        let press = |app: &mut super::super::App, code: KeyCode| {
            let key = KeyEvent::new(code, KeyModifiers::NONE);
            let consumed = matches!(
                app.handle_keypress(key),
                KeyOutcome::Continue | KeyOutcome::Break
            );
            if !consumed {
                if app.active_editor().vim_mode() == VimMode::Insert {
                    app.dispatch_insert_key(key);
                } else {
                    hjkl_vim_tui::handle_key(app.active_editor_mut(), key);
                }
            }
            app.maybe_reconcile_explorer();
        };
        // root(0) → aaa/(1), expand it (reveals kid.txt), → kid.txt(2) → zzz/(3).
        press(&mut app, KeyCode::Char('j')); // onto aaa/
        press(&mut app, KeyCode::Enter); // expand aaa/
        press(&mut app, KeyCode::Char('j')); // onto kid.txt
        press(&mut app, KeyCode::Char('j')); // onto zzz/
        press(&mut app, KeyCode::Char('O'));
        for c in "twin.txt".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Esc);

        let sibling = tmp.path().join("twin.txt").exists();
        let inside = tmp.path().join("aaa").join("twin.txt").exists();
        std::env::set_current_dir(prev).unwrap();
        assert!(
            sibling,
            "O on zzz/ must create a sibling at root (twin.txt)"
        );
        assert!(
            !inside,
            "must NOT inherit the deeper neighbour's indent (aaa/twin.txt)"
        );
    }

    #[test]
    fn buffer_line_click_maps_past_interleaved_explorer_slot() {
        use crate::app::mouse::{Zone, buffer_line_x_ranges, hit_test_zone};
        use crate::keymap_actions::AppAction;
        let pid = std::process::id();
        let f0 = std::env::temp_dir().join(format!("hjkl_bl_a_{pid}.txt"));
        let f1 = std::env::temp_dir().join(format!("hjkl_bl_b_{pid}.txt"));
        std::fs::write(&f0, "a").unwrap();
        std::fs::write(&f1, "b").unwrap();
        let mut app = super::super::App::new(Some(f0.clone()), false, None, None).unwrap();
        // Open the explorer so its slot lands BETWEEN the two file slots.
        app.dispatch_action(AppAction::ToggleExplorer, 1); // slot 1 = explorer
        app.dispatch_action(AppAction::FocusRight, 1); // focus the editor window
        app.dispatch_ex(&format!("edit {}", f1.display())); // slot 2 = f1

        // Buffer line shows f0, f1 (explorer skipped). Clicking the LAST entry
        // must resolve to f1's real slot — NOT the interleaved explorer slot.
        let ranges = buffer_line_x_ranges(&app, 80);
        assert!(ranges.len() >= 2, "expected >=2 entries, got {ranges:?}");
        let col = ranges[ranges.len() - 1].0;
        match hit_test_zone(&app, col, 0) {
            Zone::BufferLine { slot_idx } => {
                assert!(
                    !app.slots[slot_idx].is_explorer,
                    "buffer-line click must not map to the explorer slot"
                );
                assert_eq!(
                    app.slots[slot_idx].filename.as_deref(),
                    Some(f1.as_path()),
                    "last buffer-line entry must map to f1"
                );
            }
            other => panic!("expected BufferLine zone, got {other:?}"),
        }
        let _ = std::fs::remove_file(&f0);
        let _ = std::fs::remove_file(&f1);
    }

    #[test]
    fn switch_to_never_targets_the_explorer_window() {
        use crate::keymap_actions::AppAction;
        let f1 = std::env::temp_dir().join(format!("hjkl_exp_st_{}.txt", std::process::id()));
        std::fs::write(&f1, "hello").unwrap();
        let mut app = super::super::App::new(Some(f1.clone()), false, None, None).unwrap();
        // Open the explorer — it gets focused.
        app.dispatch_action(AppAction::ToggleExplorer, 1);
        assert!(app.explorer_buf_focused(), "explorer should be focused");

        // Switching to the real buffer while the explorer is focused must NOT
        // clobber the explorer pane — it redirects to a non-explorer window.
        let real_idx = app.slots.iter().position(|s| !s.is_explorer).unwrap();
        app.switch_to(real_idx);

        assert!(
            !app.explorer_buf_focused(),
            "switch_to must move focus off the explorer window"
        );
        assert!(
            !app.active().is_explorer,
            "active buffer must be the real one"
        );
        let ep_win = app.explorer.as_ref().unwrap().win_id;
        let ep_slot = app.windows[ep_win].as_ref().unwrap().slot;
        assert!(
            app.slots[ep_slot].is_explorer,
            "the explorer window must still display the explorer buffer"
        );
        let _ = std::fs::remove_file(&f1);
    }

    #[test]
    fn buffer_next_skips_explorer_slot() {
        use crate::keymap_actions::AppAction;
        // Create two real files so buffer_next has something to cycle through.
        let f1 = std::env::temp_dir().join(format!("hjkl_exp_bn_a_{}.txt", std::process::id()));
        let f2 = std::env::temp_dir().join(format!("hjkl_exp_bn_b_{}.txt", std::process::id()));
        std::fs::write(&f1, "hello").unwrap();
        std::fs::write(&f2, "world").unwrap();
        let mut app = super::super::App::new(Some(f1.clone()), false, None, None).unwrap();
        // Open a second real buffer.
        app.dispatch_ex(&format!("edit {}", f2.display()));
        // Open the explorer (it gets focused).
        app.dispatch_action(AppAction::ToggleExplorer, 1);
        assert!(app.explorer.is_some());
        // Focus the right (editor) window so buffer_next operates on a real slot.
        app.dispatch_action(AppAction::FocusRight, 1);
        assert!(!app.active().is_explorer, "should be on a real slot now");
        // buffer_next should never land on the explorer slot.
        for _ in 0..10 {
            app.buffer_next();
            assert!(
                !app.active().is_explorer,
                "buffer_next must skip is_explorer slots"
            );
        }
        let _ = std::fs::remove_file(&f1);
        let _ = std::fs::remove_file(&f2);
    }

    // ── New tests for plan features ────────────────────────────────────────

    /// Build a unique temp dir with dotfiles:
    /// root/{ .hidden_dir/, .hidden_file, visible.txt }
    fn make_dotfile_tree() -> PathBuf {
        let base =
            std::env::temp_dir().join(format!("hjkl_explorer_dot_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join(".hidden_dir")).unwrap();
        fs::write(base.join(".hidden_file"), "x").unwrap();
        fs::write(base.join("visible.txt"), "x").unwrap();
        base
    }

    #[test]
    fn read_children_shows_dotfiles_by_default() {
        let root = make_dotfile_tree();
        let tree = ExplorerTree::new(root.clone());
        // Dotfiles are shown by default now; `.git` would still be skipped.
        let names = child_names(&tree);
        assert!(
            names.iter().any(|n| n.starts_with('.')),
            "dotfiles should be shown by default, got: {names:?}"
        );
        assert!(
            names.contains(&"visible.txt".to_string()),
            "visible.txt should be present"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn toggle_hidden_hides_then_shows_dotfiles() {
        let root = make_dotfile_tree();
        let mut tree = ExplorerTree::new(root.clone());
        assert!(
            child_names(&tree).iter().any(|n| n.starts_with('.')),
            "dotfiles should be shown before toggle"
        );
        tree.toggle_hidden();
        let names = child_names(&tree);
        assert!(
            !names.iter().any(|n| n.starts_with('.')),
            "dotfiles should be hidden after toggle_hidden, got: {names:?}"
        );
        // Toggle back.
        tree.toggle_hidden();
        assert!(
            child_names(&tree).iter().any(|n| n.starts_with('.')),
            "dotfiles should be shown again after second toggle"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn reveal_expands_ancestors_and_returns_row() {
        let root = make_tree();
        // Build a fresh tree (root expanded by default, a_dir is NOT expanded).
        let mut tree = ExplorerTree::new(root.clone());
        let inner = root.join("a_dir").join("inner.txt");
        // inner.txt is two levels deep; reveal should expand a_dir.
        let row = tree.reveal(&inner);
        assert!(
            row.is_some(),
            "reveal should return a row for an existing path"
        );
        let row = row.unwrap();
        assert_eq!(
            tree.nodes[row].path, inner,
            "node at returned row should be inner.txt"
        );
        assert!(
            tree.is_expanded(&root.join("a_dir")),
            "a_dir should be expanded after reveal"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn set_root_re_roots_to_parent() {
        let root = make_tree();
        let mut tree = ExplorerTree::new(root.join("a_dir"));
        // a_dir is the root; set_root to root's parent brings us up.
        tree.set_root(root.clone());
        // Now root should be the root and a_dir visible as a child.
        assert_eq!(tree.root, root);
        let names = child_names(&tree);
        assert!(
            names.contains(&"a_dir".to_string()),
            "a_dir should be a visible child after set_root: {names:?}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn explorer_slot_features_disabled_normal_slot_features_enabled() {
        use crate::keymap_actions::AppAction;

        // Open the app with an unnamed scratch buffer (slot 0 = normal).
        let mut app = super::super::App::new(None, false, None, None).unwrap();

        // Normal slot must have all features on.
        let normal = &app.slots[0];
        assert!(
            normal.features.syntax,
            "normal slot: syntax should be enabled"
        );
        assert!(normal.features.lsp, "normal slot: lsp should be enabled");
        assert!(
            normal.features.hover,
            "normal slot: hover should be enabled"
        );

        // Open the explorer — its slot is pushed after the normal slot.
        app.dispatch_action(AppAction::ToggleExplorer, 1);
        let explorer_idx = app
            .slots
            .iter()
            .position(|s| s.is_explorer)
            .expect("explorer slot must exist after ToggleExplorer");
        let exp = &app.slots[explorer_idx];

        assert!(
            !exp.features.syntax,
            "explorer slot: syntax should be disabled"
        );
        assert!(!exp.features.lsp, "explorer slot: lsp should be disabled");
        assert!(
            !exp.features.hover,
            "explorer slot: hover should be disabled"
        );
    }

    /// Explorer slot must be modifiable (oil.nvim-style editing).
    #[test]
    fn explorer_slot_is_modifiable() {
        use crate::keymap_actions::AppAction;

        let mut app = super::super::App::new(None, false, None, None).unwrap();
        app.dispatch_action(AppAction::ToggleExplorer, 1);
        let explorer_idx = app
            .slots
            .iter()
            .position(|s| s.is_explorer)
            .expect("explorer slot must exist after ToggleExplorer");
        let exp = &app.slots[explorer_idx];
        assert!(
            exp.editor.is_modifiable(),
            "explorer slot must be modifiable (oil.nvim-style editing)"
        );
    }

    /// `render_text` emits `name/` for non-root directory nodes.
    #[test]
    fn render_text_dirs_have_trailing_slash() {
        let root = make_tree();
        let tree = ExplorerTree::new(root.clone());
        let text = tree.render_text();
        let lines: Vec<&str> = text.lines().collect();
        // The root line (depth 0) has no trailing slash.
        let root_name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        assert!(
            lines[0].trim_start().starts_with(&root_name) && !lines[0].trim_start().ends_with('/'),
            "root line must not end with '/': {:?}",
            lines[0]
        );
        // Non-root dir nodes (a_dir, b_dir) must contain trailing '/' before
        // the ID_SEP tail. Strip the id tail first.
        for line in &lines[1..] {
            let trimmed = line.trim_start();
            // Strip id tail (everything from first ID_SEP onward).
            let name_part = if let Some(sep) = trimmed.find(crate::app::explorer_reconcile::ID_SEP)
            {
                &trimmed[..sep]
            } else {
                trimmed
            };
            if name_part.starts_with("a_dir") || name_part.starts_with("b_dir") {
                assert!(
                    name_part.ends_with('/'),
                    "dir line must end with '/' (before id tail): {trimmed:?}"
                );
            }
        }
        let _ = fs::remove_dir_all(&root);
    }

    /// `<C-s>` → ExplorerOpenVsplit; `<C-S-s>` → ExplorerOpenSplit; `<C-v>` is
    /// UNBOUND (falls through to visual-block); `<C-t>` → ExplorerOpenTab;
    /// `gh` → ExplorerToggleHidden; `gi` → ExplorerToggleGitignore.
    #[test]
    fn explorer_keymap_ctrl_open_and_g_toggles() {
        use crate::app::keymap::HjklMode;
        use crate::app::keymap_build::build_explorer_keymap;
        use crate::keymap_actions::AppAction;
        use hjkl_keymap::{KeyEvent as KmKeyEvent, KeyResolve};

        let now = std::time::Instant::now();

        // Feed a sequence of key events into a fresh keymap and return the
        // matched action, if any. A fresh keymap is built per call because
        // Keymap doesn't implement Clone.
        let resolve = |events: &[KmKeyEvent]| -> Option<AppAction> {
            let mut km = build_explorer_keymap(' ');
            let mut result = None;
            for &ev in events {
                match km.feed(HjklMode::Normal, ev, now) {
                    KeyResolve::Match(b) => {
                        result = Some(b.action);
                        break;
                    }
                    KeyResolve::Pending => {}
                    KeyResolve::Ambiguous => {
                        if let KeyResolve::Match(b) = km.timeout_resolve(HjklMode::Normal) {
                            result = Some(b.action);
                        }
                        break;
                    }
                    KeyResolve::Unbound(_) => break,
                }
            }
            result
        };

        assert_eq!(
            resolve(&[KmKeyEvent::ctrl('s')]),
            Some(AppAction::ExplorerOpenVsplit),
            "<C-s> must map to ExplorerOpenVsplit"
        );
        assert_eq!(
            resolve(&[KmKeyEvent::new(
                hjkl_keymap::KeyCode::Char('s'),
                hjkl_keymap::KeyModifiers::CTRL | hjkl_keymap::KeyModifiers::SHIFT,
            )]),
            Some(AppAction::ExplorerOpenSplit),
            "<C-S-s> must map to ExplorerOpenSplit"
        );
        assert_eq!(
            resolve(&[KmKeyEvent::ctrl('v')]),
            None,
            "<C-v> must be unbound in the explorer (falls through to visual-block)"
        );
        assert_eq!(
            resolve(&[KmKeyEvent::ctrl('t')]),
            Some(AppAction::ExplorerOpenTab),
            "<C-t> must map to ExplorerOpenTab"
        );
        assert_eq!(
            resolve(&[KmKeyEvent::char('g'), KmKeyEvent::char('h')]),
            Some(AppAction::ExplorerToggleHidden),
            "gh must map to ExplorerToggleHidden"
        );
        assert_eq!(
            resolve(&[KmKeyEvent::char('g'), KmKeyEvent::char('i')]),
            Some(AppAction::ExplorerToggleGitignore),
            "gi must map to ExplorerToggleGitignore"
        );
    }

    /// `dd` on an OPEN (unfolded) dir deletes the WHOLE subtree — the dir and
    /// its children are trashed together; children must NOT be orphaned to root.
    #[test]
    fn dd_open_dir_deletes_subtree_no_orphans() {
        use crate::app::event_loop::KeyOutcome;
        use crate::keymap_actions::AppAction;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use hjkl_engine::VimMode;
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("d")).unwrap();
        std::fs::write(tmp.path().join("d").join("a.rs"), b"a").unwrap();
        std::fs::write(tmp.path().join("d").join("b.rs"), b"b").unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();
        // The explorer roots at the canonicalized cwd; build expected paths from
        // it (tempdirs are symlinked on macOS/CI, so raw `tmp.path()` mismatches).
        let base = std::env::current_dir().unwrap();

        let mut app = super::super::App::new(None, false, None, None).unwrap();
        app.dispatch_action(AppAction::ToggleExplorer, 1);
        let win_id = app.explorer.as_ref().unwrap().win_id;
        let press = |app: &mut super::super::App, code: KeyCode| {
            let key = KeyEvent::new(code, KeyModifiers::NONE);
            let consumed = matches!(
                app.handle_keypress(key),
                KeyOutcome::Continue | KeyOutcome::Break
            );
            if !consumed {
                if app.active_editor().vim_mode() == VimMode::Insert {
                    app.dispatch_insert_key(key);
                } else {
                    hjkl_vim_tui::handle_key(app.active_editor_mut(), key);
                }
            }
            app.maybe_reconcile_explorer();
        };
        let dir = base.join("d");
        let dir_row = app
            .explorer
            .as_ref()
            .unwrap()
            .tree
            .nodes
            .iter()
            .position(|n| n.path == dir)
            .unwrap();
        if let Some(Some(w)) = app.windows.get_mut(win_id) {
            w.cursor_row = dir_row;
            w.cursor_col = 0;
        }
        app.sync_viewport_to_explorer_editor();
        // OPEN (unfold) the dir via Enter (ExplorerActivate), matching the real
        // keypress path rather than calling the helper directly.
        press(&mut app, KeyCode::Enter);
        // dd on the now-open dir.
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Char('d'));

        let dir_gone = !base.join("d").exists();
        let a_orphan = base.join("a.rs").exists();
        let b_orphan = base.join("b.rs").exists();
        std::env::set_current_dir(prev).unwrap();
        assert!(dir_gone, "the dir must be deleted (trashed)");
        assert!(
            !a_orphan && !b_orphan,
            "children must NOT be orphaned to root (a={a_orphan} b={b_orphan})"
        );
    }

    /// `dd` a dir, then `p` on another dir → the dir moves INTO the target dir
    /// with its contents preserved (full keypress → reconcile → filesystem).
    #[test]
    fn dd_dir_then_p_on_dir_moves_into_target() {
        use crate::app::event_loop::KeyOutcome;
        use crate::keymap_actions::AppAction;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use hjkl_engine::VimMode;
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("mover")).unwrap();
        std::fs::write(tmp.path().join("mover").join("inner.txt"), b"CONTENT").unwrap();
        std::fs::create_dir(tmp.path().join("target")).unwrap();
        std::fs::write(tmp.path().join("target").join("keep.txt"), b"k").unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();
        let base = std::env::current_dir().unwrap();

        let mut app = super::super::App::new(None, false, None, None).unwrap();
        app.dispatch_action(AppAction::ToggleExplorer, 1);
        let win_id = app.explorer.as_ref().unwrap().win_id;
        let press = |app: &mut super::super::App, code: KeyCode| {
            let key = KeyEvent::new(code, KeyModifiers::NONE);
            let consumed = matches!(
                app.handle_keypress(key),
                KeyOutcome::Continue | KeyOutcome::Break
            );
            if !consumed {
                if app.active_editor().vim_mode() == VimMode::Insert {
                    app.dispatch_insert_key(key);
                } else {
                    hjkl_vim_tui::handle_key(app.active_editor_mut(), key);
                }
            }
            app.maybe_reconcile_explorer();
        };
        let row_of = |app: &super::super::App, p: &std::path::Path| -> Option<usize> {
            app.explorer
                .as_ref()
                .unwrap()
                .tree
                .nodes
                .iter()
                .position(|n| n.path == p)
        };
        let set_cursor = |app: &mut super::super::App, row: usize| {
            if let Some(Some(w)) = app.windows.get_mut(win_id) {
                w.cursor_row = row;
                w.cursor_col = 0;
            }
            app.sync_viewport_to_explorer_editor();
        };

        let mover = base.join("mover");
        let mr = row_of(&app, &mover).unwrap();
        set_cursor(&mut app, mr);
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Char('d'));

        let target = base.join("target");
        let tr = row_of(&app, &target).expect("target row after dd");
        set_cursor(&mut app, tr);
        press(&mut app, KeyCode::Char('p'));

        let slot = app.slots.iter().position(|s| s.is_explorer).unwrap();
        let buf = app.slots[slot].editor.buffer().as_string();
        let inside = base.join("target").join("mover").join("inner.txt");
        let at_root = base.join("mover").join("inner.txt");
        let inside_exists = inside.exists();
        let inside_content = std::fs::read(&inside).ok();
        let root_exists = at_root.exists();
        std::env::set_current_dir(prev).unwrap();
        assert!(
            inside_exists && inside_content.as_deref() == Some(b"CONTENT".as_slice()),
            "expected target/mover/inner.txt with CONTENT.\n inside={inside_exists} content={inside_content:?} root_still={root_exists}\n buffer:\n{buf:?}"
        );
    }

    // ── nodes_from_buffer round-trip tests ─────────────────────────────────

    /// `render_text` → `nodes_from_buffer` must reproduce the same
    /// path/depth/is_dir/is_last/branches for a non-trivial tree.
    #[test]
    fn nodes_from_buffer_roundtrip_simple() {
        let root = make_tree();
        // Full-tree walk: depth-2 nodes always present; toggle only changes folds.
        let tree = ExplorerTree::new(root.clone());

        let text = tree.render_text();
        let parsed = nodes_from_buffer(&text, &root);

        assert_eq!(
            tree.nodes.len(),
            parsed.len(),
            "roundtrip must preserve node count; tree.nodes={} parsed={}",
            tree.nodes.len(),
            parsed.len()
        );

        for (i, (orig, got)) in tree.nodes.iter().zip(parsed.iter()).enumerate() {
            assert_eq!(orig.path, got.path, "node[{i}] path mismatch");
            assert_eq!(orig.depth, got.depth, "node[{i}] depth mismatch");
            assert_eq!(orig.is_dir, got.is_dir, "node[{i}] is_dir mismatch");
            assert_eq!(orig.is_last, got.is_last, "node[{i}] is_last mismatch");
            assert_eq!(orig.branches, got.branches, "node[{i}] branches mismatch");
        }

        let _ = fs::remove_dir_all(&root);
    }

    /// Round-trip for a deeper nested tree with mixed last/non-last siblings.
    #[test]
    fn nodes_from_buffer_roundtrip_nested_mixed() {
        // Build: root/{ p/ { a/ { x.rs }, b.rs }, q.rs }
        let base = std::env::temp_dir().join(format!("hjkl_nfb_nested_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("p").join("a")).unwrap();
        fs::write(base.join("p").join("a").join("x.rs"), "").unwrap();
        fs::write(base.join("p").join("b.rs"), "").unwrap();
        fs::write(base.join("q.rs"), "").unwrap();

        // Full-tree walk: all nodes present without needing to toggle.
        let tree = ExplorerTree::new(base.clone());

        let text = tree.render_text();
        let parsed = nodes_from_buffer(&text, &base);

        assert_eq!(tree.nodes.len(), parsed.len(), "node count mismatch");
        for (i, (orig, got)) in tree.nodes.iter().zip(parsed.iter()).enumerate() {
            assert_eq!(orig.path, got.path, "node[{i}] path");
            assert_eq!(orig.depth, got.depth, "node[{i}] depth");
            assert_eq!(orig.is_dir, got.is_dir, "node[{i}] is_dir");
            assert_eq!(orig.is_last, got.is_last, "node[{i}] is_last");
            assert_eq!(orig.branches, got.branches, "node[{i}] branches");
        }

        let _ = fs::remove_dir_all(&base);
    }

    /// Mid-edit alignment: a blank line (e.g. a fresh `o`/`O` open-line before
    /// the name is typed) must become a `None` slot so every real node below it
    /// keeps its buffer-row index — otherwise the render overlay paints icons /
    /// git colors a row off.
    #[test]
    fn overlay_nodes_blank_line_keeps_alignment() {
        let root = Path::new("/r");
        //  row0: root           depth 0
        //  row1: dir/           depth 1
        //  row2: <blank>        (fresh open-line — no name yet)
        //  row3: file.txt       depth 1
        let text = "  r\n    dir/\n      \n    file.txt";
        let ov = overlay_nodes_from_buffer(text, root);

        assert_eq!(ov.len(), 4, "one slot per buffer line");
        assert_eq!(ov[0].as_ref().unwrap().depth, 0, "root depth 0");
        assert!(ov[1].as_ref().unwrap().is_dir, "row1 is the dir");
        assert_eq!(ov[1].as_ref().unwrap().depth, 1);
        assert!(ov[2].is_none(), "blank line → None slot (alignment hole)");
        let f = ov[3].as_ref().expect("row3 present");
        assert!(!f.is_dir, "row3 is the file");
        assert_eq!(f.depth, 1, "file stays at depth 1 below the blank");
        assert_eq!(
            f.path,
            Path::new("/r/file.txt"),
            "path resolved to root child"
        );
    }

    /// Opening a buffer reveals it in the tree WITHOUT collapsing other
    /// expanded dirs, and moves the selection onto the opened file. Regression
    /// for the destructive full-rebuild that snapped search/click-opened folds
    /// shut and landed the cursorline on the wrong row.
    #[test]
    fn reveal_active_keeps_other_folds_open_and_selects_file() {
        use crate::keymap_actions::AppAction;
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("aaa")).unwrap();
        std::fs::write(tmp.path().join("aaa").join("akid.txt"), b"").unwrap();
        std::fs::create_dir(tmp.path().join("bbb")).unwrap();
        std::fs::write(tmp.path().join("bbb").join("target.txt"), b"x").unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let mut app = super::super::App::new(None, false, None, None).unwrap();
        app.dispatch_action(AppAction::ToggleExplorer, 1);

        // Open `aaa/`'s fold directly in the buffer WITHOUT touching `expanded`
        // — this mimics a search reveal (`/`+`n`), the exact case the old
        // rebuild-from-`expanded` path wrongly collapsed on the next buffer
        // open.
        let aaa = std::env::current_dir().unwrap().join("aaa");
        let win_id = app.explorer.as_ref().unwrap().win_id;
        let aaa_row = app
            .explorer
            .as_ref()
            .unwrap()
            .tree
            .nodes
            .iter()
            .position(|n| n.path == aaa)
            .unwrap();
        let slot = app.slots.iter().position(|s| s.is_explorer).unwrap();
        app.slots[slot].editor.buffer_mut().open_fold_at(aaa_row);
        let aaa_open = !app.slots[slot]
            .editor
            .buffer()
            .folds()
            .iter()
            .any(|f| f.start_row == aaa_row && f.closed);
        assert!(aaa_open, "precondition: aaa fold open after activate");

        // Focus the editor window (not the explorer) before opening, so `:edit`
        // doesn't replace the explorer buffer — mirrors the real file-open path.
        app.dispatch_action(AppAction::FocusRight, 1);
        // Open a file under `bbb/` → triggers explorer_reveal_active.
        app.dispatch_ex("edit bbb/target.txt");

        let slot = app.slots.iter().position(|s| s.is_explorer).unwrap();
        let aaa_row2 = app
            .explorer
            .as_ref()
            .unwrap()
            .tree
            .nodes
            .iter()
            .position(|n| n.path == aaa)
            .unwrap();
        let aaa_still_open = !app.slots[slot]
            .editor
            .buffer()
            .folds()
            .iter()
            .any(|f| f.start_row == aaa_row2 && f.closed);

        let target = std::env::current_dir()
            .unwrap()
            .join("bbb")
            .join("target.txt");
        let target_row = app
            .explorer
            .as_ref()
            .unwrap()
            .tree
            .nodes
            .iter()
            .position(|n| n.path == target)
            .unwrap();
        let cur = app
            .windows
            .get(win_id)
            .and_then(|w| w.as_ref())
            .map(|w| w.cursor_row)
            .unwrap();

        std::env::set_current_dir(prev).unwrap();
        assert!(
            aaa_still_open,
            "opening a file must not collapse other expanded dirs"
        );
        assert_eq!(
            cur, target_row,
            "explorer selection must move to the opened file"
        );
    }

    /// Opening a VISIBLE file must not scroll the explorer. With the lazy walk a
    /// collapsed dir's children aren't in the buffer, so the file below it sits
    /// near the top — reveal must leave `top_row` alone.
    #[test]
    fn reveal_active_does_not_scroll_when_file_visible() {
        use crate::app::window::LayoutRect;
        use crate::keymap_actions::AppAction;
        let tmp = tempfile::tempdir().unwrap();
        // A collapsed dir with 30 children sits above a root-level file. The
        // children are NOT loaded (lazy), so target.txt is at a low row.
        std::fs::create_dir(tmp.path().join("aaa")).unwrap();
        for i in 0..30 {
            std::fs::write(tmp.path().join("aaa").join(format!("f{i:02}.txt")), b"").unwrap();
        }
        std::fs::write(tmp.path().join("target.txt"), b"x").unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let mut app = super::super::App::new(None, false, None, None).unwrap();
        app.dispatch_action(AppAction::ToggleExplorer, 1);

        let win_id = app.explorer.as_ref().unwrap().win_id;
        if let Some(Some(w)) = app.windows.get_mut(win_id) {
            w.last_rect = Some(LayoutRect::new(0, 0, 30, 24));
            w.top_row = 0;
        }
        let target = std::env::current_dir().unwrap().join("target.txt");
        let target_row = app
            .explorer
            .as_ref()
            .unwrap()
            .tree
            .nodes
            .iter()
            .position(|n| n.path == target)
            .unwrap();
        assert!(
            target_row < 24,
            "lazy: aaa's children unloaded, so target's row ({target_row}) is on screen"
        );

        // Open target.txt from the editor window → triggers reveal_active.
        app.dispatch_action(AppAction::FocusRight, 1);
        app.dispatch_ex("edit target.txt");

        let top = app
            .windows
            .get(win_id)
            .and_then(|w| w.as_ref())
            .map(|w| w.top_row)
            .unwrap();
        std::env::set_current_dir(prev).unwrap();
        assert_eq!(
            top, 0,
            "explorer must not scroll when the opened file is already visible"
        );
    }

    /// Activating an EMPTY directory must be a no-op — it has no fold of its
    /// own, so it must NOT toggle (and collapse) the enclosing parent fold.
    #[test]
    fn activate_empty_dir_does_not_collapse_parent() {
        use crate::keymap_actions::AppAction;
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("parent").join("empty")).unwrap();
        std::fs::write(tmp.path().join("parent").join("file.txt"), b"").unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let mut app = super::super::App::new(None, false, None, None).unwrap();
        app.dispatch_action(AppAction::ToggleExplorer, 1);
        let win_id = app.explorer.as_ref().unwrap().win_id;

        let row_of = |app: &super::super::App, p: &std::path::Path| -> usize {
            app.explorer
                .as_ref()
                .unwrap()
                .tree
                .nodes
                .iter()
                .position(|n| n.path == p)
                .unwrap()
        };
        let fold_open_at = |app: &super::super::App, row: usize| -> bool {
            let slot = app.slots.iter().position(|s| s.is_explorer).unwrap();
            !app.slots[slot]
                .editor
                .buffer()
                .folds()
                .iter()
                .any(|f| f.start_row == row && f.closed)
        };
        let activate = |app: &mut super::super::App, row: usize| {
            if let Some(Some(w)) = app.windows.get_mut(win_id) {
                w.cursor_row = row;
                w.cursor_col = 0;
            }
            app.sync_viewport_to_explorer_editor();
            app.explorer_activate();
        };

        let parent = std::env::current_dir().unwrap().join("parent");
        let empty = parent.join("empty");

        // Expand `parent/` so its children (incl. the empty dir) are visible.
        let pr = row_of(&app, &parent);
        activate(&mut app, pr);
        let parent_row = row_of(&app, &parent);
        assert!(
            fold_open_at(&app, parent_row),
            "parent fold open after expand"
        );

        // Activate the EMPTY dir — must be a no-op, parent stays open.
        let er = row_of(&app, &empty);
        activate(&mut app, er);
        let parent_row2 = row_of(&app, &parent);
        std::env::set_current_dir(prev).unwrap();
        assert!(
            fold_open_at(&app, parent_row2),
            "activating an empty dir must not collapse the parent fold"
        );
    }

    /// `:debug` toggles the global `debug_mode` flag; `:debug on` / `:debug off`
    /// set it explicitly.
    #[test]
    fn debug_ex_command_toggles_debug_mode() {
        let mut app = super::super::App::new(None, false, None, None).unwrap();
        assert!(!app.debug_mode, "default off");
        app.dispatch_ex("debug");
        assert!(app.debug_mode, ":debug toggles on");
        app.dispatch_ex("debug");
        assert!(!app.debug_mode, ":debug toggles back off");
        app.dispatch_ex("debug on");
        assert!(app.debug_mode, ":debug on forces on");
        app.dispatch_ex("debug on");
        assert!(app.debug_mode, ":debug on is idempotent");
        app.dispatch_ex("debug off");
        assert!(!app.debug_mode, ":debug off forces off");
    }

    /// `o<Esc>` (open-line, no name typed) must not leave a stray blank line in
    /// the tree — the no-op reconcile has to normalize the buffer back to the
    /// canonical render.
    #[test]
    fn o_then_esc_leaves_no_blank_line() {
        use crate::app::event_loop::KeyOutcome;
        use crate::keymap_actions::AppAction;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use hjkl_engine::VimMode;
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.txt"), b"").unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let mut app = super::super::App::new(None, false, None, None).unwrap();
        app.dispatch_action(AppAction::ToggleExplorer, 1);
        let press = |app: &mut super::super::App, code: KeyCode| {
            let key = KeyEvent::new(code, KeyModifiers::NONE);
            let consumed = matches!(
                app.handle_keypress(key),
                KeyOutcome::Continue | KeyOutcome::Break
            );
            if !consumed {
                if app.active_editor().vim_mode() == VimMode::Insert {
                    app.dispatch_insert_key(key);
                } else {
                    hjkl_vim_tui::handle_key(app.active_editor_mut(), key);
                }
            }
            app.maybe_reconcile_explorer();
        };
        press(&mut app, KeyCode::Char('o'));
        press(&mut app, KeyCode::Esc);

        let slot = app.slots.iter().position(|s| s.is_explorer).unwrap();
        let text = app.slots[slot].editor.buffer().as_string();
        std::env::set_current_dir(prev).unwrap();
        assert!(
            !text.split('\n').any(|l| l.trim().is_empty()),
            "o<Esc> must not leave a blank line; buffer was {text:?}"
        );
    }

    /// A fold toggle while the explorer is UNFOCUSED must keep its per-window
    /// `window_folds` snapshot in lockstep with the buffer — otherwise the
    /// unfocused-render path (which reads the snapshot) and the glyph overlay
    /// (which reads the buffer) disagree and the tree renders garbled.
    #[test]
    fn explorer_fold_toggle_keeps_window_folds_in_sync() {
        use crate::keymap_actions::AppAction;
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("sub")).unwrap();
        std::fs::write(tmp.path().join("sub").join("k.txt"), b"").unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let mut app = super::super::App::new(None, false, None, None).unwrap();
        app.dispatch_action(AppAction::ToggleExplorer, 1);
        let win_id = app.explorer.as_ref().unwrap().win_id;
        // Focus away → populates window_folds[explorer] (the snapshot the
        // unfocused render path uses).
        app.dispatch_action(AppAction::FocusRight, 1);
        assert!(
            app.window_folds.contains_key(&win_id),
            "precondition: per-window fold snapshot present"
        );

        // Toggle `sub/`'s fold via the activate path (what a mouse click runs).
        let sub = std::env::current_dir().unwrap().join("sub");
        let sub_row = app
            .explorer
            .as_ref()
            .unwrap()
            .tree
            .nodes
            .iter()
            .position(|n| n.path == sub)
            .unwrap();
        if let Some(Some(w)) = app.windows.get_mut(win_id) {
            w.cursor_row = sub_row;
            w.cursor_col = 0;
        }
        app.sync_viewport_to_explorer_editor();
        app.explorer_activate();

        let slot = app.slots.iter().position(|s| s.is_explorer).unwrap();
        let buf_folds = app.slots[slot].editor.buffer().folds();
        let snap = app.window_folds.get(&win_id).cloned().unwrap();
        std::env::set_current_dir(prev).unwrap();
        assert_eq!(
            snap, buf_folds,
            "window_folds snapshot must match buffer folds after a fold toggle"
        );
    }

    // ── Shared helpers ─────────────────────────────────────────────────────

    /// Returns `true` when any DIRECT CHILD (depth 1) of the explorer root has
    /// the given filename. Use this instead of `buf.contains(name)` to avoid
    /// false positives from nested paths (e.g. trash/ subdir containing the name).
    fn has_root_child(app: &super::super::App, name: &str) -> bool {
        app.explorer
            .as_ref()
            .map(|ep| {
                ep.tree
                    .nodes
                    .iter()
                    .any(|n| n.depth == 1 && n.path.file_name().map(|f| f == name).unwrap_or(false))
            })
            .unwrap_or(false)
    }

    /// Press a key through the full event loop path (same as existing tests).
    fn press(app: &mut super::super::App, code: crossterm::event::KeyCode) {
        use crate::app::event_loop::KeyOutcome;
        use crossterm::event::{KeyEvent, KeyModifiers};
        use hjkl_engine::VimMode;
        let key = KeyEvent::new(code, KeyModifiers::NONE);
        let consumed = matches!(
            app.handle_keypress(key),
            KeyOutcome::Continue | KeyOutcome::Break
        );
        if !consumed {
            if app.active_editor().vim_mode() == VimMode::Insert {
                app.dispatch_insert_key(key);
            } else {
                hjkl_vim_tui::handle_key(app.active_editor_mut(), key);
            }
        }
        app.maybe_reconcile_explorer();
    }

    // ── Undo / redo filesystem op tests ────────────────────────────────────

    /// `dd` trashes a file; `u` undoes the buffer edit → reconcile restores
    /// the file from trash and the line reappears in the explorer.
    #[test]
    fn ddu_restores_file() {
        use crate::keymap_actions::AppAction;
        use crossterm::event::KeyCode;

        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("victim.txt"), "content").unwrap();
        // Isolate the trash directory so the restore doesn't pick up stale
        // entries from parallel tests.
        unsafe { std::env::set_var("XDG_CACHE_HOME", tmp.path()) };
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let mut app = super::super::App::new(None, false, None, None).unwrap();
        app.dispatch_action(AppAction::ToggleExplorer, 1);

        // j → move onto victim.txt; dd → delete the line.
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Char('d'));

        let on_disk_after_dd = tmp.path().join("victim.txt").exists();
        let idx = app.explorer_slot_idx().unwrap();
        let buf_after_dd = app.slots[idx].editor.buffer().as_string();
        let root_child_after_dd = has_root_child(&app, "victim.txt");

        // u → undo the buffer deletion; reconcile should restore the file.
        press(&mut app, KeyCode::Char('u'));

        let on_disk_after_u = tmp.path().join("victim.txt").exists();
        let buf_after_u = app.slots[idx].editor.buffer().as_string();
        let root_child_after_u = has_root_child(&app, "victim.txt");

        std::env::set_current_dir(prev).unwrap();

        assert!(!on_disk_after_dd, "dd must trash victim.txt");
        assert!(
            !root_child_after_dd,
            "dd must drop victim.txt from root-level nodes; buf=<<<{buf_after_dd}>>>"
        );
        assert!(
            on_disk_after_u,
            "u must restore victim.txt from trash; buf_after_u=<<<{buf_after_u}>>>"
        );
        assert!(
            root_child_after_u,
            "u must restore victim.txt to root-level nodes; buf=<<<{buf_after_u}>>>"
        );
    }

    /// `o` + name + `<Esc>` creates a file; `u` removes it again.
    #[test]
    fn create_then_undo_removes_file() {
        use crate::keymap_actions::AppAction;
        use crossterm::event::KeyCode;

        let tmp = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CACHE_HOME", tmp.path()) };
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let mut app = super::super::App::new(None, false, None, None).unwrap();
        app.dispatch_action(AppAction::ToggleExplorer, 1);

        // o → insert line below; type "fresh.rs"; Esc → reconcile.
        press(&mut app, KeyCode::Char('o'));
        for c in "fresh.rs".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Esc);

        let on_disk_after_create = tmp.path().join("fresh.rs").exists();
        let idx = app.explorer_slot_idx().unwrap();
        let buf_after_create = app.slots[idx].editor.buffer().as_string();
        let root_child_after_create = has_root_child(&app, "fresh.rs");

        // Creating a new file opens it in the main window (focus moves there).
        // Refocus the explorer so `u` operates on the explorer buffer.
        let ep_win = app.explorer.as_ref().unwrap().win_id;
        app.set_focused_window(ep_win);
        app.sync_viewport_to_editor();

        // u → undo the insert; reconcile should trash fresh.rs.
        press(&mut app, KeyCode::Char('u'));

        let on_disk_after_u = tmp.path().join("fresh.rs").exists();
        let buf_after_u = app.slots[idx].editor.buffer().as_string();
        let root_child_after_u = has_root_child(&app, "fresh.rs");

        std::env::set_current_dir(prev).unwrap();

        assert!(
            on_disk_after_create,
            "o+type+Esc must create fresh.rs on disk"
        );
        assert!(
            root_child_after_create,
            "created file must appear in root nodes; buf=<<<{buf_after_create}>>>"
        );
        assert!(
            !on_disk_after_u,
            "u must remove fresh.rs from disk (trash it); buf_after_u=<<<{buf_after_u}>>>"
        );
        assert!(
            !root_child_after_u,
            "u must drop fresh.rs from root nodes; buf=<<<{buf_after_u}>>>"
        );
    }

    // ── Git-aware reconcile tests ───────────────────────────────────────────

    /// `dd` on a git-tracked file: the file is gone from disk but stays in the
    /// explorer list as a red (Deleted) node because it was tracked by git.
    ///
    /// Windows-gated: git2's `workdir` canonicalizes the temp path (UNC `\\?\`)
    /// while the explorer keys off `current_dir()`, so the status-map join never
    /// matches on Windows — a CI-only path-form mismatch, not a logic bug.
    #[cfg(not(target_os = "windows"))]
    #[test]
    fn git_repo_dd_tracked_stays_red() {
        use crate::keymap_actions::AppAction;
        use crossterm::event::KeyCode;

        // Create an isolated git repo with one committed file.
        let tmp = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CACHE_HOME", tmp.path()) };

        // Init a git repo, create + commit a tracked file.
        let run = |args: &[&str], dir: &std::path::Path| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .expect("git command failed")
        };
        run(&["init"], tmp.path());
        run(&["config", "user.email", "t@t.com"], tmp.path());
        run(&["config", "user.name", "T"], tmp.path());
        std::fs::write(tmp.path().join("tracked.txt"), "tracked").unwrap();
        run(&["add", "tracked.txt"], tmp.path());
        run(&["commit", "-m", "init"], tmp.path());

        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let mut app = super::super::App::new(None, false, None, None).unwrap();
        app.dispatch_action(AppAction::ToggleExplorer, 1);

        // Navigate to tracked.txt and delete it (dd).
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Char('d'));

        let on_disk = tmp.path().join("tracked.txt").exists();
        let idx = app.explorer_slot_idx().unwrap();
        let buf = app.slots[idx].editor.buffer().as_string();

        // Check that the node has Deleted git status.
        let node_git = app
            .explorer
            .as_ref()
            .and_then(|ep| {
                ep.tree.nodes.iter().find(|n| {
                    n.path
                        .file_name()
                        .map(|f| f == "tracked.txt")
                        .unwrap_or(false)
                })
            })
            .map(|n| n.git);

        std::env::set_current_dir(prev).unwrap();

        assert!(!on_disk, "dd must remove tracked.txt from disk (to trash)");
        assert!(
            buf.contains("tracked.txt"),
            "tracked deleted file must remain in explorer list; buf=<<<{buf}>>>"
        );
        assert!(
            matches!(node_git, Some(Some(hjkl_app::git::ExplorerGit::Deleted))),
            "deleted tracked file node must have git status Deleted; got {node_git:?}"
        );
    }

    /// `dd` on a file in a non-git directory: file is gone AND gone from list.
    #[test]
    fn non_git_dd_vanishes() {
        use crate::keymap_actions::AppAction;
        use crossterm::event::KeyCode;

        // Temp dir that is NOT a git repo.
        let tmp = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CACHE_HOME", tmp.path()) };
        std::fs::write(tmp.path().join("untracked.txt"), "bye").unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let mut app = super::super::App::new(None, false, None, None).unwrap();
        app.dispatch_action(AppAction::ToggleExplorer, 1);

        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Char('d'));

        let on_disk = tmp.path().join("untracked.txt").exists();
        let idx = app.explorer_slot_idx().unwrap();
        let buf = app.slots[idx].editor.buffer().as_string();

        std::env::set_current_dir(prev).unwrap();

        assert!(!on_disk, "dd must remove untracked.txt from disk");
        assert!(
            !has_root_child(&app, "untracked.txt"),
            "non-git dd'd file must vanish from list; buf=<<<{buf}>>>"
        );
    }

    /// `dd` → `u` (journal undo) → file back on disk + listed normally.
    /// After undo: undo_stack empty, redo_stack has one entry.
    #[test]
    fn dd_then_u_restores() {
        use crate::keymap_actions::AppAction;
        use crossterm::event::KeyCode;

        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("recover.txt"), "restore_me").unwrap();
        unsafe { std::env::set_var("XDG_CACHE_HOME", tmp.path()) };
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let mut app = super::super::App::new(None, false, None, None).unwrap();
        app.dispatch_action(AppAction::ToggleExplorer, 1);

        // dd
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Char('d'));
        assert!(
            !tmp.path().join("recover.txt").exists(),
            "must be trashed after dd"
        );
        // undo_stack has 1 entry after dd.
        let undo_len = app
            .explorer
            .as_ref()
            .map(|ep| ep.undo_stack.len())
            .unwrap_or(0);
        assert_eq!(undo_len, 1, "undo_stack must have 1 entry after dd");

        // u → journal undo
        press(&mut app, KeyCode::Char('u'));
        let on_disk = tmp.path().join("recover.txt").exists();
        let idx = app.explorer_slot_idx().unwrap();
        let buf = app.slots[idx].editor.buffer().as_string();
        let undo_len2 = app
            .explorer
            .as_ref()
            .map(|ep| ep.undo_stack.len())
            .unwrap_or(1);
        let redo_len = app
            .explorer
            .as_ref()
            .map(|ep| ep.redo_stack.len())
            .unwrap_or(0);

        std::env::set_current_dir(prev).unwrap();

        assert!(on_disk, "u must restore recover.txt to disk");
        assert!(
            buf.contains("recover.txt"),
            "u must show recover.txt in list; buf=<<<{buf}>>>"
        );
        assert_eq!(undo_len2, 0, "undo_stack must be empty after u");
        assert_eq!(redo_len, 1, "redo_stack must have 1 entry after u");
    }

    /// `dd` → `u` → `<C-r>` → file trashed again.
    #[test]
    fn dd_u_then_ctrl_r_retrashes() {
        use crate::app::event_loop::KeyOutcome;
        use crate::keymap_actions::AppAction;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("trashable.txt"), "yo").unwrap();
        unsafe { std::env::set_var("XDG_CACHE_HOME", tmp.path()) };
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let mut app = super::super::App::new(None, false, None, None).unwrap();
        app.dispatch_action(AppAction::ToggleExplorer, 1);

        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Char('d'));
        assert!(!tmp.path().join("trashable.txt").exists(), "dd must trash");

        press(&mut app, KeyCode::Char('u'));
        assert!(tmp.path().join("trashable.txt").exists(), "u must restore");

        // <C-r> redo
        let ctrl_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
        let consumed = matches!(
            app.handle_keypress(ctrl_r),
            KeyOutcome::Continue | KeyOutcome::Break
        );
        if !consumed {
            hjkl_vim_tui::handle_key(app.active_editor_mut(), ctrl_r);
        }
        app.maybe_reconcile_explorer();

        let on_disk = tmp.path().join("trashable.txt").exists();
        let idx = app.explorer_slot_idx().unwrap();
        let buf = app.slots[idx].editor.buffer().as_string();

        std::env::set_current_dir(prev).unwrap();

        assert!(!on_disk, "<C-r> must re-trash trashable.txt");
        assert!(
            !has_root_child(&app, "trashable.txt"),
            "<C-r> must drop trashable.txt from root nodes; buf=<<<{buf}>>>"
        );
    }

    /// `o` + name + `<Esc>` creates; `u` trashes the new file.
    #[test]
    fn create_then_u_removes() {
        use crate::keymap_actions::AppAction;
        use crossterm::event::KeyCode;

        let tmp = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CACHE_HOME", tmp.path()) };
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let mut app = super::super::App::new(None, false, None, None).unwrap();
        app.dispatch_action(AppAction::ToggleExplorer, 1);

        press(&mut app, KeyCode::Char('o'));
        for c in "newborn.rs".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Esc);

        assert!(
            tmp.path().join("newborn.rs").exists(),
            "o+type+Esc must create the file"
        );

        // Refocus explorer after file creation opened it in the editor window.
        let ep_win = app.explorer.as_ref().unwrap().win_id;
        app.set_focused_window(ep_win);
        app.sync_viewport_to_editor();

        press(&mut app, KeyCode::Char('u'));

        let on_disk = tmp.path().join("newborn.rs").exists();
        let idx = app.explorer_slot_idx().unwrap();
        let buf = app.slots[idx].editor.buffer().as_string();

        std::env::set_current_dir(prev).unwrap();

        assert!(!on_disk, "u must remove newborn.rs (trash it)");
        assert!(
            !has_root_child(&app, "newborn.rs"),
            "u must drop newborn.rs from root nodes; buf=<<<{buf}>>>"
        );
    }

    /// `dd` → `u` (restored) → `<C-r>` re-trashes the file.
    #[test]
    fn ddu_then_redo_retrashes() {
        use crate::app::event_loop::KeyOutcome;
        use crate::keymap_actions::AppAction;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("victim.txt"), "data").unwrap();
        unsafe { std::env::set_var("XDG_CACHE_HOME", tmp.path()) };
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let mut app = super::super::App::new(None, false, None, None).unwrap();
        app.dispatch_action(AppAction::ToggleExplorer, 1);

        // j, dd → trash.
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Char('d'));
        assert!(!tmp.path().join("victim.txt").exists(), "dd must trash");

        // u → restore.
        press(&mut app, KeyCode::Char('u'));
        assert!(tmp.path().join("victim.txt").exists(), "u must restore");

        // <C-r> → redo → re-trash.
        // Ctrl+r is handled by hjkl_vim_tui::handle_key (not handle_keypress).
        let ctrl_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
        let consumed = matches!(
            app.handle_keypress(ctrl_r),
            KeyOutcome::Continue | KeyOutcome::Break
        );
        if !consumed {
            hjkl_vim_tui::handle_key(app.active_editor_mut(), ctrl_r);
        }
        app.maybe_reconcile_explorer();

        let on_disk = tmp.path().join("victim.txt").exists();
        let idx = app.explorer_slot_idx().unwrap();
        let buf = app.slots[idx].editor.buffer().as_string();

        std::env::set_current_dir(prev).unwrap();

        assert!(!on_disk, "<C-r> must re-trash victim.txt");
        assert!(
            !has_root_child(&app, "victim.txt"),
            "<C-r> must remove victim.txt from root nodes; buf=<<<{buf}>>>"
        );
    }

    // ── Lazy-walk tests ─────────────────────────────────────────────────────────

    /// Lazy walk: a collapsed dir's children are NOT in `nodes`; expanding adds
    /// them, collapsing removes them.
    #[test]
    fn lazy_collapsed_dir_children_absent() {
        let root = make_tree();
        let mut tree = ExplorerTree::new(root.clone());
        let a_dir_path = tree.nodes[1].path.clone();
        assert!(!tree.is_expanded(&a_dir_path), "a_dir must start collapsed");
        let has_inner = |t: &ExplorerTree| {
            t.nodes.iter().any(|n| {
                n.path
                    .file_name()
                    .map(|f| f == "inner.txt")
                    .unwrap_or(false)
            })
        };
        assert!(
            !has_inner(&tree),
            "inner.txt must be absent while a_dir is collapsed (lazy walk)"
        );
        tree.set_expanded(&a_dir_path, true);
        tree.rebuild();
        assert!(
            has_inner(&tree),
            "inner.txt must appear once a_dir is expanded"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// The lazy explorer emits no folds (the node list is the visible tree).
    #[test]
    fn compute_folds_is_empty() {
        let root = make_tree();
        let mut tree = ExplorerTree::new(root.clone());
        assert!(tree.compute_folds().is_empty(), "no folds when collapsed");
        let a_dir_path = tree.nodes[1].path.clone();
        tree.set_expanded(&a_dir_path, true);
        tree.rebuild();
        assert!(
            tree.compute_folds().is_empty(),
            "no folds when expanded either"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// `explorer_activate` on a dir expands it (adds its children to the tree)
    /// and collapses it again on a second activation (removes them).
    #[test]
    fn activate_dir_expands_and_collapses_tree() {
        use crate::keymap_actions::AppAction;
        use crossterm::event::KeyCode;

        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("subdir")).unwrap();
        fs::write(tmp.path().join("subdir").join("child.txt"), "x").unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let mut app = super::super::App::new(None, false, None, None).unwrap();
        app.dispatch_action(AppAction::ToggleExplorer, 1);

        let nodes_before = app.explorer.as_ref().unwrap().tree.nodes.len();

        // Move to subdir (row 1) and activate → expands, child.txt joins the tree.
        press(&mut app, KeyCode::Char('j'));
        app.explorer_activate();

        let nodes_open = app.explorer.as_ref().unwrap().tree.nodes.len();
        assert!(
            nodes_open > nodes_before,
            "activate on a collapsed dir must add its children"
        );
        let has_child = app.explorer.as_ref().unwrap().tree.nodes.iter().any(|n| {
            n.path
                .file_name()
                .map(|f| f == "child.txt")
                .unwrap_or(false)
        });
        assert!(
            has_child,
            "child.txt must be in the tree after expanding subdir"
        );

        // Activate again → collapses, child.txt drops out.
        app.explorer_activate();
        let nodes_closed = app.explorer.as_ref().unwrap().tree.nodes.len();
        assert_eq!(
            nodes_closed, nodes_before,
            "activate again must collapse back to the prior node count"
        );

        std::env::set_current_dir(prev).unwrap();
    }
}
