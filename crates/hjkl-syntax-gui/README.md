# hjkl-syntax-gui

Floem/cosmic-text adapter for hjkl-syntax: converts renderer-agnostic
`RenderOutput` spans into `cosmic-text` styling attributes and routes
`DiagSign`s into owned gutter marks, mirroring the `hjkl-syntax-tui` ratatui
adapter so renderer code is the only place that imports `cosmic-text`
directly.

Part of the [hjkl monorepo](https://github.com/kryptic-sh/hjkl) — a vim-modal
editor in Rust.

## License

MIT
