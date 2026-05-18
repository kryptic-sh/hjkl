# hjkl-theme-tui

Ratatui adapter crate for
[hjkl-theme](https://github.com/kryptic-sh/hjkl-theme).

**Status:** Phase 2 — ratatui adapter, paired with `hjkl-theme` (phase 1).

## What it is

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

## MSRV

Rust 1.95
