# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). This
project adheres to [Semantic Versioning](https://semver.org/) once it reaches
0.1.0; the 0.0.x series is a churn phase where breaking changes may land on
patch bumps.

## [Unreleased]

### Fixed

- Parsing a `'<mark>` range address no longer panics on a multibyte mark
  character (e.g. `:'é`); the address is sliced at the mark's real byte boundary
  instead of a hard-coded 2-byte offset.

## [0.5.1] - 2026-05-18

### Fixed

- `global.rs` and `builtins.rs` callers updated to use `unwrap_or_default()`
  instead of `unwrap_or("")` and to pass `&line` where `&str` is required after
  `Buffer::line` changed to return `Option<String>` in hjkl-buffer 0.8.1.

## [0.5.0] - 2026-05-17

## [0.4.1] - 2026-05-17

### Added

- Backfilled 58 unit tests across `builtins.rs` handlers (#107): quit, quit!,
  write, wall, edit, edit!, bdelete, bdelete!, bwipeout, bwipeout!, wq, wq!,
  wqall, qall, qall!, nohlsearch, undo, redo, registers, marks, jumps, changes,
  delete, sort (all flags), substitute (simple, /g, /i, range, bad-pattern),
  read (file, missing file, `!cmd` success, nonzero exit, stderr, empty cmd),
  set (smoke), and `register_builtins` registry smoke.

## [0.4.0] - 2026-05-17

### Changed

- Bumped pinned `hjkl-engine` `0.10` → `0.11` (#99 cascade).

## [0.3.0] - 2026-05-17

### Changed

- Bumped pinned `hjkl-engine` `0.9` → `0.10` (#96 cascade).

## [0.2.1] - 2026-05-17

### Removed

- Dropped unused `thiserror` dep (flagged by `cargo machete`; no public API
  change).

## [0.2.0] - 2026-05-16

### Changed

- Bumped hjkl-engine dep 0.8 → 0.9 and hjkl-buffer 0.6 → 0.7. No ex-cmd public
  API changes; bump required because 0.x caret-minor pins are
  semver-incompatible.

## [0.1.0] - 2026-05-15

### Added

- Initial release. Extracted from the umbrella `kryptic-sh/hjkl` repository as
  part of the Phase 1–8 ex-command extraction arc.
- `Registry<H>` / `HostRegistry<Ctx>` extensible command registries with
  canonical names, aliases, and `ArgKind` metadata.
- `try_dispatch` resolving a command string against the editor registry and
  returning an `ExEffect`.
- `complete` / `complete_arg` Tab-completion engine dispatching to path,
  setting, buffer, register, and mark completion based on declared `ArgKind`.
- `all_setting_names()` flat list of `:set` option names + aliases for host
  completion sources.
- `parse_range` Vim-compatible line-range parser (`%`, `.`, `$`, `'a`, …).
- Built-in handlers for `:w[rite]`, `:wq`, `:q[uit]`, `:e[dit]`, `:r[ead]`,
  `:b[uffer]`, `:bd[elete]`, `:bw[ipeout]`, `:s[ubstitute]`, `:g[lobal]`,
  `:v[global]`, `:set`, `:nohlsearch`, `:noh`, `:reg[isters]`, `:marks`,
  `:delm[arks]`, `:cd`, `:pwd`, `:fold*`, `:%`, `:#`, `:<cword>` expansion +
  filename modifiers.

[0.5.1]: https://github.com/kryptic-sh/hjkl-ex/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/kryptic-sh/hjkl-ex/compare/v0.4.1...v0.5.0
[0.4.1]: https://github.com/kryptic-sh/hjkl-ex/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/kryptic-sh/hjkl-ex/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/kryptic-sh/hjkl-ex/compare/v0.2.1...v0.3.0
[0.2.1]: https://github.com/kryptic-sh/hjkl-ex/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/kryptic-sh/hjkl-ex/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/kryptic-sh/hjkl-ex/releases/tag/v0.1.0
