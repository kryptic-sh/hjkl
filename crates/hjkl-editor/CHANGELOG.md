# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). This
project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.8.1] - 2026-05-18

### Changed

- Bump `hjkl-buffer` transitive dep to 0.8.1 (closes #158 multi-view UB). No API
  changes in this crate.

## [0.8.0] - 2026-05-17

### Changed

- `default` features now `["serde"]` only — `crossterm` and `ratatui` dropped
  from defaults (#99). Mirrors the hjkl-engine default flip.
- Bumped pinned `hjkl-engine` `0.10` → `0.11` (#99 cascade).

## [0.7.0] - 2026-05-17

### Changed

- Bumped pinned `hjkl-engine` `0.9` → `0.10` (#96 cascade).

## [0.6.0] - 2026-05-16

### Changed

- Bumped `hjkl-engine` dep `0.8` → `0.9` and `hjkl-buffer` `0.6` → `0.7`. No
  editor public API changes; the bump is required because 0.x caret-minor pins
  are semver-incompatible and downstream consumers (`apps/hjkl`) need engine 0.9
  APIs.

## [0.5.0] - 2026-05-15

### Removed

- **`src/ex.rs`** deleted entirely — ex-command dispatch now lives in `hjkl-ex`
  (Phase 8b). The `runtime::ex` module and the legacy `ExEffect` enum are gone.
- **`tests/golden_ex.rs`** and **`tests/vim_ex_integration.rs`** deleted; all
  ex-command tests now live in the `hjkl-ex` crate.
- `regex` runtime dependency removed (was only needed by ex.rs).
- `insta` and `hjkl-vim` dev-dependencies removed (were only needed by ex
  tests).

### Fixed

- Gate `BufferView`, `Gutter`, `Sign`, `StyleResolver` re-exports in
  `pub mod buffer` behind `#[cfg(feature = "ratatui")]`. These types only exist
  in `hjkl-buffer` under its `ratatui` feature, so building `hjkl-editor` with
  `--no-default-features` previously failed to resolve them. Headless consumers
  (the upcoming floem GUI binary, embedding hosts, test harnesses) now build
  cleanly. Ratatui consumers see no surface change. Resolves
  [hjkl#98](https://github.com/kryptic-sh/hjkl/issues/98).

### Breaking

- `hjkl_editor::runtime::ex` module no longer exists. Consumers must switch to
  `hjkl_ex::{try_dispatch, default_registry, ExEffect}`.

## [0.4.7] - 2026-05-15

### Changed

- Bumped `hjkl-engine` dep from `0.6.8` to `0.7` (picks up Phase 6.6 FSM
  extraction; engine no longer hosts the FSM — that lives in `hjkl-vim`).
- Test helper migration: `ex.rs` test driver now uses `hjkl_vim::handle_key`
  instead of the deleted `Editor::handle_key`. No public API change.

## [0.4.6] - 2026-05-14

### Changed

- Bumped `hjkl-engine` dep from `0.6.7` to `0.6.8` (picks up Phase 5 controller
  methods + `cursorline` default flip to `true`).

### Tests

- Added `goto_line_100` regression test in `crates/hjkl-editor/src/ex.rs`
  asserting `:100` engine-layer dispatch moves the cursor to line 100 on a
  120-line buffer (engine-layer half of the umbrella `:100`-stuck fix).
- Updated the `:set` listing snapshot
  (`crates/hjkl-editor/tests/snapshots/golden_ex__set_listing.snap`) to reflect
  the new `cursorline=on` default.

## [0.4.5] - 2026-05-13

### Changed

- Bumped `hjkl-engine` dep requirement from `^0.5` to `^0.6` (engine removed the
  transitional `enter_op_*` controller methods; no API impact for hjkl-editor).

## [0.4.4] - 2026-05-10

### Changed

- Bumped `hjkl-engine` dep requirement from `^0.4` to `^0.5` and `hjkl-buffer`
  from `^0.5` to `^0.6`.
- `0.4.3` was published with `hjkl-engine = "^0.4"` but referenced fields
  (`SignColumnMode`, `cursorline`, `cursorcolumn`, `signcolumn`, `foldcolumn`,
  `colorcolumn`) that only exist in `hjkl-engine 0.5+`. This release fixes that
  version mismatch so the `:set cursorline` / `cursorcolumn` / `signcolumn` /
  `foldcolumn` / `colorcolumn` features compile correctly.

## [0.4.3] - 2026-05-10

### Added

- `:set` parser now recognizes five new render-level options via
  `apply_set_token`:
  - `cursorline` / `cul`: toggle, supports no-prefix and `!`-suffix.
  - `cursorcolumn` / `cuc`: toggle, supports no-prefix and `!`-suffix.
  - `signcolumn` / `scl`: string-valued, accepts `yes` / `no` / `auto`.
  - `foldcolumn` / `fdc`: integer, clamped 0–12.
  - `colorcolumn` / `cc`: string, comma-separated absolute column list.
- Bare `:set` listing now includes the five new options. Golden snapshot at
  `tests/snapshots/golden_ex__set_listing.snap` updated.

### Changed

- Requires `hjkl-engine 0.5.0+` for the underlying `Options` fields backing the
  new `:set` tokens.

## [0.4.2] - 2026-05-06

### Added

- `:set number` / `:set nu` / `:set nonumber` / `:set nonu` and the matching
  `relativenumber` / `rnu` / `nornu` arms in `apply_set_token` toggle the
  engine's new `Settings.number` / `Settings.relativenumber` flags.
- `:set numberwidth=N` / `:set nuw=N` accepts 1..=20 and updates
  `Settings.numberwidth` (vim's `'numberwidth'` minimum-width option).
- All three settings respect the `!` toggle suffix (`:set nu!`, `:set rnu!`).
- Bare `:set` info output now includes the three new fields.
- `:/pat` and `:?pat` search-as-address commands: forward (`/`) and backward
  (`?`) search addresses in ex ranges (e.g. `:/foo/d`, `:?bar?,/baz/y`).

### Changed

- Bumped `hjkl-engine` and `hjkl-buffer` dep requirements from `^0.3` to `^0.5`
  to consume the new `Settings` fields, `GutterNumbers` enum, and `DiagOverlay`.

## [0.4.1] - 2026-05-05

### Added

- `:qall` / `:qall!` / `:wqall` / `:wqall!` dispatch arms in `ex.rs`. The
  canonical-name table already resolved `qa` → `qall` and `wqa` → `wqall`, but
  the match block had no arms for them, so they fell through to
  `ExEffect::Unknown`. Surfaces in `hjkl --nvim-api`: nvim-rs clients send
  `:qa!` to terminate (mirrors nvim semantics); server now quits instead of
  returning an error. Reverts the `:q!` workaround added in hjkl@3469671 on the
  consumer side (`apps/hjkl/tests/nvim_api.rs` and
  `crates/hjkl-compat-oracle/src/hjkl_driver.rs`). Closes
  [kryptic-sh/hjkl#27](https://github.com/kryptic-sh/hjkl/issues/27).

## [0.4.0] - 2026-05-05

### Changed

- **Breaking:** `ExEffect::Substituted` now carries `lines_changed: usize`
  alongside `count`. Status-line consumers can now render vim-accurate
  `N substitutions on M lines`. `apply_global` (`:g/pat/d`) returns
  `lines_changed = count` since each match is its own line.
- `apply_substitute` rewired onto the new `hjkl_engine::substitute` parser:
  - `I` flag forces case-sensitive (overrides editor `ignorecase`).
  - `c` flag is parsed and silently ignored (no confirm UI in v1).
  - Empty pattern (`:s//rep/`) reuses `editor.last_search()` and errors with
    `no previous regular expression` when nothing has been searched.
  - Successful substitute updates `last_search` so subsequent `n`/`N` find the
    same pattern.

## [0.3.3] - 2026-05-04

### Docs

- Internal CHANGELOG hygiene: backfilled missing release entries and added
  reference link definitions for all version headings. No functional changes.

## [0.3.2] - 2026-05-03

### Internal

- Dropped references to `hjkl-engine/SPEC.md` from `src/lib.rs` and `README.md`.

## [0.3.1] - 2026-04-30

### Changed

- Migrated `hjkl-editor` from the `kryptic-sh/hjkl` monorepo into its own
  repository
  ([kryptic-sh/hjkl-editor](https://github.com/kryptic-sh/hjkl-editor)) with
  full git history preserved.
- Relaxed inter-crate dependency requirements from `=0.3.0` to `0.3` (caret),
  matching the standard SemVer pattern for library dependencies.
- Bumped `crossterm` to 0.29 (was 0.28).

### Added

- Standalone `LICENSE`, `.gitignore`, and `ci.yml` workflow at the repo root.

[Unreleased]: https://github.com/kryptic-sh/hjkl-editor/compare/v0.8.0...HEAD
[0.8.1]: https://github.com/kryptic-sh/hjkl-editor/compare/v0.8.0...v0.8.1
[0.8.0]: https://github.com/kryptic-sh/hjkl-editor/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/kryptic-sh/hjkl-editor/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/kryptic-sh/hjkl-editor/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/kryptic-sh/hjkl-editor/compare/v0.4.7...v0.5.0
[0.4.7]: https://github.com/kryptic-sh/hjkl-editor/compare/v0.4.6...v0.4.7
[0.4.6]: https://github.com/kryptic-sh/hjkl-editor/compare/v0.4.5...v0.4.6
[0.4.5]: https://github.com/kryptic-sh/hjkl-editor/compare/v0.4.4...v0.4.5
[0.4.4]: https://github.com/kryptic-sh/hjkl-editor/compare/v0.4.3...v0.4.4
[0.4.3]: https://github.com/kryptic-sh/hjkl-editor/releases/tag/v0.4.3
[0.4.2]: https://github.com/kryptic-sh/hjkl-editor/releases/tag/v0.4.2
[0.4.1]: https://github.com/kryptic-sh/hjkl-editor/releases/tag/v0.4.1
[0.4.0]: https://github.com/kryptic-sh/hjkl-editor/releases/tag/v0.4.0
[0.3.3]: https://github.com/kryptic-sh/hjkl-editor/releases/tag/v0.3.3
[0.3.2]: https://github.com/kryptic-sh/hjkl-editor/releases/tag/v0.3.2
[0.3.1]: https://github.com/kryptic-sh/hjkl-editor/releases/tag/v0.3.1
