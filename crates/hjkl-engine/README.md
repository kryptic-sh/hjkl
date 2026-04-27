# hjkl-engine

Vim FSM, motion grammar, and editor traits — the no-I/O core of the hjkl stack.

[![CI](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hjkl-engine.svg)](https://crates.io/crates/hjkl-engine)
[![docs.rs](https://img.shields.io/docsrs/hjkl-engine)](https://docs.rs/hjkl-engine)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](../../LICENSE)
[![Website](https://img.shields.io/badge/website-hjkl.kryptic.sh-7ee787)](https://hjkl.kryptic.sh)

Vim-mode editor engine built on top of `hjkl-buffer`. Exposes an `Editor` you
can drop into a ratatui layout — covers the bulk of vim's normal / insert /
visual / visual-line / visual-block modes, text-object operators, dot-repeat,
and ex-command handling (`:s/foo/bar/g`, `:w`, `:q`, `:noh`, ...). Imported from
sqeel-vim with full git history.

## Status

`0.2.0` — frozen public API; SPEC frozen per [SPEC.md](SPEC.md). `Buffer` trait
sealed (14 methods across Cursor/Query/BufferEdit/Search). `Editor<B, H>`
generic over buffer backend + host.

## Features

| Feature | Default | Notes                                      |
| ------- | ------- | ------------------------------------------ |
| `serde` | yes     | Serde derives for `Editor` snapshot types. |

`ratatui` and `crossterm` are unconditional deps until the engine-native `Style`
type and the `Buffer`/`Host` trait extraction land. After that they move behind
feature flags so wasm/no_std consumers can opt out.

## Usage

```toml
hjkl-engine = "0.2"
```

```rust,no_run
use hjkl_engine::{Editor, Input, Key};
use hjkl_engine::types::{DefaultHost, Options};
use hjkl_buffer::Buffer;

let mut editor = Editor::new(
    Buffer::new(),
    DefaultHost::new(),
    Options::default(),
);
editor.set_content("hello world");

// Drive the FSM with a keystroke
let input = Input { key: Key::Char('j'), ..Default::default() };
hjkl_engine::step(&mut editor, input);
```

## License

MIT. See [LICENSE](../../LICENSE).

[plan]: https://github.com/kryptic-sh/hjkl/blob/main/MIGRATION.md
