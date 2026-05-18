# hjkl-engine

Vim FSM, motion grammar, and ex commands. Pre-1.0 churn.

[![CI](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hjkl-engine.svg)](https://crates.io/crates/hjkl-engine)
[![docs.rs](https://img.shields.io/docsrs/hjkl-engine)](https://docs.rs/hjkl-engine)
[![MSRV](https://img.shields.io/badge/MSRV-1.95-blue.svg)](Cargo.toml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE)

Part of the [hjkl monorepo](https://github.com/kryptic-sh/hjkl) — a vim-modal
editor in Rust.

Vim-mode editor engine built on top of `hjkl-buffer`. Exposes an `Editor` you
can drop into a ratatui layout — covers the bulk of vim's normal / insert /
visual / visual-line / visual-block modes, text-object operators, dot-repeat,
and ex-command handling (`:s/foo/bar/g`, `:w`, `:q`, `:noh`, ...). Imported from
sqeel-vim with full git history.

## Status

`Editor<B, H>` is generic over buffer backend + host; the `Buffer` trait splits
into Cursor / Query / BufferEdit / Search subtraits. See
[docs.rs](https://docs.rs/hjkl-engine) for the canonical API reference.

## Features

| Feature | Default | Notes                                      |
| ------- | ------- | ------------------------------------------ |
| `serde` | yes     | Serde derives for `Editor` snapshot types. |

`ratatui` and `crossterm` are unconditional deps until the engine-native `Style`
type and the `Buffer`/`Host` trait extraction land. After that they move behind
feature flags so wasm/no_std consumers can opt out.

## Usage

```toml
hjkl-engine = "0.3"
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

// Drive the FSM with a keystroke (via hjkl-vim — the FSM lives there now)
let input = Input { key: Key::Char('j'), ..Default::default() };
hjkl_vim::dispatch_input(&mut editor, input);
```

## Documentation

[docs.rs/hjkl-engine](https://docs.rs/hjkl-engine)

## Contributing

See the
[monorepo CONTRIBUTING guide](https://github.com/kryptic-sh/hjkl/blob/main/CONTRIBUTING.md).

## License

MIT — see [LICENSE](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE).
