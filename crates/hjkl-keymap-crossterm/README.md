# hjkl-keymap-crossterm

Crossterm KeyEvent ↔ hjkl-keymap KeyEvent adapter. Wraps the crossterm input
boundary in a renderer-adapter crate per the hjkl naming convention.

[![CI](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hjkl-keymap-crossterm.svg)](https://crates.io/crates/hjkl-keymap-crossterm)
[![docs.rs](https://img.shields.io/docsrs/hjkl-keymap-crossterm)](https://docs.rs/hjkl-keymap-crossterm)
[![MSRV](https://img.shields.io/badge/MSRV-1.95-blue.svg)](Cargo.toml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE)

Part of the [hjkl monorepo](https://github.com/kryptic-sh/hjkl) — a vim-modal
editor in Rust.

- [`from_crossterm`] — translate a `crossterm::event::KeyEvent` to a
  `hjkl_keymap::KeyEvent`. Returns `None` for unsupported codes (media keys,
  modifier-only, null) and for non-press event kinds (release, repeat).
- [`to_crossterm`] — round-trip back to a `crossterm::event::KeyEvent` for
  replaying unbound sequences or user maps.

Companion adapters (future):

- `hjkl-keymap-floem` — floem / winit input layer (for `apps/hjkl-gui`).

The naming follows the hjkl renderer-adapter convention (`-tui` / `-gui` suffix
rule from issue #100).

## Documentation

[docs.rs/hjkl-keymap-crossterm](https://docs.rs/hjkl-keymap-crossterm)

## Contributing

See the
[monorepo CONTRIBUTING guide](https://github.com/kryptic-sh/hjkl/blob/main/CONTRIBUTING.md).

## License

MIT — see [LICENSE](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE).
