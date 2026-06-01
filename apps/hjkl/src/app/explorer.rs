//! Left-side file-explorer pane (issue #55) — a Snacks.explorer-style
//! expandable directory tree rooted at the working directory.
//!
//! This module is the pure, render-agnostic data model: it owns the visible
//! node list, expansion state, selection, and navigation. The TUI renderer
//! ([`crate::render`]) and key/mouse routing ([`crate::app::event_loop`]) drive
//! it; nothing here depends on ratatui, so it can be lifted into a standalone
//! `hjkl-explorer` crate later (agnostic / TUI-first pattern).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Default width of the explorer pane in terminal columns.
pub(crate) const EXPLORER_WIDTH: u16 = 30;

/// One visible row in the flattened tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExplorerNode {
    pub path: PathBuf,
    /// Nesting depth from the root (root entries are depth 0).
    pub depth: usize,
    pub is_dir: bool,
}

/// Explorer pane state. `None` on `App` means the pane is closed.
#[derive(Debug, Clone)]
pub(crate) struct ExplorerState {
    /// Directory the tree is rooted at (cwd captured when opened).
    root: PathBuf,
    /// Flattened depth-first list of currently visible rows.
    nodes: Vec<ExplorerNode>,
    /// Directories the user has expanded (absolute paths).
    expanded: HashSet<PathBuf>,
    /// Index into `nodes` of the highlighted row.
    selected: usize,
    /// First visible row (scroll offset); maintained by the renderer.
    top: usize,
    /// Whether the explorer currently owns keyboard input.
    focused: bool,
    /// Pane width in columns.
    width: u16,
    /// First `g` of a `gg` chord seen, awaiting the second.
    pending_g: bool,
}

impl ExplorerState {
    /// Open a fresh explorer rooted at `root` (everything collapsed, first row
    /// selected, focused).
    pub(crate) fn open(root: PathBuf) -> Self {
        let mut s = Self {
            root,
            nodes: Vec::new(),
            expanded: HashSet::new(),
            selected: 0,
            top: 0,
            focused: true,
            width: EXPLORER_WIDTH,
            pending_g: false,
        };
        s.rebuild();
        s
    }

    pub(crate) fn width(&self) -> u16 {
        self.width
    }
    pub(crate) fn focused(&self) -> bool {
        self.focused
    }
    pub(crate) fn set_focused(&mut self, f: bool) {
        self.focused = f;
    }
    pub(crate) fn nodes(&self) -> &[ExplorerNode] {
        &self.nodes
    }
    pub(crate) fn selected_index(&self) -> usize {
        self.selected
    }
    pub(crate) fn top(&self) -> usize {
        self.top
    }
    pub(crate) fn set_top(&mut self, top: usize) {
        self.top = top;
    }
    pub(crate) fn is_expanded(&self, path: &Path) -> bool {
        self.expanded.contains(path)
    }

    pub(crate) fn selected_node(&self) -> Option<&ExplorerNode> {
        self.nodes.get(self.selected)
    }

    /// Read one directory's children, sorted directories-first then by
    /// case-insensitive name. Hidden dotfiles are included (matches a basic
    /// file explorer); errors yield an empty list.
    fn read_children(dir: &Path) -> Vec<(PathBuf, bool)> {
        let mut entries: Vec<(PathBuf, bool)> = match std::fs::read_dir(dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .map(|e| {
                    let p = e.path();
                    // `file_type()` avoids a stat where possible; fall back to is_dir.
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

    /// Recursively append `dir`'s subtree (expanded dirs only) to `out`.
    fn push_subtree(&self, dir: &Path, depth: usize, out: &mut Vec<ExplorerNode>) {
        for (path, is_dir) in Self::read_children(dir) {
            let expanded = is_dir && self.expanded.contains(&path);
            out.push(ExplorerNode {
                path: path.clone(),
                depth,
                is_dir,
            });
            if expanded {
                self.push_subtree(&path, depth + 1, out);
            }
        }
    }

    /// Rebuild the flattened visible-node list from `root` + `expanded`, keeping
    /// the selection on the same path when possible (else clamped).
    pub(crate) fn rebuild(&mut self) {
        let prev_path = self.nodes.get(self.selected).map(|n| n.path.clone());
        let mut out = Vec::new();
        let root = self.root.clone();
        self.push_subtree(&root, 0, &mut out);
        self.nodes = out;
        self.selected = match prev_path {
            Some(p) => self.nodes.iter().position(|n| n.path == p).unwrap_or(0),
            None => 0,
        };
        self.clamp_selection();
    }

    fn clamp_selection(&mut self) {
        if self.nodes.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.nodes.len() {
            self.selected = self.nodes.len() - 1;
        }
    }

    pub(crate) fn select_next(&mut self) {
        if !self.nodes.is_empty() && self.selected + 1 < self.nodes.len() {
            self.selected += 1;
        }
    }
    pub(crate) fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }
    pub(crate) fn select_first(&mut self) {
        self.selected = 0;
    }
    pub(crate) fn select_last(&mut self) {
        if !self.nodes.is_empty() {
            self.selected = self.nodes.len() - 1;
        }
    }

    /// Move the selection to `idx` (clamped). Used by mouse clicks.
    pub(crate) fn select_index(&mut self, idx: usize) {
        self.selected = idx;
        self.clamp_selection();
    }

    /// Toggle the selected directory's expansion (no-op for files). Returns
    /// true if the tree changed.
    pub(crate) fn toggle_selected(&mut self) -> bool {
        let Some(node) = self.nodes.get(self.selected) else {
            return false;
        };
        if !node.is_dir {
            return false;
        }
        let p = node.path.clone();
        if self.expanded.contains(&p) {
            self.expanded.remove(&p);
        } else {
            self.expanded.insert(p);
        }
        self.rebuild();
        true
    }

    /// Record a `g` toward a `gg` chord; returns true when it completes one.
    pub(crate) fn note_g(&mut self) -> bool {
        if self.pending_g {
            self.pending_g = false;
            true
        } else {
            self.pending_g = true;
            false
        }
    }
    pub(crate) fn clear_g(&mut self) {
        self.pending_g = false;
    }

    /// `h` behavior: if the selected node is an expanded dir, collapse it;
    /// otherwise move the selection to its parent directory row (if visible).
    pub(crate) fn collapse_or_parent(&mut self) {
        let Some(node) = self.nodes.get(self.selected).cloned() else {
            return;
        };
        if node.is_dir && self.expanded.contains(&node.path) {
            self.expanded.remove(&node.path);
            self.rebuild();
            return;
        }
        // Select the parent row: nearest preceding node with depth-1.
        if node.depth == 0 {
            return;
        }
        let target_depth = node.depth - 1;
        for i in (0..self.selected).rev() {
            if self.nodes[i].depth == target_depth {
                self.selected = i;
                return;
            }
        }
    }
}

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

impl super::App {
    /// True when the explorer is open AND owns keyboard input.
    pub(crate) fn explorer_focused(&self) -> bool {
        self.explorer.as_ref().is_some_and(|e| e.focused())
    }

    /// `<leader>e`: closed → open + focus; open & unfocused → focus; open &
    /// focused → close. A predictable 3-state toggle that stays refocusable
    /// from the keyboard.
    pub(crate) fn toggle_explorer(&mut self) {
        match self.explorer.as_mut() {
            None => {
                let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                self.explorer = Some(ExplorerState::open(cwd));
            }
            Some(e) if !e.focused() => e.set_focused(true),
            Some(_) => self.explorer = None,
        }
    }

    /// A left-click on explorer pane row `pane_row` (0-based from the pane top):
    /// focus the explorer, select that node, and activate it (toggle a dir /
    /// open a file). Clicks past the last entry just focus the pane.
    pub(crate) fn explorer_click(&mut self, pane_row: usize) {
        let open_path = {
            let Some(exp) = self.explorer.as_mut() else {
                return;
            };
            exp.set_focused(true);
            let idx = exp.top() + pane_row;
            if idx >= exp.nodes().len() {
                return;
            }
            exp.select_index(idx);
            match exp.selected_node().map(|n| (n.is_dir, n.path.clone())) {
                Some((true, _)) => {
                    exp.toggle_selected();
                    None
                }
                Some((false, p)) => Some(p),
                None => None,
            }
        };
        if let Some(p) = open_path {
            if let Some(e) = self.explorer.as_mut() {
                e.set_focused(false);
            }
            let s = p.to_string_lossy().to_string();
            self.dispatch_ex(&format!("edit {s}"));
        }
    }

    /// Route a keypress while the explorer owns input. Returns `true` when the
    /// key was consumed (caller should not pass it to the editor).
    pub(crate) fn explorer_handle_key(&mut self, key: KeyEvent) -> bool {
        let open_path = {
            let Some(exp) = self.explorer.as_mut() else {
                return false;
            };
            if !exp.focused() {
                return false;
            }
            // Leave Ctrl-/Alt- chords (e.g. window nav) to the normal router.
            if key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
            {
                exp.clear_g();
                return false;
            }
            // `gg` → top.
            if let KeyCode::Char('g') = key.code {
                if exp.note_g() {
                    exp.select_first();
                }
                return true;
            }

            let mut open_path: Option<std::path::PathBuf> = None;
            match key.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    exp.clear_g();
                    exp.select_next();
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    exp.clear_g();
                    exp.select_prev();
                }
                KeyCode::Char('G') => {
                    exp.clear_g();
                    exp.select_last();
                }
                KeyCode::Char('h') | KeyCode::Left => {
                    exp.clear_g();
                    exp.collapse_or_parent();
                }
                KeyCode::Char('l') | KeyCode::Right | KeyCode::Enter => {
                    exp.clear_g();
                    match exp.selected_node().map(|n| (n.is_dir, n.path.clone())) {
                        Some((true, _)) => {
                            exp.toggle_selected();
                        }
                        Some((false, p)) => open_path = Some(p),
                        None => {}
                    }
                }
                KeyCode::Esc => {
                    exp.clear_g();
                    exp.set_focused(false);
                }
                _ => {
                    exp.clear_g();
                    return false;
                }
            }
            open_path
        };

        // Open a file: return focus to the editor, then load via `:edit` (reuses
        // buffer reuse / filetype / LSP attach — same path the picker uses).
        if let Some(p) = open_path {
            if let Some(e) = self.explorer.as_mut() {
                e.set_focused(false);
            }
            let s = p.to_string_lossy().to_string();
            self.dispatch_ex(&format!("edit {s}"));
        }
        true
    }
}

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

    fn names(s: &ExplorerState) -> Vec<String> {
        s.nodes()
            .iter()
            .map(|n| n.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn dirs_first_then_name() {
        let root = make_tree();
        let s = ExplorerState::open(root.clone());
        // dirs (a_dir, b_dir) before files (m_file, z_file), each alpha.
        assert_eq!(
            names(&s),
            vec!["a_dir", "b_dir", "m_file.txt", "z_file.txt"]
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn expand_inserts_children_at_depth() {
        let root = make_tree();
        let mut s = ExplorerState::open(root.clone());
        // select a_dir (index 0) and expand it.
        assert!(s.toggle_selected());
        assert_eq!(
            names(&s),
            vec!["a_dir", "inner.txt", "b_dir", "m_file.txt", "z_file.txt"]
        );
        let inner = &s.nodes()[1];
        assert_eq!(inner.depth, 1);
        assert!(!inner.is_dir);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn collapse_removes_subtree() {
        let root = make_tree();
        let mut s = ExplorerState::open(root.clone());
        s.toggle_selected(); // a_dir open → inner.txt visible
        assert_eq!(s.nodes().len(), 5);
        // selection is on a_dir (an expanded dir) → h collapses it.
        s.collapse_or_parent();
        assert_eq!(
            names(&s),
            vec!["a_dir", "b_dir", "m_file.txt", "z_file.txt"]
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn collapse_or_parent_selects_parent() {
        let root = make_tree();
        let mut s = ExplorerState::open(root.clone());
        s.toggle_selected(); // a_dir open
        s.select_index(1); // inner.txt (depth 1, a file)
        s.collapse_or_parent(); // → selects parent a_dir (depth 0)
        assert_eq!(s.selected_index(), 0);
        assert_eq!(
            s.selected_node().unwrap().path.file_name().unwrap(),
            "a_dir"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn navigation_clamps() {
        let root = make_tree();
        let mut s = ExplorerState::open(root.clone());
        s.select_prev(); // already at 0 → stays
        assert_eq!(s.selected_index(), 0);
        s.select_last();
        assert_eq!(s.selected_index(), 3);
        s.select_next(); // at last → stays
        assert_eq!(s.selected_index(), 3);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn expand_is_noop_on_file() {
        let root = make_tree();
        let mut s = ExplorerState::open(root.clone());
        s.select_index(2); // m_file.txt
        assert!(!s.toggle_selected());
        let _ = fs::remove_dir_all(&root);
    }
}
