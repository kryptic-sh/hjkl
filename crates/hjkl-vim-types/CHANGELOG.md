# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- `LastChange::OpMotion`, `OpTextObj`, `DeleteToEol`, `Paste` and `CharDel` each
  carry a `register: Option<char>` field, matching `LineOp`. Dot-repeat needs it
  to replay a change into the register the original named (`:h redo-register`).

### Added

- Added changelog.

[unreleased]: https://github.com/kryptic-sh/hjkl/compare/v0.40.0...HEAD
