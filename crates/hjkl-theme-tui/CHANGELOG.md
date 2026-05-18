# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1] - 2026-05-18

### Changed

- Bumped `hjkl-theme` dependency from `0.1` to `0.2` (additive `loader` module;
  no API removals, pin update required due to 0.x caret-minor semantics).

## [0.1.0] - 2026-05-16

### Added

- `ToRatatui` extension trait with impls for `Color`, `Modifiers`, and
  `StyleSpec`.
- `Color::to_ratatui()` maps RGBA to `ratatui::style::Color::Rgb(r, g, b)`;
  alpha channel is dropped (ratatui has no alpha support).
- `Modifiers::to_ratatui()` maps the five modifier flags to
  `ratatui::style::Modifier` bitflags (`BOLD`, `ITALIC`, `UNDERLINED`,
  `REVERSED`, `CROSSED_OUT`).
- `StyleSpec::to_ratatui()` builds a `ratatui::style::Style` with optional fg/bg
  and the modifier set.
- Integration tests in `tests/convert.rs` covering all five modifier flags,
  fg-only `StyleSpec`, alpha-drop, empty modifiers, and a theme round-trip via
  `hjkl_theme::Theme::from_toml_str`.

### Notes

Issue #10 specifies `impl From<&Color> for ratatui::style::Color` etc., but
Rust's orphan rule forbids `impl ForeignTrait<ForeignType> for ForeignType` when
none of the types are local. Both `From` (in `core`) and all ratatui style types
are foreign to this crate, as are the `hjkl-theme` types. The standard escape
hatch is a local extension trait (`ToRatatui`), which gives equivalent
ergonomics (`value.to_ratatui()`) without violating coherence rules.

[Unreleased]: https://github.com/kryptic-sh/hjkl-theme-tui/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/kryptic-sh/hjkl-theme-tui/releases/tag/v0.1.1
[0.1.0]: https://github.com/kryptic-sh/hjkl-theme-tui/releases/tag/v0.1.0
