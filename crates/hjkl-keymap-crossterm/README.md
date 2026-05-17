# hjkl-keymap-crossterm

Crossterm `KeyEvent` ↔ `hjkl-keymap` `KeyEvent` adapter. Wraps the crossterm
input boundary in a renderer-adapter crate per the hjkl naming convention.

- [`from_crossterm`] — translate a `crossterm::event::KeyEvent` to a
  `hjkl_keymap::KeyEvent`. Returns `None` for unsupported codes (media keys,
  modifier-only, null) and for non-press event kinds (release, repeat).
- [`to_crossterm`] — round-trip back to a `crossterm::event::KeyEvent` for
  replaying unbound sequences or user maps.

Companion adapters (future):

- `hjkl-keymap-floem` — floem / winit input layer (for `apps/hjkl-gui`).

The naming follows the hjkl renderer-adapter convention (`-tui` / `-gui` suffix
rule from issue #100).

## License

MIT
