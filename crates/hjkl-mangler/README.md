# hjkl-mangler

External-formatter dispatch for hjkl: rustfmt, prettier, gofmt, ruff, stylua,
shfmt, taplo and more

[![CI](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hjkl-mangler.svg)](https://crates.io/crates/hjkl-mangler)
[![docs.rs](https://img.shields.io/docsrs/hjkl-mangler)](https://docs.rs/hjkl-mangler)
[![MSRV](https://img.shields.io/badge/MSRV-1.95-blue.svg)](Cargo.toml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE)

Part of the [hjkl monorepo](https://github.com/kryptic-sh/hjkl) — a vim-modal
editor in Rust.

Format-on-`=` and format-on-save dispatch layer for the hjkl workspace. Wraps
each external formatter behind a uniform `Formatter` trait so the editor only
sees `format(source, project_root, range) -> Result<String, FormatError>`.

## Status

Pre-1.0. Trait shape and built-in impls are stable enough to use from production
editors; new formatter impls land additively.

## Features

- 8 built-in formatters: rustfmt, prettier, gofmt, ruff, stylua, shfmt, taplo,
  black. Dispatched by file extension via `formatter_for_path`.
- Native range-format flags per tool (rustfmt `--file-lines`, prettier
  `--range-start/--range-end`, stylua `--range-start/--range-end`, ruff
  `--range`). No diff splicing.
- Async `FormatWorker` for non-blocking dispatch with per-buffer dedup and
  stale-result drop.
- `is_tool_installed(name)` / `probe_tool(name)` for host-side fallback
  decisions (e.g. fall back to a dumb indent algo when the external tool is
  missing).
- Pipe-deadlock-safe subprocess driver (stdout/stderr drained on threads before
  stdin write — required for >64 KiB output).

## Usage

```toml
hjkl-mangler = "0.1"
```

```rust
use hjkl_mangler::{formatter_for_path, RangeSpec};
use std::path::Path;

let path = Path::new("src/lib.rs");
let formatter = formatter_for_path(path).expect("rustfmt for .rs");
let formatted = formatter
    .format("fn main() {println!(\"hi\");}", Path::new("."), None)
    .expect("rustfmt should succeed");
println!("{formatted}");
```

For async dispatch see `FormatWorker`. For range-only formatting pass
`Some(RangeSpec { start_row, end_row })`.

## Documentation

[docs.rs/hjkl-mangler](https://docs.rs/hjkl-mangler)

## Contributing

See the
[monorepo CONTRIBUTING guide](https://github.com/kryptic-sh/hjkl/blob/main/CONTRIBUTING.md).

## License

MIT — see [LICENSE](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE).
