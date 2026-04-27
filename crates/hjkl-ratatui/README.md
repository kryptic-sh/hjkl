# hjkl-ratatui

Adapters between `hjkl-engine` SPEC types and the ratatui / crossterm
ecosystems.

[![CI](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hjkl-ratatui.svg)](https://crates.io/crates/hjkl-ratatui)
[![docs.rs](https://img.shields.io/docsrs/hjkl-ratatui)](https://docs.rs/hjkl-ratatui)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](../../LICENSE)
[![Website](https://img.shields.io/badge/website-hjkl.kryptic.sh-7ee787)](https://hjkl.kryptic.sh)

Engine types are deliberately UI-agnostic so non-terminal hosts (buffr's
wasm-flavored renderer, future GUI shells) can consume them without dragging
ratatui in. This crate is the opt-in bridge ratatui-based hosts pull in.

Free functions, not `From`/`Into` impls — orphan rules forbid the trait route
since both sides are foreign types from this crate's view. Function syntax keeps
the bridge explicit at callsites, which is fine for low-frequency style mapping.

## Status

`0.2.0` — ratatui `Style` ↔ engine `Style` adapter; `crossterm::KeyEvent` →
engine SPEC `Input` bridge. The `spinner` module ships with `0.2.0`, providing a
shared braille loading indicator for use across TUI widgets.

## What's here

- `engine_to_ratatui_style` / `ratatui_to_engine_style`
- `engine_to_ratatui_color` / `ratatui_to_engine_color` (handles RGB,
  Indexed-256, named ANSI; flattens to RGB on the engine side)
- `engine_to_ratatui_attrs` / `ratatui_to_engine_attrs`
- `crossterm_key_event_to_input` (behind the default-on `crossterm` feature) —
  bridge `crossterm::KeyEvent` → engine SPEC `Input`.
- `spinner::frame() -> &'static str` — shared braille spinner for loading
  indicators, ~8 Hz monotonic epoch.

## Usage

```toml
hjkl-ratatui = "0.2"
```

```rust,no_run
use hjkl_engine::{Attrs, Color, Style};
use hjkl_ratatui::{engine_to_ratatui_style, ratatui_to_engine_style};

let engine_style = Style {
    fg: Some(Color(255, 165, 0)),
    bg: None,
    attrs: Attrs::BOLD,
};

// Convert to ratatui for rendering
let ratatui_style = engine_to_ratatui_style(engine_style);

// Bridge a crossterm key event to engine input
#[cfg(feature = "crossterm")]
{
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let ev = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
    let input = hjkl_ratatui::crossterm_key_event_to_input(ev);
}
```

## License

MIT. See [LICENSE](../../LICENSE).
