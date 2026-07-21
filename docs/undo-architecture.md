# Undo/redo architecture

Status: **proposal — awaiting sign-off.** No code yet. This document is the
design target for reworking undo/redo so that (1) checkpoint grouping is clean
and cheap, (2) the history becomes an nvim-style **undo tree**, (3) the tree is
**persisted to disk** (`undofile`) with **position restore** across sessions,
(4) the **uncommitted tail** rides the existing swap file so crash recovery
keeps the undo history, and (5) a small **shada-style state store** remembers
the last cursor per file. All five are one coherent model (a tree of states
linked by reversible deltas), not five bolt-ons, and the split across undofile /
swap / state store mirrors how neovim separates the same concerns (§5c).

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
- **Position restore:** reopening a file lands on the exact undo node it was
  saved on, with redo still able to walk forward (see §6).
- **Cross-session cursor memory:** reopening a file restores the last cursor
  position on that buffer (the last-moved window wins when several share it),
  best-effort — like vim's `'"` mark / nvim shada (see §6b). Independent of the
  undo tree; can ship on its own.
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

## 5c. Prior art — how nvim and helix handle this

**Neovim** uses **three separate systems**, and our design independently
converged on the same split (with the same validity models):

- **`undofile`** (persistent undo): written on `:w` to `undodir`; header carries
  a magic + version + **hash of the buffer contents**; on read a hash mismatch ⇒
  undo is **refused** (never applied stale). Stores the **whole undo tree** as
  **diffs** (changed lines per `u_entry`, not full snapshots) plus sequence
  numbers, so reopening positions at the file-matching state and **redo walks
  forward** — the exact scenario in §6. Bounded by `undolevels`/`undoreload`.
- **shada** (`:h shada`, viminfo's successor): the `'"` mark = cursor on last
  leaving a buffer, jumped to on open (best-effort, **clamped**),
  **path-keyed**, capped to ~100 files. This is our §6b state store.
- **swap** (`.swp`): unsaved content + header (path, mtime, inode, PID, cursor),
  written on `updatetime`, deleted on clean exit, for `:recover`. **Does not
  preserve the undo tree** — recovery gives text but flat undo.

Two decisions this validates directly: **diffs not snapshots**, and
**content-hash gating**. It also exposes a gap (below) we can beat.

**Helix** keeps undo as an **in-memory tree** of reversible `Transaction` /
`ChangeSet` values (retain/insert/delete ops over the Rope, invertible against
the original doc) — the cleanest existing form of our "reversible delta," worth
borrowing over a bare `(start, old, new)` for multi-region groups. But Helix has
**no persistent undo, no shada equivalent, and no swap** — undo and cursor are
lost on close. It validates the _model_, offers no _persistence_ prior art.

**Where we improve on both:** nvim's (and vim's) swap **loses the undo tree** on
`:recover`. Because this repo's swap already persists unsaved content + cursor
(see §6c), extending it to also carry the **uncommitted undo tail** lets
recovery restore the undo history too — better than either editor.

---

## 6. Phase 3 — disk persistence with position restore (`undofile`)

Serialize the Phase-2 tree **plus the current position**, so reopening the same
file restores the exact node the user was on and lets `u`/`<C-r>` keep walking —
forward _and_ back — across sessions. Because nodes hold **deltas**, the file is
compact.

### Target scenario (the contract)

1. User makes 5 edits → `current = n5`.
2. `u` twice → `current = n3`.
3. `:wq` — the on-disk file now holds **n3's content**; the undofile stores the
   **whole tree** (n0…n5, including the forward nodes n4/n5), `current = n3`,
   and the hash of n3.
4. Reopen the file → hash(disk) == stored current-hash → load tree, set
   `current = n3`, buffer shows n3.
5. `<C-r>` twice → forward deltas n3→n4→n5 replay → back on **n5** (buffer now
   modified relative to disk, exactly like never having closed).

### The invariant that makes this safe

**Persist only on `:w`, and record `current` as the just-saved node.** At write
time the buffer _is_ `current`, so `current`'s content == the file on disk. Thus
the persisted `current` always equals the on-disk state — reopening never shows
"phantom" unsaved content. Forward nodes (n4/n5 above) still live in the
serialized tree, so `<C-r>` can reconstruct them, but `current` itself is always
anchored to what's actually on disk. (Recovering _unsaved_ in-memory edits is a
**swap-file** concern, deliberately out of scope here — undo persistence and
swap recovery stay separate.)

### Format (`undofile`)

```
Header:
  magic          b"HJKLUNDO"
  format_version u32            // bump on incompatible change
  content_hash   [u8; 32]       // hash of the on-disk file == current node at save
  file_size      u64            // cheap pre-check before hashing
  file_mtime     i64            // cheap pre-check; not authoritative
  current_seq    u64            // node the buffer was on at save (== saved content)
  header_crc     u32            // header integrity
Body (serde, e.g. bincode/postcard, then whole-body checksum):
  nodes: Vec<SerNode>           // parent id, delta {start,old,new}, seq, ts, cursor, (marks?)
  root, next_seq
  body_crc       u32            // truncated/partial write ⇒ ignore whole file
```

- **File identity & position restore:** on open, stat the file (size/mtime cheap
  gate), then hash it. If `content_hash` matches, load the tree and set
  `current = current_seq`'s node — position restored, redo/undo both live.
- **Location:** `undodir` setting (default under the XDG state dir); filename =
  hash of the absolute path (avoids `%`-escaping collisions). Never written
  beside the file.
- **Triggers:** write on `:w`/`:wq` (when `undofile` is set) and
  `:wundo {file}`; read on buffer load and `:rundo {file}`. Settings: `undofile`
  (bool), `undodir` (path list), `undolevels` (node budget), `undoreload` (max
  lines to keep undo across an external reload).
- **Versioning:** `format_version` gate; unknown/newer → ignore with a message.
- **Bounds:** cap serialized size (drop oldest off-path leaves first, like the
  in-RAM budget); zstd the body behind the format version.

### Invalidation / reconciliation matrix

The single authority is the **content hash**; everything degrades safely to
"start fresh from the file as a new root" — an undofile is never allowed to
corrupt a buffer.

| On reopen, situation                                  | Detection                                    | Action                                                                                                                                                                                  |
| ----------------------------------------------------- | -------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Clean: file == saved `current`                        | `hash(disk) == content_hash`                 | Load tree, restore `current`, full undo+redo (the scenario)                                                                                                                             |
| Force-quit **without** saving after edits             | undofile is from the last `:w`; file too     | Still clean — file == last-saved node == undofile's `current`. Post-save edits were never persisted; lost. No stale state to invalidate (that's why we persist only on `:w`)            |
| File changed externally (git pull, other editor)      | `hash(disk) != content_hash`                 | Default: **discard** undofile, root = current file. Opt-in (`undoreconcile`): scan tree for a node whose hash == disk; if found, rebase `current` there and keep the tree; else discard |
| File truncated / partially written / corrupt undofile | `header_crc`/`body_crc` mismatch, short read | Ignore undofile entirely, root = current file                                                                                                                                           |
| Undofile version newer/unknown                        | `format_version` gate                        | Ignore with a message                                                                                                                                                                   |
| File missing / undofile missing                       | stat fails                                   | No persistence this session; fresh tree                                                                                                                                                 |
| Path reused for a different file                      | hash mismatch (content, not path)            | Same as "changed externally" — discard/reconcile                                                                                                                                        |

Two guarantees fall out:

- **Never trust path or mtime alone** — content hash decides. A `git pull` that
  happens to preserve mtime is still caught.
- **Fail safe, never fail dangerous** — any doubt ⇒ discard the undofile and
  treat the on-disk file as a pristine root, so the worst case is "you lost your
  cross-session undo," never "your buffer got corrupted."

### Why deltas are required here

A 1 MB buffer with 500 undo states as full ropes = ~500 MB undofile. As deltas
(typical edit = a few bytes) it's kilobytes. This is the concrete reason Phase 2
switches off full-rope entries before Phase 3.

---

## 6b. Companion: cross-session cursor & file-state store

Cursor memory is **not** part of the undofile, and **not** the swap file. It is
its own small **shada/viminfo-style state store**. Three persistence artifacts,
three distinct jobs and lifetimes:

| Artifact                        | Holds                                                                  | Keyed by        | Lifetime / validity                                                    |
| ------------------------------- | ---------------------------------------------------------------------- | --------------- | ---------------------------------------------------------------------- |
| **undofile** (§6, new)          | durable undo tree + saved `current` **as of the last `:w`**            | content hash    | long-lived; **discarded** on hash mismatch (external change)           |
| **swap** (`HSWP`, exists → §6c) | unsaved content + cursor + identity; **+ uncommitted undo tail** (new) | path hash + PID | **ephemeral** — live edit only, deleted on clean `:wq`; crash recovery |
| **state store** (§6b, new)      | last cursor per file (later: jumplist, marks, `"`)                     | file path       | long-lived; **best-effort**, survives external change by clamping      |

**Why cursor position lives here, not in the other two**

- _Not the undofile:_ the undofile is content-hash gated and thrown away when
  the file changes externally (git pull, other editor). Cursor memory must
  **survive** that — you still want to land near where you were, just clamped.
  Different validity model ⇒ different store.
- _Not the swap file:_ swap is crash-recovery of unsaved content and is
  **deleted on clean exit** — by the time you reopen normally it's already gone,
  so it cannot carry cross-session cursor state. (Swap does stash a cursor for
  the recovery flow, but that's only for recovering an unsaved session, not
  normal reopen.)

### What & where

```
state store (single file, e.g. undodir/../state or XDG state dir):
  format_version u32
  entries: Map<path_hash, FileState>          // capped, LRU by last_seen
FileState {
  path            String        // for collision check on the hashed key
  cursor          (row, col)     // last-moved cursor on the buffer (see below)
  content_hash    [u8; 32]       // optional: exact-restore vs clamp decision
  last_seen       SystemTime     // for LRU capping (à la shada's file cap)
}
```

- **Single indexed store, capped** (like shada's ~100-file cap), not a file per
  document — cursor records are tiny and per-file sprawl isn't worth it.
- Best-effort restore, **never gated, never errors**: on open, clamp `row` to
  `[0, last_line]` and `col` to the line's length. If `content_hash` matches,
  restore exactly; if not, still restore the clamped row (position may drift —
  acceptable, matches vim).

### Multi-window semantics (the "last-moved window wins" requirement)

The cursor is **per-window** (it lives on the per-window `View`, #151), but the
_document_ is the shared `Content`. To remember "the last cursor moved on this
buffer" regardless of which split:

- Add `Content.last_cursor: (usize, usize)`, updated whenever **any** view
  commits a cursor move on that `Content`. Two windows on one buffer both write
  it; the most recent move wins — exactly the requested behavior.
- Persist `Content.last_cursor` (not any single view's) at write/close/exit.

### Triggers & settings

- Update `Content.last_cursor` in memory on cursor move (cheap, no I/O).
- Persist on buffer close, `:w`, and editor exit — **debounced**, never
  per-keystroke.
- Setting to enable (shada-style), e.g. `restorecursor` / a `shada`-like option;
  default on. Independent of `undofile`.

### Independence & rollout

This needs **none** of the undo-tree work (Phases 1–2) — it can ship as its own
small slice **before or in parallel** with the undo phases. It only shares the
generic persistence plumbing (XDG state dir, versioned format, path keying,
fail-safe load). Future growth (jumplist, global marks, `:h '"`, registers,
search history) extends `FileState`/the store, evolving it into a proper shada
equivalent.

---

## 6c. Splitting the undo tree across `undofile` + swap (crash recovery)

The undo tree splits cleanly by **durability trigger**, and this repo **already
has the swap half**: `crates/hjkl-app/src/swap.rs` writes `HSWP` v2 files
holding `{canonical_path, file_mtime, write_time, cursor, writer_pid}` + the
unsaved buffer text, keyed by `fnv1a64(path)`, gated on `dirty_gen`,
PID-guarded, deleted on clean exit (`-r` lists them, `-n` disables). So
crash-recovery infra exists; we extend it rather than invent it.

### The split

| Part of the tree                          | Where         | Trigger                   | Matches disk? |
| ----------------------------------------- | ------------- | ------------------------- | ------------- |
| Nodes **up to the last `:w`** + saved ptr | **undofile**  | on `:w`                   | yes (hash)    |
| **Tail since the last `:w`** + live ptr   | **swap** (v3) | on `dirty_gen` (like now) | no (unsaved)  |

- **On `:w`:** fold the swap's tail into the undofile (now durable), then reset
  the swap tail — the content is saved, so the tail is empty and the swap's
  `current` == the saved node.
- **On crash + `:recover`:** `undofile (last save)` + `swap tail` reconstructs
  the full buffer **and** the full undo tree **and** the live `current`/cursor —
  vim and nvim only recover the text here (flat undo), so this is a strict
  improvement.
- **On clean exit:** swap deleted (nothing uncommitted); undofile holds
  everything; state store holds the clean-close cursor.

### Swap format extension (`HSWP` v2 → v3)

Add to the swap body: the **uncommitted undo tail** (a `Vec<Delta>`-encoded log
of groups committed since the last save) and the **live `current` pointer**.
postcard is not self-describing, so old v2 files deserialize as `Err` and are
already treated as "no usable swap" — the bump is safe by construction. The
header already has `cursor`; keep it as the live recovery cursor.

### Shared primitives (avoid three encoders)

- **One `Delta` type** (borrow Helix's retain/insert/delete `ChangeSet` for
  multi-region groups) used by the in-RAM tree edges, the undofile nodes,
  **and** the swap tail. One serializer, three consumers, guaranteed consistent.
- **Three cursors, three lifetimes, not redundant:** swap = live recovery
  cursor; undofile = saved-node cursor; state store = clean-close cursor. Each
  is the authority for its own scenario; during `:recover` the swap cursor wins.
- **Append-log option:** the swap tail can be an append-only delta log (cheaper
  than the current full-body rewrite, crash-safe via per-record CRC).
  Optimization, not required for a first cut — the existing `dirty_gen`-gated
  full write works.

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

Ordered by dependency and risk. The **state store (§6b)** and **Phase 1** are
independent of everything else and can go first / in parallel.

0. **State store (§6b)** — cross-session cursor. No dependency on the undo work;
   shares only the generic persistence plumbing. Small, self-contained slice.
1. **Phase 1 (§4)** — `UndoGroup` guard + coalesce + lazy snapshot; wrap
   `:g`/`:v`, de-hack `:normal`. Oracle + unit tests. **Closes issue #2.** No
   format/API change, low blast radius.
2. **Phase 2 (§5)** — the arena tree + delta nodes behind the existing undo API,
   behind a fallback flag; extensive oracle tree-walk suite
   (`u`/`<C-r>`/`g-`/`g+`) vs nvim before removing the flag. Introduces the
   shared `Delta`/`ChangeSet` type.
3. **Phase 3 (§6)** — `undofile` (durable tree, hash-gated, position restore),
   **plus** the swap `v2→v3` tail extension (§6c) reusing Phase 2's `Delta`.
   Format versioned from day one; every load path fail-safe (ignore on
   mismatch).

**Risks & mitigations**

- _Delta correctness_ (wrong inverse corrupts text) → property tests: for random
  edit sequences, `apply(Δ)` then `apply(Δ⁻¹)` round-trips the rope; oracle
  cross-checks against nvim.
- _115-site semantics drift_ → Phase 1 keeps them literally unchanged; Phase 2
  keeps their signatures.
- _Tree memory growth_ → node budget = `undolevels`, prune oldest off-path
  leaves.
- _Undofile / swap poisoning_ → content-hash (undofile) + version + CRC + size
  cap on both; anything wrong ⇒ discard, treat the file as a pristine root.
- _Swap regression_ → the `v2→v3` bump must not break existing recovery; old v2
  files already read as `Err` ("no usable swap"), and the PTY recovery tests
  (`apps/hjkl/tests/pty_harness/recovery.rs`) must stay green.

## 9. Open questions (decide before Phase 2/3)

- `g-`/`g+` ordering: strictly by `seq`, or by wall-clock `timestamp` (nvim uses
  a change-number that is effectively seq)? **Proposal:** `seq`, with
  `:earlier/later Ns` using `timestamp`.
- Delta representation: single `(start, old, new)` vs a Helix-style
  retain/insert/delete **`ChangeSet`** per group? **Proposal:** `ChangeSet`
  (composes/inverts cleanly for grouped multi-region edits), `Vec<Delta>` as the
  simple fallback if `ChangeSet` is too much for a first cut.
- State store shape: single capped shada-like index vs one tiny file per
  document? **Proposal:** single capped index (shada-like; ~a few hundred files,
  LRU).
- Cursor restore validity: hash-match ⇒ exact (row+col); mismatch ⇒ clamp row
  only, or skip? **Proposal:** always clamp-restore (row+col), best-effort.
- Keyframe spacing K: start unset (materialize current + LRU only), add if cold
  jumps profile hot.
- Undofile/swap compression: zstd whole-body behind the format version — ok?
- Swap tail cadence: keep the current `dirty_gen`-gated full-body write, or move
  to an append-only delta log? **Proposal:** full-body first, append-log later.

---

**Requested decision:** approve this direction (or adjust §9). Suggested first
slices to implement after sign-off, in any order between the two: the **state
store (§6b)** and **Phase 1 (§4, closes #2)** — both are self-contained and
low-risk.
