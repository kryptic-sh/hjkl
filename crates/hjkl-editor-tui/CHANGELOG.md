# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). This
project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.0] - 2026-05-17

### Added

- Initial release. Successor to `hjkl-ratatui` — same implementation,
  subsystem-named package (#100).
- `engine_to_ratatui_style` / `ratatui_to_engine_style` — lossless style bridge
  between `hjkl_engine::Style` and `ratatui::style::Style`.
- `engine_to_ratatui_color` / `ratatui_to_engine_color` — RGB and xterm-256
  color conversion.
- `engine_to_ratatui_attrs` / `ratatui_to_engine_attrs` — attribute flag
  conversion (bold, italic, underline, reverse, dim, strikethrough).
- `crossterm_key_event_to_input` (behind `crossterm` feature, on by default) —
  bridges `crossterm::event::KeyEvent` into `hjkl_engine::PlannedInput`.
- `form::draw_form` / `form::draw_form_into` — ratatui renderer for
  `hjkl_form::Form` with `FormPalette` theming.
- `prompt::draw_prompt_line` / `prompt::draw_prompt_line_into` — single-line
  vim-grammar prompt renderer (`:`, `/`, `?` prefixes).
- `spinner::frame` — animated braille spinner at ~8 Hz.

[Unreleased]:
  https://github.com/kryptic-sh/hjkl-editor-tui/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/kryptic-sh/hjkl-editor-tui/releases/tag/v0.1.0
