# hjkl

[![CI](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hjkl-engine.svg)](https://crates.io/crates/hjkl-engine)
[![docs.rs](https://img.shields.io/docsrs/hjkl-engine)](https://docs.rs/hjkl-engine)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Website](https://img.shields.io/badge/website-hjkl.kryptic.sh-7ee787)](https://hjkl.kryptic.sh)

Vim engine, rope buffer, and modal editor primitives extracted from
[sqeel](https://github.com/kryptic-sh/sqeel) for reuse across sqeel,
[buffr](https://github.com/kryptic-sh/buffr), and a future standalone hjkl
binary.

Website: <https://hjkl.kryptic.sh>.

## Status

0.1.0 — first stability-locked release. SPEC frozen, Buffer trait sealed (14
methods across Cursor/Query/BufferEdit/Search), `Editor<B, H>` generic over
backend + host. See [CHANGELOG.md](CHANGELOG.md) for the trait-extraction arc
and [crates/hjkl-engine/SPEC.md](crates/hjkl-engine/SPEC.md) for the frozen
contract.

## Crates

| Crate          | Role                                                          |
| -------------- | ------------------------------------------------------------- |
| `hjkl-engine`  | Vim FSM + grammar, traits, no I/O deps.                       |
| `hjkl-buffer`  | Rope-backed text buffer with cursor + edits + folds + search. |
| `hjkl-editor`  | Front-door facade: re-exports engine + buffer + spec types.   |
| `hjkl-ratatui` | Ratatui `Style` adapters and `crossterm::KeyEvent` bridge.    |

Published on crates.io. Add to `Cargo.toml`:

```toml
hjkl-editor = "0.1"
```

## License

MIT. See [LICENSE](LICENSE).
