# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). This
project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Security

- The live-grep source passes the user query after a `--` separator for both the
  ripgrep and grep backends, so a query beginning with `-` can no longer be
  interpreted as an option (e.g. ripgrep's `--pre=<cmd>`, which runs an
  arbitrary command).

## [0.9.1] - 2026-05-18

### Changed

- Bump `hjkl-buffer` transitive dep to 0.8.1 (closes #158 multi-view UB). No API
  changes in this crate.

## [0.9.0] - 2026-05-17

### Changed

- Bumped pinned `hjkl-engine` `0.10` → `0.11`, `hjkl-form` `0.5` → `0.6` (#99
  cascade).

## [0.8.0] - 2026-05-17

### Changed

- Bumped pinned `hjkl-engine` `0.9` → `0.10`, `hjkl-form` `0.4` → `0.5` (#96
  cascade).

## [0.7.0] - 2026-05-16

### Added

- `Picker::path_for_visible_row(row) -> Option<PathBuf>` — returns the
  filesystem path for any currently visible row, enabling host-side right-click
  context menus without reaching into picker internals.

### Changed

- Bumped `hjkl-engine` dep from `0.8` to `0.9`.
- Bumped `hjkl-buffer` dep from `0.6` to `0.7`.
- Bumped `hjkl-form` dep from `0.3` to `0.4`.

### Fixed

- `parse_grep_line` now strips Windows drive-letter prefixes (`C:`) before
  splitting on `:`, unblocking grep-backed picker sessions on Windows paths.
  Five new unit tests cover drive-letter, UNC-adjacent, and mixed-separator
  inputs.

## [0.6.0] - 2026-05-15

### Changed

- **Breaking: headless refactor.** `hjkl-picker` no longer depends on `ratatui`
  or `crossterm`. The TUI surface (preview-pane renderer, key-event router)
  moves to a new sibling crate `hjkl-picker-tui`
  ([hjkl#21](https://github.com/kryptic-sh/hjkl/issues/21)). Public API changes:
  - `Picker::handle_key(crossterm::event::KeyEvent) -> PickerEvent` removed.
    Replaced by six smaller methods so renderer adapters own key decoding:
    `cancel()`, `accept()`, `select_next()`, `select_prev()`,
    `handle_query_input(hjkl_engine::Input)`,
    `handle_source_key(hjkl_engine::Input) -> Option<PickerAction>`.
  - `PickerLogic::handle_key` parameter type changed from
    `crossterm::event::KeyEvent` to `hjkl_engine::Input`.
  - `PickerLogic::label_styles` and `Picker::visible_entry_styles` return type
    changed from `ratatui::style::Style` to `hjkl_engine::types::Style`.
  - `PreviewSpans` style storage migrated to `hjkl_engine::types::Style` for the
    same reason; `build_preview_spans` / `PreviewSpans::from_byte_ranges`
    signatures updated.
  - Removed: `pub mod render`, `PreviewTheme`, `preview_pane`. Their
    replacements live in `hjkl-picker-tui`.

### Why

Unblocks the floem GUI binary work
([hjkl#8](https://github.com/kryptic-sh/hjkl/issues/8)). Renderer-agnostic logic
now sits cleanly behind the engine's `Input` / `Style` types; ratatui + floem
adapters become parallel siblings, neither bleeds into the other's dep cone.

## [0.5.2] - 2026-05-12

### Added

- `PreviewHighlighter::spans_for_viewport(path, bytes, top_row, height)` —
  viewport-aware variant called by `preview_pane`. Default implementation
  delegates to `spans_for`, so existing impls are source-compatible. Consumers
  with expensive highlighters (tree-sitter with injections, etc.) should
  override this and clip work to the visible window. Fixes the markdown preview
  hitch in `kryptic-sh/hjkl#65`.

### Changed

- `preview_pane` now calls `spans_for_viewport` instead of `spans_for`, passing
  `picker.preview_top_row()` and the pane height.

## [0.5.1] - 2026-05-10

### Changed

- Picker preview `BufferView` construction now passes the `colorcolumn_cols` and
  `colorcolumn_style` fields required by `hjkl-buffer 0.6.0`. Both are defaulted
  to empty / no-op — picker preview does not render a colorcolumn ruler.
- Bumped `hjkl-buffer` dep from `^0.5` to `^0.6`.

## [0.5.0] - 2026-05-06

### Breaking

- **Bonsai-agnostic preview API.** Direct `hjkl-bonsai` dep removed from the
  picker. `preview_pane()` renderer now takes `&dyn PreviewHighlighter` instead
  of a concrete bonsai type. Consumers supply a `PlainPreview` (no-op) or their
  own impl. `PreviewHighlighter`, `PlainPreview`, `PreviewTheme`, and
  `preview_pane` are re-exported at the crate root.

### Fixed

- Substring match outranks scattered subsequence: when the needle appears as a
  contiguous substring of the candidate the fast-path score beats any scattered
  subsequence match, giving more intuitive top-of-list results.

### Changed

- Bumped `hjkl-buffer` dep requirement to `^0.5`.

## [0.4.0] - 2026-05-04

### Breaking

- **`PickerAction` reduced to `Custom(Box<dyn Any + Send>)` + `None`.** The enum
  no longer carries app-specific variants (`OpenPath`, `OpenPathAtLine`,
  `ShowCommit`, `CheckoutBranch`, `SwitchSlot`, `StashApply`, `StashPop`,
  `StashDrop` all removed). Consumers define their own action enum and box it;
  the dispatcher downcasts via `Box<dyn Any>::downcast`. Keeps the library
  reusable from any app without leaking file/git/buffer-specific dependencies.
- **`GitStatusSource` removed.** Moved to consumer apps. Previously the picker
  shipped a built-in source that depended on `git2`; that coupling is gone.

### Added

- `PickerLogic::handle_key(idx, key) -> Option<PickerAction>` — sources can
  intercept keys before they fall through to the query input. Picker calls it
  after built-in nav (Enter / arrows / Ctrl+N+P) so reserved keys still work.
  Default returns `None`.
- `PickerLogic::preserve_source_order() -> bool` — opt-in for sources whose
  enumeration order is meaningful (e.g. git log chronological, branches with
  HEAD-first sort). Default `false` keeps the existing alphabetical-on-empty-
  query behavior.
- `PickerLogic::label_styles(idx, label) -> Option<Vec<(Range, Style)>>` —
  optional per-row semantic styling for labels. Char-index ranges with a base
  style; fuzzy-match positions overlay these. Default `None`.

## [0.3.2] - 2026-05-03

### Docs

- Dropped 0.3.0 milestone callout from the README status section. Per the org's
  "no SPEC frozen claims" stance.

## [0.3.1] - 2026-04-30

### Changed

- Migrated `hjkl-picker` from the `kryptic-sh/hjkl` monorepo into its own
  repository
  ([kryptic-sh/hjkl-picker](https://github.com/kryptic-sh/hjkl-picker)) with
  full git history preserved.
- Relaxed inter-crate dependency requirements from `=0.3.0` to `0.3` (caret),
  matching the standard SemVer pattern for library dependencies.
- Bumped `ratatui` to 0.30 (was 0.29) and `crossterm` to 0.29 (was 0.28).

### Added

- Standalone `LICENSE`, `.gitignore`, and `ci.yml` workflow at the repo root.

[Unreleased]: https://github.com/kryptic-sh/hjkl-picker/compare/v0.9.0...HEAD
[0.9.1]: https://github.com/kryptic-sh/hjkl-picker/compare/v0.9.0...v0.9.1
[0.9.0]: https://github.com/kryptic-sh/hjkl-picker/compare/v0.8.0...v0.9.0
[0.8.0]: https://github.com/kryptic-sh/hjkl-picker/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/kryptic-sh/hjkl-picker/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/kryptic-sh/hjkl-picker/compare/v0.5.2...v0.6.0
[0.5.2]: https://github.com/kryptic-sh/hjkl-picker/releases/tag/v0.5.2
[0.5.1]: https://github.com/kryptic-sh/hjkl-picker/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/kryptic-sh/hjkl-picker/releases/tag/v0.5.0
[0.4.0]: https://github.com/kryptic-sh/hjkl-picker/releases/tag/v0.4.0
[0.3.2]: https://github.com/kryptic-sh/hjkl-picker/releases/tag/v0.3.2
[0.3.1]: https://github.com/kryptic-sh/hjkl-picker/releases/tag/v0.3.1
