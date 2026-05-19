# Changelog

All notable changes to `hjkl-statusline` are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). This crate tracks the
workspace lockstep version.

## [Unreleased]

### Changed

- `Segment::Text.content` changed from `String` to `Cow<'static, str>`. Static
  labels (e.g. mode badges, dirty markers) are now zero-allocation borrowed
  `&'static str`; dynamically-built strings use the owned path. All in-tree
  callers updated.
- `percent_segment` now accepts a `ModeKind` parameter. The badge bg/fg now
  echoes the active mode (Normal = blue, Insert = green, Visual = orange)
  instead of always using `mode_normal_*` colors.
- `truncate_filename` now uses `char_indices()` to locate a valid UTF-8 char
  boundary before slicing, preventing panics on non-ASCII (multibyte) filenames
  at the truncation boundary.
- `StatusTheme` gains four new diag-color fields: `diag_error_fg`,
  `diag_warning_fg`, `diag_info_fg`, `diag_hint_fg`. The `apps/hjkl` render path
  reads these instead of hardcoding RGB, so the host controls the severity-color
  palette. Defaults match the previous hardcoded values.
- Four new unit tests: `readonly_and_dirty_both_shown`,
  `percent_segment_empty_buffer_no_panic`,
  `bar_layout_right_alone_exceeds_width_no_panic`,
  `recording_segment_shows_register`. Closes #135.

[Unreleased]: https://github.com/kryptic-sh/hjkl/compare/v0.25.1...HEAD
