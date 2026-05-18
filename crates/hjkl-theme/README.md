# hjkl-theme

Unified theme schema: TOML parse, palette interning, capture fallback chain.

[![CI](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hjkl-theme.svg)](https://crates.io/crates/hjkl-theme)
[![docs.rs](https://img.shields.io/docsrs/hjkl-theme)](https://docs.rs/hjkl-theme)
[![MSRV](https://img.shields.io/badge/MSRV-1.95-blue.svg)](Cargo.toml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE)

Part of the [hjkl monorepo](https://github.com/kryptic-sh/hjkl) — a vim-modal
editor in Rust.

## Status

Phase 1: schema only. Parses TOML themes, interns palette refs, resolves
capture-name fallback chains. No rendering backend included yet.

See [kryptic-sh/hjkl#10](https://github.com/kryptic-sh/hjkl/issues/10) for the
full design and roadmap.

## Format

```toml
[palette]
blue = "#89b4fa"

"@function" = "$blue"
"@function.builtin" = { fg = "$blue", modifiers = ["bold"] }

[ui]
background = "#1e1e2e"
foreground = "#cdd6f4"
"statusline.inactive" = { fg = "#6c7086", bg = "#181825" }
```

## Documentation

[docs.rs/hjkl-theme](https://docs.rs/hjkl-theme)

## Contributing

See the
[monorepo CONTRIBUTING guide](https://github.com/kryptic-sh/hjkl/blob/main/CONTRIBUTING.md).

## License

MIT — see [LICENSE](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE).
