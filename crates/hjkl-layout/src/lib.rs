//! Renderer-agnostic window/split layout machinery for the hjkl editor stack.
//!
//! A [`LayoutTree`] holds either a single [`Leaf`](LayoutTree::Leaf) (one window) or a
//! [`Split`](LayoutTree::Split) that recursively divides space between two sub-trees.
//! All geometry is expressed as [`LayoutRect`] — a plain `u16` rectangle that TUI
//! hosts convert from `ratatui::layout::Rect` and GUI hosts convert from their own
//! coordinate types at the boundary.
//!
//! # Quick start
//!
//! ```rust
//! use hjkl_layout::{LayoutTree, SplitDir, LayoutRect};
//!
//! let mut tree = LayoutTree::Leaf(0);
//! assert_eq!(tree.leaves(), vec![0]);
//!
//! // Split window 0 horizontally — window 1 below window 0.
//! tree.replace_leaf(0, |id| {
//!     LayoutTree::split(
//!         SplitDir::Horizontal,
//!         0.5,
//!         LayoutTree::Leaf(id),
//!         LayoutTree::Leaf(1),
//!     )
//! });
//! assert_eq!(tree.leaves(), vec![0, 1]);
//! assert_eq!(tree.neighbor_below(0), Some(1));
//! assert_eq!(tree.neighbor_below(1), None);
//! ```

/// Stable id into the host window list. Never reused — new windows get the next
/// value from the host's `next_window_id` counter.
pub type WindowId = usize;

/// Renderer-agnostic rectangle used by the layout tree.
///
/// TUI hosts convert `ratatui::layout::Rect` at the boundary; GUI hosts convert
/// from their floating-point coordinate space. All fields are `u16` which matches
/// ratatui's field types directly and is sufficient for terminal geometry (max
/// 65535 columns/rows).
///
/// `#[non_exhaustive]` — additional fields may be added in minor releases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct LayoutRect {
    /// Column offset from the top-left corner of the terminal/window.
    pub x: u16,
    /// Row offset from the top-left corner of the terminal/window.
    pub y: u16,
    /// Width in columns.
    pub w: u16,
    /// Height in rows.
    pub h: u16,
}

impl LayoutRect {
    /// Construct a new [`LayoutRect`].
    pub fn new(x: u16, y: u16, w: u16, h: u16) -> Self {
        Self { x, y, w, h }
    }
}

/// Per-tab layout + focus state.
///
/// Each tab owns one [`LayoutTree`] (the window arrangement within that tab)
/// and records which window in that tree currently has focus. Windows and
/// slots are shared across tabs — a `WindowId` refers into the host's window
/// list regardless of which tab it lives in.
///
/// `#[non_exhaustive]` — additional fields may be added in minor releases.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Tab {
    /// Spatial layout tree for this tab. Leaves reference [`WindowId`]s.
    pub layout: LayoutTree,
    /// The window that has focus within this tab.
    pub focused_window: WindowId,
}

impl Tab {
    /// Create a new tab with the given layout and focused window.
    pub fn new(layout: LayoutTree, focused_window: WindowId) -> Self {
        Self {
            layout,
            focused_window,
        }
    }
}

impl Default for Tab {
    fn default() -> Self {
        Self {
            layout: LayoutTree::Leaf(0),
            focused_window: 0,
        }
    }
}

/// Per-window scroll + geometry state.
///
/// `#[non_exhaustive]` — additional fields may be added in minor releases.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Window {
    /// Index into the host's slot list for the buffer this window displays.
    pub slot: usize,
    /// The rect this window occupied in the last rendered frame.  Written
    /// by the renderer every frame; used by direction-navigation.
    /// `None` until the first render.
    pub last_rect: Option<LayoutRect>,
}

impl Window {
    /// Create a new window pointing at the given slot index.
    ///
    /// Per-window cursor + scroll live on the host's per-window `Editor`
    /// (#151 Phase D) — a `Window` is pure geometry (slot + last rendered rect).
    pub fn new(slot: usize) -> Self {
        Self {
            slot,
            last_rect: None,
        }
    }
}

impl Default for Window {
    fn default() -> Self {
        Self::new(0)
    }
}

/// Direction of a split.
///
/// `#[non_exhaustive]` — new directions may be added in minor releases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SplitDir {
    /// Stacked top-to-bottom (`:split` / `:sp`).
    Horizontal,
    /// Side-by-side left-to-right (`:vsplit` / `:vsp`).
    Vertical,
}

/// Which geometric axis a split is oriented along.
///
/// Returned by [`SplitDir::axis`] so consumers outside the crate can match
/// exhaustively without bumping into the `#[non_exhaustive]` restriction on
/// [`SplitDir`] itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// The split divides space top-to-bottom (rows).
    Row,
    /// The split divides space left-to-right (columns).
    Col,
}

impl SplitDir {
    /// Map this direction to its [`Axis`].
    ///
    /// `Horizontal` splits stacked vertically → they separate **rows**
    /// (`Axis::Row`). `Vertical` splits arranged side-by-side → they
    /// separate **columns** (`Axis::Col`).
    ///
    /// Unknown future variants fall back to `Axis::Row` so callers always
    /// receive a valid answer without panicking.
    pub fn axis(self) -> Axis {
        // `#[allow(unreachable_patterns)]` because from INSIDE this crate all
        // variants are known; the wildcard exists so external consumers relying
        // on `axis()` are future-proof when new variants are added.
        #[allow(unreachable_patterns)]
        match self {
            Self::Horizontal => Axis::Row,
            Self::Vertical => Axis::Col,
            _ => Axis::Row,
        }
    }
}

/// An exact size for one child of a [`Split`](LayoutTree::Split), overriding
/// that split's `ratio`.
///
/// The number is the child's **rendered** extent along the split axis (columns
/// for [`SplitDir::Vertical`], rows for [`SplitDir::Horizontal`]) — the size
/// that comes back from [`LayoutTree::window_rects`], not an allocation the
/// separator is later taken out of. `First(30)` and `Second(30)` both render 30
/// cells; the extra cell the separator needs is found for you.
///
/// This is deliberate: a dock's width comes from user config, and the caller
/// must never have to add one to compensate for a separator it can't see.
///
/// The request is clamped so the sibling always keeps at least one cell — see
/// [`LayoutTree::window_rects`] for the full geometry contract.
///
/// `#[non_exhaustive]` — additional variants may be added in minor releases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Fixed {
    /// Render the first (top / left) child at exactly this many cells.
    First(u16),
    /// Render the second (bottom / right) child at exactly this many cells.
    Second(u16),
}

/// A binary spatial tree that partitions the editor area into windows.
///
/// # Examples
///
/// ```rust
/// use hjkl_layout::{LayoutTree, SplitDir};
///
/// let tree = LayoutTree::split(
///     SplitDir::Horizontal,
///     0.5,
///     LayoutTree::Leaf(0),
///     LayoutTree::Leaf(1),
/// );
/// assert_eq!(tree.leaves(), vec![0, 1]);
/// assert_eq!(tree.neighbor_below(0), Some(1));
/// ```
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum LayoutTree {
    /// A single window occupying the full allocated area.
    Leaf(WindowId),
    /// Two sub-trees dividing the available space.
    Split {
        /// Axis along which the space is divided.
        dir: SplitDir,
        /// Fraction of the available space allocated to `a`. `0.0 < ratio < 1.0`.
        ///
        /// Ignored when `fixed` is `Some` — but still retained (and still
        /// mutated by resize commands) so that clearing `fixed` restores the
        /// previous proportional layout.
        ratio: f32,
        /// Exact cell allocation for one child, overriding `ratio` when set.
        ///
        /// This is how a dock-style window keeps a constant width/height while
        /// its sibling absorbs every resize.
        fixed: Option<Fixed>,
        /// The first (top / left) sub-tree.
        a: Box<Self>,
        /// The second (bottom / right) sub-tree.
        b: Box<Self>,
        /// Rect this split last occupied. Filled by the renderer each frame;
        /// read by resize commands to convert line/col deltas to ratio updates.
        /// None before the first render.
        last_rect: Option<LayoutRect>,
    },
}

impl Default for LayoutTree {
    fn default() -> Self {
        Self::Leaf(0)
    }
}

impl LayoutTree {
    /// Create a new single-leaf layout tree containing `id`.
    pub fn new(id: WindowId) -> Self {
        Self::Leaf(id)
    }

    /// Convenience constructor for an ordinary proportional split
    /// (`fixed: None`, `last_rect: None`).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use hjkl_layout::{LayoutTree, SplitDir};
    ///
    /// let tree = LayoutTree::split(
    ///     SplitDir::Vertical,
    ///     0.5,
    ///     LayoutTree::Leaf(0),
    ///     LayoutTree::Leaf(1),
    /// );
    /// assert_eq!(tree.leaves(), vec![0, 1]);
    /// ```
    pub fn split(dir: SplitDir, ratio: f32, a: Self, b: Self) -> Self {
        Self::Split {
            dir,
            ratio,
            fixed: None,
            a: Box::new(a),
            b: Box::new(b),
            last_rect: None,
        }
    }

    /// Convenience constructor for a split that renders one child at a fixed
    /// number of cells along the split axis.
    ///
    /// `ratio` is still stored (and used if `fixed` is later cleared); see
    /// [`Fixed`] for the exact geometry.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use hjkl_layout::{Fixed, LayoutRect, LayoutTree, SplitDir};
    ///
    /// // A 30-column dock on the left, the rest for window 1.
    /// let tree = LayoutTree::split_fixed(
    ///     SplitDir::Vertical,
    ///     0.5,
    ///     Fixed::First(30),
    ///     LayoutTree::Leaf(0),
    ///     LayoutTree::Leaf(1),
    /// );
    /// let rects = tree.window_rects(LayoutRect::new(0, 0, 80, 24));
    /// // 30 columns of dock, 1 separator column, 49 columns for window 1.
    /// assert_eq!(rects[0].1.w, 30);
    /// assert_eq!(rects[1].1.w, 49);
    /// ```
    pub fn split_fixed(dir: SplitDir, ratio: f32, fixed: Fixed, a: Self, b: Self) -> Self {
        Self::Split {
            dir,
            ratio,
            fixed: Some(fixed),
            a: Box::new(a),
            b: Box::new(b),
            last_rect: None,
        }
    }

    /// Pre-order traversal — returns all leaf ids in the order they appear
    /// top-to-bottom / left-to-right in the layout.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use hjkl_layout::{LayoutTree, SplitDir};
    ///
    /// let tree = LayoutTree::split(
    ///     SplitDir::Horizontal,
    ///     0.5,
    ///     LayoutTree::Leaf(0),
    ///     LayoutTree::Leaf(1),
    /// );
    /// assert_eq!(tree.leaves(), vec![0, 1]);
    /// ```
    pub fn leaves(&self) -> Vec<WindowId> {
        let mut out = Vec::new();
        self.collect_leaves(&mut out);
        out
    }

    fn collect_leaves(&self, out: &mut Vec<WindowId>) {
        match self {
            Self::Leaf(id) => out.push(*id),
            Self::Split { a, b, .. } => {
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
            Self::Leaf(leaf_id) => *leaf_id == id,
            Self::Split { a, b, .. } => a.contains(id) || b.contains(id),
        }
    }

    /// Find the leaf for `id` and replace it in-place with `f(id)`.
    /// Returns `true` if the leaf was found and replaced.
    pub fn replace_leaf<F: FnOnce(WindowId) -> Self + 'static>(
        &mut self,
        id: WindowId,
        f: F,
    ) -> bool {
        self.replace_leaf_boxed(id, Box::new(f))
    }

    fn replace_leaf_boxed(&mut self, id: WindowId, f: Box<dyn FnOnce(WindowId) -> Self>) -> bool {
        match self {
            Self::Leaf(leaf_id) if *leaf_id == id => {
                *self = f(id);
                true
            }
            Self::Leaf(_) => false,
            Self::Split { a, b, .. } => {
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
            Self::Leaf(_) => None,
            Self::Split {
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
    ///
    /// **Splits carrying a [`Fixed`] allocation are never candidates.** Their
    /// size belongs to whoever set it (a dock's configured width, say), not to
    /// a resize command, and `ratio` is ignored while `fixed` is set — writing
    /// it would do nothing now and make the layout jump later, when `fixed` is
    /// cleared. The search simply continues outward, so a `<C-w>+`-style resize
    /// inside a fixed pane moves the nearest resizable ancestor instead. That
    /// is vim's `winfixwidth` / `winfixheight` behaviour.
    pub fn enclosing_split_mut(
        &mut self,
        id: WindowId,
        dir: SplitDir,
    ) -> Option<(&mut f32, Option<LayoutRect>, bool)> {
        match self {
            Self::Leaf(_) => None,
            Self::Split {
                dir: my_dir,
                ratio,
                fixed,
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
                let is_fixed = fixed.is_some();

                // Try deeper first (innermost wins).
                let inner = if in_a {
                    a.enclosing_split_mut(id, dir)
                } else {
                    b.enclosing_split_mut(id, dir)
                };
                if inner.is_some() {
                    return inner;
                }

                // No deeper match — am I a candidate? A fixed split never is;
                // returning None here hands the search to my parent.
                if my_dir == dir && !is_fixed {
                    Some((ratio, saved_rect, in_a))
                } else {
                    None
                }
            }
        }
    }

    /// Reset all splits in the tree to 0.5 ratio, leaving pinned leaves at
    /// their current size.
    ///
    /// `pinned` lists the leaves that must survive equalization unchanged
    /// (docks and other fixed-size windows). A split is left alone when it
    /// carries a [`Fixed`] allocation or when either of its immediate children
    /// is a pinned leaf; recursion still descends into both children so
    /// ordinary splits underneath a pinned one are still equalized.
    ///
    /// Pass `&[]` for the plain "equalize everything" behaviour.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use hjkl_layout::{Fixed, LayoutRect, LayoutTree, SplitDir};
    ///
    /// let mut tree = LayoutTree::split_fixed(
    ///     SplitDir::Vertical,
    ///     0.5,
    ///     Fixed::First(30),
    ///     LayoutTree::Leaf(0),
    ///     LayoutTree::Leaf(1),
    /// );
    /// let area = LayoutRect::new(0, 0, 80, 24);
    /// let before = tree.window_rects(area);
    /// tree.equalize_all(&[0]);
    /// assert_eq!(tree.window_rects(area), before);
    /// ```
    pub fn equalize_all(&mut self, pinned: &[WindowId]) {
        if let Self::Split {
            ratio, fixed, a, b, ..
        } = self
        {
            let touches_pinned =
                fixed.is_some() || is_pinned_leaf(a, pinned) || is_pinned_leaf(b, pinned);
            if !touches_pinned {
                *ratio = 0.5;
            }
            a.equalize_all(pinned);
            b.equalize_all(pinned);
        }
    }

    /// Collapse the tree down to `keep` plus every pinned leaf (vim's `:only`).
    ///
    /// Leaves that are neither `keep` nor listed in `pinned` are dropped and
    /// their parent splits collapse onto the surviving sibling, so the relative
    /// arrangement of the retained leaves (and the geometry of the splits that
    /// join them) is preserved.
    ///
    /// Returns the ids of the removed leaves so the caller can dispose of the
    /// corresponding window state. Returns an empty vector — and leaves the
    /// tree untouched — when `keep` is not in the tree.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use hjkl_layout::{LayoutTree, SplitDir};
    ///
    /// // dock | (1 | 2)
    /// let mut tree = LayoutTree::split(
    ///     SplitDir::Vertical,
    ///     0.5,
    ///     LayoutTree::Leaf(9),
    ///     LayoutTree::split(SplitDir::Vertical, 0.5, LayoutTree::Leaf(1), LayoutTree::Leaf(2)),
    /// );
    /// let removed = tree.only(1, &[9]);
    /// assert_eq!(removed, vec![2]);
    /// assert_eq!(tree.leaves(), vec![9, 1]);
    /// ```
    pub fn only(&mut self, keep: WindowId, pinned: &[WindowId]) -> Vec<WindowId> {
        if !self.contains(keep) {
            return Vec::new();
        }
        let mut removed = Vec::new();
        self.prune_to(keep, pinned, &mut removed);
        removed
    }

    /// Recursive helper for [`only`](Self::only). Every subtree it is invoked
    /// on retains at least one leaf, which `only` guarantees at the root by
    /// checking that `keep` is present.
    fn prune_to(&mut self, keep: WindowId, pinned: &[WindowId], removed: &mut Vec<WindowId>) {
        let Self::Split { a, b, .. } = self else {
            return;
        };
        let a_keeps = a.retains_any(keep, pinned);
        let b_keeps = b.retains_any(keep, pinned);
        match (a_keeps, b_keeps) {
            (true, true) => {
                a.prune_to(keep, pinned, removed);
                b.prune_to(keep, pinned, removed);
            }
            (true, false) => {
                b.collect_leaves(removed);
                let mut survivor = std::mem::replace(a.as_mut(), Self::Leaf(keep));
                survivor.prune_to(keep, pinned, removed);
                *self = survivor;
            }
            (false, true) => {
                a.collect_leaves(removed);
                let mut survivor = std::mem::replace(b.as_mut(), Self::Leaf(keep));
                survivor.prune_to(keep, pinned, removed);
                *self = survivor;
            }
            // Unreachable from `only` (the root always retains `keep`), but a
            // defensive no-op beats a panic if a caller reaches it directly.
            (false, false) => {}
        }
    }

    /// Does this subtree contain `keep` or any leaf in `pinned`?
    fn retains_any(&self, keep: WindowId, pinned: &[WindowId]) -> bool {
        match self {
            Self::Leaf(id) => *id == keep || pinned.contains(id),
            Self::Split { a, b, .. } => a.retains_any(keep, pinned) || b.retains_any(keep, pinned),
        }
    }

    /// For each enclosing Split on the path from root to leaf `id`, invoke
    /// `f` with the split's mutable state. Order: outermost first.
    ///
    /// Splits carrying a [`Fixed`] allocation are **skipped** (recursion still
    /// descends through them) for the same reason
    /// [`enclosing_split_mut`](Self::enclosing_split_mut) refuses them: their
    /// size is not a ratio anyone may rewrite.
    pub fn for_each_ancestor<F>(&mut self, id: WindowId, f: &mut F)
    where
        F: FnMut(SplitDir, &mut f32, bool, Option<LayoutRect>),
    {
        if let Self::Split {
            dir,
            ratio,
            fixed,
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
            if fixed.is_none() {
                f(*dir, ratio, in_a, *last_rect);
            }
            if in_a {
                a.for_each_ancestor(id, f);
            } else {
                b.for_each_ancestor(id, f);
            }
        }
    }

    /// Walk the tree and compute the [`LayoutRect`] each leaf window occupies
    /// within `area`. Every step goes through [`split_geometry`], which is also
    /// what the TUI renderer descends with, so headless geometry is the same
    /// geometry the user sees rather than a copy of it.
    ///
    /// # Split math (see [`split_geometry`])
    ///
    /// For a **Horizontal** split (stacks top-to-bottom, `Axis::Row`):
    ///   `a_h = round(area.h * ratio).clamp(1, area.h - 1)`
    ///   `b_h = area.h - a_h`
    ///   If `a_h >= 2` and `b_h > 0`: the bottom row of `a` becomes a separator;
    ///   `a` shrinks by 1 (height becomes `a_h - 1`), `b` starts after the separator.
    ///
    /// For a **Vertical** split (side-by-side, `Axis::Col`):
    ///   `a_w = round(area.w * ratio).clamp(1, area.w - 1)`
    ///   `b_w = area.w - a_w`
    ///   If `a_w >= 2` and `b_w > 0`: the rightmost column of `a` becomes a separator;
    ///   `a` shrinks by 1 (width becomes `a_w - 1`), `b` starts right after `a`.
    ///
    /// Leaf → single entry `(id, area)`.
    ///
    /// # Fixed sizes
    ///
    /// When the split carries `fixed: Some(..)`, the `round(len * ratio)` step
    /// above is replaced by whatever allocation makes the named child *render*
    /// the requested number of cells, and the sibling takes the remainder. The
    /// two variants are symmetric: on an 80-column vertical split both
    /// `Fixed::First(30)` and `Fixed::Second(30)` produce a 30-column rect for
    /// their child, with the separator absorbed on the `a` side either way
    /// (`First(30)` → `a` = 30, `b` = 49; `Fixed::Second(30)` → `a` = 49,
    /// `b` = 30).
    ///
    /// The allocation is clamped to `1 ..= axis_len - 1` — the same clamp the
    /// ratio path applies — so an oversized fixed size degrades to "as large as
    /// possible while the sibling still gets one cell" instead of underflowing
    /// or starving the sibling.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use hjkl_layout::{LayoutTree, SplitDir, LayoutRect};
    ///
    /// let tree = LayoutTree::Leaf(0);
    /// let area = LayoutRect::new(0, 0, 80, 23);
    /// let rects = tree.window_rects(area);
    /// assert_eq!(rects, vec![(0, area)]);
    /// ```
    pub fn window_rects(&self, area: LayoutRect) -> Vec<(WindowId, LayoutRect)> {
        let mut out = Vec::new();
        self.collect_rects(area, &mut out);
        out
    }

    fn collect_rects(&self, area: LayoutRect, out: &mut Vec<(WindowId, LayoutRect)>) {
        match self {
            Self::Leaf(id) => out.push((*id, area)),
            Self::Split {
                dir,
                ratio,
                fixed,
                a,
                b,
                ..
            } => {
                let geo = split_geometry(area, *dir, *ratio, *fixed);
                a.collect_rects(geo.a, out);
                b.collect_rects(geo.b, out);
            }
        }
    }

    /// Swap the two children of the deepest Split that directly contains
    /// `Leaf(id)` as one of its `a` or `b` children.
    ///
    /// Returns `true` if the swap was applied (i.e. there is an enclosing
    /// Split — `false` when `id` is the only window).
    ///
    /// Refuses (returns `false`, tree untouched) when either side of the swap
    /// is pinned: `id` itself being in `pinned`, or the sibling subtree
    /// containing any pinned leaf. A dock must not be dragged across the
    /// layout, nor another window dragged through it.
    ///
    /// Pass `&[]` for the plain "swap anything" behaviour.
    pub fn swap_with_sibling(&mut self, id: WindowId, pinned: &[WindowId]) -> bool {
        if pinned.contains(&id) {
            return false;
        }
        self.swap_with_sibling_inner(id, pinned)
    }

    fn swap_with_sibling_inner(&mut self, id: WindowId, pinned: &[WindowId]) -> bool {
        match self {
            Self::Leaf(_) => false,
            Self::Split { a, b, .. } => {
                let a_is_focused_leaf = matches!(a.as_ref(), Self::Leaf(leaf) if *leaf == id);
                let b_is_focused_leaf = matches!(b.as_ref(), Self::Leaf(leaf) if *leaf == id);
                if a_is_focused_leaf || b_is_focused_leaf {
                    // The sibling is whichever side isn't the focused leaf.
                    let sibling_pinned = if a_is_focused_leaf {
                        contains_pinned(b, pinned)
                    } else {
                        contains_pinned(a, pinned)
                    };
                    if sibling_pinned {
                        return false;
                    }
                    std::mem::swap(a, b);
                    return true;
                }
                // Recurse into whichever side contains id.
                if a.contains(id) {
                    return a.swap_with_sibling_inner(id, pinned);
                }
                if b.contains(id) {
                    return b.swap_with_sibling_inner(id, pinned);
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
    ///
    /// # Errors
    ///
    /// Returns `Err("E444: Cannot close last window")` when attempting to
    /// remove the only leaf in the tree.
    pub fn remove_leaf(&mut self, id: WindowId) -> Result<WindowId, &'static str> {
        if matches!(self, Self::Leaf(_)) {
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
            Self::Leaf(_) => None, // can't remove the only leaf
            Self::Split { a, b, .. } => {
                // Case 1: `a` is the leaf we want to remove.
                if matches!(a.as_ref(), Self::Leaf(leaf) if *leaf == id) {
                    let new_focus = first_leaf(b);
                    // Collapse: replace self with b.
                    *self = *b.clone();
                    return Some(new_focus);
                }
                // Case 2: `b` is the leaf we want to remove.
                if matches!(b.as_ref(), Self::Leaf(leaf) if *leaf == id) {
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

/// Where one split's two children and its separator land inside a parent rect.
///
/// Produced by [`split_geometry`]. `a` and `b` are the rects the children are
/// rendered into — already shrunk for the separator — and `separator` is the
/// 1-cell strip between them, or `None` when the area was too small for one to
/// be drawn.
///
/// `#[non_exhaustive]` — additional fields may be added in minor releases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct SplitGeometry {
    /// Rect for the first (top / left) child.
    pub a: LayoutRect,
    /// Rect for the second (bottom / right) child.
    pub b: LayoutRect,
    /// The 1-cell separator strip between the children, if one fits. Its
    /// orientation follows the split: a single column for [`SplitDir::Vertical`],
    /// a single row for [`SplitDir::Horizontal`].
    pub separator: Option<LayoutRect>,
}

/// Divide `area` between one split's two children, carving out the separator.
///
/// This is the **single source of truth** for split geometry: the headless
/// [`LayoutTree::window_rects`] walk, the TUI renderer (which also needs the
/// separator rect to draw it) and the mouse border hit-test all call it, so a
/// border the user can see is always a border they can grab.
///
/// ## Separator rules
///
/// **Vertical** (side-by-side, `Axis::Col`): separator is the rightmost cell
/// of `a`'s allocation. Applied only when that allocation is `>= 2` columns AND
/// `b` gets `> 0`; `a` shrinks by 1 column, `b` position/size are unchanged.
///
/// **Horizontal** (stacked, `Axis::Row`): separator is the bottom cell of `a`'s
/// allocation. Applied only when that allocation is `>= 2` rows AND `b` gets
/// `> 0`; `a` shrinks by 1 row, `b` position/size are unchanged.
///
/// ## Fixed sizes
///
/// `fixed` replaces the `round(len * ratio)` term with the allocation that
/// makes the named child render exactly the requested number of cells (see
/// [`Fixed`] and `first_child_cells`). It goes through the identical clamp, so
/// an oversized request can never underflow `u16` or leave the sibling with
/// zero cells in an area that could hold both.
///
/// # Examples
///
/// ```rust
/// use hjkl_layout::{Fixed, LayoutRect, SplitDir, split_geometry};
///
/// let geo = split_geometry(
///     LayoutRect::new(0, 0, 80, 24),
///     SplitDir::Vertical,
///     0.5,
///     Some(Fixed::First(30)),
/// );
/// assert_eq!(geo.a.w, 30);
/// assert_eq!(geo.separator.map(|s| s.x), Some(30));
/// assert_eq!((geo.b.x, geo.b.w), (31, 49));
/// ```
pub fn split_geometry(
    area: LayoutRect,
    dir: SplitDir,
    ratio: f32,
    fixed: Option<Fixed>,
) -> SplitGeometry {
    match dir.axis() {
        Axis::Row => {
            // A zero-height parent can't be split; the clamp below would
            // otherwise force a size-1 child that overflows the parent.
            if area.h == 0 {
                let empty = LayoutRect::new(area.x, area.y, area.w, 0);
                return SplitGeometry {
                    a: empty,
                    b: empty,
                    separator: None,
                };
            }
            // Horizontal split: divide height.
            let a_h = first_child_cells(area.h, ratio, fixed);
            let a_h = a_h.clamp(1, area.h.saturating_sub(1).max(1));
            let b_h = area.h.saturating_sub(a_h);
            let mut rect_a = LayoutRect::new(area.x, area.y, area.w, a_h);
            let rect_b = LayoutRect::new(area.x, area.y + a_h, area.w, b_h);
            // Carve separator: bottom row of rect_a, only when safe.
            let separator = if rect_a.h >= 2 && rect_b.h > 0 {
                rect_a.h -= 1;
                Some(LayoutRect::new(rect_a.x, rect_a.y + rect_a.h, rect_a.w, 1))
            } else {
                None
            };
            SplitGeometry {
                a: rect_a,
                b: rect_b,
                separator,
            }
        }
        Axis::Col => {
            // A zero-width parent can't be split; see the Row branch.
            if area.w == 0 {
                let empty = LayoutRect::new(area.x, area.y, 0, area.h);
                return SplitGeometry {
                    a: empty,
                    b: empty,
                    separator: None,
                };
            }
            // Vertical split: divide width.
            let a_w = first_child_cells(area.w, ratio, fixed);
            let a_w = a_w.clamp(1, area.w.saturating_sub(1).max(1));
            let b_w = area.w.saturating_sub(a_w);
            let mut rect_a = LayoutRect::new(area.x, area.y, a_w, area.h);
            let rect_b = LayoutRect::new(area.x + a_w, area.y, b_w, area.h);
            // Carve separator: rightmost column of rect_a, only when safe.
            let separator = if rect_a.w >= 2 && rect_b.w > 0 {
                rect_a.w -= 1;
                Some(LayoutRect::new(rect_a.x + rect_a.w, rect_a.y, 1, rect_a.h))
            } else {
                None
            };
            SplitGeometry {
                a: rect_a,
                b: rect_b,
                separator,
            }
        }
    }
}

/// Cells to allocate to the first child along the split axis, before the
/// caller's `1 ..= len - 1` clamp.
///
/// `len` is the parent's extent along the split axis. Without a `fixed` the
/// answer is the historical `round(len * ratio)`. With one:
///
/// - `Fixed::First(n)` → `n + 1`: [`Fixed`] counts **rendered** cells and the
///   separator is carved out of the first child, so the first child needs one
///   more cell than it renders. Saturating, so `u16::MAX` can't wrap.
/// - `Fixed::Second(n)` → `len - n`, saturating at 0 so a request larger than
///   the parent degrades (via the caller's clamp) to "everything but one cell"
///   instead of wrapping around `u16`. No `+ 1` here: the separator already
///   comes out of the *first* child, so the second renders its whole share.
///
/// The `+ 1` is unconditional because the caller's `1 ..= len - 1` clamp
/// already collapses it in exactly the cases where no separator gets carved.
/// A separator is skipped only when the first child ends up with a single cell
/// (`rect_a < 2`) — the clamp's own floor — or when the second child gets zero,
/// which the clamp's ceiling makes impossible for `len >= 2`. So `First(n)`
/// renders `n` whenever the area can hold it, and the largest size that fits
/// otherwise.
///
/// Unknown future `Fixed` variants fall back to the ratio, so they degrade to
/// an ordinary split rather than a panic.
fn first_child_cells(len: u16, ratio: f32, fixed: Option<Fixed>) -> u16 {
    match fixed {
        Some(Fixed::First(n)) => n.saturating_add(1),
        Some(Fixed::Second(n)) => len.saturating_sub(n),
        _ => ((len as f32) * ratio).round() as u16,
    }
}

/// Is this subtree a single leaf that the caller asked to leave alone?
fn is_pinned_leaf(tree: &LayoutTree, pinned: &[WindowId]) -> bool {
    matches!(tree, LayoutTree::Leaf(id) if pinned.contains(id))
}

/// Does this subtree contain any leaf the caller asked to leave alone?
fn contains_pinned(tree: &LayoutTree, pinned: &[WindowId]) -> bool {
    match tree {
        LayoutTree::Leaf(id) => pinned.contains(id),
        LayoutTree::Split { a, b, .. } => contains_pinned(a, pinned) || contains_pinned(b, pinned),
    }
}

/// Internal direction enum used by `neighbor_direction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NavDir {
    Below,
    Above,
    Left,
    Right,
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
        LayoutTree::split(SplitDir::Horizontal, ratio, a, b)
    }

    fn vsplit(ratio: f32, a: LayoutTree, b: LayoutTree) -> LayoutTree {
        LayoutTree::split(SplitDir::Vertical, ratio, a, b)
    }

    fn hsplit_with_rect(ratio: f32, a: LayoutTree, b: LayoutTree, rect: LayoutRect) -> LayoutTree {
        LayoutTree::Split {
            dir: SplitDir::Horizontal,
            ratio,
            fixed: None,
            a: Box::new(a),
            b: Box::new(b),
            last_rect: Some(rect),
        }
    }

    fn vsplit_with_rect(ratio: f32, a: LayoutTree, b: LayoutTree, rect: LayoutRect) -> LayoutTree {
        LayoutTree::Split {
            dir: SplitDir::Vertical,
            ratio,
            fixed: None,
            a: Box::new(a),
            b: Box::new(b),
            last_rect: Some(rect),
        }
    }

    /// Ratio of the split at the root, for equalize assertions.
    fn root_ratio(t: &LayoutTree) -> f32 {
        match t {
            LayoutTree::Split { ratio, .. } => *ratio,
            LayoutTree::Leaf(_) => panic!("expected a split at the root"),
        }
    }

    // ── LayoutRect ────────────────────────────────────────────────────────────

    #[test]
    fn layout_rect_new_roundtrips_fields() {
        let r = LayoutRect::new(1, 2, 80, 24);
        assert_eq!(r.x, 1);
        assert_eq!(r.y, 2);
        assert_eq!(r.w, 80);
        assert_eq!(r.h, 24);
    }

    #[test]
    fn layout_rect_default_is_zero() {
        let r = LayoutRect::default();
        assert_eq!(r, LayoutRect::new(0, 0, 0, 0));
    }

    #[test]
    fn headless_split_zero_size_parent_does_not_overflow() {
        // A zero-height parent must yield zero-height children, not a size-1
        // child that exceeds the parent.
        let geo = split_geometry(
            LayoutRect::new(0, 0, 10, 0),
            SplitDir::Horizontal,
            0.5,
            None,
        );
        assert_eq!((geo.a.h, geo.b.h), (0, 0), "row split of h=0 must stay 0");
        assert_eq!(
            geo.separator, None,
            "no separator fits in a zero-height row"
        );
        // Same for a zero-width parent under a vertical split.
        let geo = split_geometry(LayoutRect::new(0, 0, 0, 10), SplitDir::Vertical, 0.5, None);
        assert_eq!((geo.a.w, geo.b.w), (0, 0), "col split of w=0 must stay 0");
        assert_eq!(geo.separator, None, "no separator fits in a zero-width col");
    }

    // ── Tab ───────────────────────────────────────────────────────────────────

    #[test]
    fn tab_new_and_default() {
        let t = Tab::new(LayoutTree::Leaf(5), 5);
        assert_eq!(t.focused_window, 5);
        assert_eq!(t.layout.leaves(), vec![5]);

        let d = Tab::default();
        assert_eq!(d.focused_window, 0);
        assert_eq!(d.layout.leaves(), vec![0]);
    }

    // ── Window ────────────────────────────────────────────────────────────────

    #[test]
    fn window_new_and_default() {
        let w = Window::new(3);
        assert_eq!(w.slot, 3);
        assert!(w.last_rect.is_none());

        let d = Window::default();
        assert_eq!(d.slot, 0);
    }

    // ── LayoutTree::new / default ─────────────────────────────────────────────

    #[test]
    fn layout_tree_new_creates_leaf() {
        let t = LayoutTree::new(7);
        assert_eq!(t.leaves(), vec![7]);
    }

    #[test]
    fn layout_tree_default_is_leaf_zero() {
        let t = LayoutTree::default();
        assert_eq!(t.leaves(), vec![0]);
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
        // 0 / (1 / 2)
        let tree = hsplit(0.5, leaf(0), hsplit(0.5, leaf(1), leaf(2)));
        assert_eq!(tree.leaves(), vec![0, 1, 2]);
    }

    #[test]
    fn leaves_nested_left_split() {
        let tree = hsplit(0.5, hsplit(0.5, leaf(0), leaf(1)), leaf(2));
        assert_eq!(tree.leaves(), vec![0, 1, 2]);
    }

    // ── contains() ───────────────────────────────────────────────────────────

    #[test]
    fn contains_returns_true_for_present_id() {
        let tree = hsplit(0.5, leaf(0), leaf(1));
        assert!(tree.contains(0));
        assert!(tree.contains(1));
        assert!(!tree.contains(2));
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
        // 0 / (1 / 2)
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
        let tree = hsplit(0.5, leaf(0), leaf(1));
        assert_eq!(tree.neighbor_left(0), None);
        assert_eq!(tree.neighbor_left(1), None);
        assert_eq!(tree.neighbor_right(0), None);
        assert_eq!(tree.neighbor_right(1), None);
    }

    #[test]
    fn neighbor_left_three_leaf_vertical() {
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
        let tree = vsplit(0.5, leaf(0), hsplit(0.5, leaf(1), leaf(2)));
        assert_eq!(tree.next_leaf(0), Some(1));
        assert_eq!(tree.next_leaf(1), Some(2));
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
        let outer_rect = LayoutRect::new(0, 0, 80, 40);
        let inner_rect = LayoutRect::new(0, 20, 80, 20);
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
        assert!(in_a, "id=1 is in the 'a' side of the inner split");
    }

    #[test]
    fn enclosing_split_mut_skips_wrong_dir() {
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
        let mut tree = hsplit(0.3, leaf(0), hsplit(0.7, leaf(1), leaf(2)));
        tree.equalize_all(&[]);
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
        let outer_rect = LayoutRect::new(0, 0, 80, 24);
        let inner_rect = LayoutRect::new(24, 0, 56, 24);
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
        let mut tree = hsplit(0.5, leaf(0), leaf(1));
        let swapped = tree.swap_with_sibling(0, &[]);
        assert!(swapped, "swap should succeed in a two-leaf split");
        assert_eq!(tree.leaves(), vec![1, 0], "leaves should be swapped");
    }

    #[test]
    fn swap_with_sibling_in_nested_split_swaps_at_focused_parent() {
        let mut tree = hsplit(0.5, leaf(0), vsplit(0.5, leaf(1), leaf(2)));
        let swapped = tree.swap_with_sibling(1, &[]);
        assert!(swapped, "swap should succeed");
        assert_eq!(
            tree.leaves(),
            vec![0, 2, 1],
            "inner leaves should be swapped"
        );
    }

    #[test]
    fn swap_with_sibling_returns_false_for_only_leaf() {
        let mut tree = leaf(0);
        let swapped = tree.swap_with_sibling(0, &[]);
        assert!(!swapped, "single leaf has no sibling to swap with");
    }

    #[test]
    fn swap_with_sibling_refuses_when_the_moving_leaf_is_pinned() {
        let mut tree = vsplit(0.5, leaf(9), leaf(0));
        let swapped = tree.swap_with_sibling(9, &[9]);
        assert!(!swapped, "a pinned leaf must not move");
        assert_eq!(tree.leaves(), vec![9, 0], "tree must be untouched");
    }

    #[test]
    fn swap_with_sibling_refuses_when_the_sibling_is_pinned() {
        let mut tree = vsplit(0.5, leaf(9), leaf(0));
        let swapped = tree.swap_with_sibling(0, &[9]);
        assert!(!swapped, "must not swap a pinned sibling out of place");
        assert_eq!(tree.leaves(), vec![9, 0], "tree must be untouched");
    }

    #[test]
    fn swap_with_sibling_refuses_when_the_sibling_subtree_holds_a_pin() {
        // 0 | (9 | 1) — swapping 0 with its sibling would drag the pinned 9.
        let mut tree = vsplit(0.5, leaf(0), vsplit(0.5, leaf(9), leaf(1)));
        let swapped = tree.swap_with_sibling(0, &[9]);
        assert!(!swapped, "a pin anywhere in the sibling blocks the swap");
        assert_eq!(tree.leaves(), vec![0, 9, 1]);
    }

    #[test]
    fn swap_with_sibling_still_works_beside_an_unrelated_pin() {
        // 9 | (0 | 1) — swapping 0 and 1 leaves the pinned 9 where it is.
        let mut tree = vsplit(0.5, leaf(9), vsplit(0.5, leaf(0), leaf(1)));
        let swapped = tree.swap_with_sibling(0, &[9]);
        assert!(swapped, "the pin is not on either side of this swap");
        assert_eq!(tree.leaves(), vec![9, 1, 0]);
    }

    // ── equalize_all() with pins ──────────────────────────────────────────────

    #[test]
    fn equalize_all_leaves_a_pinned_leaf_at_its_size() {
        // dock | (1 / 2), dock fixed at 30 columns.
        let area = LayoutRect::new(0, 0, 80, 24);
        let mut tree = LayoutTree::split_fixed(
            SplitDir::Vertical,
            0.9,
            Fixed::First(30),
            leaf(9),
            hsplit(0.8, leaf(1), leaf(2)),
        );
        let dock_before = tree.window_rects(area)[0].1;
        tree.equalize_all(&[9]);
        let after = tree.window_rects(area);
        assert_eq!(after[0].0, 9);
        assert_eq!(after[0].1, dock_before, "pinned dock must keep its rect");
        // The dock's own split kept its ratio; the regular split below did not.
        assert!((root_ratio(&tree) - 0.9).abs() < 1e-5);
        if let LayoutTree::Split { b, .. } = &tree {
            assert!(
                (root_ratio(b) - 0.5).abs() < 1e-5,
                "ordinary splits under a pin still equalize"
            );
        } else {
            panic!("root should still be a split");
        }
    }

    #[test]
    fn equalize_all_protects_a_ratio_split_next_to_a_pinned_leaf() {
        // No `fixed` here — the pin alone must stop the ratio being reset.
        let mut tree = vsplit(0.2, leaf(9), leaf(0));
        tree.equalize_all(&[9]);
        assert!(
            (root_ratio(&tree) - 0.2).abs() < 1e-5,
            "a split with a pinned child keeps its ratio"
        );
    }

    // ── only() ────────────────────────────────────────────────────────────────

    #[test]
    fn only_collapses_to_the_kept_leaf_when_nothing_is_pinned() {
        let mut tree = hsplit(0.5, leaf(0), vsplit(0.5, leaf(1), leaf(2)));
        let mut removed = tree.only(1, &[]);
        removed.sort_unstable();
        assert_eq!(removed, vec![0, 2]);
        assert_eq!(tree.leaves(), vec![1]);
    }

    #[test]
    fn only_retains_pinned_leaves_and_their_arrangement() {
        // dock | ((1 / 2) / qf) → only(1) keeps dock | (1 / qf), in that order.
        let mut tree = vsplit(
            0.5,
            leaf(9),
            hsplit(0.5, hsplit(0.5, leaf(1), leaf(2)), leaf(8)),
        );
        let removed = tree.only(1, &[9, 8]);
        assert_eq!(removed, vec![2]);
        assert_eq!(
            tree.leaves(),
            vec![9, 1, 8],
            "dock stays left of the kept window, quickfix stays below it"
        );
    }

    #[test]
    fn only_on_a_single_leaf_is_a_no_op() {
        let mut tree = leaf(0);
        assert!(tree.only(0, &[]).is_empty());
        assert_eq!(tree.leaves(), vec![0]);
    }

    #[test]
    fn only_with_an_absent_keep_changes_nothing() {
        let mut tree = hsplit(0.5, leaf(0), leaf(1));
        assert!(tree.only(99, &[]).is_empty());
        assert_eq!(tree.leaves(), vec![0, 1]);
    }

    #[test]
    fn only_keeping_a_pinned_leaf_drops_the_rest() {
        let mut tree = vsplit(0.5, leaf(9), hsplit(0.5, leaf(0), leaf(1)));
        let mut removed = tree.only(9, &[9]);
        removed.sort_unstable();
        assert_eq!(removed, vec![0, 1]);
        assert_eq!(tree.leaves(), vec![9]);
    }

    #[test]
    fn only_preserves_the_geometry_of_the_surviving_split() {
        let mut tree = vsplit(0.25, leaf(9), hsplit(0.5, leaf(0), leaf(1)));
        tree.only(0, &[9]);
        assert!(
            (root_ratio(&tree) - 0.25).abs() < 1e-5,
            "the split joining the retained leaves keeps its ratio"
        );
    }

    // ── fixed sizing ──────────────────────────────────────────────────────────

    #[test]
    fn fixed_first_renders_exact_cells_along_a_vertical_split() {
        // The dock renders the full 30 columns it asked for; the separator
        // costs the sibling, not the dock.
        let tree =
            LayoutTree::split_fixed(SplitDir::Vertical, 0.5, Fixed::First(30), leaf(0), leaf(1));
        let rects = tree.window_rects(LayoutRect::new(0, 0, 80, 24));
        assert_eq!(rects[0].1, LayoutRect::new(0, 0, 30, 24));
        assert_eq!(rects[1].1, LayoutRect::new(31, 0, 49, 24));
    }

    #[test]
    fn fixed_second_renders_exact_cells_along_a_vertical_split() {
        // Mirror image of the `First` case: same requested size, same rendered
        // size, only the side differs. `Fixed` must not mean two things.
        let tree =
            LayoutTree::split_fixed(SplitDir::Vertical, 0.5, Fixed::Second(30), leaf(0), leaf(1));
        let rects = tree.window_rects(LayoutRect::new(0, 0, 80, 24));
        assert_eq!(
            rects[1].1.w, 30,
            "Second(30) must render 30, like First(30)"
        );
        assert_eq!(rects[0].1, LayoutRect::new(0, 0, 49, 24));
        assert_eq!(rects[1].1, LayoutRect::new(50, 0, 30, 24));
    }

    #[test]
    fn fixed_first_and_second_render_the_same_size_on_both_axes() {
        let area = LayoutRect::new(0, 0, 80, 24);
        let along = |dir: SplitDir, r: LayoutRect| match dir.axis() {
            Axis::Col => r.w,
            Axis::Row => r.h,
        };
        for dir in [SplitDir::Vertical, SplitDir::Horizontal] {
            for n in [1u16, 2, 10, 20] {
                let first = LayoutTree::split_fixed(dir, 0.5, Fixed::First(n), leaf(0), leaf(1));
                let second = LayoutTree::split_fixed(dir, 0.5, Fixed::Second(n), leaf(0), leaf(1));
                assert_eq!(
                    along(dir, first.window_rects(area)[0].1),
                    n,
                    "First({n}) on {dir:?} must render {n}"
                );
                assert_eq!(
                    along(dir, second.window_rects(area)[1].1),
                    n,
                    "Second({n}) on {dir:?} must render {n}"
                );
            }
        }
    }

    #[test]
    fn fixed_second_renders_exact_cells_along_a_horizontal_split() {
        let tree = LayoutTree::split_fixed(
            SplitDir::Horizontal,
            0.5,
            Fixed::Second(10),
            leaf(0),
            leaf(1),
        );
        let rects = tree.window_rects(LayoutRect::new(0, 0, 80, 24));
        assert_eq!(rects[0].1, LayoutRect::new(0, 0, 80, 13));
        assert_eq!(rects[1].1, LayoutRect::new(0, 14, 80, 10));
    }

    #[test]
    fn fixed_wins_over_ratio() {
        let ratio_only = LayoutTree::split(SplitDir::Vertical, 0.5, leaf(0), leaf(1));
        let fixed =
            LayoutTree::split_fixed(SplitDir::Vertical, 0.5, Fixed::First(20), leaf(0), leaf(1));
        let area = LayoutRect::new(0, 0, 80, 24);
        assert_eq!(ratio_only.window_rects(area)[0].1.w, 39);
        assert_eq!(fixed.window_rects(area)[0].1.w, 20);
    }

    #[test]
    fn fixed_is_independent_of_the_parent_size() {
        let tree =
            LayoutTree::split_fixed(SplitDir::Vertical, 0.5, Fixed::First(30), leaf(0), leaf(1));
        for total in [60u16, 80, 120, 200] {
            let rects = tree.window_rects(LayoutRect::new(0, 0, total, 24));
            assert_eq!(rects[0].1.w, 30, "dock width must not track the parent");
            assert_eq!(rects[1].1.w, total - 31);
        }
    }

    #[test]
    fn fixed_renders_the_requested_size_when_no_separator_is_carved() {
        // A 2-cell axis is too small for a separator (`rect_a < 2`), so the
        // `+ 1` the `First` path adds must be absorbed by the clamp rather
        // than stealing the sibling's only cell.
        let area = LayoutRect::new(0, 0, 2, 2);
        for dir in [SplitDir::Vertical, SplitDir::Horizontal] {
            let first = LayoutTree::split_fixed(dir, 0.5, Fixed::First(1), leaf(0), leaf(1));
            let second = LayoutTree::split_fixed(dir, 0.5, Fixed::Second(1), leaf(0), leaf(1));
            for tree in [first, second] {
                let rects = tree.window_rects(area);
                let (a, b) = (rects[0].1, rects[1].1);
                let (a_len, b_len) = match dir.axis() {
                    Axis::Col => (a.w, b.w),
                    Axis::Row => (a.h, b.h),
                };
                assert_eq!((a_len, b_len), (1, 1), "{dir:?}: both children render 1");
            }
        }

        // One cell more and the separator does get carved — the requested size
        // is still exactly what renders, on both sides.
        let area = LayoutRect::new(0, 0, 3, 24);
        let first =
            LayoutTree::split_fixed(SplitDir::Vertical, 0.5, Fixed::First(1), leaf(0), leaf(1));
        let rects = first.window_rects(area);
        assert_eq!(rects[0].1, LayoutRect::new(0, 0, 1, 24));
        assert_eq!(rects[1].1, LayoutRect::new(2, 0, 1, 24));
        let second =
            LayoutTree::split_fixed(SplitDir::Vertical, 0.5, Fixed::Second(1), leaf(0), leaf(1));
        let rects = second.window_rects(area);
        assert_eq!(rects[0].1, LayoutRect::new(0, 0, 1, 24));
        assert_eq!(rects[1].1, LayoutRect::new(2, 0, 1, 24));
    }

    #[test]
    fn oversized_fixed_clamps_to_leave_the_sibling_one_cell() {
        let area = LayoutRect::new(0, 0, 80, 24);

        // First(200) in an 80-column area → a is clamped to 79 allocated cells
        // (78 after the separator) and b keeps exactly 1.
        let first =
            LayoutTree::split_fixed(SplitDir::Vertical, 0.5, Fixed::First(200), leaf(0), leaf(1));
        let rects = first.window_rects(area);
        assert_eq!(rects[0].1.w, 78);
        assert_eq!(rects[1].1.w, 1);

        // Second(200) is the mirror image: `a` keeps 1 allocated cell (no
        // separator is carved, since a.w < 2) and `b` takes the other 79.
        let second = LayoutTree::split_fixed(
            SplitDir::Vertical,
            0.5,
            Fixed::Second(200),
            leaf(0),
            leaf(1),
        );
        let rects = second.window_rects(area);
        assert_eq!(rects[0].1.w, 1);
        assert_eq!(rects[1].1.w, 79);
        assert_eq!(
            rects[0].1.w + rects[1].1.w,
            80,
            "no cells may be lost or invented"
        );
    }

    #[test]
    fn fixed_on_a_degenerate_area_does_not_underflow() {
        // u16::MAX request against a 1-cell and a 0-cell axis: must not panic
        // and must not wrap.
        for dir in [SplitDir::Vertical, SplitDir::Horizontal] {
            for fixed in [Fixed::First(u16::MAX), Fixed::Second(u16::MAX)] {
                for area in [
                    LayoutRect::new(0, 0, 0, 0),
                    LayoutRect::new(0, 0, 1, 1),
                    LayoutRect::new(0, 0, 2, 2),
                ] {
                    let tree = LayoutTree::split_fixed(dir, 0.5, fixed, leaf(0), leaf(1));
                    let rects = tree.window_rects(area);
                    let (a, b) = (rects[0].1, rects[1].1);
                    assert!(a.w <= area.w && b.w <= area.w, "child wider than parent");
                    assert!(a.h <= area.h && b.h <= area.h, "child taller than parent");
                }
            }
        }
    }

    #[test]
    fn fixed_nested_under_an_ordinary_split() {
        // (0 | dock-fixed(20 rows)) stacked over 1.
        let tree = hsplit(
            0.5,
            LayoutTree::split_fixed(SplitDir::Vertical, 0.5, Fixed::Second(20), leaf(0), leaf(9)),
            leaf(1),
        );
        let rects = tree.window_rects(LayoutRect::new(0, 0, 80, 24));
        assert_eq!(rects.len(), 3);
        let dock = rects.iter().find(|(id, _)| *id == 9).unwrap().1;
        assert_eq!(dock.w, 20, "nested fixed child keeps its exact width");
    }

    // ── mixed_layout_navigation ───────────────────────────────────────────────

    #[test]
    fn mixed_layout_navigation() {
        // Layout:
        //   ┌───┬───┐
        //   │ 0 │ 1 │
        //   ├───┴───┤
        //   │   2   │
        //   └───────┘
        let tree = hsplit(0.5, vsplit(0.5, leaf(0), leaf(1)), leaf(2));

        assert_eq!(tree.neighbor_right(0), Some(1));
        assert_eq!(tree.neighbor_left(1), Some(0));
        assert_eq!(tree.neighbor_right(1), None);
        assert_eq!(tree.neighbor_left(0), None);

        assert_eq!(tree.neighbor_below(0), Some(2));
        assert_eq!(tree.neighbor_below(1), Some(2));
        assert_eq!(tree.neighbor_below(2), None);
        assert_eq!(tree.neighbor_above(2), Some(1));
        assert_eq!(tree.neighbor_above(0), None);
        assert_eq!(tree.neighbor_above(1), None);

        assert_eq!(tree.next_leaf(0), Some(1));
        assert_eq!(tree.next_leaf(1), Some(2));
        assert_eq!(tree.next_leaf(2), Some(0));
        assert_eq!(tree.prev_leaf(0), Some(2));
        assert_eq!(tree.prev_leaf(2), Some(1));
    }

    // ── window_rects() ────────────────────────────────────────────────────────

    #[test]
    fn window_rects_single_leaf_gets_full_area() {
        let tree = leaf(0);
        let area = LayoutRect::new(0, 0, 80, 23);
        let rects = tree.window_rects(area);
        assert_eq!(rects, vec![(0, area)]);
    }

    #[test]
    fn window_rects_vsplit_two_side_by_side() {
        // Vertical split (side-by-side): area.w=80, ratio=0.5
        // a_w = round(80 * 0.5) = 40, clamped => 40
        // b_w = 80 - 40 = 40
        // Separator: rect_a.w(40) >= 2 and rect_b.w(40) > 0 → rect_a.w = 39
        // rect_a = (0,0,39,23), rect_b = (40,0,40,23)
        let tree = vsplit(0.5, leaf(0), leaf(1));
        let area = LayoutRect::new(0, 0, 80, 23);
        let rects = tree.window_rects(area);
        assert_eq!(rects.len(), 2);
        let (id_a, ra) = rects[0];
        let (id_b, rb) = rects[1];
        assert_eq!(id_a, 0);
        assert_eq!(id_b, 1);
        // widths: 39 + 1 (sep) + 40 = 80
        assert_eq!(
            ra.w + 1 + rb.w,
            80,
            "widths + separator must sum to parent width"
        );
        assert_eq!(ra.h, 23);
        assert_eq!(rb.h, 23);
        // rect_b starts right after rect_a + separator
        assert_eq!(rb.x, ra.x + ra.w + 1);
    }

    #[test]
    fn window_rects_hsplit_stacked() {
        // Horizontal split (stacked): area.h=23, ratio=0.5
        // a_h = round(23 * 0.5) = 12 (banker's rounding on some platforms; 11.5 → 12)
        // Actually: 23 * 0.5 = 11.5, round() = 12 in Rust (round half away from zero)
        // b_h = 23 - 12 = 11
        // Separator: rect_a.h(12) >= 2 and rect_b.h(11) > 0 → rect_a.h = 11
        let tree = hsplit(0.5, leaf(0), leaf(1));
        let area = LayoutRect::new(0, 0, 80, 23);
        let rects = tree.window_rects(area);
        assert_eq!(rects.len(), 2);
        let (_, ra) = rects[0];
        let (_, rb) = rects[1];
        // heights: ra.h + 1 (sep) + rb.h == area.h
        assert_eq!(
            ra.h + 1 + rb.h,
            23,
            "heights + separator must sum to parent height"
        );
        assert_eq!(ra.w, 80);
        assert_eq!(rb.w, 80);
        // rect_b starts after rect_a + separator row
        assert_eq!(rb.y, ra.y + ra.h + 1);
    }

    #[test]
    fn window_rects_nested_vsplit_inside_vsplit() {
        // vsplit(0.5, leaf(0), vsplit(0.5, leaf(1), leaf(2))) over 80x23
        // Outer: a_w=40, sep → rect_a.w=39; b_w=40 starting at x=40
        // Inner (over 40-wide area starting at x=40): a_w=20, sep → 19; b_w=20 at x=60
        let tree = vsplit(0.5, leaf(0), vsplit(0.5, leaf(1), leaf(2)));
        let area = LayoutRect::new(0, 0, 80, 23);
        let rects = tree.window_rects(area);
        assert_eq!(rects.len(), 3);
        // total width coverage: sum(widths) + 2 separators = 80
        let total_w: u16 = rects.iter().map(|(_, r)| r.w).sum::<u16>() + 2;
        assert_eq!(total_w, 80, "all window widths + 2 separators == 80");
    }

    // ── remove_leaf error message ─────────────────────────────────────────────

    #[test]
    fn remove_leaf_error_contains_e444() {
        let mut tree = leaf(0);
        let err = tree.remove_leaf(0).unwrap_err();
        assert!(err.contains("E444"), "error must mention E444, got: {err}");
    }

    // ── Layout update via enclosing_split_mut ─────────────────────────────────

    #[test]
    fn enclosing_split_mut_ratio_update_persists() {
        let mut tree = hsplit(0.5, leaf(0), leaf(1));
        {
            let (ratio, _, _) = tree.enclosing_split_mut(0, SplitDir::Horizontal).unwrap();
            *ratio = 0.75;
        }
        // Verify the ratio was actually mutated.
        if let LayoutTree::Split { ratio, .. } = &tree {
            assert!((*ratio - 0.75).abs() < 1e-5, "ratio should now be 0.75");
        } else {
            panic!("tree should still be a Split");
        }
    }
}

#[cfg(test)]
mod fixed_sizing_sweep {
    use super::*;

    /// Exhaustive sweep of the `Fixed` contract, added during review of the
    /// original implementation — which allocated cells rather than rendered
    /// cells, so `First(n)` came out one short while `Second(n)` was exact.
    /// A per-case example test let that through; only a sweep makes the
    /// invariant unmissable.
    ///
    /// Two properties, over axis lengths 0..40 and requests 0..45:
    ///
    /// - **Everywhere** (degenerate sizes included): neither child may exceed
    ///   the parent, and nothing may panic or underflow.
    /// - **Where the request is satisfiable** (axis holds two children plus
    ///   the separator): both variants render EXACTLY the requested size, so
    ///   a config-sourced width needs no caller-side compensation.
    ///
    /// Outside that domain the two sides legitimately differ: clamping has to
    /// pick a side to starve, and which one depends on the variant.
    #[test]
    fn fixed_renders_requested_size_and_never_exceeds_parent() {
        let mut asym = Vec::new();
        for len in 0u16..40 {
            for n in 0u16..45 {
                for dir in [SplitDir::Vertical, SplitDir::Horizontal] {
                    let area = match dir.axis() {
                        Axis::Col => LayoutRect::new(0, 0, len, 10),
                        Axis::Row => LayoutRect::new(0, 0, 10, len),
                    };
                    let ext = |r: LayoutRect| match dir.axis() {
                        Axis::Col => r.w,
                        Axis::Row => r.h,
                    };
                    let fa = split_geometry(area, dir, 0.5, Some(Fixed::First(n))).a;
                    let sb = split_geometry(area, dir, 0.5, Some(Fixed::Second(n))).b;
                    let (first, second) = (ext(fa), ext(sb));
                    // Meaningful domain only: the axis must hold two
                    // children plus the separator, and the request must fit.
                    let meaningful = len >= 3 && n >= 1 && n < len - 1;
                    if meaningful && first != second {
                        asym.push((len, n, format!("{dir:?}"), first, second));
                    }
                    if meaningful {
                        assert_eq!(first, n, "First({n}) on len {len} rendered {first}");
                        assert_eq!(second, n, "Second({n}) on len {len} rendered {second}");
                    }
                    assert!(first <= len && second <= len, "child exceeds parent");
                }
            }
        }
        assert!(
            asym.is_empty(),
            "First/Second render differently in {} cases, e.g. {:?}",
            asym.len(),
            &asym[..asym.len().min(6)]
        );
    }

    // ── split_geometry: the separator (#63 Phase 2) ───────────────────────────

    /// `a`, the separator and `b` must tile the parent exactly, with no gap and
    /// no overlap — for ratio splits and fixed splits alike. An off-by-one here
    /// is a divider drawn over a window's last column, or one the user can see
    /// but not grab.
    #[test]
    fn split_geometry_children_and_separator_tile_the_parent() {
        let area = LayoutRect::new(3, 5, 40, 20);
        let fixings = [
            None,
            Some(Fixed::First(10)),
            Some(Fixed::Second(10)),
            Some(Fixed::First(1)),
            Some(Fixed::Second(1)),
        ];
        for dir in [SplitDir::Vertical, SplitDir::Horizontal] {
            for fixed in fixings {
                let geo = split_geometry(area, dir, 0.5, fixed);
                let sep = geo
                    .separator
                    .unwrap_or_else(|| panic!("{dir:?}/{fixed:?} must fit a separator"));
                match dir.axis() {
                    Axis::Col => {
                        assert_eq!(geo.a.x, area.x, "a starts at the parent's left edge");
                        assert_eq!(sep.x, geo.a.x + geo.a.w, "separator abuts a's right edge");
                        assert_eq!(sep.w, 1, "separator is one column");
                        assert_eq!(geo.b.x, sep.x + 1, "b starts right after the separator");
                        assert_eq!(
                            geo.b.x + geo.b.w,
                            area.x + area.w,
                            "b reaches the right edge"
                        );
                        assert_eq!((sep.y, sep.h), (area.y, area.h), "separator spans the rows");
                    }
                    Axis::Row => {
                        assert_eq!(geo.a.y, area.y, "a starts at the parent's top edge");
                        assert_eq!(sep.y, geo.a.y + geo.a.h, "separator abuts a's bottom edge");
                        assert_eq!(sep.h, 1, "separator is one row");
                        assert_eq!(geo.b.y, sep.y + 1, "b starts right after the separator");
                        assert_eq!(
                            geo.b.y + geo.b.h,
                            area.y + area.h,
                            "b reaches the bottom edge"
                        );
                        assert_eq!(
                            (sep.x, sep.w),
                            (area.x, area.w),
                            "separator spans the columns"
                        );
                    }
                }
            }
        }
    }

    /// The separator is reported only when it is actually drawn: an area with
    /// no room for one yields `None`, not a phantom border cell.
    #[test]
    fn split_geometry_reports_no_separator_when_none_is_drawn() {
        // 1 column: `a` gets the single column, `b` gets nothing.
        let geo = split_geometry(LayoutRect::new(0, 0, 1, 10), SplitDir::Vertical, 0.5, None);
        assert_eq!(geo.separator, None);
        // Same along the row axis.
        let geo = split_geometry(
            LayoutRect::new(0, 0, 10, 1),
            SplitDir::Horizontal,
            0.5,
            None,
        );
        assert_eq!(geo.separator, None);
    }

    // ── Fixed splits refuse resizing (#63 Phase 2) ────────────────────────────

    /// A fixed split is never the target of a resize command: the search walks
    /// past it to the nearest resizable ancestor (vim's `winfixwidth`).
    #[test]
    fn enclosing_split_mut_skips_fixed_splits() {
        // Vertical(ratio 0.25) { leaf 9 , Vertical(fixed) { leaf 0, leaf 1 } }
        let inner = LayoutTree::Split {
            dir: SplitDir::Vertical,
            ratio: 0.5,
            fixed: Some(Fixed::First(20)),
            a: Box::new(LayoutTree::Leaf(0)),
            b: Box::new(LayoutTree::Leaf(1)),
            last_rect: Some(LayoutRect::new(20, 0, 60, 24)),
        };
        let mut tree = LayoutTree::Split {
            dir: SplitDir::Vertical,
            ratio: 0.25,
            fixed: None,
            a: Box::new(LayoutTree::Leaf(9)),
            b: Box::new(inner),
            last_rect: Some(LayoutRect::new(0, 0, 80, 24)),
        };

        let (ratio, rect, in_a) = tree
            .enclosing_split_mut(0, SplitDir::Vertical)
            .expect("the outer ratio split is still resizable");
        assert!(
            (*ratio - 0.25).abs() < 1e-5,
            "must be the OUTER split's ratio"
        );
        assert_eq!(rect, Some(LayoutRect::new(0, 0, 80, 24)));
        assert!(!in_a, "leaf 0 lives in the outer split's `b` branch");
    }

    /// When the *only* enclosing split is fixed there is nothing to resize —
    /// `<C-w><` becomes a no-op rather than silently rewriting a dead ratio.
    #[test]
    fn enclosing_split_mut_returns_none_for_a_lone_fixed_split() {
        let mut tree = LayoutTree::Split {
            dir: SplitDir::Vertical,
            ratio: 0.5,
            fixed: Some(Fixed::First(20)),
            a: Box::new(LayoutTree::Leaf(0)),
            b: Box::new(LayoutTree::Leaf(1)),
            last_rect: Some(LayoutRect::new(0, 0, 80, 24)),
        };
        assert!(tree.enclosing_split_mut(0, SplitDir::Vertical).is_none());
        assert!(tree.enclosing_split_mut(1, SplitDir::Vertical).is_none());
    }

    /// `for_each_ancestor` (maximize height/width) skips fixed splits too, so a
    /// dock can't be squashed to one cell by `<C-w>_` in a neighbouring pane.
    #[test]
    fn for_each_ancestor_skips_fixed_splits() {
        let inner = LayoutTree::Split {
            dir: SplitDir::Vertical,
            ratio: 0.7,
            fixed: Some(Fixed::First(20)),
            a: Box::new(LayoutTree::Leaf(1)),
            b: Box::new(LayoutTree::Leaf(2)),
            last_rect: Some(LayoutRect::new(0, 12, 80, 12)),
        };
        let mut tree = LayoutTree::Split {
            dir: SplitDir::Horizontal,
            ratio: 0.3,
            fixed: None,
            a: Box::new(LayoutTree::Leaf(0)),
            b: Box::new(inner),
            last_rect: Some(LayoutRect::new(0, 0, 80, 24)),
        };

        let mut seen = Vec::new();
        tree.for_each_ancestor(1, &mut |dir, ratio, _in_a, _rect| {
            seen.push((dir, *ratio));
            *ratio = 0.9;
        });
        assert_eq!(seen.len(), 1, "only the non-fixed ancestor is visited");
        assert_eq!(seen[0].0, SplitDir::Horizontal);
        // The fixed split's ratio survived untouched.
        let LayoutTree::Split { b, .. } = &tree else {
            panic!("expected a split at the root");
        };
        let LayoutTree::Split { ratio, .. } = b.as_ref() else {
            panic!("expected a split at b");
        };
        assert!((*ratio - 0.7).abs() < 1e-5, "fixed split's ratio untouched");
    }
}
