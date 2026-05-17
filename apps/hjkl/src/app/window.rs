//! Window-tree data model for vim-style splits (Phase 1 + Phase 2).
//!
//! A [`LayoutTree`] holds either a single [`Leaf`] (one window) or a
//! [`Split`] that recursively divides space between two sub-trees.  The
//! [`Window`] struct carries per-window scroll state so that two windows
//! showing the same buffer slot can scroll independently.

/// Stable id into `App::windows`. Never reused — new windows get the next
/// value from `App::next_window_id`.
pub type WindowId = usize;

/// Per-tab layout + focus state.
///
/// Each tab owns one [`LayoutTree`] (the window arrangement within that tab)
/// and records which window in that tree currently has focus.  Windows and
/// slots are shared across tabs — a `WindowId` refers into `App::windows`
/// regardless of which tab it lives in.
#[derive(Debug, Clone)]
pub struct Tab {
    /// Spatial layout tree for this tab. Leaves reference [`WindowId`]s.
    pub layout: LayoutTree,
    /// The window that has focus within this tab.
    pub focused_window: WindowId,
}

/// Per-window scroll + geometry state.
#[derive(Debug, Clone)]
pub struct Window {
    /// Index into `App::slots` for the buffer this window displays.
    pub slot: usize,
    /// Per-window top scroll row. Synced into the editor host viewport
    /// before dispatch, and back out after, for the focused window only.
    pub top_row: usize,
    /// Per-window top scroll column (char index).
    pub top_col: usize,
    /// Per-window cursor row (0-based). Synced alongside scroll so two
    /// windows on the same slot keep independent cursors.
    pub cursor_row: usize,
    /// Per-window cursor column (0-based).
    pub cursor_col: usize,
    /// The rect this window occupied in the last rendered frame.  Written
    /// by the renderer every frame; used by direction-navigation in later
    /// phases.  `None` until the first render.
    pub last_rect: Option<ratatui::layout::Rect>,
}

/// Direction of a split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDir {
    Horizontal,
    Vertical,
}

/// A binary spatial tree that partitions the editor area into windows.
#[derive(Debug, Clone)]
pub enum LayoutTree {
    Leaf(WindowId),
    Split {
        dir: SplitDir,
        /// Fraction of the available space allocated to `a`. `0.0 < ratio < 1.0`.
        ratio: f32,
        a: Box<LayoutTree>,
        b: Box<LayoutTree>,
        /// Rect this split last occupied. Filled by render_layout each frame;
        /// read by resize commands to convert line/col deltas to ratio updates.
        /// None before the first render.
        last_rect: Option<ratatui::layout::Rect>,
    },
}

/// Internal direction enum used by `neighbor_direction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NavDir {
    Below,
    Above,
    Left,
    Right,
}

impl LayoutTree {
    /// Pre-order traversal — returns all leaf ids in the order they appear
    /// top-to-bottom / left-to-right in the layout.
    pub fn leaves(&self) -> Vec<WindowId> {
        let mut out = Vec::new();
        self.collect_leaves(&mut out);
        out
    }

    fn collect_leaves(&self, out: &mut Vec<WindowId>) {
        match self {
            LayoutTree::Leaf(id) => out.push(*id),
            LayoutTree::Split { a, b, .. } => {
                a.collect_leaves(out);
                b.collect_leaves(out);
            }
        }
    }

    /// Return the next leaf in pre-order traversal, wrapping around.
    ///
    /// Returns `None` only if `id` is not in the tree (shouldn't happen in
    /// practice).
    pub fn next_leaf(&self, id: WindowId) -> Option<WindowId> {
        let leaves = self.leaves();
        let pos = leaves.iter().position(|&l| l == id)?;
        Some(leaves[(pos + 1) % leaves.len()])
    }

    /// Return the previous leaf in pre-order traversal, wrapping around.
    ///
    /// Returns `None` only if `id` is not in the tree (shouldn't happen in
    /// practice).
    pub fn prev_leaf(&self, id: WindowId) -> Option<WindowId> {
        let leaves = self.leaves();
        let pos = leaves.iter().position(|&l| l == id)?;
        let len = leaves.len();
        Some(leaves[(pos + len - 1) % len])
    }

    /// Return `true` if `id` appears anywhere in the tree.
    pub fn contains(&self, id: WindowId) -> bool {
        match self {
            LayoutTree::Leaf(leaf_id) => *leaf_id == id,
            LayoutTree::Split { a, b, .. } => a.contains(id) || b.contains(id),
        }
    }

    /// Find the leaf for `id` and replace it in-place with `f(id)`.
    /// Returns `true` if the leaf was found and replaced.
    pub fn replace_leaf<F: FnOnce(WindowId) -> LayoutTree + 'static>(
        &mut self,
        id: WindowId,
        f: F,
    ) -> bool {
        self.replace_leaf_boxed(id, Box::new(f))
    }

    fn replace_leaf_boxed(
        &mut self,
        id: WindowId,
        f: Box<dyn FnOnce(WindowId) -> LayoutTree>,
    ) -> bool {
        match self {
            LayoutTree::Leaf(leaf_id) if *leaf_id == id => {
                *self = f(id);
                true
            }
            LayoutTree::Leaf(_) => false,
            LayoutTree::Split { a, b, .. } => {
                // We need to check `a` first; if not found, check `b`.
                // Because `Box<dyn FnOnce>` is not `Copy`, we do this by
                // checking containment first, then calling.
                if a.contains(id) {
                    a.replace_leaf_boxed(id, f)
                } else {
                    b.replace_leaf_boxed(id, f)
                }
            }
        }
    }

    /// Return the id of the next leaf below `id` in a `Horizontal` split,
    /// using pre-order traversal semantics.
    ///
    /// "Below" means: walking up from `id`, find the innermost enclosing
    /// `Horizontal` split where `id` lives in `a`; the answer is then the
    /// first (leftmost) leaf of `b`.  If `id` is the bottom-most window
    /// (or there are no horizontal splits above it), returns `None`.
    pub fn neighbor_below(&self, id: WindowId) -> Option<WindowId> {
        self.neighbor_direction(id, NavDir::Below)
    }

    /// Return the id of the next leaf above `id` in a `Horizontal` split.
    pub fn neighbor_above(&self, id: WindowId) -> Option<WindowId> {
        self.neighbor_direction(id, NavDir::Above)
    }

    /// Return the id of the next leaf to the left of `id` in a `Vertical`
    /// split.  Horizontal splits are passed through.
    pub fn neighbor_left(&self, id: WindowId) -> Option<WindowId> {
        self.neighbor_direction(id, NavDir::Left)
    }

    /// Return the id of the next leaf to the right of `id` in a `Vertical`
    /// split.  Horizontal splits are passed through.
    pub fn neighbor_right(&self, id: WindowId) -> Option<WindowId> {
        self.neighbor_direction(id, NavDir::Right)
    }

    /// Internal unified helper for directional navigation.
    ///
    /// - `Below` / `Above` act on `Horizontal` splits; `Vertical` is a pass-through.
    /// - `Left` / `Right` act on `Vertical` splits; `Horizontal` is a pass-through.
    ///
    /// In each "active" split direction:
    /// - For the "forward" direction (Below / Right), when `id` is in `a`:
    ///   try to find a deeper neighbour inside `a` first; failing that, cross to `b`.
    ///   When `id` is in `b`: recurse into `b` only (no cross available).
    /// - For the "backward" direction (Above / Left), symmetric.
    fn neighbor_direction(&self, id: WindowId, dir: NavDir) -> Option<WindowId> {
        match self {
            LayoutTree::Leaf(_) => None,
            LayoutTree::Split {
                dir: split_dir,
                a,
                b,
                ..
            } => {
                // Which split direction is "active" for this nav direction?
                let active_split = match dir {
                    NavDir::Below | NavDir::Above => SplitDir::Horizontal,
                    NavDir::Left | NavDir::Right => SplitDir::Vertical,
                };
                // Is this a "forward" traversal (a→b) or "backward" (b→a)?
                let forward = matches!(dir, NavDir::Below | NavDir::Right);

                if *split_dir == active_split {
                    if a.contains(id) {
                        if forward {
                            // Try deeper forward-neighbour inside `a`.
                            let inner = a.neighbor_direction(id, dir);
                            if inner.is_some() {
                                return inner;
                            }
                            // Cross to `b`.
                            Some(first_leaf(b))
                        } else {
                            // Backward, `id` in `a` (the "first" half) — recurse only.
                            a.neighbor_direction(id, dir)
                        }
                    } else if b.contains(id) {
                        if forward {
                            // Forward, `id` in `b` (the "second" half) — recurse only.
                            b.neighbor_direction(id, dir)
                        } else {
                            // Try deeper backward-neighbour inside `b`.
                            let inner = b.neighbor_direction(id, dir);
                            if inner.is_some() {
                                return inner;
                            }
                            // Cross to `a`.
                            Some(last_leaf(a))
                        }
                    } else {
                        None
                    }
                } else {
                    // Pass-through: this split axis is orthogonal — recurse without offering a sibling.
                    if a.contains(id) {
                        a.neighbor_direction(id, dir)
                    } else if b.contains(id) {
                        b.neighbor_direction(id, dir)
                    } else {
                        None
                    }
                }
            }
        }
    }

    /// Walk the tree looking for the innermost enclosing Split with matching
    /// `dir` that contains `id`. Returns a mutable reference to the ratio,
    /// a copy of the last_rect, and whether the focused leaf is in `a`.
    /// Returns None if no such enclosing Split exists.
    pub fn enclosing_split_mut(
        &mut self,
        id: WindowId,
        dir: SplitDir,
    ) -> Option<(&mut f32, Option<ratatui::layout::Rect>, bool)> {
        match self {
            LayoutTree::Leaf(_) => None,
            LayoutTree::Split {
                dir: my_dir,
                ratio,
                a,
                b,
                last_rect,
            } => {
                let in_a = a.contains(id);
                let in_b = b.contains(id);
                if !in_a && !in_b {
                    return None;
                }

                let my_dir = *my_dir;
                let saved_rect = *last_rect;

                // Try deeper first (innermost wins).
                let inner = if in_a {
                    a.enclosing_split_mut(id, dir)
                } else {
                    b.enclosing_split_mut(id, dir)
                };
                if inner.is_some() {
                    return inner;
                }

                // No deeper match — am I a candidate?
                if my_dir == dir {
                    Some((ratio, saved_rect, in_a))
                } else {
                    None
                }
            }
        }
    }

    /// Reset all splits in the tree to 0.5 ratio.
    pub fn equalize_all(&mut self) {
        if let LayoutTree::Split { ratio, a, b, .. } = self {
            *ratio = 0.5;
            a.equalize_all();
            b.equalize_all();
        }
    }

    /// For each enclosing Split on the path from root to leaf `id`, invoke
    /// `f` with the split's mutable state. Order: outermost first.
    pub fn for_each_ancestor<F>(&mut self, id: WindowId, f: &mut F)
    where
        F: FnMut(SplitDir, &mut f32, bool, Option<ratatui::layout::Rect>),
    {
        if let LayoutTree::Split {
            dir,
            ratio,
            a,
            b,
            last_rect,
        } = self
        {
            let in_a = a.contains(id);
            let in_b = b.contains(id);
            if !in_a && !in_b {
                return;
            }
            // Outermost first: call f on this node before recursing.
            f(*dir, ratio, in_a, *last_rect);
            if in_a {
                a.for_each_ancestor(id, f);
            } else {
                b.for_each_ancestor(id, f);
            }
        }
    }

    /// Swap the two children of the deepest Split that directly contains
    /// `Leaf(id)` as one of its `a` or `b` children.
    ///
    /// Returns `true` if the swap was applied (i.e. there is an enclosing
    /// Split — `false` when `id` is the only window).
    pub fn swap_with_sibling(&mut self, id: WindowId) -> bool {
        match self {
            LayoutTree::Leaf(_) => false,
            LayoutTree::Split { a, b, .. } => {
                let a_is_focused_leaf = matches!(a.as_ref(), LayoutTree::Leaf(leaf) if *leaf == id);
                let b_is_focused_leaf = matches!(b.as_ref(), LayoutTree::Leaf(leaf) if *leaf == id);
                if a_is_focused_leaf || b_is_focused_leaf {
                    std::mem::swap(a, b);
                    return true;
                }
                // Recurse into whichever side contains id.
                if a.contains(id) {
                    return a.swap_with_sibling(id);
                }
                if b.contains(id) {
                    return b.swap_with_sibling(id);
                }
                false
            }
        }
    }

    /// Remove the leaf `id` from the tree.  When its parent `Split` is left
    /// with only the sibling, that split is replaced by the sibling subtree
    /// (collapse).
    ///
    /// Returns the `WindowId` of the leaf that should receive focus after
    /// removal (the sibling that survived the collapse), or `Err` if `id` is
    /// the only remaining leaf.
    pub fn remove_leaf(&mut self, id: WindowId) -> Result<WindowId, &'static str> {
        if matches!(self, LayoutTree::Leaf(_)) {
            return Err("E444: Cannot close last window");
        }
        match self.try_remove_leaf(id) {
            Some(focus) => Ok(focus),
            None => Err("E444: Cannot close last window"),
        }
    }

    /// Recursive helper for `remove_leaf`.  Returns `Some(new_focus)` when
    /// `id` was found and removed (or the caller needs to collapse this node),
    /// `None` when `id` was not in this subtree.
    fn try_remove_leaf(&mut self, id: WindowId) -> Option<WindowId> {
        match self {
            LayoutTree::Leaf(_) => None, // can't remove the only leaf
            LayoutTree::Split { a, b, .. } => {
                // Case 1: `a` is the leaf we want to remove.
                if matches!(a.as_ref(), LayoutTree::Leaf(leaf) if *leaf == id) {
                    let new_focus = first_leaf(b);
                    // Collapse: replace self with b.
                    *self = *b.clone();
                    return Some(new_focus);
                }
                // Case 2: `b` is the leaf we want to remove.
                if matches!(b.as_ref(), LayoutTree::Leaf(leaf) if *leaf == id) {
                    let new_focus = last_leaf(a);
                    // Collapse: replace self with a.
                    *self = *a.clone();
                    return Some(new_focus);
                }
                // Case 3: recurse into `a`.
                if a.contains(id) {
                    return a.try_remove_leaf(id);
                }
                // Case 4: recurse into `b`.
                if b.contains(id) {
                    return b.try_remove_leaf(id);
                }
                None
            }
        }
    }
}

/// First (top / left) leaf in a subtree.
fn first_leaf(tree: &LayoutTree) -> WindowId {
    match tree {
        LayoutTree::Leaf(id) => *id,
        LayoutTree::Split { a, .. } => first_leaf(a),
    }
}

/// Last (bottom / right) leaf in a subtree.
fn last_leaf(tree: &LayoutTree) -> WindowId {
    match tree {
        LayoutTree::Leaf(id) => *id,
        LayoutTree::Split { b, .. } => last_leaf(b),
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tab_tests {
    use super::*;

    #[test]
    fn tab_struct_constructs_with_layout_and_focus() {
        let layout = LayoutTree::Leaf(0);
        let tab = Tab {
            layout,
            focused_window: 0,
        };
        assert_eq!(tab.focused_window, 0);
        assert_eq!(tab.layout.leaves(), vec![0]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(id: WindowId) -> LayoutTree {
        LayoutTree::Leaf(id)
    }

    fn hsplit(ratio: f32, a: LayoutTree, b: LayoutTree) -> LayoutTree {
        LayoutTree::Split {
            dir: SplitDir::Horizontal,
            ratio,
            a: Box::new(a),
            b: Box::new(b),
            last_rect: None,
        }
    }

    fn vsplit(ratio: f32, a: LayoutTree, b: LayoutTree) -> LayoutTree {
        LayoutTree::Split {
            dir: SplitDir::Vertical,
            ratio,
            a: Box::new(a),
            b: Box::new(b),
            last_rect: None,
        }
    }

    /// Build a horizontal split with a pre-filled last_rect for resize tests.
    fn hsplit_with_rect(
        ratio: f32,
        a: LayoutTree,
        b: LayoutTree,
        rect: ratatui::layout::Rect,
    ) -> LayoutTree {
        LayoutTree::Split {
            dir: SplitDir::Horizontal,
            ratio,
            a: Box::new(a),
            b: Box::new(b),
            last_rect: Some(rect),
        }
    }

    /// Build a vertical split with a pre-filled last_rect for resize tests.
    fn vsplit_with_rect(
        ratio: f32,
        a: LayoutTree,
        b: LayoutTree,
        rect: ratatui::layout::Rect,
    ) -> LayoutTree {
        LayoutTree::Split {
            dir: SplitDir::Vertical,
            ratio,
            a: Box::new(a),
            b: Box::new(b),
            last_rect: Some(rect),
        }
    }

    // ── leaves() ─────────────────────────────────────────────────────────────

    #[test]
    fn leaves_single_leaf() {
        let tree = leaf(0);
        assert_eq!(tree.leaves(), vec![0]);
    }

    #[test]
    fn leaves_two_leaf_split() {
        let tree = hsplit(0.5, leaf(0), leaf(1));
        assert_eq!(tree.leaves(), vec![0, 1]);
    }

    #[test]
    fn leaves_nested_horizontal_splits() {
        // 0
        // ──
        // 1
        // ──
        // 2
        let tree = hsplit(0.5, leaf(0), hsplit(0.5, leaf(1), leaf(2)));
        assert_eq!(tree.leaves(), vec![0, 1, 2]);
    }

    #[test]
    fn leaves_nested_left_split() {
        let tree = hsplit(0.5, hsplit(0.5, leaf(0), leaf(1)), leaf(2));
        assert_eq!(tree.leaves(), vec![0, 1, 2]);
    }

    // ── replace_leaf() ───────────────────────────────────────────────────────

    #[test]
    fn replace_leaf_on_single_leaf() {
        let mut tree = leaf(0);
        let replaced = tree.replace_leaf(0, |_| leaf(99));
        assert!(replaced);
        assert_eq!(tree.leaves(), vec![99]);
    }

    #[test]
    fn replace_leaf_in_split_left() {
        let mut tree = hsplit(0.5, leaf(0), leaf(1));
        let replaced = tree.replace_leaf(0, |id| hsplit(0.5, leaf(id + 10), leaf(id)));
        assert!(replaced);
        assert_eq!(tree.leaves(), vec![10, 0, 1]);
    }

    #[test]
    fn replace_leaf_not_found_returns_false() {
        let mut tree = hsplit(0.5, leaf(0), leaf(1));
        let replaced = tree.replace_leaf(99, |_| leaf(99));
        assert!(!replaced);
        assert_eq!(tree.leaves(), vec![0, 1]);
    }

    // ── neighbor_below() / neighbor_above() ──────────────────────────────────

    #[test]
    fn neighbor_below_two_leaf() {
        let tree = hsplit(0.5, leaf(0), leaf(1));
        assert_eq!(tree.neighbor_below(0), Some(1));
        assert_eq!(tree.neighbor_below(1), None);
    }

    #[test]
    fn neighbor_above_two_leaf() {
        let tree = hsplit(0.5, leaf(0), leaf(1));
        assert_eq!(tree.neighbor_above(0), None);
        assert_eq!(tree.neighbor_above(1), Some(0));
    }

    #[test]
    fn neighbor_below_three_leaf_nested_bottom() {
        // 0
        // ─
        // 1
        // ─
        // 2
        let tree = hsplit(0.5, leaf(0), hsplit(0.5, leaf(1), leaf(2)));
        assert_eq!(tree.neighbor_below(0), Some(1));
        assert_eq!(tree.neighbor_below(1), Some(2));
        assert_eq!(tree.neighbor_below(2), None);
    }

    #[test]
    fn neighbor_above_three_leaf_nested_bottom() {
        let tree = hsplit(0.5, leaf(0), hsplit(0.5, leaf(1), leaf(2)));
        assert_eq!(tree.neighbor_above(0), None);
        assert_eq!(tree.neighbor_above(1), Some(0));
        assert_eq!(tree.neighbor_above(2), Some(1));
    }

    #[test]
    fn neighbor_below_three_leaf_nested_top() {
        // 0
        // ─
        // 1
        // ─
        // 2   (layout: (0|1)/2)
        let tree = hsplit(0.5, hsplit(0.5, leaf(0), leaf(1)), leaf(2));
        assert_eq!(tree.neighbor_below(0), Some(1));
        assert_eq!(tree.neighbor_below(1), Some(2));
        assert_eq!(tree.neighbor_below(2), None);
    }

    #[test]
    fn neighbor_above_three_leaf_nested_top() {
        let tree = hsplit(0.5, hsplit(0.5, leaf(0), leaf(1)), leaf(2));
        assert_eq!(tree.neighbor_above(0), None);
        assert_eq!(tree.neighbor_above(1), Some(0));
        assert_eq!(tree.neighbor_above(2), Some(1));
    }

    // ── remove_leaf() ────────────────────────────────────────────────────────

    #[test]
    fn remove_leaf_only_leaf_errors() {
        let mut tree = leaf(0);
        assert!(tree.remove_leaf(0).is_err());
    }

    #[test]
    fn remove_leaf_collapses_parent_keeps_sibling() {
        let mut tree = hsplit(0.5, leaf(0), leaf(1));
        let focus = tree.remove_leaf(0).unwrap();
        assert_eq!(focus, 1);
        assert_eq!(tree.leaves(), vec![1]);
    }

    #[test]
    fn remove_leaf_b_side_collapses_to_a() {
        let mut tree = hsplit(0.5, leaf(0), leaf(1));
        let focus = tree.remove_leaf(1).unwrap();
        assert_eq!(focus, 0);
        assert_eq!(tree.leaves(), vec![0]);
    }

    #[test]
    fn remove_leaf_nested_middle() {
        // 0 / (1 / 2)  → remove 1 → 0 / 2
        let mut tree = hsplit(0.5, leaf(0), hsplit(0.5, leaf(1), leaf(2)));
        let focus = tree.remove_leaf(1).unwrap();
        assert_eq!(focus, 2);
        assert_eq!(tree.leaves(), vec![0, 2]);
    }

    // ── neighbor_left() / neighbor_right() ───────────────────────────────────

    #[test]
    fn neighbor_left_in_vertical_split() {
        // vsplit: a=0 (left), b=1 (right)
        let tree = vsplit(0.5, leaf(0), leaf(1));
        assert_eq!(tree.neighbor_left(0), None);
        assert_eq!(tree.neighbor_left(1), Some(0));
    }

    #[test]
    fn neighbor_right_in_vertical_split() {
        let tree = vsplit(0.5, leaf(0), leaf(1));
        assert_eq!(tree.neighbor_right(0), Some(1));
        assert_eq!(tree.neighbor_right(1), None);
    }

    #[test]
    fn neighbor_left_no_op_in_horizontal_split() {
        // A pure horizontal split has no left/right neighbours.
        let tree = hsplit(0.5, leaf(0), leaf(1));
        assert_eq!(tree.neighbor_left(0), None);
        assert_eq!(tree.neighbor_left(1), None);
        assert_eq!(tree.neighbor_right(0), None);
        assert_eq!(tree.neighbor_right(1), None);
    }

    #[test]
    fn neighbor_left_three_leaf_vertical() {
        // vsplit: 0 | (1 | 2)
        let tree = vsplit(0.5, leaf(0), vsplit(0.5, leaf(1), leaf(2)));
        assert_eq!(tree.neighbor_left(0), None);
        assert_eq!(tree.neighbor_left(1), Some(0));
        assert_eq!(tree.neighbor_left(2), Some(1));
    }

    #[test]
    fn neighbor_right_three_leaf_vertical() {
        let tree = vsplit(0.5, leaf(0), vsplit(0.5, leaf(1), leaf(2)));
        assert_eq!(tree.neighbor_right(0), Some(1));
        assert_eq!(tree.neighbor_right(1), Some(2));
        assert_eq!(tree.neighbor_right(2), None);
    }

    // ── next_leaf() / prev_leaf() ─────────────────────────────────────────────

    #[test]
    fn next_leaf_cycles_through_all_leaves() {
        // 0 | (1 / 2) — mix of vertical and horizontal
        let tree = vsplit(0.5, leaf(0), hsplit(0.5, leaf(1), leaf(2)));
        assert_eq!(tree.next_leaf(0), Some(1));
        assert_eq!(tree.next_leaf(1), Some(2));
        // wraps around
        assert_eq!(tree.next_leaf(2), Some(0));
    }

    #[test]
    fn prev_leaf_wraps_around() {
        let tree = vsplit(0.5, leaf(0), hsplit(0.5, leaf(1), leaf(2)));
        assert_eq!(tree.prev_leaf(0), Some(2));
        assert_eq!(tree.prev_leaf(1), Some(0));
        assert_eq!(tree.prev_leaf(2), Some(1));
    }

    #[test]
    fn next_leaf_single_leaf_wraps_to_self() {
        let tree = leaf(0);
        assert_eq!(tree.next_leaf(0), Some(0));
    }

    #[test]
    fn next_prev_returns_none_for_unknown_id() {
        let tree = vsplit(0.5, leaf(0), leaf(1));
        assert_eq!(tree.next_leaf(99), None);
        assert_eq!(tree.prev_leaf(99), None);
    }

    // ── enclosing_split_mut() ────────────────────────────────────────────────

    #[test]
    fn enclosing_split_mut_returns_innermost() {
        // outer: hsplit 0 / inner: hsplit 1 / 2
        // Querying for id=1 should return the inner split (ratio=0.6), not the outer (ratio=0.4).
        // Pre-fill rects to verify last_rect is propagated correctly.
        let outer_rect = ratatui::layout::Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 40,
        };
        let inner_rect = ratatui::layout::Rect {
            x: 0,
            y: 20,
            width: 80,
            height: 20,
        };
        let mut tree = hsplit_with_rect(
            0.4,
            leaf(0),
            hsplit_with_rect(0.6, leaf(1), leaf(2), inner_rect),
            outer_rect,
        );
        let result = tree.enclosing_split_mut(1, SplitDir::Horizontal);
        assert!(result.is_some(), "should find enclosing horizontal split");
        let (ratio, rect, in_a) = result.unwrap();
        assert!(
            (*ratio - 0.6).abs() < 1e-5,
            "innermost split ratio should be 0.6, got {ratio}"
        );
        assert_eq!(
            rect,
            Some(inner_rect),
            "should return inner rect, not outer"
        );
        assert!(
            in_a,
            "id=1 is in the 'a' (left/top) side of the inner split"
        );
    }

    #[test]
    fn enclosing_split_mut_skips_wrong_dir() {
        // vsplit at root containing a leaf — asking for Horizontal should find nothing.
        let mut tree = vsplit(0.5, leaf(0), leaf(1));
        let result = tree.enclosing_split_mut(0, SplitDir::Horizontal);
        assert!(
            result.is_none(),
            "should not match a Vertical split for Horizontal dir"
        );
    }

    #[test]
    fn enclosing_split_mut_returns_none_for_only_leaf() {
        let mut tree = leaf(0);
        let result = tree.enclosing_split_mut(0, SplitDir::Horizontal);
        assert!(result.is_none(), "single leaf has no enclosing split");
    }

    #[test]
    fn equalize_all_resets_nested_splits_to_half() {
        // Two nested hsplits with non-0.5 ratios — equalize must reset both.
        let mut tree = hsplit(0.3, leaf(0), hsplit(0.7, leaf(1), leaf(2)));
        tree.equalize_all();
        // Walk and verify every split is now 0.5.
        fn check_all_half(t: &LayoutTree) {
            if let LayoutTree::Split { ratio, a, b, .. } = t {
                assert!(
                    (ratio - 0.5).abs() < 1e-5,
                    "ratio should be 0.5, got {ratio}"
                );
                check_all_half(a);
                check_all_half(b);
            }
        }
        check_all_half(&tree);
    }

    #[test]
    fn for_each_ancestor_visits_outermost_first() {
        // outer vsplit (ratio=0.3) containing inner hsplit (ratio=0.7) at leaf 1 / leaf 2.
        // Asking from leaf 1 — should visit outer vsplit, then inner hsplit, in that order.
        let outer_rect = ratatui::layout::Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        let inner_rect = ratatui::layout::Rect {
            x: 24,
            y: 0,
            width: 56,
            height: 24,
        };
        let mut tree = vsplit_with_rect(
            0.3,
            leaf(0),
            hsplit_with_rect(0.7, leaf(1), leaf(2), inner_rect),
            outer_rect,
        );
        let mut visited_dirs: Vec<SplitDir> = Vec::new();
        let mut visited_ratios: Vec<f32> = Vec::new();
        tree.for_each_ancestor(1, &mut |dir, ratio, _in_a, _rect| {
            visited_dirs.push(dir);
            visited_ratios.push(*ratio);
        });
        assert_eq!(
            visited_dirs,
            vec![SplitDir::Vertical, SplitDir::Horizontal],
            "outermost (Vertical) should be visited first"
        );
        assert!(
            (visited_ratios[0] - 0.3).abs() < 1e-5,
            "outer ratio should be 0.3"
        );
        assert!(
            (visited_ratios[1] - 0.7).abs() < 1e-5,
            "inner ratio should be 0.7"
        );
    }

    // ── swap_with_sibling() ───────────────────────────────────────────────────

    #[test]
    fn swap_with_sibling_swaps_two_leaves() {
        // hsplit: a=0 (top), b=1 (bottom). Swap from 0 — should produce a=1, b=0.
        let mut tree = hsplit(0.5, leaf(0), leaf(1));
        let swapped = tree.swap_with_sibling(0);
        assert!(swapped, "swap should succeed in a two-leaf split");
        assert_eq!(tree.leaves(), vec![1, 0], "leaves should be swapped");
    }

    #[test]
    fn swap_with_sibling_in_nested_split_swaps_at_focused_parent() {
        // Layout: hsplit( leaf(0), vsplit( leaf(1), leaf(2) ) )
        // Focused = 1. Its direct parent is the inner vsplit.
        // Swap should swap 1 and 2 within the vsplit, not the outer hsplit.
        let mut tree = hsplit(0.5, leaf(0), vsplit(0.5, leaf(1), leaf(2)));
        let swapped = tree.swap_with_sibling(1);
        assert!(swapped, "swap should succeed");
        // Pre-order after: 0, then inner vsplit now has a=2, b=1 → [0, 2, 1]
        assert_eq!(
            tree.leaves(),
            vec![0, 2, 1],
            "inner leaves should be swapped"
        );
    }

    #[test]
    fn swap_with_sibling_returns_false_for_only_leaf() {
        let mut tree = leaf(0);
        let swapped = tree.swap_with_sibling(0);
        assert!(!swapped, "single leaf has no sibling to swap with");
    }

    // ── mixed_layout_navigation ───────────────────────────────────────────────

    #[test]
    fn mixed_layout_navigation() {
        // Layout:
        //   Horizontal split:
        //     a = Vertical split { A=0, B=1 }   (top row, two columns)
        //     b = Leaf(2)                         (bottom row, full width)
        //
        // Visual:
        //   ┌───┬───┐
        //   │ 0 │ 1 │
        //   ├───┴───┤
        //   │   2   │
        //   └───────┘
        let tree = hsplit(0.5, vsplit(0.5, leaf(0), leaf(1)), leaf(2));

        // Left/right within the top vsplit.
        assert_eq!(tree.neighbor_right(0), Some(1));
        assert_eq!(tree.neighbor_left(1), Some(0));

        // No right neighbour for 1 (rightmost in vsplit, hsplit passthrough).
        assert_eq!(tree.neighbor_right(1), None);
        // No left neighbour for 0 (leftmost in vsplit).
        assert_eq!(tree.neighbor_left(0), None);

        // Above/below across the horizontal split.
        // 0 and 1 are both in `a`; below from either reaches 2.
        assert_eq!(tree.neighbor_below(0), Some(2));
        assert_eq!(tree.neighbor_below(1), Some(2));
        assert_eq!(tree.neighbor_below(2), None);
        assert_eq!(tree.neighbor_above(2), Some(1)); // last_leaf of `a` = last_leaf(vsplit(0,1)) = 1
        assert_eq!(tree.neighbor_above(0), None);
        assert_eq!(tree.neighbor_above(1), None);

        // Cycle: pre-order is 0, 1, 2.
        assert_eq!(tree.next_leaf(0), Some(1));
        assert_eq!(tree.next_leaf(1), Some(2));
        assert_eq!(tree.next_leaf(2), Some(0));
        assert_eq!(tree.prev_leaf(0), Some(2));
        assert_eq!(tree.prev_leaf(2), Some(1));
    }
}

// ── App window-action dispatcher ──────────────────────────────────────────────

use super::App;

impl App {
    /// Dispatch a window-management [`crate::keymap_actions::AppAction`].
    ///
    /// Handles variants:
    ///   - FocusLeft / FocusBelow / FocusAbove / FocusRight
    ///   - FocusNext / FocusPrev
    ///   - CloseFocusedWindow / OnlyFocusedWindow
    ///   - SwapWithSibling / MoveWindowToNewTab
    ///   - NewSplit
    ///   - ResizeHeight / ResizeWidth
    ///   - EqualizeLayout / MaximizeHeight / MaximizeWidth
    ///   - TmuxNavigate (focus neighbour or fall through to tmux select-pane)
    pub(crate) fn dispatch_window_action(
        &mut self,
        action: crate::keymap_actions::AppAction,
        count: usize,
    ) {
        use crate::keymap_actions::AppAction;
        match action {
            AppAction::FocusLeft => self.focus_left(),
            AppAction::FocusBelow => self.focus_below(),
            AppAction::FocusAbove => self.focus_above(),
            AppAction::FocusRight => self.focus_right(),
            AppAction::FocusNext => self.focus_next(),
            AppAction::FocusPrev => self.focus_previous(),
            AppAction::CloseFocusedWindow => self.close_focused_window(),
            AppAction::OnlyFocusedWindow => self.only_focused_window(),
            AppAction::SwapWithSibling => self.swap_with_sibling(),
            AppAction::MoveWindowToNewTab => match self.move_window_to_new_tab() {
                Ok(()) => self.status_message = Some("moved window to new tab".into()),
                Err(msg) => self.status_message = Some(msg.to_string()),
            },
            AppAction::NewSplit => self.dispatch_ex("new"),
            AppAction::ResizeHeight(delta) => self.resize_height(delta * count as i32),
            AppAction::ResizeWidth(delta) => self.resize_width(delta * count as i32),
            AppAction::EqualizeLayout => self.equalize_layout(),
            AppAction::MaximizeHeight => self.maximize_height(),
            AppAction::MaximizeWidth => self.maximize_width(),
            AppAction::TmuxNavigate(dir) => self.dispatch_tmux_navigate(dir),
            _ => {}
        }
    }

    /// `<C-h/j/k/l>` — focus the neighbour window or fall through to tmux.
    ///
    /// When a window neighbour exists in `dir`, focuses it. When no neighbour
    /// exists and `$TMUX` is set, forwards to `tmux select-pane`.
    pub(crate) fn dispatch_tmux_navigate(&mut self, dir: super::NavDir) {
        use super::NavDir;
        let focused = self.focused_window();
        let neighbour = match dir {
            NavDir::Left => self.layout().neighbor_left(focused),
            NavDir::Down => self.layout().neighbor_below(focused),
            NavDir::Up => self.layout().neighbor_above(focused),
            NavDir::Right => self.layout().neighbor_right(focused),
        };
        if neighbour.is_some() {
            match dir {
                NavDir::Left => self.focus_left(),
                NavDir::Down => self.focus_below(),
                NavDir::Up => self.focus_above(),
                NavDir::Right => self.focus_right(),
            }
        } else if std::env::var("TMUX").is_ok() {
            let flag = match dir {
                NavDir::Left => "-L",
                NavDir::Down => "-D",
                NavDir::Up => "-U",
                NavDir::Right => "-R",
            };
            let _ = std::process::Command::new("tmux")
                .args(["select-pane", flag])
                .status();
        }
    }

    // ── Window focus navigation ───────────────────────────────────────────

    /// Move focus to the window below the current one (`Ctrl-w j`).
    pub fn focus_below(&mut self) {
        let fw = self.focused_window();
        if let Some(target) = self.layout().neighbor_below(fw) {
            self.sync_viewport_from_editor();
            self.set_focused_window(target);
            self.sync_viewport_to_editor();
        }
    }

    /// Move focus to the window above the current one (`Ctrl-w k`).
    pub fn focus_above(&mut self) {
        let fw = self.focused_window();
        if let Some(target) = self.layout().neighbor_above(fw) {
            self.sync_viewport_from_editor();
            self.set_focused_window(target);
            self.sync_viewport_to_editor();
        }
    }

    /// Move focus to the window left of the current one (`Ctrl-w h`).
    pub fn focus_left(&mut self) {
        let fw = self.focused_window();
        if let Some(target) = self.layout().neighbor_left(fw) {
            self.sync_viewport_from_editor();
            self.set_focused_window(target);
            self.sync_viewport_to_editor();
        }
    }

    /// Move focus to the window right of the current one (`Ctrl-w l`).
    pub fn focus_right(&mut self) {
        let fw = self.focused_window();
        if let Some(target) = self.layout().neighbor_right(fw) {
            self.sync_viewport_from_editor();
            self.set_focused_window(target);
            self.sync_viewport_to_editor();
        }
    }

    /// Move focus to the next window in pre-order traversal, wrapping around (`Ctrl-w w`).
    pub fn focus_next(&mut self) {
        let fw = self.focused_window();
        if let Some(target) = self.layout().next_leaf(fw) {
            self.sync_viewport_from_editor();
            self.set_focused_window(target);
            self.sync_viewport_to_editor();
        }
    }

    /// Move focus to the previous window in pre-order traversal, wrapping around (`Ctrl-w W`).
    pub fn focus_previous(&mut self) {
        let fw = self.focused_window();
        if let Some(target) = self.layout().prev_leaf(fw) {
            self.sync_viewport_from_editor();
            self.set_focused_window(target);
            self.sync_viewport_to_editor();
        }
    }

    /// Close all windows except the focused one. Replaces the layout with a
    /// single leaf and drops the `Option<Window>` entries for all other windows.
    pub fn only_focused_window(&mut self) {
        let focused = self.focused_window();
        let all_leaves = self.layout().leaves();
        for id in all_leaves {
            if id != focused {
                self.windows[id] = None;
            }
        }
        *self.layout_mut() = LayoutTree::Leaf(focused);
        self.status_message = Some("only".into());
    }

    /// Swap the focused leaf with its sibling in the immediately enclosing
    /// Split. No-op (with no message) when the focused window is the only one.
    pub fn swap_with_sibling(&mut self) {
        let focused = self.focused_window();
        if self.layout_mut().swap_with_sibling(focused) {
            self.status_message = Some("swap".into());
        }
    }

    /// Move the focused window to a new tab (`Ctrl-w T`).
    ///
    /// Fails if the current tab has only one window (vim's "E1: at last window").
    /// On success: the window is removed from the current tab's layout (the
    /// previous tab gets focus on its new top leaf), and a new tab is appended
    /// containing only the moved window.
    pub fn move_window_to_new_tab(&mut self) -> Result<(), &'static str> {
        let focused = self.focused_window();
        if self.layout().leaves().len() <= 1 {
            return Err("E1: only one window in this tab");
        }
        self.sync_viewport_from_editor();
        // Remove the focused leaf from the current tab's layout. The returned
        // value is the leaf that should receive focus in the current tab.
        let new_focus_in_old_tab = self
            .layout_mut()
            .remove_leaf(focused)
            .map_err(|_| "remove_leaf failed")?;
        // Update the old tab's focused window to the surviving sibling.
        self.tabs[self.active_tab].focused_window = new_focus_in_old_tab;

        // Create a new tab containing only the moved window.
        let new_tab = Tab {
            layout: LayoutTree::Leaf(focused),
            focused_window: focused,
        };
        self.tabs.push(new_tab);
        self.active_tab = self.tabs.len() - 1;
        self.sync_viewport_to_editor();
        Ok(())
    }

    /// Close the focused window.  Fails (with status message) when only one
    /// window remains.  On success the layout collapses and focus moves to the
    /// sibling that took over.
    ///
    /// When the focused window is the command-line window (issue #37), the
    /// transient slot is cleaned up via `close_cmdline_window` instead of the
    /// normal path to avoid leaving orphaned slots.
    pub fn close_focused_window(&mut self) {
        // Cmdline window: delegate to its own cleanup.
        if self.is_cmdline_win_focused() {
            self.close_cmdline_window();
            return;
        }
        let focused = self.focused_window();
        match self.layout_mut().remove_leaf(focused) {
            Err(_) => {
                self.status_message = Some("E444: Cannot close last window".into());
            }
            Ok(new_focus) => {
                self.windows[focused] = None;
                self.set_focused_window(new_focus);
                self.sync_viewport_to_editor();
                self.status_message = Some("window closed".into());
            }
        }
    }

    // ── Window size manipulation ───────────────────────────────────────────

    /// Adjust the focused window's height by `delta` lines. Positive grows,
    /// negative shrinks. Clamps so neither sibling drops below 1 line.
    /// No-op when there is no enclosing Horizontal split or last_rect is None.
    pub fn resize_height(&mut self, delta: i32) {
        let fw = self.focused_window();
        if let Some((ratio, Some(rect), in_a)) = self
            .layout_mut()
            .enclosing_split_mut(fw, SplitDir::Horizontal)
        {
            let parent_h = rect.height as i32;
            if parent_h < 2 {
                return;
            }
            let current_focused_height = if in_a {
                (parent_h as f32 * *ratio) as i32
            } else {
                (parent_h as f32 * (1.0 - *ratio)) as i32
            };
            let new_focused = (current_focused_height + delta).clamp(1, parent_h - 1);
            let new_ratio = if in_a {
                new_focused as f32 / parent_h as f32
            } else {
                (parent_h - new_focused) as f32 / parent_h as f32
            };
            *ratio = new_ratio.clamp(0.01, 0.99);
        }
    }

    /// Adjust the focused window's width by `delta` columns. Positive grows,
    /// negative shrinks. Clamps so neither sibling drops below 1 column.
    /// No-op when there is no enclosing Vertical split or last_rect is None.
    pub fn resize_width(&mut self, delta: i32) {
        let fw = self.focused_window();
        if let Some((ratio, Some(rect), in_a)) = self
            .layout_mut()
            .enclosing_split_mut(fw, SplitDir::Vertical)
        {
            let parent_w = rect.width as i32;
            if parent_w < 2 {
                return;
            }
            let current_focused_width = if in_a {
                (parent_w as f32 * *ratio) as i32
            } else {
                (parent_w as f32 * (1.0 - *ratio)) as i32
            };
            let new_focused = (current_focused_width + delta).clamp(1, parent_w - 1);
            let new_ratio = if in_a {
                new_focused as f32 / parent_w as f32
            } else {
                (parent_w - new_focused) as f32 / parent_w as f32
            };
            *ratio = new_ratio.clamp(0.01, 0.99);
        }
    }

    /// Equalize all splits to 0.5 ratio.
    pub fn equalize_layout(&mut self) {
        self.layout_mut().equalize_all();
    }

    /// Resize the split whose `last_rect` encompasses `split_origin` and
    /// `split_total` so the boundary sits at `split_pos` cells from the
    /// split origin. `split_pos` is clamped to leave at least
    /// `SPLIT_MIN_SIZE_COLS` / `SPLIT_MIN_SIZE_ROWS` on each side.
    ///
    /// Called by the border-drag handler in the event loop (Phase 9).
    /// `orientation` determines whether we're moving a column (VSplit) or
    /// a row (HSplit) boundary.
    pub(crate) fn resize_split_to(
        &mut self,
        orientation: super::mouse::SplitOrientation,
        split_origin: u16,
        split_total: u16,
        split_pos: u16,
    ) {
        let min_size = match orientation {
            super::mouse::SplitOrientation::Vertical => super::SPLIT_MIN_SIZE_COLS,
            super::mouse::SplitOrientation::Horizontal => super::SPLIT_MIN_SIZE_ROWS,
        };

        if split_total < min_size * 2 + 1 {
            return; // too small to resize
        }

        // Clamp split_pos so both children stay at least min_size.
        let clamped = split_pos.clamp(min_size, split_total.saturating_sub(min_size + 1));
        let new_ratio = clamped as f32 / split_total as f32;
        let new_ratio = new_ratio.clamp(0.01, 0.99);

        // Find the matching split node by walking the layout tree and looking
        // for a Split whose last_rect matches the origin + total we recorded
        // when the drag started.
        let dir = match orientation {
            super::mouse::SplitOrientation::Vertical => SplitDir::Vertical,
            super::mouse::SplitOrientation::Horizontal => SplitDir::Horizontal,
        };
        fn update_matching(
            node: &mut LayoutTree,
            dir: SplitDir,
            origin: u16,
            total: u16,
            new_ratio: f32,
        ) {
            if let LayoutTree::Split {
                dir: my_dir,
                ratio,
                a,
                b,
                last_rect,
            } = node
            {
                if *my_dir == dir
                    && let Some(r) = last_rect
                {
                    let (rect_origin, rect_total) = match dir {
                        SplitDir::Vertical => (r.x, r.width),
                        SplitDir::Horizontal => (r.y, r.height),
                    };
                    if rect_origin == origin && rect_total == total {
                        *ratio = new_ratio;
                        return; // found the target; done
                    }
                }
                update_matching(a, dir, origin, total, new_ratio);
                update_matching(b, dir, origin, total, new_ratio);
            }
        }
        update_matching(self.layout_mut(), dir, split_origin, split_total, new_ratio);
    }

    /// Equalize all splits (set every ratio to 0.5). Used by double-click on a
    /// border (Phase 9). Delegates to the existing `equalize_layout`.
    pub(crate) fn equalize_split(&mut self) {
        self.equalize_layout();
    }

    /// Maximize focused window's height — set every enclosing Horizontal
    /// split so the focused branch gets as much height as possible (siblings
    /// collapse to 1 line each).
    pub fn maximize_height(&mut self) {
        let focused = self.focused_window();
        self.layout_mut()
            .for_each_ancestor(focused, &mut |dir, ratio, in_a, rect| {
                if dir != SplitDir::Horizontal {
                    return;
                }
                if let Some(r) = rect {
                    let h = r.height as f32;
                    if h < 2.0 {
                        return;
                    }
                    let max_branch = (h - 1.0) / h;
                    let min_branch = 1.0 / h;
                    *ratio = if in_a { max_branch } else { min_branch };
                }
            });
    }

    /// Maximize focused window's width — set every enclosing Vertical split
    /// so the focused branch gets as much width as possible (siblings collapse
    /// to 1 column each).
    pub fn maximize_width(&mut self) {
        let focused = self.focused_window();
        self.layout_mut()
            .for_each_ancestor(focused, &mut |dir, ratio, in_a, rect| {
                if dir != SplitDir::Vertical {
                    return;
                }
                if let Some(r) = rect {
                    let w = r.width as f32;
                    if w < 2.0 {
                        return;
                    }
                    let max_branch = (w - 1.0) / w;
                    let min_branch = 1.0 / w;
                    *ratio = if in_a { max_branch } else { min_branch };
                }
            });
    }
}
