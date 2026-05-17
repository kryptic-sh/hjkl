# hjkl-app

Host-agnostic application layer for the hjkl editor.

[![crates.io](https://img.shields.io/crates/v/hjkl-app.svg)](https://crates.io/crates/hjkl-app)
[![docs.rs](https://img.shields.io/docsrs/hjkl-app)](https://docs.rs/hjkl-app)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Website](https://img.shields.io/badge/website-hjkl.kryptic.sh-7ee787)](https://hjkl.kryptic.sh)

Shared application primitives consumed by `apps/hjkl` (TUI) and
`apps/hjkl-gui` (GUI). Modules: `config`, `editorconfig`, `lang` (tree-sitter
grammar directory), `git` + `git_worker` (background diff signs), `completion`,
`keymap_actions` (chord-bindable `AppAction` enum), `picker_action`,
`picker_sources`, `picker_git`.

## Status

Pre-1.0 — extracted in stages from `apps/hjkl` (see kryptic-sh/hjkl#125). API
shape is still settling as more host-agnostic surface migrates here from the
TUI binary. Not recommended for external consumers yet; bumps will land on
patch versions while the surface stabilises.

## License

MIT. See [LICENSE](../../LICENSE).
