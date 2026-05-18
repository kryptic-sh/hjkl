# hjkl-keymap-tui

Ratatui/crossterm KeyEvent ↔ `hjkl-keymap` KeyEvent adapter. The TUI-side
input boundary for hjkl. Renamed from `hjkl-keymap-crossterm` 2026-05-18 to
match the `-tui` adapter convention used across the workspace.

[![CI](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hjkl-keymap-tui.svg)](https://crates.io/crates/hjkl-keymap-tui)
[![docs.rs](https://img.shields.io/docsrs/hjkl-keymap-tui)](https://docs.rs/hjkl-keymap-tui)
[![MSRV](https://img.shields.io/badge/MSRV-1.95-blue.svg)](Cargo.toml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE)

Part of the [hjkl monorepo](https://github.com/kryptic-sh/hjkl) — a vim-modal
editor in Rust.

- [`from_crossterm`] — translate a `crossterm::event::KeyEvent` to a
  `hjkl_keymap::KeyEvent`. Returns `None` for unsupported codes (media keys,
  modifier-only, null) and for non-press event kinds (release, repeat).
- [`to_crossterm`] — round-trip back to a `crossterm::event::KeyEvent` for
  replaying unbound sequences or user maps.

Companion adapter (future):

- `hjkl-keymap-gui` — floem / winit input layer (for `apps/hjkl-gui`).

## Documentation

[docs.rs/hjkl-keymap-tui](https://docs.rs/hjkl-keymap-tui)

## License

MIT — see [LICENSE](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE).
