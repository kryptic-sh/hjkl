# hjkl — review backlog

Single source for open findings, deferred decisions, and blocked work. Findings
use symbol names rather than line numbers so references survive refactors.

## 1. Open work — ranked

### 1.1 Undo-tree step cost after keyframes

`crates/hjkl-buffer/src/undo.rs`. Keyframes reduced `:earlier 9999` at depth
1024 from 212 ms to 5.4 ms, exposing a different bottleneck:

- `node_below` / `node_above` linear-scan the whole arena on every history step.
- `retarget_current` rewrites the entire root→target path per step.
- `set_node_state`'s `unchanged` check compares full rope content on every step.

Together these impose an O(N)-per-step floor: 75 µs at depth 1024 despite ≤ 15
delta applies. A freshly deserialized undofile also has no keyframes, so its
first deep jump remains O(depth); eager construction may waste work and memory.

### 1.2 Swap `SerTree.base` duplicates the document

`crates/hjkl-app/src/swap.rs` stores the document roughly twice: once as the
streamed body and once as `SerTree.base`. A single-node tree on a 20 000-line
document serializes in ~99 µs; marginal node cost is only ~30–40 ns.

De-duplicate `base` against the streamed body. Do not implement the append-only
delta log paired with issue #302: it attacks node count, not the base copy or
`fsync`. Worst measured cell is 457 µs, so urgency is low.

### 1.3 Round-2 deferred items

| Item                           | Where                                                  | Why deferred                                                                                          |
| ------------------------------ | ------------------------------------------------------ | ----------------------------------------------------------------------------------------------------- |
| Settings/Options full collapse | `hjkl-engine/src/editor.rs`                            | L-sized; staged for 0.1.0.                                                                            |
| P6 per-cell span resolve sweep | engine span layering                                   | M–L, layering-order-sensitive; needs a sortedness guarantee first.                                    |
| P10 wrap-mode scrolloff O(h²)  | wrap scroll math                                       | Wrap is not the default; needs the same care as P6.                                                   |
| R10 stringly errors → enum     | `hjkl-app/src/git.rs` (`Result<(), String>`)           | Design decision, not mechanical.                                                                      |
| R13 `unnecessary_wraps` triage | dispatch tables                                        | Uniform signatures are deliberate; needs per-family review.                                           |
| Y5 `hjkl-editor::spec`         | `crates/hjkl-editor/src/lib.rs`                        | Needs external-consumer confirmation before deletion; workspace grep is insufficient for public APIs. |
| Multicursor `lens` vector      | `hjkl-engine/src/editor.rs` (`buf_line_chars` collect) | O(buffer) per edit, but gated behind unwired multicursor.                                             |

### 1.4 Special-buffer guards that still say `is_explorer`

| Item                            | Where                 | Effect                                                                                                                                  |
| ------------------------------- | --------------------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| Swap files for scratch docks    | `write_swap_for_slot` | Guards on `is_explorer()` only, so `:copen` and `q:` buffers get swap files and a crash can offer to “recover” a quickfix listing.      |
| `:qa` blocked by a scratch slot | `quit_all`            | Blocks on `dirty && !is_explorer()`. A dirty quickfix/cmdline scratch slot makes `:qa` refuse with unsatisfiable `E37 ... "[No Name]"`. |

### 1.5 LSP and span follow-ups

| Item                                        | Where                                       | Note                                                                                                                                                                                |
| ------------------------------------------- | ------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| LSP full-sync still copies once             | `hjkl-lsp/src/runtime.rs`, `server.rs`      | `Buffer::content_joined()` caches the `Arc`, so `Arc::unwrap_or_clone` cannot move. Avoiding the copy requires direct serialization instead of an intermediate `serde_json::Value`. |
| `attach_buffer` copies at the boundary      | `hjkl-lsp/src/manager.rs` (`attach_buffer`) | Takes `text: &str` and calls `text.to_string()`. Change the boundary ownership model.                                                                                               |
| `styled_spans` is a write-only public field | `hjkl-engine/src/editor.rs`                 | No readers. Removal wins ~27% on full installation and nothing per keystroke, but it is a public API removal on a published crate.                                                  |

### 1.6 Differential-oracle and code-review fixes

Detailed reproductions remain in `docs/code-review.md`. Preserve each fixed case
in the tier-2 compatibility corpus and verify it against nvim before changing
expectations.

| Priority | Task                                                                                                                                                                                                                                               | Where / acceptance criterion                                                                                                    |
| -------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| Highest  | Distinguish failed linewise motions from zero-distance successful motions. At a buffer edge, `dk`, `dj`, `d+`, and `d-` must leave the only line unchanged; count-1 `d_` must still delete it.                                                     | `hjkl-vim::apply_op_with_motion`; live data-loss regression introduced by `0107e2e8`.                                           |
| High     | Replace repeated paste application with one batched edit and set a memory budget that does not amplify a permitted 10 MiB paste beyond libFuzzer's 2048 MiB RSS limit. Refuse oversized pastes with an error rather than silently truncating them. | `do_paste`; `yy999999999p` currently aborts under `ulimit -v 2 GB` but succeeds under 8 GB.                                     |
| High     | Replace the no-op-delete undo-depth proxy with the actual “did this delete remove a line?” result.                                                                                                                                                 | No-op `dd` register behavior must not depend on unrelated prior undoable actions.                                               |
| High     | Fix empty `ReplaceMode` dot replay so `R<Esc>e.` does not leave `sticky_col` stale. Reject Alt/Shift-modified `.` and avoid an undo entry when replay performs no mutation.                                                                        | `crates/hjkl-vim/src/vim/dot_repeat.rs`, `normal.rs`; make regression `37433df2` pass without weakening the curswant invariant. |
| Medium   | Implement blockwise non-delete operators with block geometry, including `H`, `L`, and `gE` motion/cursor behavior.                                                                                                                                 | 21 residual cases; `<C-v>iw<` on `"\t(x).[y]"` must remain unchanged like nvim.                                                 |
| Medium   | Correct paragraph/WORD landings: `}` at EOF must reach the last character and `B` at column 0 of row 1 must remain on row 1.                                                                                                                       | Expected positions `(0,23)` and `(1,0)`.                                                                                        |
| Medium   | Triage `V}u2)1gUiW` and fix the state or cursor transition between individually-correct components.                                                                                                                                                | `"'qux'  A-B"` must become `"'QUX'  a-b"`.                                                                                      |
| Medium   | Convert `Move::Vertical`'s display-column `sticky_col` back to a character column before clamping.                                                                                                                                                 | Tabstop=4 repro must land on `(1,2)`, not `(1,3)`.                                                                              |
| Medium   | Make `outdent_rows` consume visual indentation width, including tabs.                                                                                                                                                                              | `<<` on `"\t\tfoo"` with tabstop/shiftwidth 4 must produce `"\tfoo"`.                                                           |
| Medium   | Add hexadecimal handling to visual `<C-a>`/`<C-x>`.                                                                                                                                                                                                | `Vg<C-a>` on `0xFF` must produce `0x100`.                                                                                       |
| Medium   | Make inline `\c`/`\C` override substitute `/i` and `/I` flags in execution and match collection.                                                                                                                                                   | Cover `apply_substitute` and `collect_substitute_matches`.                                                                      |
| Low      | Preserve every character emitted by Unicode upper/lowercase mappings.                                                                                                                                                                              | Cover mappings such as `ß → SS` and `İ → i\u{307}`.                                                                             |

### 1.7 Cursor-move API migration

`Move` and the debug invariant shipped; remaining phases are:

1. Migrate remaining `hjkl-engine` motions to `Editor::move_cursor`.
2. Migrate `hjkl-vim` motion, command, bridge, visual, and operator paths. Fix
   insert paths first, then visual yank, then normal operators/edits; widen the
   invariant after each class is clean.
3. Migrate `apps/hjkl` cursor writes.
4. Make raw cursor primitives crate-internal, remove public `set_sticky_col`,
   and reduce `apply_sticky_col` to the vertical clamp.
5. Report counts by `Move` variant and justify every `Move::Raw` site. Keep the
   compat oracle and PTY e2e behavior unchanged.

### 1.8 Harness, coverage, and hardening

- Clear nvim undo history after fixture seeding, then fuzz undo/redo. Extend
  cursor comparison beyond ASCII and add ex/search, fold, and `gq` coverage.
- Complete review coverage for app, LSP, editor/TUI, ex/completion,
  prompt/menu/picker, and remaining non-core crates.
- Stabilize process-global CWD/environment explorer tests and flaky PTY e2e
  cases.
- Use `checked_add` in `SnapshotFoldProvider::next_visible_row`.
- Rename undo-tree `depth` to `depth_for_keyframe`.
- Bounds-check public `rope_line_char_count` / `rope_line_bytes` helpers.
- Clamp `top_row` when another view shrinks a buffer and
  `cursor_screen_row_from` returns `None`.

## 2. Blocked on platform access

| Finding                                                      | Location                                                         | Blocker                                                                             |
| ------------------------------------------------------------ | ---------------------------------------------------------------- | ----------------------------------------------------------------------------------- |
| INCR transfer timeout signalled as completion                | `x11_thread.rs` (`prune_expired_incr_sends`), Wayland equivalent | Needs a live session; truncated transfer remains indistinguishable from completion. |
| `SELECTION_NOTIFY` refusal arm ignores selection             | `x11_thread.rs` refusal arm                                      | Needs a live X server; unrelated selection refusal can be read as ours.             |
| `CString::new(..).expect(..)` panics on NUL in a type string | `hjkl-clipboard/src/backend/macos.rs`                            | Needs a Mac.                                                                        |
| Windows FFI paths lack runtime coverage                      | `hjkl-fs/src/identity.rs`, `hjkl-fs/src/dir.rs`                  | Needs a Windows host.                                                               |

## 3. Deferred security design

### Remote grammar compilation and `dlopen` (issue #314)

`hjkl-bonsai/src/runtime/grammar.rs` and `compile.rs` download tree-sitter
grammars, compile them with `$CC`/`$CXX`, and `dlopen` them. The bundled
manifest pins `git_url`/`git_rev` but has no signature or artifact-hash
verification. This is not remotely reachable today because the manifest is
`include_str!` bundled. A signature/hash-pinning design is required before code
changes.

## 4. Process reference

### Gates

Per item: run workspace clippy with warnings denied, format, full nextest
(including e2e when app code changes), and the nvim compatibility oracle. Never
edit the oracle corpus to make a change pass. Performance items require measured
before/after results.

### Platform lint coverage

Linux-only lint runs do not cover platform-gated code. `hjkl-fs`,
`hjkl-clipboard`, and `hjkl-lsp` can be cross-linted for macOS and Windows;
crates pulling tree-sitter, mimalloc, or aws-lc-sys require CI runners.

### Benchmarks

| Bench                               | Measures                                               |
| ----------------------------------- | ------------------------------------------------------ |
| `hjkl-buffer-tui/benches/render.rs` | viewport render, short vs long lines                   |
| `hjkl-buffer/benches/undo.rs`       | cold `g-` jump cost vs undo depth                      |
| `hjkl-buffer/benches/budgets.rs`    | per-operation budget guards                            |
| `hjkl-app/benches/swap.rs`          | full swap write + undo serialization vs size and depth |

### Standing traps

- Preserve exact char, byte, and grapheme units in rope math.
- Read an `Edit`'s semantics before applying its inverse through `apply_edit`.
- `hjkl_driver` cannot replay `:` keys, and `cargo test -p hjkl` skips the e2e
  binary.
- Workspace grep cannot prove a published crate or public API has no external
  consumers.
