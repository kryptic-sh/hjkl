# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). This
project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

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

[0.25.0]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.25.0
