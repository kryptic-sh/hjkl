# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). This
project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

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
