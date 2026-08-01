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

### 1.4 LSP and span follow-ups

| Item                                        | Where                                       | Note                                                                                                                                                                                |
| ------------------------------------------- | ------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| LSP full-sync still copies once             | `hjkl-lsp/src/runtime.rs`, `server.rs`      | `Buffer::content_joined()` caches the `Arc`, so `Arc::unwrap_or_clone` cannot move. Avoiding the copy requires direct serialization instead of an intermediate `serde_json::Value`. |
| `attach_buffer` copies at the boundary      | `hjkl-lsp/src/manager.rs` (`attach_buffer`) | Takes `text: &str` and calls `text.to_string()`. Change the boundary ownership model.                                                                                               |
| `styled_spans` is a write-only public field | `hjkl-engine/src/editor.rs`                 | No readers. Removal wins ~27% on full installation and nothing per keystroke, but it is a public API removal on a published crate.                                                  |

### 1.5 Remaining differential-oracle and code-review fixes

Fixed by `9a156885`, `b97e9bce`, `76cfb459`, and earlier commits. Detailed
reproductions for resolved entries are preserved in the supporting-evidence
appendix below, marked as fixed. Preserve each fixed case in the tier-2
compatibility corpus and verify it against nvim before changing expectations.

| Priority | Task                                                                                                                                                     | Where / acceptance criterion                                                           |
| -------- | -------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------- |
| High     | Surface a user-visible error when a paste is rejected for exceeding the 1 MiB budget. Currently `do_paste` returns `false` silently — no `Host` channel. | `crates/hjkl-vim/src/vim/command.rs:657-685`; `hjkl-engine/src/types.rs` `Host` trait. |
| Medium   | Implement blockwise non-delete operators with block geometry, including `H`, `L`, and `gE` motion/cursor behavior.                                       | 21 residual cases; `<C-v>iw<` on `"\t(x).[y]"` must remain unchanged like nvim.        |
| Medium   | Correct paragraph/WORD landings: `}` at EOF must reach the last character and `B` at column 0 of row 1 must remain on row 1.                             | Expected positions `(0,23)` and `(1,0)`.                                               |
| Medium   | Triage `V}u2)1gUiW` and fix the state or cursor transition between individually-correct components.                                                      | `"'qux'  A-B"` must become `"'QUX'  a-b"`.                                             |

### 1.6 Cursor-move API migration

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

### 1.8 Open from the 2026-08-01 review and the 0.40.0 cut

Full findings in `docs/code-review.md`; everything actionable there was fixed
and shipped in 0.40.0. What follows is what was NOT tackled.

#### Needs an owner decision, not more work

| Item                                                | Where                                                  | Decision needed                                                                                                                                                                                                               |
| --------------------------------------------------- | ------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `to_path_within` has no callers                     | `hjkl-lsp/src/uri.rs`                                  | The bug is fixed and the fn documents its own status, but it is dead. Deleting it is a public API removal on a published crate — ask, do not infer from grep.                                                                 |
| Anvil TOFU sidecar survives uninstall               | `apps/hjkl/src/app/ex_dispatch.rs` (`anvil_uninstall`) | Keeping it is safer (a changed artifact still trips `ChecksumMismatch`) but a user uninstalling to recover from a bad install cannot clear it. Delete on uninstall, or add `:Anvil forget`.                                   |
| `hjkl-quickfix` / `hjkl-app` have no `CHANGELOG.md` | those two crates                                       | Both are published and both shipped BREAKING changes in 0.40.0, documented only in the root changelog. BCTP says do not create changelog files unasked — but these are the two crates a consumer checks after a failed build. |

#### Deferred refactors

| Item                                           | Where                                                                                                         | Why deferred                                                                                                                                                                                                                                                |
| ---------------------------------------------- | ------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Four hand-rolled width truncators              | `hjkl-statusline`, `hjkl-prompt-tui`, `hjkl-editor-tui`, `hjkl-which-key`, `hjkl-buffer-tui`                  | Each re-implements "accumulate `UnicodeWidthChar::width` until the budget runs out" with different tab handling. Cross-crate UI refactor, real regression surface, no bug attached.                                                                         |
| `is_safe_relative_path` vs `safe_join`         | `hjkl-bonsai/src/runtime/source.rs`, `hjkl-anvil/src/installer.rs`                                            | Same invariant, different contracts. The clean unification is reworking both onto `hjkl_fs::resolve_under`, which also catches a symlink in the prefix — a behaviour change, so its own change.                                                             |
| `Options` / `OptionsConfig` still hand-written | `hjkl-engine/src/types.rs`, `hjkl-app/src/config.rs`                                                          | The option registry drives every `:set` table and the config key mapping, but not these two structs — both need compile-time fields (engine snapshot, serde schema). Pinned by tests instead. A generating macro was considered and declined as too opaque. |
| Oversized modules                              | `nvim_api.rs` (5.7k), `explorer.rs` (4.4k), `render.rs` (3.8k), `lsp_glue.rs` (3.2k), `ex_dispatch.rs` (3.2k) | Recording only; splitting has no correctness payoff. Noted because the duplicate-`WorkspaceEdit` bug lived in `lsp_glue.rs` and survived precisely because the file is that size.                                                                           |

#### `char_col_to_visual_col` is not wide-character aware

`crates/hjkl-buffer/src/geom.rs`. It counts every non-tab character as **one**
cell:

```rust
visual += if ch == '\t' { tab_w - (visual % tab_w) } else { 1 };
```

The renderer does not. `paint_row` advances by `ch.width().unwrap_or(1)`, the
real `unicode-width` value. So on any line containing CJK, emoji, or a combining
mark, the engine's idea of a visual column and the column the glyph was actually
painted at diverge — by one cell per wide character.

This is load-bearing in more places than it looks: `motions.rs` (`$`, sticky
column), `cursor_move.rs` (`Move::Vertical`), `editor.rs` (`cursor_screen_pos`),
and `hjkl-buffer-tui`'s cursorcolumn pass all key off it. The cursorcolumn fix
in 0.40.0 deliberately routed through this helper so the bar stays consistent
with where the _cursor_ is drawn — consistency was the requirement there — but
both are then wrong together against the painted text.

Fix by teaching `char_col_to_visual_col` / `visual_col_to_char_col` the
`unicode-width` table, which `hjkl-buffer` already depends on. Expect fallout:
this is the same class as the `listchars` approximation fixed in 0.40.0, and
several column assertions across the engine were written against the naive
behaviour. Verify against nvim, which is wide-char correct.

#### Smaller, unclaimed

- **`:set` write-through is TUI-only.** `--headless` and `--embed` call
  `hjkl_ex::try_dispatch` directly rather than going through `App::dispatch_ex`,
  so a `:set` in those modes applies to the session and is never persisted.
  Defensible for non-interactive modes, but it was an implementation call, not a
  stated decision.
- **Bare `:!cmd` gives the child no tty.** It runs under `Command::output()`,
  which captures stdout and hands the child a null stdin, so `:!git commit` or
  `:!less` cannot work. Vim suspends the TUI and passes the terminal through.
  Either implement the suspend or document the limitation on `:!`.
- **The trash directory has no reaper.** `$XDG_CACHE_HOME/hjkl/trash/` grows
  without bound and `MAX_RETRIES = 1000` means the 1001st deletion of a
  same-named file fails rather than recycling a slot. 0.40.0 documented this;
  nothing reclaims it.
- **Mutex-poisoning policy is documented, not enforced.** `buffer.rs` now states
  that `lock().unwrap()` on buffer state is deliberate and a poisoned lock is
  fatal. The ~110 call sites are unchanged, so one panic while any of those
  locks is held still takes down every later access, including the save path.

### 1.7 Harness, coverage, and hardening

- Clear nvim undo history after fixture seeding, then fuzz undo/redo. Extend
  cursor comparison beyond ASCII and add ex/search, fold, and `gq` coverage.
- Complete review coverage for app, LSP, editor/TUI, ex/completion,
  prompt/menu/picker, and remaining non-core crates.
- Stabilize flaky PTY e2e cases. Cache/CWD/color isolation landed in `ca3852b2`;
  the two explorer `dd` tests that still failed under `cargo test`'s thread pool
  are fixed — they pointed `XDG_CACHE_HOME` at the very directory they were
  exploring, so the trash's own `hjkl/` directory appeared as a tree entry and,
  sorting before files, absorbed the `j` that was meant to land on the file
  under test. Unspecified PTY flakes may remain.
- **`cargo test` must stay usable — `ddu_then_redo_retrashes` races under it.**
  `app::explorer::tests::ddu_then_redo_retrashes` mutates two pieces of
  process-global state (the working directory via `CwdGuard::enter`, and
  `XDG_CACHE_HOME`). Under nextest each test is its own process, so nothing can
  race and it passes; under `cargo test`'s thread pool a concurrent test can
  change the cwd out from under it, so the explorer enumerates a different
  directory and `dd` trashes nothing — the failure reads as "dd must trash".
  Measured at roughly 2 failures in 30 runs of `cargo test -p hjkl` (0 in 14 on
  the unmodified tree at the time, so the rate is low, not zero).

  `cargo nextest run` is canonical in CI, but `cargo test` is documented in
  CONTRIBUTING as working too, and a test that fails ~7% of local runs trains
  people to re-run rather than read. Fix by removing the global-state dependency
  (thread an explicit root through, the way `AnvilPaths` did for the anvil tests
  in audit-r2 fix 4) rather than by adding a nextest test-group — a group would
  not help `cargo test` at all. Audit the other `CwdGuard` users for the same
  shape while there.

- **"CI green" does not include the Cron workflow.** miri / fuzz / deny / bench
  run on a separate weekly schedule and are not checked by a release. They were
  not checked for 0.40.0. Either fold the cheap ones into the release gate or
  add an explicit pre-release step that reads the last Cron result.
- Use `checked_add` in `SnapshotFoldProvider::next_visible_row`.
- Rename undo-tree `depth` to `depth_for_keyframe`.
- Bounds-check public `rope_line_char_count` / `rope_line_bytes` helpers.

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
- **A green local run is not a green CI run.** Local checks are Linux-only, so
  anything platform-shaped passes locally and fails on the matrix. A
  `#[cfg(unix)]` test that created a non-UTF-8 filename sat red on macOS (APFS
  rejects the name with `EILSEQ`) across ~15 commits during the 0.40.0 work,
  because every slice was verified locally and pushed without checking the run.
  Check `gh run list` after pushing, not only before releasing.
- macOS and Windows are the two platforms local work never exercises: filename
  encoding, path separators, and symlink permissions all differ there. Gate on
  the capability (probe and skip) rather than on `cfg(unix)`, which includes
  macOS.

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
does not fail at count 1; `k` at row 0 does. Fixed by `b97e9bce`.

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

The budget is therefore too generous relative to per-byte overhead. Fixed by
`b97e9bce`: budget lowered to 1 MiB with batched, pre-allocated edits. Silent
rejection of over-budget pastes is tracked in the ranked backlog above.

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
vim keys off. Fixed by `b97e9bce`: `cut_vim_range` now uses the actual inverse
edit payload; an empty inverse leaves registers untouched.

**W3. `62bd4853` silently truncates oversized pastes.** A user asking for N
copies of a large register gets fewer, with no message. Silent partial execution
is arguably worse than refusing. Superseded by `b97e9bce`: oversized pastes are
now rejected outright rather than silently truncated. The rejection is still
silent — tracked in the ranked backlog above.

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

Remediation is tracked in the ranked backlog above. Fixed by `9a156885`.

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

Fixed by `9a156885`.

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

Fixed. The first pass added a hex branch to `adjust_number_visual` but left two
divergences that the normal-mode path did not have; both are closed by folding
the two implementations into one shared `adjusted_number_at` helper, so the
modes can no longer drift:

- Hex digit case was always lowercased (`0xAB` `<C-a>` → `0xac`). vim takes the
  case of the **last letter digit** of the original (`0xaB` → `0xAC`, `0xAb` →
  `0xac`), falling back to the `x`/`X` prefix's own case when the number has no
  letter digit (`0X19` → `0X1A`, `0x19` → `0x1a`). This one was wrong in normal
  mode too.
- Visual decimal dropped zero-padding (`007` `<C-a>` → `8` instead of `008`),
  which normal-mode `adjust_number` had handled since it was written.

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

The same bug exists in `collect_substitute_matches` (lines 442-458). Fixed by
`9a156885`.

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
case mappings. Fixed by `9a156885`.

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
  `sticky_col` IS `Some` (display column) — see finding 1 (fixed by `9a156885`).

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
  invisible. Fixed. `e219e664` first assigned the raw `cursor.row`, which does
  not repair the shrink case at all: the guard above already establishes
  `cursor.row >= top_row`, so on a shrink (`last < top_row <= cursor.row`) that
  assignment can only move `top_row` further past the rope's end. `top_row` is
  now set to `cursor.row.min(last_row)`, which pulls it back into the live rope.
  The other `None` path — the cursor's row hidden inside a closed fold — is
  covered by the same assignment, and both paths now have a regression test.

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

Both actions are tracked in the ranked backlog above. Fixed by `9a156885` (empty
replay) and `b97e9bce` (modifier rejection).

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
