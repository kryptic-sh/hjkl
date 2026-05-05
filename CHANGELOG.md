# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). This
project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- `BufferView::non_text_style` field; rows past end-of-buffer now paint `~`
  (vim's NonText marker) at the first text column. The gutter on those rows
  stays blank. Style defaults to `Style::default()` when not set.

## [0.4.0] - 2026-05-06

### Added

- `GutterNumbers` enum (`Absolute` / `Relative` / `Hybrid` / `None`) controls
  what is rendered in the gutter. `Absolute` is the default, matching the
  previous always-on behaviour.
- `Gutter::numbers: GutterNumbers` field. Existing consumers using
  `..Default::default()` keep working; consumers using exhaustive struct-init
  syntax must add `numbers: GutterNumbers::Absolute` (minor break).
- `GutterNumbers` is re-exported from the crate root under the `ratatui` feature
  alongside `Gutter`.

## [0.3.5] - 2026-05-05

### Docs

- Inlined former `IMPLEMENTERS.md` invariants into rustdoc on the actual types
  and methods (`Position`, `Edit` + variants, `Fold`, `Viewport`, `Span`,
  `Buffer::set_cursor` / `clamp_position` / `ensure_cursor_visible`,
  `BufferView` render module, crate-level `lib.rs`). Now renders on docs.rs next
  to each symbol and shows up in IDE hover.
- Removed `IMPLEMENTERS.md` (content fully relocated; README points at docs.rs).
- Dropped stale Marks + Search sections — those APIs were removed from `Buffer`
  at v0.0.37 (now live in the engine layer).
- Fixed broken `MIGRATION.md` link in crate-level rustdoc (file was deleted
  upstream pre-0.1.0).
- Fixed three pre-existing broken intra-doc links in `render.rs`.

## [0.3.4] - 2026-05-04

### Docs

- Internal CHANGELOG hygiene: backfilled missing release entries and added
  reference link definitions for all version headings. No functional changes.

## [0.3.3] - 2026-05-03

### Docs

- Dropped frozen / sealed rhetoric from the README status section. Per the org's
  "no SPEC frozen claims" stance: features keep landing, bumps follow semver —
  no need to oversell stability.

## [0.3.2] - 2026-05-03

### Internal

- Dropped reference to `hjkl-engine/SPEC.md` from `src/motion.rs` doc comment.

## [0.3.1] - 2026-04-30

### Changed

- Migrated `hjkl-buffer` from the `kryptic-sh/hjkl` monorepo into its own
  repository
  ([kryptic-sh/hjkl-buffer](https://github.com/kryptic-sh/hjkl-buffer)) with
  full git history preserved.
- Relaxed inter-crate dependency requirements from `=0.3.0` to `0.3` (caret),
  matching the standard SemVer pattern for library dependencies.
- Bumped `ratatui` to 0.30 (was 0.29) and `criterion` to 0.8 (was 0.5).

### Added

- Standalone `LICENSE`, `.gitignore`, and `ci.yml` workflow at the repo root.

[Unreleased]: https://github.com/kryptic-sh/hjkl-buffer/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/kryptic-sh/hjkl-buffer/releases/tag/v0.4.0
[0.3.5]: https://github.com/kryptic-sh/hjkl-buffer/releases/tag/v0.3.5
[0.3.4]: https://github.com/kryptic-sh/hjkl-buffer/releases/tag/v0.3.4
[0.3.3]: https://github.com/kryptic-sh/hjkl-buffer/releases/tag/v0.3.3
[0.3.2]: https://github.com/kryptic-sh/hjkl-buffer/releases/tag/v0.3.2
[0.3.1]: https://github.com/kryptic-sh/hjkl-buffer/releases/tag/v0.3.1
