# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). This
project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.6.2] - 2026-05-16

### Added

- `visual_col_to_char_col(line, visual_col, tab_width) -> usize` in the new
  `geom` module (re-exported from the crate root). Inverse of `hjkl-engine`'s
  internal `visual_col_for_char`: walks a line's chars accumulating tab-expanded
  visual width and returns the char index where a mouse click at `visual_col`
  lands. Tabs snap to the tab character itself (Vim behaviour). Past-EOL clicks
  clamp to `char_count`. Enables host-driven mouse translation without baking
  terminal-layout assumptions into the engine. See
  `geom::visual_col_to_char_col` rustdoc for the full contract and wide-char
  note.

## [0.6.1] - 2026-05-16

### Changed

- `resolve_span_style` now layers every overlapping span instead of picking one.
  Spans are sorted broadest-first, then `Style::patch`-merged so the narrower
  span's set fields override the broader span's, but unset fields (e.g. a narrow
  `@keyword` carrying only `fg`) inherit the broader span's values (e.g. a wide
  `@markup.raw.block` carrying only `bg`). This matches vim and Helix's layered
  hi-group model and lets hosts tint markdown code blocks (or any nested region)
  without bloating every injected language's captures with the same background.

  The pre-0.6.1 narrowest-wins-completely behaviour is preserved whenever the
  narrower span sets every field — `Style::patch` only carries broader fields
  through `None` slots.

### Tests

- `layered_spans_blend_broad_bg_with_narrow_fg` pins the new contract: broad
  bg-only span + narrow fg-only span ⇒ cells carry both.
- `narrow_span_with_explicit_bg_still_overrides_broad_bg` pins the override
  case: a narrow span that DOES set bg overrides the broader bg.

## [0.6.0] - 2026-05-10

### Added

- `BufferView` gains `colorcolumn_cols: &'a [u16]` (sorted, deduplicated 1-based
  column indices to highlight) and `colorcolumn_style: Style` (background style
  applied to those cells).
- Renderer paints those columns under syntax in a new pass between the
  cursorcolumn pass and the diagnostic overlay. `Wrap::None` only.

### Changed

- **Breaking**: `BufferView` is a `pub struct` without `#[non_exhaustive]`;
  adding required fields breaks any downstream that builds it via literal struct
  expression. Pass `colorcolumn_cols: &[]` and
  `colorcolumn_style: Style::default()` to keep prior behaviour.

## [0.5.0] - 2026-05-06

### Added

- `BufferView::non_text_style` field; rows past end-of-buffer now paint `~`
  (vim's NonText marker) at the first text column. The gutter on those rows
  stays blank. Style defaults to `Style::default()` when not set.
- `DiagOverlay` struct (`row`, `col_start`, `col_end`, `style`) for inline
  diagnostic highlighting. Applied in a post-paint pass so it layers on top of
  syntax and selection colours.
- `BufferView::diag_overlays: &'a [DiagOverlay]` field. Existing
  exhaustive-struct-init consumers must add `diag_overlays: &[]`. Pass `&[]` to
  disable (no behaviour change).

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

[Unreleased]: https://github.com/kryptic-sh/hjkl-buffer/compare/v0.6.2...HEAD
[0.6.2]: https://github.com/kryptic-sh/hjkl-buffer/compare/v0.6.1...v0.6.2
[0.6.1]: https://github.com/kryptic-sh/hjkl-buffer/compare/v0.6.0...v0.6.1
[0.6.0]: https://github.com/kryptic-sh/hjkl-buffer/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/kryptic-sh/hjkl-buffer/releases/tag/v0.5.0
[0.4.0]: https://github.com/kryptic-sh/hjkl-buffer/releases/tag/v0.4.0
[0.3.5]: https://github.com/kryptic-sh/hjkl-buffer/releases/tag/v0.3.5
[0.3.4]: https://github.com/kryptic-sh/hjkl-buffer/releases/tag/v0.3.4
[0.3.3]: https://github.com/kryptic-sh/hjkl-buffer/releases/tag/v0.3.3
[0.3.2]: https://github.com/kryptic-sh/hjkl-buffer/releases/tag/v0.3.2
[0.3.1]: https://github.com/kryptic-sh/hjkl-buffer/releases/tag/v0.3.1
