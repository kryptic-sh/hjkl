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

// ── Prompt / confirm / clipboard state ────────────────────────────────────────

/// Kind of explorer prompt currently open.
#[derive(Debug, Clone)]
pub(crate) enum ExplorerPromptKind {
    /// `a` — create a new file or directory.
    Create,
    /// `r` — rename the node under cursor.
    Rename {
        /// The path being renamed.
        from: PathBuf,
    },
}

/// An active explorer text prompt (create / rename).
pub(crate) struct ExplorerPrompt {
    pub kind: ExplorerPromptKind,
    pub field: hjkl_form::TextFieldEditor,
    /// Directory under which the new name will be placed.
    pub base: PathBuf,
}

/// Pending delete confirmation.
#[derive(Debug, Clone)]
pub(crate) struct ExplorerConfirm {
    /// Path to delete.
    pub path: PathBuf,
    pub is_dir: bool,
}

/// Clipboard entry for copy (`y`) / cut (`x`) operations.
#[derive(Debug, Clone)]
pub(crate) struct ExplorerClip {
    pub path: PathBuf,
    /// `true` → move (cut); `false` → copy.
    pub cut: bool,
}

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
    fn push_children(
        &self,
        dir: &Path,
        depth: usize,
        prefix: &[bool],
        out: &mut Vec<ExplorerNode>,
        repo: Option<&git2::Repository>,
    ) {
        let children = self.read_children(dir, repo);
        let n = children.len();
        for (i, (path, is_dir)) in children.into_iter().enumerate() {
            let is_last = i + 1 == n;
            out.push(ExplorerNode {
                path: path.clone(),
                depth,
                is_dir,
                is_last,
                branches: prefix.to_vec(),
            });
            if is_dir && self.expanded.contains(&path) {
                let mut child_prefix = prefix.to_vec();
                child_prefix.push(!is_last);
                self.push_children(&path, depth + 1, &child_prefix, out, repo);
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
        let mut out = Vec::new();
        let root = self.root.clone();
        out.push(ExplorerNode {
            path: root.clone(),
            depth: 0,
            is_dir: true,
            is_last: true,
            branches: Vec::new(),
        });
        if self.expanded.contains(&root) {
            let repo = self.open_repo();
            self.push_children(&root, 1, &[], &mut out, repo.as_ref());
        }
        self.nodes = out;
        self.match_count = 0;
        self.total_count = 0;
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

        // DFS to build nodes — force-expanded, include only `show` members.
        let mut out = Vec::new();
        out.push(ExplorerNode {
            path: root.clone(),
            depth: 0,
            is_dir: true,
            is_last: true,
            branches: Vec::new(),
        });
        self.push_children_filtered(&root, 1, &[], &show, &mut out, repo.as_ref());
        self.nodes = out;
    }

    /// Recursive helper for filtered rebuild — mirrors `push_children` but
    /// limits children to those in `show` and always recurses into dirs
    /// (force-expanded).
    fn push_children_filtered(
        &self,
        dir: &Path,
        depth: usize,
        prefix: &[bool],
        show: &HashSet<PathBuf>,
        out: &mut Vec<ExplorerNode>,
        repo: Option<&git2::Repository>,
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
            out.push(ExplorerNode {
                path: path.clone(),
                depth,
                is_dir,
                is_last,
                branches: prefix.to_vec(),
            });
            if is_dir {
                let mut child_prefix = prefix.to_vec();
                child_prefix.push(!is_last);
                self.push_children_filtered(&path, depth + 1, &child_prefix, show, out, repo);
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

    /// Collapse the directory at `path` (no-op if not expanded).
    pub(crate) fn collapse(&mut self, path: &Path) {
        self.expanded.remove(path);
        self.rebuild();
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

    /// Expand all ancestors of `path` up to (and including) `self.root`, then
    /// rebuild. Returns the row index of `path` in `self.nodes`, or `None` if
    /// `path` is not under the root or not visible after rebuild.
    pub(crate) fn reveal(&mut self, path: &Path) -> Option<usize> {
        // Walk from path upward, inserting each ancestor into `expanded`.
        let mut cur = path.parent();
        while let Some(p) = cur {
            self.expanded.insert(p.to_path_buf());
            if p == self.root {
                break;
            }
            cur = p.parent();
        }
        self.rebuild();
        self.nodes.iter().position(|n| n.path == path)
    }

    /// Build the buffer text and line→node map for the current tree state.
    ///
    /// Each line in the returned `String` corresponds to `nodes[i]`, so
    /// `cursor_row` in the editor maps directly to `nodes[cursor_row]`.
    pub(crate) fn render_text(&self, icons: hjkl_icons::IconMode) -> String {
        let mut out = String::new();
        for (i, node) in self.nodes.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            if node.depth == 0 {
                // Root line: just the directory path (or name).
                let name = node
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| node.path.to_string_lossy().into_owned());
                out.push(hjkl_icons::dir_icon_for_path(
                    &node.path,
                    self.is_expanded(&node.path),
                    icons,
                ));
                out.push(' ');
                out.push_str(&name);
            } else {
                // Guide columns for each ancestor level.
                for &b in &node.branches {
                    out.push(if b { '│' } else { ' ' });
                    out.push(' ');
                }
                // Connector glyph.
                out.push(if node.is_last { '└' } else { '├' });
                out.push('╴');
                // Icon + space + name.
                let icon = if node.is_dir {
                    hjkl_icons::dir_icon_for_path(&node.path, self.is_expanded(&node.path), icons)
                } else {
                    hjkl_icons::file_icon_for_path(&node.path, icons)
                };
                out.push(icon);
                out.push(' ');
                let name = node
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                out.push_str(&name);
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

        // Reveal the active file's path before rendering, so the initial
        // cursor lands on it.
        let reveal_row: Option<usize> = active_file.as_deref().and_then(|p| {
            // Only reveal when the file is under cwd.
            if p.starts_with(&cwd) {
                tree.reveal(p)
            } else {
                None
            }
        });

        let text = tree.render_text(self.icon_mode);
        // Nodes are rebuilt by new() above; no extra rebuild needed.

        let buffer_id = self.next_buffer_id;
        self.next_buffer_id += 1;

        let host = TuiHost::new();
        let mut editor = Editor::new(
            Buffer::new(),
            host,
            Options {
                readonly: true,
                ..Options::default()
            },
        );
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
        });

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
        let icons = self.icon_mode;
        let (text, win_id) = match self.explorer.as_ref() {
            Some(ep) => (ep.tree.render_text(icons), ep.win_id),
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

    /// Sync the explorer editor's cursor from the explorer window's snapshot.
    /// Like `sync_viewport_to_editor` but only for the explorer slot.
    fn sync_viewport_to_explorer_editor(&mut self) {
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
            let s = node.path.to_string_lossy().to_string();
            self.dispatch_ex(&format!("edit {s}"));
        }
    }

    /// h/Left: collapse expanded dir or move to parent line.
    pub(crate) fn explorer_collapse(&mut self) {
        let cursor_row = {
            let ep = self.explorer.as_ref().unwrap();
            let win = self.windows.get(ep.win_id).and_then(|w| w.as_ref());
            win.map(|w| w.cursor_row).unwrap_or(0)
        };

        let node = self
            .explorer
            .as_ref()
            .and_then(|ep| ep.tree.nodes.get(cursor_row))
            .cloned();
        let Some(node) = node else { return };

        if node.is_dir
            && let Some(ref ep) = self.explorer
            && ep.tree.is_expanded(&node.path)
        {
            let path = node.path.clone();
            if let Some(ref mut ep) = self.explorer {
                ep.tree.collapse(&path);
            }
            self.explorer_rebuild_buffer();
            return;
        }

        // Move cursor to the parent row.
        if node.depth == 0 {
            return;
        }
        let target_depth = node.depth - 1;
        let parent_row = self.explorer.as_ref().and_then(|ep| {
            ep.tree.nodes[..cursor_row]
                .iter()
                .rposition(|n| n.depth == target_depth)
        });
        if let Some(row) = parent_row {
            let ep = self.explorer.as_ref().unwrap();
            let win_id = ep.win_id;
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

    /// Resolve the "target directory" for create / paste from the cursor node:
    /// - dir node → that directory
    /// - file node → parent of file
    /// - root node with no parent → root
    fn explorer_target_dir(node: &ExplorerNode) -> PathBuf {
        if node.is_dir {
            node.path.clone()
        } else {
            node.path
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| node.path.clone())
        }
    }

    // ── Refresh / hidden / root ───────────────────────────────────────────────

    /// Re-read the filesystem and rebuild the buffer (preserves cursor path).
    pub(crate) fn explorer_refresh(&mut self) {
        if let Some(ref mut ep) = self.explorer {
            ep.tree.rebuild();
        }
        self.explorer_rebuild_buffer();
    }

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
        let s = node.path.to_string_lossy().to_string();
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
        let s = node.path.to_string_lossy().to_string();
        self.dispatch_ex(&format!("vsplit {s}"));
    }

    /// Open the file under cursor in a new tab.
    pub(crate) fn explorer_open_tab(&mut self) {
        let node = match self.explorer_cursor_node() {
            Some(n) if !n.is_dir => n,
            _ => return,
        };
        let s = node.path.to_string_lossy().to_string();
        self.dispatch_ex(&format!("tabnew {s}"));
    }

    // ── File operations ───────────────────────────────────────────────────────

    /// `a` — open a create prompt. Name ending with `/` creates a directory.
    pub(crate) fn explorer_create(&mut self) {
        let node = match self.explorer_cursor_node() {
            Some(n) => n,
            None => return,
        };
        let base = Self::explorer_target_dir(&node);
        let mut field = hjkl_form::TextFieldEditor::new(true);
        field.enter_insert_at_end();
        self.explorer_prompt = Some(ExplorerPrompt {
            kind: ExplorerPromptKind::Create,
            field,
            base,
        });
    }

    /// `r` — open a rename prompt prefilled with the current filename.
    pub(crate) fn explorer_rename(&mut self) {
        let node = match self.explorer_cursor_node() {
            Some(n) => n,
            None => return,
        };
        if node.depth == 0 {
            return; // Don't rename the root.
        }
        let from = node.path.clone();
        let base = from
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| from.clone());
        let prefill = from
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();

        let mut field = hjkl_form::TextFieldEditor::new(true);
        field.enter_insert_at_end();
        // Seed the field with the current filename.
        for c in prefill.chars() {
            let input = hjkl_engine::Input {
                key: hjkl_engine::Key::Char(c),
                ctrl: false,
                alt: false,
                shift: false,
            };
            field.handle_input(input);
        }
        self.explorer_prompt = Some(ExplorerPrompt {
            kind: ExplorerPromptKind::Rename { from },
            field,
            base,
        });
    }

    /// `d` — open a delete confirmation prompt.
    pub(crate) fn explorer_delete(&mut self) {
        let node = match self.explorer_cursor_node() {
            Some(n) => n,
            None => return,
        };
        if node.depth == 0 {
            return; // Refuse to delete the root.
        }
        self.explorer_confirm = Some(ExplorerConfirm {
            path: node.path,
            is_dir: node.is_dir,
        });
    }

    /// `y` — copy the node under cursor to the clipboard.
    pub(crate) fn explorer_copy(&mut self) {
        let node = match self.explorer_cursor_node() {
            Some(n) => n,
            None => return,
        };
        if node.depth == 0 {
            return;
        }
        let name = node
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.bus.info(format!("Copied: {name}"));
        self.explorer_clip = Some(ExplorerClip {
            path: node.path,
            cut: false,
        });
    }

    /// `x` — cut the node under cursor (move on paste).
    pub(crate) fn explorer_cut(&mut self) {
        let node = match self.explorer_cursor_node() {
            Some(n) => n,
            None => return,
        };
        if node.depth == 0 {
            return;
        }
        let name = node
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.bus.info(format!("Cut: {name}"));
        self.explorer_clip = Some(ExplorerClip {
            path: node.path,
            cut: true,
        });
    }

    /// `p` — paste from clipboard into the target directory.
    pub(crate) fn explorer_paste(&mut self) {
        let clip = match self.explorer_clip.clone() {
            Some(c) => c,
            None => {
                self.bus.info("Nothing to paste");
                return;
            }
        };
        let node = match self.explorer_cursor_node() {
            Some(n) => n,
            None => return,
        };
        let dest_dir = Self::explorer_target_dir(&node);
        let file_name = match clip.path.file_name() {
            Some(n) => n,
            None => {
                self.bus.error("Cannot paste: source has no filename");
                return;
            }
        };
        let dest = dest_dir.join(file_name);

        if clip.cut {
            // Move: try rename first (same device), fall back to copy+remove.
            if let Err(e) = std::fs::rename(&clip.path, &dest) {
                // Cross-device: copy then remove.
                if copy_recursive(&clip.path, &dest).is_err() {
                    self.bus.error(format!("Paste failed: {e}"));
                    return;
                }
                let _ = if clip.path.is_dir() {
                    std::fs::remove_dir_all(&clip.path)
                } else {
                    std::fs::remove_file(&clip.path)
                };
            }
            // Clear clip after cut-paste.
            self.explorer_clip = None;
        } else {
            // Copy.
            if let Err(e) = copy_recursive(&clip.path, &dest) {
                self.bus.error(format!("Copy failed: {e}"));
                return;
            }
        }

        self.explorer_refresh();
        // Reveal the destination.
        if let Some(ref mut ep) = self.explorer {
            let row = ep.tree.reveal(&dest);
            let win_id = ep.win_id;
            if let (Some(row), Some(Some(win))) = (row, self.windows.get_mut(win_id)) {
                win.cursor_row = row;
                win.cursor_col = 0;
            }
        }
        self.explorer_rebuild_buffer();
    }

    // ── Prompt commit helpers ─────────────────────────────────────────────────

    /// Called when the user presses Enter in a Create prompt.
    pub(crate) fn explorer_commit_create(&mut self, name: String) {
        let base = match self.explorer_prompt.as_ref() {
            Some(ep) => ep.base.clone(),
            None => return,
        };
        self.explorer_prompt = None;

        let new_path = base.join(&name);
        let result = if name.ends_with('/') {
            std::fs::create_dir_all(&new_path)
        } else {
            // Ensure parent dirs exist, then create the file.
            if let Some(parent) = new_path.parent()
                && let Err(e) = std::fs::create_dir_all(parent)
            {
                self.bus.error(format!("Create failed: {e}"));
                return;
            }
            std::fs::File::create(&new_path).map(|_| ())
        };

        match result {
            Ok(()) => {
                self.explorer_refresh();
                // Expand the base dir and reveal the new path.
                if let Some(ref mut ep) = self.explorer {
                    ep.tree.expanded.insert(base.clone());
                    let row = ep.tree.reveal(&new_path);
                    let win_id = ep.win_id;
                    if let (Some(row), Some(Some(win))) = (row, self.windows.get_mut(win_id)) {
                        win.cursor_row = row;
                        win.cursor_col = 0;
                    }
                }
                self.explorer_rebuild_buffer();
            }
            Err(e) => {
                self.bus.error(format!("Create failed: {e}"));
            }
        }
    }

    /// Called when the user presses Enter in a Rename prompt.
    pub(crate) fn explorer_commit_rename(&mut self, new_name: String) {
        let (from, base) = match self.explorer_prompt.as_ref() {
            Some(ep) => match &ep.kind {
                ExplorerPromptKind::Rename { from } => (from.clone(), ep.base.clone()),
                _ => return,
            },
            None => return,
        };
        self.explorer_prompt = None;

        let dest = base.join(&new_name);
        match std::fs::rename(&from, &dest) {
            Ok(()) => {
                self.explorer_refresh();
                if let Some(ref mut ep) = self.explorer {
                    let row = ep.tree.reveal(&dest);
                    let win_id = ep.win_id;
                    if let (Some(row), Some(Some(win))) = (row, self.windows.get_mut(win_id)) {
                        win.cursor_row = row;
                        win.cursor_col = 0;
                    }
                }
                self.explorer_rebuild_buffer();
            }
            Err(e) => {
                self.bus.error(format!("Rename failed: {e}"));
            }
        }
    }

    /// Called when the user confirms deletion with `y`.
    pub(crate) fn explorer_commit_delete(&mut self) {
        let confirm = match self.explorer_confirm.take() {
            Some(c) => c,
            None => return,
        };
        let result = if confirm.is_dir {
            std::fs::remove_dir_all(&confirm.path)
        } else {
            std::fs::remove_file(&confirm.path)
        };
        match result {
            Ok(()) => {
                self.explorer_refresh();
            }
            Err(e) => {
                self.bus.error(format!("Delete failed: {e}"));
            }
        }
    }

    // ── Prompt / confirm key handlers ─────────────────────────────────────────

    /// Route a key when `explorer_prompt` is active.
    pub(crate) fn handle_explorer_prompt_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;
        match key.code {
            KeyCode::Esc => {
                self.explorer_prompt = None;
            }
            KeyCode::Enter => {
                let (kind, name) = match self.explorer_prompt.as_ref() {
                    Some(ep) => {
                        let name = ep.field.text();
                        (ep.kind.clone(), name)
                    }
                    None => return,
                };
                match kind {
                    ExplorerPromptKind::Create => {
                        self.explorer_commit_create(name);
                    }
                    ExplorerPromptKind::Rename { .. } => {
                        self.explorer_commit_rename(name);
                    }
                }
            }
            _ => {
                // Forward to the text field.
                let input = hjkl_engine_tui::crossterm_to_input(key);
                if let Some(ref mut ep) = self.explorer_prompt {
                    ep.field.handle_input(input);
                }
            }
        }
    }

    /// Route a key when `explorer_confirm` (delete) is active.
    pub(crate) fn handle_explorer_confirm_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;
        match key.code {
            // Accept either case regardless of whether SHIFT is reported.
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.explorer_commit_delete();
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.explorer_confirm = None;
            }
            _ => {} // consume but do nothing
        }
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
    pub(crate) fn explorer_apply_search(&mut self) {
        let text = match self.explorer_search.as_ref() {
            Some(f) => f.text(),
            None => return,
        };
        if let Some(ref mut ep) = self.explorer {
            ep.tree.apply_filter(&text);
        }
        self.explorer_rebuild_buffer();

        // Move cursor to the first matched file row (first non-dir, non-root node).
        let first_match = self
            .explorer
            .as_ref()
            .and_then(|ep| ep.tree.nodes.iter().position(|n| !n.is_dir));
        let win_id = self.explorer.as_ref().map(|ep| ep.win_id);
        if let (Some(row), Some(win_id)) = (first_match, win_id) {
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

        // Enter → commit (close the field, keep filter, cursor on first match).
        if input.key == EngineKey::Enter {
            self.explorer_search = None;
            // Cursor was already placed on first match by the last
            // explorer_apply_search call. Nothing more to do.
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
                // Cancel: close + clear filter.
                self.explorer_search = None;
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
                if let Some(ref mut ep) = self.explorer {
                    ep.tree.clear_filter();
                }
                self.explorer_rebuild_buffer();
                return;
            }
        }

        // Forward the key to the field; live-filter if content changed.
        let dirty = match self.explorer_search.as_mut() {
            Some(f) => f.handle_input(input),
            None => return,
        };
        if dirty {
            self.explorer_apply_search();
        }
    }
}

/// Width of the explorer window in columns.
pub(crate) const EXPLORER_WINDOW_WIDTH: u16 = 36;

/// Recursively copy `src` to `dst`. `src` may be a file or directory.
fn copy_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    if src.is_dir() {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            let ty = entry.file_type()?;
            let child_dst = dst.join(entry.file_name());
            if ty.is_dir() {
                copy_recursive(&entry.path(), &child_dst)?;
            } else {
                std::fs::copy(entry.path(), child_dst)?;
            }
        }
        Ok(())
    } else {
        std::fs::copy(src, dst).map(|_| ())
    }
}

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

        // Typing routes to the field and applies a live filter.
        app.handle_keypress(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
        let filter = app.explorer.as_ref().and_then(|ep| ep.tree.filter.clone());
        assert_eq!(
            filter.as_deref(),
            Some("z"),
            "typing in the search field must set the tree filter"
        );

        // Esc: insert→normal, then normal(non-empty)→cancel (close + clear).
        app.handle_keypress(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.handle_keypress(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(
            app.explorer_search.is_none(),
            "Esc must close the search field"
        );
        let filter2 = app.explorer.as_ref().and_then(|ep| ep.tree.filter.clone());
        assert_eq!(filter2, None, "cancel must restore the unfiltered tree");
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
        tree.collapse(&a_dir_path);
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
        let text = tree.render_text(hjkl_icons::IconMode::Nerd);
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
        let text = tree.render_text(hjkl_icons::IconMode::Nerd);
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
    fn target_dir_resolver_dir_node_returns_itself() {
        let root = make_tree();
        let tree = ExplorerTree::new(root.clone());
        // nodes[1] is a_dir (a directory).
        let node = tree.nodes[1].clone();
        assert!(node.is_dir, "test prerequisite: nodes[1] is a_dir");
        let target = super::super::App::explorer_target_dir(&node);
        assert_eq!(target, node.path, "dir node → that dir");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn target_dir_resolver_file_node_returns_parent() {
        let root = make_tree();
        let tree = ExplorerTree::new(root.clone());
        // Find m_file.txt (a file node).
        let node = tree
            .nodes
            .iter()
            .find(|n| {
                n.path
                    .file_name()
                    .map(|f| f == "m_file.txt")
                    .unwrap_or(false)
            })
            .cloned()
            .expect("m_file.txt should exist");
        assert!(!node.is_dir, "test prerequisite: m_file.txt is not a dir");
        let target = super::super::App::explorer_target_dir(&node);
        assert_eq!(
            target,
            node.path.parent().unwrap().to_path_buf(),
            "file node → parent dir"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn commit_create_file_creates_on_disk() {
        let root = make_tree();
        let mut app = super::super::App::new(None, false, None, None).unwrap();
        // Manually set up explorer_prompt state as if `a` was pressed.
        let mut field = hjkl_form::TextFieldEditor::new(true);
        field.enter_insert_at_end();
        app.explorer_prompt = Some(super::ExplorerPrompt {
            kind: super::ExplorerPromptKind::Create,
            field,
            base: root.clone(),
        });
        // Commit creation of a plain file.
        app.explorer_commit_create("new_file.txt".to_string());
        assert!(
            root.join("new_file.txt").exists(),
            "new_file.txt should be created on disk"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn commit_create_dir_trailing_slash_creates_dir() {
        let root = make_tree();
        let mut app = super::super::App::new(None, false, None, None).unwrap();
        let mut field = hjkl_form::TextFieldEditor::new(true);
        field.enter_insert_at_end();
        app.explorer_prompt = Some(super::ExplorerPrompt {
            kind: super::ExplorerPromptKind::Create,
            field,
            base: root.clone(),
        });
        app.explorer_commit_create("new_dir/".to_string());
        assert!(
            root.join("new_dir").is_dir(),
            "new_dir/ should create a directory"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn commit_rename_moves_file() {
        let root = make_tree();
        let from = root.join("m_file.txt");
        let expected = root.join("renamed.txt");
        let mut app = super::super::App::new(None, false, None, None).unwrap();
        let mut field = hjkl_form::TextFieldEditor::new(true);
        field.enter_insert_at_end();
        app.explorer_prompt = Some(super::ExplorerPrompt {
            kind: super::ExplorerPromptKind::Rename { from: from.clone() },
            field,
            base: root.clone(),
        });
        app.explorer_commit_rename("renamed.txt".to_string());
        assert!(!from.exists(), "original file should be gone after rename");
        assert!(expected.exists(), "renamed file should exist");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn commit_delete_removes_file() {
        let root = make_tree();
        let path = root.join("m_file.txt");
        assert!(path.exists(), "test prerequisite: m_file.txt exists");
        let mut app = super::super::App::new(None, false, None, None).unwrap();
        app.explorer_confirm = Some(super::ExplorerConfirm {
            path: path.clone(),
            is_dir: false,
        });
        app.explorer_commit_delete();
        assert!(!path.exists(), "m_file.txt should be deleted");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn commit_delete_removes_dir() {
        let root = make_tree();
        let path = root.join("b_dir");
        assert!(path.is_dir(), "test prerequisite: b_dir exists");
        let mut app = super::super::App::new(None, false, None, None).unwrap();
        app.explorer_confirm = Some(super::ExplorerConfirm {
            path: path.clone(),
            is_dir: true,
        });
        app.explorer_commit_delete();
        assert!(!path.exists(), "b_dir should be deleted");
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
}
