# Differential audit against neovim (2026-07-28)

## Method

Built a randomised differential fuzzer on top of the existing oracle
infrastructure: generate a random ASCII buffer and a random normal-mode
keystroke sequence, replay both through `hjkl_driver::run_case` and
`nvim_driver::run_case`, diff buffer / cursor / mode / default register.
Divergences are greedily shrunk (drop one keystroke token at a time, then one
buffer line at a time, while the divergence survives) and printed as paste-ready
corpus TOML.

- `crates/hjkl-compat-oracle/examples/difffuzz.rs` — the fuzzer.
- `crates/hjkl-compat-oracle/examples/dfcase.rs` — replay one ad-hoc case
  through both engines, for narrowing a shrunk case by hand.

```
cargo run -p hjkl-compat-oracle --release --example difffuzz -- 400 777
cargo run -p hjkl-compat-oracle --release --example dfcase -- '<buf>' <row> <col> '<keys>'
```

400 cases produced 114 divergences, which collapse to the 11 findings below
(several group a handful of related cases under one root cause). **Every finding
here was re-verified by hand through `dfcase` outside the fuzzer**; the fuzzer
only located them.

Both drivers pin `shiftwidth=4`, `expandtab`, `noautoindent`,
`foldmethod=manual`, so a divergence means an engine defect rather than config
skew between hjkl and `nvim --clean`.

## Severity 1 — buffer corruption

### 1. Linewise case operators scramble the buffer when the range includes the last line

```
buffer "(qux) foo\n    abc def", cursor (1,4), keys `guu`
hjkl: "\n    abc def(qux) foo"   ← row 0 emptied, text spliced onto row 1
nvim: "(qux) foo\n    abc def"
```

The case transform itself is correct; the text lands in the wrong place. Also
reproduces with `gUU`, `g~~`, `g??`, `gUj`, `2gUU` — any linewise case range
that reaches the final line. It does **not** reproduce on row 0, nor for the
charwise forms (`gUiw`, `gU$`), nor for visual `VgU`.

Cause: `apply_case_op_to_selection`
(`crates/hjkl-vim/src/vim/text_object_ops.rs:275`) cuts the range, then
re-inserts the transformed text at `buf_cursor_pos(ed.buffer())` (line 295) —
the cursor **after** the cut. `cut_vim_range`
(`crates/hjkl-vim/src/vim/command.rs:73`) makes no promise about where the
cursor lands; deleting through the last line removes the trailing newline, so
the cursor clamps somewhere other than the range start and the re-insert misses.
Insert at the range start (`top`), not at the post-cut cursor.

Reached from `crates/hjkl-vim/src/vim/linewise.rs:142`.

## Severity 2 — wrong edits and data loss

### 2. Operators at a buffer edge edit instead of aborting

Vim aborts the whole operator when its motion fails. hjkl clamps the motion and
edits anyway, destroying text.

| case                         | hjkl                | nvim      |
| ---------------------------- | ------------------- | --------- |
| `dk` on a one-line buffer    | `""` — line deleted | unchanged |
| `J` on the last line         | eats trailing blank | unchanged |
| `3D` with fewer than 3 lines | deletes anyway      | unchanged |

### 3. `$` ignores its count

`[count]$` means "end of line, `count-1` lines down".

```
buffer "aaa\nbbb\nccc\nddd", cursor (0,1)
`2$`   hjkl cursor (0,2)                nvim (1,2)
`d3$`  hjkl "a\nbbb\nccc\nddd"          nvim "a\nddd"
`y3$`  hjkl register "aa"               nvim "aa\nbbb\nccc"
```

### 4. `D` and `C` drop their count entirely

`crates/hjkl-vim/src/normal.rs:662` and `:670` call `delete_to_eol()` /
`change_to_eol()`, which take no count parameter at all
(`crates/hjkl-vim/src/vim/bridges.rs:213`, `:223`). `3D` performs a bare `D`.
`Y` (`normal.rs:666`) does thread its count into
`apply_op_with_motion(Yank, Motion::LineEnd, count)`, but that is finding 3 —
the motion discards it.

The blockwise `D`/`C`/`X`/`Y` at `normal.rs:320`–`337` are a separate, correct
path; count is not meaningful there.

### 5. Blockwise visual + text object selects the wrong range

```
buffer "'bar'", cursor (0,2), keys `<C-v>iwd`
hjkl: "'b"    (deleted cols 2..4)
nvim: "''"    (deleted cols 1..3)
```

The text object moves only the active end; the block's anchor stays at the
original cursor, and the computed end is wrong too. vim also switches to
charwise for these — `<C-v>i(` leaves nvim in `visual`, hjkl in `visual_block`.
Charwise (`viwd`) and linewise (`Viwd`) are correct, and `<C-v>` with plain
motions (`<C-v>jd`, `<C-v>jlld`) is correct.

## Severity 3 — cursor and register

### 6. `j` / `k` lose the virtual column across tabs

```
buffer "\ta  n1\n    'baz' (baz) {n1}", cursor (0,5), keys `j`
hjkl: (1,5)   nvim: (1,8)
```

hjkl carries the char index; vim carries the display column (tabstop 4 puts char
5 at screen column 8).

### 7. Edge motions clamp instead of failing

`+`, `-` and `3_` all move on a one-line buffer; vim leaves the cursor where it
was. Same root shape as finding 2, on the motion side.

### 8. Assorted motion landings

| case                                            | hjkl                  | nvim   |
| ----------------------------------------------- | --------------------- | ------ |
| `}` at EOF, buffer `"    it's.{foo}.A-B.{A-B}"` | (0,0)                 | (0,23) |
| `b` inside leading whitespace at col 3          | (0,3)                 | (0,0)  |
| `B` at col 0 of row 1                           | (0,0)                 | (1,0)  |
| `3w` past EOF on `"n1"`                         | (0,2) — past EOL      | (0,1)  |
| `W` on the last blank row of `"\n\n"`           | (2,0) — out of bounds | (1,0)  |

`3w` is another instance of the class-D past-EOL violation tracked in
`docs/cursor-moves.md`.

### 9. Linewise register newline is misplaced when the range ends at the last line

```
`dip` on "solo"           hjkl register "solo"        nvim "solo\n"
`3dd` at EOF on 4 lines   hjkl register "\nl2\nl3"    nvim "l2\nl3\n"
`dd`  on an empty buffer  hjkl register ""            nvim "\n"
```

The `3dd` case has the newline on the wrong end, so pasting the register back
misplaces the content. Plain `dd` mid-buffer is correct.

### 10. `VgU` cursor column

hjkl leaves the cursor at (1,4); vim moves it to the start of the selection,
(1,0).

## Robustness

### 11. Unbounded memory on large paste counts

`cargo fuzz run handle_key` OOMed at 4.3 GB from a seed under 1 KB and at most
256 keystrokes (artifact `oom-2932edc579699c6bfaec9cbeb18e1673945ce40b`; the
allocation stack bottoms out in `ropey::slice::RopeSlice::to_string`). Reduced
by hand: `yy1000000p` aborts the process.

`MAX_COUNT = 999_999_999` (`crates/hjkl-vim/src/vim/state.rs:15`) bounds motion
_walks_ but nothing bounds paste volume. The weekly cron fuzz job passed at its
600s budget; this surfaced at ~1500s, so expect it to start failing as the
corpus grows.

## Verified — not defects

Checked and deliberately excluded, so they are not re-reported next time:

- **`s` / `S` do not substitute.** They are bound to vim-sneak when
  `settings().motion_sneak` is on (`crates/hjkl-vim/src/normal.rs:674`, `:689`),
  which is the default. Intentional divergence.
- **`Y` yanks to end-of-line, not the whole line.** Matches nvim 0.12, which
  maps `Y` to `y$`. hjkl is right; traditional vim is the odd one out.
- **All `u` / `<C-r>` divergences were a harness artifact.** The nvim driver
  seeds the buffer over RPC, which is itself an undoable change, so `u` rolls
  back the fixture. Not an engine defect — but it does mean undo/redo is
  currently invisible to this fuzzer.
- **`==` reindents where `nvim --clean` does not.** hjkl ships a real formatter;
  stock nvim has no `indentexpr` for plain text. Design choice.

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

1. Fix finding 1 first — it is a one-argument change and it silently corrupts
   text.
2. Findings 2 and 7 are the same underlying rule ("a failed motion aborts the
   operator") and are probably one fix.
3. Findings 3 and 4 are the count plumbing; 3 subsumes part of 4.
4. Promote each fixed case into the tier-2 corpus so the oracle guards it,
   rather than leaving it to the fuzzer to rediscover.
5. Teach the nvim driver to clear undo history after seeding (`nvim_command`
   with `:let old_ul=&ul | set ul=-1 | ... | let &ul=old_ul`) so undo/redo
   becomes fuzzable.
