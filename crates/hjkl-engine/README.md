# hjkl-engine

[![CI](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/hjkl-engine.svg)](https://crates.io/crates/hjkl-engine)
[![docs.rs](https://img.shields.io/docsrs/hjkl-engine)](https://docs.rs/hjkl-engine)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE)
[![Website](https://img.shields.io/badge/website-hjkl.kryptic.sh-7ee787)](https://hjkl.kryptic.sh)

Vim FSM, motion grammar, ex commands, and editor glue. Imported from sqeel-vim
with full git history; the engine/editor sub-split per the [migration
plan][plan] happens in-place during phase 5.

Website: <https://hjkl.kryptic.sh>. Source:
<https://github.com/kryptic-sh/hjkl>.

## Status

**Pre-1.0 churn.** API may change in patch bumps until 0.1.0. See
[SPEC.md](SPEC.md) for the planned 0.0.1 trait surface and stability contract.

## Features

| Feature | Default | Notes                                      |
| ------- | ------- | ------------------------------------------ |
| `serde` | yes     | Serde derives for `Editor` snapshot types. |

`ratatui` and `crossterm` are unconditional deps until phase 5 lands the
engine-native `Style` type and the `Buffer`/`Host` trait extraction. After that
they move behind feature flags so wasm/no_std consumers can opt out.

## License

MIT

[plan]: https://github.com/kryptic-sh/hjkl/blob/main/MIGRATION.md
