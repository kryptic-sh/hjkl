# hjkl-statusline

Renderer-agnostic statusline data model for hjkl editors. Segments + mode +
filename + cursor; ratatui/floem adapters live in hjkl-statusline-tui /
hjkl-statusline-gui.

[![CI](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hjkl-statusline.svg)](https://crates.io/crates/hjkl-statusline)
[![docs.rs](https://img.shields.io/docsrs/hjkl-statusline)](https://docs.rs/hjkl-statusline)
[![MSRV](https://img.shields.io/badge/MSRV-1.95-blue.svg)](Cargo.toml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE)

Part of the [hjkl monorepo](https://github.com/kryptic-sh/hjkl) — a vim-modal
editor in Rust.

Pure data — no ratatui, no floem. Companion crates wire it to a backend:

- [`hjkl-statusline-tui`](https://crates.io/crates/hjkl-statusline-tui) —
  ratatui
- `hjkl-statusline-gui` (future) — floem

The naming follows the vim convention (`:help statusline`, lualine, vim-airline,
lightline).

Scaffolding crate — model lands in a follow-up patch.

## Documentation

[docs.rs/hjkl-statusline](https://docs.rs/hjkl-statusline)

## Contributing

See the
[monorepo CONTRIBUTING guide](https://github.com/kryptic-sh/hjkl/blob/main/CONTRIBUTING.md).

## License

MIT — see [LICENSE](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE).
