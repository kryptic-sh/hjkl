# Undo/redo architecture

Status: **proposal — awaiting sign-off.** No code yet. This document is the
design target for reworking undo/redo so that (1) checkpoint grouping is clean
and cheap, (2) the history becomes an nvim-style **undo tree**, and (3) the tree
can be **persisted to disk** (`undofile`). The three are designed as one model,
not three bolt-ons.

---

## 1. Where we are today

**Storage** (`crates/hjkl-buffer`):

```rust
// undo.rs
pub struct UndoEntry {
    pub rope: ropey::Rope,      // FULL buffer snapshot (O(1) Arc-clone in RAM)
    pub cursor: (usize, usize),
    pub timestamp: SystemTime,
    pub marks: MarkSnapshot,
}

// content.rs — lives on the shared Content (per-document)
undo_stack: Vec<UndoEntry>,
redo_stack: Vec<UndoEntry>,
```

**Semantics** (`crates/hjkl-engine/src/editor.rs`):

- `push_undo()` snapshots current state onto `undo_stack`, then **clears
  `redo_stack`**, then `cap_undo(undo_levels)`.
- `undo()`/`redo()` move one entry between the two stacks.
- `earlier_by_steps`/`later_by_steps` — linear N-step travel.
- `earlier_by_time`/`later_by_time` — walk the linear stacks by `timestamp`
  (`:earlier 5s` / `:later 5s`).
- Insert sessions are grouped ad-hoc via `break_undo_group_in_insert`.
- `undo_line` (`U`) is separate, driven by `ChangeBank::u_line`.

**Gaps this rework targets:**

| Gap                   | Consequence                                                                                  |
| --------------------- | -------------------------------------------------------------------------------------------- |
| No group primitive    | 115 `push_undo` sites each checkpoint independently; `:g`/`@reg`/`:normal` over-checkpoint   |
| Collapsing is manual  | `:normal` uses a fragile `base_len` + `pop_last_undo()` loop (breaks on early return/panic)  |
| Linear, redo-clearing | New edit after undo **discards** the redo branch — no nvim undo tree, `g-`/`g+` lose history |
| Full-rope entries     | Fine in RAM (Arc-clone); **fatal for an undofile** (a whole buffer per node)                 |
| Not serializable      | `ropey::Rope` + `SystemTime` don't cleanly persist; no format, no file-identity model        |

The `:g`-granularity bug (issue #2 / this doc's Phase 1) is a _symptom_ of the
first two rows.

---

## 2. Goals / non-goals

**Goals**

- One undo step per user-intent action (fixes the `:g`/`:normal`/macro
  over-checkpointing) — RAII-safe, no manual pop loops.
- nvim-parity **undo tree**: undoing then editing creates a _branch_, not a
  wipe; `u`/`<C-r>` walk the current branch, `g-`/`g+` walk the whole tree by
  time, `:undolist` shows branches.
- **Disk persistence** (`undofile`): compact, versioned, keyed to file identity,
  safe to ignore on mismatch — matching `:h persistent-undo`.
- No regression to the cheap in-RAM path (rope Arc-clones stay the hot path).

**Non-goals (for now)**

- Cross-file/global undo. Undo stays per-document (per `Content`).
- Byte-exact undofile compatibility with nvim's format (we define our own,
  versioned format; we match _behavior_, not their binary layout).

---

## 3. The unifying model: a tree of states linked by reversible deltas

One data structure underlies all three phases.

```
        n0 (root: buffer as opened / last save)
        │  Δ0  (reversible change)
        n1
        │  Δ1
        n2 ── Δ2b ── n4        ← branch: n2 had a 2nd child after an undo+edit
        │  Δ2a
        n3  (current)          ← `current` pointer
```

- A **node** is a buffer _state_ the user could land on = the boundary of one
  **undo group**.
- An **edge** is a **reversible delta** `Δ` between parent and child.
- `current` is a pointer to the node the buffer currently shows.
- `u` = move `current` to its parent (apply the inverse delta). `<C-r>` = move
  to the **last-visited** child (apply the forward delta). `g-`/`g+` = move to
  the node with the next-lower/next-higher **global seq** (time order) anywhere
  in the tree. A new edit from `current` appends a **new child** — siblings (old
  redo branches) are **retained**, not cleared.

Why this model makes everything fall out:

- **Grouping** = "a node is created only at group boundaries." Phase 1 is just
  the boundary policy; the linear stack is a degenerate tree (each node has ≤1
  child) so Phase 1 needs no tree yet.
- **Undo tree** = allow a node to have >1 child (stop clearing the redo branch).
- **Persistence** = serialize the nodes + deltas + `current`/seq. Deltas (not
  full ropes) keep the file small.

### Delta representation

```rust
/// Reversible edit between two adjacent states, at byte granularity on the rope.
struct Delta {
    start: usize,        // byte offset into the PARENT state
    old: Box<str>,       // bytes present in parent, absent in child
    new: Box<str>,       // bytes present in child, absent in parent
}
```

- Forward (redo): replace `parent[start..start+old.len()]` with `new`.
- Inverse (undo): replace `child[start..start+new.len()]` with `old`.
- A group with several edits collapses to the **minimal single (start, old,
  new)** spanning them (or a small `Vec<Delta>` when they're disjoint — start
  with `Vec<Delta>`, optimize to a coalesced single later).
- Cursor + marks: store the **post-state cursor** and a `MarkSnapshot` delta (or
  full snapshot; marks are small) per node, as today.

### In-RAM materialization (keep it fast)

Deltas are **canonical**; we do **not** give up the O(1) rope hot path:

- Each node optionally caches a materialized `ropey::Rope` (Arc-clone — cheap).
- Keep the materialized rope for the **current node** and a bounded LRU of
  recently visited nodes; drop the rest to deltas.
- `u`/`<C-r>` between adjacent cached nodes = today's snapshot restore (fast).
  Jumping to a cold node = walk deltas from the nearest cached ancestor (rare,
  bounded).
- Optional **keyframes**: every K nodes, cache a full rope so cold jumps are
  O(K) deltas, not O(depth). Tunable; start without, add if profiling wants it.

This means Phase 1 can literally keep `UndoEntry.rope` and add grouping; the
delta form is introduced in Phase 2 without changing the public undo API.

---

## 4. Phase 1 — grouping primitive (fixes issue #2, additive, low-risk)

**No format change. No tree yet.** Introduce an explicit group boundary and make
`push_undo()` coalesce within it.

### API

```rust
impl Editor {
    /// Open an undo group. All push_undo() calls until the guard drops
    /// collapse into a single undo step. Re-entrant (depth-counted): nested
    /// groups commit only at the outermost close.
    #[must_use]
    fn undo_group(&mut self) -> UndoGroup<'_>;
}

// RAII guard — closing on drop makes it exception/early-return safe.
struct UndoGroup<'a> { /* &mut Editor, prev_depth */ }
```

### Semantics

- Buffer gains `undo_group_depth: u32` and a per-group `armed: bool`.
- `push_undo()`:
  - If `depth == 0`: behaves exactly as today (snapshot + clear redo).
  - If `depth > 0`: **coalesce** — take the snapshot **only on the first real
    mutation in the group** (`armed`), suppress the rest. No create-then-pop.
- **Lazy first snapshot via `dirty_gen`:** arm the snapshot on the first
  `push_undo()` whose `dirty_gen` differs from the group's opening `dirty_gen`.
  A group that mutates nothing (`:g` matching zero lines) creates **zero**
  entries. A `push_undo()` whose state equals the top entry (no-op) is skipped.
- Nested groups: inner `undo_group()` just increments depth; only the outermost
  drop commits. Correct for `:g` whose sub-command is itself grouped.

### Call-site impact

- The **115 `push_undo()` sites stay unchanged** — inside a group they coalesce
  automatically. This is the key to low risk.
- New wrappers (one line each): `global_handler` (fixes #2 for the whole
  `:g`/`:v` family at once), and `:normal` **drops its
  `base_len`/`pop_last_undo` hack** for `let _g = self.undo_group();`.
- Opportunistic adopters (same primitive, unifies today's ad-hoc grouping):
  `@reg` macro replay (one undo per macro), `[count].` dot-repeat, multi-key
  operators, and eventually `break_undo_group_in_insert` (insert session = a
  group that stays open until a break).

### Perf

`:g`/`:%normal` over N lines: **N checkpoints → 1**, and the `cap_undo` churn
those N entries caused disappears. Empty/no-op groups cost nothing.

### Tested

`u`/`<C-r>` are normal-mode keys the **compat-oracle can drive against real
nvim**. Add oracle cases: after `:g/re/normal …` (or a macro), a single `u`
reverts the whole thing — matching nvim. Plus hand-authored `:g`/`:normal` unit
tests asserting `undo_stack_len` deltas.

---

## 5. Phase 2 — the undo tree + delta entries (internal, behind the API)

Replace the linear `undo_stack`/`redo_stack` with the tree from §3, and switch
node payloads from full ropes to deltas (+ cached materialization).

### Structure

```rust
struct UndoNode {
    parent: Option<NodeId>,
    children: Vec<NodeId>,      // >1 child ⇒ a branch point
    last_child: Option<NodeId>, // which child <C-r> follows
    delta: Option<Delta>,       // edit from parent → this node (root: None)
    seq: u64,                   // global monotonic order (for g-/g+ and :undolist)
    timestamp: SystemTime,
    cursor: (usize, usize),
    marks: MarkSnapshot,
    rope_cache: Option<ropey::Rope>, // materialized state (LRU-evicted)
}

struct UndoTree {
    nodes: Slab<UndoNode>,      // arena; NodeId = index+generation
    root: NodeId,
    current: NodeId,
    next_seq: u64,
    save_seq: Option<u64>,      // seq at last write → drives the 'modified' flag
}
```

### Operation mapping

| Action              | Tree op                                                                           |
| ------------------- | --------------------------------------------------------------------------------- |
| edit (group commit) | new child of `current`, apply forward; **do not** drop `current`'s other children |
| `u`                 | `current = current.parent`; apply inverse delta                                   |
| `<C-r>`             | `current = current.last_child`; apply forward delta                               |
| `g-` / `:earlier N` | move to node with next-lower `seq` (or by `timestamp`) tree-wide                  |
| `g+` / `:later N`   | move to node with next-higher `seq`/time                                          |
| `:earlier 5s` etc.  | same time-walk, now over the whole tree instead of a linear stack                 |
| `:undolist`         | enumerate branch points + leaf `seq`/time (new, nvim-parity)                      |

- `undo_levels` becomes a **node budget**: prune the oldest leaves (lowest seq,
  not on the path to `current`) when exceeded.
- `U` (`undo_line`) stays `ChangeBank`-driven; unaffected structurally.
- Cursor/marks restore exactly as today, read from the destination node.

### Migration

- Introduce the tree behind the **existing** `undo()/redo()/earlier_*/later_*`
  signatures so `editor.rs` callers and the 115 sites don't change.
- Delta computation hooks the existing `mutate_edit`/`ContentEdit` path — we
  already emit per-edit change descriptions there, so a group can accumulate its
  net delta instead of snapshotting a rope.
- Keep a feature flag / fallback to the linear path during bring-up; delete once
  the oracle tree-suite is green.

### Tested

`u`, `<C-r>`, `g-`, `g+` are all oracle-drivable → pin branch behavior directly
against nvim (e.g. `iA<Esc>uiB<Esc>g-g-g+` and assert buffer+cursor match nvim's
tree walk). This is the highest-value verification and it's essentially free.

---

## 6. Phase 3 — disk persistence (`undofile`)

Serialize the Phase-2 tree. Because nodes hold **deltas**, the file is compact.

### Format (`undofile`)

```
Header:
  magic         b"HJKLUNDO"
  format_version u32                 // bump on incompatible change
  buffer_hash    [u8; 32]            // hash of the on-disk file contents at save
  buffer_mtime   i64                 // sanity cross-check
  save_seq       u64                 // node seq that equals the saved file
Body (serde, e.g. bincode/postcard):
  nodes: Vec<SerNode>               // parent id, delta {start,old,new}, seq, ts, cursor, marks
  root, current, next_seq
```

- **File identity:** on open, compute the buffer hash; load the undofile only if
  `buffer_hash` matches (else ignore — never corrupt). Matches nvim's
  content-hash keying, safer than path-only.
- **Location:** `undodir` setting (default under the XDG state dir), filename
  derived from the absolute path (percent/`%`-escaped like nvim, or a hash).
- **Triggers:** write on `:w` (if `undofile` set) and `:wundo {file}`; read on
  buffer load and `:rundo {file}`. Settings: `undofile` (bool), `undodir` (path
  list), `undolevels` (already exists, becomes the node budget), `undoreload`
  (max lines to keep undo across a reload).
- **Versioning:** `format_version` gate; unknown/newer → ignore with a message.
  Deltas are self-describing, so forward migration is a node-walk.
- **Bounds:** cap serialized size (drop oldest leaves first, like the in-RAM
  budget); large `old`/`new` blobs can be zstd-compressed in the body.

### Why deltas are required here

A 1 MB buffer with 500 undo states as full ropes = ~500 MB undofile. As deltas
(typical edit = a few bytes) it's kilobytes. This is the concrete reason Phase 2
switches off full-rope entries before Phase 3.

---

## 7. Cross-cutting: performance

- Hot path unchanged: adjacent `u`/`<C-r>` between cached nodes = rope Arc-clone
  restore (today's speed).
- Grouping removes bulk-op checkpoint churn (Phase 1).
- Delta entries shrink RAM per node and make the undofile small (Phase 2/3).
- Cold-node jumps bounded by keyframe spacing K (opt-in).
- Marks: currently a full `MarkSnapshot` per entry — small, keep as-is; revisit
  only if profiling flags it.

## 8. Rollout, risk, testing

1. **Phase 1** ships first as its own slice(s): `UndoGroup` guard + coalesce +
   lazy snapshot; wrap `:g`/`:v`, de-hack `:normal`. Oracle + unit tests. Closes
   issue #2. Low blast radius (no format/API change).
2. **Phase 2** behind the existing undo API + a fallback flag; extensive oracle
   tree-walk suite (`u`/`<C-r>`/`g-`/`g+`) vs nvim before removing the flag.
3. **Phase 3** last; format is versioned from day one; hash-gated load is
   fail-safe (ignore on mismatch).

**Risks & mitigations**

- _Delta correctness_ (wrong inverse corrupts text) → property tests: for random
  edit sequences, `apply(Δ)` then `apply(Δ⁻¹)` round-trips the rope; oracle
  cross-checks against nvim.
- _115-site semantics drift_ → Phase 1 keeps them literally unchanged; Phase 2
  keeps their signatures.
- _Tree memory growth_ → node budget = `undolevels`, prune oldest off-path
  leaves.
- _Undofile poisoning_ → content-hash gate + version gate + size cap.

## 9. Open questions (decide before Phase 2/3)

- `g-`/`g+` ordering: strictly by `seq`, or by wall-clock `timestamp` (nvim uses
  a change-number that is effectively seq)? Proposal: `seq`, with
  `:earlier/later Ns` using `timestamp`.
- Delta granularity: single coalesced `(start, old, new)` per group vs
  `Vec<Delta>`? Proposal: `Vec<Delta>` first (simple, correct), coalesce later.
- Keyframe spacing K: start unset (materialize current + LRU only), add if cold
  jumps profile hot.
- Undofile compression: zstd the body, or per-blob? Proposal: whole-body, behind
  the format version.
- Do we persist marks/cursor per node in v1, or only text deltas? Proposal:
  persist cursor (cheap, better UX on reload); marks optional.

---

**Requested decision:** approve this direction (or adjust §9), then Phase 1
lands as the first implementation slice.
