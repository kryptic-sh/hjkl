# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). This
project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

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

[Unreleased]: https://github.com/kryptic-sh/hjkl-engine/compare/v0.5.13...HEAD
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
