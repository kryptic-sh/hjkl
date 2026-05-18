# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). This
project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Removed

- Dropped the `ratatui` feature and all ratatui adapter APIs
  (`intern_ratatui_style`, `install_ratatui_syntax_spans`,
  `ratatui_style_table`, plus the `engine_style_to_ratatui` /
  `ratatui_style_to_engine` free fns, and the `cursor_screen_pos_in_rect`
  convenience wrapper). They now live in the new `hjkl-engine-tui` crate. Phase
  2 of #162.

## [0.11.4] - 2026-05-18

### Fixed

- `Query::line` trait method now returns `String` (owned) instead of `&str`. All
  call sites updated: `buf_helpers::buf_line`, `buf_line_chars`,
  `buf_line_bytes`, `read_line` / `read_line_opt` in `motions.rs`, and every
  place that forwarded the result to `&str`-expecting APIs (`push_str`,
  `find_at`, `find_iter`, `byte_offset`, `wrap_segments`). This is the
  engine-side complement to the `hjkl-buffer` 0.8.1 fix (closes #158: latent
  multi-view UB in `Buffer::lines` / `Buffer::line`).
- `buffer_impl::RopeBuffer::slice` single-line path now returns `Cow::Owned`
  (was `Cow::Borrowed` from a temporary `String`). Test
  `query_slice_single_line_borrows` updated to reflect this.
- Mock `Query` impls in `buffer_impl` and `motions` test modules updated to
  return `String` from `fn line`.

## [0.11.1] - 2026-05-18

### Fixed

- `jump_cursor` now resets `sticky_col` so subsequent `j`/`k` motions preserve
  the jumped-to column instead of snapping back to the pre-jump column.

## [0.11.0] - 2026-05-17

### Changed

- `default` features now `["serde"]` only — `crossterm` and `ratatui` dropped
  from defaults (#99). Consumers that relied on defaults must add
  `features = ["crossterm", "ratatui"]` (or `features = ["crossterm"]` /
  `features = ["ratatui"]`) explicitly.

## [0.10.0] - 2026-05-17

### Added

- `Editor::lnum_width() -> u16` — public method returning the line-number gutter
  width for the current buffer and settings. Matches the width reserved by
  `Editor::cursor_screen_pos`; returns `0` when both `number` and
  `relativenumber` are off. Consumers (apps/hjkl, hjkl-picker-tui) can now call
  this instead of maintaining a local copy of the formula (#96).

### Changed

- `Editor::cursor_screen_pos` now delegates to `Editor::lnum_width()` internally
  — single source of truth for the gutter-width formula.

## [0.9.1] - 2026-05-16

### Changed

- `hjkl-buffer` dep pin bumped from `"0.6"` to `"0.7"` to keep engine and
  downstream consumers aligned on the same buffer minor — required because 0.x
  caret-minor pins are semver-incompatible.

## [0.9.0] - 2026-05-16

### Added

- `Editor::set_content_undoable(text)` — replaces the entire buffer contents as
  a single undoable operation, preserving undo history for whole-buffer replace
  workflows (e.g. format-on-save via hjkl-mangler).
- `Editor::range_for_op_motion(motion)`, `range_for_op_motion_g(motion)`,
  `range_for_op_motion_text_obj(obj)` — query helpers that return the byte range
  an operator would act on without applying it; used by hjkl-mangler for
  partial-format dispatch (#119).
- `Editor::scroll_left(cols)` / `scroll_right(cols)` — expose horizontal
  viewport scrolling as first-class API calls, mirroring the existing
  `scroll_up` / `scroll_down`.
- `Editor::last_indent_range()` — returns the byte range of the most recent
  auto-indent insertion so hosts can highlight or override it in the UI.
- `OperatorKind::AutoIndent` variant — auto-indent is now a proper operator in
  the FSM; `Editor::auto_indent_range()` returns the affected line span.
- `Editor::cursor_screen_pos()` now accounts for sign-column and fold-column
  widths when computing the terminal column, fixing off-by-N cursor placement
  when either column is enabled.
- Auto-indent chain continuation: lines beginning with `.foo()` (method-chain
  style) are recognised as continuation lines and receive the same indent level
  as the preceding chain link rather than the base indent.

### Fixed

- `Editor::mouse_click_doc(row, col)` now performs mode-aware EOL clamping:
  Normal-mode clicks clamp `col` to `line.chars().count().saturating_sub(1)`,
  Insert-mode allows `col == line.chars().count()` (past-end sentinel). Also
  resets `sticky_col` on click so subsequent `j`/`k` motions start from the
  clicked column, not a stale remembered column.
- Auto-indent bracket scan no longer misidentifies comment tokens or
  string/character literals as bracket openers, preventing spurious extra-indent
  on lines like `// {` or `let s = "{";`.

## [0.8.0] - 2026-05-16

### Breaking

Phase 1 of kryptic-sh/hjkl#114 — mouse support. The old u16-flavoured,
terminal-layout-baking mouse helpers have been removed. Hosts must now perform
their own cell→doc or pixel→doc translation and call the new doc-coord
primitives. This lets the upcoming `apps/hjkl-gui` (floem, pixel geometry) plug
in without going through terminal assumptions.

**Deleted**:

- `Editor::mouse_to_doc_pos_xy(area_x, area_y, col, row)` (private fn, now gone
  — listed for docs.rs cross-reference)
- `Editor::mouse_click(area_x: u16, area_y: u16, col: u16, row: u16)` — baked in
  1-row tab bar + lnum-width gutter offset; not host-agnostic.
- `Editor::mouse_click_in_rect(area: Rect, col: u16, row: u16)` — Rect-flavoured
  wrapper of the above.
- `Editor::mouse_extend_drag(area_x: u16, area_y: u16, col: u16, row: u16)` —
  same layout assumption.
- `Editor::mouse_extend_drag_in_rect(area: Rect, col: u16, row: u16)` — wrapper.
- `VimState::enter_visual(anchor)` — was only used by the deleted
  `mouse_begin_drag` internals; callers should use `enter_visual_char_bridge` or
  the public `Editor::enter_visual_char`.

**Added** (host-agnostic doc-coord surface):

- `Editor::set_cursor_doc(row, col)` — clamp and place cursor in doc-space.
  `col` may equal `line.chars().count()` (Insert-mode sentinel).
- `Editor::mouse_click_doc(row, col)` — exits Visual, breaks insert-mode undo
  group (Vim parity), then calls `set_cursor_doc`. Host does cell→doc first.
- `Editor::mouse_begin_drag()` — unchanged semantics, now calls
  `enter_visual_char_bridge` so `vim_mode()` reflects `Visual` immediately.
- `Editor::mouse_extend_drag_doc(row, col)` — moves live cursor in Visual;
  anchor stays where `mouse_begin_drag` placed it.

### Migration

```rust
// Before (TUI host):
editor.mouse_click(area.x, area.y, col, row);
editor.mouse_extend_drag(area.x, area.y, col, row);

// After (TUI host — translate first):
let (doc_row, doc_col) = cell_to_doc(&app, win_id, outer, cell_x, cell_y)?;
editor.mouse_click_doc(doc_row, doc_col);
editor.mouse_extend_drag_doc(doc_row, doc_col);
```

## [0.7.1] - 2026-05-15

### Fixed

- `editor.rs` now gates `use crate::input::Key` behind
  `#[cfg(feature = "crossterm")]` so `cargo build --no-default-features` builds
  without an `unused import: Key` warning. With `-D warnings` in CI the bare
  import would fail the standalone hjkl-engine build path used by
  `default-features = false` consumers (e.g. apps/hjkl-gui).

## [0.7.0] - 2026-05-15

### Breaking

Phase 6.6 of kryptic-sh/hjkl#72 — the vim FSM has been physically extracted from
`hjkl-engine` into the `hjkl-vim` crate. `hjkl-engine` is now a pure controller:
`Editor` primitives + `VimState` + the new `begin_step` / `end_step`
prelude/epilogue API. The FSM dispatch lives only in `hjkl-vim`.

Deleted from `Editor`:

- `Editor::handle_key` (crossterm `KeyEvent` shim) — use `hjkl_vim::handle_key`
- `Editor::step_input` (was `#[deprecated]`) — use `hjkl_vim::dispatch_input`
- `Editor::step_input_raw` (was `#[doc(hidden)]`) — use
  `hjkl_vim::dispatch_input`
- `Editor::feed_input` (PlannedInput dispatch) — use `hjkl_vim::feed_input`

Deleted from module surface:

- `pub fn hjkl_engine::step` — use `hjkl_vim::dispatch_input`
- `pub fn vim::step` — use `hjkl_vim::dispatch_input`
- Inline FSM bodies in `vim.rs`: `step_insert` + `handle_insert_key`,
  `step_normal` + `handle_normal_only`, `step_search_prompt`, plus 14 `handle_*`
  dispatch helpers and 4 FSM-only utilities (`take_count`, `char_to_operator`,
  `visual_operator`, `find_entry`). Total ~1193 LOC.

### Added

- `Editor::begin_step(input) -> Result<StepBookkeeping, bool>` — pre-FSM
  bookkeeping: input timestamps, macro-stop early-return, pre-step snapshots.
  `Ok(bookkeeping)` proceeds to FSM dispatch; `Err(consumed)` indicates the
  prelude already handled the input (macro-stop chord).
- `Editor::end_step(input, bookkeeping, consumed) -> bool` — post-FSM
  bookkeeping: visual-mode `<` / `>` mark snapshot, one-shot-normal handling,
  `sync_buffer_content_from_textarea`, `ensure_cursor_in_scrolloff`, recorder
  hook, `current_mode` sync. Returns consumed pass-through.
- `pub struct StepBookkeeping` — opaque token round-tripped between `begin_step`
  and `end_step`.
- `pub fn hjkl_engine::input::from_planned(PlannedInput) -> Option<Input>` —
  pure decode helper extracted from the old `Editor::feed_input` body. Used by
  `hjkl_vim::feed_input`. Also re-exported as
  `hjkl_engine::decode_planned_input`.
- `pub fn hjkl_engine::crossterm_to_input(KeyEvent) -> Input` — visibility
  widened from `pub(super)` for `hjkl_vim::handle_key`.
- `Editor::emit_cursor_shape_if_changed` — visibility widened to `pub` so
  `hjkl_vim::handle_key` and `hjkl_vim::feed_input` can run the same post-FSM
  cursor-shape emit the engine used to do internally.
- 84 new pub `Editor::*` accessor methods covering every FSM-relevant `VimState`
  field (pending chord, count, mode, replaying flag, last find, last change,
  last visual, last input pos / edit pos, insert session, visual anchors, block
  vcol, yank linewise, macro recording target, recording keys, search prompt +
  history + last pattern + last direction, jump back / fwd lists, change list,
  viewport pinned, etc.). Phase 6.6b boundary so the relocated FSM in hjkl-vim
  reads state only through public API.
- 36 `VimState` fields promoted from private to `pub` (round-trip APIs prefer
  the accessors but direct access is supported for test setups).
- 6 internal types promoted to `pub` and re-exported from the crate root:
  `Pending`, `LastChange`, `InsertEntry`, `InsertReason`, `InsertSession`,
  `LastVisual`.
- `pub use vim::Mode as FsmMode` — re-export needed by `hjkl-vim::normal`.
- `pub use vim::{Motion, op_is_change, parse_motion}` — pure helpers used by the
  relocated FSM.
- 13 normal-mode shared helpers promoted from private to pub `Editor::*`
  methods: `apply_op_with_motion_direct`, `adjust_number`, `enter_search`,
  `visual_block_insert_at_left`, `visual_block_append_at_right`,
  `execute_motion`, `update_block_vcol`, `apply_visual_operator`,
  `replace_block_char`, `visual_text_obj_extend`, `visual_block_bounds`,
  `line_char_count`, `is_visual`.

### Changed

- `Editor` is no longer the canonical input entry point — drive the FSM via
  `hjkl_vim::dispatch_input` / `hjkl_vim::handle_key` / `hjkl_vim::feed_input`.
  The engine still owns all state and primitives.
- `vim.rs` shed ~17% LOC (7189 → 5996) as the FSM dispatch tree moved to
  hjkl-vim. The remaining surface is Editor primitives, `*_bridge` helpers
  called by those primitives, and the begin_step/end_step prelude/epilogue.

### Fixed

- `move_viewport_middle` underflow at degenerate viewports (motions.rs:502 used
  naive subtraction; now `saturating_sub`). Exposed by `proptest_fsm` during the
  6.6 extraction.

### Migration

Replace:

```rust
// Before:
editor.handle_key(crossterm_key);
editor.step_input(input);
editor.feed_input(planned);
hjkl_engine::step(&mut editor, input);

// After:
hjkl_vim::handle_key(&mut editor, crossterm_key);
hjkl_vim::dispatch_input(&mut editor, input);
hjkl_vim::feed_input(&mut editor, planned);
hjkl_vim::dispatch_input(&mut editor, input);
```

## [0.6.9] - 2026-05-14

### Added

Phase 6.1 of kryptic-sh/hjkl#72 (engine slimdown, chunk 1 of 8). Promote
`handle_insert_key`'s arms to public `Editor::*` controller methods so
insert-mode can be driven without going through the FSM. The FSM still works
(internal `step_insert` now delegates to the new primitives) — this release is
purely additive.

16 new public methods on `Editor`:

- `insert_char(ch)` — insert char; overstrike in Replace mode; smartindent
  dedent on closing brackets
- `insert_newline()` — split line + autoindent/smartindent prefix
- `insert_tab()` — tab or spaces to next softtabstop boundary
- `insert_backspace()` — soft-tab run delete; join prev line at col 0
- `insert_delete()` — delete under cursor; join next line at EOL
- `insert_arrow(InsertDir)` — L/R/U/D cursor move + `break_undo_group`
- `insert_home()` / `insert_end()` — move to line start/end + `break_undo_group`
- `insert_pageup(viewport_h)` / `insert_pagedown(viewport_h)` — page scroll
- `insert_ctrl_w()` — delete to previous word start (vim `b`-motion)
- `insert_ctrl_u()` — delete to line start (no-op at col 0)
- `insert_ctrl_h()` — single backspace (Ctrl-H alias)
- `insert_ctrl_o_arm()` — arm one-shot Normal mode flag
- `insert_ctrl_r_arm()` — arm register-paste selector
- `insert_ctrl_t()` / `insert_ctrl_d()` — indent/outdent current line by
  `shiftwidth`
- `insert_paste_register(reg)` — paste register text at cursor
- `leave_insert_to_normal()` — Esc handler: `finish_insert_session` + step
  cursor 1 left + record `gi` target + update sticky col

New `vim::InsertDir` enum re-exported from `lib.rs` for `insert_arrow`.

Each primitive runs through `Editor::mutate_edit` so dirty / undo /
`InsertSession` bookkeeping fires correctly (verified for dot-repeat
compatibility).

31 new unit tests in `editor.rs` (722 total, up from 691) covering each
primitive plus edge cases: softtabstop run-delete, join-up at col 0, expandtab
vs real tab, Esc step-back col, replace-mode overstrike, buffer-boundary no-ops.

## [0.6.8] - 2026-05-14

### Added

- `Editor::replay_last_change(count)` — public controller method for the
  dot-repeat (`.`) command. Delegates to `vim::replay_last_change`. Phase 5c of
  kryptic-sh/hjkl#71: lets the app dispatch `EngineCmd::DotRepeat` from the
  hjkl-vim reducer instead of routing `.` through the engine FSM. Engine FSM `.`
  arm stays for macro-replay defensive coverage; `LastChange` storage stays on
  the engine.
- Public macro controller methods (Phase 5b of kryptic-sh/hjkl#71):
  `start_macro_record(register)`, `stop_macro_record()`, `is_recording_macro()`,
  `is_replaying_macro()`, `play_macro(register, count)`, `end_macro_replay()`,
  `record_input(key)`. Lets the app drive macro record/playback through
  `route_chord_key` rather than the engine FSM. Engine FSM macro arms
  (stop-on-bare-q, recorder hook, record/play handlers) stay for defensive
  coverage; Phase 6 deletes them.
- 8 unit tests covering the macro controller surface.

### Changed

- **`cursorline` option default flipped from `false` → `true`.** New `Editor`
  instances render the cursor row highlighted by default, matching the most
  common user expectation. Existing config files / `:set nocursorline` still
  toggle off as before. Behavior change only — public API unchanged.

## [0.6.7] - 2026-05-13

### Added

- `Editor::set_mark_at_cursor(ch)` — public controller method. Validates `ch`
  (`a`–`z` / `A`–`Z`) and records the current cursor position under that mark
  name. Invalid chars are silently ignored (no-op). Promoted so the hjkl-vim
  `PendingState::SetMark` reducer can dispatch `EngineCmd::SetMark` without
  re-entering the engine FSM. Phase 5a of kryptic-sh/hjkl#71.
- `Editor::goto_mark_line(ch)` — public controller method. Validates `ch` (same
  set as vim's `'<ch>` command), resolves the target position, and jumps the
  cursor linewise (row only; col snaps to first non-blank). Pushes the pre-jump
  position onto the jumplist if the cursor actually moved. Invalid or unset
  marks are silently ignored (no-op). Phase 5a of kryptic-sh/hjkl#71.
- `Editor::goto_mark_char(ch)` — public controller method. Same as
  `goto_mark_line` but jumps charwise (exact row + col). Phase 5a of
  kryptic-sh/hjkl#71.
- `vim::set_mark_at_cursor` / `vim::goto_mark` — `pub(crate)` controller helpers
  that back the three public methods. `handle_set_mark` and `handle_goto_mark`
  now delegate to these helpers to avoid logic duplication, mirroring the
  `handle_select_register` → `Editor::set_pending_register` delegation pattern
  from 0.5.14–0.5.16.
- Bumped `hjkl-vim` dependency from `"0.17"` to `"0.18"` in `Cargo.toml`.
- 9 controller-level tests in `crates/hjkl-engine/src/editor.rs` (Phase 5a):
  `set_mark_at_cursor_alphabetic_records`,
  `set_mark_at_cursor_invalid_char_no_op`,
  `set_mark_at_cursor_special_left_bracket`,
  `goto_mark_line_jumps_to_first_non_blank`, `goto_mark_line_unset_mark_no_op`,
  `goto_mark_line_invalid_char_no_op`, `goto_mark_char_jumps_to_exact_pos`,
  `goto_mark_char_unset_mark_no_op`, `goto_mark_char_invalid_char_no_op`.

## [0.6.6] - 2026-05-13

### Added

- `apply_motion_kind` extended with `MotionKind::FindRepeat` and
  `MotionKind::FindRepeatReverse` arms (Phase 3e of kryptic-sh/hjkl#69): `;`
  routes through
  `execute_motion(ed, Motion::FindRepeat { reverse: false }, count)` and `,`
  through `Motion::FindRepeat { reverse: true }`. Engine handles the "no prior
  find" no-op internally via `ed.vim.last_find`. Engine FSM arms for `;` and `,`
  in `parse_motion` are kept intact for macro-replay.
- `apply_motion_kind` extended with `MotionKind::BracketMatch` arm (Phase 3f of
  kryptic-sh/hjkl#69): `%` routes through
  `execute_motion(ed, Motion::MatchBracket, count)`. Engine handles the no-match
  case as a no-op (cursor stays). Engine FSM arm for `%` in `parse_motion` is
  kept intact for macro-replay.
- `apply_motion_kind` extended with 7 new MotionKind arms (Phase 3g of
  kryptic-sh/hjkl#69): `ViewportTop` (`H`), `ViewportMiddle` (`M`),
  `ViewportBottom` (`L`) route through `execute_motion` to
  `Motion::ViewportTop/Middle/Bottom` respectively (engine FSM `parse_motion`
  arms preserved for macro-replay). `HalfPageDown` (`<C-d>`), `HalfPageUp`
  (`<C-u>`), `FullPageDown` (`<C-f>`), `FullPageUp` (`<C-b>`) call
  `scroll_cursor_rows` directly (same expressions as the FSM Ctrl arm in
  `step_normal`) without adding new `Motion` enum variants — approach (a) per
  spec. Engine FSM Ctrl-d/u/f/b arms preserved for macro-replay.
- Bumped `hjkl-vim` dependency from `"0.15"` to `"0.17"` in `Cargo.toml`
  (carries Phase 3e `FindRepeat`/`FindRepeatReverse` and Phase 3f
  `BracketMatch`; skips the never-published 0.6.5 version).
- 4 controller-level tests in `crates/hjkl-engine/src/editor.rs` (Phase 3e):
  `find_repeat_after_f_finds_next_occurrence`,
  `find_repeat_reverse_after_f_finds_prev_occurrence`,
  `find_repeat_with_no_prior_find_is_noop`,
  `find_repeat_with_count_advances_count_times`.
- 3 controller-level tests in `crates/hjkl-engine/src/editor.rs` (Phase 3f):
  `bracket_match_jumps_to_matching_close_paren`,
  `bracket_match_jumps_to_matching_open_paren`,
  `bracket_match_with_no_match_on_line_is_noop_or_engine_behaviour`.
- 8 controller-level tests in `crates/hjkl-engine/src/editor.rs` (Phase 3g):
  `viewport_top_lands_on_first_visible_row`,
  `viewport_top_with_count_offsets_down`,
  `viewport_middle_lands_on_middle_visible_row`,
  `viewport_bottom_lands_on_last_visible_row`,
  `half_page_down_moves_cursor_by_half_window`,
  `half_page_up_moves_cursor_by_half_window_reverse`,
  `full_page_down_moves_cursor_by_full_window`,
  `full_page_up_moves_cursor_by_full_window_reverse`.

## [0.6.4] - 2026-05-13

### Added

- `apply_motion_kind` extended with `MotionKind::GotoLine` arm (Phase 3d of
  kryptic-sh/hjkl#69): `G` routes through
  `execute_motion(ed, Motion::FileBottom, count)`. Count convention:
  `apply_motion_kind` normalises raw count to `count.max(1)`; the `FileBottom`
  execution arm maps `count <= 1` → `move_bottom(0)` (last content row) and
  `count > 1` → `move_bottom(count)` (1-based line N, clamped). Engine FSM arm
  for `G` in `parse_motion` is kept intact for macro-replay defensive coverage.
- Bumped `hjkl-vim` dependency from `"0.14"` to `"0.15"` in `Cargo.toml`.
- 3 controller-level tests in `crates/hjkl-engine/src/editor.rs` covering: bare
  `G` (count=1 → last line), `5G` (count=5 → row 4), and `100G` on a 3-line
  buffer (clamps to last content row).

## [0.6.3] - 2026-05-13

### Added

- `apply_motion_kind` extended with 3 new `MotionKind` arms (Phase 3c of
  kryptic-sh/hjkl#69): `LineStart` (`0` / `<Home>`), `FirstNonBlank` (`^`),
  `LineEnd` (`$` / `<End>`). Each routes through `execute_motion` to the
  existing `Motion::LineStart` / `Motion::FirstNonBlank` / `Motion::LineEnd`
  primitives so cursor, sticky column, scroll, and sync semantics are identical
  to the engine FSM path. Engine FSM arms for `0`/`^`/`$`/`<Home>`/`<End>` are
  kept intact for macro-replay defensive coverage.
- Bumped `hjkl-vim` dependency from `"0.13"` to `"0.14"` in `Cargo.toml`.
- 6 controller-level tests in `crates/hjkl-engine/src/editor.rs` covering each
  of the 3 new variants (including edge cases: line start from col 0, first
  non-blank on all-whitespace line, line end on empty line).

## [0.6.2] - 2026-05-13

### Added

- `apply_motion_kind` extended with 6 new `MotionKind` arms (Phase 3b of
  kryptic-sh/hjkl#69): `WordForward` (`w`), `BigWordForward` (`W`),
  `WordBackward` (`b`), `BigWordBackward` (`B`), `WordEnd` (`e`), `BigWordEnd`
  (`E`). Each routes through `execute_motion` to the existing `Motion::WordFwd`
  / `Motion::BigWordFwd` / `Motion::WordBack` / `Motion::BigWordBack` /
  `Motion::WordEnd` / `Motion::BigWordEnd` primitives so cursor, sticky column,
  scroll, and sync semantics are identical to the engine FSM path. Engine FSM
  arms for `w`/`W`/`b`/`B`/`e`/`E` are kept intact for macro-replay defensive
  coverage.
- Bumped `hjkl-vim` dependency from `"0.12"` to `"0.13"` in `Cargo.toml`.
- 12 controller-level tests in `crates/hjkl-engine/src/editor.rs` covering each
  of the 6 new variants with count=1 and count>1.

## [0.6.1] - 2026-05-13

### Added

- `Editor::apply_motion(kind: hjkl_vim::MotionKind, count: usize)` — public
  controller entry point for the keymap-layer motion path (Phase 3a of
  kryptic-sh/hjkl#69). Maps the 6 `MotionKind` variants introduced in hjkl-vim
  0.12.0 to the engine's internal motion primitives via a new
  `pub(crate) fn apply_motion_kind` helper in `vim.rs`. Cursor, sticky column,
  scroll, and sync semantics are identical to the engine FSM path. Engine FSM
  arms for `h`/`j`/`k`/`l`/`<BS>`/`<Space>`/`+`/`-` are kept intact for
  macro-replay defensive coverage.
- `hjkl-vim = "0.12"` added to `[dependencies]` in `Cargo.toml`; the workspace
  `[patch.crates-io]` resolves it to the local submodule path.

## [0.6.0] - 2026-05-13

### Removed (breaking)

- `Editor::enter_op_text_obj`, `Editor::enter_op_g`, `Editor::enter_op_find` —
  transitional controller methods added in 0.5.12 so the hjkl-vim
  `PendingState::AfterOp` reducer could ask the engine to set its op-pending
  sub-state. After Phase 2c-ii/iii/iv of kryptic-sh/hjkl#62, the reducer owns
  the full op-pending FSM via `PendingState::OpFind` / `OpTextObj` / `OpG`
  internal transitions and commits `ApplyOpFind` / `ApplyOpTextObj` / `ApplyOpG`
  directly — these enter-helpers are dead from the app side. The internal
  `vim::enter_op_*` `pub(crate)` helpers were removed alongside. Engine FSM
  macro-replay paths set `Pending::Op*` fields directly without going through
  these helpers, so replay is unaffected.

## [0.5.17] - 2026-05-13

### Added

- `Editor::set_pending_register(reg: char)` — public controller entry point:
  validates `reg` against `[a-zA-Z0-9"+*_]` and sets `vim.pending_register` if
  valid; invalid chars are silently ignored (no-op), matching the engine FSM
  behaviour. Allows the hjkl-vim `PendingState::SelectRegister` reducer to
  dispatch `EngineCmd::SetPendingRegister` without re-entering the engine FSM.
- `handle_select_register` now delegates to `Editor::set_pending_register` to
  eliminate validation logic duplication (mirrors the extraction pattern
  established by `apply_op_find_motion` / `apply_op_text_obj_inner` in
  0.5.14–0.5.15). The engine FSM path (`Pending::SelectRegister`) stays intact
  for macro-replay defensive coverage.

## [0.5.16] - 2026-05-13

### Added

- `Editor::apply_op_g(op, ch, total_count)` — public controller entry point:
  applies operator over a g-chord motion or case-op linewise form (`dgg` / `dge`
  / `dgE` / `dgj` / `dgk` / `gUgU` etc.). If `op` is
  Uppercase/Lowercase/ToggleCase and `ch` matches the op's letter (`U`/`u`/`~`),
  executes the linewise case-op. Otherwise maps `ch` to a motion (`g`→`FileTop`,
  `e`→`WordEndBack`, `E`→`BigWordEndBack`, `j`→`ScreenDown`, `k`→`ScreenUp`);
  unknown chars are silently ignored (no-op). Updates `last_change` for
  dot-repeat when `op` is a change operator.
- `pub(crate) fn apply_op_g_inner` in `vim.rs` — shared implementation called by
  both `Editor::apply_op_g` (reducer path) and `handle_op_after_g` (engine FSM
  chord-init path), eliminating logic duplication. Mirrors the extraction
  pattern established by `apply_op_text_obj_inner` in 0.5.15.

## [0.5.15] - 2026-05-13

### Added

- `Editor::apply_op_text_obj(op, ch, inner, total_count)` — public controller
  entry point: applies operator over a text-object range (`diw` / `daw` / `di"`
  etc.). Maps `ch` to a `TextObject` per the standard vim table, calls
  `apply_op_with_text_object`, and updates `last_change` when `op` is Change
  (dot-repeat). Unknown `ch` values are silently ignored (no-op). `total_count`
  is accepted for API symmetry with `apply_op_motion` / `apply_op_find` but is
  currently unused — text objects don't repeat in vim's current grammar.
- `pub(crate) fn apply_op_text_obj_inner` in `vim.rs` — shared implementation
  called by both `Editor::apply_op_text_obj` (reducer path) and
  `handle_text_object` (engine FSM chord-init path), eliminating logic
  duplication. Returns `false` on unknown `ch` so the FSM can decide how to
  handle it.

## [0.5.14] - 2026-05-13

### Added

- `Editor::apply_op_find(op, ch, forward, till, total_count)` — public
  controller entry point: applies operator over a find motion (`df<x>` / `dF<x>`
  / `dt<x>` / `dT<x>`). Builds `Motion::Find { ch, forward, till }`, applies via
  `apply_op_with_motion`, records `last_find` for `;` / `,` repeat, and updates
  `last_change` when `op` is Change (dot-repeat). `total_count` is the
  already-folded product of prefix and inner counts.
- `pub(crate) fn apply_op_find_motion` in `vim.rs` — shared implementation
  called by both `Editor::apply_op_find` (reducer path) and
  `handle_op_find_target` (engine FSM chord-init path), eliminating logic
  duplication.

## [0.5.13] - 2026-05-13

### Added

- Re-export `Operator` at crate root (`pub use vim::Operator`). The five
  controller methods added in 0.5.12 (`apply_op_motion`, `apply_op_double`,
  `enter_op_text_obj`, `enter_op_g`, `enter_op_find`) take `Operator` as a
  parameter, but 0.5.12 failed to re-export the type, making those methods
  unusable from downstream crates. This patch makes `hjkl_engine::Operator` a
  proper public API surface.

## [0.5.12] - 2026-05-13

### Added

- `Editor::apply_op_motion(op, motion_key, total_count)` — public controller
  entry point: applies operator over the motion identified by `motion_key` (a
  raw char, e.g. `'w'`, `'$'`). Engine resolves via `parse_motion`, applies the
  same vim quirks as `handle_after_op` (`cw` → `ce`, `FindRepeat` resolution,
  `last_find` / `last_change` update). No-op on unknown motion keys.
- `Editor::apply_op_double(op, total_count)` — public controller entry point:
  applies a doubled-letter line op (`dd` / `yy` / `cc` / `>>` / `<<`). Delegates
  to `execute_line_op` and records `last_change`.
- `Editor::enter_op_text_obj(op, count1, inner)` — sets `Pending::OpTextObj` so
  the engine FSM handles the next bracket/word key for text-object completion.
- `Editor::enter_op_g(op, count1)` — sets `Pending::OpG` so the engine FSM
  handles the next `g`-second char.
- `Editor::enter_op_find(op, count1, forward, till)` — sets `Pending::OpFind` so
  the engine FSM handles the find-target character.
- `pub(crate)` helpers `apply_op_motion_key`, `apply_op_double`,
  `enter_op_text_obj`, `enter_op_g`, `enter_op_find` in `vim.rs` — shared
  implementations called by both the new controller methods and (via
  refactoring) `handle_after_op`.

All five methods are promoted to the public surface in 0.5.12 so `hjkl-vim`'s
`PendingState::AfterOp` reducer can dispatch its `EngineCmd` variants without
re-entering the engine FSM.

## [0.5.11] - 2026-05-13

### Added

- `Editor::after_z(ch, count)` — public controller entry point for the bare
  `z<x>` chord. Delegates to the new `pub(crate) apply_after_z` helper that
  contains the full `handle_after_z` dispatch table (`zz`, `zt`, `zb`, `zo`,
  `zc`, `za`, `zR`, `zM`, `zE`, `zd`, `zf`). Enables hjkl-vim's
  `PendingState::AfterZ` reducer to dispatch `AfterZChord` without re-entering
  the engine FSM. The `zf` visual-selection branch reads `ed.vim.mode` and
  visual anchors internally so the host just calls `after_z('f', count)` after
  any mode transition.

## [0.5.10] - 2026-05-13

### Added

- `Editor::after_g(ch, count)` — public controller entry point for the bare
  `g<x>` chord. Delegates to the new `pub(crate) apply_after_g` helper that
  contains the full `handle_after_g` dispatch table (`gg`, `ge`, `gE`, `g_`,
  `gM`, `gv`, `gj`, `gk`, `gU`, `gu`, `g~`, `gq`, `gJ`, `gd`, `gi`, `g;`, `g,`,
  `g*`, `g#`). Enables hjkl-vim's `PendingState::AfterG` reducer to dispatch
  `AfterGChord` without re-entering the engine FSM.

## [0.5.9] - 2026-05-13

### Added

- `Editor::find_char(ch, forward, till, count)` — public controller entry point
  for bare `f<x>` / `F<x>` / `t<x>` / `T<x>` motions. Applies the motion via
  `execute_motion` and records `last_find` so `;` / `,` repeat work. Enables
  hjkl-vim's `PendingState::Find` reducer to dispatch `FindChar` without
  re-entering the engine FSM.

## [0.5.8] - 2026-05-13

### Fixed

- **Dot mark (`'.` / `` `. ``) records change-start, not post-insert cursor.**
  `Editor::mutate_edit` now captures the pre-edit cursor before applying the
  buffer edit and stores that in `vim.last_edit_pos`. Pre-0.5.8 the post-edit
  cursor was stored, causing `` `. `` after `iX<Esc>` to land one column past
  the insert start instead of on the change start. Matches vim's `:h '.` rule
  "the position where the last change was made". Fixes oracle case
  `mark_dot_jump_to_last_edit` (kryptic-sh/hjkl#83).

- **`100G` clamps to last content row on trailing-newline buffers.**
  `motions::move_bottom` now applies the same trailing-empty-row skip for
  counted `G` as for bare `G`. Pre-0.5.8 `(count-1).min(raw_last)` ignored the
  phantom row, landing on row 3 instead of row 2 for a 3-line buffer with a
  trailing newline. Fixes oracle case `count_100G_clamps_to_last_line`
  (kryptic-sh/hjkl#83).

- **`gi` moves to the last-insert position and enters insert mode.** Added a new
  field `VimState::last_insert_pos` that captures the pre-step-back cursor on
  every insert-mode exit (Esc). Added a `gi` arm in `handle_after_g` that jumps
  to `last_insert_pos` then calls `begin_insert`. Pre-0.5.8 `gi` was silently
  swallowed by the `g`-prefix handler and had no effect. Fixes oracle case
  `gi_resume_last_insert` (kryptic-sh/hjkl#83).

- **Visual-block `c<text><Esc>` cursor lands on last inserted char.** Introduced
  a new `InsertReason::BlockChange` variant (distinct from `BlockEdge` used by
  `I`/`A`) so `finish_insert_session` can advance the block-start-row cursor to
  `col + ins_chars` (pre-step-back) after block replication. The Esc step-back
  then places the cursor at `col + ins_chars - 1`, matching nvim. Pre-0.5.8 the
  cursor stayed at the block-start column. `I` and `A` retain their original
  cursor-at-col behaviour. Fixes oracle case `visual_block_jl_c_change_block`
  (kryptic-sh/hjkl#83).

- **`"_` (black-hole) register discards deletes without touching unnamed.**
  `handle_select_register` now accepts `'_'` as a valid register character.
  `Registers::record_delete` and `Registers::record_yank` short-circuit
  immediately when `target == Some('_')`, leaving `"`, `"0`, and the `"1`–`"9`
  ring untouched. Pre-0.5.8 `"_dw` fell through to the unnamed register (because
  `_` was not in the accepted set), corrupting the last yank and causing `p` to
  paste the deleted text instead of the prior yank. Fixes oracle case
  `register_blackhole_d` (kryptic-sh/hjkl#83).

## [0.5.7] - 2026-05-13

### Fixed

- `` `< `` / `` `> `` (and `'<` / `'>` linewise variants) now resolve correctly
  through `handle_goto_mark`. Pre-0.5.7 the marks were set by the visual-exit
  hook added in 0.5.3 but the goto-mark dispatcher didn't list `<` / `>` in its
  target match, so `` `< `` silently no-op'd. Surfaced by the oracle tier-2
  marks corpus. Bracket marks `[` / `]` were already wired through; this commit
  closes the same gap for visual marks.

## [0.5.6] - 2026-05-13

### Added

- Special marks `[` and `]` tracking last yank / change / paste bounds (vim
  `:h '[` / `:h ']`):
  - **Yank** (`y<motion>`, `yy`): `[` = first yanked char, `]` = last yanked
    char. Mode-aware: linewise snaps `[` to `(top_row, 0)` and `]` to
    `(bot_row, last_col)`; inclusive motions use `bot` directly; exclusive
    motions use `bot.col.saturating_sub(1)`.
  - **Delete** (`d<motion>`, `dd`): both `[` and `]` park at the post-delete
    cursor position (the join point where the deletion collapsed), matching
    vim's "both at cursor" rule.
  - **Change** (`c<motion>`, `cc`): `[` = start of changed range (set before the
    cut); `]` = cursor position when insert mode exits via Esc. If no text is
    typed, both marks coincide at the change start.
  - **Paste** (`p` / `P`): `[` = first inserted char, `]` = last inserted char.
    Linewise paste snaps to line edges; charwise uses the actual insertion
    bounds. When `count > 1`, marks reflect the final paste's bounds.
- `` `[ `` / `` `] `` backtick jumps now resolve `[` and `]` in
  `handle_goto_mark`.
- Backtick mark jumps (`` ` ``) are now accepted in Visual, VisualLine, and
  VisualBlock modes so the `` `[v`] `` re-select idiom works end-to-end.

## [0.5.5] - 2026-05-12

### Added

- `Editor::replace_char_at(ch, count)` — controller entry point for hjkl-vim's
  pending-state reducer. Cursor, undo, and count semantics match vim's `r<x>`:
  one undo snapshot, cursor lands on the last replaced char, stops at line end.
  Thin wrapper over the internal `replace_char` free fn, which is now
  `pub(crate)`.

## [0.5.4] - 2026-05-12

### Fixed

- Visual-exit `<` / `>` mark positions are now mode-aware:
  - **Visual** (charwise): position-ordered tuple comparison (unchanged).
  - **VisualLine**: snaps `<` to `(top_row, 0)` and `>` to `(bot_row, last_col)`
    — matches vim's `:h v_:` rule that linewise selections normalise column
    components to line edges.
  - **VisualBlock**: corners computed independently — `<` =
    `(min_row, min_col)`, `>` = `(max_row, max_col)`. Previously used tuple
    ordering, which mis-placed columns when the cursor moved left of the anchor
    (e.g. block growing leftward).
- Doesn't affect ex-range commands like `:'<,'>sort` (which only read the row
  component) but does fix `` ` < `` / `` ` > `` jumps and any consumer reading
  the marks as block corners.

## [0.5.3] - 2026-05-12

### Added

- Visual-exit now sets the `'<` and `'>` marks to the start and end (in position
  order, not selection order) of the last visual selection. Required for
  ex-range commands like `:'<,'>sort` to resolve their range. Applies to Visual,
  VisualLine, and VisualBlock modes.

## [0.5.2] - 2026-05-12

### Added

- `Editor::is_chord_pending() -> bool` — true while the engine is in any
  multi-key pending state (Replace / Find / OpFind / G / OpG / Op / OpTextObj /
  VisualTextObj / Z / SetMark / GotoMarkLine / GotoMarkChar / SelectRegister /
  RecordMacroTarget / PlayMacroTarget). Hosts use this to bypass their own chord
  dispatch and forward keys directly to the engine so in-flight commands like
  `r<x>` / `f<x>` / `m<a>` aren't interrupted.

## [0.5.1] - 2026-05-10

### Changed

- Bumped `hjkl-buffer` dep requirement from `^0.5` to `^0.6`.

## [0.5.0] - 2026-05-10

### Added

- Five new fields on `Options` (`src/types.rs`) and `Settings`
  (`src/editor.rs`):
  - `cursorline: bool` (default `false`, alias `cul`) — highlight the line the
    cursor is on.
  - `cursorcolumn: bool` (default `false`, alias `cuc`) — highlight the column
    the cursor is on.
  - `signcolumn: SignColumnMode` (default `Auto`, alias `scl`) — controls sign
    column visibility; variants: `No`, `Yes`, `Auto`.
  - `foldcolumn: u32` (default `0`, clamped `0..=12`, alias `fdc`) — width of
    the fold column.
  - `colorcolumn: String` (default `""`, alias `cc`) — comma-separated list of
    absolute column numbers to highlight.
- New public enum `SignColumnMode` with variants `No`, `Yes`, `Auto`; derives
  `serde::Serialize` / `Deserialize` when the `serde` feature is enabled.
- `set_by_name` and `get_by_name` honour every new alias and reject malformed
  values with `EngineError::Ex`.

### Changed

- Version bumped to **0.5.0** (minor) because adding public fields to
  non-`#[non_exhaustive]` structs (`Options`, `Settings`) is a breaking change
  for any downstream crate that constructs those structs with a literal struct
  expression. All existing field positions are preserved; only additive changes
  were made.

## [0.4.1] - 2026-05-06

### Added

- `Editor::ensure_cursor_in_scrolloff` promoted from `pub(crate)` to `pub` so
  hosts can reveal the cursor after non-engine-driven jumps (e.g. LSP `gd`
  goto-definition, `]d` diagnostic nav). Without this call the cursor lands on
  the right row but the viewport stays parked, leaving the cursor off- screen.
  Engine-driven motions still call it automatically end-of-step.
- `Settings.numberwidth` (default 4, range 1..=20) with `:set numberwidth=N` /
  `:set nuw=N` ex-command surface, matching vim's `'numberwidth'` option. Gutter
  width is now `max(numberwidth, digits+1)` instead of a fixed `digits+2`.
- Same field added to `Options` and wired through `settings_from_options`,
  `set_by_name`, `get_by_name`.

## [0.4.0] - 2026-05-06

### Added

- `Settings.number` and `Settings.relativenumber` boolean fields with `:set nu`
  / `nonu` / `rnu` / `nornu` / `nu!` / `rnu!` ex-command surface (and full
  `number` / `nonumber` / `relativenumber` / `norelativenumber` forms). `number`
  defaults to `true` to preserve the existing always-on gutter; `relativenumber`
  defaults to `false`.
- Same two fields added to `Options` and wired through `settings_from_options`.
- `cursor_screen_pos` and `mouse_to_doc_pos_xy` now honour `number` /
  `relativenumber` when computing the gutter offset, so the terminal cursor
  lands in the correct column when the gutter is suppressed.

## [0.3.8] - 2026-05-05

### Fixed

- `G` now lands on the last content-bearing line rather than the phantom empty
  row produced by a trailing newline in the buffer.
- `dd` on the last line clamps the cursor to the new last row instead of leaving
  it on the phantom empty row after deletion.
- `d$` leaves the cursor on the final character of the shortened line (col
  `n-1`) rather than one past it (col `n`).
- All charwise deletes (`d<motion>`, `da"`, `daB`, etc.) apply the normal-mode
  cursor clamp on return, preventing one-past-end col values.
- `x` and `X` now write the deleted characters to the unnamed register `"` so
  that `xp` correctly round-trips the deleted character.
- Undo clamps the restored cursor to the last valid normal-mode column, fixing
  the off-by-one after `a text<Esc>u` sequences.
- `da<quote>` eats the trailing whitespace after the closing delimiter (or
  leading whitespace if no trailing exists), matching vim's `:help text-objects`
  "around" rule and avoiding double-space residue.
- `daB` / `da{` cursor off-by-one fixed: cursor now lands on the last character
  of the line preceding the deleted block.
- `diB` / `di{` on a multi-line block now uses a linewise range over the
  interior lines, preserving the newlines adjacent to `{` and `}` instead of
  collapsing the block to a single line.

## [0.3.7] - 2026-05-05

### Added

- New public module `hjkl_engine::substitute` exposing `parse_substitute`,
  `apply_substitute`, `SubstituteCmd`, `SubstFlags`, `SubstituteOutcome`, and
  `SubstError`. These types support the `:[range]s/pattern/replacement/[flags]`
  ex-command surface in TUI hosts.
- `parse_substitute` parses the `/pattern/replacement/flags` tail (delimiter
  must be `/`; flags: `g`, `i`, `I`, `c`). Empty pattern returns `None` so the
  caller can fall back to `Editor::last_search`. Replacement supports `&` (whole
  match), `\1`…`\9` (capture groups), `\\` (literal backslash), `\&` (literal
  ampersand).
- `apply_substitute` applies a `SubstituteCmd` over a 0-based inclusive
  `RangeInclusive<u32>` of buffer lines. Handles case-sensitivity precedence
  (`I` > `i` > editor `ignore_case`), updates `Editor::set_last_search` on
  success, and returns a `SubstituteOutcome` with `replacements` and
  `lines_changed` counts.
- All new items are re-exported at the crate root.

## [0.3.6] - 2026-05-05

### Fixed

- `pos_at_byte` no longer panics when the requested byte index lands inside a
  multi-byte UTF-8 codepoint. The function now rounds down to the nearest char
  boundary so the returned `Pos` points at the column of the containing char.
  Caught by the cargo-fuzz `handle_key` target on a Cyrillic seed.

## [0.3.5] - 2026-05-05

### Added

- Re-export `decode_macro` at the crate root (`hjkl_engine::decode_macro`).
  Previously only reachable via the private `input` module. Lets external
  consumers parse vim-key strings (`<Esc>`, `<C-r>`, etc.) into `Input` events
  without depending on internal module paths.

## [0.3.4] - 2026-05-04

### Docs

- Internal CHANGELOG hygiene: backfilled missing release entries and added
  reference link definitions for all version headings. No functional changes.

## [0.3.3] - 2026-05-03

### Docs

- Dropped sealed / 14-method rhetoric from the README status section. Per the
  org's "no SPEC frozen claims" stance: the trait surface keeps growing with
  semver-respecting bumps — no value in pinning the count.

## [0.3.2] - 2026-05-03

### Removed

- `SPEC.md` deleted; rustdoc on [docs.rs](https://docs.rs/hjkl-engine) is now
  the canonical API reference. All in-source references to `SPEC.md` removed.

## [0.3.1] - 2026-04-30

### Changed

- Migrated `hjkl-engine` from the `kryptic-sh/hjkl` monorepo into its own
  repository
  ([kryptic-sh/hjkl-engine](https://github.com/kryptic-sh/hjkl-engine)) with
  full git history preserved.
- Relaxed inter-crate dependency requirements from `=0.3.0` to `0.3` (caret),
  matching the standard SemVer pattern for library dependencies.
- Bumped `ratatui` to 0.30 (was 0.29) and `crossterm` to 0.29 (was 0.28).

### Added

- Standalone `LICENSE`, `.gitignore`, and `ci.yml` workflow at the repo root.

[Unreleased]: https://github.com/kryptic-sh/hjkl-engine/compare/v0.11.1...HEAD
[0.11.4]: https://github.com/kryptic-sh/hjkl-engine/compare/v0.11.3...v0.11.4
[0.11.3]: https://github.com/kryptic-sh/hjkl-engine/compare/v0.11.2...v0.11.3
[0.11.2]: https://github.com/kryptic-sh/hjkl-engine/compare/v0.11.1...v0.11.2
[0.11.1]: https://github.com/kryptic-sh/hjkl-engine/compare/v0.11.0...v0.11.1
[0.11.0]: https://github.com/kryptic-sh/hjkl-engine/compare/v0.10.0...v0.11.0
[0.10.0]: https://github.com/kryptic-sh/hjkl-engine/compare/v0.9.1...v0.10.0
[0.9.1]: https://github.com/kryptic-sh/hjkl-engine/compare/v0.9.0...v0.9.1
[0.9.0]: https://github.com/kryptic-sh/hjkl-engine/compare/v0.8.0...v0.9.0
[0.8.0]: https://github.com/kryptic-sh/hjkl-engine/compare/v0.7.1...v0.8.0
[0.7.1]: https://github.com/kryptic-sh/hjkl-engine/compare/v0.7.0...v0.7.1
[0.7.0]: https://github.com/kryptic-sh/hjkl-engine/compare/v0.6.9...v0.7.0
[0.6.9]: https://github.com/kryptic-sh/hjkl-engine/compare/v0.6.8...v0.6.9
[0.6.8]: https://github.com/kryptic-sh/hjkl-engine/compare/v0.6.7...v0.6.8
[0.6.7]: https://github.com/kryptic-sh/hjkl-engine/compare/v0.6.6...v0.6.7
[0.6.6]: https://github.com/kryptic-sh/hjkl-engine/compare/v0.6.4...v0.6.6
[0.6.4]: https://github.com/kryptic-sh/hjkl-engine/compare/v0.6.3...v0.6.4
[0.6.3]: https://github.com/kryptic-sh/hjkl-engine/compare/v0.6.2...v0.6.3
[0.6.2]: https://github.com/kryptic-sh/hjkl-engine/compare/v0.6.1...v0.6.2
[0.6.1]: https://github.com/kryptic-sh/hjkl-engine/compare/v0.6.0...v0.6.1
[0.6.0]: https://github.com/kryptic-sh/hjkl-engine/compare/v0.5.17...v0.6.0
[0.5.17]: https://github.com/kryptic-sh/hjkl-engine/compare/v0.5.16...v0.5.17
[0.5.16]: https://github.com/kryptic-sh/hjkl-engine/compare/v0.5.15...v0.5.16
[0.5.15]: https://github.com/kryptic-sh/hjkl-engine/compare/v0.5.14...v0.5.15
[0.5.14]: https://github.com/kryptic-sh/hjkl-engine/compare/v0.5.13...v0.5.14
[0.5.13]: https://github.com/kryptic-sh/hjkl-engine/compare/v0.5.12...v0.5.13
[0.5.12]: https://github.com/kryptic-sh/hjkl-engine/compare/v0.5.11...v0.5.12
[0.5.11]: https://github.com/kryptic-sh/hjkl-engine/compare/v0.5.10...v0.5.11
[0.5.10]: https://github.com/kryptic-sh/hjkl-engine/compare/v0.5.9...v0.5.10
[0.5.9]: https://github.com/kryptic-sh/hjkl-engine/compare/v0.5.8...v0.5.9
[0.5.8]: https://github.com/kryptic-sh/hjkl-engine/compare/v0.5.7...v0.5.8
[0.5.7]: https://github.com/kryptic-sh/hjkl-engine/compare/v0.5.6...v0.5.7
[0.5.6]: https://github.com/kryptic-sh/hjkl-engine/compare/v0.5.5...v0.5.6
[0.5.5]: https://github.com/kryptic-sh/hjkl-engine/compare/v0.5.4...v0.5.5
[0.5.4]: https://github.com/kryptic-sh/hjkl-engine/releases/tag/v0.5.4
[0.5.3]: https://github.com/kryptic-sh/hjkl-engine/releases/tag/v0.5.3
[0.5.2]: https://github.com/kryptic-sh/hjkl-engine/compare/v0.5.1...v0.5.2
[0.5.1]: https://github.com/kryptic-sh/hjkl-engine/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/kryptic-sh/hjkl-engine/compare/v0.4.1...v0.5.0
[0.4.1]: https://github.com/kryptic-sh/hjkl-engine/releases/tag/v0.4.1
[0.4.0]: https://github.com/kryptic-sh/hjkl-engine/releases/tag/v0.4.0
[0.3.8]: https://github.com/kryptic-sh/hjkl-engine/releases/tag/v0.3.8
[0.3.7]: https://github.com/kryptic-sh/hjkl-engine/releases/tag/v0.3.7
[0.3.6]: https://github.com/kryptic-sh/hjkl-engine/releases/tag/v0.3.6
[0.3.5]: https://github.com/kryptic-sh/hjkl-engine/releases/tag/v0.3.5
[0.3.4]: https://github.com/kryptic-sh/hjkl-engine/releases/tag/v0.3.4
[0.3.3]: https://github.com/kryptic-sh/hjkl-engine/releases/tag/v0.3.3
[0.3.2]: https://github.com/kryptic-sh/hjkl-engine/releases/tag/v0.3.2
[0.3.1]: https://github.com/kryptic-sh/hjkl-engine/releases/tag/v0.3.1
