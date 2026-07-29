# Differential audit against neovim (2026-07-28, revised 2026-07-29)

## Method

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

## Status

Seed 777, 400 cases, measured after each pass:

| pass                          | divergences |
| ----------------------------- | ----------- |
| original audit                | 114         |
| after the first fix pass      | 99          |
| after the regression fix pass | **91**      |

Of the 91: ~9 are known harness noise, 21 are the residual blockwise cluster
(finding 5), and the rest are the long tail below. Gate state:
`cargo clippy --all-targets -D warnings` clean, `cargo test --workspace` 5595
passed, `hjkl-compat-oracle` 81 passed across 4 suites.

Resolved and pruned from this document:

| Finding                                     | Fixed by                            |
| ------------------------------------------- | ----------------------------------- |
| 1 — linewise case-op corruption             | `05418277`, `19638f39`              |
| 2 — `dk` one-line, `J` on the last line     | `b4458135`, `62bd4853` (but see R4) |
| 3 — `$` ignores its count                   | `2e5b484b`                          |
| 4 — `D` / `C` drop their count              | `2e5b484b`, `46561b22`              |
| 6 — `j`/`k` lose the display column         | `37b62b73`, `b949484c`              |
| 7 — `+` / `-` / `_` clamp at edges          | `b4458135`                          |
| 8 — `b` in leading ws, `3w`, `W`            | `f808513e`                          |
| 9 — register newline placement              | `d0ee6cca`, `79e024d5`              |
| 10 — `VgU` cursor column                    | `7e260e27` (see W1)                 |
| R1 — `d_` no-op                             | `0107e2e8` (but see R4)             |
| R2 — no-op delete clobbers the register     | `79e024d5` (see W2)                 |
| R3 — whole-buffer case op adds a blank line | `19638f39`                          |

The case-op family cleared 10 fuzz cases in the last pass (`guu`, `gUap`, `g~G`,
`VjU`, `V1bU`, `V3toU`, `V}uyG`, `gE`, `==1dL`, `IZ>H`).

## Open

### R4. `dk` / `dj` / `d+` / `d-` destroy the line at a buffer edge

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

This re-breaks the half of finding 2 that `b4458135` had fixed. The fuzzer
independently surfaced `dk` as a new divergence in the latest pass. Multi-line
`dk` (row 2 of 3) is still correct.

The guard needs to key off whether the _motion failed_, not off the cursor delta
— `_` does not fail at count 1; `k` at row 0 does.

### 11. Unbounded memory on large paste counts — improved, not closed

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

The budget is therefore too generous relative to per-byte overhead. Either drop
it substantially or build the payload once and apply it as a single edit rather
than looping `count` times.

The weekly cron fuzz job runs with libFuzzer's default 2048 MB rss limit, so
this remains reachable there.

### 5 (residual). Blockwise visual — non-delete operators

`ba813ca0` fixed blockwise + text object (`<C-v>iwd`, `<C-v>i(d` now match). 21
blockwise divergences remain, unchanged across the last two passes, concentrated
on the indent operators and on `H` / `L` / `gE` motions:

```
`<C-v>iw<` on "\t(x).[y]", cursor (0,6)
hjkl: "(x).[y]"     ← outdented the whole line
nvim: "\t(x).[y]"   ← unchanged
```

`<C-v>H>` also still diverges on cursor. Blockwise `~` is correct.

### 8 (residual). Two motion landings

| case                                            | hjkl   | nvim   |
| ----------------------------------------------- | ------ | ------ |
| `}` at EOF, buffer `"    it's.{foo}.A-B.{A-B}"` | (0,21) | (0,23) |
| `B` at col 0 of row 1                           | (0,0)  | (1,0)  |

`}` no longer jumps to (0,0) but still does not reach the last character. `B`
still overshoots to the previous line.

### T1. Composite case-op sequence — needs triage

```
`V}u2)1gUiW` on "'qux'  A-B", cursor (0,9)
hjkl: "'qux'  A-B"   (unchanged)
nvim: "'QUX'  a-b"
```

New in the latest pass. Every component passes in isolation (`Vu`, `V}u`, `gUiW`
all match), so the likely culprit is cursor placement after `2)` rather than the
case operators — unconfirmed.

## Watch items

**W1. `7e260e27` moves the cursor with `buf_set_cursor_rc`.** The landing
position is correct, but per `docs/cursor-moves.md` that primitive does not
maintain curswant — the exact class of latent bug that document exists to
prevent. A following `j` may snap to a stale column.

**W2. `79e024d5` distinguishes a no-op delete by undo-stack depth.** It reads
`ed.undo_stack_len() > 1` as "the buffer was modified by a prior operation",
which is a proxy for the real rule rather than the rule itself. It makes the
oracle cases pass, but the register outcome of a no-op `dd` now depends on
whether _any_ prior undoable action occurred in the session, which is not what
vim keys off.

**W3. `62bd4853` silently truncates oversized pastes.** A user asking for N
copies of a large register gets fewer, with no message. Silent partial execution
is arguably worse than refusing; consider surfacing it the way vim reports
`E1240`-style limits.

## Verified — not defects

Checked and deliberately excluded, so they are not re-reported next time:

- **`s` / `S` do not substitute.** They are bound to vim-sneak when
  `settings().motion_sneak` is on (`crates/hjkl-vim/src/normal.rs`), which is
  the default. Intentional divergence.
- **`Y` yanks to end-of-line, not the whole line.** Matches nvim 0.12, which
  maps `Y` to `y$`. hjkl is right; traditional vim is the odd one out.
- **All `u` / `<C-r>` divergences were a harness artifact.** The nvim driver
  seeds the buffer over RPC, which is itself an undoable change, so `u` rolls
  back the fixture. Not an engine defect — but it does mean undo/redo is
  currently invisible to this fuzzer.
- **`==` reindents where `nvim --clean` does not.** hjkl ships a real formatter;
  stock nvim has no `indentexpr` for plain text. Design choice.
- **`app::explorer::tests::non_git_dd_vanishes` failing under
  `cargo test --workspace`.** Pre-existing flake, not a regression: it passes in
  isolation and across repeated app-suite runs. It mutates process-global CWD
  and env, so it races under workspace-wide parallelism.

## Not covered by this pass

- **Non-ASCII.** `HjklOutcome.cursor` is a char index and `NvimOutcome.cursor`
  is a byte index, so the comparison is only sound on ASCII. This is exactly
  where the char-vs-grapheme column trap lives, and it is unaudited.
- **Ex commands** (`:`), search prompts (`/`, `?`) — the in-process hjkl driver
  cannot replay them.
- **Undo / redo** — masked by the harness artifact above.
- **Folds** (`z`) and `gq` — excluded to avoid config-skew noise.
- The app / window layer, LSP, and everything above the engine.

# Code review — full-codebase (2026-07-29)

Tree clean, v0.39.0 + 13 commits. Reviewed via three read-only `explore`
sub-agents (hjkl-vim, hjkl-engine, hjkl-buffer) plus direct review of the recent
diff, curswant invariant, rope_util, and Move API. Every cited file:line was
re-read and every failure scenario traced end-to-end by hand.

## Findings

### 1. `Move::Vertical` mixes display-column `sticky_col` with char-column cursor math

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

Fix: convert `want` from display to char column before clamping, matching what
`apply_sticky_col` does.

### 2. `outdent_rows` strips by character count, not visual column width

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

Fix: iterate chars, accumulating visual width via the existing `indent_width`
helper (`command.rs:572`), stop when accumulated width reaches `width`.

### 3. `adjust_number_visual` ignores hex literals

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

Fix: replicate the `is_hex_prefix` check and hex-increment logic from
`adjust_number` into `adjust_number_visual`.

### 4. `:s` `/i` and `/I` flags ignore inline `\c`/`\C` overrides

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

Same bug in `collect_substitute_matches` (lines 442-458). Fix: pass the
flag-resolved `CaseMode` as `base` to `resolve_case_mode` so inline overrides
win, then apply the result.

### 5. `toggle_case_str` discards multi-character case mappings

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

## Cleared

- **`content_row_count` false-positive** (sub-agent #2): ropey always produces
  exactly one phantom trailing empty line. `"foo\n\n"` → 3 ropey lines (real
  "foo", real empty, phantom empty). Stripping only the last is correct; the
  second empty line is a genuine user line. Verified with `"foo"`, `"foo\n"`,
  `"foo\n\n"`, `"foo\n\n\n"`, `"foo\nbar"`, `"foo\n\nbar"`.

- **`to_uppercase`/`to_lowercase` next().unwrap_or(c) pattern in `command.rs`**
  — same as finding 5, confirmed as minor-only.

- **`read_vim_range` exclusive-row blank segments** — `lo < hi` guard prevents
  empty pushes; `row < bot.0` newline push is correct. Safe.

- **`cut_vim_range` inclusive wrap to next row at buffer edge** — unreachable in
  practice; safeguard exists but never triggers. Safe.

- **`reflow_keep_cursor` blank-line char offset** — traced a concrete 4-line
  example with blank middle line; +1 separator accumulation cancels with -1
  scanning. Safe.

- **`change_linewise_rows` single-line `cc`** — `end_row > top_row` false → only
  content deleted, indent preserved. Safe.

- **`replace_char` count guard** — `>` (not `>=`), so exact fit is allowed.
  Safe.

- **`do_char_delete` empty-line guard** — `break` not `continue`, prevents
  infinite spin. Safe.

- **`indent_rows` empty-line skip** — whitespace-only lines are NOT empty, so
  they get indented = matches vim. Safe.

- **`word_at_cursor_search` punctuation-only line** — empty-vec guard returns
  early. Safe.

- **`bracket_net` single-quote lifetime heuristic** — 5-char lookahead is
  bounded; 6+ char literals don't exist in practice. Safe.

- **`do_block_paste` width-padding gated on `tail`** — empty `tail` → no
  trailing padding, matches nvim v0.12.4. Safe.

- **`do_insert_block` double-lock pattern** — safe on current single-threaded
  architecture. Hardening note only.

- **`rope_line_char_count` public OOB panic** — all current callers clamp row
  first. Hardening note only.

- **`prune_root_side` stale `depth`** — documented as cache-only field.
  Hardening note only.

- **`ensure_cursor_visible` stale `top_row` on `cursor_screen_row_from` None** —
  edge case with multi-view shrink; very low risk.

- **14 additional items from sub-agent #3 (buffer crate)** — all verified safe
  by the sub-agent and spot-checked.

## Hardening

- **`SnapshotFoldProvider::next_visible_row` unbounded `+= 1`**
  (`buffer_impl.rs:608`): `r += 1` in while loop without `checked_add`.
  `prev_visible_row` uses `checked_sub`. Risk only at `last == usize::MAX`,
  unreachable with realistic buffers. Use `checked_add` for consistency.

- **`Move::Vertical` bootstrap path** (`cursor_move.rs:89`): when `sticky_col`
  is `None`, the bootstrap uses `self.cursor().1` (char column) as `want`, but
  `want` is later compared against `max_col` (char count, ok) and used directly
  as cursor column (ok for the bootstrap case). The real bug is when
  `sticky_col` IS `Some` (display column) — see finding 1.

- **`prune_root_side` depth inconsistency with `clear_all`** (`undo.rs`):
  `clear_all` resets survivor depth to 0 (line 1105); `prune_root_side` does not
  (line 1032-1068). Rename `depth` to `depth_for_keyframe` to make the
  cache-only contract explicit.

- **`rope_line_char_count` / `rope_line_bytes` OOB panic** (`buffer.rs:806-815`,
  `794-803`): public functions without bounds checks. Clamp row internally,
  matching `pos_to_char_idx` which does.

- **`ensure_cursor_visible` top_row stale on shrink from other view**
  (`buffer.rs:189-191`): when `cursor_screen_row_from` returns `None`, only
  `top_col` is zeroed; `top_row` stays where it was, potentially leaving cursor
  invisible. Clamp `top_row` to `last_content_row()`.

## Coverage

Reviewed:

- Recent diff (v0.39.0..HEAD): 13 commits, all changed files read in full.
- `crates/hjkl-vim/src/vim/command.rs` — full file (1245 lines).
- `crates/hjkl-vim/src/vim/text_object_ops.rs` — full file (626 lines).
- `crates/hjkl-vim/src/vim/motion.rs` — `apply_sticky_col`,
  `is_vertical_motion`.
- `crates/hjkl-vim/src/curswant.rs` — full file (189 lines).
- `crates/hjkl-engine/src/cursor_move.rs` — full file (221 lines).
- `crates/hjkl-engine/src/rope_util.rs` — full file (141 lines).
- `crates/hjkl-engine/src/editor.rs` — `search_advance`, `scroll_line`,
  `jump_cursor`, `sync_sticky_col_to_cursor`.
- `crates/hjkl-engine/src/motions.rs` — `content_row_count`, `move_bottom`.
- `crates/hjkl-engine/src/substitute.rs` — `apply_substitute` case-mode branch.
- `crates/hjkl-engine/src/types.rs` — `Options` defaults diff.
- `crates/hjkl-buffer/src/engine_types.rs` — full file.
- `crates/hjkl-buffer/src/buffer.rs` — `View` impl, first 200 lines.

Sub-agent coverage (read-only, changes nothing):

- Sub-agent #1 (hjkl-vim): `motion.rs`, `command.rs`, `text_object_ops.rs`,
  `normal.rs`, `visual.rs`, `state.rs`, `count.rs`, `linewise.rs`.
- Sub-agent #2 (hjkl-engine): `motions.rs`, `buffer_impl.rs`, `substitute.rs`,
  `search.rs`, `editor.rs`, `types.rs`.
- Sub-agent #3 (hjkl-buffer): `edit.rs`, `undo.rs`, `buffer.rs`, `geom.rs`,
  `lib.rs`.

Not reviewed:

- `apps/hjkl/` — app layer, PTY harness, e2e tests. GAP.
- `crates/hjkl-lsp/` — LSP runtime, codec, manager. GAP.
- `crates/hjkl-editor/`, `crates/hjkl-editor-tui/` and sibling TUI crates. GAP.
- `crates/hjkl-ex/`, `crates/hjkl-completion/` — ex commands, completion. GAP.
- `crates/hjkl-prompt/`, `crates/hjkl-menu/`, `crates/hjkl-picker/` —
  prompt/menu/picker logic. GAP.
- All remaining TUI and non-core crates. GAP.

## Gate

`cargo fmt --all --check` ✓,
`cargo clippy --all-targets --all-features -- -D warnings` ✓,
`cargo build --workspace --examples --all-features` ✓.
`cargo nextest run --workspace --all-features --no-fail-fast`: 5453 passed, 22
failed, 95 skipped.

All 22 failures are pre-existing (tree unchanged this session):

- 3 `app::explorer::tests::dd_*` — same class as the documented
  `non_git_dd_vanishes` flake (process-global CWD/env mutation).
- 1 `app::tests::ex::scratch_buffer_writes_swap_when_dirty` — pre-existing.
- 1 `hjkl-vim-tui::proptest_fsm esc_returns_to_normal` — curswant invariant
  correctly caught `M-.` (Alt+dot) cursor move without syncing `sticky_col`; the
  point of the phase-0 assertion. Pre-existing.
- 17 `e2e pty_harness::*` — all TRY 3, PTY/TTY flake class. Pre-existing.

No regressions introduced by this review.

## Suggested next steps

1. R4 first — it is a live data-loss path and the narrowest fix here.
2. Re-point finding 11 at a smaller budget plus a single batched edit.
3. Replace W2's undo-depth proxy with the actual "did this delete remove a line"
   condition.
4. Promote each fixed case into the tier-2 corpus so the oracle guards it,
   rather than leaving it to the fuzzer to rediscover.
5. Teach the nvim driver to clear undo history after seeding (`nvim_command`
   with `:let old_ul=&ul | set ul=-1 | ... | let &ul=old_ul`) so undo/redo
   becomes fuzzable.

# Code review — pending changes (2026-07-29)

**Scope:** pending unstaged change: a new proptest regression entry in
`crates/hjkl-vim-tui/tests/proptest_fsm.proptest-regressions` (+1 line, hash
`37433df2`). The regression was discovered by the `esc_returns_to_normal`
property test and also triggers in `no_panic_on_random_keys`.

**Method:** Traced each failing input from `handle_key` through
`crossterm_to_input`, `dispatch_input`, `step_insert`/`step_normal`,
`replay_last_change`, `finish_insert_session`, and the curswant invariant check.
Verified the same code-path reproduction with the exact shrunk sequences.

## Findings

### 1. `replay_last_change` for empty `ReplaceMode` moves cursor without updating `sticky_col`

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

## Cleared

- **`push_undo()` doesn't change `dirty_gen`** — confirmed by reading
  `Editor::push_undo_at` (`crates/hjkl-engine/src/editor.rs:4808–4828`): it
  snapshots the rope (read-only), pushes into the undo tree, and clears redo —
  none of which bumps `dirty_gen`. So an empty `ReplaceMode` replay does pass
  through the curswant guard at line 142 (`dirty_gen` unchanged) and reaches the
  motion check. This is correct for the guard but exposes the missing
  `sticky_col` update.

- **`replay_insert_and_finish` is not affected** — it calls `mutate_edit` before
  `move_left`, which bumps `dirty_gen`, so the curswant check skips it. And it
  explicitly sets `vim_mut(ed).mode = Mode::Normal`, which also trips the
  mode-change guard.

- **Other `LastChange` variants in `replay_last_change` are safe**: all either
  mutate the buffer (changing `dirty_gen`) or change mode (tripping the
  mode-change guard) before any raw cursor move.

- **`leave_insert_to_normal_bridge` is not affected** — it explicitly calls
  `ed.set_sticky_col(Some(ed.cursor().1))` after `move_left`, syncing the sticky
  column (`crates/hjkl-vim/src/vim/insert_bridges.rs:867`).

## Hardening

- **Dot-repeat gate doesn't filter `alt`/`shift`** — `step_normal` line 463
  checks only `!input.ctrl && input.key == Key::Char('.')`. Real vim does not
  trigger dot-repeat on `Alt-.` or `Shift-.`. This is a divergence that
  increases the input surface hitting this bug but is not itself a correctness
  defect.

- **Wasteful `push_undo()` on empty ReplaceMode replay** — `dot_repeat.rs:207`
  pushes an undo entry for a replay that performs zero buffer mutations. This is
  harmless but creates pointless undo-tree entries.

## Coverage

Examined: the full dispatch chain from `hjkl_vim_tui::handle_key` →
`crossterm_to_input` → `dispatch_input` (including curswant pre/post checks) →
`dispatch_input_inner` → `step_insert` / `step_normal` →
`leave_insert_to_normal_bridge` → `finish_insert_session` →
`replay_last_change`. Also verified `push_undo_at` doesn't bump `dirty_gen`, and
confirmed `move_left` only moves the cursor with no sticky_col side effect.

Both failing proptest cases (`esc_returns_to_normal` and
`no_panic_on_random_keys`) converge on the same root cause.

Not reviewed: the remaining five proptest tests (all pass — they don't generate
the `R→Esc→e→.` or equivalent pattern). No other pending changes exist in the
working tree.
