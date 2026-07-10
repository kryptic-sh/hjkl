# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). This
project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Fixed

- `wrap::wrap_segments` no longer loops forever (growing an unbounded `Vec`
  until the process runs out of memory) when a single character is wider than
  the wrap width — e.g. a double-width CJK/emoji character in a 1-cell text
  area. The oversized character is emitted as its own segment so wrapping always
  makes forward progress.

### Removed

- Dropped the `ratatui` feature and `render` module. The ratatui Widget adapter
  now lives in the new `hjkl-buffer-tui` crate. Part of #162.

## [0.8.1] - 2026-05-18

### Fixed

- `lines()` and `line()` now return owned data (`Vec<String>` and
  `Option<String>`) instead of references backed by a held `MutexGuard`. The
  previous implementation used an unsafe lifetime extension that was unsound
  under multi-view use: a second `Buffer` sharing the same `Arc<Mutex<Content>>`
  could reallocate the `Vec` between guard-drop and caller use. Returning owned
  data eliminates the hazard with no API behaviour change for single-view users.
- `folds()` and `fold_at_row()` apply the same fix: `folds()` returns
  `Vec<Fold>` (cloned from the guard) and `fold_at_row()` returns `Option<Fold>`
  (copy-on-find, `Fold: Copy`). All unsafe blocks removed.
- `Arc<RefCell<Content>>` → `Arc<Mutex<Content>>` in module-level doc comment;
  concurrency rationale updated to match the actual implementation.

## [0.8.0] - 2026-05-16

## [0.7.0] - 2026-05-16

### Added

- `over_provisioned_range(line_count, top, height) -> Range<usize>` in the
  `viewport` module (re-exported from the crate root). Computes a 3×-viewport
  row range (one viewport above + current + one viewport below), clamped to the
  buffer's line count. Host-agnostic: takes only document line count and
  viewport extents — no terminal cells or pixels. Future GUI hosts call the same
  function with their own viewport dims. Previously this math lived inline in
  `apps/hjkl/src/app/syntax_glue.rs` (duplicated twice). Covered by 6 unit tests
  including boundary cases (top-of-buffer, bottom-of-buffer, zero height, empty
  buffer, and the invariant that the result always covers the original
  viewport).
- `is_big_viewport_jump(prev_top, cur_top, viewport_height) -> bool` — detects
  when a viewport scroll lands beyond the 3× over-provisioned band (jump >
  `viewport_height` in either direction). Hosts use this to decide whether to
  block briefly on a fresh parse so `gg` / `G` / `<C-d>` / `:N` don't flash
  un-highlighted rows. Host-agnostic: pure math, no terminal cells or pixels.
  Covered by 2 unit tests (within-band and out-of-band, including the boundary
  case: exactly `== height` is NOT a big jump; `height + 1` IS).
- `Gutter::sign_column_width: u16` field (default `0`). When non-zero, the
  gutter reserves the leftmost `sign_column_width` columns exclusively for signs
  (`paint_signs` writes into `area.x..area.x + sign_column_width`); line numbers
  are offset right by `sign_column_width` cells and text starts after the full
  gutter. Fixes 5-digit line number corruption where a `~` sign char would
  overwrite the leading digit (e.g. `~3109` instead of `13109`). Default `0`
  preserves the previous layout.

### Fixed

- Empty rows in a visual selection now paint a placeholder space cell carrying
  `selection_bg` for all three visual kinds (Char / Line / Block). Previously
  the char loop iterated zero chars on empty rows and the selection marker was
  lost, breaking the visual highlight across blank lines.
- Block-selection placeholder on empty rows now spans the full block column
  range (`lo..=hi` clipped to `row_end_x`) instead of a single col-0 cell.
  Previously the rectangle broke visually — empty rows in the middle of a column
  block showed nothing at the block's actual column range. Char and Line
  selections continue to paint a single col-0 marker.

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

[Unreleased]: https://github.com/kryptic-sh/hjkl-buffer/compare/v0.7.0...HEAD
[0.8.1]: https://github.com/kryptic-sh/hjkl-buffer/compare/v0.8.0...v0.8.1
[0.8.0]: https://github.com/kryptic-sh/hjkl-buffer/releases/tag/v0.8.0
[0.7.0]: https://github.com/kryptic-sh/hjkl-buffer/compare/v0.6.2...v0.7.0
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
