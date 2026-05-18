# hjkl-css-gui

Renamed from `hjkl-css-floem` 2026-05-18 to match the `-gui` floem-adapter
convention used elsewhere in hjkl (e.g. `hjkl-editor-gui`).

Adapter that maps a `hjkl-css` `Stylesheet` onto
[`floem`](https://github.com/lapce/floem) views via an extension trait.

[![CI](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hjkl-css-gui.svg)](https://crates.io/crates/hjkl-css-gui)
[![docs.rs](https://img.shields.io/docsrs/hjkl-css-gui)](https://docs.rs/hjkl-css-gui)
[![MSRV](https://img.shields.io/badge/MSRV-1.95-blue.svg)](Cargo.toml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE)

Part of the [hjkl monorepo](https://github.com/kryptic-sh/hjkl) — a vim-modal
editor in Rust.

## Usage

### Flat call (no combinator context)

```rust
use hjkl_css::parse;
use hjkl_css_gui::ViewCssExt;

let sheet = parse(r#"
    label { color: #c0caf5; padding: 4px 8px; }
    label.prompt { color: #21d1d3; }
    .row:hover { background-color: rgba(33, 209, 211, 0.18); }
"#).unwrap();

let view = floem::views::label(|| "hello")
    .css(&sheet, "label", &["prompt"]);
```

`.css(sheet, element, classes)` resolves the cascade for the base state plus
every supported pseudo (`:hover`, `:focus`, `:active`, `:disabled`, `:selected`)
and merges the resulting property bags into a single floem `Style` block,
delegating per-state behaviour to floem's own `.hover()` / `.focus()` / etc.
chain.

### Context-aware call (combinator selectors)

Use `.css_in(...)` when the view lives inside a hierarchy and descendant (` `),
child (`>`), adjacent-sibling (`+`), or general-sibling (`~`) selectors need to
match:

```rust
use hjkl_css::{Node, parse};
use hjkl_css_gui::ViewCssExt;

let sheet = parse(".row .label { color: #fff; }").unwrap();

let target = Node { element: "span", classes: &["label"] };
let ancestors = [Node { element: "div", classes: &["row"] }];

let view = floem::views::label(|| "hello")
    .css_in(&sheet, target, &ancestors, &[]);
```

`ancestors` is root→parent (exclusive of target); `prev_siblings` is
oldest→immediately-preceding sibling (exclusive of target).

## Supported properties (hjkl-css v0.4.0)

| CSS property                   | Notes                                                                     |
| ------------------------------ | ------------------------------------------------------------------------- |
| `color`                        |                                                                           |
| `background-color`             |                                                                           |
| `width`, `height`              | `auto` supported                                                          |
| `padding`                      | 1..=4 length shorthand                                                    |
| `margin`                       | 1..=4 length/auto shorthand; `auto` and mixed `4px auto` both work        |
| `gap`, `row-gap`, `column-gap` |                                                                           |
| `display`                      | `flex`, `block`, `none`                                                   |
| `flex-direction`               | `row`, `column`, `row-reverse`, `column-reverse`                          |
| `align-items`                  | `start`, `end`, `center`, `stretch`, `baseline`                           |
| `justify-content`              | `start`, `end`, `center`, `space-between`, `space-around`, `space-evenly` |
| `flex-grow`, `flex-shrink`     | unitless number                                                           |
| `flex-basis`                   | length or `auto`                                                          |
| `border`                       | shorthand — sets all four sides + `border-color`                          |
| `border-top/right/bottom/left` | per-side shorthand                                                        |
| `border-width`                 | 1..=4 side shorthand (px only)                                            |
| `border-color`                 |                                                                           |
| `border-top-color`             | collapses to global brush — floem 0.2 has no per-side color setter        |
| `border-right-color`           | collapses to global brush — floem 0.2 has no per-side color setter        |
| `border-bottom-color`          | collapses to global brush — floem 0.2 has no per-side color setter        |
| `border-left-color`            | collapses to global brush — floem 0.2 has no per-side color setter        |
| `border-radius`                | first value used as uniform radius; floem 0.2 has no per-corner API       |
| `outline`                      | width + color forwarded via `outline` + `outline_color`                   |
| `font-size`                    | px only — floem 0.2 `font_size` takes `Px`                                |
| `font-weight`                  | numeric (e.g. `700`) or keyword (`normal`, `bold`)                        |
| `font-style`                   | `normal`, `italic`, `oblique`                                             |
| `font-family`                  | first family name used; floem 0.2 accepts one `String`                    |
| `line-height`                  | unitless multiplier only; px variant silently skipped                     |
| `text-align`                   | **gap**: floem 0.2 has no `text_align` setter — parsed but not forwarded  |

Unknown properties are silently skipped per the CSS lenient-parsing posture.

## Selectors

Supported via hjkl-css v0.4.0:

- Type selectors: `label`
- Class selectors: `.prompt`
- Pseudo-class selectors: `:hover`, `:focus`, `:active`, `:disabled`,
  `:selected`
- Compound selectors: `button.primary:hover`
- Combinators (require `.css_in(...)`): descendant ` `, child `>`,
  adjacent-sibling `+`, general-sibling `~`
- Selector lists: `label, .btn { … }`

## Documentation

[docs.rs/hjkl-css-gui](https://docs.rs/hjkl-css-gui)

## Contributing

See the
[monorepo CONTRIBUTING guide](https://github.com/kryptic-sh/hjkl/blob/main/CONTRIBUTING.md).

## License

MIT — see [LICENSE](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE).
