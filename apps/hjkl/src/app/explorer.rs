//! File-explorer as a real read-only buffer window (#55) — Neovim neo-tree/oil style.
//!
//! The tree **model** (`ExplorerTree`: root, expanded set, flat node list) is
//! kept from the previous implementation. `render_text` produces the buffer
//! text and a line→node map. Everything else (custom render, key handler,
//! focus flag, scroll, selection, mouse zone) is gone — the engine provides
//! those for free because the explorer is now a real left window in the layout
//! tree.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

// ── Icon helpers (moved from render.rs so explorer.rs can build text) ─────────

/// Nerd-font icon for a directory entry (open vs closed), with special folder names.
pub(crate) fn explorer_dir_icon(path: &Path, expanded: bool) -> char {
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        match name {
            ".git" => return '\u{e702}',         //
            ".github" => return '\u{e709}',      //
            ".config" => return '\u{f013}',      //
            "node_modules" => return '\u{e718}', //
            _ => {}
        }
    }
    if expanded {
        '\u{f0770}' // 󰝰 open folder
    } else {
        '\u{f024b}' // 󰉋 closed folder
    }
}

/// Nerd-font icon for a file, keyed by extension.
pub(crate) fn explorer_file_icon(path: &Path) -> char {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "rs" => '\u{f1617}',                                                   // 󱘗
        "md" | "markdown" => '\u{f0354}',                                      // 󰍔
        "toml" | "ini" | "conf" | "cfg" => '\u{f013}',                         //
        "lock" => '\u{f023}',                                                  //
        "json" => '\u{e60b}',                                                  //
        "yaml" | "yml" => '\u{f0626}',                                         // 󰘦
        "html" | "htm" => '\u{f031d}',                                         // 󰌝
        "css" | "scss" => '\u{e749}',                                          //
        "js" | "mjs" | "cjs" => '\u{e74e}',                                    //
        "ts" | "tsx" => '\u{e628}',                                            //
        "py" => '\u{e606}',                                                    //
        "go" => '\u{e627}',                                                    //
        "sh" | "bash" | "zsh" | "fish" => '\u{f489}',                          //
        "txt" | "text" => '\u{f0219}',                                         // 󰈙
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "ico" => '\u{f1c5}', //
        _ => '\u{f0214}',                                                      // 󰈔 generic document
    }
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
}

impl ExplorerTree {
    /// Create a fresh tree rooted at `root`. The root starts expanded so its
    /// children are visible immediately.
    pub(crate) fn new(root: PathBuf) -> Self {
        let mut expanded = HashSet::new();
        expanded.insert(root.clone());
        let mut tree = Self {
            root,
            expanded,
            nodes: Vec::new(),
        };
        tree.rebuild();
        tree
    }

    /// Read one directory's children, sorted directories-first then by
    /// case-insensitive name. Hidden dotfiles are included.
    fn read_children(dir: &Path) -> Vec<(PathBuf, bool)> {
        let mut entries: Vec<(PathBuf, bool)> = match std::fs::read_dir(dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .map(|e| {
                    let p = e.path();
                    let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                    (p, is_dir)
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

    /// Recursively append `dir`'s expanded children to `out`.
    fn push_children(
        &self,
        dir: &Path,
        depth: usize,
        prefix: &[bool],
        out: &mut Vec<ExplorerNode>,
    ) {
        let children = Self::read_children(dir);
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
                self.push_children(&path, depth + 1, &child_prefix, out);
            }
        }
    }

    /// Rebuild the flattened node list from the current expansion state.
    pub(crate) fn rebuild(&mut self) {
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
            self.push_children(&root, 1, &[], &mut out);
        }
        self.nodes = out;
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

    /// Build the buffer text and line→node map for the current tree state.
    ///
    /// Each line in the returned `String` corresponds to `nodes[i]`, so
    /// `cursor_row` in the editor maps directly to `nodes[cursor_row]`.
    pub(crate) fn render_text(&self) -> String {
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
                let icon = explorer_dir_icon(&node.path, self.is_expanded(&node.path));
                out.push(icon);
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
                    explorer_dir_icon(&node.path, self.is_expanded(&node.path))
                } else {
                    explorer_file_icon(&node.path)
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

        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let tree = ExplorerTree::new(cwd);
        let text = tree.render_text();
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
    fn explorer_rebuild_buffer(&mut self) {
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
    fn nearest_non_explorer_window(&self) -> Option<super::window::WindowId> {
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
}
