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
pub(crate) const EXPLORER_WIDTH: u16 = 36;

/// Height of the explorer's title/header box (rows reserved at the top).
pub(crate) const EXPLORER_HEADER_H: u16 = 3;

/// One visible row in the flattened tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExplorerNode {
    pub path: PathBuf,
    /// Nesting depth: the root is depth 0, its children depth 1, …
    pub depth: usize,
    pub is_dir: bool,
    /// True when this node is the last child of its parent (`└╴` vs `├╴`).
    pub is_last: bool,
    /// For each ancestor level above this node (excluding the root): whether that
    /// ancestor has a following sibling, i.e. whether to draw a `│` guide in that
    /// column. Length `depth - 1` for `depth >= 1`; empty for the root.
    pub branches: Vec<bool>,
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
        let mut expanded = HashSet::new();
        // The root starts expanded so its contents show immediately.
        expanded.insert(root.clone());
        let mut s = Self {
            root,
            nodes: Vec::new(),
            expanded,
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

    /// Recursively append `dir`'s expanded children to `out`. `prefix` carries,
    /// for each ancestor guide column, whether to draw a `│` (the ancestor has a
    /// following sibling).
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
                // This node's column shows a `│` for its children iff it has a
                // following sibling.
                child_prefix.push(!is_last);
                self.push_children(&path, depth + 1, &child_prefix, out);
            }
        }
    }

    /// Rebuild the flattened visible-node list (root node + expanded subtree),
    /// keeping the selection on the same path when possible (else clamped).
    pub(crate) fn rebuild(&mut self) {
        let prev_path = self.nodes.get(self.selected).map(|n| n.path.clone());
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

    /// A left-click on explorer pane row `pane_row` (0-based from the pane top,
    /// header included): focus the explorer, select the node under it, and
    /// activate it (toggle a dir / open a file). Clicks on the header or past
    /// the last entry just focus the pane.
    pub(crate) fn explorer_click(&mut self, pane_row: usize) {
        let open_path = {
            let Some(exp) = self.explorer.as_mut() else {
                return;
            };
            exp.set_focused(true);
            let header = EXPLORER_HEADER_H as usize;
            if pane_row < header {
                return; // header click → focus only
            }
            let idx = exp.top() + (pane_row - header);
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

    /// Names of the non-root nodes (the visible tree under the root).
    fn child_names(s: &ExplorerState) -> Vec<String> {
        s.nodes()[1..]
            .iter()
            .map(|n| n.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn root_first_then_dirs_first_then_name() {
        let root = make_tree();
        let s = ExplorerState::open(root.clone());
        // node 0 is the root dir (depth 0, expanded).
        assert_eq!(s.nodes()[0].path, root);
        assert!(s.nodes()[0].is_dir && s.nodes()[0].depth == 0);
        // dirs (a_dir, b_dir) before files (m_file, z_file), each alpha.
        assert_eq!(
            child_names(&s),
            vec!["a_dir", "b_dir", "m_file.txt", "z_file.txt"]
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn expand_inserts_children_at_depth_with_guide() {
        let root = make_tree();
        let mut s = ExplorerState::open(root.clone());
        s.select_index(1); // a_dir
        assert!(s.toggle_selected());
        assert_eq!(
            child_names(&s),
            vec!["a_dir", "inner.txt", "b_dir", "m_file.txt", "z_file.txt"]
        );
        let inner = &s.nodes()[2]; // root, a_dir, inner.txt
        assert_eq!(inner.depth, 2);
        assert!(!inner.is_dir);
        // a_dir is not the last child → its subtree draws one `│` guide column.
        assert_eq!(inner.branches, vec![true]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn last_child_flag() {
        let root = make_tree();
        let s = ExplorerState::open(root.clone());
        assert!(!s.nodes()[1].is_last); // a_dir
        let z = s.nodes().last().unwrap();
        assert_eq!(z.path.file_name().unwrap(), "z_file.txt");
        assert!(z.is_last);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn collapse_removes_subtree() {
        let root = make_tree();
        let mut s = ExplorerState::open(root.clone());
        s.select_index(1); // a_dir
        s.toggle_selected(); // open → inner.txt visible
        assert_eq!(s.nodes().len(), 6); // root + 4 + inner
        s.collapse_or_parent(); // a_dir is an expanded dir → collapse
        assert_eq!(
            child_names(&s),
            vec!["a_dir", "b_dir", "m_file.txt", "z_file.txt"]
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn collapse_or_parent_selects_parent() {
        let root = make_tree();
        let mut s = ExplorerState::open(root.clone());
        s.select_index(1); // a_dir
        s.toggle_selected(); // open
        s.select_index(2); // inner.txt (a file under a_dir)
        s.collapse_or_parent(); // → selects parent a_dir at index 1
        assert_eq!(s.selected_index(), 1);
        assert_eq!(
            s.selected_node().unwrap().path.file_name().unwrap(),
            "a_dir"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn collapse_root_hides_all() {
        let root = make_tree();
        let mut s = ExplorerState::open(root.clone());
        s.select_first(); // root
        s.collapse_or_parent(); // collapse the root
        assert_eq!(s.nodes().len(), 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn navigation_clamps() {
        let root = make_tree();
        let mut s = ExplorerState::open(root.clone());
        s.select_prev(); // at 0 (root) → stays
        assert_eq!(s.selected_index(), 0);
        s.select_last();
        assert_eq!(s.selected_index(), s.nodes().len() - 1);
        s.select_next(); // at last → stays
        assert_eq!(s.selected_index(), s.nodes().len() - 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn expand_is_noop_on_file() {
        let root = make_tree();
        let mut s = ExplorerState::open(root.clone());
        s.select_index(3); // root, a_dir, b_dir, m_file.txt
        assert_eq!(
            s.selected_node().unwrap().path.file_name().unwrap(),
            "m_file.txt"
        );
        assert!(!s.toggle_selected());
        let _ = fs::remove_dir_all(&root);
    }
}
