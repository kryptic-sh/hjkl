# hjkl-keymap-gui

Floem `KeyEvent` → `hjkl-keymap` `KeyEvent` adapter. The floem GUI-side input
boundary for hjkl, mirroring the `hjkl-keymap-tui` crossterm adapter so
renderer code is the only place that imports `floem::keyboard` directly.

Part of the [hjkl monorepo](https://github.com/kryptic-sh/hjkl) — a vim-modal
editor in Rust.

- [`from_floem`] — translate a `floem::keyboard::KeyEvent` to a
  `hjkl_keymap::KeyEvent`. Returns `None` for key-release events and for
  keys with no meaningful representation in the keymap (dead keys,
  unidentified keys, modifier-only presses).

Unlike `hjkl-keymap-tui`, there is no `to_floem` round-trip: floem's
underlying `winit::event::KeyEvent` has no public constructor (it carries a
private, platform-specific field), so a `hjkl_keymap::KeyEvent` cannot be
synthesized back into a real floem key event outside of `winit` itself.

## License

MIT
