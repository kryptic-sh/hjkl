# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Changed

- Crate renamed from `hjkl-css-floem` to `hjkl-css-gui`. The previous name is
  dead — no shim. (will publish at next umbrella release)

## [0.2.1] - 2026-05-18

### Fixed

- CI `cargo-deny` job: ignore the `paste` unmaintained advisory
  (RUSTSEC-2024-0436) — pulled transitively via floem→wgpu→wgpu-hal→metal with
  no safe upgrade path on floem 0.2. Allow git sources by GitHub org
  (`kryptic-sh`, `mxaddict`) so trailing-slash and `.git`-suffix variants all
  match. Add `version = "0.4"` alongside the `hjkl-css` git tag so the
  `wildcards = "deny"` ban passes; cargo resolves git locally, registry on
  publish.

## [0.2.0] - 2026-05-18

### Added

- Wire `font-style: oblique` — new `Oblique` arm in `apply_one` maps to
  `floem::text::Style::Oblique` (introduced in hjkl-css v0.4.0 parser).
- Wire per-side border color longhands: `border-top-color`,
  `border-right-color`, `border-bottom-color`, `border-left-color` are now
  first-class `PropertyKind::Color` in hjkl-css v0.4.0 and are routed to
  `s.border_color(...)`. floem 0.2 collapses all four to a single brush
  (last-write-wins); the per-side limitation comment documents this.
- Integration test `integration_label_view_with_css` (approach a): constructs
  `label`, `stack`, and `container` views, chains `.css(...)` and `.css_in(...)`
  on each, and confirms the calls typecheck and run without panic. No headless
  floem runtime required — the style closure is captured but not reactor-driven.
- Tests `font_style_oblique_resolves` and `border_side_color_longhands_resolve`
  pin the two new property arms.

### Changed

- Bump `hjkl-css` dependency from `v0.3.1` to `v0.4.0`.
- `apply_border` doc comment: removed the "Shorthand vs longhand cascade" caveat
  (alphabetical iter order) — hjkl-css v0.4.0's `iter()` now returns properties
  in CSS source order. Replaced with a positive note confirming source order is
  honoured upstream.

- **Behaviour change for adopters.** Cascade ordering for shorthand/longhand
  collisions now follows CSS source order, not alphabetical key order. Prior
  versions of `hjkl-css` iterated properties alphabetically, so `border-color`
  always applied after `border` regardless of source order. With v0.4.0, source
  order wins — `x { border-color: blue; border: 1px solid red; }` now resolves
  to red on the right of the cascade, not blue. Audit any stylesheets that
  depended on the old behaviour.

### Project hygiene

- Backfilled org-wide boilerplate: CI workflow, `deny.toml`,
  `rust-toolchain.toml`, `rustfmt.toml`, `.editorconfig`, `CHANGELOG.md`,
  `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, `SECURITY.md`.

## [0.1.0] - 2026-05-12

### Added

- Initial release. `ViewCssExt` extension trait that maps a
  `hjkl_css::Stylesheet` onto floem `Style` setters via a blanket impl over
  `Decorators + Sized`. Eager pseudo-state resolution (`:hover` / `:focus` /
  `:active` / `:disabled` / `:selected`) chained into floem's interaction
  wiring. Six properties wired: `color`, `background-color`, `width`, `height`,
  `padding`, `margin`.
- Documents the workspace-root `[patch.crates-io]` requirement for the
  layer-shell floem fork.

[Unreleased]: https://github.com/kryptic-sh/hjkl-css-gui/compare/v0.2.1...HEAD
[0.2.1]: https://github.com/kryptic-sh/hjkl-css-gui/releases/tag/v0.2.1
[0.2.0]: https://github.com/kryptic-sh/hjkl-css-gui/releases/tag/v0.2.0
[0.1.0]: https://github.com/kryptic-sh/hjkl-css-gui/releases/tag/v0.1.0
