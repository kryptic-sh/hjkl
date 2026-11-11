# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **Breaking:** the adapter is between `hjkl_keymap::KeyEvent` and
  `ttyr::event::KeyEvent`: `from_crossterm` / `to_crossterm` are `from_ttyr` /
  `to_ttyr`. A character key is the character it typed, so from a kitty terminal
  Alt+Shift+a is `A` with Alt (`<M-A>`), not `a` with Alt.

## [0.45.0] - 2026-11-04

### Added

- Added changelog.

[Unreleased]: https://github.com/kryptic-sh/hjkl/compare/v0.45.1...HEAD
[0.45.0]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.45.0
[unreleased]: https://github.com/kryptic-sh/hjkl/compare/v0.40.0...HEAD
