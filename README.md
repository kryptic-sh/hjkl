# hjkl-keymap

Backend-agnostic modal keymap: chord parsing, trie dispatch, leader/chord
resolution for the hjkl editor stack.

Parses vim-style notation (`<leader>gs`, `<C-x>`, `<S-Tab>`, `<C-S-Tab>`,
named specials, `<lt>`, bare characters), stores bindings per-mode in separate
tries, and resolves key events to `Pending` / `Match` / `Ambiguous` / `Unbound`
outcomes via a stateful `feed` API.

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Website](https://img.shields.io/badge/website-hjkl.kryptic.sh-7ee787)](https://hjkl.kryptic.sh)

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

## License

MIT — see [LICENSE](LICENSE).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). For security issues, see
[SECURITY.md](SECURITY.md).
