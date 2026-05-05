//! Window-tree data model for vim-style splits (Phase 1).
//!
//! A [`LayoutTree`] holds either a single [`Leaf`] (one window) or a
//! [`Split`] that recursively divides space between two sub-trees.  The
//! [`Window`] struct carries per-window scroll state so that two windows
//! showing the same buffer slot can scroll independently.

/// Stable id into `App::windows`. Never reused — new windows get the next
/// value from `App::next_window_id`.
pub type WindowId = usize;

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
    /// The rect this window occupied in the last rendered frame.  Written
    /// by the renderer every frame; used by direction-navigation in later
    /// phases.  `None` until the first render.
    pub last_rect: Option<ratatui::layout::Rect>,
}

/// Direction of a split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDir {
    Horizontal,
    /// Reserved for Phase 2 vertical splits.
    #[allow(dead_code)]
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
    },
}

impl LayoutTree {
    /// Pre-order traversal — returns all leaf ids in the order they appear
    /// top-to-bottom / left-to-right in the layout.
    #[allow(dead_code)]
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
        self.neighbor_direction(id, true)
    }

    /// Return the id of the next leaf above `id` in a `Horizontal` split.
    pub fn neighbor_above(&self, id: WindowId) -> Option<WindowId> {
        self.neighbor_direction(id, false)
    }

    /// Internal helper — `below=true` searches downward, `below=false` upward.
    ///
    /// Returns `Some(target_id)` if a neighbour was found in the given
    /// direction, `None` otherwise.  The algorithm walks the tree looking for
    /// the innermost `Horizontal` split where `id` is in one branch and there
    /// is a meaningful sibling in the other branch in that direction.
    ///
    /// For "below" when `id` is in `a` (top branch): first recurse into `a`
    /// to find an inner-split neighbour; failing that, cross to `b`.
    /// When `id` is in `b` (bottom branch): recurse into `b` only.
    fn neighbor_direction(&self, id: WindowId, below: bool) -> Option<WindowId> {
        match self {
            LayoutTree::Leaf(_) => None,
            LayoutTree::Split { dir, a, b, .. } => {
                if *dir == SplitDir::Horizontal {
                    if a.contains(id) {
                        if below {
                            // Try to find a deeper below-neighbour inside `a`.
                            let inner = a.neighbor_direction(id, below);
                            if inner.is_some() {
                                return inner;
                            }
                            // No inner found — `b` is the next pane below.
                            Some(first_leaf(b))
                        } else {
                            // Searching above: `id` is in top half → recurse
                            // into `a`; no cross to `b` possible from here.
                            a.neighbor_direction(id, below)
                        }
                    } else if b.contains(id) {
                        if below {
                            // `id` is in bottom half → recurse into `b` only.
                            b.neighbor_direction(id, below)
                        } else {
                            // Try to find a deeper above-neighbour inside `b`.
                            let inner = b.neighbor_direction(id, below);
                            if inner.is_some() {
                                return inner;
                            }
                            // No inner found — `a` is the next pane above.
                            Some(last_leaf(a))
                        }
                    } else {
                        None
                    }
                } else {
                    // Vertical split — pass through without offering a sibling.
                    if a.contains(id) {
                        a.neighbor_direction(id, below)
                    } else if b.contains(id) {
                        b.neighbor_direction(id, below)
                    } else {
                        None
                    }
                }
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
}
