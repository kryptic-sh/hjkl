# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). This
project adheres to [Semantic Versioning](https://semver.org/) once it reaches
0.1.0; the 0.0.x series is a churn phase where breaking changes may land on
patch bumps.

## [Unreleased]

## [0.0.10] - 2026-04-26

### Added

- `hjkl-engine::types::OptionValue { Bool, Int, String }` — typed value carrier
  for the `:set` parser.
- `Options::set_by_name(name, OptionValue) -> Result<(), EngineError>` and
  `Options::get_by_name(name) -> Option<OptionValue>`. Vim-style short aliases
  supported (`ts`, `sw`, `et`, `isk`, `ic`, `scs`, `hls`, `is`, `ws`, `ai`,
  `tm`, `ul`, `ro`).

## [0.0.9] - 2026-04-26

### Changed (breaking the 0.0.8 snapshot wire format)

- `EditorSnapshot::VERSION` bumped to `3`. Adds a
  `file_marks: HashMap<char, (u32, u32)>` field carrying the uppercase / "file"
  marks (`'A`–`'Z`). Survives `set_content`, so hosts persisting between tab
  swaps round-trip mark state. 0.0.8 snapshots fail `restore_snapshot` with
  `EngineError::SnapshotVersion`.

## [0.0.8] - 2026-04-26

### Changed (breaking the 0.0.7 snapshot wire format)

- `EditorSnapshot::VERSION` bumped to `2`. The struct gains a
  `registers: Registers` field carrying vim's `""`, `"0`, `"1`–`"9`, `"a`–`"z`,
  and `"+`/`"*` slots. 0.0.7 snapshots fail `restore_snapshot` with
  `EngineError::SnapshotVersion`.
- `Slot` and `Registers` derive `Serialize` / `Deserialize` behind the `serde`
  feature.

## [0.0.7] - 2026-04-26

### Added

- `hjkl-engine::types::RenderFrame` — borrow-style render frame the host
  consumes once per redraw. Coarse today: mode + cursor + cursor_shape +
  viewport_top + line_count.
- `Editor::render_frame()` builder.
- `Editor::highlights_for_line(u32)` — SPEC `Highlight` emission with
  `HighlightKind::SearchMatch` for search hits.
- `Editor::selection_highlight()` — bridges the active visual selection to a
  SPEC `Highlight` with `HighlightKind::Selection`. None outside visual modes;
  visual-line / visual-block collapse to their bounding char range.

### Changed

- `CursorShape` now derives `Hash` so `RenderFrame` can derive it.

## [0.0.6] - 2026-04-26

### Added

- `hjkl-engine::types::EditorSnapshot` — coarse serde-friendly snapshot of
  editor state for host persistence. Carries `version`, `mode`, `cursor`,
  `lines`, `viewport_top`. Bumps the snapshot `EditorSnapshot::VERSION` constant
  to track wire-format compat.
- `hjkl-engine::types::SnapshotMode` — status-line mode summary embedded in the
  snapshot.
- `Editor::take_snapshot()` — produces an `EditorSnapshot` at the current state.
- `Editor::restore_snapshot(snap)` — restores from a snapshot; returns
  `EngineError::SnapshotVersion` on wire-format mismatch.

## [0.0.5] - 2026-04-26

### Changed

- **`ex.rs` relocated from `hjkl-engine` to `hjkl-editor`.** Ex commands now
  live in the crate they belong to. Consumers reach `ex` via
  `hjkl_editor::runtime::ex` (unchanged surface — the facade was already routing
  there).
- `hjkl-editor` gains `regex` as a direct dep (ex uses it for `:s/pat/.../`) and
  `crossterm` as a dev-dep.
- `mark_dirty_after_ex` is now a free function. Ex callsites that previously
  wrote `editor.mark_dirty_after_ex()` now write `mark_dirty_after_ex(editor)`.

### Added (engine internal — sealed at 0.1.0)

Several `pub(super)` / `pub(crate)` items on `Editor` and `VimState` gained
`#[doc(hidden)] pub` visibility so ex commands can reach them across the crate
boundary:

- `Editor`: `vim`, `undo_stack`, `registers`, `settings`, `file_marks`,
  `syntax_fold_ranges` fields; `settings_mut`, `mutate_edit`, `push_undo`,
  `restore`, `jump_cursor` methods.
- `VimState`: `last_edit_pos`, `jump_back`, `marks` fields.
- `vim::do_undo`, `vim::do_redo` re-exported at the crate root.

These are explicit churn-phase exposures and will be sealed under the 0.1.0
trait extraction. Do not rely on them.

### Migrated tests

5 vim+ex integration tests (`gqq` reflow, `gq` motion, paragraph break
preservation, `gqq` undo, `:marks` listing) moved from
`crates/hjkl-engine/src/vim.rs` to
`crates/hjkl-editor/tests/vim_ex_integration.rs`. cargo dev-dep cycles between
hjkl-engine and hjkl-editor produce duplicate type IDs, so they must run from
the editor side.

## [0.0.4] - 2026-04-26

### Changed

- Workspace `homepage` set to <https://hjkl.kryptic.sh>.
- Per-crate READMEs now carry CI / crates.io version / docs.rs / License /
  Website badges and a Website + Source line.

## [0.0.3] - 2026-04-26

### Added

- `hjkl-engine::Editor::take_content_change()` — pull-model coarse change
  observation. Returns `Some(Arc<String>)` if content changed since the last
  call, `None` otherwise. Drains the dirty flag. Bridges the gap until SPEC's
  `take_changes() -> Vec<EditOp>` ships with full edit-path instrumentation.
- `hjkl-engine::types::Viewport` (re-exported as `PlannedViewport` at the crate
  root to disambiguate from `hjkl_buffer::Viewport`).
- `hjkl-engine::types::BufferId` opaque newtype.
- 513-case proptest harness for the FSM (`tests/proptest_fsm.rs`): random
  keystroke sequences never panic, and three Escapes always return to Normal
  mode.

## [0.0.2] - 2026-04-26

### Added

- `hjkl-engine::types` extended with the planned 0.1.0 trait surface: `Options`,
  `EngineError`, `Modifiers`, `SpecialKey`, `MouseEvent`, `MouseKind`, `Input`,
  `Host` trait. All additive — coexists with the legacy runtime types in
  `hjkl-engine::editor`.
- `hjkl-editor`: real facade crate (was placeholder). Exposes three modules:
  `buffer`, `runtime`, `spec`. Consumers depend on hjkl-editor alone instead of
  all three downstream crates.
- `hjkl-buffer/IMPLEMENTERS.md`: caller invariants documentation.
- `hjkl-buffer` criterion benches under the `budgets` harness:
  `insert_char_1MB_buffer`, `search_next_10k_lines`, `cold_load_10MB`.
- `hjkl-buffer` proptest roundtrip property suite for `apply_edit` (768 random
  scenarios per test run).

### Changed

- `hjkl-engine::types::Edit` re-exported from the crate root as `EditOp` to
  disambiguate from `hjkl_buffer::Edit`.

## [0.0.1] - 2026-04-26

### Added

- `hjkl-buffer`: full sqeel-buffer port with cursor, edits, motions, folds,
  viewport, search. ratatui Widget impl behind optional `ratatui` feature.
  Default features off — buffer is UI-agnostic.
- `hjkl-engine`: full sqeel-vim port with vim FSM, ex commands, registers,
  dot-repeat, marks. ratatui + crossterm currently mandatory; phase 5 trait
  extraction will move them behind features.
- `hjkl-engine::types` module: SPEC core types (`Pos`, `Selection`,
  `SelectionKind`, `SelectionSet`, `Edit`, `Mode`, `CursorShape`, `Style`,
  `Color`, `Attrs`, `Highlight`, `HighlightKind`). Additive alongside the legacy
  public API; trait extraction wires the FSM and Editor onto these
  progressively.

### Changed

- `hjkl-editor` and `hjkl-ratatui`: still placeholder; ship 0.0.1 to keep
  lockstep workspace version.

## [0.0.0] - 2026-04-26

### Added

- Initial placeholder release. Reserves `hjkl-engine`, `hjkl-buffer`,
  `hjkl-editor`, and `hjkl-ratatui` names on crates.io. No public API.
- `MIGRATION.md` — extraction plan and design rationale.

[Unreleased]: https://github.com/kryptic-sh/hjkl/compare/v0.0.0...HEAD
[0.0.0]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.0
