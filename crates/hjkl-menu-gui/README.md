# hjkl-menu-gui

Floem adapter for hjkl-menu: renders the renderer-agnostic context menu model
as a floem view with click-to-select and click-outside dismissal, mirroring
the `hjkl-menu-tui` ratatui adapter so renderer code is the only place that
imports `floem` directly.

Part of the [hjkl monorepo](https://github.com/kryptic-sh/hjkl) — a vim-modal
editor in Rust.

## License

MIT
