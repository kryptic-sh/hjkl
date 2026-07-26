# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

## [0.4.0] - 2026-05-18

### Changed (breaking)

- **`ResolvedStyle::iter()` returns CSS source order**, not alphabetical.
  Properties are sorted by `(rule_idx, decl_idx)` so adapters that apply
  declarations sequentially get spec-correct shorthand/longhand override
  semantics. Snapshot tests that depended on alphabetical order must sort the
  collected `Vec` themselves.
- **Cascade key extended to `(important, specificity, rule_idx, decl_idx)`.**
  Intra-rule declaration position is now part of cascade tie-breaking; later
  declarations within the same rule win over earlier ones at the same
  specificity (matching CSS spec).
- **`PropertyKind::NumberOrKeyword` removed.** `font-weight` is now dispatched
  via a dedicated `PropertyKind::FontWeight`; no other property used the old
  variant.

### Added

- `font-style: oblique` accepted alongside `normal` / `italic`. (#3)
- `border-top-color` / `border-right-color` / `border-bottom-color` /
  `border-left-color` accepted as `PropertyKind::Color`. (#4)
- `font-weight` value validation: integers outside `1..=1000` or with fractional
  parts are rejected at parse time. (#5)

### Fixed

- `ResolvedStyle::iter()` source-order semantics. (#6) Without this, an adapter
  walking `border` then `border-color` always saw `border` first (alphabetical),
  silently inverting the cascade for shorthand/longhand collisions regardless of
  CSS source order.

### Project hygiene

- Backfilled org-wide boilerplate: CI workflow, `deny.toml`,
  `rust-toolchain.toml`, `rustfmt.toml`, `.editorconfig`, `CHANGELOG.md`,
  `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, `SECURITY.md`.
- `Cargo.lock` untracked (library convention).
- Removed stray 5.8 MB ELF `test` binary from repo root.
- README rewritten to reflect current property surface, combinator support, and
  `resolve()` signature.

## [0.3.1] - 2026-05-12

### Fixed

- Whitespace handling around CSS comments inside selectors:
  `.a /* comment */ .b` now parses as a descendant combinator instead of
  silently dropping the rule.
- `border` shorthand accepts any style keyword (e.g. `dashed`, `dotted`);
  unsupported styles are ignored rather than failing the declaration.
- `expand_side_set` covered by a regression test.

## [0.3.0] - 2026-05-12

### Added

- Selector combinators: descendant (` `), child (`>`), adjacent sibling (`+`),
  general sibling (`~`). `Selector` now carries `parts` + `combinators`;
  `Selector::matches` walks right-to-left through the chain.

### Changed (breaking)

- `Stylesheet::resolve(target: &Node, ancestors: &[Node], prev_siblings: &[Node], state: Option<PseudoClass>)`
  — new signature accepting full tree context.

## [0.2.0] - 2026-05-09

### Added

- Phase-2 property surface: `display`, `flex-direction`, `flex-grow`,
  `flex-shrink`, `flex-basis`, `align-items`, `justify-content`, `gap`,
  `row-gap`, `column-gap`, `border` (1..=4 sides), `border-color`,
  `border-width`, `border-radius`, `outline`, `font-family`, `font-size`,
  `font-weight`, `font-style`, `text-align`, `line-height`.
- `Value` variants: `Keyword`, `Auto`, `Number`, `FontFamilyList`, `Border`,
  `SideSet` for mixed length/`auto` shorthands.

## [0.1.0] - 2026-05-03

### Added

- Initial release. CSS parser built on `cssparser 0.37`. AST for stylesheets,
  rules, selectors (type / class / pseudo-class / AND-compound), declarations.
  Cascade with specificity + `!important` + source-order tie-break. Phase-1
  properties: `color`, `background-color`, `width`, `height`, `padding`,
  `margin`.

[Unreleased]: https://github.com/kryptic-sh/hjkl-css/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/kryptic-sh/hjkl-css/releases/tag/v0.4.0
[0.3.1]: https://github.com/kryptic-sh/hjkl-css/releases/tag/v0.3.1
[0.3.0]: https://github.com/kryptic-sh/hjkl-css/releases/tag/v0.3.0
[0.2.0]: https://github.com/kryptic-sh/hjkl-css/releases/tag/v0.2.0
[0.1.0]: https://github.com/kryptic-sh/hjkl-css/releases/tag/v0.1.0
