//! Undo/redo entry type for per-buffer undo history.
//!
//! Lives in `hjkl-buffer` so that [`crate::Buffer`] can own the undo stack
//! directly, keeping per-buffer state co-located with the rope.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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

// ─── Reversible edge delta (Phase 3a) ──────────────────────────────────────────
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
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Delta {
    /// Char offset of the first differing char (the common-prefix length).
    pub start: usize,
    /// Chars present in the PARENT but not the CHILD (removed going forward).
    pub old: String,
    /// Chars present in the CHILD but not the PARENT (inserted going forward).
    pub new: String,
}

/// Is `byte_idx` a char boundary of `r`? (`str::is_char_boundary` for ropes.)
///
/// Ropey always splits chunks ON char boundaries, so the question is answerable
/// inside the one chunk containing the byte — O(log N), no materialization.
fn is_char_boundary(r: &ropey::Rope, byte_idx: usize) -> bool {
    if byte_idx == 0 || byte_idx == r.len_bytes() {
        return true;
    }
    let (chunk, chunk_start, _, _) = r.chunk_at_byte(byte_idx);
    chunk.is_char_boundary(byte_idx - chunk_start)
}

/// Length of the longest common byte PREFIX of `a` and `b`, capped at `max`.
///
/// Walks both ropes' chunks in lockstep — never materializes either rope. Two
/// fast paths keep the common editor case near-free: identical chunk pointers
/// (ropey leaves are `Arc`-shared, so a clone-then-edit child shares every leaf
/// outside the edit) are accepted without reading bytes, and otherwise whole
/// overlapping runs are compared with one slice `==` (memcmp).
fn common_prefix_bytes(a: &ropey::Rope, b: &ropey::Rope, max: usize) -> usize {
    let mut a_chunks = a.chunks();
    let mut b_chunks = b.chunks();
    let mut at: &[u8] = &[];
    let mut bt: &[u8] = &[];
    let mut n = 0;
    while n < max {
        if at.is_empty() {
            match a_chunks.next() {
                Some(c) => at = c.as_bytes(),
                None => break,
            }
            continue;
        }
        if bt.is_empty() {
            match b_chunks.next() {
                Some(c) => bt = c.as_bytes(),
                None => break,
            }
            continue;
        }
        // Same shared leaf, wholly within the cap: equal without looking.
        if at.as_ptr() == bt.as_ptr() && at.len() == bt.len() && at.len() <= max - n {
            n += at.len();
            at = &[];
            bt = &[];
            continue;
        }
        let m = at.len().min(bt.len()).min(max - n);
        if at[..m] == bt[..m] {
            n += m;
            at = &at[m..];
            bt = &bt[m..];
        } else {
            let mut i = 0;
            while at[i] == bt[i] {
                i += 1;
            }
            n += i;
            break;
        }
    }
    n
}

/// Length of the longest common byte SUFFIX of `a` and `b`, capped at `max`.
///
/// The mirror of [`common_prefix_bytes`], walking both ropes' chunk cursors
/// backwards from the end via [`ropey::iter::Chunks::prev`].
fn common_suffix_bytes(a: &ropey::Rope, b: &ropey::Rope, max: usize) -> usize {
    let mut a_chunks = a.chunks_at_byte(a.len_bytes()).0;
    let mut b_chunks = b.chunks_at_byte(b.len_bytes()).0;
    let mut at: &[u8] = &[];
    let mut bt: &[u8] = &[];
    let mut n = 0;
    while n < max {
        if at.is_empty() {
            match a_chunks.prev() {
                Some(c) => at = c.as_bytes(),
                None => break,
            }
            continue;
        }
        if bt.is_empty() {
            match b_chunks.prev() {
                Some(c) => bt = c.as_bytes(),
                None => break,
            }
            continue;
        }
        if at.as_ptr() == bt.as_ptr() && at.len() == bt.len() && at.len() <= max - n {
            n += at.len();
            at = &[];
            bt = &[];
            continue;
        }
        let m = at.len().min(bt.len()).min(max - n);
        let (a_tail, b_tail) = (&at[at.len() - m..], &bt[bt.len() - m..]);
        if a_tail == b_tail {
            n += m;
            at = &at[..at.len() - m];
            bt = &bt[..bt.len() - m];
        } else {
            let mut i = 0;
            while a_tail[m - 1 - i] == b_tail[m - 1 - i] {
                i += 1;
            }
            n += i;
            break;
        }
    }
    n
}

/// Common-prefix / common-suffix diff of two ropes → the minimal single spanning
/// [`Delta`]. Guarantees `apply_forward(a, diff(a, b)) == b` and
/// `apply_inverse(b, diff(a, b)) == a` for ALL `a`, `b` (see the property
/// tests). Boundaries are found on bytes (fast) then snapped to char boundaries
/// so `old`/`new` are always valid UTF-8 and `start` is a true char offset.
///
/// The scans walk the ropes' chunks directly and only the differing MIDDLE is
/// materialized — a full `to_string()` of both sides used to dominate every edit
/// on a multi-MB buffer (measured 1.55 ms + ~6.4 MB of allocation per push at
/// 3.2 MB). `diff_reference` in the tests below is the old materializing
/// implementation, kept as the differential-test oracle.
fn diff(parent: &ropey::Rope, child: &ropey::Rope) -> Delta {
    let a_len = parent.len_bytes();
    let b_len = child.len_bytes();

    // Longest common byte prefix, snapped DOWN to a char boundary.
    let max_pre = a_len.min(b_len);
    let mut pre = common_prefix_bytes(parent, child, max_pre);
    while pre > 0 && !is_char_boundary(parent, pre) {
        pre -= 1;
    }

    // Longest common byte suffix not overlapping the prefix. The cut points
    // `a_end`/`b_end` sit at identical trailing bytes, so snapping `a_end` UP to
    // a char boundary snaps `b_end` by the same byte delta simultaneously.
    let suf = common_suffix_bytes(parent, child, max_pre - pre);
    let mut a_end = a_len - suf;
    while a_end < a_len && !is_char_boundary(parent, a_end) {
        a_end += 1;
    }
    let b_end = b_len - (a_len - a_end);

    Delta {
        start: parent.byte_to_char(pre),
        old: parent.byte_slice(pre..a_end).to_string(),
        new: child.byte_slice(pre..b_end).to_string(),
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
//
// Keyframes (issue #302): the warm LRU alone bounds nothing — a `g-` onto a node
// far outside it replayed the WHOLE chain from the root, so one jump was O(depth)
// and `:earlier 9999` was O(depth²) (measured 212 ms for a 1024-deep history).
// Every node at a depth that is a multiple of `KEYFRAME_INTERVAL` therefore PINS
// its materialized rope, capping any single replay at `KEYFRAME_INTERVAL - 1`
// applies; and `materialize` caches every intermediate it replays, so the
// step-by-step walk pays that replay once per interval rather than once per step.
// Keyframes are a pure in-memory cache: they are recomputable from the root base
// plus the deltas, so they are NOT part of the `SerTree` on-disk projection, and
// dropping every one of them changes only speed, never content.

/// Keyframe spacing, in nodes of depth. Every node whose depth from the root is
/// a multiple of this pins its materialized rope, so `materialize` never replays
/// more than `KEYFRAME_INTERVAL - 1` deltas from the nearest anchor.
///
/// **Why 16.** The cost of a keyframe is *not* a document copy. `ropey::Rope` is
/// a persistent tree with `Arc`-shared leaves, so a snapshot taken between small
/// edits shares every chunk outside the edited path with its neighbours and only
/// retains the O(log N) interior nodes the edit rewrote. Measured marginal RSS of
/// retaining one such snapshot (1024 small edits, keeping every 16th):
///
/// | document | bytes retained per keyframe |
/// | --- | --- |
/// | 119 KiB | ~3.0 KiB |
/// | 11.9 MiB | ~7.2 KiB |
/// | 11.9 MiB, 4 KiB edits | ~11.5 KiB |
///
/// i.e. essentially independent of document size — a 200 MB buffer does not pay
/// 200 MB per keyframe. At one keyframe per 16 nodes that is well under a KiB of
/// amortized overhead per undo state, next to the `Delta` (two `String`s of the
/// changed span) every node already stores unconditionally. 16 also sits under
/// [`WARM_CAP`], which is what lets a full walk stay linear (see there).
///
/// The one shape that would break the "cheap" argument — an edit that rewrites
/// the entire document, so consecutive states share nothing — already costs two
/// full-document `String`s in that node's own `Delta`, so the keyframe adds at
/// most another 1/16 of a cost the tree was paying anyway.
const KEYFRAME_INTERVAL: usize = 16;

/// Hard ceiling on how many keyframes are pinned at once; beyond it the
/// least-recently-touched keyframe is unpinned (it becomes an ordinary cold
/// node, replayable as before — correctness is unaffected).
///
/// Deliberately a COUNT, not a byte budget: by the measurement on
/// [`KEYFRAME_INTERVAL`] a keyframe's real retention is roughly document-size
/// *independent*, so a byte budget computed from `len_bytes()` would be a wild
/// over-estimate and would switch keyframes off precisely on the large documents
/// that need them most. 512 keyframes covers 8192 undo states — past any sane
/// `undolevels` — for a measured ceiling of a few MiB.
const KEYFRAME_CAP: usize = 512;

/// Index into [`UndoTree::nodes`]. Slots are reused via a free list, so an id is
/// only valid while the node it names is live — the tree never hands ids out.
pub type NodeId = usize;

/// How many recently-materialized ORDINARY node ropes to keep warm (besides the
/// root base, `current`, and the pinned keyframes, which are always available).
///
/// Kept above [`KEYFRAME_INTERVAL`] on purpose: `materialize` caches every
/// intermediate it replays, so one keyframe interval's worth of intermediates has
/// to survive here for a step-by-step history walk (`:earlier 9999`) to cost one
/// replay per INTERVAL rather than one per step — the difference between an O(N)
/// and an O(N·K) walk.
const WARM_CAP: usize = 32;

/// One node of the undo arena tree: a buffer state the user could land on, plus
/// its links and the reversible edge to its parent. A node with `> 1` child is a
/// branch point (Phase 2b); `last_child` records which child `<C-r>` follows.
#[derive(Debug, Clone)]
pub struct UndoNode {
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
    /// Materialized content, LRU-managed. Warm for `current`, recently visited
    /// nodes, and keyframe-depth nodes (which are pinned rather than aged out);
    /// `None` (cold) otherwise, reconstructable from deltas.
    pub rope_cache: Option<ropey::Rope>,
    /// Distance from the root, root == 0. Assigned once at creation and never
    /// renumbered — root-side pruning shifts the whole numbering down uniformly,
    /// which leaves keyframes exactly [`KEYFRAME_INTERVAL`] apart either way.
    /// Purely a cache-placement input: a wrong depth costs speed, never content.
    pub depth: usize,
    /// Post-state cursor for this node (restored alongside the text).
    pub cursor: (usize, usize),
    /// Wall-clock time this state was created — drives `:earlier`/`:later`.
    pub timestamp: SystemTime,
    /// Marks / jumplist / changelist snapshot restored with the text.
    ///
    /// Shared (`Arc`) rather than owned: a `push` writes the SAME snapshot
    /// into the node being left and into the fresh child, and a
    /// `MarkSnapshot` is up to five collections. Nodes never mutate it in
    /// place — it is only ever replaced wholesale — so sharing is invisible.
    pub marks: Arc<MarkSnapshot>,
    /// Global monotonic order across the whole tree — the change number that
    /// `g-`/`g+`, `:earlier`/`:later`, and `:undolist` traverse and display.
    pub seq: u64,
}

/// Arena tree of [`UndoNode`]s. Replaces the old `undo_stack`/`redo_stack`
/// `Vec<UndoEntry>` pair on [`crate::Buffer`]; see the module comment for how
/// `u`/`<C-r>` (branch-local) and `g-`/`g+` (seq-ordered) map onto it, and how
/// Phase 3a stores edges as deltas behind a materialization cache.
#[derive(Debug)]
pub struct UndoTree {
    /// Slab; `None` slots are free and recorded in `free`.
    nodes: Vec<Option<UndoNode>>,
    /// Reusable slot indices (frees push here, allocs pop here first).
    free: Vec<NodeId>,
    /// LRU of ORDINARY node ids with a warm `rope_cache` (root and keyframe-depth
    /// nodes excluded — they live in `base` / `keyframes`), most-recently-touched
    /// last. Bounded by [`WARM_CAP`]; `current` is never evicted.
    warm: Vec<NodeId>,
    /// Node ids at a keyframe depth whose `rope_cache` is PINNED — the replay
    /// anchors that bound `materialize` at [`KEYFRAME_INTERVAL`] applies.
    /// Most-recently-touched last, bounded by [`KEYFRAME_CAP`].
    keyframes: Vec<NodeId>,
    root: NodeId,
    current: NodeId,
    next_seq: u64,
}

/// Trim `list` (an LRU, oldest first) down to `cap`, dropping the evicted
/// nodes' materialized ropes. `current` is never evicted — the live state must
/// stay available without a replay.
///
/// A free function over the pieces rather than a method so it can hold `&mut`
/// on one arena field and one list at the same time.
fn evict_to(list: &mut Vec<NodeId>, nodes: &mut [Option<UndoNode>], current: NodeId, cap: usize) {
    while list.len() > cap {
        let Some(pos) = list.iter().position(|&n| n != current) else {
            break;
        };
        let victim = list.remove(pos);
        if let Some(node) = nodes[victim].as_mut() {
            node.rope_cache = None;
        }
    }
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
            depth: 0,
            cursor: (0, 0),
            timestamp: SystemTime::now(),
            marks: Arc::default(),
            seq: 0,
        };
        Self {
            nodes: vec![Some(root)],
            free: Vec::new(),
            warm: Vec::new(),
            keyframes: Vec::new(),
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
    /// from both cache LRUs.
    fn free(&mut self, id: NodeId) {
        self.nodes[id] = None;
        self.free.push(id);
        self.warm.retain(|&n| n != id);
        self.keyframes.retain(|&n| n != id);
    }

    // ── materialization (Phase 3a + keyframes) ───────────────────────────────

    /// Is `id` at a keyframe depth, i.e. should its materialized rope be PINNED
    /// as a replay anchor rather than aged out of the ordinary warm LRU?
    ///
    /// The root qualifies arithmetically (depth 0) but is excluded: it carries a
    /// full `base` and is already an anchor.
    fn is_keyframe(&self, id: NodeId) -> bool {
        id != self.root && self.get(id).depth.is_multiple_of(KEYFRAME_INTERVAL)
    }

    /// Record `id` as freshly materialized. Keyframe-depth nodes go into the
    /// pinned `keyframes` LRU (bounded by [`KEYFRAME_CAP`]), everything else into
    /// the ordinary `warm` LRU (bounded by [`WARM_CAP`]). Neither ever evicts
    /// `current`; the root is skipped entirely (it has no cache, it has `base`).
    fn touch_warm(&mut self, id: NodeId) {
        if id == self.root {
            return;
        }
        let (list, cap) = if self.is_keyframe(id) {
            (&mut self.keyframes, KEYFRAME_CAP)
        } else {
            (&mut self.warm, WARM_CAP)
        };
        list.retain(|&n| n != id);
        list.push(id);
        evict_to(list, &mut self.nodes, self.current, cap);
    }

    /// Materialize node `id`'s content, warming its cache. Uses the node's own
    /// cache if present, else the root `base`, else replays forward deltas from
    /// the nearest materialized ancestor — a warm node, a pinned keyframe, or the
    /// root. Always terminates: the root carries a base.
    ///
    /// Every intermediate along the replay is cached too, not just the target:
    /// they were computed anyway and a `ropey::Rope` clone is an `Arc` bump, so
    /// caching them is free — and it is what makes a step-by-step history walk
    /// (`g-` held down, `:earlier 9999`) pay ONE replay per keyframe interval
    /// instead of one per step.
    fn materialize(&mut self, id: NodeId) -> ropey::Rope {
        if let Some(r) = &self.get(id).rope_cache {
            return r.clone();
        }
        if let Some(base) = &self.get(id).base {
            return base.clone();
        }
        // Walk up to the nearest ancestor that holds content (warm cache, pinned
        // keyframe, or the root base), recording the path to replay forward.
        // Bounded by the keyframe spacing whenever the ancestor chain has been
        // materialized before.
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
        // `path` is target-first, so replaying in reverse ends on `id` — which
        // therefore lands last in its LRU and cannot be the eviction picked by
        // its own `touch_warm`.
        for &node in path.iter().rev() {
            let d = self
                .get(node)
                .delta
                .as_ref()
                .expect("a non-root node always carries its edge delta");
            rope = apply_forward(&rope, d);
            self.get_mut(node).rope_cache = Some(rope.clone());
            self.touch_warm(node);
        }
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
            marks: (*n.marks).clone(),
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
        marks: Arc<MarkSnapshot>,
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
            self.keyframes.retain(|&n| n != id);
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
        // ONE snapshot serves both the node being left and the fresh child —
        // this runs per edit, and a deep `MarkSnapshot` copy is up to five
        // collection allocations.
        let marks = Arc::new(entry.marks);
        // Finalize the node being left with the pre-edit live state, recomputing
        // its edge delta from its parent (or the root base).
        self.set_node_state(
            cur,
            entry.rope.clone(),
            entry.cursor,
            entry.timestamp,
            Arc::clone(&marks),
        );
        let seq = self.next_seq;
        self.next_seq += 1;
        // Fresh child: identical to `cur` for now (empty edge delta + warm cache
        // holding the pre-edit rope). Its true post-edit content is finalized on
        // the way past it (next move) or by the next `push`, at which point the
        // edge delta is recomputed against `cur`.
        let child_depth = self.get(cur).depth + 1;
        let child = self.alloc(UndoNode {
            parent: Some(cur),
            children: Vec::new(),
            last_child: None,
            delta: Some(Delta::default()),
            base: None,
            rope_cache: Some(entry.rope),
            depth: child_depth,
            cursor: entry.cursor,
            timestamp: entry.timestamp,
            marks,
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
        self.set_node_state(cur, rope, cursor, dest_ts, Arc::new(marks));
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
        self.set_node_state(cur, rope, cursor, dest_ts, Arc::new(marks));
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
        self.set_node_state(cur, rope, cursor, ts, Arc::new(marks));
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
        self.keyframes.retain(|&n| n != child);
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
        self.keyframes.clear();
        let node = self.get_mut(cur);
        node.parent = None;
        node.children.clear();
        node.last_child = None;
        node.delta = None;
        node.base = Some(base);
        node.rope_cache = None;
        // The survivor is the new root: restart the depth numbering under it so
        // its descendants land on the keyframe ladder from 0 again.
        node.depth = 0;
        self.root = cur;
    }
}

// ─── Serializable projection (Phase 3b) ───────────────────────────────────────
//
// The undofile persists the tree as a compact, self-consistent projection: the
// root's full base text (String) plus, per node, its edge `delta` and links.
// `rope_cache`/`warm` are runtime-only and dropped — every node reconstructs
// from the root base + deltas, so the round-trip reproduces identical content
// at every node. NodeIds are DENSE in the projection (the live-slab holes are
// compacted away and links remapped), so `from_serializable` rebuilds a fresh
// arena 1:1 with no free list.

/// One node of the serialized undo tree. Mirrors [`UndoNode`] minus the
/// runtime-only materialization cache; ids are dense indices into
/// [`SerTree::nodes`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerNode {
    /// Parent index, `None` only for the root.
    pub parent: Option<u32>,
    /// Child indices (order preserved; `> 1` ⇒ branch point).
    pub children: Vec<u32>,
    /// `<C-r>` target child index.
    pub last_child: Option<u32>,
    /// Reversible edge delta from the parent, `None` only for the root.
    pub delta: Option<Delta>,
    /// Post-state cursor `(row, col)`.
    pub cursor: (u32, u32),
    /// Wall-clock creation time, ms since the UNIX epoch.
    pub timestamp_unix_ms: u64,
    /// Marks / jumplist / changelist snapshot.
    pub marks: MarkSnapshot,
    /// Global monotonic change number.
    pub seq: u64,
}

/// Serializable projection of an [`UndoTree`] for the undofile. Postcard-encoded
/// (non-self-describing, so a schema/version drift surfaces as a parse `Err`
/// that the reader discards). See [`UndoTree::to_serializable`] /
/// [`UndoTree::from_serializable`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerTree {
    /// Root base text (the anchor the delta chain replays from).
    pub base: String,
    /// Dense node arena (no holes).
    pub nodes: Vec<SerNode>,
    /// Root index into `nodes`.
    pub root: u32,
    /// Current (live) index into `nodes`.
    pub current: u32,
    /// Next `seq` to assign.
    pub next_seq: u64,
}

/// [`SystemTime`] → ms since the UNIX epoch (saturating, pre-epoch ⇒ 0).
fn system_time_to_unix_ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// ms since the UNIX epoch → [`SystemTime`].
fn unix_ms_to_system_time(ms: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(ms)
}

impl UndoTree {
    /// `seq` of the current (live) node — the header's `current_seq` for the
    /// undofile (the just-saved content per the §6 invariant).
    pub(crate) fn current_node_seq(&self) -> u64 {
        self.get(self.current).seq
    }

    /// Materialize the current (live) node's content. Used by the swap
    /// recovery consistency guard (docs §6c) to check a deserialized tree
    /// agrees with the freshly-recovered buffer text before it's installed.
    pub(crate) fn current_content(&mut self) -> ropey::Rope {
        let cur = self.current;
        self.materialize(cur)
    }

    /// Stash `rope` into the current node as the live buffer state, preserving
    /// that node's own cursor/timestamp/marks. Called just before serializing so
    /// the on-disk tree's `current` edge is exact even when `current` is a fresh
    /// (still-stale) leaf — the in-session self-heal (first undo/edit stashes
    /// live) applied eagerly at save time.
    pub(crate) fn sync_current(&mut self, rope: ropey::Rope) {
        let cur = self.current;
        let (cursor, ts, marks) = {
            let n = self.get(cur);
            (n.cursor, n.timestamp, n.marks.clone())
        };
        self.set_node_state(cur, rope, cursor, ts, marks);
    }

    /// Project the live tree into a serializable, dense form (holes compacted,
    /// links remapped). `rope_cache`/`warm` are dropped; the root's `base`
    /// carries the anchor text and every non-root node its edge `delta`.
    pub(crate) fn to_serializable(&self) -> SerTree {
        // Dense remap: old NodeId → new index, in slab order.
        let mut map: Vec<Option<u32>> = vec![None; self.nodes.len()];
        let mut order: Vec<NodeId> = Vec::new();
        for (id, slot) in self.nodes.iter().enumerate() {
            if slot.is_some() {
                map[id] = Some(order.len() as u32);
                order.push(id);
            }
        }
        let remap = |id: NodeId| map[id].expect("live link points at a live node");
        let nodes = order
            .iter()
            .map(|&id| {
                let n = self.get(id);
                SerNode {
                    parent: n.parent.map(remap),
                    children: n.children.iter().map(|&c| remap(c)).collect(),
                    last_child: n.last_child.map(remap),
                    delta: n.delta.clone(),
                    cursor: (n.cursor.0 as u32, n.cursor.1 as u32),
                    timestamp_unix_ms: system_time_to_unix_ms(n.timestamp),
                    marks: (*n.marks).clone(),
                    seq: n.seq,
                }
            })
            .collect();
        let base = self
            .get(self.root)
            .base
            .as_ref()
            .map(|r| r.to_string())
            .unwrap_or_default();
        SerTree {
            base,
            nodes,
            root: remap(self.root),
            current: remap(self.current),
            next_seq: self.next_seq,
        }
    }

    /// Rebuild an arena tree from a projection. Returns `None` on any structural
    /// inconsistency (out-of-range link, a non-root node missing its delta, a
    /// root carrying one) so a corrupt-but-parseable file degrades to a fresh
    /// tree rather than a broken one. The root's content comes from `base`; the
    /// current node's is materialized on demand from base + deltas.
    pub(crate) fn from_serializable(s: &SerTree) -> Option<Self> {
        let len = s.nodes.len();
        if len == 0 || s.root as usize >= len || s.current as usize >= len {
            return None;
        }
        // Validate links and the root/non-root delta discipline up front.
        for (i, n) in s.nodes.iter().enumerate() {
            let is_root = i as u32 == s.root;
            match (is_root, &n.delta, &n.parent) {
                (true, None, None) => {}
                (false, Some(_), Some(_)) => {}
                _ => return None,
            }
            if let Some(p) = n.parent
                && p as usize >= len
            {
                return None;
            }
            if n.children.iter().any(|&c| c as usize >= len) {
                return None;
            }
            if let Some(c) = n.last_child
                && c as usize >= len
            {
                return None;
            }
        }
        let base = ropey::Rope::from_str(&s.base);
        let depths = depths_from_root(s);
        let nodes: Vec<Option<UndoNode>> = s
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| {
                let is_root = i as u32 == s.root;
                Some(UndoNode {
                    parent: n.parent.map(|p| p as NodeId),
                    children: n.children.iter().map(|&c| c as NodeId).collect(),
                    last_child: n.last_child.map(|c| c as NodeId),
                    delta: n.delta.clone(),
                    base: if is_root { Some(base.clone()) } else { None },
                    rope_cache: None,
                    depth: depths[i],
                    cursor: (n.cursor.0 as usize, n.cursor.1 as usize),
                    timestamp: unix_ms_to_system_time(n.timestamp_unix_ms),
                    marks: Arc::new(n.marks.clone()),
                    seq: n.seq,
                })
            })
            .collect();
        Some(Self {
            nodes,
            free: Vec::new(),
            warm: Vec::new(),
            keyframes: Vec::new(),
            root: s.root as NodeId,
            current: s.current as NodeId,
            next_seq: s.next_seq,
        })
    }
}

/// Depth-from-root of every node in a projection, by BFS over `children`.
///
/// Depth is NOT part of the on-disk format — it is derivable, and the undofile
/// deliberately stores only what is not (issue #302: keyframes are an in-memory
/// cache, so nothing about them enters `SerTree`). The `seen` guard makes this
/// terminate on a malformed file whose links form a cycle; anything unreachable
/// from the root keeps depth 0, which at worst places a keyframe oddly.
fn depths_from_root(s: &SerTree) -> Vec<usize> {
    let mut depths = vec![0usize; s.nodes.len()];
    let mut seen = vec![false; s.nodes.len()];
    let mut queue = std::collections::VecDeque::new();
    seen[s.root as usize] = true;
    queue.push_back(s.root as usize);
    while let Some(i) = queue.pop_front() {
        for &c in &s.nodes[i].children {
            let c = c as usize;
            if !seen[c] {
                seen[c] = true;
                depths[c] = depths[i] + 1;
                queue.push_back(c);
            }
        }
    }
    depths
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

    /// Evict every cache INCLUDING the pinned keyframes (root keeps its `base`),
    /// forcing the next materialization of any node to reconstruct purely from
    /// deltas off the root — the strongest cold path there is.
    fn drop_all_caches(&mut self) {
        for n in self.nodes.iter_mut().flatten() {
            n.rope_cache = None;
        }
        self.warm.clear();
        self.keyframes.clear();
    }

    /// Evict only the ordinary warm LRU, leaving the pinned keyframes — the
    /// steady state a deep history walk actually runs in.
    fn drop_warm_caches(&mut self) {
        for id in std::mem::take(&mut self.warm) {
            if let Some(n) = self.nodes[id].as_mut() {
                n.rope_cache = None;
            }
        }
    }

    /// How many forward delta applies `materialize(id)` would perform right now
    /// (0 when `id` already holds content). This is the cost keyframes exist to
    /// bound, made assertable.
    fn replay_distance(&self, id: NodeId) -> usize {
        let mut n = 0;
        let mut cur = id;
        loop {
            let node = self.get(cur);
            if node.rope_cache.is_some() || node.base.is_some() {
                return n;
            }
            n += 1;
            match node.parent {
                Some(p) => cur = p,
                None => return n,
            }
        }
    }

    /// Reconstruct `id`'s content the naive way: walk to the root and replay
    /// every forward delta off the root `base`, consulting NO cache and NO
    /// keyframe. The differential oracle for keyframe-accelerated
    /// [`Self::materialize`] — the two must agree exactly, always.
    fn materialize_naive(&self, id: NodeId) -> ropey::Rope {
        let mut path = vec![id];
        let mut cur = id;
        while let Some(p) = self.get(cur).parent {
            path.push(p);
            cur = p;
        }
        let mut rope = self
            .get(cur)
            .base
            .clone()
            .expect("the root always carries a base");
        // Skip the root itself (it has no edge delta); replay root-ward → target.
        for &node in path.iter().rev().skip(1) {
            let d = self
                .get(node)
                .delta
                .as_ref()
                .expect("a non-root node always carries its edge delta");
            rope = apply_forward(&rope, d);
        }
        rope
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

// ─── Phase 3a delta-storage tests ─────────────────────────────────────────────
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
            Self(if seed == 0 {
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

    // ── (0) differential oracle: the pre-chunk-walk `diff` ────────────────────
    //
    // The original implementation, verbatim, materializing BOTH ropes with
    // `to_string()` before scanning bytes. `diff` was rewritten to walk chunks
    // instead (no full materialization); this is the semantic pin — the two must
    // agree on the EXACT `Delta` for every input, not merely round-trip.

    fn diff_reference(parent: &ropey::Rope, child: &ropey::Rope) -> Delta {
        let a = parent.to_string();
        let b = child.to_string();
        let ab = a.as_bytes();
        let bb = b.as_bytes();

        let max_pre = ab.len().min(bb.len());
        let mut pre = 0;
        while pre < max_pre && ab[pre] == bb[pre] {
            pre += 1;
        }
        while pre > 0 && !a.is_char_boundary(pre) {
            pre -= 1;
        }

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

    /// Assert the chunk-walking `diff` is byte-identical to `diff_reference`,
    /// over BOTH single-chunk ropes and multi-chunk ones (ropey only splits past
    /// its ~1 KiB leaf size, so short fixtures alone would never exercise the
    /// cross-chunk cursor logic).
    #[track_caller]
    fn assert_diff_matches_reference(sa: &str, sb: &str) {
        let a = ropey::Rope::from_str(sa);
        let b = ropey::Rope::from_str(sb);
        assert_eq!(
            diff(&a, &b),
            diff_reference(&a, &b),
            "diff != reference for {sa:?} -> {sb:?}"
        );
        // Same content, but built by insertion so the two ropes have DIFFERENT,
        // misaligned chunk layouts — the reference sees only bytes, the walker
        // sees chunk seams, and they must still agree.
        let mut a2 = ropey::Rope::new();
        a2.insert(0, sa);
        let mut b2 = ropey::Rope::new();
        for (i, c) in sb.chars().enumerate() {
            b2.insert_char(i, c);
        }
        assert_eq!(
            diff(&a2, &b2),
            diff_reference(&a2, &b2),
            "diff != reference (misaligned chunks) for {sa:?} -> {sb:?}"
        );
    }

    #[test]
    fn diff_matches_reference_on_edge_cases() {
        let cases: &[(&str, &str)] = &[
            // equal / empty
            ("", ""),
            ("", "a"),
            ("a", ""),
            ("abc", "abc"),
            ("café🎉", "café🎉"),
            // prefix-only / suffix-only change
            ("abcdef", "abcdefXY"),
            ("abcdefXY", "abcdef"),
            ("Xabcdef", "abcdef"),
            ("abcdef", "Xabcdef"),
            // change at position 0 and at the very end
            ("abcdef", "Zbcdef"),
            ("abcdef", "abcdeZ"),
            // overlapping repeats — prefix and suffix scans would collide
            ("abcabc", "abc"),
            ("abc", "abcabc"),
            ("aaaa", "aa"),
            ("aa", "aaaa"),
            ("abab", "ababab"),
            ("xyxyxy", "xyxy"),
            // multi-byte chars sitting exactly on the cut points
            ("café", "cafés"),
            ("cafés", "café"),
            ("café", "cafè"),
            ("日本語", "日語"),
            ("日本語", "日本本語"),
            ("🎉🎉🎉", "🎉🎉"),
            ("🎉🎉", "🎉🎉🎉"),
            ("🎉x🎉", "🎉y🎉"),
            ("a🎉b", "a🎊b"),
            ("é", "e"),
            ("e", "é"),
            ("🎉", ""),
            ("", "🎉"),
            // byte-level suffix match that is NOT a char boundary: the tails of
            // 'é' (0xC3 0xA9) and 'é' share no byte, but 日 (E6 97 A5) vs 旦
            // (E6 97 A6) share a two-byte prefix mid-codepoint.
            ("日", "旦"),
            ("x日y", "x旦y"),
            ("語", "誤"),
            // long enough to be multi-chunk in both ropes
            (
                &"the quick brown fox ".repeat(400),
                &"the quick brown fox ".repeat(400),
            ),
        ];
        for (sa, sb) in cases {
            assert_diff_matches_reference(sa, sb);
        }

        // Multi-chunk with an edit in the middle / at each end.
        let big: String = "the quick brown fox jumps over the lazy dog\n".repeat(200);
        let mid = big.len() / 2;
        let mut edited = big.clone();
        edited.insert(mid, 'Z');
        assert_diff_matches_reference(&big, &edited);
        assert_diff_matches_reference(&edited, &big);
        assert_diff_matches_reference(&big, &format!("Z{big}"));
        assert_diff_matches_reference(&big, &format!("{big}Z"));
        assert_diff_matches_reference(&big, &big.repeat(2));

        // Multi-chunk with multi-byte chars straddling likely leaf seams.
        let uni: String = "café 日本語 🎉 αβγ\n".repeat(200);
        let umid = uni.len() / 2;
        let umid = (0..=umid).rev().find(|i| uni.is_char_boundary(*i)).unwrap();
        let mut uedited = uni.clone();
        uedited.insert(umid, '🎊');
        assert_diff_matches_reference(&uni, &uedited);
        assert_diff_matches_reference(&uedited, &uni);
    }

    #[test]
    fn diff_matches_reference_over_random_evolving_content() {
        let mut rng = Rng::new(0x0BAD_F00D_1234_5678);
        let mut s = String::from("seed café 日本語\n🎉");
        for _ in 0..4000 {
            let t = mutate(&s, &mut rng);
            let a = ropey::Rope::from_str(&s);
            let b = ropey::Rope::from_str(&t);
            assert_eq!(diff(&a, &b), diff_reference(&a, &b), "{s:?} -> {t:?}");
            assert_eq!(diff(&b, &a), diff_reference(&b, &a), "{t:?} -> {s:?}");
            s = t;
        }
    }

    #[test]
    fn diff_matches_reference_on_shared_leaf_clones() {
        // The `Arc`-shared-leaf fast path: `child` is a CLONE of `parent` plus
        // one edit, so most chunks are pointer-identical. Exercised at several
        // edit positions across a multi-chunk rope, plus deletes and the
        // degenerate no-op clone.
        let base: String = "the quick brown fox jumps over the lazy dog\n".repeat(300);
        let parent = ropey::Rope::from_str(&base);
        assert_eq!(
            diff(&parent, &parent.clone()),
            diff_reference(&parent, &parent.clone())
        );
        let n = parent.len_chars();
        for at in [0, 1, n / 4, n / 2, n - 1, n] {
            let mut child = parent.clone();
            child.insert_char(at, '𝄞');
            assert_eq!(
                diff(&parent, &child),
                diff_reference(&parent, &child),
                "@{at}"
            );
            assert_eq!(
                diff(&child, &parent),
                diff_reference(&child, &parent),
                "@{at}"
            );
        }
        for at in [0, n / 3, n - 10] {
            let mut child = parent.clone();
            child.remove(at..at + 5);
            assert_eq!(
                diff(&parent, &child),
                diff_reference(&parent, &child),
                "-{at}"
            );
            assert_eq!(
                diff(&child, &parent),
                diff_reference(&child, &parent),
                "-{at}"
            );
        }
    }

    #[test]
    fn diff_matches_reference_over_random_multi_chunk_pairs() {
        // Random pairs built from a multi-chunk corpus, so chunk seams land in
        // arbitrary places relative to the common prefix/suffix.
        let mut rng = Rng::new(0xF00D_BEEF_0BAD_C0DE);
        let units = ["ab", "café ", "日本語", "🎉", "\n", "x", "語日", "é"];
        let build = |rng: &mut Rng| -> String {
            let mut s = String::new();
            for _ in 0..rng.below(400) {
                s.push_str(units[rng.below(units.len())]);
            }
            s
        };
        for _ in 0..300 {
            let sa = build(&mut rng);
            // Half the pairs share a long common prefix/suffix with `sa`.
            let sb = if rng.below(2) == 0 {
                build(&mut rng)
            } else {
                let mut t = sa.clone();
                if !t.is_empty() {
                    let cut = rng.below(t.chars().count() + 1);
                    let byte = t.char_indices().nth(cut).map_or(t.len(), |(i, _)| i);
                    t.insert_str(byte, "🎊zz");
                }
                t
            };
            let a = ropey::Rope::from_str(&sa);
            let b = ropey::Rope::from_str(&sb);
            assert_eq!(diff(&a, &b), diff_reference(&a, &b));
            assert_eq!(diff(&b, &a), diff_reference(&b, &a));
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

    // ── (iv) keyframes: accelerated materialize vs the naive root replay ──────
    //
    // Keyframes (issue #302) pin a materialized rope every `KEYFRAME_INTERVAL`
    // nodes so a cold `g-` replays O(K) deltas instead of O(depth). They are a
    // CACHE: whatever they accelerate must be bit-identical to replaying every
    // delta from the root base with no cache at all. `materialize_naive` is that
    // oracle, in the same spirit as `diff_reference` above.

    /// For every live node: the keyframe-accelerated `materialize` must equal the
    /// naive root-base replay exactly.
    #[track_caller]
    fn assert_materialize_matches_naive(t: &mut UndoTree) {
        for id in t.live_ids() {
            let naive = t.materialize_naive(id).to_string();
            let got = t.materialize_for_test(id).to_string();
            assert_eq!(got, naive, "accelerated != naive root replay for node {id}");
        }
    }

    /// A linear history `n` states deep (so it crosses many keyframe intervals),
    /// plus the expected content of each state indexed by `seq`/depth. Every node
    /// is finalized, including the tip.
    fn deep_linear_history(n: usize) -> (UndoTree, Vec<String>) {
        let base: String =
            "the quick brown fox\njumps over the lazy dog\ncafé 日本語 🎉\n".repeat(20);
        let mut t = UndoTree::new(ropey::Rope::from_str(&base));
        let mut states = vec![base.clone()];
        let mut live = base;
        for i in 0..n {
            // Engine discipline: commit the PRE-edit state, then mutate.
            t.push(entry_str(&live));
            live = format!("e{i} {live}");
            states.push(live.clone());
        }
        // Stash the tip's live content so no node is left holding a stale edge.
        t.sync_current(ropey::Rope::from_str(&live));
        (t, states)
    }

    #[test]
    fn deep_history_walks_back_and_forward_exactly() {
        // The `:earlier 9999` / `:later 9999` shape, deep enough that most jumps
        // land outside the warm window and go through a keyframe.
        let n = 200;
        assert!(n > 4 * KEYFRAME_INTERVAL);
        let (mut t, states) = deep_linear_history(n);

        let mut live = states[n].clone();
        for want in (0..n).rev() {
            let got = t
                .seq_earlier_step(
                    ropey::Rope::from_str(&live),
                    (0, 0),
                    MarkSnapshot::default(),
                )
                .expect("history is deeper than the walk");
            live = got.rope.to_string();
            assert_eq!(live, states[want], "g- onto seq {want}");
        }
        assert!(
            t.seq_earlier_step(
                ropey::Rope::from_str(&live),
                (0, 0),
                MarkSnapshot::default()
            )
            .is_none(),
            "walk ended at the oldest state"
        );
        for (seq, want) in states.iter().enumerate().skip(1) {
            let got = t
                .seq_later_step(
                    ropey::Rope::from_str(&live),
                    (0, 0),
                    MarkSnapshot::default(),
                )
                .expect("history is deeper than the walk");
            live = got.rope.to_string();
            assert_eq!(&live, want, "g+ onto seq {seq}");
        }
        assert_materialize_matches_naive(&mut t);
        assert_warm_equals_cold(&mut t);
    }

    #[test]
    fn keyframes_bound_the_cold_replay_distance() {
        let n = 200;
        let (mut t, _) = deep_linear_history(n);
        // Steady state: the ordinary warm entries have aged out, the keyframes
        // are still pinned. Every node must be within one interval of an anchor.
        t.drop_warm_caches();
        for id in t.live_ids() {
            let d = t.replay_distance(id);
            assert!(
                d < KEYFRAME_INTERVAL,
                "node {id} (depth {}) replays {d} deltas, over the keyframe bound",
                t.get(id).depth
            );
        }
        // Drop the keyframes too and the bound is gone — proof that it is the
        // keyframes doing the bounding and not the warm LRU or the tree shape.
        let deepest = *t
            .live_ids()
            .iter()
            .max_by_key(|&&id| t.get(id).depth)
            .unwrap();
        t.drop_all_caches();
        assert!(t.replay_distance(deepest) > KEYFRAME_INTERVAL);
        // One materialize off the fully-cold tree re-pins the whole ladder.
        t.materialize_for_test(deepest);
        t.drop_warm_caches();
        for id in t.live_ids() {
            assert!(t.replay_distance(id) < KEYFRAME_INTERVAL, "node {id}");
        }
    }

    #[test]
    fn keyframe_materialize_matches_naive_over_random_ops() {
        // Push-heavy op mix so the tree gets deep enough to cross many keyframe
        // intervals, with undo/redo/g-/g+ and periodic `cap` pruning mixed in —
        // pruning renumbers nothing but does free nodes and re-root the tree, so
        // it is where a stale keyframe would surface as corrupted text.
        let mut rng = Rng::new(0x0FF1_CE00_D15E_A5E5);
        let start = "α\nβγ\n日本🎉\nthe quick brown fox\n";
        let mut t = UndoTree::new(ropey::Rope::from_str(start));
        let mut live = start.to_string();

        for step in 0..5000 {
            match rng.below(10) {
                0..=5 => {
                    let pre = live.clone();
                    t.push(entry_str(&pre));
                    live = mutate(&live, &mut rng);
                }
                6 => {
                    if let Some(e) = t.undo_step(
                        ropey::Rope::from_str(&live),
                        (0, 0),
                        MarkSnapshot::default(),
                    ) {
                        live = e.rope.to_string();
                    }
                }
                7 => {
                    if let Some(e) = t.redo_step(
                        ropey::Rope::from_str(&live),
                        (0, 0),
                        MarkSnapshot::default(),
                    ) {
                        live = e.rope.to_string();
                    }
                }
                8 => {
                    if let Some(e) = t.seq_earlier_step(
                        ropey::Rope::from_str(&live),
                        (0, 0),
                        MarkSnapshot::default(),
                    ) {
                        live = e.rope.to_string();
                    }
                }
                _ => {
                    if let Some(e) = t.seq_later_step(
                        ropey::Rope::from_str(&live),
                        (0, 0),
                        MarkSnapshot::default(),
                    ) {
                        live = e.rope.to_string();
                    }
                }
            }
            // Whatever the tree hands back must be what the naive replay of the
            // node it landed on says — checked every step, cheaply.
            let cur = t.current;
            assert_eq!(
                t.materialize_for_test(cur).to_string(),
                t.materialize_naive(cur).to_string(),
                "current node diverged @ {step}"
            );
            if step % 250 == 0 {
                assert_materialize_matches_naive(&mut t);
            }
            if step % 700 == 0 {
                t.cap(60);
            }
        }
        assert_materialize_matches_naive(&mut t);
        assert_warm_equals_cold(&mut t);
    }

    #[test]
    fn deserialized_deep_tree_rebuilds_the_keyframe_ladder() {
        // Depth is NOT serialized (keyframes are a cache, the on-disk format is
        // untouched), so a loaded tree has to recompute it — otherwise every
        // cross-session `g-` would be a full replay again.
        let n = 100;
        let (t, states) = deep_linear_history(n);
        let ser = t.to_serializable();
        let mut back = UndoTree::from_serializable(&ser).expect("valid projection");

        let deepest = *back
            .live_ids()
            .iter()
            .max_by_key(|&&id| back.get(id).depth)
            .unwrap();
        assert_eq!(back.get(deepest).depth, n, "depths recomputed on load");
        assert_eq!(back.materialize_for_test(deepest).to_string(), states[n]);
        assert_materialize_matches_naive(&mut back);

        back.drop_warm_caches();
        for id in back.live_ids() {
            assert!(back.replay_distance(id) < KEYFRAME_INTERVAL, "node {id}");
        }
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
            Self {
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
            Self {
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

// ─── Phase 3b serialize/deserialize tests ─────────────────────────────────────
//
// The undofile is only as trustworthy as this round-trip: a projection that
// loses a branch, mislinks a parent, or reconstructs a node's content wrong
// would silently corrupt cross-session undo. These build the headline tree
// (5 edits, u, u), project it, rebuild, and assert BOTH the per-node content
// (keyed by the stable `seq`) and the live walk (`<C-r>` forward, `u` back)
// survive the trip.
#[cfg(test)]
mod serialize_tests {
    use super::*;

    fn e(text: &str) -> UndoEntry {
        UndoEntry {
            rope: ropey::Rope::from_str(text),
            cursor: (0, 0),
            timestamp: SystemTime::now(),
            marks: MarkSnapshot::default(),
        }
    }
    fn l(text: &str) -> (ropey::Rope, (usize, usize), MarkSnapshot) {
        (ropey::Rope::from_str(text), (0, 0), MarkSnapshot::default())
    }

    /// The headline tree: root "s0", five edits to live "s5", then `u` twice so
    /// `current` sits on "s3" with the forward branch (s4/s5) retained — exactly
    /// the state a `:wq` would persist.
    fn headline_tree() -> UndoTree {
        let mut t = UndoTree::new(ropey::Rope::from_str("s0"));
        for pre in ["s0", "s1", "s2", "s3", "s4"] {
            t.push(e(pre)); // engine discipline: push the PRE-edit live state
        }
        let (r, c, m) = l("s5");
        t.undo_step(r, c, m); // -> s4
        let (r, c, m) = l("s4");
        t.undo_step(r, c, m); // -> s3
        t.sync_current(ropey::Rope::from_str("s3")); // stash exact live, like save
        t
    }

    /// Every node's content (keyed by `seq`), materialized cold-then-warm.
    fn content_by_seq(t: &mut UndoTree) -> std::collections::BTreeMap<u64, String> {
        t.live_ids()
            .into_iter()
            .map(|id| {
                let seq = t.get(id).seq;
                (seq, t.materialize_for_test(id).to_string())
            })
            .collect()
    }

    #[test]
    fn round_trip_reproduces_structure_and_content() {
        let mut orig = headline_tree();
        let cur_seq = orig.current_node_seq();
        let ser = orig.to_serializable();
        let orig_content = content_by_seq(&mut orig);

        let mut back = UndoTree::from_serializable(&ser).expect("valid projection");
        assert_eq!(back.current_node_seq(), cur_seq, "current preserved");
        assert_eq!(back.next_seq, orig.next_seq, "next_seq preserved");
        // Force cold reconstruction (fresh tree has no warm caches) and compare.
        assert_eq!(
            content_by_seq(&mut back),
            orig_content,
            "content at every node reproduced"
        );
        // Six states: s0..s5.
        assert_eq!(orig_content.len(), 6);
        assert_eq!(orig_content[&3], "s3");
        assert_eq!(orig_content[&5], "s5");
    }

    #[test]
    fn deserialized_tree_walks_forward_and_back() {
        let ser = headline_tree().to_serializable();
        let mut t = UndoTree::from_serializable(&ser).unwrap();
        // `<C-r>` twice: s3 -> s4 -> s5 (the retained forward branch).
        let (r, c, m) = l("s3");
        assert_eq!(t.redo_step(r, c, m).unwrap().rope.to_string(), "s4");
        let (r, c, m) = l("s4");
        assert_eq!(t.redo_step(r, c, m).unwrap().rope.to_string(), "s5");
        // `u` all the way back to the root.
        let mut live = "s5".to_string();
        for want in ["s4", "s3", "s2", "s1", "s0"] {
            let (r, c, m) = l(&live);
            assert_eq!(t.undo_step(r, c, m).unwrap().rope.to_string(), want);
            live = want.to_string();
        }
        assert!(t.is_at_root());
    }

    #[test]
    fn from_serializable_rejects_out_of_range_current() {
        let mut ser = headline_tree().to_serializable();
        ser.current = ser.nodes.len() as u32; // past the end
        assert!(UndoTree::from_serializable(&ser).is_none());
    }

    #[test]
    fn from_serializable_rejects_non_root_missing_delta() {
        let mut ser = headline_tree().to_serializable();
        // Blank a non-root node's delta ⇒ structurally invalid ⇒ rejected.
        let victim = if ser.root == 0 { 1 } else { 0 };
        ser.nodes[victim].delta = None;
        assert!(UndoTree::from_serializable(&ser).is_none());
    }

    #[test]
    fn multibyte_content_survives_round_trip() {
        let mut t = UndoTree::new(ropey::Rope::from_str("café\n日本語"));
        t.push(e("café\n日本語"));
        t.push(e("cafés\n日本語"));
        t.sync_current(ropey::Rope::from_str("cafés\n日本語です🎉"));
        let want = content_by_seq(&mut t);
        let ser = t.to_serializable();
        let mut back = UndoTree::from_serializable(&ser).unwrap();
        assert_eq!(content_by_seq(&mut back), want);
    }
}
