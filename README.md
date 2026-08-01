# hjkl

Vim engine, rope buffer, and modal editor primitives for building vim-modal
terminal apps in Rust.

[![CI](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hjkl-engine.svg)](https://crates.io/crates/hjkl-engine)
[![docs.rs](https://img.shields.io/docsrs/hjkl-engine)](https://docs.rs/hjkl-engine)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Website](https://img.shields.io/badge/website-kryptic.sh%2Fhjkl-7ee787)](https://www.kryptic.sh/hjkl/)

Extracted from [sqeel](https://github.com/kryptic-sh/sqeel) for reuse across the
kryptic-sh stack and the standalone [`hjkl`](apps/hjkl) binary. Crates published
from this workspace are consumed by
[sqeel](https://github.com/kryptic-sh/sqeel),
[buffr](https://github.com/kryptic-sh/buffr),
[inbx](https://github.com/kryptic-sh/inbx),
[hodl](https://github.com/kryptic-sh/hodl),
[hrdr](https://github.com/kryptic-sh/hrdr),
[tikr](https://github.com/kryptic-sh/tikr),
[gpur](https://github.com/kryptic-sh/gpur), and
[pikr](https://github.com/kryptic-sh/pikr).

## Status

Pre-1.0 and moving. Working today: full LSP client (diagnostics, goto, hover,
completion, code actions, rename, format), window splits, tabs, multi-buffer
editing, fuzzy file/buffer/grep pickers, tree-sitter highlighting, folds,
tmux-navigator handoff, mouse scroll, line numbers, and a consumer-agnostic
picker `PreviewHighlighter` trait.

See [CHANGELOG.md](./CHANGELOG.md) for the release arc, the
[releases page](https://github.com/kryptic-sh/hjkl/releases) for the current
version, and [docs.rs/hjkl-engine](https://docs.rs/hjkl-engine) for the trait
reference.

## Crates

`crates/` holds the full set of `hjkl-*` crates; the ones most consumers reach
for are:

| Crate             | Role                                                                                 |
| ----------------- | ------------------------------------------------------------------------------------ |
| `hjkl-engine`     | Vim FSM + grammar, traits, no I/O deps.                                              |
| `hjkl-buffer`     | Rope-backed text buffer with cursor + edits + folds + search.                        |
| `hjkl-editor`     | Front-door facade: re-exports engine + buffer + spec types.                          |
| `hjkl-editor-tui` | Ratatui adapter: editor draw, form rendering, spinner widget.                        |
| `hjkl-clipboard`  | In-house clipboard for the ecosystem (sync + async, OSC 52 SSH).                     |
| `hjkl-form`       | Vim-modal forms with full vim grammar inside every text field.                       |
| `hjkl-bonsai`     | Tree-sitter syntax highlighting; runtime `.so` grammars, Neovim-flavoured themes.    |
| `hjkl-picker`     | Fuzzy picker subsystem: file walk, grep, custom sources, `PreviewHighlighter` trait. |
| `hjkl-config`     | Shared TOML config loader: XDG paths, span errors, layered merge.                    |
| `hjkl-splash`     | Rendering-agnostic startup splash animation (`hjkl-splash-tui` draws it).            |
| `hjkl-lsp`        | LSP client: per-language server lifecycle, full text-sync, diagnostics.              |

Each crate publishes independently to crates.io:

```bash
cargo add hjkl-editor
```

The standalone editor installs the same way:

```bash
cargo install hjkl
```

## Configuring `hjkl`

The standalone editor reads `$XDG_CONFIG_HOME/hjkl/config.toml`, falling back to
`~/.config/hjkl/config.toml`. That layout is used on **every** platform —
including macOS and Windows, which deliberately do not get
`~/Library/Application Support` or `%APPDATA%`, so a config tree is identical
everywhere. Defaults are bundled into the binary from
[`crates/hjkl-app/src/config.toml`](crates/hjkl-app/src/config.toml) — that file
is the single source of truth for default values. The user file is
**deep-merged** on top: only the fields you want to override need to appear
there. Unknown keys are an error.

A custom path can be passed with `--config <PATH>`.

```toml
# ~/.config/hjkl/config.toml — minimal override example
[editor]
leader = "\\"

[options]
shiftwidth = 2
tabstop = 2
```

The `[options]` table carries every global `:set` option, keyed by its exact
`:set` name — `options.scrolloff = 8` and `:set scrolloff=8` are the same knob.
Running `:set` in the editor writes the resulting value back to the config file
in place: comments, key order, and quoting all survive. A `:set` the editor
rejected writes nothing, and a query (`:set nu?`) writes nothing. Buffer-local
options (`filetype`, `commentstring`, `readonly`, `modifiable`, `endofline`) are
session-scoped and never persisted. `.editorconfig` and a file's own modeline
layer on top of `[options]`, in that order.

See [`crates/hjkl-app/src/config.toml`](crates/hjkl-app/src/config.toml) for the
full schema with comments.

### Colorschemes

`:colorscheme <name>` and `theme.name` accept the bundled schemes `dark`,
`light`, `tokyonight`, `catppuccin`, `gruvbox`, `nord`, `dracula`, and
`onedark`. An unrecognised `theme.name` warns on startup and falls back to
`dark`.

## Development

```bash
git clone git@github.com:kryptic-sh/hjkl.git
cd hjkl
rustup toolchain install stable    # rust-toolchain.toml pins this for you
cargo test --workspace
```

Each `hjkl-*` crate lives under `crates/<name>/` in this monorepo and ships
independently to crates.io. `#![deny(missing_docs)]` is enforced on
`hjkl-engine` — new public API needs rustdoc.

Performance budgets are defined in
[`crates/hjkl-buffer/benches/budgets.rs`](crates/hjkl-buffer/benches/budgets.rs).
CI fails if a criterion bench regresses past budget.

### Fuzzing

The only `cargo fuzz` target today is `hjkl-engine/fuzz :: handle_key` — feeds
an arbitrary keystroke stream into a fresh `Editor` and asserts no panics. Local
reproduction:

```bash
cd crates/hjkl-engine/fuzz
cargo +nightly fuzz run handle_key
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for dev setup, PR conventions, the BCTP
release flow, and MSRV policy. Org-wide conventions live in the
[kryptic-sh CONTRIBUTING guide](https://github.com/kryptic-sh/.github/blob/main/.github/CONTRIBUTING.md).

For security issues, see the org-wide
[SECURITY policy](https://github.com/kryptic-sh/.github/blob/main/.github/SECURITY.md)
— do not file public issues.

## License

MIT. See [LICENSE](LICENSE).
