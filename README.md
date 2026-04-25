# hjkl

[![CI](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Website](https://img.shields.io/badge/website-hjkl.kryptic.sh-7ee787)](https://hjkl.kryptic.sh)

Vim engine, rope buffer, and modal editor primitives extracted from
[sqeel](https://github.com/kryptic-sh/sqeel) for reuse across sqeel,
[buffr](https://github.com/kryptic-sh/buffr), and a future standalone hjkl
binary.

Website: <https://hjkl.kryptic.sh>.

## Status

Pre-1.0 churn — see [CHANGELOG.md](CHANGELOG.md) for the latest release.
[MIGRATION.md](MIGRATION.md) tracks the full extraction plan and design
rationale; [crates/hjkl-engine/SPEC.md](crates/hjkl-engine/SPEC.md) documents
the planned 0.1.0 trait surface.

## Crates

| Crate          | Role                                                          |
| -------------- | ------------------------------------------------------------- |
| `hjkl-engine`  | Vim FSM + grammar, traits, no I/O deps.                       |
| `hjkl-buffer`  | Rope-backed text buffer with cursor + edits + folds + search. |
| `hjkl-editor`  | Front-door facade: re-exports engine + buffer + spec types.   |
| `hjkl-ratatui` | Ratatui `Style` adapters and `crossterm::KeyEvent` bridge.    |

Published on crates.io. Add to `Cargo.toml` with an exact-version pin during the
0.0.x churn:

```toml
hjkl-editor = "=0.0.4"
```

## License

MIT. See [LICENSE](LICENSE).
