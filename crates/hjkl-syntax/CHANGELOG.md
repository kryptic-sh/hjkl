# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- `SyntaxLayer::apply_edits` drops the span cache along with the parse,
  row-start and sign caches. It cleared only the latter three and left
  `cache_spans` to be dropped by the `dirty_gen` mismatch in `render_viewport` —
  which does happen for a real buffer edit, so this was not reachable from the
  app, but it made the function's correctness depend on a counter it neither
  reads nor controls. A caller that edited through `apply_edits` without the
  buffer's `dirty_gen` moving got the PRE-edit spans back: byte ranges one short
  across the row and the edited identifier's captures unchanged, even though the
  tree was correctly reparsed. This is what
  `incremental_path_matches_cold_for_small_edit` had been failing on.

### Added

- Added changelog.

[unreleased]: https://github.com/kryptic-sh/hjkl/compare/v0.40.0...HEAD
