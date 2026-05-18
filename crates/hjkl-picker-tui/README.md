# hjkl-picker-tui

Ratatui adapter for hjkl-picker — preview pane renderer + crossterm key router.

[![CI](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hjkl-picker-tui.svg)](https://crates.io/crates/hjkl-picker-tui)
[![docs.rs](https://img.shields.io/docsrs/hjkl-picker-tui)](https://docs.rs/hjkl-picker-tui)
[![MSRV](https://img.shields.io/badge/MSRV-1.95-blue.svg)](Cargo.toml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE)

Part of the [hjkl monorepo](https://github.com/kryptic-sh/hjkl) — a vim-modal
editor in Rust.

Thin glue layer between the backend-agnostic
[`hjkl-picker`](https://crates.io/crates/hjkl-picker) crate and a ratatui
terminal UI. Owns the crossterm key-event translation and the syntax-highlighted
preview pane widget; the picker state and fuzzy scoring stay in `hjkl-picker`.

## What lives here

- `handle_key` — translates a `crossterm::event::KeyEvent` into the appropriate
  `Picker` method call and returns a `PickerEvent`. Handles `Esc` / `C-c`
  (cancel), `Enter` (accept), `Down` / `C-n` / `Up` / `C-p` (navigation), and
  forwards all other keys to the query field or source handler.
- `preview_pane` — renders the active picker item's preview buffer into a
  `ratatui::Frame`. Draws a border, calls the supplied `PreviewHighlighter` for
  syntax spans, and paints the match row highlight.
- `PreviewTheme` — pre-computed `ratatui::Style` values (border, gutter,
  non-text glyphs, cursor-line background) so callers retain full style control.

## Usage

```toml
hjkl-picker-tui = "0.1"
```

```rust,no_run
use hjkl_picker_tui::{handle_key, preview_pane, PreviewTheme};
use hjkl_picker::Picker;
use ratatui::style::Style;

let theme = PreviewTheme {
    border: Style::default(),
    gutter: Style::default(),
    non_text: Style::default(),
    cursor_line: Style::default(),
};

// In your event loop:
let event = handle_key(&mut picker, key_event);

// In your render function:
preview_pane(&mut frame, &picker, &highlighter, &theme, area);
```

## Documentation

[docs.rs/hjkl-picker-tui](https://docs.rs/hjkl-picker-tui)

## Contributing

See the
[monorepo CONTRIBUTING guide](https://github.com/kryptic-sh/hjkl/blob/main/CONTRIBUTING.md).

## License

MIT — see [LICENSE](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE).
