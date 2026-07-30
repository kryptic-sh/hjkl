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

## Evidence for differential-audit backlog items

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

This regresses the earlier one-line `dk` edge fix from `b4458135`. The fuzzer
independently surfaced `dk` as a new divergence in the latest pass. Multi-line
`dk` (row 2 of 3) is still correct.

This distinguishes failed motions from zero-distance successful motions: `_`
does not fail at count 1; `k` at row 0 does. The action is tracked in
`docs/backlog.md`.

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

The budget is therefore too generous relative to per-byte overhead. Remediation
is tracked in `docs/backlog.md`.

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

### T1. Composite case-op sequence — unconfirmed cause

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
is arguably worse than refusing; remediation is tracked in `docs/backlog.md`.

## Not covered by this pass

- **Non-ASCII.** `HjklOutcome.cursor` is a char index and `NvimOutcome.cursor`
  is a byte index, so the comparison is only sound on ASCII. This is exactly
  where the char-vs-grapheme column trap lives, and it is unaudited.
- **Ex commands** (`:`), search prompts (`/`, `?`) — the in-process hjkl driver
  cannot replay them.
- **Undo / redo** — nvim fixture seeding over RPC creates an undoable change, so
  `u` rolls back the fixture rather than the generated operation.
- **Folds** (`z`) and `gq` — excluded to avoid config-skew noise.
- The app / window layer, LSP, and everything above the engine.

# Code review — full-codebase (2026-07-29)

Tree clean, v0.39.0 + 13 commits. Reviewed via three read-only `explore`
sub-agents (hjkl-vim, hjkl-engine, hjkl-buffer) plus direct review of the recent
diff, curswant invariant, rope_util, and Move API. Every cited file:line was
re-read and every failure scenario traced end-to-end by hand.

## Evidence for code-review backlog items

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

Remediation is tracked in `docs/backlog.md`.

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

Remediation is tracked in `docs/backlog.md`.

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

Remediation is tracked in `docs/backlog.md`.

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

The same bug exists in `collect_substitute_matches` (lines 442-458). Remediation
is tracked in `docs/backlog.md`.

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

## Hardening evidence

- **`SnapshotFoldProvider::next_visible_row` uses unbounded `+= 1`**
  (`buffer_impl.rs:608`): `r += 1` in while loop without `checked_add`.
  `prev_visible_row` uses `checked_sub`. Risk only at `last == usize::MAX`,
  unreachable with realistic buffers. Remediation is tracked in
  `docs/backlog.md`.

- **`Move::Vertical` bootstrap path** (`cursor_move.rs:89`): when `sticky_col`
  is `None`, the bootstrap uses `self.cursor().1` (char column) as `want`, but
  `want` is later compared against `max_col` (char count, ok) and used directly
  as cursor column (ok for the bootstrap case). The real bug is when
  `sticky_col` IS `Some` (display column) — see finding 1.

- **`prune_root_side` depth inconsistency with `clear_all`** (`undo.rs`):
  `clear_all` resets survivor depth to 0 (line 1105); `prune_root_side` does not
  (line 1032-1068). The naming remediation is tracked in `docs/backlog.md`.

- **`rope_line_char_count` / `rope_line_bytes` OOB panic** (`buffer.rs:806-815`,
  `794-803`): public functions without bounds checks. Current callers clamp the
  row; API hardening is tracked in `docs/backlog.md`.

- **`ensure_cursor_visible` top_row stale on shrink from other view**
  (`buffer.rs:189-191`): when `cursor_screen_row_from` returns `None`, only
  `top_col` is zeroed; `top_row` stays where it was, potentially leaving cursor
  invisible. Remediation is tracked in `docs/backlog.md`.

## Remaining review coverage

- `apps/hjkl/` — app layer, PTY harness, e2e tests.
- `crates/hjkl-lsp/` — LSP runtime, codec, manager.
- `crates/hjkl-editor/`, `crates/hjkl-editor-tui/`, and sibling TUI crates.
- `crates/hjkl-ex/`, `crates/hjkl-completion/` — ex commands, completion.
- `crates/hjkl-prompt/`, `crates/hjkl-menu/`, `crates/hjkl-picker/`.
- All remaining TUI and non-core crates.

# Code review — pending changes (2026-07-29)

**Scope:** pending unstaged change: a new proptest regression entry in
`crates/hjkl-vim-tui/tests/proptest_fsm.proptest-regressions` (+1 line, hash
`37433df2`). The regression was discovered by the `esc_returns_to_normal`
property test and also triggers in `no_panic_on_random_keys`.

**Method:** Traced each failing input from `handle_key` through
`crossterm_to_input`, `dispatch_input`, `step_insert`/`step_normal`,
`replay_last_change`, `finish_insert_session`, and the curswant invariant check.
Verified the same code-path reproduction with the exact shrunk sequences.

## Evidence for pending-change backlog item

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

## Hardening evidence

- **Dot-repeat gate doesn't filter `alt`/`shift`** — `step_normal` line 463
  checks only `!input.ctrl && input.key == Key::Char('.')`. Real vim does not
  trigger dot-repeat on `Alt-.` or `Shift-.`. This divergence increases the
  input surface hitting this bug but is not itself a correctness defect.

- **Wasteful `push_undo()` on empty ReplaceMode replay** — `dot_repeat.rs:207`
  pushes an undo entry for a replay that performs zero buffer mutations. This is
  harmless but creates pointless undo-tree entries.

Both actions are tracked in `docs/backlog.md`.
