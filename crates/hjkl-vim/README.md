# hjkl-vim

Vim modal state types and grammar primitives for the hjkl editor stack. Pre-1.0
churn.

[![CI](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hjkl-vim.svg)](https://crates.io/crates/hjkl-vim)
[![docs.rs](https://img.shields.io/docsrs/hjkl-vim)](https://docs.rs/hjkl-vim)
[![MSRV](https://img.shields.io/badge/MSRV-1.95-blue.svg)](Cargo.toml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE)

Part of the [hjkl monorepo](https://github.com/kryptic-sh/hjkl) — a vim-modal
editor in Rust.

Provides the `Mode` enum used as the mode discriminator in `hjkl-keymap`'s
generic `Keymap<A, M>`, plus the vim input FSM: `feed_input` / `dispatch_input`
drive normal/operator-pending/insert handling, with count accumulation
(`CountAccumulator`), operator resolution (`OperatorKind`), the pending-key
state machine (`PendingState`, `step`), and engine commands (`EngineCmd`).

## Usage

```toml
[dependencies]
hjkl-vim = "0.33"
```

```rust
use hjkl_vim::Mode;

// Mode satisfies hjkl_keymap::Mode via the blanket impl for
// Copy + Eq + Hash + Debug types.
let mode = Mode::Normal;
```

## Documentation

[docs.rs/hjkl-vim](https://docs.rs/hjkl-vim)

## Contributing

See the
[monorepo CONTRIBUTING guide](https://github.com/kryptic-sh/hjkl/blob/main/CONTRIBUTING.md).

## License

MIT — see [LICENSE](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE).
