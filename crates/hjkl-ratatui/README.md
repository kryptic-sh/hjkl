# hjkl-ratatui

[![CI](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/hjkl-ratatui.svg)](https://crates.io/crates/hjkl-ratatui)
[![docs.rs](https://img.shields.io/docsrs/hjkl-ratatui)](https://docs.rs/hjkl-ratatui)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE)
[![Website](https://img.shields.io/badge/website-hjkl.kryptic.sh-7ee787)](https://hjkl.kryptic.sh)

Adapters between [`hjkl-engine`](../hjkl-engine) SPEC types and the
[ratatui](https://crates.io/crates/ratatui) /
[crossterm](https://crates.io/crates/crossterm) ecosystems.

Website: <https://hjkl.kryptic.sh>. Source:
<https://github.com/kryptic-sh/hjkl>.

## What's here

- `engine_to_ratatui_style` / `ratatui_to_engine_style`
- `engine_to_ratatui_color` / `ratatui_to_engine_color` (handles RGB,
  Indexed-256, named ANSI; flattens to RGB on the engine side)
- `engine_to_ratatui_attrs` / `ratatui_to_engine_attrs`
- `crossterm_key_event_to_input` (behind the default-on `crossterm` feature) —
  bridge `crossterm::KeyEvent` → engine SPEC `Input`.

Free functions, not `From`/`Into` impls — orphan rules forbid the trait route
since both sides are foreign types from this crate's view.

## Status

Pre-1.0 churn. API may change in patch bumps until 0.1.0.

## License

MIT
