# hjkl-xdg

XDG Base Directory resolution — env-first, $HOME-fallback, uniform across
platforms

[![CI](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hjkl-xdg.svg)](https://crates.io/crates/hjkl-xdg)
[![docs.rs](https://img.shields.io/docsrs/hjkl-xdg)](https://docs.rs/hjkl-xdg)
[![MSRV](https://img.shields.io/badge/MSRV-1.95-blue.svg)](Cargo.toml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE)

Part of the [hjkl monorepo](https://github.com/kryptic-sh/hjkl) — a vim-modal
editor in Rust.

Honors `$XDG_CONFIG_HOME` / `$XDG_DATA_HOME` / `$XDG_CACHE_HOME` when set to an
absolute path. Falls back to `~/.config` / `~/.local/share` / `~/.cache` on
every platform — including macOS and Windows. Deliberately does not use
platform-native paths (`~/Library/Application Support`, `%APPDATA%`) so that
hjkl-style CLI tools produce an identical layout everywhere.

## Usage

```rust
use hjkl_xdg::{config_dir, data_dir, cache_dir};

let cfg = config_dir("myapp")?;  // ~/.config/myapp
let data = data_dir("myapp")?;   // ~/.local/share/myapp
let cache = cache_dir("myapp")?; // ~/.cache/myapp
```

## Documentation

[docs.rs/hjkl-xdg](https://docs.rs/hjkl-xdg)

## Contributing

See the
[monorepo CONTRIBUTING guide](https://github.com/kryptic-sh/hjkl/blob/main/CONTRIBUTING.md).

## License

MIT — see [LICENSE](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE).
