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
use std::thread;
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender};

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
    /// Directories the user has expanded (absolute paths).
    expanded: HashSet<PathBuf>,
    /// Flattened depth-first list of currently visible rows.
    /// Indexed 1:1 with buffer lines after `render_text`.
    pub(crate) nodes: Vec<ExplorerNode>,
    /// When `false`, entries whose name starts with `.` are hidden. Defaults to
    /// `true` (dotfiles shown). `H` toggles this. The `.git` dir is always
    /// skipped regardless.
    pub(crate) show_hidden: bool,
    /// When `true` (default), entries matched by the repo's git ignore rules
    /// (`.gitignore`, `.git/info/exclude`, core.excludesfile) are hidden — this
    /// also prunes ignored dirs (e.g. `target/`, `node_modules/`) from the fuzzy
    /// search walk, keeping it fast. `I` toggles this. No effect outside a repo.
    pub(crate) respect_gitignore: bool,
    /// Active fuzzy filter query. `None` = unfiltered (normal expansion mode).
    /// When `Some`, `rebuild` performs a full recursive walk and only shows
    /// files that fuzzy-match the query, plus their ancestor dirs.
    pub(crate) filter: Option<String>,
    /// Number of files that matched the last filtered rebuild. 0 when unfiltered.
    pub(crate) match_count: usize,
    /// Total number of files visited during the last filtered rebuild. 0 when
    /// unfiltered.
    pub(crate) total_count: usize,
    /// Row (index into `nodes`) of the highest-scoring matched file after the
    /// last filtered rebuild, so the search can focus the BEST match rather than
    /// the first in tree order. `None` when unfiltered or no match.
    pub(crate) best_match_row: Option<usize>,
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
            filter: None,
            match_count: 0,
            total_count: 0,
            best_match_row: None,
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
            if is_dir && self.expanded.contains(&path) {
                let mut child_prefix = prefix.to_vec();
                child_prefix.push(!is_last);
                self.push_children(&path, depth + 1, &child_prefix, out, repo, status);
            }
        }
    }

    /// Rebuild the flattened node list from the current expansion state.
    ///
    /// When `self.filter` is `Some(q)`, performs a full bounded recursive walk
    /// of the filesystem under `root`, scores each file with `hjkl_fuzzy::score`,
    /// and builds a force-expanded filtered view. Otherwise falls back to the
    /// lazy expansion-set walk.
    pub(crate) fn rebuild(&mut self) {
        if let Some(ref q) = self.filter.clone() {
            self.rebuild_filtered(q);
        } else {
            self.rebuild_unfiltered();
        }
    }

    fn rebuild_unfiltered(&mut self) {
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
        if self.expanded.contains(&root) {
            let repo = self.open_repo();
            self.push_children(&root, 1, &[], &mut out, repo.as_ref(), &status);
        }
        roll_up_dir_status(&mut out);
        self.nodes = out;
        self.match_count = 0;
        self.total_count = 0;
        self.best_match_row = None;
    }

    /// Full recursive fs walk under `root`, filtered to fuzzy-matching files.
    /// Cap total entries visited at 20_000 to bound scan time on huge trees.
    fn rebuild_filtered(&mut self, query: &str) {
        // Walk the entire tree under root, collecting matching files.
        const CAP: usize = 20_000;
        let root = self.root.clone();
        let root_str = root.to_string_lossy().to_string();
        let root_str_len = root_str.len();

        // Open the repo once for the whole walk so ignored dirs (target/,
        // node_modules/, …) are pruned and never descended into.
        let repo = self.open_repo();

        let mut visited: usize = 0;
        let mut truncated = false;
        // file_path → score (only files that matched)
        let mut scored: HashMap<PathBuf, i64> = HashMap::new();
        let mut total: usize = 0;

        // Iterative DFS using a stack of dirs to visit.
        let mut dir_stack: Vec<PathBuf> = vec![root.clone()];
        while let Some(dir) = dir_stack.pop() {
            if visited >= CAP {
                truncated = true;
                break;
            }
            let children = self.read_children(&dir, repo.as_ref());
            for (path, is_dir) in children {
                visited += 1;
                if visited > CAP {
                    truncated = true;
                    break;
                }
                if is_dir {
                    dir_stack.push(path);
                } else {
                    total += 1;
                    // Score relative path so matches feel project-relative.
                    let full = path.to_string_lossy();
                    let rel = if full.starts_with(&root_str) {
                        let stripped = &full[root_str_len..];
                        stripped.trim_start_matches(std::path::MAIN_SEPARATOR)
                    } else {
                        full.as_ref()
                    };
                    if let Some((s, _)) = hjkl_fuzzy::score(rel, query) {
                        scored.insert(path, s);
                    }
                }
            }
        }

        if truncated {
            tracing::debug!(
                cap = CAP,
                "explorer filter walk capped at {CAP} entries; tree may be incomplete"
            );
        }

        self.match_count = scored.len();
        self.total_count = total;

        // Build the ancestor set: every dir from a matched file up to root.
        let mut show: HashSet<PathBuf> = HashSet::new();
        show.insert(root.clone());
        for path in scored.keys() {
            let mut cur = path.parent();
            while let Some(p) = cur {
                show.insert(p.to_path_buf());
                if p == root {
                    break;
                }
                cur = p.parent();
            }
        }
        // Also insert matched files themselves into show.
        for path in scored.keys() {
            show.insert(path.clone());
        }

        // Build the git status map once for the whole walk (empty outside a repo).
        let status = hjkl_app::git::explorer_status_map(&self.root);
        self.repo_present = git2::Repository::discover(&self.root).is_ok();
        self.git_base = status.clone();

        // DFS to build nodes — force-expanded, include only `show` members.
        let mut out = Vec::new();
        out.push(ExplorerNode {
            path: root.clone(),
            depth: 0,
            is_dir: true,
            is_last: true,
            branches: Vec::new(),
            git: None,
        });
        self.push_children_filtered(&root, 1, &[], &show, &mut out, repo.as_ref(), &status);
        self.nodes = out;

        // Focus the highest-scoring match (not the first in tree order). Pick
        // the matched file with the max score, then its row in `nodes`.
        self.best_match_row = scored
            .iter()
            .max_by_key(|(_, s)| **s)
            .map(|(p, _)| p.clone())
            .and_then(|best| self.nodes.iter().position(|n| n.path == best));
    }

    /// Recursive helper for filtered rebuild — mirrors `push_children` but
    /// limits children to those in `show` and always recurses into dirs
    /// (force-expanded). Deleted-file injection is skipped in the filtered
    /// path (search results show only matched on-disk files).
    #[allow(clippy::too_many_arguments)]
    fn push_children_filtered(
        &self,
        dir: &Path,
        depth: usize,
        prefix: &[bool],
        show: &HashSet<PathBuf>,
        out: &mut Vec<ExplorerNode>,
        repo: Option<&git2::Repository>,
        status: &HashMap<PathBuf, hjkl_app::git::ExplorerGit>,
    ) {
        let children = self.read_children(dir, repo);
        // Keep only children that are in the show set.
        let visible: Vec<(PathBuf, bool)> = children
            .into_iter()
            .filter(|(p, _)| show.contains(p))
            .collect();
        let n = visible.len();
        for (i, (path, is_dir)) in visible.into_iter().enumerate() {
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
            if is_dir {
                let mut child_prefix = prefix.to_vec();
                child_prefix.push(!is_last);
                self.push_children_filtered(
                    &path,
                    depth + 1,
                    &child_prefix,
                    show,
                    out,
                    repo,
                    status,
                );
            }
        }
    }

    /// Apply a fuzzy filter query. Empty string → clears the filter.
    /// Triggers a `rebuild`.
    pub(crate) fn apply_filter(&mut self, query: &str) {
        let q = query.trim();
        if q.is_empty() {
            self.filter = None;
        } else {
            self.filter = Some(q.to_string());
        }
        self.rebuild();
    }

    /// Clear the active filter and rebuild.
    pub(crate) fn clear_filter(&mut self) {
        self.filter = None;
        self.rebuild();
    }

    /// Build a filtered tree for a worker thread search. Constructs the struct
    /// directly (no unfiltered `new()` rebuild), sets `filter = Some(query)`,
    /// and calls `rebuild()` to run the filtered walk. The `git2::Repository`
    /// opened inside `rebuild()` stays on the calling (worker) thread — never
    /// send a `Repository` across a channel.
    pub(crate) fn for_search(
        root: PathBuf,
        show_hidden: bool,
        respect_gitignore: bool,
        query: String,
    ) -> Self {
        // Build the struct directly without running the unfiltered walk.
        let mut expanded = HashSet::new();
        expanded.insert(root.clone());
        let mut tree = Self {
            root,
            expanded,
            nodes: Vec::new(),
            show_hidden,
            respect_gitignore,
            filter: Some(query),
            match_count: 0,
            total_count: 0,
            best_match_row: None,
            git_base: HashMap::new(),
            repo_present: false,
        };
        tree.rebuild();
        tree
    }

    /// Install a worker result onto the tree without running a filesystem walk.
    /// Called from the main thread when a worker result arrives.
    pub(crate) fn apply_search_result(
        &mut self,
        query: String,
        nodes: Vec<ExplorerNode>,
        match_count: usize,
        total_count: usize,
        best_match_row: Option<usize>,
    ) {
        self.filter = Some(query);
        self.nodes = nodes;
        self.match_count = match_count;
        self.total_count = total_count;
        self.best_match_row = best_match_row;
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

    /// Toggle the expansion of the directory at `path`. Returns `true` if the
    /// tree changed (caller should rebuild + set_content the buffer).
    pub(crate) fn toggle(&mut self, path: &Path) -> bool {
        if self.expanded.contains(path) {
            self.expanded.remove(path);
        } else {
            self.expanded.insert(path.to_path_buf());
        }
        self.rebuild();
        true
    }

    pub(crate) fn is_expanded(&self, path: &Path) -> bool {
        self.expanded.contains(path)
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

    /// Expand all ancestor dirs of `path` and return its row in `self.nodes`,
    /// or `None` if `path` is not under the root.
    ///
    /// Robust to path-form differences (relative vs absolute, symlinked cwd):
    /// the path is reduced to components relative to the root — trying a plain
    /// `strip_prefix` first, then a canonicalized one — and the node path is
    /// reconstructed as `root.join(rel)` so it matches how the tree builds node
    /// paths (`read_dir` → `dir.join(name)`).
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

        // Reconstruct the node path from the root + relative components, and
        // expand every ancestor directory down to (and including) the root.
        let mut target = self.root.clone();
        for comp in rel.components() {
            target = target.join(comp);
        }
        self.expanded.insert(self.root.clone());
        let mut anc = target.parent();
        while let Some(p) = anc {
            self.expanded.insert(p.to_path_buf());
            if p == self.root {
                break;
            }
            anc = p.parent();
        }
        self.rebuild();
        self.nodes.iter().position(|n| n.path == target)
    }

    /// Build the buffer text and line→node map for the current tree state.
    ///
    /// Each line in the returned `String` corresponds to `nodes[i]`, so
    /// `cursor_row` in the editor maps directly to `nodes[cursor_row]`.
    ///
    /// The buffer contains **only** indentation spaces + the bare name — no
    /// tree-guide glyphs, no connector, no icon. All glyphs are painted as a
    /// render overlay in `render.rs` (same approach as the git-color overlay)
    /// so that the buffer text stays clean for future oil.nvim-style editing.
    ///
    /// Column layout (identical to what was emitted before; the overlay paints
    /// the leading cells):
    ///   depth 0  : `"  " + name`  (2-space indent for the icon+space slot)
    ///   depth ≥ 1: `" ".repeat(depth*2 + 2) + name`
    pub(crate) fn render_text(&self) -> String {
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
}

// ── Explorer search worker ─────────────────────────────────────────────────────

/// Job submitted to the explorer search worker.
pub(crate) struct ExplorerSearchJob {
    /// Monotonic generation counter — used by the main thread to discard stale
    /// results (only results whose `generation == explorer_search_gen` are applied).
    pub generation: u64,
    pub root: PathBuf,
    pub query: String,
    pub show_hidden: bool,
    pub respect_gitignore: bool,
}

/// Result produced by the explorer search worker.
pub(crate) struct ExplorerSearchResult {
    pub generation: u64,
    pub query: String,
    pub nodes: Vec<ExplorerNode>,
    pub match_count: usize,
    pub total_count: usize,
    pub best_match_row: Option<usize>,
}

/// Background worker that runs the filtered fs walk off the UI thread.
///
/// One background thread services all submitted jobs. Jobs are coalesced
/// by processing only the **last** job in a drained batch (queries are
/// strictly latest-wins — no per-key map needed because there is only one
/// explorer search at a time). Results are sent back on an unbounded channel.
///
/// `Drop` closes the job channel (dropping `tx`) and then **joins** the
/// thread — this prevents teardown races with `libgit2`'s OpenSSL cleanup
/// (same rationale as `BlameWorker`).
pub(crate) struct ExplorerSearchWorker {
    tx: Option<Sender<ExplorerSearchJob>>,
    rx: Receiver<ExplorerSearchResult>,
    join: Option<thread::JoinHandle<()>>,
}

impl ExplorerSearchWorker {
    /// Spawn the worker thread. Returns immediately.
    pub(crate) fn new() -> Self {
        let (job_tx, job_rx) = crossbeam_channel::unbounded::<ExplorerSearchJob>();
        let (res_tx, res_rx) = crossbeam_channel::unbounded::<ExplorerSearchResult>();

        let handle = thread::Builder::new()
            .name("hjkl-explorer-search".into())
            .spawn(move || explorer_search_worker_loop(job_rx, res_tx))
            .expect("spawn explorer-search worker");

        Self {
            tx: Some(job_tx),
            rx: res_rx,
            join: Some(handle),
        }
    }

    /// Submit a search job. Non-blocking.
    pub(crate) fn submit(&self, job: ExplorerSearchJob) {
        if let Some(tx) = self.tx.as_ref() {
            let _ = tx.send(job);
        }
    }

    /// Non-blocking drain. Returns the next completed result, if any.
    pub(crate) fn try_recv(&self) -> Option<ExplorerSearchResult> {
        self.rx.try_recv().ok()
    }
}

impl Drop for ExplorerSearchWorker {
    fn drop(&mut self) {
        // Close the sender first — the worker's `recv()` will return `Err`
        // and the loop will exit cleanly.
        drop(self.tx.take());
        if let Some(h) = self.join.take() {
            let _ = h.join();
        }
    }
}

impl Default for ExplorerSearchWorker {
    fn default() -> Self {
        Self::new()
    }
}

/// Main loop executed on the worker thread.
///
/// Blocks on `recv()` until a job arrives, then drains all
/// immediately-available additional jobs with `try_recv()` and
/// **processes only the last one** (highest index — pure coalesce; earlier
/// queries are stale). Builds the filtered tree via
/// `ExplorerTree::for_search`, which creates the `git2::Repository` on
/// this thread (never sent across a channel). Loops until the sender is
/// dropped.
fn explorer_search_worker_loop(
    job_rx: Receiver<ExplorerSearchJob>,
    res_tx: Sender<ExplorerSearchResult>,
) {
    loop {
        // Block until at least one job arrives (or channel closes).
        let first = match job_rx.recv() {
            Ok(j) => j,
            Err(_) => return, // sender dropped → exit
        };

        // Drain all immediately-available additional jobs without blocking,
        // then keep only the last one (latest-wins coalescing).
        let mut last = first;
        while let Ok(j) = job_rx.try_recv() {
            last = j;
        }

        // Run the filtered fs walk on the worker thread.
        let tree = ExplorerTree::for_search(
            last.root,
            last.show_hidden,
            last.respect_gitignore,
            last.query.clone(),
        );

        let result = ExplorerSearchResult {
            generation: last.generation,
            query: last.query,
            match_count: tree.match_count,
            total_count: tree.total_count,
            best_match_row: tree.best_match_row,
            nodes: tree.nodes,
        };

        if res_tx.send(result).is_err() {
            // Receiver dropped → UI is gone. Exit.
            return;
        }
    }
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

        self.explorer = Some(ExplorerPane {
            win_id: new_win_id,
            tree,
            last_reconcile_gen: 0,
            trashed: Vec::new(),
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
        self.slots[slot_idx].editor.set_content(&text);
        let _ = self.slots[slot_idx].editor.take_content_edits();
        let _ = self.slots[slot_idx].editor.take_content_reset();

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
    /// since the last reconcile, diff the buffer against the tree baseline and
    /// apply the resulting filesystem ops. Cheap when nothing changed (guarded
    /// by the `dirty_gen` check and the Normal-mode check).
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
            let baseline: Vec<(PathBuf, bool)> = ep
                .tree
                .nodes
                .iter()
                .map(|n| (n.path.clone(), n.is_dir))
                .collect();
            let text = self.slots[slot_idx].editor.buffer().as_string();
            let root = ep.tree.root.clone();
            (baseline, text, root)
        };

        let ops = super::explorer_reconcile::reconcile(&baseline, &text, &root);
        if ops.is_empty() {
            // Nothing to do — update the gen so we don't re-check next tick.
            if let Some(ep) = self.explorer.as_mut() {
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

        let (newly_created, errors) = super::explorer_reconcile::apply_ops(&ops, &mut trashed);

        // Put the trashed registry back.
        if let Some(ep) = self.explorer.as_mut() {
            ep.trashed = trashed;
        }

        // Toast any errors.
        for err in &errors {
            self.bus.error(format!("explorer: {err}"));
        }

        // Re-read disk, rebuild tree, reset buffer text + cursor (sticky on path).
        self.explorer_rebuild_buffer();
        self.recompute_explorer_git_base();

        // Update last_reconcile_gen to the NEW dirty_gen (after the rebuild
        // which changes buffer text and thus bumps dirty_gen again).
        let new_gen = self
            .explorer_slot_idx()
            .map(|idx| self.slots[idx].editor.buffer().dirty_gen())
            .unwrap_or(cur_gen);
        if let Some(ep) = self.explorer.as_mut() {
            ep.last_reconcile_gen = new_gen;
        }

        // Open newly-created files in the nearest non-explorer window.
        for path in newly_created {
            let target_win = self.nearest_non_explorer_window();
            if let Some(win_id) = target_win {
                self.switch_focus(win_id);
            }
            self.suppress_open_notice = true;
            let s = Self::explorer_open_arg(&path);
            self.dispatch_ex(&format!("edit {s}"));
            self.suppress_open_notice = false;
        }
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

        let win_id;
        let row;
        {
            let Some(ep) = self.explorer.as_mut() else {
                return;
            };
            if !abs.starts_with(&ep.tree.root) {
                return; // file isn't under the explorer's root
            }
            win_id = ep.win_id;
            row = ep.tree.reveal(&abs);
        }
        self.explorer_rebuild_buffer();
        if let Some(r) = row {
            if let Some(Some(win)) = self.windows.get_mut(win_id) {
                win.cursor_row = r;
                win.cursor_col = 0;
                // Scroll the explorer window so the revealed row stays in
                // view. The window's per-window `top_row` is independent of
                // any other window on the same buffer, so this only moves
                // the explorer's viewport — a second window showing the same
                // slot is unaffected. Without this the cursor can land off
                // the bottom/top of the pane after a buffer switch.
                let height = win.last_rect.map(|rc| rc.h as usize).unwrap_or(0);
                if height > 0 {
                    if r < win.top_row {
                        win.top_row = r;
                    } else if r >= win.top_row + height {
                        win.top_row = r + 1 - height;
                    }
                }
            }
            // Sync the explorer editor cursor so the (usually unfocused)
            // cursorline highlights the revealed row.
            self.sync_viewport_to_explorer_editor();
        }
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
            // Toggle dir expansion and rebuild buffer.
            let path = node.path.clone();
            if let Some(ref mut ep) = self.explorer {
                ep.tree.toggle(&path);
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

    // ── Explorer search (fuzzy filter) ────────────────────────────────────────

    /// Open the explorer's vim-editable fuzzy-filter field. The field is seeded
    /// with any committed filter query so re-focusing keeps the current search
    /// visible (Enter commits without clearing; only Esc cancels). `insert`
    /// picks the starting mode: `true` (via `/` or click) drops straight into
    /// insert; `false` (via `k` from the top row) lands in normal mode so `j`
    /// moves back down into the tree.
    pub(crate) fn open_explorer_search(&mut self, insert: bool) {
        // Seed from the committed filter so the box keeps showing it.
        let seed = self
            .explorer
            .as_ref()
            .and_then(|ep| ep.tree.filter.clone())
            .unwrap_or_default();

        let mut field = hjkl_form::TextFieldEditor::new(true);
        field.enter_insert_at_end();
        for c in seed.chars() {
            field.handle_input(hjkl_engine::Input {
                key: hjkl_engine::Key::Char(c),
                ctrl: false,
                alt: false,
                shift: false,
            });
        }
        if !insert {
            field.enter_normal();
        }
        self.explorer_search = Some(field);
    }

    /// Re-filter the tree from the current field text and move cursor to the
    /// first matched file row.
    #[allow(dead_code)]
    /// Move the explorer cursor to the highest-scoring match
    /// (`tree.best_match_row`), falling back to the first matched file row.
    pub(crate) fn explorer_cursor_to_best_match(&mut self) {
        let target = self.explorer.as_ref().and_then(|ep| {
            ep.tree
                .best_match_row
                .or_else(|| ep.tree.nodes.iter().position(|n| !n.is_dir))
        });
        let win_id = self.explorer.as_ref().map(|ep| ep.win_id);
        if let (Some(row), Some(win_id)) = (target, win_id) {
            if let Some(Some(win)) = self.windows.get_mut(win_id) {
                win.cursor_row = row;
                win.cursor_col = 0;
            }
            let fw = self.focused_window();
            if fw == win_id {
                self.sync_viewport_to_explorer_editor();
            }
        }
    }

    /// Key handler while `explorer_search` is active — mirrors
    /// `handle_search_field_key` minus history / `<C-f>`.
    pub(crate) fn handle_explorer_search_key(&mut self, key: crossterm::event::KeyEvent) {
        use hjkl_engine::{Key as EngineKey, VimMode};

        let input = hjkl_engine_tui::crossterm_to_input(key);

        // Enter → commit (close the field, keep filter). Clear the pending
        // debounce and bump gen so any in-flight worker result is dropped —
        // the committed filter is already installed from the last applied result.
        if input.key == EngineKey::Enter {
            // If a debounce was still pending (typed fast, then Enter before it
            // fired), apply the final query synchronously so the committed
            // filter reflects exactly what was typed.
            if let Some(q) = self.explorer_search_pending_query.take() {
                if let Some(ref mut ep) = self.explorer {
                    ep.tree.apply_filter(&q);
                }
                self.explorer_rebuild_buffer();
                self.explorer_cursor_to_best_match();
            }
            self.explorer_search = None;
            self.explorer_search_dirty_at = None;
            self.explorer_search_pending_query = None;
            self.explorer_search_gen = self.explorer_search_gen.wrapping_add(1);
            return;
        }

        // Esc logic:
        //   - empty field → cancel (clear filter + close)
        //   - Insert mode → enter Normal mode
        //   - Normal mode, non-empty → cancel (clear filter + close)
        if input.key == EngineKey::Esc {
            let (is_empty, is_insert) = match self.explorer_search.as_ref() {
                Some(f) => (f.text().is_empty(), f.vim_mode() == VimMode::Insert),
                None => return,
            };
            if is_empty || !is_insert {
                // Cancel: close + clear filter + clear pending debounce.
                self.explorer_search = None;
                self.explorer_search_dirty_at = None;
                self.explorer_search_pending_query = None;
                self.explorer_search_gen = self.explorer_search_gen.wrapping_add(1);
                if let Some(ref mut ep) = self.explorer {
                    ep.tree.clear_filter();
                }
                self.explorer_rebuild_buffer();
            } else {
                // Insert → Normal.
                if let Some(ref mut f) = self.explorer_search {
                    f.enter_normal();
                }
            }
            return;
        }

        // In NORMAL mode, `j`/Down returns focus to the tree (keeps the
        // committed filter — this is navigation, not a cancel).
        {
            use crossterm::event::KeyCode;
            let in_normal = self
                .explorer_search
                .as_ref()
                .map(|f| f.vim_mode() == VimMode::Normal)
                .unwrap_or(false);
            if in_normal && matches!(key.code, KeyCode::Char('j') | KeyCode::Down) {
                self.explorer_search = None;
                self.explorer_search_dirty_at = None;
                self.explorer_search_pending_query = None;
                self.explorer_search_gen = self.explorer_search_gen.wrapping_add(1);
                return;
            }
        }

        // Backspace on empty prompt → cancel.
        if input.key == EngineKey::Backspace {
            let is_empty = self
                .explorer_search
                .as_ref()
                .map(|f| f.text().is_empty())
                .unwrap_or(true);
            if is_empty {
                self.explorer_search = None;
                self.explorer_search_dirty_at = None;
                self.explorer_search_pending_query = None;
                self.explorer_search_gen = self.explorer_search_gen.wrapping_add(1);
                if let Some(ref mut ep) = self.explorer {
                    ep.tree.clear_filter();
                }
                self.explorer_rebuild_buffer();
                return;
            }
        }

        // Forward the key to the field; if content changed, schedule a
        // debounced worker submission rather than filtering synchronously.
        let dirty = match self.explorer_search.as_mut() {
            Some(f) => f.handle_input(input),
            None => return,
        };
        if dirty {
            let text = self
                .explorer_search
                .as_ref()
                .map(|f| f.text())
                .unwrap_or_default();
            self.explorer_search_pending_query = Some(text);
            self.explorer_search_dirty_at = Some(Instant::now());
        }
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

    /// End-to-end: `/` in the focused explorer opens the fuzzy-search field via
    /// `handle_keypress`, typing applies a tree filter, and Esc cancels +
    /// restores the unfiltered tree. Guards the full key path (not just the
    /// `ExplorerTree::apply_filter` unit).
    #[test]
    fn slash_opens_search_typing_filters_esc_cancels() {
        use crate::keymap_actions::AppAction;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut app = super::super::App::new(None, false, None, None).unwrap();
        app.dispatch_action(AppAction::ToggleExplorer, 1);
        assert!(app.explorer.is_some(), "explorer should be open");
        assert!(app.explorer_buf_focused(), "explorer should be focused");

        // `/` opens the search field.
        app.handle_keypress(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        assert!(
            app.explorer_search.is_some(),
            "`/` must open the explorer fuzzy-search field"
        );

        // Typing routes to the field and SCHEDULES a debounce (not synchronous
        // filter) — the worker fires after EXPLORER_SEARCH_DEBOUNCE elapses.
        app.handle_keypress(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
        assert_eq!(
            app.explorer_search_pending_query.as_deref(),
            Some("z"),
            "typing must schedule a debounce with the query"
        );
        assert!(
            app.explorer_search_dirty_at.is_some(),
            "typing must arm the debounce timer"
        );

        // Esc: insert→normal, then normal(non-empty)→cancel (close + clear).
        // Cancel must also clear the pending debounce state.
        app.handle_keypress(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.handle_keypress(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(
            app.explorer_search.is_none(),
            "Esc must close the search field"
        );
        assert!(
            app.explorer_search_pending_query.is_none(),
            "Esc must clear the pending query"
        );
        assert!(
            app.explorer_search_dirty_at.is_none(),
            "Esc must clear the debounce timer"
        );
        let filter2 = app.explorer.as_ref().and_then(|ep| ep.tree.filter.clone());
        assert_eq!(filter2, None, "cancel must restore the unfiltered tree");
    }

    /// `ExplorerTree::for_search` constructs and walks a filtered tree without
    /// spinning up a worker thread — exactly the code path the worker uses.
    /// Query "buf" on the make_filter_tree fixture must yield 2 matches
    /// (buffer_ops.rs, buffer_test.rs) plus their ancestor dirs.
    #[test]
    fn for_search_returns_filtered_tree() {
        let root = make_filter_tree();
        let tree = ExplorerTree::for_search(root.clone(), true, false, "buf".to_string());

        let paths: Vec<String> = tree
            .nodes
            .iter()
            .filter_map(|n| n.path.file_name().map(|f| f.to_string_lossy().into_owned()))
            .collect();

        assert_eq!(tree.match_count, 2, "match_count must be 2 (buf query)");
        assert!(
            paths.contains(&"buffer_ops.rs".to_string()),
            "buffer_ops.rs must be present: {paths:?}"
        );
        assert!(
            paths.contains(&"buffer_test.rs".to_string()),
            "buffer_test.rs must be present: {paths:?}"
        );
        // Ancestor dirs must be included.
        assert!(
            paths.contains(&"src".to_string()),
            "src dir must be present as ancestor: {paths:?}"
        );
        assert!(
            paths.contains(&"tests".to_string()),
            "tests dir must be present as ancestor: {paths:?}"
        );
        // Non-matching files must be absent.
        assert!(
            !paths.contains(&"main.rs".to_string()),
            "main.rs must NOT be present: {paths:?}"
        );
        assert!(
            !paths.contains(&"readme.md".to_string()),
            "readme.md must NOT be present: {paths:?}"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn root_first_then_dirs_first_then_name() {
        let root = make_tree();
        let tree = ExplorerTree::new(root.clone());
        assert_eq!(tree.nodes[0].path, root);
        assert!(tree.nodes[0].is_dir && tree.nodes[0].depth == 0);
        assert_eq!(
            child_names(&tree),
            vec!["a_dir", "b_dir", "m_file.txt", "z_file.txt"]
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn expand_inserts_children_at_depth_with_guide() {
        let root = make_tree();
        let mut tree = ExplorerTree::new(root.clone());
        let a_dir_path = tree.nodes[1].path.clone(); // a_dir
        tree.toggle(&a_dir_path);
        assert_eq!(
            child_names(&tree),
            vec!["a_dir", "inner.txt", "b_dir", "m_file.txt", "z_file.txt"]
        );
        let inner = &tree.nodes[2]; // root, a_dir, inner.txt
        assert_eq!(inner.depth, 2);
        assert!(!inner.is_dir);
        assert_eq!(inner.branches, vec![true]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn last_child_flag() {
        let root = make_tree();
        let tree = ExplorerTree::new(root.clone());
        assert!(!tree.nodes[1].is_last); // a_dir
        let z = tree.nodes.last().unwrap();
        assert_eq!(z.path.file_name().unwrap(), "z_file.txt");
        assert!(z.is_last);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn collapse_removes_subtree() {
        let root = make_tree();
        let mut tree = ExplorerTree::new(root.clone());
        let a_dir_path = tree.nodes[1].path.clone();
        tree.toggle(&a_dir_path); // expand
        assert_eq!(tree.nodes.len(), 6); // root + 4 + inner
        tree.toggle(&a_dir_path); // collapse (toggle on an expanded dir)
        assert_eq!(
            child_names(&tree),
            vec!["a_dir", "b_dir", "m_file.txt", "z_file.txt"]
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
    fn render_text_line_count_after_expand() {
        let root = make_tree();
        let mut tree = ExplorerTree::new(root.clone());
        let a_dir_path = tree.nodes[1].path.clone();
        tree.toggle(&a_dir_path);
        let text = tree.render_text();
        let line_count = text.lines().count();
        assert_eq!(line_count, tree.nodes.len());
        let _ = fs::remove_dir_all(&root);
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
                if app.active().editor.vim_mode() == VimMode::Insert {
                    app.dispatch_insert_key(key);
                } else {
                    hjkl_vim_tui::handle_key(&mut app.active_mut().editor, key);
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
        std::env::set_current_dir(prev).unwrap();
        assert!(exists, "o + type + Esc must create the file on disk");
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

    // ── Plan Verification: fuzzy filter tests ──────────────────────────────

    /// Tree fixture for filter tests:
    /// root/{ src/{ buffer_ops.rs, main.rs }, tests/{ buffer_test.rs }, readme.md }
    fn make_filter_tree() -> PathBuf {
        let base = std::env::temp_dir().join(format!("hjkl_filter_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("src")).unwrap();
        fs::create_dir_all(base.join("tests")).unwrap();
        fs::write(base.join("src").join("buffer_ops.rs"), "x").unwrap();
        fs::write(base.join("src").join("main.rs"), "x").unwrap();
        fs::write(base.join("tests").join("buffer_test.rs"), "x").unwrap();
        fs::write(base.join("readme.md"), "x").unwrap();
        base
    }

    #[test]
    fn filter_best_match_row_points_at_highest_scorer() {
        let root = make_filter_tree();
        let mut tree = ExplorerTree::new(root.clone());
        // "main" matches only src/main.rs in this fixture; best_match_row must
        // point at that node (a file, not a dir).
        tree.apply_filter("main");
        let row = tree.best_match_row.expect("best_match_row should be set");
        let node = &tree.nodes[row];
        assert!(!node.is_dir, "best match must be a file");
        assert_eq!(
            node.path.file_name().unwrap(),
            "main.rs",
            "best match should be main.rs, got {:?}",
            node.path
        );
        // Unfiltered → cleared.
        tree.clear_filter();
        assert!(tree.best_match_row.is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn filter_keeps_matching_files_and_ancestors() {
        let root = make_filter_tree();
        let mut tree = ExplorerTree::new(root.clone());
        tree.apply_filter("buf");

        // buffer_ops.rs and buffer_test.rs should match; main.rs and readme.md
        // should be absent.
        let paths: Vec<String> = tree
            .nodes
            .iter()
            .filter_map(|n| n.path.file_name().map(|f| f.to_string_lossy().into_owned()))
            .collect();

        assert!(
            paths.contains(&"buffer_ops.rs".to_string()),
            "buffer_ops.rs must be present: {paths:?}"
        );
        assert!(
            paths.contains(&"buffer_test.rs".to_string()),
            "buffer_test.rs must be present: {paths:?}"
        );
        // Ancestor dirs must appear too.
        assert!(
            paths.contains(&"src".to_string()),
            "src dir must be present as ancestor: {paths:?}"
        );
        assert!(
            paths.contains(&"tests".to_string()),
            "tests dir must be present as ancestor: {paths:?}"
        );
        // Non-matching files must be absent.
        assert!(
            !paths.contains(&"main.rs".to_string()),
            "main.rs must NOT be present: {paths:?}"
        );
        assert!(
            !paths.contains(&"readme.md".to_string()),
            "readme.md must NOT be present: {paths:?}"
        );
        // match_count must equal 2 (buffer_ops.rs + buffer_test.rs).
        assert_eq!(
            tree.match_count, 2,
            "match_count should be 2, got {}",
            tree.match_count
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn filter_empty_query_equals_full_tree() {
        let root = make_filter_tree();
        let mut tree = ExplorerTree::new(root.clone());
        // Count nodes with no filter (default: root + dirs only at top level,
        // since a_dir etc. are collapsed). We expand everything manually.
        for entry in fs::read_dir(&root).unwrap().flatten() {
            let p = entry.path();
            if p.is_dir() {
                tree.expanded.insert(p);
            }
        }
        tree.rebuild();
        let unfiltered_count = tree.nodes.len();

        // Apply and then clear the filter.
        tree.apply_filter("buf");
        assert!(
            tree.nodes.len() < unfiltered_count,
            "filtered should be shorter"
        );

        tree.apply_filter(""); // empty → clear
        assert_eq!(
            tree.nodes.len(),
            unfiltered_count,
            "after clearing filter, node count must equal unfiltered count"
        );
        assert!(
            tree.filter.is_none(),
            "filter should be None after empty apply"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn filter_nodes_have_no_orphans() {
        let root = make_filter_tree();
        let mut tree = ExplorerTree::new(root.clone());
        tree.apply_filter("buf");

        // Every non-root node's parent path must appear in the node list.
        let node_paths: HashSet<PathBuf> = tree.nodes.iter().map(|n| n.path.clone()).collect();
        for node in &tree.nodes {
            if node.depth == 0 {
                continue;
            }
            let parent = node.path.parent().map(|p| p.to_path_buf());
            if let Some(p) = parent {
                assert!(
                    node_paths.contains(&p),
                    "node {:?} has no parent in the node list (orphan)",
                    node.path
                );
            }
        }

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn filter_no_match_yields_only_root() {
        let root = make_filter_tree();
        let mut tree = ExplorerTree::new(root.clone());
        tree.apply_filter("xyzzy_no_match_possible");

        assert_eq!(
            tree.nodes.len(),
            1,
            "a query matching nothing should yield only the root node"
        );
        assert_eq!(tree.nodes[0].path, root, "the sole node should be the root");
        assert_eq!(tree.match_count, 0, "match_count must be 0");

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
        // Non-root dir nodes (a_dir, b_dir) must end with '/'.
        for line in &lines[1..] {
            let trimmed = line.trim_start();
            // Identify dir lines by the name set known from make_tree.
            if trimmed.starts_with("a_dir") || trimmed.starts_with("b_dir") {
                assert!(
                    trimmed.ends_with('/'),
                    "dir line must end with '/': {trimmed:?}"
                );
            }
        }
        let _ = fs::remove_dir_all(&root);
    }

    /// `<C-s>` resolves to ExplorerOpenSplit; `<C-v>` → ExplorerOpenVsplit;
    /// `<C-t>` → ExplorerOpenTab; `gh` → ExplorerToggleHidden; `gi` →
    /// ExplorerToggleGitignore in the explorer keymap.
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
            Some(AppAction::ExplorerOpenSplit),
            "<C-s> must map to ExplorerOpenSplit"
        );
        assert_eq!(
            resolve(&[KmKeyEvent::ctrl('v')]),
            Some(AppAction::ExplorerOpenVsplit),
            "<C-v> must map to ExplorerOpenVsplit"
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
}
