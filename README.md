# hjkl-clipboard

Unified clipboard sink for the hjkl editor stack (arboard + OSC 52 fallback).

[![CI](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hjkl-clipboard.svg)](https://crates.io/crates/hjkl-clipboard)
[![docs.rs](https://img.shields.io/docsrs/hjkl-clipboard)](https://docs.rs/hjkl-clipboard)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](../../LICENSE)
[![Website](https://img.shields.io/badge/website-hjkl.kryptic.sh-7ee787)](https://hjkl.kryptic.sh)

Tries `arboard` first (native X11/Wayland/macOS/Windows). Falls back to OSC 52
when over SSH or when arboard is unavailable. Wraps OSC 52 in a tmux DCS
passthrough when running inside tmux.

Over SSH, arboard writes to the remote host's clipboard and silently drops the
payload — so SSH connections skip arboard entirely and use OSC 52 to reach the
user's local terminal emulator (works in iTerm2, WezTerm, Alacritty, kitty, tmux
3.3+, and recent xterm).

## Status

`0.2.0` — production-ready clipboard abstraction used by the `hjkl` binary.

## Usage

```toml
hjkl-clipboard = "0.2"
```

```rust
use hjkl_clipboard::Clipboard;

let mut cb = Clipboard::new();

// Write to clipboard (arboard or OSC 52 fallback)
cb.set_text("hello from hjkl");

// Read from clipboard (returns None over SSH — no way to pull
// the user's local clipboard back over the wire)
if let Some(text) = cb.get_text() {
    println!("clipboard: {text}");
}
```

## License

MIT. See [LICENSE](../../LICENSE).
