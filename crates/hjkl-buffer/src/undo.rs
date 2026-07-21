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

// ─── Reversible edge delta (Phase 3a, docs/undo-architecture.md §3/§6) ─────────
//
// Phase 2b stored a FULL rope snapshot on every node. Phase 3a stores only a
// reversible **delta** on each parent→child edge (the root keeps a full base
// rope) plus a materialization cache, so the in-RAM hot path stays snapshot-fast
// while a future undofile shrinks from hundreds of MB to KB. This slice changes
// ONLY internal storage — every public signature, and every observable
// behaviour, is byte-identical to Phase 2b.

/// A reversible edit between two adjacent buffer states, expressed as a single
/// spanning replacement in **char-offset space** on the rope.
///
/// The index space is ropey `char` offsets throughout — never bytes — so
/// multi-byte UTF-8 round-trips (a byte offset could split a codepoint). In the
/// PARENT state `chars[start .. start + old.chars().count()] == old`; replacing
/// that region with `new` yields the CHILD state, and swapping the two inverts
/// it. A whole undo group collapses to the one region spanning its edits
/// (common-prefix / common-suffix diff); a `Vec<Delta>` for disjoint regions is
/// an acceptable future generalization, but one spanning region is all Phase 3a
/// needs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Delta {
    /// Char offset of the first differing char (the common-prefix length).
    pub start: usize,
    /// Chars present in the PARENT but not the CHILD (removed going forward).
    pub old: String,
    /// Chars present in the CHILD but not the PARENT (inserted going forward).
    pub new: String,
}

/// Common-prefix / common-suffix diff of two ropes → the minimal single spanning
/// [`Delta`]. Guarantees `apply_forward(a, diff(a, b)) == b` and
/// `apply_inverse(b, diff(a, b)) == a` for ALL `a`, `b` (see the property
/// tests). Boundaries are found on bytes (fast) then snapped to char boundaries
/// so `old`/`new` are always valid UTF-8 and `start` is a true char offset.
fn diff(parent: &ropey::Rope, child: &ropey::Rope) -> Delta {
    let a = parent.to_string();
    let b = child.to_string();
    let ab = a.as_bytes();
    let bb = b.as_bytes();

    // Longest common byte prefix, snapped DOWN to a char boundary.
    let max_pre = ab.len().min(bb.len());
    let mut pre = 0;
    while pre < max_pre && ab[pre] == bb[pre] {
        pre += 1;
    }
    while pre > 0 && !a.is_char_boundary(pre) {
        pre -= 1;
    }

    // Longest common byte suffix not overlapping the prefix. The cut points
    // `a_end`/`b_end` sit at identical trailing bytes, so snapping `a_end` UP to
    // a char boundary snaps `b_end` by the same byte delta simultaneously.
    let max_suf = max_pre - pre;
    let mut suf = 0;
    while suf < max_suf && ab[ab.len() - 1 - suf] == bb[bb.len() - 1 - suf] {
        suf += 1;
    }
    let mut a_end = ab.len() - suf;
    while a_end < ab.len() && !a.is_char_boundary(a_end) {
        a_end += 1;
    }
    let b_end = bb.len() - (ab.len() - a_end);

    Delta {
        start: a[..pre].chars().count(),
        old: a[pre..a_end].to_string(),
        new: b[pre..b_end].to_string(),
    }
}

/// Apply a forward delta (PARENT → CHILD) to `parent`, returning the child rope.
fn apply_forward(parent: &ropey::Rope, d: &Delta) -> ropey::Rope {
    let mut r = parent.clone();
    let old_chars = d.old.chars().count();
    r.remove(d.start..d.start + old_chars);
    r.insert(d.start, &d.new);
    r
}

/// Apply an inverse delta (CHILD → PARENT) to `child`, returning the parent rope.
fn apply_inverse(child: &ropey::Rope, d: &Delta) -> ropey::Rope {
    let mut r = child.clone();
    let new_chars = d.new.chars().count();
    r.remove(d.start..d.start + new_chars);
    r.insert(d.start, &d.old);
    r
}

// ─── Undo arena tree (Phase 2b + Phase 3a delta storage) ──────────────────────
//
// The undo history is a real arena TREE of buffer states (Phase 2a introduced
// the arena; Phase 2b makes it branch; Phase 3a stores edges as deltas). An edit
// after an undo FORKS a new child instead of truncating the forward branch, so
// old branches stay reachable — matching nvim's undo tree. `seq` is
// load-bearing: `g-`/`g+` and the `:earlier`/`:later` count forms walk ALL
// states by global `seq` (see `seq_earlier_step`/`seq_later_step`), while
// `u`/`<C-r>` stay branch-local (parent / `last_child`).
//
// The linear-history subset is unchanged: with no forks the tree is a single
// root→current→leaf path and every operation degrades to the old two-stack
// behaviour.
//
// - `current` points at the node representing the LIVE buffer state.
// - The ancestors of `current` (parent, … up to `root`) are the reachable undo
//   line; `current.parent` is the `u` target.
// - `current.last_child` is the `<C-r>` target. Landing on any node (undo,
//   redo, or a `g-`/`g+` jump) rewrites `last_child` down the root→node path so
//   a later `<C-r>` retraces the branch just taken.
//
// Storage (Phase 3a): each non-root node holds the reversible `delta` on its
// edge from `parent`; the root holds a full `base` rope. A node's content is
// reconstructed on demand (`materialize`) from the nearest cached ancestor (or
// the root base) by replaying forward deltas, or — for the `u`/`<C-r>` hot path
// — from the adjacent warm node by one delta apply. Recently materialized ropes
// are kept in a bounded LRU (`warm`); `current` is always kept warm. A node's
// `delta`/content is FINALIZED lazily on the way past it (whenever the live rope
// is written into it), never read as a restore target until then — so the fresh
// leaf `current` holds a placeholder edge that is corrected before it matters.

/// Index into [`UndoTree::nodes`]. Slots are reused via a free list, so an id is
/// only valid while the node it names is live — the tree never hands ids out.
pub(crate) type NodeId = usize;

/// How many recently-materialized node ropes to keep warm (besides the root
/// base and `current`, which are always available). A cold jump beyond this
/// window replays deltas from the nearest warm ancestor — rare and bounded.
const WARM_CAP: usize = 16;

/// One node of the undo arena tree: a buffer state the user could land on, plus
/// its links and the reversible edge to its parent. A node with `> 1` child is a
/// branch point (Phase 2b); `last_child` records which child `<C-r>` follows.
#[derive(Debug, Clone)]
pub(crate) struct UndoNode {
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub last_child: Option<NodeId>,
    /// Reversible edit from the parent's content to this node's content. `None`
    /// only for the root (and any node promoted to root by pruning), which holds
    /// `base` instead.
    pub delta: Option<Delta>,
    /// Full base rope. `Some` ONLY for the root — the anchor the delta chain
    /// replays from. Non-root nodes leave this `None` and carry a `delta`.
    pub base: Option<ropey::Rope>,
    /// Materialized content, LRU-managed. Warm for `current` and recently
    /// visited nodes; `None` (cold) otherwise, reconstructable from deltas.
    pub rope_cache: Option<ropey::Rope>,
    /// Post-state cursor for this node (restored alongside the text).
    pub cursor: (usize, usize),
    /// Wall-clock time this state was created — drives `:earlier`/`:later`.
    pub timestamp: SystemTime,
    /// Marks / jumplist / changelist snapshot restored with the text.
    pub marks: MarkSnapshot,
    /// Global monotonic order across the whole tree — the change number that
    /// `g-`/`g+`, `:earlier`/`:later`, and `:undolist` traverse and display.
    pub seq: u64,
}

/// Arena tree of [`UndoNode`]s. Replaces the old `undo_stack`/`redo_stack`
/// `Vec<UndoEntry>` pair on [`crate::Buffer`]; see the module comment for how
/// `u`/`<C-r>` (branch-local) and `g-`/`g+` (seq-ordered) map onto it, and how
/// Phase 3a stores edges as deltas behind a materialization cache.
#[derive(Debug)]
pub(crate) struct UndoTree {
    /// Slab; `None` slots are free and recorded in `free`.
    nodes: Vec<Option<UndoNode>>,
    /// Reusable slot indices (frees push here, allocs pop here first).
    free: Vec<NodeId>,
    /// LRU of node ids with a warm `rope_cache` (root excluded — it uses
    /// `base`), most-recently-touched last. Bounded by [`WARM_CAP`]; `current`
    /// is never evicted.
    warm: Vec<NodeId>,
    root: NodeId,
    current: NodeId,
    next_seq: u64,
}

impl UndoTree {
    /// New tree with a single root == current node holding `rope` as its base
    /// state (the buffer as opened / last saved). The root is always
    /// materializable from this base.
    pub(crate) fn new(rope: ropey::Rope) -> Self {
        let root = UndoNode {
            parent: None,
            children: Vec::new(),
            last_child: None,
            delta: None,
            base: Some(rope),
            rope_cache: None,
            cursor: (0, 0),
            timestamp: SystemTime::now(),
            marks: MarkSnapshot::default(),
            seq: 0,
        };
        Self {
            nodes: vec![Some(root)],
            free: Vec::new(),
            warm: Vec::new(),
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
    /// links first). Drops the node's delta + materialized cache and purges it
    /// from the warm LRU.
    fn free(&mut self, id: NodeId) {
        self.nodes[id] = None;
        self.free.push(id);
        self.warm.retain(|&n| n != id);
    }

    // ── materialization (Phase 3a) ────────────────────────────────────────────

    /// Record `id` as freshly materialized, evicting the coldest cache beyond
    /// [`WARM_CAP`] (never the root — it has no cache — nor `current`).
    fn touch_warm(&mut self, id: NodeId) {
        if id == self.root {
            return;
        }
        self.warm.retain(|&n| n != id);
        self.warm.push(id);
        while self.warm.len() > WARM_CAP {
            let Some(pos) = self.warm.iter().position(|&n| n != self.current) else {
                break;
            };
            let victim = self.warm.remove(pos);
            if let Some(node) = self.nodes[victim].as_mut() {
                node.rope_cache = None;
            }
        }
    }

    /// Materialize node `id`'s content, warming its cache. Uses the warm cache
    /// if present, else the root `base`, else replays forward deltas from the
    /// nearest materialized ancestor (or the root). Always terminates: the root
    /// carries a base.
    fn materialize(&mut self, id: NodeId) -> ropey::Rope {
        if let Some(r) = &self.get(id).rope_cache {
            return r.clone();
        }
        if let Some(base) = &self.get(id).base {
            return base.clone();
        }
        // Walk up to the nearest ancestor that is warm or is the root, recording
        // the path of nodes to replay forward.
        let mut path = Vec::new();
        let base_rope;
        let mut anchor = id;
        loop {
            path.push(anchor);
            let par = self
                .get(anchor)
                .parent
                .expect("a non-root, non-based node always has a parent");
            if let Some(r) = &self.get(par).rope_cache {
                base_rope = r.clone();
                break;
            }
            if let Some(b) = &self.get(par).base {
                base_rope = b.clone();
                break;
            }
            anchor = par;
        }
        let mut rope = base_rope;
        for &node in path.iter().rev() {
            let d = self
                .get(node)
                .delta
                .clone()
                .expect("a non-root node always carries its edge delta");
            rope = apply_forward(&rope, &d);
        }
        self.get_mut(id).rope_cache = Some(rope.clone());
        self.touch_warm(id);
        rope
    }

    /// Reconstruct node `id`'s restorable [`UndoEntry`] — the byte-for-byte
    /// equivalent of Phase 2b's `node.snapshot.clone()`.
    fn entry_of(&mut self, id: NodeId) -> UndoEntry {
        let rope = self.materialize(id);
        let n = self.get(id);
        UndoEntry {
            rope,
            cursor: n.cursor,
            timestamp: n.timestamp,
            marks: n.marks.clone(),
        }
    }

    /// Finalize node `id` to hold `rope` as its content, recomputing its edge
    /// delta (or the root base) and updating cursor/timestamp/marks. A no-op
    /// diff is skipped when the content is unchanged (the common case on a
    /// history walk, where only the fields move) — which also avoids
    /// materializing the parent, keeping the walk cheap.
    fn set_node_state(
        &mut self,
        id: NodeId,
        rope: ropey::Rope,
        cursor: (usize, usize),
        timestamp: SystemTime,
        marks: MarkSnapshot,
    ) {
        let is_root = self.get(id).parent.is_none();
        let unchanged = self.get(id).rope_cache.as_ref() == Some(&rope)
            || (is_root && self.get(id).base.as_ref() == Some(&rope));
        {
            let node = self.get_mut(id);
            node.cursor = cursor;
            node.timestamp = timestamp;
            node.marks = marks;
        }
        if unchanged {
            return;
        }
        if is_root {
            self.get_mut(id).base = Some(rope);
            // The root is materialized from `base`; keep no stale cache.
            self.get_mut(id).rope_cache = None;
            self.warm.retain(|&n| n != id);
        } else {
            let par = self.get(id).parent.expect("non-root has a parent");
            let par_rope = self.materialize(par);
            let d = diff(&par_rope, &rope);
            let node = self.get_mut(id);
            node.delta = Some(d);
            node.rope_cache = Some(rope);
            self.touch_warm(id);
        }
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

    /// `undo_stack.last().timestamp` == `current.parent`'s timestamp.
    pub(crate) fn parent_timestamp(&self) -> Option<SystemTime> {
        self.get(self.current).parent.map(|p| self.get(p).timestamp)
    }

    /// `redo_stack.last().timestamp` == `current.last_child`'s timestamp.
    pub(crate) fn child_timestamp(&self) -> Option<SystemTime> {
        self.get(self.current)
            .last_child
            .map(|c| self.get(c).timestamp)
    }

    // ── mutations ────────────────────────────────────────────────────────────

    /// Commit a new boundary from `current`, growing the tree (Phase 2b).
    ///
    /// `entry` is the pre-edit LIVE state. It is written into `current`'s
    /// snapshot (making `current` a real, restorable state), then a fresh child
    /// is APPENDED and becomes the new `current` for the edit about to happen.
    ///
    /// Unlike Phase 2a this does NOT drop `current`'s existing children: an edit
    /// after an undo now forks a new branch and the old forward branch(es) stay
    /// reachable via `g-`/`g+` and `:undolist`, matching nvim's undo tree. The
    /// new child is made `last_child` so a subsequent `<C-r>` follows the branch
    /// just created.
    pub(crate) fn push(&mut self, entry: UndoEntry) {
        let cur = self.current;
        // Finalize the node being left with the pre-edit live state, recomputing
        // its edge delta from its parent (or the root base).
        self.set_node_state(
            cur,
            entry.rope.clone(),
            entry.cursor,
            entry.timestamp,
            entry.marks.clone(),
        );
        let seq = self.next_seq;
        self.next_seq += 1;
        // Fresh child: identical to `cur` for now (empty edge delta + warm cache
        // holding the pre-edit rope). Its true post-edit content is finalized on
        // the way past it (next move) or by the next `push`, at which point the
        // edge delta is recomputed against `cur`.
        let child = self.alloc(UndoNode {
            parent: Some(cur),
            children: Vec::new(),
            last_child: None,
            delta: Some(Delta::default()),
            base: None,
            rope_cache: Some(entry.rope),
            cursor: entry.cursor,
            timestamp: entry.timestamp,
            marks: entry.marks,
            seq,
        });
        let cur_node = self.get_mut(cur);
        // Append (retain old branches); the freshest child is the redo target.
        cur_node.children.push(child);
        cur_node.last_child = Some(child);
        self.current = child;
        self.touch_warm(child);
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
        let dest_ts = self.get(par).timestamp;
        self.set_node_state(cur, rope, cursor, dest_ts, marks);
        // Redo from the parent must return to the node we just left.
        self.get_mut(par).last_child = Some(cur);
        self.current = par;
        // Hot-path materialization: derive the (possibly cold) parent from the
        // just-finalized child by one inverse delta apply, so `u` never walks the
        // ancestor chain even far outside the warm window.
        if self.get(par).rope_cache.is_none() && self.get(par).base.is_none() {
            let child_rope = self.get(cur).rope_cache.clone();
            let child_delta = self.get(cur).delta.clone();
            if let (Some(cr), Some(d)) = (child_rope, child_delta) {
                let par_rope = apply_inverse(&cr, &d);
                self.get_mut(par).rope_cache = Some(par_rope);
                self.touch_warm(par);
            }
        }
        Some(self.entry_of(par))
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
        let dest_ts = self.get(child).timestamp;
        self.set_node_state(cur, rope, cursor, dest_ts, marks);
        self.current = child;
        // `cur` is now warm, so materializing the child is one forward apply.
        Some(self.entry_of(child))
    }

    // ── seq-ordered tree walk (`g-` / `g+`, `:earlier`/`:later` — Phase 2b) ───
    //
    // `u`/`<C-r>` are branch-local (parent / `last_child`); `g-`/`g+` traverse
    // ALL states by global `seq`, crossing branch boundaries. `g-` restores the
    // node with the greatest `seq` strictly below `current`'s; `g+` the least
    // `seq` strictly above. Confirmed against nvim v0.12.4 (`iA<Esc>uiB<Esc>`
    // then `g-`/`g-g-`/`g-g+` walks empty↔A↔B by change number).

    /// `seq` of the node the buffer currently shows.
    fn current_seq(&self) -> u64 {
        self.get(self.current).seq
    }

    /// Live node with the greatest `seq` strictly below `s` (the `g-` target).
    fn node_below(&self, s: u64) -> Option<NodeId> {
        let mut best: Option<(u64, NodeId)> = None;
        for (id, slot) in self.nodes.iter().enumerate() {
            if let Some(n) = slot
                && n.seq < s
                && best.is_none_or(|(bs, _)| n.seq > bs)
            {
                best = Some((n.seq, id));
            }
        }
        best.map(|(_, id)| id)
    }

    /// Live node with the least `seq` strictly above `s` (the `g+` target).
    fn node_above(&self, s: u64) -> Option<NodeId> {
        let mut best: Option<(u64, NodeId)> = None;
        for (id, slot) in self.nodes.iter().enumerate() {
            if let Some(n) = slot
                && n.seq > s
                && best.is_none_or(|(bs, _)| n.seq < bs)
            {
                best = Some((n.seq, id));
            }
        }
        best.map(|(_, id)| id)
    }

    /// Point `current` at `target` and rewrite `last_child` down the whole
    /// root→target path, so a later `<C-r>` retraces the branch just landed on
    /// (nvim parity: landing on a node updates its ancestors' redo direction).
    fn retarget_current(&mut self, target: NodeId) {
        self.current = target;
        let mut node = target;
        while let Some(p) = self.get(node).parent {
            self.get_mut(p).last_child = Some(node);
            node = p;
        }
    }

    /// Stash the live buffer state into the node being left (it may be a fresh,
    /// still-stale leaf), preserving that node's own timestamp, then move.
    fn stash_and_move(
        &mut self,
        target: NodeId,
        rope: ropey::Rope,
        cursor: (usize, usize),
        marks: MarkSnapshot,
    ) {
        let cur = self.current;
        let ts = self.get(cur).timestamp;
        self.set_node_state(cur, rope, cursor, ts, marks);
        self.retarget_current(target);
    }

    /// One `g-` / `:earlier` step: move to the next-lower-`seq` node tree-wide.
    /// Returns its snapshot to restore, or `None` at the lowest state.
    pub(crate) fn seq_earlier_step(
        &mut self,
        rope: ropey::Rope,
        cursor: (usize, usize),
        marks: MarkSnapshot,
    ) -> Option<UndoEntry> {
        let target = self.node_below(self.current_seq())?;
        self.stash_and_move(target, rope, cursor, marks);
        Some(self.entry_of(target))
    }

    /// One `g+` / `:later` step: move to the next-higher-`seq` node tree-wide.
    /// Returns its snapshot to restore, or `None` at the highest state.
    pub(crate) fn seq_later_step(
        &mut self,
        rope: ropey::Rope,
        cursor: (usize, usize),
        marks: MarkSnapshot,
    ) -> Option<UndoEntry> {
        let target = self.node_above(self.current_seq())?;
        self.stash_and_move(target, rope, cursor, marks);
        Some(self.entry_of(target))
    }

    /// Timestamp of the next-lower-`seq` node (the `:earlier Ns` predicate walks
    /// the seq order tree-wide, stopping once this dips to/below the cutoff).
    pub(crate) fn seq_earlier_timestamp(&self) -> Option<SystemTime> {
        self.node_below(self.current_seq())
            .map(|id| self.get(id).timestamp)
    }

    /// Timestamp of the next-higher-`seq` node (the `:later Ns` predicate).
    pub(crate) fn seq_later_timestamp(&self) -> Option<SystemTime> {
        self.node_above(self.current_seq())
            .map(|id| self.get(id).timestamp)
    }

    /// Leaves of the tree (nodes with no children), each as
    /// `(seq, depth-from-root, timestamp, is_current)`, sorted by `seq`.
    /// Drives `:undolist`, which — like nvim — lists only branch leaves.
    pub(crate) fn leaves(&self) -> Vec<(u64, usize, SystemTime, bool)> {
        let mut out: Vec<(u64, usize, SystemTime, bool)> = Vec::new();
        for (id, slot) in self.nodes.iter().enumerate() {
            let Some(n) = slot else { continue };
            // The root is the base state (change number 0), never a listed
            // "change" — like nvim, an untouched buffer lists nothing.
            if id == self.root || !n.children.is_empty() {
                continue;
            }
            // Depth = number of ancestors (root leaf ⇒ 0).
            let mut depth = 0;
            let mut p = n.parent;
            while let Some(pid) = p {
                depth += 1;
                p = self.get(pid).parent;
            }
            out.push((n.seq, depth, n.timestamp, id == self.current));
        }
        out.sort_by_key(|&(seq, ..)| seq);
        out
    }

    /// Number of live nodes (used by [`Self::cap`] as the state budget).
    fn live_count(&self) -> usize {
        self.nodes.iter().filter(|n| n.is_some()).count()
    }

    /// `undo_stack.pop()` — discard the most-recent boundary WITHOUT moving the
    /// live state. Used by `:s` with zero replacements and by a no-op undo
    /// group; in both, `current` is the childless leaf the last [`Self::push`]
    /// created, so reverse that push: drop the leaf, step `current` back to its
    /// parent (its snapshot equals the unchanged buffer), and restore the
    /// parent's `last_child`. Retains any sibling branches the push appended to.
    /// Returns `false` at the root, or if `current` is not a childless leaf
    /// (nothing safe to pop).
    pub(crate) fn pop_committed(&mut self) -> bool {
        let cur = self.current;
        if !self.get(cur).children.is_empty() {
            return false;
        }
        let Some(par) = self.get(cur).parent else {
            return false;
        };
        let par_node = self.get_mut(par);
        par_node.children.retain(|&c| c != cur);
        // The freshest surviving sibling (if any) becomes the redo target again.
        par_node.last_child = par_node.children.last().copied();
        self.current = par;
        // The popped leaf always holds the highest seq (push assigns it last),
        // so reclaim the seq to keep numbering gapless.
        if self.get(cur).seq + 1 == self.next_seq {
            self.next_seq -= 1;
        }
        self.free(cur);
        true
    }

    /// Node budget (`undolevels`). While the number of undo states (live nodes
    /// minus the root) exceeds `cap`, prune — branch-aware (Phase 2b):
    ///
    /// 1. First drop the lowest-`seq` LEAF that is NOT on the root→`current`
    ///    path — an abandoned branch tip. This never touches `current` or its
    ///    ancestors, so the state you're on and its full undo line survive.
    /// 2. When only the main line remains (no off-path leaves left), fall back
    ///    to promoting the root's on-path child to root and dropping the old
    ///    root — the Phase 2a root-side prune, which matches nvim's linear
    ///    `undolevels` trimming (oldest states drop first).
    ///
    /// `cap == 0` means unlimited (matches the old guard).
    pub(crate) fn cap(&mut self, cap: usize) {
        if cap == 0 {
            return;
        }
        // Guard against a pathological loop: at most one prune per live node.
        let mut budget_iters = self.live_count() + 1;
        while self.live_count().saturating_sub(1) > cap && budget_iters > 0 {
            budget_iters -= 1;
            if let Some(leaf) = self.lowest_offpath_leaf() {
                self.detach_leaf(leaf);
            } else if !self.prune_root_side() {
                break;
            }
        }
    }

    /// Ids on the root→`current` path (inclusive), which pruning must never
    /// touch. Small (one per undo level), so a `Vec` membership check is fine.
    fn current_path(&self) -> Vec<NodeId> {
        let mut path = Vec::new();
        let mut n = Some(self.current);
        while let Some(id) = n {
            path.push(id);
            n = self.get(id).parent;
        }
        path
    }

    /// Lowest-`seq` leaf that is not on the root→`current` path, if any.
    fn lowest_offpath_leaf(&self) -> Option<NodeId> {
        let path = self.current_path();
        let mut best: Option<(u64, NodeId)> = None;
        for (id, slot) in self.nodes.iter().enumerate() {
            if let Some(n) = slot
                && n.children.is_empty()
                && !path.contains(&id)
                && best.is_none_or(|(bs, _)| n.seq < bs)
            {
                best = Some((n.seq, id));
            }
        }
        best.map(|(_, id)| id)
    }

    /// Unlink `leaf` from its parent and free it (leaf ⇒ no subtree to recurse).
    fn detach_leaf(&mut self, leaf: NodeId) {
        if let Some(par) = self.get(leaf).parent {
            let par_node = self.get_mut(par);
            par_node.children.retain(|&c| c != leaf);
            if par_node.last_child == Some(leaf) {
                par_node.last_child = par_node.children.last().copied();
            }
        }
        self.free(leaf);
    }

    /// Promote the root's on-path child to the new root and free the old root.
    /// Returns `false` when the root is `current` (nothing left to trim).
    fn prune_root_side(&mut self) -> bool {
        let root = self.root;
        if root == self.current {
            return false;
        }
        // The child on the path to `current` (the root always has one here).
        let path = self.current_path();
        let Some(&child) = self.get(root).children.iter().find(|c| path.contains(c)) else {
            return false;
        };
        // Any OTHER root children are off-path branches; drop them with the root.
        let others: Vec<NodeId> = self
            .get(root)
            .children
            .iter()
            .copied()
            .filter(|&c| c != child)
            .collect();
        for c in others {
            self.free_subtree(c);
        }
        // The promoted child becomes the new root: materialize it (while the old
        // root still anchors the chain) into a full base rope, then drop its
        // now-meaningless parent edge. This keeps every delta below it valid.
        let base = self.materialize(child);
        {
            let node = self.get_mut(child);
            node.parent = None;
            node.base = Some(base);
            node.delta = None;
            node.rope_cache = None;
        }
        self.warm.retain(|&n| n != child);
        self.root = child;
        self.free(root);
        true
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
        // The survivor becomes a self-contained root: give it a full base rope
        // (materialized while the chain is still intact) so it needs no parent.
        let base = self.materialize(cur);
        for id in 0..self.nodes.len() {
            if id != cur && self.nodes[id].is_some() {
                self.nodes[id] = None;
                self.free.push(id);
            }
        }
        self.warm.clear();
        let node = self.get_mut(cur);
        node.parent = None;
        node.children.clear();
        node.last_child = None;
        node.delta = None;
        node.base = Some(base);
        node.rope_cache = None;
        self.root = cur;
    }
}

#[cfg(test)]
impl UndoTree {
    /// Ids of every live node, for warm-vs-cold materialization checks.
    fn live_ids(&self) -> Vec<NodeId> {
        (0..self.nodes.len())
            .filter(|&i| self.nodes[i].is_some())
            .collect()
    }

    /// Materialize `id` for a test (public wrapper over the private method).
    fn materialize_for_test(&mut self, id: NodeId) -> ropey::Rope {
        self.materialize(id)
    }

    /// Evict every warm cache (root keeps its `base`), forcing the next
    /// materialization of any node to reconstruct purely from deltas.
    fn drop_all_caches(&mut self) {
        for n in self.nodes.iter_mut().flatten() {
            n.rope_cache = None;
        }
        self.warm.clear();
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
    fn push_retains_forward_branch() {
        // Phase 2b: an edit after an undo forks a new branch; the old forward
        // branch is NOT dropped and remains reachable by seq.
        let mut t = UndoTree::new(ropey::Rope::from_str("s0"));
        t.push(entry("A")); // root -> nA (seq1, "A")
        let root = t.root;
        let na = t.current;
        let (r, c, m) = live("A");
        t.undo_step(r, c, m); // back to root, nA is the redo child
        assert!(t.has_redo());
        // A new edit from the root forks a SECOND child (nB, seq2).
        t.push(entry("B"));
        let nb = t.current;
        assert_ne!(nb, na);
        // Both branches live: root now has two children.
        assert_eq!(t.get(root).children.len(), 2);
        assert!(t.get(root).children.contains(&na));
        assert!(t.get(root).children.contains(&nb));
        // `<C-r>` follows the freshest branch (nB).
        assert_eq!(t.get(root).last_child, Some(nb));
        // Four live nodes: root + nA + nB + (nB is current/leaf). No leak of nA.
        let live = t.nodes.iter().filter(|n| n.is_some()).count();
        assert_eq!(live, 3);
    }

    #[test]
    fn seq_walk_crosses_branches() {
        // Mirror nvim `iA<Esc>uiB<Esc>` then g-/g+ (buffer starts empty "").
        // `push(entry)` writes `entry` into the node being LEFT (its true
        // pre-edit content); the fresh leaf holds the live post-edit state only
        // once it is stashed on the way past — exactly the engine's discipline.
        let mut t = UndoTree::new(ropey::Rope::from_str(""));
        t.push(entry("")); // leave root("") -> nA(seq1), live "A"
        let (r, c, m) = live("A");
        t.undo_step(r, c, m); // stash "A" into nA, back to root("")
        t.push(entry("")); // leave root("") -> nB(seq2), branch, live "B"
        let nb = t.current;
        // At B (seq2). g- -> greatest seq below 2 = seq1 = "A".
        let (r, c, m) = live("B");
        let a = t.seq_earlier_step(r, c, m).unwrap();
        assert_eq!(a.rope.to_string(), "A");
        // g- again -> root "".
        let (r, c, m) = live("A");
        let root_snap = t.seq_earlier_step(r, c, m).unwrap();
        assert_eq!(root_snap.rope.to_string(), "");
        // g+ -> back up to seq1 "A".
        let (r, c, m) = live("");
        let a2 = t.seq_later_step(r, c, m).unwrap();
        assert_eq!(a2.rope.to_string(), "A");
        // g+ -> seq2 "B" (crosses to the other branch).
        let (r, c, m) = live("A");
        let b = t.seq_later_step(r, c, m).unwrap();
        assert_eq!(b.rope.to_string(), "B");
        assert_eq!(t.current, nb);
        // At the tip: no higher seq.
        let (r, c, m) = live("B");
        assert!(t.seq_later_step(r, c, m).is_none());
    }

    #[test]
    fn seq_walk_updates_retrace_path() {
        // Land on a deep leaf via g-, then u/u and <C-r>/<C-r> must retrace it
        // (nvim `iX<Esc>iY<Esc>uiZ<Esc>g-uu<C-r><C-r>`). State labels: root "R".
        let mut t = UndoTree::new(ropey::Rope::from_str("R"));
        t.push(entry("R")); // leave root("R") -> nX(seq1), live "X"
        t.push(entry("X")); // leave nX("X") -> nY(seq2), live "Y"
        let (r, c, m) = live("Y");
        t.undo_step(r, c, m); // stash "Y" into nY, back to nX("X")
        t.push(entry("X")); // leave nX("X") -> nZ(seq3), branch, live "Z"
        // g- from Z(seq3) -> nY(seq2) "Y".
        let (r, c, m) = live("Z");
        let y = t.seq_earlier_step(r, c, m).unwrap();
        assert_eq!(y.rope.to_string(), "Y");
        // u,u back to root.
        let (r, c, m) = live("Y");
        t.undo_step(r, c, m);
        let (r, c, m) = live("X");
        t.undo_step(r, c, m);
        assert!(t.is_at_root());
        // <C-r>,<C-r> retraces the branch we landed on: root->X->Y.
        let (r, c, m) = live("R");
        let x = t.redo_step(r, c, m).unwrap();
        assert_eq!(x.rope.to_string(), "X");
        let (r, c, m) = live("X");
        let y2 = t.redo_step(r, c, m).unwrap();
        assert_eq!(y2.rope.to_string(), "Y");
    }

    #[test]
    fn leaves_lists_branch_tips_by_seq() {
        // root -> nX -> nY -> nW (leaf, seq3, depth3) and nX -> nZ (leaf, seq4,
        // depth2). Mirrors nvim `iX iY iW uu iZ`.
        let mut t = UndoTree::new(ropey::Rope::from_str(""));
        t.push(entry("X"));
        t.push(entry("Y"));
        t.push(entry("W"));
        let (r, c, m) = live("W");
        t.undo_step(r, c, m);
        let (r, c, m) = live("Y");
        t.undo_step(r, c, m); // back to nX
        t.push(entry("Z")); // nX -> nZ(seq4)
        let leaves = t.leaves();
        // Two leaves: W(seq3, depth3) and Z(seq4, depth2). Z is current.
        let dims: Vec<(u64, usize, bool)> =
            leaves.iter().map(|&(s, d, _, cur)| (s, d, cur)).collect();
        assert_eq!(dims, vec![(3, 3, false), (4, 2, true)]);
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
    fn cap_drops_offpath_leaf_before_main_line() {
        // Fork two abandoned branches off the root, then extend the main line,
        // and cap: the lowest-seq OFF-PATH leaf must go first, and `current`
        // plus its ancestors must survive.
        let mut t = UndoTree::new(ropey::Rope::from_str(""));
        t.push(entry("A")); // root -> nA(seq1) [abandoned branch tip]
        let na = t.current;
        let (r, c, m) = live("A");
        t.undo_step(r, c, m);
        t.push(entry("B")); // root -> nB(seq2) [abandoned branch tip]
        let nb = t.current;
        let (r, c, m) = live("B");
        t.undo_step(r, c, m);
        t.push(entry("C")); // root -> nC(seq3), the live main line
        let nc = t.current;
        // 4 live nodes (root, nA, nB, nC) => 3 states. Cap to 2.
        assert_eq!(t.leaves().len(), 3);
        t.cap(2);
        // The lowest-seq off-path leaf (nA, seq1) was dropped; current (nC) and
        // its ancestor (root) survive, and the newer off-path leaf nB survives.
        assert!(t.nodes[na].is_none());
        assert!(t.nodes[nb].is_some());
        assert_eq!(t.current, nc);
        assert!(!t.is_at_root());
        assert!(t.get(t.root).children.contains(&nb));
        assert!(t.get(t.root).children.contains(&nc));
    }

    #[test]
    fn pop_committed_reverses_last_push() {
        let mut t = UndoTree::new(ropey::Rope::from_str("s0"));
        t.push(entry("s0")); // depth 1, current = fresh leaf
        assert_eq!(t.depth(), 1);
        assert!(t.pop_committed());
        // The just-pushed leaf is gone; current stepped back to the root.
        assert_eq!(t.depth(), 0);
        assert!(t.is_at_root());
        assert_eq!(t.free.len(), 1);
        // Seq reclaimed so the next push is gapless.
        assert_eq!(t.next_seq, 1);
    }

    #[test]
    fn pop_committed_retains_sibling_branches() {
        // Fork a branch, then a no-op push at the fork must pop cleanly without
        // orphaning the sibling branch.
        let mut t = UndoTree::new(ropey::Rope::from_str(""));
        t.push(entry("A")); // root -> nA(seq1)
        let na = t.current;
        let (r, c, m) = live("A");
        t.undo_step(r, c, m); // back to root
        t.push(entry("B")); // root -> nB(seq2); root children [nA, nB]
        let root = t.root;
        // A spurious no-op push at nB, then pop it.
        assert!(t.pop_committed());
        // nB is gone, current back at root; nA branch still intact & reachable.
        assert!(t.get(root).children.contains(&na));
        assert_eq!(t.get(root).children.len(), 1);
        assert_eq!(t.current, root);
        let live = t.nodes.iter().filter(|n| n.is_some()).count();
        assert_eq!(live, 2); // root + nA
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

// ─── Phase 3a delta-storage tests (docs/undo-architecture.md §3/§6) ───────────
//
// Correctness of the reversible delta and the warm/cold materialization is
// where text gets silently corrupted, so these lean hard on it: exact diff
// round-trips over random (incl. multi-byte) content, every node reconstructing
// identically warm and cold, and a random op stream cross-checked against a
// full-snapshot reference model kept alongside. All randomness is a deterministic
// xorshift seeded from a fixed constant — never `SystemTime`/entropy — so a
// failure reproduces exactly.
#[cfg(test)]
mod delta_tests {
    use super::*;

    /// Deterministic xorshift64* PRNG, fixed-seeded so runs are reproducible.
    struct Rng(u64);
    impl Rng {
        fn new(seed: u64) -> Self {
            // xorshift needs a non-zero state.
            Rng(if seed == 0 {
                0x9E37_79B9_7F4A_7C15
            } else {
                seed
            })
        }
        fn next_u64(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next_u64() % n as u64) as usize
        }
    }

    /// A random char-granular mutation of `s`: insert, delete, or replace a
    /// span, drawing from an alphabet that mixes ASCII, accented, CJK, and
    /// emoji so multi-byte boundaries are exercised.
    fn mutate(s: &str, rng: &mut Rng) -> String {
        const ALPHABET: [char; 10] = ['a', 'b', '\n', 'é', '日', '本', '🎉', '語', 'x', 'z'];
        let chars: Vec<char> = s.chars().collect();
        let pick = |rng: &mut Rng| ALPHABET[rng.below(ALPHABET.len())];
        match rng.below(3) {
            0 => {
                let pos = rng.below(chars.len() + 1);
                let mut v = chars.clone();
                v.insert(pos, pick(rng));
                v.into_iter().collect()
            }
            1 if !chars.is_empty() => {
                let pos = rng.below(chars.len());
                let mut v = chars.clone();
                v.remove(pos);
                v.into_iter().collect()
            }
            _ => {
                if chars.is_empty() {
                    return pick(rng).to_string();
                }
                let a = rng.below(chars.len());
                let b = (a + rng.below(chars.len() - a + 1)).min(chars.len());
                let mut v = chars[..a].to_vec();
                v.push(pick(rng));
                v.extend_from_slice(&chars[b..]);
                v.into_iter().collect()
            }
        }
    }

    fn entry_str(s: &str) -> UndoEntry {
        UndoEntry {
            rope: ropey::Rope::from_str(s),
            cursor: (0, 0),
            timestamp: SystemTime::now(),
            marks: MarkSnapshot::default(),
        }
    }

    // ── (i) delta round-trip: apply(diff(a,b))==b and apply_inverse==a ────────

    #[test]
    fn diff_round_trips_over_random_evolving_content() {
        let mut rng = Rng::new(0x1234_5678_9ABC_DEF0);
        let mut s = String::from("seed café 日本語\n🎉");
        for _ in 0..4000 {
            let t = mutate(&s, &mut rng);
            let a = ropey::Rope::from_str(&s);
            let b = ropey::Rope::from_str(&t);
            let d = diff(&a, &b);
            assert_eq!(
                apply_forward(&a, &d).to_string(),
                t,
                "forward a->b failed (start={}, old={:?}, new={:?})",
                d.start,
                d.old,
                d.new
            );
            assert_eq!(
                apply_inverse(&b, &d).to_string(),
                s,
                "inverse b->a failed (start={}, old={:?}, new={:?})",
                d.start,
                d.old,
                d.new
            );
            s = t;
        }
    }

    #[test]
    fn diff_round_trips_over_unrelated_pairs() {
        // Disjoint corpus pairs (not just single-edit neighbours) so the diff's
        // prefix/suffix logic is stressed on wholly different multi-byte text.
        let corpus = [
            "",
            "a",
            "café\n日本語\n",
            "🎉🎉🎉",
            "abcdef",
            "日本",
            "x\ny\nz\n",
            "aXb",
            "café",
            "語日本",
            "\n\n\n",
            "🎉x🎉y🎉",
        ];
        let mut rng = Rng::new(0xDEAD_BEEF_CAFE_1234);
        for _ in 0..3000 {
            let sa = corpus[rng.below(corpus.len())];
            let sb = corpus[rng.below(corpus.len())];
            let a = ropey::Rope::from_str(sa);
            let b = ropey::Rope::from_str(sb);
            let d = diff(&a, &b);
            assert_eq!(apply_forward(&a, &d).to_string(), sb);
            assert_eq!(apply_inverse(&b, &d).to_string(), sa);
        }
    }

    // ── non-ASCII edit → undo → redo round-trip (multi-byte across a leave) ───

    #[test]
    fn non_ascii_edit_undo_redo_round_trip() {
        // Edits land INSIDE multi-byte lines; undo/redo must round-trip the exact
        // bytes, proving the char-offset delta never splits a codepoint.
        let mut d = Driver::new("café\n日本語\n");
        d.edit("cafés\n日本語\n");
        d.edit("cafés\n日本語です\n");
        d.edit("cafés\n日本語です🎉\n");
        assert_eq!(d.undo().as_deref(), Some("cafés\n日本語です\n"));
        assert_eq!(d.undo().as_deref(), Some("cafés\n日本語\n"));
        assert_eq!(d.undo().as_deref(), Some("café\n日本語\n"));
        assert_eq!(d.redo().as_deref(), Some("cafés\n日本語\n"));
        assert_eq!(d.redo().as_deref(), Some("cafés\n日本語です\n"));
        assert_eq!(d.redo().as_deref(), Some("cafés\n日本語です🎉\n"));
        // Cold reconstruction of every node still matches (drop all caches).
        assert_warm_equals_cold(&mut d.t);
    }

    // ── (ii) + (iii) random op stream vs a full-snapshot reference model ──────

    #[test]
    fn tree_matches_full_snapshot_reference_over_random_ops() {
        let mut rng = Rng::new(0x9E37_79B9_7F4A_7C15);
        let start = "α\nβγ\n日本🎉\n";
        let mut real = UndoTree::new(ropey::Rope::from_str(start));
        let mut refr = RefTree::new(start);
        let mut live = start.to_string();

        for step in 0..6000 {
            // Structural predicates stay in lockstep with the reference.
            assert_eq!(real.is_at_root(), refr.is_at_root(), "is_at_root @ {step}");
            assert_eq!(real.has_redo(), refr.has_redo(), "has_redo @ {step}");
            assert_eq!(real.depth(), refr.depth(), "depth @ {step}");

            match rng.below(6) {
                0 | 1 => {
                    // Edit: push the PRE-edit state (engine discipline), then
                    // mutate the live buffer.
                    let pre = live.clone();
                    real.push(entry_str(&pre));
                    refr.push(&pre);
                    live = mutate(&live, &mut rng);
                }
                2 => {
                    let got = real
                        .undo_step(
                            ropey::Rope::from_str(&live),
                            (0, 0),
                            MarkSnapshot::default(),
                        )
                        .map(|e| e.rope.to_string());
                    let want = refr.undo_step(&live);
                    assert_eq!(got, want, "undo @ {step}");
                    if let Some(c) = got {
                        live = c;
                    }
                }
                3 => {
                    let got = real
                        .redo_step(
                            ropey::Rope::from_str(&live),
                            (0, 0),
                            MarkSnapshot::default(),
                        )
                        .map(|e| e.rope.to_string());
                    let want = refr.redo_step(&live);
                    assert_eq!(got, want, "redo @ {step}");
                    if let Some(c) = got {
                        live = c;
                    }
                }
                4 => {
                    let got = real
                        .seq_earlier_step(
                            ropey::Rope::from_str(&live),
                            (0, 0),
                            MarkSnapshot::default(),
                        )
                        .map(|e| e.rope.to_string());
                    let want = refr.seq_earlier_step(&live);
                    assert_eq!(got, want, "g- @ {step}");
                    if let Some(c) = got {
                        live = c;
                    }
                }
                _ => {
                    let got = real
                        .seq_later_step(
                            ropey::Rope::from_str(&live),
                            (0, 0),
                            MarkSnapshot::default(),
                        )
                        .map(|e| e.rope.to_string());
                    let want = refr.seq_later_step(&live);
                    assert_eq!(got, want, "g+ @ {step}");
                    if let Some(c) = got {
                        live = c;
                    }
                }
            }

            // (ii) Every so often, assert warm and cold materialization agree
            // for every node — a cold-reconstructed node must equal the rope the
            // full-snapshot model would have held.
            if step % 200 == 0 {
                assert_warm_equals_cold(&mut real);
            }
        }
        assert_warm_equals_cold(&mut real);
    }

    /// For every live node: materialize warm, drop all caches, materialize cold,
    /// assert identical. Restores nothing else (test-local).
    fn assert_warm_equals_cold(t: &mut UndoTree) {
        let ids = t.live_ids();
        let warm: Vec<String> = ids
            .iter()
            .map(|&id| t.materialize_for_test(id).to_string())
            .collect();
        t.drop_all_caches();
        for (i, &id) in ids.iter().enumerate() {
            let cold = t.materialize_for_test(id).to_string();
            assert_eq!(cold, warm[i], "warm != cold for node {id}");
        }
    }

    /// Engine-faithful driver over the real (delta) [`UndoTree`]: mirrors how
    /// `editor.rs` pushes the PRE-edit state and restores returned content.
    struct Driver {
        t: UndoTree,
        live: String,
    }
    impl Driver {
        fn new(s: &str) -> Self {
            Driver {
                t: UndoTree::new(ropey::Rope::from_str(s)),
                live: s.to_string(),
            }
        }
        fn edit(&mut self, new: &str) {
            self.t.push(entry_str(&self.live));
            self.live = new.to_string();
        }
        fn undo(&mut self) -> Option<String> {
            let e = self.t.undo_step(
                ropey::Rope::from_str(&self.live),
                (0, 0),
                MarkSnapshot::default(),
            )?;
            self.live = e.rope.to_string();
            Some(self.live.clone())
        }
        fn redo(&mut self) -> Option<String> {
            let e = self.t.redo_step(
                ropey::Rope::from_str(&self.live),
                (0, 0),
                MarkSnapshot::default(),
            )?;
            self.live = e.rope.to_string();
            Some(self.live.clone())
        }
    }

    /// Full-snapshot reference tree — Phase 2b's model (a whole rope per node),
    /// the oracle the delta tree is cross-checked against. Content only (cursor /
    /// marks / timestamps are covered by the existing tree tests).
    struct RefNode {
        parent: Option<usize>,
        children: Vec<usize>,
        last_child: Option<usize>,
        content: String,
        seq: u64,
    }
    struct RefTree {
        nodes: Vec<Option<RefNode>>,
        current: usize,
        next_seq: u64,
    }
    impl RefTree {
        fn new(s: &str) -> Self {
            let root = RefNode {
                parent: None,
                children: Vec::new(),
                last_child: None,
                content: s.to_string(),
                seq: 0,
            };
            RefTree {
                nodes: vec![Some(root)],
                current: 0,
                next_seq: 1,
            }
        }
        fn get(&self, id: usize) -> &RefNode {
            self.nodes[id].as_ref().unwrap()
        }
        fn get_mut(&mut self, id: usize) -> &mut RefNode {
            self.nodes[id].as_mut().unwrap()
        }
        fn alloc(&mut self, n: RefNode) -> usize {
            self.nodes.push(Some(n));
            self.nodes.len() - 1
        }
        fn is_at_root(&self) -> bool {
            self.get(self.current).parent.is_none()
        }
        fn has_redo(&self) -> bool {
            self.get(self.current).last_child.is_some()
        }
        fn depth(&self) -> usize {
            let mut d = 0;
            let mut n = self.get(self.current).parent;
            while let Some(p) = n {
                d += 1;
                n = self.get(p).parent;
            }
            d
        }
        fn push(&mut self, pre: &str) {
            let cur = self.current;
            self.get_mut(cur).content = pre.to_string();
            let seq = self.next_seq;
            self.next_seq += 1;
            let child = self.alloc(RefNode {
                parent: Some(cur),
                children: Vec::new(),
                last_child: None,
                content: pre.to_string(),
                seq,
            });
            let c = self.get_mut(cur);
            c.children.push(child);
            c.last_child = Some(child);
            self.current = child;
        }
        fn undo_step(&mut self, live: &str) -> Option<String> {
            let cur = self.current;
            let par = self.get(cur).parent?;
            self.get_mut(cur).content = live.to_string();
            self.get_mut(par).last_child = Some(cur);
            self.current = par;
            Some(self.get(par).content.clone())
        }
        fn redo_step(&mut self, live: &str) -> Option<String> {
            let cur = self.current;
            let child = self.get(cur).last_child?;
            self.get_mut(cur).content = live.to_string();
            self.current = child;
            Some(self.get(child).content.clone())
        }
        fn current_seq(&self) -> u64 {
            self.get(self.current).seq
        }
        fn node_below(&self, s: u64) -> Option<usize> {
            let mut best: Option<(u64, usize)> = None;
            for (id, slot) in self.nodes.iter().enumerate() {
                if let Some(n) = slot
                    && n.seq < s
                    && best.is_none_or(|(bs, _)| n.seq > bs)
                {
                    best = Some((n.seq, id));
                }
            }
            best.map(|(_, id)| id)
        }
        fn node_above(&self, s: u64) -> Option<usize> {
            let mut best: Option<(u64, usize)> = None;
            for (id, slot) in self.nodes.iter().enumerate() {
                if let Some(n) = slot
                    && n.seq > s
                    && best.is_none_or(|(bs, _)| n.seq < bs)
                {
                    best = Some((n.seq, id));
                }
            }
            best.map(|(_, id)| id)
        }
        fn retarget(&mut self, target: usize) {
            self.current = target;
            let mut node = target;
            while let Some(p) = self.get(node).parent {
                self.get_mut(p).last_child = Some(node);
                node = p;
            }
        }
        fn stash_and_move(&mut self, target: usize, live: &str) {
            let cur = self.current;
            self.get_mut(cur).content = live.to_string();
            self.retarget(target);
        }
        fn seq_earlier_step(&mut self, live: &str) -> Option<String> {
            let target = self.node_below(self.current_seq())?;
            self.stash_and_move(target, live);
            Some(self.get(target).content.clone())
        }
        fn seq_later_step(&mut self, live: &str) -> Option<String> {
            let target = self.node_above(self.current_seq())?;
            self.stash_and_move(target, live);
            Some(self.get(target).content.clone())
        }
    }
}
