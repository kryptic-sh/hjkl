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

`0.12.2` — full LSP client (diagnostics, goto, hover, completion, code actions,
rename, format), window splits, tabs, tmux-navigator handoff, mouse scroll, line
numbers, and a consumer-agnostic picker `PreviewHighlighter` trait. See
[CHANGELOG.md](CHANGELOG.md) for the full release arc and
[docs.rs/hjkl-engine](https://docs.rs/hjkl-engine) for the trait reference.

## Crates

| Crate            | Role                                                                                 |
| ---------------- | ------------------------------------------------------------------------------------ |
| `hjkl-engine`    | Vim FSM + grammar, traits, no I/O deps.                                              |
| `hjkl-buffer`    | Rope-backed text buffer with cursor + edits + folds + search.                        |
| `hjkl-editor`    | Front-door facade: re-exports engine + buffer + spec types.                          |
| `hjkl-ratatui`   | Ratatui `Style` adapters and `crossterm::KeyEvent` bridge.                           |
| `hjkl-clipboard` | In-house clipboard for the ecosystem (sync + async, OSC 52 SSH).                     |
| `hjkl-form`      | Vim-modal forms with full vim grammar inside every text field.                       |
| `hjkl-bonsai`    | Tree-sitter syntax highlighting; runtime `.so` grammars, Neovim-flavoured themes.    |
| `hjkl-picker`    | Fuzzy picker subsystem: file walk, grep, custom sources, `PreviewHighlighter` trait. |
| `hjkl-config`    | Shared TOML config loader: XDG paths, span errors, layered merge.                    |
| `hjkl-splash`    | Startup splash screen widget (ratatui feature).                                      |
| `hjkl-lsp`       | LSP client: per-language server lifecycle, full text-sync, diagnostics.              |

Published on crates.io. Add to `Cargo.toml`:

```toml
hjkl-editor = "0.4"
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

## Development

```bash
git clone git@github.com:kryptic-sh/hjkl.git
cd hjkl
rustup toolchain install stable    # rust-toolchain.toml pins this for you
cargo test --workspace
```

Each `hjkl-*` crate lives in its own submodule and ships independently to
crates.io. `#![deny(missing_docs)]` is enforced on `hjkl-engine` — new public
API needs rustdoc.

Performance budgets are documented in [`MIGRATION.md`](MIGRATION.md). CI fails
if a criterion bench regresses past budget.

### Fuzzing

The only `cargo fuzz` target today is `hjkl-engine/fuzz :: handle_key` — feeds
an arbitrary keystroke stream into a fresh `Editor` and asserts no panics. Local
reproduction:

```bash
cd crates/hjkl-engine/fuzz
cargo +nightly fuzz run handle_key
```

## Contributing

See the org-wide
[CONTRIBUTING guide](https://github.com/kryptic-sh/.github/blob/main/.github/CONTRIBUTING.md)
for PR conventions, BCTP release flow, snapshot test workflow, and supported
language toolchains. Project-specific dev setup lives above in **Development**.

For security issues, see the org-wide
[SECURITY policy](https://github.com/kryptic-sh/.github/blob/main/.github/SECURITY.md)
— do not file public issues.

## License

MIT. See [LICENSE](LICENSE).
