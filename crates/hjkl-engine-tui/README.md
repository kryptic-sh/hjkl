# hjkl-engine-tui

Ratatui adapters for [`hjkl-engine`](https://crates.io/crates/hjkl-engine).

Provides free conversion functions and an extension trait for
[`hjkl_engine::Editor`] that expose ratatui-flavoured style interning and
syntax-span installation. Extracted from `hjkl-engine` as part of #162
(Host-trait phase 2) so the agnostic engine crate carries no ratatui dependency.

[![CI](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hjkl-engine-tui.svg)](https://crates.io/crates/hjkl-engine-tui)
[![docs.rs](https://img.shields.io/docsrs/hjkl-engine-tui)](https://docs.rs/hjkl-engine-tui)
[![MSRV](https://img.shields.io/badge/MSRV-1.95-blue.svg)](Cargo.toml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE)

Part of the [hjkl monorepo](https://github.com/kryptic-sh/hjkl) — a vim-modal
editor in Rust.

## Usage

```rust
use hjkl_engine::{Editor, types::DefaultHost};
use hjkl_engine_tui::{style_to_ratatui, style_from_ratatui, EditorRatatuiExt};
use ratatui::style::{Color, Style};

let mut editor = Editor::new(
    hjkl_buffer::Buffer::new(),
    DefaultHost::new(),
    hjkl_engine::types::Options::default(),
);

// Install ratatui-flavoured syntax spans.
editor.install_ratatui_syntax_spans(vec![vec![
    (0, 6, Style::default().fg(Color::Red)),
]]);

// Convert individual styles.
let engine_style = hjkl_engine::types::Style::default();
let ratatui_style = style_to_ratatui(engine_style);
```

## Documentation

[docs.rs/hjkl-engine-tui](https://docs.rs/hjkl-engine-tui)

## License

MIT — see [LICENSE](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE).
