# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). This
project adheres to [Semantic Versioning](https://semver.org/) once it reaches
0.1.0; the 0.0.x series is a churn phase where breaking changes may land on
patch bumps.

## [Unreleased]

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
