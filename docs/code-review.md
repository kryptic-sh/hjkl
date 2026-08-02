# Code review — 2026-08-03

**Scope:** full codebase at `ba9f9a58` (tree clean), focus on the two most
recent undo-history commits (`82cca4bb`, `ba9f9a58`) and the wider vim/engine
layers. Three read-only `review` sub-agents (`hjkl-buffer/undo.rs`,
`hjkl-vim/src/`, `hjkl-engine/src/`). Every finding was re-verified by
re-reading the cited `file:line`, tracing the failure scenario, and confirming
reachability.

## Findings

### 1. Cycle → infinite loop in `materialize` and `retarget_current` (HIGH)

**`crates/hjkl-buffer/src/undo.rs:593-608`, `:913-926`**

`from_serializable` validates parent/child/last_child indices are in range
(lines 1362-1383) but does not detect parent-link cycles. A `SerTree` with a
cycle (e.g. node A's parent is B, B's parent is A, neither is root) passes
validation and is loaded.

Both `materialize` (line 593) and `retarget_current` (line 918) then walk parent
links in unbounded `loop`/`while` with no cycle guard:

- `materialize` at line 593:
  `loop { path.push(anchor); let par = self.get(anchor).parent.expect(…); … anchor = par; }`
  — infinite, grows `path` unboundedly.
- `retarget_current` at line 918:
  `while !self.get(node).on_path { … node = p; }` — infinite when no ancestor in
  the cycle carries `on_path`.

The `on_path` setup loop at line 1419 IS bounded (`for _ in 0..len`), and
`depths_from_root` has a `seen` guard (line 1463) — but neither runs inside
`materialize` or `retarget_current`, which are the hot paths called after
loading.

```
Repro: SerTree with s.nodes[1].parent = Some(2), s.nodes[2].parent = Some(1),
       s.current = 1, s.root = 3 (independent, valid root).
       from_serializable(&s) → Some(tree)  (passes validation)
       tree.materialize(1) or tree.retarget_current(1) → hangs
Expect: from_serializable returns None (rejects corrupt input)
```

Only reachable with a corrupted undofile — no normal operation creates cycles.

---

### 2. Stale `sticky_col` after `:s` (HIGH)

**`crates/hjkl-engine/src/substitute.rs:372-373`, `:585-586`**

Both `apply_substitute` and `apply_collected_matches` move the cursor via
`ed.buffer_mut().set_cursor(hjkl_buffer::Position::new(…))` directly, bypassing
`Editor::jump_cursor`. This means `Editor::sticky_col` (vim's `curswant`) is
never reset. After `:s/pat/rep/`, the next `j`/`k` aims at the pre-substitute
column instead of the column where the cursor actually landed.

The same pattern in `apply_fold_op` (`editor.rs:2531-2533`) correctly sets
`self.sticky_col = Some(0)` manually. The substitute paths have no equivalent.

```
Repro: "abcdefgh\nab\nabcdefgh", cursor (0,7)='h', sticky_col=Some(7)
       :s/ab/XX/  → cursor moved to (1,0) by set_cursor, sticky_col stays Some(7)
       j          → Move::Vertical { row: 2 } reads want=7, lands (2,7)
Expect: cursor at (2,0)  (curswant reset to landed column 0)
Actual: cursor at (2,7)  (sticky_col preserved from before :s)
```

Callers in `hjkl-vim/src/vim/operator.rs:480,497` and
`hjkl-ex/src/builtins.rs:1293` do no post-call fixup.

---

### 3. `paste_bridge` drops named register on dot-repeat (MEDIUM)

**`crates/hjkl-vim/src/vim/bridges.rs:327-343`,
`crates/hjkl-vim-types/src/lib.rs:332-340`,
`crates/hjkl-vim/src/vim/dot_repeat.rs:117-124`**

`LastChange::Paste` has no `register` field, unlike `LastChange::LineOp` (lib.rs
lines 315-323, which carries `register: Option<char>` and restores it before
executing). When dot-repeat replays a paste (`dot_repeat.rs:117-124`), it calls
`do_paste(ed, before, scaled(count), cursor_after, reindent)` directly — which
reads `pending_register` (currently `None`, nothing set it) and falls through to
the unnamed register.

```
Repro: "ayw  (yank word into register a)
       "ap   (paste from register a — correct)
       .     (dot-repeat replays paste from UNNAMED register)
Expect: pastes from register "a  (vim: redo-register)
Actual: pastes whatever is in the unnamed register
```

`LastChange::LineOp` handles this correctly at `dot_repeat.rs:89`:
`vim_mut(ed).pending_register = register;` — `Paste` needs the same.

---

### 4. `prune_root_side` does not renumber descendant `depth` fields (LOW)

**`crates/hjkl-buffer/src/undo.rs:1155-1161`**

When `prune_root_side` promotes a child to root (`parent = None`), the promoted
child's `depth` keeps its old value (e.g. 1) instead of 0. Descendants keep
their old depths. The `depth` field comment at line 379-382 explicitly says
"assigned once at creation and never renumbered" and "a wrong depth costs speed,
never content." The keyframe ladder (`is_keyframe` at line 548:
`depth.is_multiple_of(KEYFRAME_INTERVAL)`) stays uniformly spaced, so no content
corruption — but the root-to-first-keyframe gap drifts between 1 and
`KEYFRAME_INTERVAL` instead of a fixed interval. Over many `undolevels` caps the
O(1) jump guarantee degrades.

`clear_all` at line 1209 correctly resets `depth = 0` for its survivor.

```
Repro: 100-deep linear history, cap(10) repeated
       After ~90 prunes, node at true-depth 16 from new root has depth 106
       is_keyframe(16) → false (depth=106 is not a multiple of 16)
Expect: depth renumbered to 16, keyframe at correct position
```

---

### 5. Duplicate `seq` values silently drop from `by_seq` (LOW)

**`crates/hjkl-buffer/src/undo.rs:1429-1433`**

`by_seq` is built via `.collect()` on `(seq, id)` pairs. If two nodes share a
`seq` (corrupt `SerTree`), `BTreeMap` keeps only the last one. `g-`/`g+` seq
traversal would skip the lost node silently.

Only reachable with corrupted data, and a missing node is better than a crash —
but the undofile loader could reject duplicate `seq` values instead.

```
Repro: SerTree with s.nodes[0].seq = s.nodes[1].seq = 5
       from_serializable succeeds but one node is missing from seq walk
Expect: rejected as corrupt
```

---

## Cleared

- **`do_paste` MAX_PASTE_BYTES enforcement.** Verified correct at
  `command.rs:652+:1` — budget checked before batching, batched edits
  pre-allocated, charwise/linewise/blockwise branches consistent.

- **`cut_vim_range` empty-inverse handling.** Verified correct — empty inverse
  leaves register untouched (no spurious register pollution), linewise
  normalisation correct.

- **`adjusted_number_at` hex/decimal/zero-padding/overflow.** Verified correct —
  `0x`/`0X`/`0o`/`0b` prefixes handled, zero-padding preserved, no overflow on
  `u64::MAX` increment (wraps to 0 per vim semantics).

- **`toggle_case_at_cursor` / `toggle_case_str` multi-character mappings.**
  Verified correct — uses `to_uppercase()`/`to_lowercase()` full iterators, not
  `.next()`. Fixed in `9a156885`.

- **`apply_op_with_motion` range-kind handling.** Verified correct for
  exclusive/inclusive/linewise ranges, zero-distance guards, the `dk`/`dj`
  linewise-exemption at buffer edges. Fixed in `b97e9bce`.

- **`outdent_rows` visual-column accounting.** Verified correct — uses
  `visual_col_to_char_col` for tab-aware stripping. Fixed in `9a156885`.

- **`replay_last_change` empty ReplaceMode.** Verified correct — empty
  `ReplaceMode { text: "" }` no longer calls `push_undo` or `move_left`. Fixed
  in `9a156885`.

- **`Move::Vertical` display/char column conversion.** Verified correct at
  `cursor_move.rs:86-105` — boots from current col as display, stores un-clamped
  want, converts back via `visual_col_to_char_col` before clamp. Tests cover
  tabs and wide chars.

- **`Move::Jump` / `Horizontal` / `Raw` variants.** Verified correct — Jump and
  Horizontal route through `jump_cursor` (resets sticky_col), Raw uses
  `set_cursor_quiet` (leaves it alone).

- **`\c`/`\C` override vs `/i`/`/I` flags.** Verified correct at
  `substitute.rs:270-285` — `resolve_case_mode` is called with the combined base
  mode, inline overrides win. Both directions tested.

- **`substitute.rs` empty pattern / empty replacement / empty buffer.** Verified
  correct — empty pattern reuses `last_search`, empty replacement deletes match,
  empty buffer returns zero matches and pops undo.

- **Folds:** `set_auto_folds` level computation, `open_fold_at`/`close_fold_at`,
  `reveal_row` nested-fold handling, `apply_fold_op` cursor snap — all verified
  correct.

- **Edits:** `do_delete_range` linewise, `do_replace` inverse, `do_join_lines` /
  `do_split_lines` per-join space tracking, `do_insert_block` /
  `do_delete_block_chunks` pad tracking — all verified correct.

- **Editor state transitions:** `undo_core`/`redo_core` snapshot/restore/clamp,
  `settle_after_history_jump` cursor clamp/fold reveal, `push_undo` group
  coalescing and `cap_undo` — all verified correct.

- **`Rope::is_instance` fast-path in `set_node_state`.** Verified correct at
  `undo.rs:660` — `Arc::ptr_eq` is a sufficient-but-not-necessary fast path; two
  identical-content snapshots with different `Arc` roots fall through to `==`.

- **`depth` across serialization round-trip.** Verified correct —
  `depths_from_root` at line 1454 recomputes via BFS with a `seen` guard. The
  round-trip test at line 2843 confirms deepest node depth matches.

- **Keyframe staleness.** Verified correct — keyframes pinned by `touch_warm`,
  evicted only by `KEYFRAME_CAP` (512 max), correctly removed from the keyframe
  list by `free`, `prune_root_side`, and `clear_all`.

- **Integer overflow.** No reachable overflow: `child_depth + 1` at `usize::MAX`
  is memory-bound, `budget_iters` at `live_count() + 1` same,
  `common_prefix_bytes` `n += at.len()` is capped by `max - n`.

- **Empty buffer / single-node / edge-of-history undo/redo.** All edge cases
  correctly no-op.

## Hardening

- **`sentence_boundary` / `sentence_step_forward` allocate full-buffer
  `Vec<Vec<char>>`** (`text_object.rs:70-77, 227-234`) — O(N) per `(`/`)`
  keystroke. Not a correctness bug; the function was rewritten for parity, not
  for allocation discipline.

- **`transform_block_case` / `block_replace_bounds` and `reflow_rows` allocate
  full `Vec<String>`** (`visual_ops.rs:521,652`, `text_object_ops.rs:152,186`) —
  full-buffer rebuild for small rectangular or reflow edits. Same class.

- **`prune_root_side` does not renumber descendant `depth`** — documented in
  Finding 4 above. Degrades keyframe placement over many prunes; content never
  corrupted.

- **`lowest_offpath_leaf` uses `Vec::contains` in O(N·D) loop**
  (`undo.rs:1101-1114`) — only matters with thousands of off-path branch tips in
  the cap loop.

## Coverage

Full codebase at `ba9f9a58`, tree clean. Reviewed via three read-only sub-agents
covering the highest-risk areas:

- `crates/hjkl-buffer/src/undo.rs` (full file, 3164 lines)
- `crates/hjkl-vim/src/` (all `.rs` files)
- `crates/hjkl-engine/src/` (all `.rs` files)

And direct trace-verification of the `paste_bridge`/`LastChange`/`dot_repeat`
chain, the `from_serializable` validation + `materialize`/`retarget_current`
parent-walk termination, and the substitute→`sticky_col` data flow.

**Not reviewed** (same gap as the 2026-07-29 and 2026-08-01 passes): everything
in `crates/` outside `hjkl-buffer`, `hjkl-vim`, `hjkl-vim-types`, `hjkl-engine`,
and the few files touched by call-chain tracing. That includes `hjkl-lsp`,
`hjkl-ex`, `hjkl-editor(-tui)`, `hjkl-config`, `hjkl-keymap(-tui)`,
`hjkl-layout`, `hjkl-syntax(-tui)`, `hjkl-markdown(-tui)`, `hjkl-theme(-tui)`,
`hjkl-tabs(-tui)`, `hjkl-hover(-tui)`, `hjkl-holler(-tui)`, `hjkl-form`,
`hjkl-fs`, `hjkl-fs-watch`, `hjkl-fuzzy`, `hjkl-mangler`, `hjkl-kitty`,
`hjkl-lang`, `hjkl-icons`, `hjkl-splash(-tui)`, `hjkl-info-popup(-tui)`,
`hjkl-vim-tui`, `hjkl-xdg`, `hjkl-which-key(-tui)`, `hjkl-bonsai`,
`hjkl-clipboard`, `hjkl-completion(-tui)`, `hjkl-picker(-tui)`,
`hjkl-prompt(-tui)`, `hjkl-menu(-tui)`, `hjkl-quickfix`,
`hjkl-statusline(-tui)`, `hjkl-buffer-tui`, `hjkl-engine-tui`, `hjkl-css`,
`hjkl-anvil`, `hjkl-app`, and `apps/`. All findings already in `docs/backlog.md`
were excluded per the brief.
