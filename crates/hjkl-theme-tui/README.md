# hjkl-theme-tui

Ratatui adapters for hjkl-theme: Color/StyleSpec/Modifiers conversions.

[![CI](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hjkl-theme-tui.svg)](https://crates.io/crates/hjkl-theme-tui)
[![docs.rs](https://img.shields.io/docsrs/hjkl-theme-tui)](https://docs.rs/hjkl-theme-tui)
[![MSRV](https://img.shields.io/badge/MSRV-1.95-blue.svg)](Cargo.toml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE)

Part of the [hjkl monorepo](https://github.com/kryptic-sh/hjkl) — a vim-modal
editor in Rust.

**Status:** Phase 2 — ratatui adapter, paired with `hjkl-theme` (phase 1).

Converts `hjkl-theme` types (`Color`, `StyleSpec`, `Modifiers`) into their
`ratatui::style` equivalents. Pure conversion; no TOML parsing, no schema work.

## Usage

```rust
use hjkl_theme::{Color, Modifiers, StyleSpec};
use hjkl_theme_tui::ToRatatui;

let color = Color::rgb(0x89, 0xb4, 0xfa);
let rcolor = color.to_ratatui(); // ratatui::style::Color::Rgb(...)

let spec = StyleSpec {
    fg: Some(color),
    bg: None,
    modifiers: Modifiers { bold: true, ..Default::default() },
};
let style = spec.to_ratatui(); // ratatui::style::Style
```

## Documentation

[docs.rs/hjkl-theme-tui](https://docs.rs/hjkl-theme-tui)

## Contributing

See the
[monorepo CONTRIBUTING guide](https://github.com/kryptic-sh/hjkl/blob/main/CONTRIBUTING.md).

## License

MIT — see [LICENSE](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE).
