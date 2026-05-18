# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.1] - 2026-05-18

### Changed

- Bump `hjkl-buffer` transitive dep to 0.8.1 (closes #158 multi-view UB). No API
  changes in this crate.

## [0.4.0] - 2026-05-17

### Changed

- Bumped pinned `hjkl-picker` `0.8` → `0.9`, `hjkl-engine` `0.10` → `0.11` (#99
  cascade). Explicit `features = ["ratatui", "crossterm"]` on engine dep — no
  longer relying on engine defaults.

## [0.3.0] - 2026-05-17

### Changed

- Bumped pinned `hjkl-picker` `0.7` → `0.8`, `hjkl-engine` `0.9` → `0.10` (#96
  cascade). The preview pane's `gutter_width` helper retains its local formula
  (`digits+1` floored to 4) because `preview_pane` has no `&Editor` — the
  preview renders a plain file buffer without editor settings.

## [0.2.0] - 2026-05-16

### Changed

- Bumped hjkl-picker 0.6 → 0.7, hjkl-engine 0.8 → 0.9, hjkl-buffer 0.6 → 0.7. No
  picker-tui public API changes; bump required because 0.x caret-minor pins are
  semver-incompatible.

## [0.1.0] - 2026-05-16

### Added

- Initial release. Extracted from the umbrella `kryptic-sh/hjkl` repository.
- `handle_key` translating `crossterm::event::KeyEvent` into `Picker` method
  calls and returning a `PickerEvent` (cancel / accept / navigation / forward).
- `preview_pane` rendering the active picker item's preview buffer into a
  `ratatui::Frame` with border, gutter, syntax highlighting, and match-row
  highlight.
- `PreviewTheme` struct exposing pre-computed `ratatui::Style` values for
  border, gutter, non-text glyphs, and cursor-line background.

[Unreleased]:
  https://github.com/kryptic-sh/hjkl-picker-tui/compare/v0.4.0...HEAD
[0.4.1]: https://github.com/kryptic-sh/hjkl-picker-tui/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/kryptic-sh/hjkl-picker-tui/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/kryptic-sh/hjkl-picker-tui/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/kryptic-sh/hjkl-picker-tui/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/kryptic-sh/hjkl-picker-tui/releases/tag/v0.1.0
