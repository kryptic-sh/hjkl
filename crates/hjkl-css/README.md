# hjkl-css

Parser + AST for a CSS subset used to drive declarative UI styling.

[![CI](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hjkl-css.svg)](https://crates.io/crates/hjkl-css)
[![docs.rs](https://img.shields.io/docsrs/hjkl-css)](https://docs.rs/hjkl-css)
[![MSRV](https://img.shields.io/badge/MSRV-1.95-blue.svg)](Cargo.toml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE)

Part of the [hjkl monorepo](https://github.com/kryptic-sh/hjkl) — a vim-modal
editor in Rust.

Toolkit-agnostic. Pair with `hjkl-css-floem` to map onto
[`floem`](https://github.com/lapce/floem) views.

## Supported subset

- **Selectors**: type (`label`), class (`.prompt`), pseudo-class (`:hover`,
  `:focus`, `:active`, `:disabled`, `:selected`), and AND-combinations on a
  single simple selector (`button.primary:hover`). Selector lists (`.a, .b`)
  honoured. Combinators supported: descendant (` `), child (`>`), adjacent
  sibling (`+`), general sibling (`~`).
- **Properties** (31 known): `color`, `background-color`, `width`, `height`,
  `flex-basis`, `padding`, `margin`, `gap`, `row-gap`, `column-gap`, `display`,
  `flex-direction`, `align-items`, `justify-content`, `flex-grow`,
  `flex-shrink`, `border`, `border-top`, `border-right`, `border-bottom`,
  `border-left`, `border-width`, `border-color`, `border-top-color`,
  `border-right-color`, `border-bottom-color`, `border-left-color`,
  `border-radius`, `outline`, `font-family`, `font-size`, `font-weight`,
  `font-style`, `text-align`, `line-height`. Unknown properties are silently
  dropped per the lenient-parsing posture.
- **Values**: hex colors (`#rgb`, `#rrggbb`, `#rrggbbaa`), `rgb()` / `rgba()`
  functions, named colors (CSS Level 1), lengths in `px` / `%` / unitless
  (treated as px), `auto` keyword, unitless numbers, font-family lists, border
  shorthand (`<width> <style> <color>`).
- **Cascade**: standard specificity (classes/pseudo = 10, type = 1);
  `!important` boost; CSS source-order tie-break (rule index, then in-rule
  declaration index).

## Usage

```rust
use hjkl_css::{parse, Node, Value, Color};

let sheet = parse(r#"
    label { color: #ccc; padding: 4px 8px; }
    .prompt { color: #21d1d3; }
    .row:hover { background-color: rgba(0, 0, 0, 0.2); }
"#).expect("stylesheet parses");

let target = Node { element: "label", classes: &["prompt"] };
let style = sheet.resolve(&target, &[], &[], None);
assert_eq!(
    style.get("color"),
    Some(&Value::Color(Color::rgb(0x21, 0xd1, 0xd3))),
);
```

`resolve` takes the target node, its ancestors (root → parent, exclusive of
target), its previous siblings (oldest → immediately preceding, exclusive of
target), and the active pseudo-class. Pass empty slices for top-level views with
no hierarchy.

`ResolvedStyle::iter()` yields properties in CSS source order, so adapters that
apply properties sequentially get spec-correct shorthand/longhand override
semantics.

## Documentation

[docs.rs/hjkl-css](https://docs.rs/hjkl-css)

## Contributing

See the
[monorepo CONTRIBUTING guide](https://github.com/kryptic-sh/hjkl/blob/main/CONTRIBUTING.md).

## License

MIT — see [LICENSE](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE).
