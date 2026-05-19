# hjkl-vim-tui

Crossterm/ratatui driver for [`hjkl-vim`](https://crates.io/crates/hjkl-vim).

Provides `handle_key` — the single entry point that wires a crossterm `KeyEvent`
through the vim FSM and emits cursor-shape changes.

## Usage

```rust
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hjkl_vim_tui::handle_key;

// In your event loop:
if let crossterm::event::Event::Key(key) = crossterm::event::read()? {
    handle_key(&mut editor, key);
}
```

## Crate relationship

| Crate              | Role                          |
| ------------------ | ----------------------------- |
| `hjkl-engine`      | Toolkit-agnostic editor core  |
| `hjkl-vim`         | Vim FSM — no toolkit dep      |
| `hjkl-engine-tui`  | Ratatui + crossterm adapters  |
| **`hjkl-vim-tui`** | **Wires crossterm → vim FSM** |

Part of the [hjkl](https://hjkl.kryptic.sh) editor stack.
