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
