# hjkl

Vim engine, rope buffer, and modal editor primitives for building vim-modal
terminal apps in Rust.

[![CI](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hjkl-engine.svg)](https://crates.io/crates/hjkl-engine)
[![docs.rs](https://img.shields.io/docsrs/hjkl-engine)](https://docs.rs/hjkl-engine)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Website](https://img.shields.io/badge/website-hjkl.kryptic.sh-7ee787)](https://hjkl.kryptic.sh)

Extracted from [sqeel](https://github.com/kryptic-sh/sqeel) for reuse across
sqeel, [buffr](https://github.com/kryptic-sh/buffr), and the standalone
[`hjkl`](apps/hjkl) binary.

## Status

`0.4.0` — Buffer trait split across Cursor/Query/BufferEdit/Search,
`Editor<B, H>` generic over backend + host, clipboard via our in-house
[`hjkl-clipboard`](https://crates.io/crates/hjkl-clipboard). See
[CHANGELOG.md](CHANGELOG.md) for the release arc and
[docs.rs/hjkl-engine](https://docs.rs/hjkl-engine) for the trait reference.

## Crates

| Crate              | Role                                                               |
| ------------------ | ------------------------------------------------------------------ |
| `hjkl-engine`      | Vim FSM + grammar, traits, no I/O deps.                            |
| `hjkl-buffer`      | Rope-backed text buffer with cursor + edits + folds + search.      |
| `hjkl-editor`      | Front-door facade: re-exports engine + buffer + spec types.        |
| `hjkl-ratatui`     | Ratatui `Style` adapters and `crossterm::KeyEvent` bridge.         |
| `hjkl-clipboard`   | In-house clipboard for the ecosystem (sync + async, OSC 52 SSH).   |
| `hjkl-form`        | Vim-modal forms with full vim grammar inside every text field.     |
| `hjkl-tree-sitter` | Tree-sitter syntax highlighting (Rust, Markdown, JSON, TOML, SQL). |
| `hjkl-picker`      | Fuzzy picker subsystem: file walk, grep search, custom sources.    |
| `hjkl-config`      | Shared TOML config loader: XDG paths, span errors, layered merge.  |

Published on crates.io. Add to `Cargo.toml`:

```toml
hjkl-editor = "0.3"
```

## Configuring `hjkl`

The standalone editor reads `$XDG_CONFIG_HOME/hjkl/config.toml` (Linux/macOS) or
`%APPDATA%\kryptic\hjkl\config\config.toml` (Windows). Defaults are bundled into
the binary from [`apps/hjkl/src/config.toml`](apps/hjkl/src/config.toml) — that
file is the single source of truth for default values. The user file is
**deep-merged** on top: only the fields you want to override need to appear
there. Unknown keys are an error.

A custom path can be passed with `--config <PATH>`.

```toml
# ~/.config/hjkl/config.toml — minimal override example
[editor]
leader = "\\"
tab_width = 2
```

See [`apps/hjkl/src/config.toml`](apps/hjkl/src/config.toml) for the full schema
with comments.

## License

MIT. See [LICENSE](LICENSE).
