# hjkl-app

Host-agnostic application layer for the hjkl editor. Used by apps/hjkl (TUI) and
apps/hjkl-gui (GUI).

[![CI](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hjkl-app.svg)](https://crates.io/crates/hjkl-app)
[![docs.rs](https://img.shields.io/docsrs/hjkl-app)](https://docs.rs/hjkl-app)
[![MSRV](https://img.shields.io/badge/MSRV-1.95-blue.svg)](Cargo.toml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE)

Part of the [hjkl monorepo](https://github.com/kryptic-sh/hjkl) — a vim-modal
editor in Rust.

Shared application primitives consumed by `apps/hjkl` (TUI) and `apps/hjkl-gui`
(GUI). Modules: `config`, `editorconfig`, `lang` (tree-sitter grammar
directory), `git` + `git_worker` (background diff signs), `completion`,
`keymap_actions` (chord-bindable `AppAction` enum), `picker_action`,
`picker_sources`, `picker_git`.

## Status

Pre-1.0 — extracted in stages from `apps/hjkl` (see kryptic-sh/hjkl#125). API
shape is still settling as more host-agnostic surface migrates here from the TUI
binary. Not recommended for external consumers yet; bumps will land on patch
versions while the surface stabilises.

## Documentation

[docs.rs/hjkl-app](https://docs.rs/hjkl-app)

## Contributing

See the
[monorepo CONTRIBUTING guide](https://github.com/kryptic-sh/hjkl/blob/main/CONTRIBUTING.md).

## License

MIT — see [LICENSE](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE).
