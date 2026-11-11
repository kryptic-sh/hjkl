# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). This
project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Changed

- **Breaking:** `KeyEvent` re-exports `ttyr::event::KeyEvent` in place of
  crossterm's, and `crossterm_to_input(KeyEvent)` is `ttyr_to_input(&KeyEvent)`.
  A character key converts to the character it typed (`logical_char`), with
  Shift dropped where that character carries it, so Shift+a is `A` from a legacy
  or a kitty terminal alike.
- ratatui without its default features: no crossterm in the dependency tree.

## [0.40.0] - 2026-08-01

### Removed

- `EditorRatatuiExt::intern_ratatui_style` and
  `EditorRatatuiExt::ratatui_style_table` — both had zero callers. Intern an
  engine-native style with `Editor::intern_style` after converting via
  `style_from_ratatui`; convert table entries with `style_to_ratatui`.

### Added

- Added the crossterm adapter (`KeyEvent` re-export + `crossterm_to_input` free
  fn) moved from `hjkl-engine`. The 231-test engine integration suite relocated
  to `tests/editor_behavior.rs`. Phase 3 of #162.

## [0.25.0] - 2026-05-18

### Added

- Initial release. Extracted from `hjkl-engine` 0.25.0 — the ratatui adapter
  surface previously lived behind `hjkl-engine`'s `ratatui` feature gate,
  dropped as part of #162 phase 2. Provides `style_to_ratatui`,
  `style_from_ratatui`, and the `EditorRatatuiExt` extension trait
  (`intern_ratatui_style`, `install_ratatui_syntax_spans`,
  `ratatui_style_table`).

[Unreleased]: https://github.com/kryptic-sh/hjkl/compare/v0.45.1...HEAD
[0.40.0]: https://github.com/kryptic-sh/hjkl/compare/v0.25.0...v0.40.0
[0.25.0]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.25.0
