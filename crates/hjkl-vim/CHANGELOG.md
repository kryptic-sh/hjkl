# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.41.0] - 2026-08-04

### Fixed

- `insert_newline_bridge` applies `autoindent` the way vim does: the computed
  indent REPLACES the leading whitespace of the text that moves down, and it is
  computed from the part of the line left of the cursor rather than the whole
  line. Previously it inserted in front of that whitespace, so an Enter at
  column 0 of an indented line produced a row carrying both the old indent and a
  fresh copy — geometric growth under a held key. Only the plain autoindent path
  changed; the autopair open-pair, code-fence and comment-continuation branches
  are untouched, and `noautoindent` still moves the text down verbatim.

- `.` reuses the register the repeated change named (`:h redo-register`).
  `LastChange::OpMotion`, `OpTextObj`, `DeleteToEol` and `Paste` did not carry
  the register, so `"adw` then `.` deleted into the unnamed register and `"ap`
  then `.` pasted from it. They now record it the way `LineOp` already did, and
  `dot_repeat` restores it before replaying. Four cases in
  `corpus/tier2_registers.toml` pin this against nvim.
- An over-budget paste reports `E342: Out of memory!  (allocating N bytes)`
  instead of failing silently. `do_paste` returned `false` and nothing
  downstream could tell that apart from an empty register; it queues the message
  on the editor now (`Editor::push_error`), which the host drains. Both the
  byte-budget checks and the arithmetic-overflow guards report it — the user's
  ask is the same size either way. An empty register is still silent, as in vim.
- **Blockwise `>` / `<` shift at the block's left column.** They fell through to
  the linewise `indent_rows` / `outdent_rows` under a comment claiming vim did
  the same — it does not: `<C-v>jl>` with the block starting at column 2 turns
  `"abcdef"` into `"ab    cdef"`, where the linewise fallback produced
  `"    abcdef"`. New `indent_block` / `outdent_block` do the real thing: the
  fill is built from the block's DISPLAY column so `noexpandtab` emits a tab
  only where one actually reaches the next tab stop, a row too short to reach
  the block column (or an empty one) is skipped, and an outdent splits a tab
  that straddles the boundary — `"ab\tcd"` outdented from column 2 at
  `tabstop=8 shiftwidth=4` becomes `"ab  cd"`. A row with no whitespace at the
  block column is left alone. Six oracle cases; the differential fuzzer at seed
  777 drops from 78 to 77.

  Not fixed, and tracked in `docs/backlog.md`: `<C-v>iw<` still outdents the
  whole line, because a text object collapses hjkl out of blockwise visual
  before the operator runs. That is a separate defect from the shift geometry.

- **Visual mode takes a `"reg` selector.** `"` opened the register chord only in
  Normal mode, so in a selection the `"` fell through unconsumed, the register
  letter armed the around-text-object chord instead, and the operator key was
  swallowed as that chord's target — `vll"ad` left the buffer untouched and the
  selection still up. `"` now arms `Pending::SelectRegister` in charwise,
  linewise and blockwise visual as well, so `"ad`, `"ay`, `"ac` and `"ap` all
  reach the register they name.
- `.` after `"ax` / `"aX` deletes into `"a` too. `LastChange::CharDel` was the
  last variant with no `register` field, so the repeat fell back to the unnamed
  register and left `"a` holding the FIRST deletion (`"axl.` on `"abcdef"` left
  `"a` as `a` where nvim leaves `c`). The live `x` / `X` path already honoured
  an explicit register; only the repeat did not. **`LastChange::CharDel` gained
  a `register` field** — a breaking change for anyone matching on it.
  `dot_repeat_char_delete_reuses_the_explicit_register` covers `x`, `X` and a
  counted `x`, and five more cases in `corpus/tier2_registers.toml` pin the
  whole family against nvim.
- `.` after a visual operator or `"acgn` reuses the register too, closing out
  the family. `LastChange::GnOp` and `VisualOp` were the two remaining variants
  with no `register` field — both **gained one**, a breaking change for anyone
  matching on them. The blockwise replay had a second defect behind that one:
  `replay_block_visual_op` passed a hardcoded `None` target to
  `record_delete_block`, so even with the register restored the repeat could not
  have written it.
- `d}` with the cursor already on the last character of the last line deletes
  that character. vim's `}` sets `*pincl` even when it does not move, so the
  zero-distance form is a successful INCLUSIVE motion; hjkl treated every
  zero-width charwise range as a failed motion and did nothing. The landing is
  now emitted as the equivalent one-char exclusive range — the shape `dl` on a
  line's final char already produces — rather than by loosening the
  `start == end` guards, which still have to reject `d$` on an empty line.
- A counted `$` fails on the last line instead of clamping to it. vim's
  `nv_dollar` runs `cursor_down(count - 1)` before moving to the line end and
  aborts the whole command when that fails — which happens only when the cursor
  is already on the last line; an overshooting count still succeeds, clamped.
  hjkl always clamped, so `2$` on a one-line buffer moved to the line end and
  `2C` / `2D` emptied the line where vim does nothing at all. `5$` on three rows
  still lands on row 2 and `5C` still collapses them. Found by the differential
  fuzzer (seed 777, case 115); its divergence count drops from 84 to 83.
- `"aD` and `"aC` write the named register. `command::delete_to_eol` wrote the
  deleted text with `set_yank`, which only ever reaches the unnamed register, so
  an explicit `"reg` was silently dropped. It now routes through
  `record_delete`, which also puts an unnamed `D` in the small-delete register
  `"-` as vim does.

### Added

- `MAX_PASTE_BYTES` is public, so a host can report the paste budget it hit
  rather than restating the number. `vim::command` is public to carry it.

### Changed

- The `p` / `P` byte budget is raised from 1 MiB to 64 MiB. The old value was
  derived from a per-iteration paste implementation that no longer exists;
  measured on the current batched path, peak RSS is linear in payload (~3.1x
  charwise, ~4.1x linewise, ~2.9x blockwise) and independent of document size,
  so 64 MiB costs ~208 MiB peak and ~73 ms. Pasting is no longer stricter than
  opening a file of the same size. Over-budget pastes are still rejected — but
  no longer silently; see the `E342` entry above.
- Blockwise `p` / `P` no longer rebuilds the whole document. It applied its edit
  by materializing every line into a `Vec<String>`, joining it back and
  replacing the entire buffer, so cost scaled with the DOCUMENT rather than the
  pasted block and every block paste reached the undo stack, the change log and
  any LSP as a whole-document replacement. It now issues one `Edit::InsertBlock`
  (plus one `Edit::InsertStr` when the block extends past the last row). A 1 MiB
  block paste into a 1 000 000-line buffer drops from +128.5 MiB and 351 ms to
  +0.2 MiB and 1 ms; it remains a single undo step.

### Fixed

- A blockwise paste extending past the last row no longer strips the buffer's
  trailing newline. Rows opened past EOF were spliced into ropey's phantom
  trailing line — the newline terminator — instead of past it.
- Operator ranges over `}` follow vim's three `findpar` / `:h exclusive` rules.
  A `}` that lands on the last character of the last line makes the motion
  inclusive, so `d}` in the final paragraph reaches end-of-buffer instead of
  stopping one character short. An exclusive charwise range that ends in column
  0 of a later row pulls its end back to the end of the previous line and turns
  inclusive, and promotes to linewise when the start is at or before its line's
  first non-blank — so `y}P` from column 0 pastes the paragraph back as whole
  lines like nvim, and `d}` from mid-line no longer eats the line break. A
  charwise `d` spanning more than one row whose range ends with only whitespace
  left on the last row, started from within the indent, promotes to linewise
  (vim's "strange Vi behaviour" in `op_delete`), which is what gives `d}` the
  linewise register content nvim produces.
- `(` and `)` are all-or-nothing under a count. `findsent` fails as soon as one
  repetition has nowhere left to go and vim leaves the cursor untouched; the
  motion previously moved as far as it could, so `2)` on a line with no sentence
  terminator landed on the last character instead of staying put (the cause of
  the `V}u2)1gUiW` composite divergence) and `3(` past the first sentence
  stopped at `(0, 0)` instead of failing. A terminator that closes the buffer
  still counts as a real boundary, so `3)` on `"One. Two."` succeeds while `3)`
  on `"One. Two"` fails — the new `SentenceStep` classification in
  `vim::text_object` is what separates the two.

## [0.40.0] - 2026-08-01

### Fixed

- Linewise operator motions at a buffer edge no longer delete the current line;
  rejected oversized pastes preserve the prior dot-repeat change; and empty
  Replace-mode dot replay restores the cursor's sticky column without an undo
  entry.
- Counted linewise paste now uses one edit and rejects oversized charwise or
  blockwise payloads before allocating or mutating the buffer.

### Removed

- Dropped the `crossterm` feature and `handle_key`. The crossterm-driven FSM
  wrapper lives in the new `hjkl-vim-tui` crate. Three test files
  (`editor_fsm.rs`, `vim_fsm.rs`, `proptest_fsm.rs`) relocated to
  `hjkl-vim-tui/tests/`. Phase 3 of #162.

## [0.23.2] - 2026-05-18

### Fixed

- Test helpers in `vim_fsm.rs` and `editor_fsm.rs` updated to use
  `unwrap_or_default()` after `Buffer::line` changed to return `Option<String>`
  in hjkl-buffer 0.8.1.

## [0.23.1] - 2026-05-18

## [0.23.0] - 2026-05-17

### Added

- `descriptors` module: `VimDescriptor` struct and `children_for(mode, prefix)`
  function that return the direct children of a vim FSM prefix for which-key
  popup integration (#64). Covers Normal root (83 bindings), g-prefix (19),
  z-prefix (11), operator-pending motions (24), and Visual root. `COUNT_*`
  constants assert exact counts to catch table drift.
- Added `hjkl-keymap = "0.3"` dependency (used by
  `VimDescriptor::key: KeyEvent`).

## [0.22.0] - 2026-05-17

### Changed

- `default` features now `[]` — `crossterm` dropped from defaults (#99).
  Consumers that relied on the default must add `features = ["crossterm"]`
  explicitly.
- Bumped pinned `hjkl-engine` `0.10` → `0.11` (#99 cascade).

## [0.21.0] - 2026-05-17

### Changed

- Bumped pinned `hjkl-engine` `0.9` → `0.10` (#96 cascade).

## [0.20.0] - 2026-05-16

### Added

- `OperatorKind::AutoIndent` — new public variant for the `=` operator. The FSM
  grammar now routes `=` in Normal mode into the `AfterOp` reducer with
  `op = OperatorKind::AutoIndent`, enabling `=<motion>` and `==` (double) forms.
  Users mapping operator-based keybinds gain auto-indent without extra host
  code.

### Changed

- `hjkl-engine` dependency bumped `0.8` → `0.9` (tracks hjkl-engine 0.9.1 which
  fixes the buffer pin and resolves type collisions with hjkl-buffer 0.7).
- `hjkl-buffer` dependency bumped `0.6` → `0.7`.

### Fixed

- FSM test suite updated to use `Editor::mouse_click_doc` after the upstream
  rename removed `mouse_click_in_rect`.

## [0.19.0] - 2026-05-15

### Added

Phase 6.6 of kryptic-sh/hjkl#72 — the vim FSM physically lives in `hjkl-vim` now
(previously inline in `hjkl-engine::vim`). hjkl-vim is the canonical external
entry point for driving the vim grammar.

- `hjkl_vim::dispatch_input(editor, input) -> bool` — canonical FSM entry. Wraps
  `Editor::begin_step` / per-mode dispatch / `Editor::end_step`.
- `hjkl_vim::handle_key(editor, key_event) -> bool` (under
  `#[cfg(feature = "crossterm")]`) — convenience wrapper that decodes a
  crossterm `KeyEvent` via `hjkl_engine::crossterm_to_input` and routes through
  `dispatch_input`. Emits cursor-shape change after dispatch.
- `hjkl_vim::feed_input(editor, planned) -> bool` — convenience wrapper that
  decodes a `hjkl_engine::PlannedInput` via `hjkl_engine::decode_planned_input`
  and routes through `dispatch_input`. Emits cursor-shape change after dispatch.
- `hjkl_vim::search_prompt::step_search_prompt` — search-prompt FSM body.
  Dispatched by `dispatch_input` before the general per-mode dispatch.
- `hjkl_vim::insert::step_insert` (+ `handle_insert_key`) — insert-mode FSM
  body.
- `hjkl_vim::normal::step_normal` (+ `handle_normal_only` + 17 dispatch helpers)
  — normal + visual mode FSM body. Drives all keys for non-insert
  non-search-prompt modes.

### Changed

- Depends on `hjkl-engine` `>=0.7`. hjkl-engine 0.7.0 ships the breaking FSM
  removal that this crate's new entry points replace.
- Test suite expanded with ~200 FSM-driving tests relocated from hjkl-engine's
  internal test mods. New `tests/vim_fsm.rs`, `tests/editor_fsm.rs`,
  `tests/proptest_fsm.rs`, `tests/dispatch_input.rs`.

### Migration

If you previously drove the FSM through `hjkl-engine`:

```rust
// Before:
hjkl_engine::step(&mut editor, input);
editor.handle_key(crossterm_key);
editor.step_input(input);
editor.feed_input(planned);

// After:
hjkl_vim::dispatch_input(&mut editor, input);
hjkl_vim::handle_key(&mut editor, crossterm_key);
hjkl_vim::dispatch_input(&mut editor, input);
hjkl_vim::feed_input(&mut editor, planned);
```

## [0.18.1] - 2026-05-14

### Added

- `PendingState::RecordMacroTarget` — second-key chord state for `q<x>`. On the
  next `Key::Char(ch)` matching `[a-zA-Z0-9]` emits
  `EngineCmd::StartMacroRecord { register: ch }`; Esc or any other key cancels.
  Phase 5b of kryptic-sh/hjkl#71.
- `PendingState::PlayMacroTarget { count }` — second-key chord state for `@<x>`.
  Accepts `[a-zA-Z0-9]`, `'@'` (replay last played), and `':'` (replay last `:`
  ex command, vim's `@:`); emits `EngineCmd::PlayMacro { register, count }`. Esc
  or any other key cancels. Phase 5b/5d of kryptic-sh/hjkl#71.
- `EngineCmd::StartMacroRecord { register }` — chord completion for `q<ch>`.
  Host calls `Editor::start_macro_record(register)`. Phase 5b.
- `EngineCmd::PlayMacro { register, count }` — chord completion for `@<ch>`.
  Host calls `Editor::play_macro(register, count)` (or, for `':'`, replays the
  last ex command via the host). Phase 5b/5d.

## [0.18.0] - 2026-05-13

### Added

- `PendingState::SetMark` — second-key chord state for `m<x>`. On the next
  `Key::Char(ch)` emits `EngineCmd::SetMark { ch }`; Esc or any non-char key
  cancels. Phase 5a of kryptic-sh/hjkl#71.
- `PendingState::GotoMarkLine` — second-key chord state for `'<x>`. On the next
  `Key::Char(ch)` emits `EngineCmd::GotoMarkLine { ch }`; Esc or any non-char
  key cancels. Phase 5a of kryptic-sh/hjkl#71.
- `PendingState::GotoMarkChar` — second-key chord state for `` `<x> ``. On the
  next `Key::Char(ch)` emits `EngineCmd::GotoMarkChar { ch }`; Esc or any
  non-char key cancels. Fires in both Normal and Visual modes. Phase 5a of
  kryptic-sh/hjkl#71.
- `EngineCmd::SetMark { ch }` — chord completion for `m<ch>`. Host calls
  `Editor::set_mark_at_cursor(ch)`. Phase 5a of kryptic-sh/hjkl#71.
- `EngineCmd::GotoMarkLine { ch }` — chord completion for `'<ch>`. Host calls
  `Editor::goto_mark_line(ch)`. Phase 5a of kryptic-sh/hjkl#71.
- `EngineCmd::GotoMarkChar { ch }` — chord completion for `` `<ch> ``. Host
  calls `Editor::goto_mark_char(ch)`. Phase 5a of kryptic-sh/hjkl#71.

## [0.17.0] - 2026-05-13

### Added

- `CountAccumulator` — digit-prefix count buffer for the vim grammar. Owns vim's
  count semantics including the digit-0-vs-LineStart quirk and overflow
  saturation. Migrated from `apps/hjkl`'s `pending_count: String` field.
- `MotionKind::BracketMatch` (`%`) — jump to the matching bracket (`()`, `[]`,
  `{}`). Count is passed through to the engine; the engine currently implements
  the matching-bracket semantic only (vim's `N%` percentage-of-file form is not
  yet wired). No-op when the cursor is not on a bracket character. Phase 3f of
  kryptic-sh/hjkl#69. Enum remains `#[non_exhaustive]`; consumers on hjkl-vim
  0.16.x must bump to 0.17 to handle this new arm.
- `MotionKind::ViewportTop` (`H`) — cursor to top of visible viewport; count
  offsets `count - 1` rows down from top (matching vim's `H` count semantics).
  Lands on first non-blank. Phase 3g of kryptic-sh/hjkl#69.
- `MotionKind::ViewportMiddle` (`M`) — cursor to middle row of visible viewport;
  count ignored (vim's `M` is a plain motion). Lands on first non-blank. Phase
  3g of kryptic-sh/hjkl#69.
- `MotionKind::ViewportBottom` (`L`) — cursor to bottom of visible viewport;
  count offsets `count - 1` rows up from bottom (matching vim's `L` count
  semantics). Lands on first non-blank. Phase 3g of kryptic-sh/hjkl#69.
- `MotionKind::HalfPageDown` (`<C-d>`) — move cursor half a page down; count
  multiplies the half-page distance. Lands on first non-blank. Phase 3g of
  kryptic-sh/hjkl#69.
- `MotionKind::HalfPageUp` (`<C-u>`) — move cursor half a page up; count
  multiplies. Lands on first non-blank. Phase 3g of kryptic-sh/hjkl#69.
- `MotionKind::FullPageDown` (`<C-f>`) — move cursor a full page down (2-line
  overlap); count multiplies. Lands on first non-blank. Phase 3g of
  kryptic-sh/hjkl#69.
- `MotionKind::FullPageUp` (`<C-b>`) — move cursor a full page up (2-line
  overlap); count multiplies. Lands on first non-blank. Phase 3g of
  kryptic-sh/hjkl#69.

## [0.16.0] - 2026-05-13

### Added

- `MotionKind::FindRepeat` (`;`) — repeat last `f`/`F`/`t`/`T` in the same
  direction. No-op if no prior find exists. Phase 3e of kryptic-sh/hjkl#69.
- `MotionKind::FindRepeatReverse` (`,`) — repeat last `f`/`F`/`t`/`T` in the
  reverse direction. No-op if no prior find exists. Phase 3e of
  kryptic-sh/hjkl#69. Enum remains `#[non_exhaustive]`; consumers on hjkl-vim
  0.15.x must bump to 0.16 to handle these new arms.

## [0.15.0] - 2026-05-13

### Added

- `MotionKind::GotoLine` (`G`) — Phase 3d of kryptic-sh/hjkl#69. Count
  semantics: count 0 or 1 (bare `G`) → last line of buffer; count > 1 → jump to
  that 1-based line number. `gg` (first line) continues to route through the
  G-chord path (`Editor::after_g`) and is unaffected. Enum remains
  `#[non_exhaustive]`; consumers on hjkl-vim 0.14.x must bump to 0.15 to handle
  this new arm.

## [0.14.0] - 2026-05-13

### Added

- `MotionKind::LineStart` (`0` / `<Home>`), `MotionKind::FirstNonBlank` (`^`),
  `MotionKind::LineEnd` (`$` / `<End>`) — the 3 Phase 3c line-anchored motion
  variants added to `crates/hjkl-vim/src/motion.rs`. Enum remains
  `#[non_exhaustive]`; consumers on hjkl-vim 0.13.x must bump to 0.14 to handle
  these new arms.

## [0.13.0] - 2026-05-13

### Added

- `MotionKind::WordForward` (`w`), `MotionKind::BigWordForward` (`W`),
  `MotionKind::WordBackward` (`b`), `MotionKind::BigWordBackward` (`B`),
  `MotionKind::WordEnd` (`e`), `MotionKind::BigWordEnd` (`E`) — the 6 Phase 3b
  word-motion variants added to `crates/hjkl-vim/src/motion.rs`. Enum remains
  `#[non_exhaustive]`; consumers on hjkl-vim 0.12.x must bump to 0.13 to handle
  these new arms.

## [0.12.0] - 2026-05-13

### Added

- `MotionKind` enum (`crates/hjkl-vim/src/motion.rs`, re-exported from
  `lib.rs`): names the 6 Phase 3a cursor motions so the host keymap path can
  dispatch them without depending on engine internals. Marked
  `#[non_exhaustive]` so later phases add variants without a major bump on the
  `hjkl-vim` side. Initial variants: `CharLeft` (`h` / `<BS>`), `CharRight` (`l`
  / `<Space>`), `LineDown` (`j`), `LineUp` (`k`), `FirstNonBlankDown` (`+`),
  `FirstNonBlankUp` (`-`).

## [0.11.0] - 2026-05-13

### Added

- `PendingState::SelectRegister` — reducer sub-state for the `"<reg>` chord in
  Normal mode. Hosts set this variant after intercepting `"`; `step` routes the
  next `Key::Char(ch)` to `EngineCmd::SetPendingRegister { reg: ch }` or cancels
  on `Key::Esc` / any non-char key (mirrors the `AfterG` arm). The char is
  passed through unvalidated — engine validates against `[a-zA-Z0-9"+*_]`.
- `EngineCmd::SetPendingRegister { reg: char }` — emitted by the
  `SelectRegister` reducer arm; host calls `Editor::set_pending_register(reg)`
  on receipt. Engine validates `reg` and sets `vim.pending_register` if valid;
  invalid chars are silently ignored (no-op, matching the engine FSM behaviour).

## [0.10.0] - 2026-05-13

### Added

- `OperatorKind::Uppercase`, `OperatorKind::Lowercase`,
  `OperatorKind::ToggleCase`, `OperatorKind::Reflow` variants — chord-initiated
  case/reflow operators bridged through the reducer in Phase 2c-v. `Uppercase`
  maps to `gU`, `Lowercase` to `gu`, `ToggleCase` to `g~`, `Reflow` to `gq`.
- `OperatorKind::double_char` updated to cover all nine variants: the four new
  operators map to `'U'`, `'u'`, `'~'`, `'q'` respectively, so the `AfterOp`
  reducer's doubled-letter detection (`gUU`, `guu`, `g~~`, `gqq`) works
  automatically via the existing `ch == op.double_char()` check.

## [0.9.0] - 2026-05-13

### Added

- `PendingState::OpG { op, total_count }` — reducer sub-state reached from
  `AfterOp` when the operator key is followed by `g`. `total_count` is
  `count1.max(1) * inner_count.max(1)` folded at transition time. The next char
  is the g-chord second key (`g` for `gg` = file-top, `e` for `ge` =
  word-end-back, `E`, `j`, `k`, or the case-op doubled form `U`/`u`/`~`);
  `Key::Esc` or any non-char key cancels.
- `EngineCmd::ApplyOpG { op, ch, total_count }` — emitted by the `OpG` reducer
  arm when the second char arrives. Host calls the new `Editor::apply_op_g`
  method (hjkl-engine 0.5.16+). Unknown chars are passed through unvalidated;
  the engine treats them as a no-op.

### Removed

- `EngineCmd::EnterOpG { op, count1 }` — **breaking**. The `AfterOp` arm no
  longer emits this variant; it transitions to `PendingState::OpG` instead,
  keeping the g-chord second char in the reducer rather than handing control
  back to the engine FSM. Hosts must replace any `EnterOpG` match arm with
  `ApplyOpG`.

## [0.8.0] - 2026-05-13

### Added

- `PendingState::OpTextObj { op, total_count, inner }` — reducer sub-state
  reached from `AfterOp` when the operator key is followed by `i` or `a`.
  `total_count` is `count1.max(1) * inner_count.max(1)` folded at transition
  time. The next char is the text-object kind; `Key::Esc` or any non-char key
  cancels.
- `EngineCmd::ApplyOpTextObj { op, ch, inner, total_count }` — emitted by the
  `OpTextObj` reducer arm when the text-object char arrives. Host calls the new
  `Editor::apply_op_text_obj` method (hjkl-engine 0.5.15+).

### Removed

- `EngineCmd::EnterOpTextObj { op, count1, inner }` — **breaking**. The
  `AfterOp` arm no longer emits this variant; it transitions to
  `PendingState::OpTextObj` instead, keeping the text-object char in the reducer
  rather than handing control back to the engine FSM. Hosts must replace any
  `EnterOpTextObj` match arm with `ApplyOpTextObj`.

## [0.7.0] - 2026-05-13

### Added

- `PendingState::OpFind { op, total_count, forward, till }` — reducer sub-state
  reached from `AfterOp` when the operator key (`d`/`y`/`c`/`>`/`<`) is followed
  by `f`/`F`/`t`/`T`. `total_count` is `count1.max(1) * inner_count.max(1)`
  folded at transition time. The next char is the find target; `Key::Esc` or any
  non-char key cancels (vim's `f<Esc>` semantics).
- `EngineCmd::ApplyOpFind { op, ch, forward, till, total_count }` — emitted by
  the `OpFind` reducer arm when the find-target char arrives. Host calls the new
  `Editor::apply_op_find` method (hjkl-engine 0.5.14+). Engine builds
  `Motion::Find { ch, forward, till }` and applies the operator.

### Removed

- `EngineCmd::EnterOpFind { op, count1, forward, till }` — **breaking**. The
  `AfterOp` arm no longer emits this variant; it transitions to
  `PendingState::OpFind` instead, keeping the find-target char in the reducer
  rather than handing control back to the engine FSM. Hosts must replace any
  `EnterOpFind` match arm with `ApplyOpFind`.

## [0.6.0] - 2026-05-13

### Added

- `OperatorKind` enum — carries operator identity in the reducer without
  depending on `hjkl-engine`. Variants: `Delete`, `Yank`, `Change`, `Indent`,
  `Outdent`. Exported from crate root.
- `PendingState::AfterOp { op, count1, inner_count }` — pending variant for bare
  op-pending entered from Normal mode after `d` / `y` / `c` / `>` / `<`. The
  reducer owns both `count1` (prefix) and `inner_count` (post-operator digit
  accumulation); `total = count1.max(1) * inner_count.max(1)` is passed to the
  engine on completion. Vim quirk: bare `0` when `inner_count == 0` is the
  `LineStart` motion, not a digit.
- `EngineCmd::ApplyOpMotion { op, motion_key, total_count }` — emitted when the
  next char is any single-key motion; host calls `Editor::apply_op_motion`.
- `EngineCmd::ApplyOpDouble { op, total_count }` — emitted on doubled-letter
  line op (`dd` / `yy` / `cc` / `>>` / `<<`); host calls
  `Editor::apply_op_double`.
- `EngineCmd::EnterOpTextObj { op, count1, inner }` — emitted on `i` / `a`; host
  calls `Editor::enter_op_text_obj` to set `Pending::OpTextObj`.
- `EngineCmd::EnterOpG { op, count1 }` — emitted on `g`; host calls
  `Editor::enter_op_g` to set `Pending::OpG`.
- `EngineCmd::EnterOpFind { op, count1, forward, till }` — emitted on `f` / `F`
  / `t` / `T`; host calls `Editor::enter_op_find` to set `Pending::OpFind`.

## [0.5.0] - 2026-05-13

### Added

- `PendingState::AfterZ { count }` — pending variant for the bare `z<x>` chord.
  Hosts set this variant after intercepting `z`; `step` routes the next
  `Key::Char(ch)` to `EngineCmd::AfterZChord { ch, count }` or cancels on
  `Key::Esc` / any non-char key (mirrors the `AfterG` arm).
- `EngineCmd::AfterZChord { ch, count }` — emitted by the `AfterZ` reducer arm;
  host calls `Editor::after_z(ch, count)` on receipt.

## [0.4.0] - 2026-05-13

### Added

- `PendingState::AfterG { count }` — pending variant for the bare `g<x>` chord.
  Hosts set this variant after intercepting `g`; `step` routes the next
  `Key::Char(ch)` to `EngineCmd::AfterGChord { ch, count }` or cancels on
  `Key::Esc` / any non-char key (mirrors the `Find` arm).
- `EngineCmd::AfterGChord { ch, count }` — emitted by the `AfterG` reducer arm;
  host calls `Editor::after_g(ch, count)` on receipt.

## [0.3.0] - 2026-05-13

### Added

- `PendingState::Find { count, forward, till }` — pending variant for `f<x>` /
  `F<x>` / `t<x>` / `T<x>` bare find chords. Hosts set this variant; `step`
  routes the next `Key::Char` to `EngineCmd::FindChar` or cancels on `Key::Esc`
  / any non-char key.
- `EngineCmd::FindChar { ch, forward, till, count }` — emitted by the `Find`
  reducer arm; host calls `Editor::find_char` on receipt.

## [0.2.0] - 2026-05-12

### Added

- `PendingState` enum: app-level chord accumulator; initial variant
  `Replace { count }`.
- `Outcome` enum: reducer result — `Wait`, `Commit`, `Cancel`, `Forward`.
- `Key` enum: crossterm-free key representation for the pending-state reducer.
- `step(state, key) -> Outcome`: pure reducer driving `PendingState`
  transitions.
- `EngineCmd` enum: controller commands emitted to the host; initial variant
  `ReplaceChar { ch, count }`.
- Re-exports at crate root: `PendingState`, `Outcome`, `Key`, `step`,
  `EngineCmd`.

## [0.1.0] - 2026-05-12

### Added

- Initial release: `Mode` enum extracted from `apps/hjkl::keymap::HjklMode`.
  Future phases will land the vim FSM here.

[Unreleased]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.23.0...HEAD
[0.23.2]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.23.1...v0.23.2
[0.23.1]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.23.0...v0.23.1
[0.23.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.22.0...v0.23.0
[0.22.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.21.0...v0.22.0
[0.21.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.20.0...v0.21.0
[0.20.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.19.0...v0.20.0
[0.19.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.18.1...v0.19.0
[0.18.1]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.18.0...v0.18.1
[0.18.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.17.0...v0.18.0
[0.17.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.16.0...v0.17.0
[0.16.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.15.0...v0.16.0
[0.15.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.14.0...v0.15.0
[0.14.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.13.0...v0.14.0
[0.13.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.12.0...v0.13.0
[0.12.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.11.0...v0.12.0
[0.11.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.10.0...v0.11.0
[0.10.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.9.0...v0.10.0
[0.9.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.8.0...v0.9.0
[0.8.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/kryptic-sh/hjkl-vim/releases/tag/v0.1.0
