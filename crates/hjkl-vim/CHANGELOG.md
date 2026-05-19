# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
