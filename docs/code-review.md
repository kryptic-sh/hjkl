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
cargo run -p hjkl-compat-oracle --release --example difffuzz -- 400 777
cargo run -p hjkl-compat-oracle --release --example dfcase -- '<buf>' <row> <col> '<keys>'
```

Both drivers pin `shiftwidth=4`, `expandtab`, `noautoindent`,
`foldmethod=manual`, so a divergence means an engine defect rather than config
skew between hjkl and `nvim --clean`.

Every entry below was verified by hand through `dfcase`; the fuzzer only located
them.

## Status

The first audit pass (seed 777, 400 cases) produced 114 divergences across 11
findings. The fix pass that followed closed most of them. **Re-running the same
seed after the fixes: 99 divergences** — roughly 9 are known harness noise and
21 are the residual blockwise cluster (finding 5 below).

Resolved and pruned from this document:

| Finding                             | Fixed by               |
| ----------------------------------- | ---------------------- |
| 1 — linewise case-op corruption     | `05418277` (see R3)    |
| 3 — `$` ignores its count           | `2e5b484b`             |
| 4 — `D` / `C` drop their count      | `2e5b484b`, `46561b22` |
| 6 — `j`/`k` lose the display column | `37b62b73`, `b949484c` |
| 7 — `+` / `-` / `_` clamp at edges  | `b4458135` (see R1)    |
| 10 — `VgU` cursor column            | `7e260e27` (see W1)    |
| 2 — `dk` on a one-line buffer       | `b4458135`             |
| 5 — blockwise + **text object**     | `ba813ca0`             |
| 8 — `b` in leading ws, `3w`, `W`    | `f808513e`             |
| 9 — empty-buffer `dd` register      | `d0ee6cca` (see R2)    |

Note the changelog entry claiming all nine findings are fixed overstates it —
findings 2, 5, 8 and 9 were only partially closed, and 11 was not closed at all.
The residue is tracked below.

## Regressions introduced by the fix pass

These broke previously-passing cases and are the highest priority.

### R1. `d_` is a no-op — breaks `tier1_corpus_passes`

```
`d_` on "one\ntwo\nthree", cursor (0,0)
hjkl: "one\ntwo\nthree"   (unchanged)
nvim: "two\nthree"
```

`b4458135` added a blanket guard to `apply_op_with_motion`
(`crates/hjkl-vim/src/vim/op_motion.rs`):

```rust
if start == end {
    return;
}
```

It sits _before_ `motion_kind(motion)` is consulted, so it never exempts
linewise motions. Count-1 `_` legitimately stays on its own row and still covers
that line. The guard is right for charwise motions and wrong for linewise ones.
Corpus case: `op_d_underscore_linewise_current`.

### R2. A no-op linewise delete clobbers the register — breaks `tier2_registers_corpus_passes`

```
`VGddd` on "one\ntwo\nthree\nfour\n"
hjkl register: "\n"
nvim register: "one\ntwo\nthree\nfour\n"
```

`d0ee6cca` added an `is_empty_op` branch to `cut_vim_range`
(`crates/hjkl-vim/src/vim/command.rs`) that force-records `"\n"` whenever the
delete yielded no text; the previous code skipped recording entirely.

The distinction it missed: a _first_ `dd` on an empty buffer does record `"\n"`
(that was finding 9, correctly fixed), but a linewise delete that removes
nothing must leave the register untouched. Record `"\n"` only when the operation
actually deleted a line. Corpus cases: `register_survives_noop_dd`,
`register_survives_noop_visual_line_delete`.

### R3. Whole-buffer case operators append a blank line

```
`VGgU` / `gUG` on "aa\nbb"
hjkl: "AA\nBB\n"   ← spurious trailing blank line
nvim: "AA\nBB"
```

`05418277`'s last-row special case keys off `ordered_top.0 >= n_rows`, which is
false when the range covers _every_ row (the cut leaves one empty row behind).
The else branch then re-inserts text carrying `read_vim_range`'s trailing `\n`
at (0,0), growing the buffer by a line. Not caught by any existing test.

## Still open from the original audit

### 11. Unbounded memory on large paste counts — fix was ineffective

`1116270b` clamps the paste count to `MAX_COUNT` (999,999,999), but the OOM
occurs far below that ceiling. Verified at exactly the cap:

```
yy999999999p   →  memory allocation of 12582912 bytes failed, exit 134
```

A 10-byte register at the cap is ~10 GB. The clamp bounds nothing that matters;
the limit has to be on resulting bytes, not on the count.

Original signal: `cargo fuzz run handle_key` OOMed at 4.3 GB from a seed under 1
KB (artifact `oom-2932edc579699c6bfaec9cbeb18e1673945ce40b`).

### 5 (residual). Blockwise visual — non-delete operators

`ba813ca0` fixed blockwise + text object (`<C-v>iwd`, `<C-v>i(d` now match). 21
blockwise divergences remain, concentrated on the indent operators and on `H` /
`L` / `gE` motions:

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

`}` changed behaviour (it used to land on (0,0)) but still does not reach the
last character. `B` still overshoots to the previous line.

### 2 (residual). `J` on the last line

```
`J` on "abc\n\n", cursor (1,0)
hjkl: "abc\n"     ← consumed the trailing blank
nvim: "abc\n\n"
```

`dk` is fixed; `J` still edits where vim aborts.

## Watch items

**W1. `7e260e27` moves the cursor with `buf_set_cursor_rc`.** The landing
position is correct, but per `docs/cursor-moves.md` that primitive does not
maintain curswant — the exact class of latent bug that document exists to
prevent. A following `j` may snap to a stale column.

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

## Suggested next steps

1. R1 and R2 first — they are the two failing oracle suites, and both are a few
   lines.
2. R3 next; it silently grows the buffer and nothing tests it.
3. Re-point finding 11 at a byte budget rather than a count ceiling.
4. Promote each fixed case into the tier-2 corpus so the oracle guards it,
   rather than leaving it to the fuzzer to rediscover.
5. Teach the nvim driver to clear undo history after seeding (`nvim_command`
   with `:let old_ul=&ul | set ul=-1 | ... | let &ul=old_ul`) so undo/redo
   becomes fuzzable.
