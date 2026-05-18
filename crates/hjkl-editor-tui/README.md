# hjkl-editor-tui

Ratatui TUI editor subsystem for the [hjkl](https://hjkl.kryptic.sh) stack.

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

Replace `hjkl-ratatui` with `hjkl-editor-tui` and update `use hjkl_ratatui`
to `use hjkl_editor_tui` in your source. The `hjkl-ratatui` 0.7.x shim
re-exports everything from this crate for zero-change backwards compatibility.

## License

MIT
