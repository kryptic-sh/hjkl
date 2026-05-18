# hjkl-editor-tui

Ratatui TUI editor subsystem for the hjkl stack: Style conversions, KeyEvent
bridging, form and prompt renderers.

[![CI](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hjkl-editor-tui.svg)](https://crates.io/crates/hjkl-editor-tui)
[![docs.rs](https://img.shields.io/docsrs/hjkl-editor-tui)](https://docs.rs/hjkl-editor-tui)
[![MSRV](https://img.shields.io/badge/MSRV-1.95-blue.svg)](Cargo.toml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE)

Part of the [hjkl monorepo](https://github.com/kryptic-sh/hjkl) — a vim-modal
editor in Rust.

Adapters between `hjkl_engine`'s SPEC types and the `ratatui` / `crossterm`
ecosystems. Successor to `hjkl-ratatui` (see deprecation notice there).

## Features

- Style, color, and attribute conversions between `hjkl_engine` and `ratatui`
- `crossterm` `KeyEvent` → `hjkl_engine::PlannedInput` bridge (feature-gated)
- `form` module: ratatui renderer for `hjkl_form::Form`
- `prompt` module: single-line vim-grammar prompt renderer
- `spinner` module: animated braille spinner at ~8 Hz

## Usage

```toml
[dependencies]
hjkl-editor-tui = "0.1"
```

With crossterm support (default):

```toml
hjkl-editor-tui = { version = "0.1", features = ["crossterm"] }
```

Without crossterm (ratatui + style bridge only):

```toml
hjkl-editor-tui = { version = "0.1", default-features = false }
```

## Migration from hjkl-ratatui

Replace `hjkl-ratatui` with `hjkl-editor-tui` and update `use hjkl_ratatui` to
`use hjkl_editor_tui` in your source. The `hjkl-ratatui` 0.7.x shim re-exports
everything from this crate for zero-change backwards compatibility.

## Documentation

[docs.rs/hjkl-editor-tui](https://docs.rs/hjkl-editor-tui)

## Contributing

See the
[monorepo CONTRIBUTING guide](https://github.com/kryptic-sh/hjkl/blob/main/CONTRIBUTING.md).

## License

MIT — see [LICENSE](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE).
