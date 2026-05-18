# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). This
project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.2.0] - 2026-05-18

### Added

- `loader` module: free-function API for theme loading — `parse_toml`,
  `load_from_path`, `resolve_palette_refs`, and `default_theme()`.
- `loader::default_theme()`: bundled minimal dark palette embedded at compile
  time (`themes/default.toml`); covers common tree-sitter capture groups and
  basic `[ui]` surface fields.
- `themes/default.toml`: canonical built-in dark theme (neutral dark base with a
  Catppuccin-inspired palette; full `@markup.*` coverage).

## [0.1.0] - 2026-05-16

### Added

- `Color`: RGBA u8x4 type with hex parser (`#rgb`, `#rrggbb`, `#rrggbbaa`) and
  serde support.
- `StyleSpec`: fg/bg `Color` pair plus `Modifiers` (bold, italic, underline,
  reverse, strikethrough).
- `Palette`: `HashMap<String, Color>` with `$name` resolution at parse time.
- `CaptureMap`: fallback-chain lookup (`@function.builtin` -> `@function`) via
  `resolve`.
- `UiStyles`: typed surface fields for background, foreground, cursor,
  statusline, gutter, popup, selection, diagnostics.
- `Theme::from_toml_str` / `Theme::from_path`: two-stage parse (raw `ColorRef`
  -> resolved `Color`).
- `ThemeError`: typed error enum covering I/O, TOML parse, bad hex, unresolved
  palette refs, bad modifiers.
- TOML shorthand: `"@capture" = "#hex"` deserializes as `StyleSpec { fg, .. }`.
- Test suite covering palette resolution, fallback chain, shorthand/full forms,
  hex parsing, error cases.

[Unreleased]: https://github.com/kryptic-sh/hjkl-theme/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/kryptic-sh/hjkl-theme/releases/tag/v0.2.0
[0.1.0]: https://github.com/kryptic-sh/hjkl-theme/releases/tag/v0.1.0
