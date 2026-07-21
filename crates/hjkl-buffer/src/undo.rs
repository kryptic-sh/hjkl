//! Undo/redo entry type for per-buffer undo history.
//!
//! Lives in `hjkl-buffer` so that [`crate::Buffer`] can own the undo stack
//! directly, keeping per-buffer state co-located with the rope.

use std::collections::BTreeMap;
use std::time::SystemTime;

/// A single entry in the undo or redo stack.
///
/// The `timestamp` records the wall-clock time at which the snapshot was
/// taken (i.e. when `push_undo` was called), enabling the `:earlier` /
/// `:later` time-travel ex commands to walk the stack by duration rather
/// than by step count.
///
/// Stored as a `ropey::Rope` (O(1) Arc-clone) rather than a `String` so
/// snapshot cost is negligible even on multi-MB buffers.
#[derive(Debug, Clone)]
pub struct UndoEntry {
    pub rope: ropey::Rope,
    pub cursor: (usize, usize),
    pub timestamp: SystemTime,
    /// Local marks / jumplist / changelist / this-buffer's-global-marks
    /// snapshot, so undo/redo restore mark-ish positions alongside the
    /// text instead of leaving them shifted by the edit being undone
    /// (audit-r2 fix 2). `Default::default()` (all empty) for callers
    /// that don't populate it — restoring an all-empty snapshot is a
    /// no-op against a freshly-constructed buffer's own empty state, so
    /// existing fixtures that only care about text/cursor stay valid.
    pub marks: MarkSnapshot,
}

/// Buffer-scoped "edit coherence" state snapshotted alongside a
/// [`UndoEntry`]'s rope so undo/redo can restore marks, not just text.
///
/// Positions are plain `(row, col)` (or `(row, col)` values keyed by
/// mark char) — no buffer-id tagging needed here even for
/// `global_marks`, because a `MarkSnapshot` always belongs to exactly
/// one buffer's undo stack; the engine is responsible for reattaching
/// its own `buffer_id` when writing entries back into the session-global
/// marks map (see `Editor::restore_marks`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MarkSnapshot {
    /// `ma`-`mz` local marks (`View::marks_cloned`).
    pub local_marks: BTreeMap<char, (usize, usize)>,
    /// Back-jumplist (`Ctrl-o` stack), newest at the back.
    pub jump_back: Vec<(usize, usize)>,
    /// Forward-jumplist (`Ctrl-i` stack), newest at the back.
    pub jump_fwd: Vec<(usize, usize)>,
    /// `` `. ``  / `'.` — position of the most recent change.
    pub change_last_edit: Option<(usize, usize)>,
    /// Changelist ring (`g;` / `g,`).
    pub change_list: Vec<(usize, usize)>,
    /// Walk cursor into `change_list`; `None` outside a walk.
    pub change_cursor: Option<usize>,
    /// `mA`-`mZ` global marks that belong to THIS buffer (bare
    /// `(row, col)` — the buffer-id is implicit, this buffer).
    pub global_marks: BTreeMap<char, (usize, usize)>,
}

// ─── Undo arena tree (Phase 2a, docs/undo-architecture.md §3/§5) ──────────────
//
// The undo history is stored as an arena tree of state snapshots instead of the
// two linear `Vec<UndoEntry>` stacks it replaces. This slice is STRUCTURAL ONLY:
// it keeps behaviour byte-identical to the old two-stack model by never letting
// a node keep more than one child — every `push` drops the forward ("redo")
// branch, exactly as the old `clear_redo` did. Branch retention + `g-`/`g+`
// tree-walk semantics are a later slice (Phase 2b).
//
// Mapping from the old two Vecs onto the tree's single root→current→leaf path:
//
// - `current` points at the node representing the LIVE buffer state.
// - The ancestors of `current` (parent, grandparent, … up to `root`) are the
//   old `undo_stack`, oldest at `root`. `undo_stack.last()` == `current.parent`.
// - The descendant chain below `current` (`current.last_child`, its child, …)
//   is the old `redo_stack`, nearest-future first. `redo_stack.last()` ==
//   `current.last_child`.
// - `current`'s OWN snapshot is scratch (the live state): it is written on the
//   way past a node and never read as a restore target until it has been
//   written, so a placeholder there is safe (see the module tests).

/// Index into [`UndoTree::nodes`]. Slots are reused via a free list, so an id is
/// only valid while the node it names is live — the tree never hands ids out.
pub(crate) type NodeId = usize;

/// One node of the undo arena tree: a buffer state the user could land on, plus
/// its links. In this slice every node has at most one child (`children` holds
/// 0 or 1 id) because `push` drops the forward branch; the `Vec`/`last_child`
/// shape is kept so Phase 2b can allow real branches without a data change.
#[derive(Debug, Clone)]
pub(crate) struct UndoNode {
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub last_child: Option<NodeId>,
    pub snapshot: UndoEntry,
    #[allow(dead_code)] // load-bearing only for Phase 2b `g-`/`g+`; kept now.
    pub seq: u64,
}

/// Arena tree of [`UndoNode`]s. Replaces the old `undo_stack`/`redo_stack`
/// `Vec<UndoEntry>` pair on [`crate::Buffer`]; see the module comment for the
/// stack⇔tree mapping that keeps behaviour byte-identical.
#[derive(Debug)]
pub(crate) struct UndoTree {
    /// Slab; `None` slots are free and recorded in `free`.
    nodes: Vec<Option<UndoNode>>,
    /// Reusable slot indices (frees push here, allocs pop here first).
    free: Vec<NodeId>,
    root: NodeId,
    current: NodeId,
    next_seq: u64,
}

impl UndoTree {
    /// New tree with a single root == current node holding `rope` as its
    /// placeholder state. The root snapshot is never read as a restore target
    /// until the first `push` overwrites it (you cannot undo past the root), so
    /// the placeholder content is immaterial.
    pub(crate) fn new(rope: ropey::Rope) -> Self {
        let root = UndoNode {
            parent: None,
            children: Vec::new(),
            last_child: None,
            snapshot: UndoEntry {
                rope,
                cursor: (0, 0),
                timestamp: SystemTime::now(),
                marks: MarkSnapshot::default(),
            },
            seq: 0,
        };
        Self {
            nodes: vec![Some(root)],
            free: Vec::new(),
            root: 0,
            current: 0,
            next_seq: 1,
        }
    }

    // ── slab helpers ─────────────────────────────────────────────────────────

    fn get(&self, id: NodeId) -> &UndoNode {
        self.nodes[id].as_ref().expect("live NodeId")
    }

    fn get_mut(&mut self, id: NodeId) -> &mut UndoNode {
        self.nodes[id].as_mut().expect("live NodeId")
    }

    fn alloc(&mut self, node: UndoNode) -> NodeId {
        if let Some(id) = self.free.pop() {
            self.nodes[id] = Some(node);
            id
        } else {
            self.nodes.push(Some(node));
            self.nodes.len() - 1
        }
    }

    /// Free a single slot (does NOT recurse into children — callers detach
    /// links first). Reclaims the node's `UndoEntry` (its rope Arc-clone).
    fn free(&mut self, id: NodeId) {
        self.nodes[id] = None;
        self.free.push(id);
    }

    /// Free `id` and its whole subtree (iteratively, so a long redo chain can't
    /// overflow the stack).
    fn free_subtree(&mut self, id: NodeId) {
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            let kids = std::mem::take(&mut self.get_mut(n).children);
            stack.extend(kids);
            self.free(n);
        }
    }

    // ── read-only queries (mirror the old stack accessors) ───────────────────

    /// `undo_stack.is_empty()` ⇔ `current` has no parent (is the root).
    pub(crate) fn is_at_root(&self) -> bool {
        self.get(self.current).parent.is_none()
    }

    /// `!redo_stack.is_empty()` ⇔ `current` has a forward child.
    pub(crate) fn has_redo(&self) -> bool {
        self.get(self.current).last_child.is_some()
    }

    /// `undo_stack.len()` == number of ancestors of `current` (depth from root).
    pub(crate) fn depth(&self) -> usize {
        let mut d = 0;
        let mut n = self.get(self.current).parent;
        while let Some(p) = n {
            d += 1;
            n = self.get(p).parent;
        }
        d
    }

    /// `undo_stack.last().timestamp` == `current.parent`'s snapshot timestamp.
    pub(crate) fn parent_timestamp(&self) -> Option<SystemTime> {
        self.get(self.current)
            .parent
            .map(|p| self.get(p).snapshot.timestamp)
    }

    /// `redo_stack.last().timestamp` == `current.last_child`'s timestamp.
    pub(crate) fn child_timestamp(&self) -> Option<SystemTime> {
        self.get(self.current)
            .last_child
            .map(|c| self.get(c).snapshot.timestamp)
    }

    // ── mutations ────────────────────────────────────────────────────────────

    /// `undo_stack.push(entry)` + `redo_stack.clear()`.
    ///
    /// `entry` is the pre-edit LIVE state. It is committed as `current`'s
    /// snapshot (making `current` the new `undo_stack.last()`), `current`'s
    /// forward branch is dropped (the redo clear), and a fresh child becomes the
    /// new `current` for the edit that is about to happen.
    pub(crate) fn push(&mut self, entry: UndoEntry) {
        let cur = self.current;
        self.get_mut(cur).snapshot = entry.clone();
        // Drop the old redo branch (byte-identical to the old clear_redo).
        let old_children = std::mem::take(&mut self.get_mut(cur).children);
        self.get_mut(cur).last_child = None;
        for c in old_children {
            self.free_subtree(c);
        }
        let seq = self.next_seq;
        self.next_seq += 1;
        let child = self.alloc(UndoNode {
            parent: Some(cur),
            children: Vec::new(),
            last_child: None,
            snapshot: entry,
            seq,
        });
        let cur_node = self.get_mut(cur);
        cur_node.children.push(child);
        cur_node.last_child = Some(child);
        self.current = child;
    }

    /// One undo step. `live` is the current buffer state (the node being left);
    /// it is written into that node but INHERITS the destination (parent)
    /// timestamp — byte-parity with the old dance, where the pushed redo entry
    /// took the popped undo entry's timestamp. Returns the parent snapshot to
    /// restore, or `None` at the root.
    pub(crate) fn undo_step(
        &mut self,
        rope: ropey::Rope,
        cursor: (usize, usize),
        marks: MarkSnapshot,
    ) -> Option<UndoEntry> {
        let cur = self.current;
        let par = self.get(cur).parent?;
        let dest_ts = self.get(par).snapshot.timestamp;
        self.get_mut(cur).snapshot = UndoEntry {
            rope,
            cursor,
            timestamp: dest_ts,
            marks,
        };
        // Redo from the parent must return to the node we just left.
        self.get_mut(par).last_child = Some(cur);
        self.current = par;
        Some(self.get(par).snapshot.clone())
    }

    /// One redo step. Symmetric to [`Self::undo_step`]: `live` is written into
    /// the node being left (which becomes an undo ancestor) with the
    /// destination (child) timestamp. Returns the child snapshot to restore, or
    /// `None` when there is no forward branch.
    pub(crate) fn redo_step(
        &mut self,
        rope: ropey::Rope,
        cursor: (usize, usize),
        marks: MarkSnapshot,
    ) -> Option<UndoEntry> {
        let cur = self.current;
        let child = self.get(cur).last_child?;
        let dest_ts = self.get(child).snapshot.timestamp;
        self.get_mut(cur).snapshot = UndoEntry {
            rope,
            cursor,
            timestamp: dest_ts,
            marks,
        };
        self.current = child;
        Some(self.get(child).snapshot.clone())
    }

    /// `undo_stack.pop()` — discard the most-recent undo boundary WITHOUT moving
    /// the live state: splice `current`'s parent out of the ancestor chain,
    /// reconnecting `current` to its grandparent. Used by `:s` with zero
    /// replacements and by a no-op undo group. Returns `false` at the root.
    pub(crate) fn pop_committed(&mut self) -> bool {
        let cur = self.current;
        let Some(par) = self.get(cur).parent else {
            return false;
        };
        let grand = self.get(par).parent;
        self.get_mut(cur).parent = grand;
        match grand {
            Some(g) => {
                let g_node = self.get_mut(g);
                if let Some(slot) = g_node.children.iter_mut().find(|c| **c == par) {
                    *slot = cur;
                }
                if g_node.last_child == Some(par) {
                    g_node.last_child = Some(cur);
                }
            }
            None => self.root = cur,
        }
        // `par` has only `cur` as a child (linear invariant) and `cur` is kept —
        // free just the spliced-out slot.
        self.free(par);
        true
    }

    /// `if len > cap { undo_stack.drain(..len-cap) }` — a node budget. Prunes
    /// the oldest ancestors (root side); never touches `current` or the redo
    /// branch. `cap == 0` means unlimited (matches the old guard).
    pub(crate) fn cap(&mut self, cap: usize) {
        if cap == 0 {
            return;
        }
        let mut depth = self.depth();
        while depth > cap {
            let root = self.root;
            // Linear tree: the root has exactly one child, on the path to
            // `current`. Promote it to the new root and drop the old root.
            let child = self
                .get(root)
                .last_child
                .expect("depth > 0 implies the root has a child");
            self.get_mut(child).parent = None;
            self.root = child;
            self.free(root);
            depth -= 1;
        }
    }

    /// `redo_stack.clear()` — drop `current`'s forward branch.
    pub(crate) fn clear_redo(&mut self) {
        let cur = self.current;
        let kids = std::mem::take(&mut self.get_mut(cur).children);
        self.get_mut(cur).last_child = None;
        for c in kids {
            self.free_subtree(c);
        }
    }

    /// `undo_stack.clear(); redo_stack.clear()` — collapse to a single root ==
    /// current node, preserving the live state. Frees every other node.
    pub(crate) fn clear_all(&mut self) {
        let cur = self.current;
        for id in 0..self.nodes.len() {
            if id != cur && self.nodes[id].is_some() {
                self.nodes[id] = None;
                self.free.push(id);
            }
        }
        let node = self.get_mut(cur);
        node.parent = None;
        node.children.clear();
        node.last_child = None;
        self.root = cur;
    }
}

#[cfg(test)]
mod tree_tests {
    use super::*;

    fn entry(text: &str) -> UndoEntry {
        UndoEntry {
            rope: ropey::Rope::from_str(text),
            cursor: (0, 0),
            timestamp: SystemTime::now(),
            marks: MarkSnapshot::default(),
        }
    }

    fn live(text: &str) -> (ropey::Rope, (usize, usize), MarkSnapshot) {
        (ropey::Rope::from_str(text), (0, 0), MarkSnapshot::default())
    }

    #[test]
    fn fresh_tree_is_root_current_empty() {
        let t = UndoTree::new(ropey::Rope::from_str("hello"));
        assert!(t.is_at_root());
        assert!(!t.has_redo());
        assert_eq!(t.depth(), 0);
        assert_eq!(t.root, t.current);
    }

    #[test]
    fn push_links_child_and_advances_current() {
        let mut t = UndoTree::new(ropey::Rope::from_str("hello"));
        let root = t.current;
        t.push(entry("hello"));
        // root now parents current; current is a fresh leaf.
        assert_eq!(t.get(t.current).parent, Some(root));
        assert_eq!(t.get(root).last_child, Some(t.current));
        assert_eq!(t.get(root).children, vec![t.current]);
        assert_eq!(t.depth(), 1);
        assert!(!t.has_redo());
        assert!(!t.is_at_root());
    }

    #[test]
    fn undo_then_redo_round_trips_links() {
        let mut t = UndoTree::new(ropey::Rope::from_str("s0"));
        t.push(entry("s0")); // commit s0, current = n1 (live s1)
        let n0 = t.root;
        let n1 = t.current;
        // undo: current -> n0, restores s0.
        let (r, c, m) = live("s1");
        let restored = t.undo_step(r, c, m).unwrap();
        assert_eq!(restored.rope.to_string(), "s0");
        assert_eq!(t.current, n0);
        assert!(t.has_redo());
        assert_eq!(t.get(n0).last_child, Some(n1));
        // redo: current -> n1, restores what we left (s1).
        let (r, c, m) = live("s0");
        let restored = t.redo_step(r, c, m).unwrap();
        assert_eq!(restored.rope.to_string(), "s1");
        assert_eq!(t.current, n1);
        assert!(!t.has_redo());
    }

    #[test]
    fn undo_at_root_and_redo_at_leaf_are_noops() {
        let mut t = UndoTree::new(ropey::Rope::from_str("x"));
        let (r, c, m) = live("x");
        assert!(t.undo_step(r, c, m).is_none());
        let (r, c, m) = live("x");
        assert!(t.redo_step(r, c, m).is_none());
        assert_eq!(t.depth(), 0);
    }

    #[test]
    fn push_drops_forward_branch() {
        let mut t = UndoTree::new(ropey::Rope::from_str("s0"));
        t.push(entry("s0")); // -> live s1
        let (r, c, m) = live("s1");
        t.undo_step(r, c, m); // back to s0, redo available
        assert!(t.has_redo());
        // A new edit from here drops the redo branch (linear behaviour).
        t.push(entry("s0"));
        assert!(!t.has_redo());
        assert_eq!(t.depth(), 1);
        // The old redo node's slot was reclaimed (then reused by the new
        // child): only root + current remain live, no leak.
        let live = t.nodes.iter().filter(|n| n.is_some()).count();
        assert_eq!(live, 2);
    }

    #[test]
    fn cap_prunes_oldest_from_root_side() {
        let mut t = UndoTree::new(ropey::Rope::from_str("s"));
        for _ in 0..5 {
            t.push(entry("s"));
        }
        assert_eq!(t.depth(), 5);
        t.cap(3);
        assert_eq!(t.depth(), 3);
        // Redo side untouched (there is none), current unchanged.
        assert!(!t.has_redo());
        // Two oldest slots were reclaimed.
        assert_eq!(t.free.len(), 2);
    }

    #[test]
    fn pop_committed_splices_parent_keeping_live() {
        let mut t = UndoTree::new(ropey::Rope::from_str("s0"));
        t.push(entry("s0")); // depth 1, current = fresh leaf
        assert_eq!(t.depth(), 1);
        assert!(t.pop_committed());
        // The just-pushed boundary is gone; live node preserved as new root.
        assert_eq!(t.depth(), 0);
        assert!(t.is_at_root());
        assert_eq!(t.free.len(), 1);
    }

    #[test]
    fn pop_committed_at_root_is_false() {
        let mut t = UndoTree::new(ropey::Rope::from_str("s"));
        assert!(!t.pop_committed());
    }

    #[test]
    fn clear_redo_drops_forward_only() {
        let mut t = UndoTree::new(ropey::Rope::from_str("s0"));
        t.push(entry("s0"));
        let (r, c, m) = live("s1");
        t.undo_step(r, c, m);
        assert!(t.has_redo());
        assert_eq!(t.depth(), 0);
        t.clear_redo();
        assert!(!t.has_redo());
        assert_eq!(t.depth(), 0);
    }

    #[test]
    fn clear_all_collapses_to_single_node() {
        let mut t = UndoTree::new(ropey::Rope::from_str("s"));
        for _ in 0..3 {
            t.push(entry("s"));
        }
        t.clear_all();
        assert!(t.is_at_root());
        assert!(!t.has_redo());
        assert_eq!(t.depth(), 0);
        assert_eq!(t.root, t.current);
    }
}
