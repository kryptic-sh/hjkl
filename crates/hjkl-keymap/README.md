# hjkl-keymap

Backend-agnostic modal keymap: chord parsing, trie dispatch, leader/chord
resolution for the hjkl editor stack

[![CI](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hjkl-keymap.svg)](https://crates.io/crates/hjkl-keymap)
[![docs.rs](https://img.shields.io/docsrs/hjkl-keymap)](https://docs.rs/hjkl-keymap)
[![MSRV](https://img.shields.io/badge/MSRV-1.95-blue.svg)](Cargo.toml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE)

Part of the [hjkl monorepo](https://github.com/kryptic-sh/hjkl) — a vim-modal
editor in Rust.

Parses vim-style notation (`<leader>gs`, `<C-x>`, `<S-Tab>`, `<C-S-Tab>`, named
specials, `<lt>`, bare characters), stores bindings per-mode in separate tries,
and resolves key events to `Pending` / `Match` / `Ambiguous` / `Unbound`
outcomes via a stateful `feed` API.

## Usage

```toml
[dependencies]
hjkl-keymap = "0.1"
```

```rust
use hjkl_keymap::{Keymap, Mode, KeyResolve, KeyCode, KeyEvent, KeyModifiers};
use std::time::Instant;

let mut km: Keymap<&str> = Keymap::new(' '); // space as leader
km.add(Mode::Normal, "<leader>gs", "git_status", "Git status").unwrap();
km.add(Mode::Normal, "<leader>gd", "git_diff", "Git diff").unwrap();

let space = KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE);
let g = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE);
let s = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE);

let now = Instant::now();
assert!(matches!(km.feed(Mode::Normal, space, now), KeyResolve::Pending));
assert!(matches!(km.feed(Mode::Normal, g, now), KeyResolve::Pending));
assert!(matches!(km.feed(Mode::Normal, s, now), KeyResolve::Match(_)));
```

## Documentation

[docs.rs/hjkl-keymap](https://docs.rs/hjkl-keymap)

## Contributing

See the
[monorepo CONTRIBUTING guide](https://github.com/kryptic-sh/hjkl/blob/main/CONTRIBUTING.md).

## License

MIT — see [LICENSE](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE).
