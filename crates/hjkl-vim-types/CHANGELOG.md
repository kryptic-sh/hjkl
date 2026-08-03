# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Every `LastChange` variant that writes a register carries a
  `register: Option<char>` field, matching `LineOp`: `OpMotion`, `OpTextObj`,
  `DeleteToEol`, `Paste`, `CharDel`, `GnOp` and `VisualOp`. Dot-repeat needs it
  to replay a change into the register the original named (`:h redo-register`).

### Added

- Added changelog.

[unreleased]: https://github.com/kryptic-sh/hjkl/compare/v0.40.0...HEAD
