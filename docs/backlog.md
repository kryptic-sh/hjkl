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

Detailed reproductions are preserved in the supporting-evidence appendix below.
Preserve each fixed case in the tier-2 compatibility corpus and verify it
against nvim before changing expectations.

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

## 5. Supporting evidence

These appendices preserve the reproductions, design constraints, and audit
method needed to complete the open work above.

### Differential audit against neovim (2026-07-28, revised 2026-07-29)

#### Method

Randomised differential fuzzer over the existing oracle infrastructure: generate
a random ASCII buffer and a random normal-mode keystroke sequence, replay both
through `hjkl_driver::run_case` and `nvim_driver::run_case`, diff buffer /
cursor / mode / default register. Divergences are greedily shrunk (drop one
keystroke token at a time, then one buffer line at a time, while the divergence
survives) and printed as paste-ready corpus TOML.

- `crates/hjkl-compat-oracle/examples/difffuzz.rs` — the fuzzer.
- `crates/hjkl-compat-oracle/examples/dfcase.rs` — replay one ad-hoc case
  through both engines, for narrowing a shrunk case by hand.

```
cargo build -p hjkl-compat-oracle --release --examples   # BOTH, see note
cargo run -p hjkl-compat-oracle --release --example difffuzz -- 400 777
cargo run -p hjkl-compat-oracle --release --example dfcase -- '<buf>' <row> <col> '<keys>'
```

Build with `--examples`, not `--example dfcase`. Rebuilding only one leaves the
other stale, and a stale `difffuzz` silently reports the previous commit's
divergences — byte-identical results across a run are the tell.

Both drivers pin `shiftwidth=4`, `expandtab`, `noautoindent`,
`foldmethod=manual`, so a divergence means an engine defect rather than config
skew between hjkl and `nvim --clean`.

Every entry below was verified by hand through `dfcase`; the fuzzer only located
them.

#### Evidence for differential-audit backlog items

##### R4. `dk` / `dj` / `d+` / `d-` destroy the line at a buffer edge

Introduced by `0107e2e8`, which fixed R1 by exempting linewise motions from the
`start == end` guard in `apply_op_with_motion`:

```rust
if start == end && !matches!(kind, RangeKind::Linewise) {
    return;
}
```

The exemption is too broad. It is correct for `_`, whose count-1 form covers the
current row by definition, but `j` / `k` / `+` / `-` are also linewise and must
**fail** when there is no row to move to. All four now delete:

```
`dk` (also `dj`, `d+`, `d-`) on "only line", cursor (0,3)
hjkl: ""              ← line destroyed
nvim: "only line"     ← unchanged
```

This regresses the earlier one-line `dk` edge fix from `b4458135`. The fuzzer
independently surfaced `dk` as a new divergence in the latest pass. Multi-line
`dk` (row 2 of 3) is still correct.

This distinguishes failed motions from zero-distance successful motions: `_`
does not fail at count 1; `k` at row 0 does. The action is tracked in the ranked
backlog above.

##### 11. Unbounded memory on large paste counts — improved, not closed

`62bd4853` replaced the ineffective count cap with a 10 MiB byte budget in
`do_paste`. The payload is now bounded, but applying it still peaks far above
the budget:

| case                                        | `ulimit -v 2 GB` |
| ------------------------------------------- | ---------------- |
| `yy999999999p`, 10-byte register            | abort (134)      |
| `yy999999999p`, 2000-byte register          | abort (134)      |
| `yy5000p`, 10-byte register (50 KB payload) | ok               |

Same input succeeds at `ulimit -v 8 GB`. The cost tracks total payload bytes,
not iteration count — ~5200 iterations of a 2000-byte register (10 MiB, the
clamp ceiling) aborts, while 5000 iterations of a 10-byte register (50 KB) does
not. So a paste sitting exactly at the permitted ceiling needs >2 GB peak RSS,
roughly 200× amplification.

The budget is therefore too generous relative to per-byte overhead. Remediation
is tracked in the ranked backlog above.

The weekly cron fuzz job runs with libFuzzer's default 2048 MB rss limit, so
this remains reachable there.

##### 5 (residual). Blockwise visual — non-delete operators

`ba813ca0` fixed blockwise + text object (`<C-v>iwd`, `<C-v>i(d` now match). 21
blockwise divergences remain, unchanged across the last two passes, concentrated
on the indent operators and on `H` / `L` / `gE` motions:

```
`<C-v>iw<` on "\t(x).[y]", cursor (0,6)
hjkl: "(x).[y]"     ← outdented the whole line
nvim: "\t(x).[y]"   ← unchanged
```

`<C-v>H>` also still diverges on cursor. Blockwise `~` is correct.

##### 8 (residual). Two motion landings

| case                                            | hjkl   | nvim   |
| ----------------------------------------------- | ------ | ------ |
| `}` at EOF, buffer `"    it's.{foo}.A-B.{A-B}"` | (0,21) | (0,23) |
| `B` at col 0 of row 1                           | (0,0)  | (1,0)  |

`}` no longer jumps to (0,0) but still does not reach the last character. `B`
still overshoots to the previous line.

##### T1. Composite case-op sequence — unconfirmed cause

```
`V}u2)1gUiW` on "'qux'  A-B", cursor (0,9)
hjkl: "'qux'  A-B"   (unchanged)
nvim: "'QUX'  a-b"
```

New in the latest pass. Every component passes in isolation (`Vu`, `V}u`, `gUiW`
all match), so the likely culprit is cursor placement after `2)` rather than the
case operators — unconfirmed.

#### Watch items

**W1. `7e260e27` moves the cursor with `buf_set_cursor_rc`.** The landing
position is correct, but per the cursor-move design appendix below that
primitive does not maintain curswant — the exact class of latent bug that
document exists to prevent. A following `j` may snap to a stale column.

**W2. `79e024d5` distinguishes a no-op delete by undo-stack depth.** It reads
`ed.undo_stack_len() > 1` as "the buffer was modified by a prior operation",
which is a proxy for the real rule rather than the rule itself. It makes the
oracle cases pass, but the register outcome of a no-op `dd` now depends on
whether _any_ prior undoable action occurred in the session, which is not what
vim keys off.

**W3. `62bd4853` silently truncates oversized pastes.** A user asking for N
copies of a large register gets fewer, with no message. Silent partial execution
is arguably worse than refusing; remediation is tracked in the ranked backlog
above.

#### Not covered by this pass

- **Non-ASCII.** `HjklOutcome.cursor` is a char index and `NvimOutcome.cursor`
  is a byte index, so the comparison is only sound on ASCII. This is exactly
  where the char-vs-grapheme column trap lives, and it is unaudited.
- **Ex commands** (`:`), search prompts (`/`, `?`) — the in-process hjkl driver
  cannot replay them.
- **Undo / redo** — nvim fixture seeding over RPC creates an undoable change, so
  `u` rolls back the fixture rather than the generated operation.
- **Folds** (`z`) and `gq` — excluded to avoid config-skew noise.
- The app / window layer, LSP, and everything above the engine.

### Code review — full-codebase (2026-07-29)

Tree clean, v0.39.0 + 13 commits. Reviewed via three read-only `explore`
sub-agents (hjkl-vim, hjkl-engine, hjkl-buffer) plus direct review of the recent
diff, curswant invariant, rope_util, and Move API. Every cited file:line was
re-read and every failure scenario traced end-to-end by hand.

#### Evidence for code-review backlog items

##### 1. `Move::Vertical` mixes display-column `sticky_col` with char-column cursor math

`crates/hjkl-engine/src/cursor_move.rs:86-98` — in production via `scroll_line`
(`crates/hjkl-engine/src/editor.rs:3122`):

`jump_cursor` stores `sticky_col` as a display column (line 2386:
`char_col_to_visual_col`). `Move::Vertical` reads it and uses it directly as a
char column to clamp against `max_col` (`buf_line_chars(...) - 1`, a char
count). On tab-indented lines, display col ≠ char col, so the cursor lands on
the wrong character.

`apply_sticky_col` in the vim motion path (`motion.rs:383`) correctly converts
back via `visual_col_to_char_col`; the `Move` API path does not, because it used
`want.min(max_col)` without the conversion.

```
Repro: tabstop=4, buffer "\tabcdef\n\txyz", cursor (0,2)='b'
        sticky_col = visual col 5 (from jump_cursor)
        <C-e> pushes cursor to row 1 via scroll_line
        → move_cursor(Move::Vertical { row: 1 })
        → want=5 (display), max_col=3 (chars-1), want.min(3)=3
        → cursor (1,3)='z'
Expect: cursor (1,2)='y' (visual col 5 on a tab+line = char col 2)
```

Remediation is tracked in the ranked backlog above.

##### 2. `outdent_rows` strips by character count, not visual column width

`crates/hjkl-vim/src/vim/text_object_ops.rs:403-407`:

`width` is computed in visual columns (`shiftwidth * count`), but
`line.chars().take(width)` limits by CHARACTER count. Tabs consume 1 char but
represent `tabstop` visual columns, so lines with tabs are over-stripped.

```
Repro: << on "\t\tfoo" (tabstop=4, shiftwidth=4, noexpandtab)
        width=4 (visual cols), line.chars().take(4)=['\t','\t','f','o']
        take_while(is_whitespace) → strip=2 → both tabs removed → "foo"
Expect: "\tfoo" (vim strips 4 visual cols = 1 tab)
```

Remediation is tracked in the ranked backlog above.

##### 3. `adjust_number_visual` ignores hex literals

`crates/hjkl-vim/src/vim/command.rs:1122-1123`:

Visual-mode `<C-a>`/`<C-x>` (`g<C-a>`, `g<C-x>`) scans for `is_ascii_digit()`
only — the `0` in `0xFF` matches, making it treat the hex prefix as a decimal
`0`. Normal-mode `adjust_number` (line 284) checks `is_hex_prefix(i)` first.

```
Repro: Vg<C-a> on "0xFF", cursor row 0
        → finds digit '0' at col 0, span_end stops at 'x'
        → s="0", n=0, replaces "0" with "1" → "1xFF"
Expect: hex increment → "0x100"
```

Remediation is tracked in the ranked backlog above.

##### 4. `:s` `/i` and `/I` flags ignore inline `\c`/`\C` overrides

`crates/hjkl-engine/src/substitute.rs:273-283`:

The `/i` and `/I` paths pass `CaseMode::Sensitive` as a dummy base to
`resolve_case_mode` and discard the returned mode (`(stripped, _)`), then
force-sensitise or wrap with `(?i)` unconditionally. The comment on line 272
says "matching vim's documented precedence: flag > inline override", but vim's
actual precedence is the reverse: inline `\c`/`\C` wins over the `/i`/`/I` flag.
(`:help /ignorecase`: `\c` overrides `'ignorecase'`, and `/I`/`/i` map to the
same toggle.)

```
Repro: :s/\cFOO/bar/I
        → \c stripped, /I path returns pattern as-is → case-sensitive
Expect: \c forces case-insensitive despite the I flag

Repro: :s/\CFOO/bar/i
        → \C stripped, /i wraps with (?i) → case-insensitive
Expect: \C forces case-sensitive despite the i flag
```

The same bug exists in `collect_substitute_matches` (lines 442-458). Remediation
is tracked in the ranked backlog above.

##### 5. `toggle_case_str` discards multi-character case mappings

`crates/hjkl-vim/src/vim/text_object_ops.rs:597-609` and
`crates/hjkl-vim/src/vim/command.rs:407-411`:

`to_uppercase()` and `to_lowercase()` return iterators that may yield multiple
chars (e.g. `ß` → `SS`, `İ` → `i\u{307}`). The code uses `.next().unwrap_or(c)`,
silently dropping all but the first output.

```
Repro: g~ on "Straße", cursor over 'ß'
        ß.to_uppercase() → ['S','S'], .next() → 'S'
        → "StraSe" (one character lost)
Expect: "STRASSE" (or "STRAẞE")
```

Minor — affects users whose text contains precomposed characters with multi-char
case mappings.

#### Hardening evidence

- **`SnapshotFoldProvider::next_visible_row` uses unbounded `+= 1`**
  (`buffer_impl.rs:608`): `r += 1` in while loop without `checked_add`.
  `prev_visible_row` uses `checked_sub`. Risk only at `last == usize::MAX`,
  unreachable with realistic buffers. Remediation is tracked in the ranked
  backlog above.

- **`Move::Vertical` bootstrap path** (`cursor_move.rs:89`): when `sticky_col`
  is `None`, the bootstrap uses `self.cursor().1` (char column) as `want`, but
  `want` is later compared against `max_col` (char count, ok) and used directly
  as cursor column (ok for the bootstrap case). The real bug is when
  `sticky_col` IS `Some` (display column) — see finding 1.

- **`prune_root_side` depth inconsistency with `clear_all`** (`undo.rs`):
  `clear_all` resets survivor depth to 0 (line 1105); `prune_root_side` does not
  (line 1032-1068). The naming remediation is tracked in the ranked backlog
  above.

- **`rope_line_char_count` / `rope_line_bytes` OOB panic** (`buffer.rs:806-815`,
  `794-803`): public functions without bounds checks. Current callers clamp the
  row; API hardening is tracked in the ranked backlog above.

- **`ensure_cursor_visible` top_row stale on shrink from other view**
  (`buffer.rs:189-191`): when `cursor_screen_row_from` returns `None`, only
  `top_col` is zeroed; `top_row` stays where it was, potentially leaving cursor
  invisible. Remediation is tracked in the ranked backlog above.

#### Remaining review coverage

- `apps/hjkl/` — app layer, PTY harness, e2e tests.
- `crates/hjkl-lsp/` — LSP runtime, codec, manager.
- `crates/hjkl-editor/`, `crates/hjkl-editor-tui/`, and sibling TUI crates.
- `crates/hjkl-ex/`, `crates/hjkl-completion/` — ex commands, completion.
- `crates/hjkl-prompt/`, `crates/hjkl-menu/`, `crates/hjkl-picker/`.
- All remaining TUI and non-core crates.

### Code review — pending changes (2026-07-29)

**Scope:** pending unstaged change: a new proptest regression entry in
`crates/hjkl-vim-tui/tests/proptest_fsm.proptest-regressions` (+1 line, hash
`37433df2`). The regression was discovered by the `esc_returns_to_normal`
property test and also triggers in `no_panic_on_random_keys`.

**Method:** Traced each failing input from `handle_key` through
`crossterm_to_input`, `dispatch_input`, `step_insert`/`step_normal`,
`replay_last_change`, `finish_insert_session`, and the curswant invariant check.
Verified the same code-path reproduction with the exact shrunk sequences.

#### Evidence for pending-change backlog item

`crates/hjkl-vim/src/vim/dot_repeat.rs:205–226`

Dot-replay of an empty `ReplaceMode` session (user typed `R<Esc>` then `.`)
calls `push_undo()` (no buffer-content change → `dirty_gen` unchanged) and then
`move_left` (cursor moves but `sticky_col` is left stale). The debug-only
curswant invariant (`crates/hjkl-vim/src/curswant.rs:181`) catches this and
panics. In release builds the bug is silent but leaves `sticky_col` wrong,
causing `j`/`k` to snap to the pre-dot-repeat column instead of the current one.

The sequence `R` → `Esc` → `e` → `.` on `"hello world\nsecond line\n"`:

- `R` enters replace mode (`VimMode::Insert`)
- `Esc` exits without typing → `finish_insert_session` sets
  `last_change = ReplaceMode { text: "" }`, `sticky_col = Some(0)`
- `e` (word-end motion) moves cursor to (0,4), sets `sticky_col = Some(4)`
- `.` (dot-repeat) enters `replay_last_change`:
  - `push_undo()` — no dirty_gen change
  - `for ch in "".chars()` — loop body never executes, no dirty_gen change
  - `cursor.1 > 0` (4 > 0) → `move_left(buf, 1)` — cursor moves to (0,3)
  - `sticky_col` remains `Some(4)` ← **stale**

Curswant check fires: cursor moved from (0,4) to (0,3), dirty_gen unchanged,
mode unchanged, but `sticky_col == Some(4)` while `display_col == 3` — not a
vertical clamp (line has 5 chars, col 3 < 4).

```
Repro: replay_last_change with last_change = ReplaceMode { text: "" },
       cursor at (0, 4), sticky_col = Some(4)
Expect: sticky_col = Some(3) (or cursor unchanged)
Actual: sticky_col = Some(4), cursor at (0, 3)
       → debug-only panic in curswant::assert_invariant
       → release: stale sticky_col, next j/k snaps to column 4
```

The same bug also manifests when modifiers are present on the `.` key
(`KeyModifiers::ALT` or `KeyModifiers::SHIFT`) because the dot-repeat gate in
`step_normal` (`crates/hjkl-vim/src/normal.rs:463`) only checks `!input.ctrl`
and `input.key == Key::Char('.')` — it does not reject `alt` or `shift`.

#### Hardening evidence

- **Dot-repeat gate doesn't filter `alt`/`shift`** — `step_normal` line 463
  checks only `!input.ctrl && input.key == Key::Char('.')`. Real vim does not
  trigger dot-repeat on `Alt-.` or `Shift-.`. This divergence increases the
  input surface hitting this bug but is not itself a correctness defect.

- **Wasteful `push_undo()` on empty ReplaceMode replay** — `dot_repeat.rs:207`
  pushes an undo entry for a replay that performs zero buffer mutations. This is
  harmless but creates pointless undo-tree entries.

Both actions are tracked in the ranked backlog above.

### Cursor moves carry their own curswant semantics (2026-07-27)

#### Why

Moving the cursor and maintaining `sticky_col` (vim's `curswant`) are two
separate actions today, and nothing forces the second:

| primitive           | non-test sites | maintains curswant? |
| ------------------- | -------------- | ------------------- |
| `buf_set_cursor_rc` | 106            | **no**              |
| `View::set_cursor`  | 80             | **no** (documented) |
| `jump_cursor`       | 107            | yes (`= col`)       |
| `set_sticky_col`    | 33             | manual follow-up    |
| `apply_sticky_col`  | 1              | the vim catch-all   |

That is ~186 cursor moves which do not maintain curswant, each a potential
instance of the bug fixed in `c022a3a4`: `/pattern<CR>` moved the cursor without
resetting curswant, so the next `j` snapped back to the pre-search column.

The rule was never unknown — it is written correctly in two places already
(`apply_sticky_col`: "Everything else — search, gg/G, word jumps — lands at the
match's own column"; `jump_cursor`: "every explicit jump … search hit, click").
It was **unenforced**. Four call sites reached the search advance and only the
one routed through the vim motion dispatch was right.

#### Target shape

The selected design makes curswant semantics part of every cursor move:

```rust
pub enum Move {
    Vertical   { row: usize },              // j/k: READS curswant, clamps, leaves it
    Jump       { row: usize, col: usize },  // search/gg/G/marks/click: SETS curswant
    Horizontal { col: usize },              // h/l/w/b/$/^: SETS curswant
    Raw        { row: usize, col: usize },  // must NOT disturb curswant
}

impl Editor { pub fn move_cursor(&mut self, m: Move); }
```

The reserved end state for raw primitives is crate-internal use, keeping
`buf_set_cursor_rc` and `View::set_cursor` unreachable from vim/app code. This
explains why every migrated motion requires an explicit variant; unfinished work
is tracked in the ranked backlog above.

`Raw` is deliberately conspicuous. Today the forgetful option is the one that
looks like the default; naming it inverts that, and it stays greppable for
review.

#### Phase-0 safety net

The unfinished migration and its acceptance criteria are tracked in the ranked
backlog above. Phase 0 shipped the debug invariant that guards that work: a site
classified into the wrong variant trips the assertion instead of shipping a
silent behavior change.

#### What phase 0 found

The unconditional assertion the plan called for is **not enableable**: it
produces 236 violations across 158 tests. The shipped check is therefore scoped
to plain motions — no chord/count in flight, `dirty_gen` unchanged, mode
unchanged across the key, no search prompt open. Every guard is structural, not
a suppression list, so a motion added later is covered the day it is written.

Two design calls made during implementation, both kept:

- **State-based, not key-based.** Classifying the key is unsound: `j` is
  vertical bare, a target in `dj`/`fj`/`rj`, and a literal in Insert. The check
  instead tests the states the rules can produce — after a move `sticky_col`
  must be `None`, equal to the landed column, or greater with the cursor clamped
  to the row end.
- **Fires on the transition, not the state.** It triggers only when a keystroke
  goes from a legal pair to an illegal one, so stale state from an earlier key
  is not blamed on the next movement.

Observed phase-0 violation classes still relevant to migration:

| Class | Count | What                                                                                                                                                           |
| ----- | ----- | -------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A     | 170   | Insert mode: every printable char, `Tab`, `<C-w>`. One systemic site.                                                                                          |
| B     | 66    | Normal-mode operators/edits: `y{motion}`, `d{motion}`, `J`, `p`/`P`, `x`/`X`, `~`, `>`, `.`, `u`, `<C-a>`. The operator path never reaches `apply_sticky_col`. |
| C     | 12    | Visual-mode `y` — invisible to a `dirty_gen` check since yank makes no edit.                                                                                   |

#### The real cost

The ~186 sites expose the migration's classification cost. A mechanical
translation to `Raw` would compile while preserving the bug class. Variant-count
reporting and raw-move justification are tracked in the ranked backlog above.

#### Invariants established by phases 0–1

- The compat oracle remained ALL-pass; its corpus was not edited to make either
  phase pass. It covers search/curswant directly with five expectations taken
  from headless nvim.
- The pty e2e suite served as the cursor-behavior safety net.
- Phase 0 was the only phase allowed to change behavior; later assertion changes
  indicate a migration misclassification.
