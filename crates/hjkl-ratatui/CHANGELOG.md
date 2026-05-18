# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). This
project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.7.0] - 2026-05-17

### Changed

- **Deprecated.** This crate is now a thin re-export shim for `hjkl-editor-tui`
  (#100). Replace `hjkl-ratatui` with `hjkl-editor-tui = "0.1"` and update
  `use hjkl_ratatui` → `use hjkl_editor_tui`. All public symbols are re-exported
  unchanged.
- Implementation removed; `lib.rs` is now `pub use hjkl_editor_tui::*`.
- `form`, `prompt`, `spinner` modules still accessible via re-export.

## [0.6.0] - 2026-05-17

### Changed

- Bumped pinned `hjkl-engine` `0.10` → `0.11`, `hjkl-form` `0.5` → `0.6` (#99
  cascade). This is the last release of `hjkl-ratatui` with its own
  implementation — 0.7.0 will be a thin re-export shim for `hjkl-editor-tui`
  (#100).

## [0.5.0] - 2026-05-17

### Changed

- Bumped pinned `hjkl-engine` `0.9` → `0.10`, `hjkl-form` `0.4` → `0.5` (#96
  cascade).

## [0.4.0] - 2026-05-16

### Changed

- Bumped `hjkl-engine` 0.8 → 0.9 and `hjkl-form` 0.3 → 0.4. No ratatui public
  API changes; bump required because 0.x caret-minor pins are
  semver-incompatible.

## [0.3.7] - 2026-05-15

### Changed

- Bumped `hjkl-engine` dep from `0.6` to `0.7`. Phase 6.6 FSM extraction removed
  `Editor::handle_key` / `step_input` etc., but hjkl-ratatui only uses the
  Style/Rect/KeyEvent adapter surface — no source change needed.

## [0.3.6] - 2026-05-13

### Changed

- Bumped `hjkl-engine` dep requirement from `^0.5` to `^0.6` (engine removed the
  transitional `enter_op_*` controller methods; no API impact for hjkl-ratatui).

## [0.3.5] - 2026-05-10

### Changed

- Bumped `hjkl-engine` dep requirement from `^0.4` to `^0.5`.

## [0.3.4] - 2026-05-06

### Changed

- Bumped `hjkl-engine` dep requirement to `^0.4`.

## [0.3.3] - 2026-05-04

### Docs

- Internal CHANGELOG hygiene: backfilled missing release entries and added
  reference link definitions for all version headings. No functional changes.

## [0.3.2] - 2026-05-03

### Docs

- Dropped SPEC noun and 0.3.0 milestone callout from the README status section.
  Per the org's "no SPEC frozen claims" stance.

## [0.3.1] - 2026-04-30

### Changed

- Migrated `hjkl-ratatui` from the `kryptic-sh/hjkl` monorepo into its own
  repository
  ([kryptic-sh/hjkl-ratatui](https://github.com/kryptic-sh/hjkl-ratatui)) with
  full git history preserved.
- Relaxed inter-crate dependency requirements from `=0.3.0` to `0.3` (caret),
  matching the standard SemVer pattern for library dependencies.
- Bumped `ratatui` to 0.30 (was 0.29) and `crossterm` to 0.29 (was 0.28).

### Added

- Standalone `LICENSE`, `.gitignore`, and `ci.yml` workflow at the repo root.

[Unreleased]: https://github.com/kryptic-sh/hjkl-ratatui/compare/v0.7.0...HEAD
[0.7.0]: https://github.com/kryptic-sh/hjkl-ratatui/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/kryptic-sh/hjkl-ratatui/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/kryptic-sh/hjkl-ratatui/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/kryptic-sh/hjkl-ratatui/compare/v0.3.7...v0.4.0
[0.3.7]: https://github.com/kryptic-sh/hjkl-ratatui/compare/v0.3.6...v0.3.7
[0.3.6]: https://github.com/kryptic-sh/hjkl-ratatui/compare/v0.3.5...v0.3.6
[0.3.5]: https://github.com/kryptic-sh/hjkl-ratatui/compare/v0.3.4...v0.3.5
[0.3.4]: https://github.com/kryptic-sh/hjkl-ratatui/releases/tag/v0.3.4
[0.3.3]: https://github.com/kryptic-sh/hjkl-ratatui/releases/tag/v0.3.3
[0.3.2]: https://github.com/kryptic-sh/hjkl-ratatui/releases/tag/v0.3.2
[0.3.1]: https://github.com/kryptic-sh/hjkl-ratatui/releases/tag/v0.3.1
