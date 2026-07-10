# hjkl-buffer-tui

Ratatui Widget adapter for
[`hjkl-buffer`](https://crates.io/crates/hjkl-buffer).

Provides a single-pass cell renderer [`BufferView`] that implements
`ratatui::widgets::Widget`. Extracted from `hjkl-buffer` as part of #162
(Host-trait phase 4) so the agnostic buffer crate carries no ratatui dependency.

[![CI](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hjkl-buffer-tui.svg)](https://crates.io/crates/hjkl-buffer-tui)
[![docs.rs](https://img.shields.io/docsrs/hjkl-buffer-tui)](https://docs.rs/hjkl-buffer-tui)
[![MSRV](https://img.shields.io/badge/MSRV-1.95-blue.svg)](Cargo.toml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE)

Part of the [hjkl monorepo](https://github.com/kryptic-sh/hjkl) — a vim-modal
editor in Rust.

## Usage

```rust
use hjkl_buffer::{Buffer, Viewport, Wrap};
use hjkl_buffer_tui::{BufferView, StyleResolver};
use ratatui::style::Style;

let buf = Buffer::from_str("hello\nworld");
let vp = Viewport { top_row: 0, top_col: 0, width: 80, height: 24, wrap: Wrap::None, text_width: 80, tab_width: 0 };

let view = BufferView {
    buffer: &buf,
    viewport: &vp,
    selection: None,
    resolver: &(|_id: u32| Style::default()),
    cursor_line_bg: Style::default(),
    cursor_column_bg: Style::default(),
    selection_bg: Style::default(),
    cursor_style: Style::default(),
    gutter: None,
    search_bg: Style::default(),
    signs: &[],
    conceals: &[],
    spans: &[],
    search_pattern: None,
    non_text_style: Style::default(),
    diag_overlays: &[],
    colorcolumn_cols: &[],
    colorcolumn_style: Style::default(),
};
// frame.render_widget(view, area);
```

## Documentation

[docs.rs/hjkl-buffer-tui](https://docs.rs/hjkl-buffer-tui)

## License

MIT — see [LICENSE](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE).
