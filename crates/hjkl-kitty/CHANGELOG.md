# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.45.0] - 2026-11-04

### Added

- Added changelog.

### Fixed

- `normalize_legacy` preserves the event's `kind` and `state` when mapping
  Ctrl+[ / Ctrl+I / Ctrl+M. It rebuilt the event with `KeyEvent::new`, which
  resets `kind` to `Press`, so on Windows (where every key-up arrives as a
  `Release`) a mapped key's release became a second press.

[Unreleased]: https://github.com/kryptic-sh/hjkl/compare/v0.46.0...HEAD
[0.45.0]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.45.0
[unreleased]: https://github.com/kryptic-sh/hjkl/compare/v0.40.0...HEAD
