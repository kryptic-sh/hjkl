# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). This
project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.40.0] - 2026-08-01

### Breaking

- `BufferView` gained a required `cursor_column: Option<usize>` field. It is the
  counterpart of `cursor_line_row`: a host with per-window cursors must pass the
  window's column, because `BufferView.buffer` is the shared buffer whose cursor
  is a different object. `None` keeps the old behaviour (read the buffer's own
  cursor), which is correct for a single-cursor host.

### Fixed

- `cursorcolumn` painted column 0 rather than the cursor's column, for any host
  whose real cursor lives outside `BufferView.buffer` — see the new field above.
  The pass also used the cursor's CHAR column directly as a screen offset, so a
  tab earlier in the line shifted the bar one cell left per tab; it now goes
  through `char_col_to_visual_col`, the same expansion `paint_row` applies.

### Security

- Control characters in rendered buffer text are neutralized: C0/C1 controls and
  DEL now map to visible single-width glyphs (the Unicode Control Pictures
  block) instead of being written to the terminal verbatim. Previously a file
  containing raw escape sequences (OSC 52 clipboard writes, title spoofing, and
  other terminal control) could act on the host terminal merely by being
  displayed. The replacement glyphs are single-width, matching the width already
  assigned to control characters, so no column or cursor math changes.

## [0.25.0] - 2026-05-18

### Added

- Initial release. Extracted from `hjkl-buffer` 0.25.0 (the ratatui Widget impl
  previously lived behind `hjkl-buffer`'s `ratatui` feature gate, dropped as
  part of #162).

[0.25.0]: https://github.com/kryptic-sh/hjkl-buffer-tui/releases/tag/v0.25.0
