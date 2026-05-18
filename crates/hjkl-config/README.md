# hjkl-config

Shared TOML config loader for hjkl-based apps — XDG path resolution, span-aware
parse errors, opt-in validation hook.

[![CI](https://github.com/kryptic-sh/hjkl-config/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl-config/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hjkl-config.svg)](https://crates.io/crates/hjkl-config)
[![docs.rs](https://img.shields.io/docsrs/hjkl-config)](https://docs.rs/hjkl-config)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Website](https://img.shields.io/badge/website-hjkl.kryptic.sh-7ee787)](https://hjkl.kryptic.sh)

Implements one of the few cross-cutting patterns shared by every hjkl-family
app: read a `config.toml` from the platform's XDG config dir, fall back to
in-memory defaults when the file is missing, and surface parse errors with
line/column/snippet context. No file is ever auto-created — apps that want to
scaffold a starter config call `write_default` explicitly.

## Status

Pre-1.0. Public API is small and stable; expect additive changes (env-var
overlay, CLI merge helpers, `notify`-based reload watcher) on minor bumps.

## Usage

```toml
hjkl-config = "0.1"
```

```rust,no_run
use hjkl_config::{AppConfig, load};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
struct MyConfig {
    greeting: String,
    count: u32,
}

impl AppConfig for MyConfig {
    const APPLICATION: &'static str = "myapp";
}

let (cfg, source) = load::<MyConfig>().expect("load config");
println!("loaded from {:?}: {}", source, cfg.greeting);
```

The `MyConfig::default()` value is returned with `ConfigSource::Defaults` when
no file exists on disk. Otherwise the file at
`$XDG_CONFIG_HOME/myapp/config.toml` (or platform equivalent) is parsed.

## What's here

- **`AppConfig`** trait — declare `APPLICATION` (required) plus optional
  `QUALIFIER` / `ORGANIZATION` / `FILE` constants. Identity passed to
  [`directories::ProjectDirs`].
- **`load<C>()`** — XDG load with `Default`-on-missing, never writes to disk.
- **`load_from<C>(path)`** — explicit path load (used for `--config <PATH>`
  flags and tests).
- **`config_dir<C>()` / `config_path<C>()`** — resolve the directory or full
  file path without loading.
- **`write_default<C: Serialize>(path, cfg)`** — opt-in serialize-and-write
  helper. Creates parent dirs. Never auto-called.
- **`ConfigError`** — `NoConfigDir`, `Io`, `Write`, `Parse { line, col,
  snippet, ... }`.
- **`ConfigSource { File(PathBuf), Defaults }`** — log-friendly tag for
  whether the loaded value came from disk.
- **`Validate`** trait — opt-in hook with consumer-defined error type. `load`
  does not call it; consumers invoke `cfg.validate()` themselves.

## Platform paths

Resolved via [`directories`]:

- **Linux** — `$XDG_CONFIG_HOME/<app>/config.toml`
  (default `~/.config/<app>/config.toml`)
- **macOS** — `~/Library/Application Support/<qualifier>.<org>.<app>/config.toml`
- **Windows** — `%APPDATA%\<org>\<app>\config\config.toml`

## License

MIT. See [LICENSE](LICENSE).

[`directories`]: https://docs.rs/directories
