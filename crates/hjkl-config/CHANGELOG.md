# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). This
project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Fixed

- `write_key_at` now writes atomically (temp-file + `fsync` + `rename`) so a
  crash or I/O error mid-write never leaves a partial or truncated config file.
  The read-modify-write sequence is also guarded by a lock file
  (`<config>.lock`) with retry, preventing concurrent hjkl processes from
  silently losing each other's updates.

[Unreleased]: https://github.com/kryptic-sh/hjkl-config/compare/v0.2.0...HEAD

## [0.2.0] - 2026-05-03

XDG-everywhere path resolution. Same paths on Linux, macOS, and Windows —
`~/.config/<app>` / `~/.local/share/<app>` / `~/.cache/<app>` (with
`$XDG_CONFIG_HOME` / `$XDG_DATA_HOME` / `$XDG_CACHE_HOME` honored on every
platform). Replaces the prior `directories` crate which produced platform-native
paths (reverse-DNS Bundle ID prefix on macOS, deeper `<org>\<app>\<purpose>`
structure on Windows).

### Breaking

- **`AppConfig::QUALIFIER` and `AppConfig::ORGANIZATION` const fields removed.**
  They were only used by the `directories::ProjectDirs` resolver. Consumer impls
  that set them — most don't — should drop the override; the trait is simpler
  now (just `APPLICATION` + optional `FILE`).
- **`config_dir<C>()` removed.** Replaced by the free function
  `config_dir(app: &str)`. Call sites with an `AppConfig` type can get the dir
  via `config_dir(C::APPLICATION)` or get the full file path via
  `config_path::<C>()` (still generic, unchanged signature).
- **`ConfigError::NoConfigDir { app }` renamed to `ConfigError::NoHomeDir`** (no
  body). Triggered when `dirs::home_dir()` returns `None` and no XDG var is set
  — a system-level failure, not an app-specific one.
- **macOS path migration.** Existing macOS users move from
  `~/Library/Application Support/sh.<org>.<app>/` to `~/.config/<app>/`. Linux
  unchanged.
- **Windows path migration.** Existing Windows users move from
  `%APPDATA%\<org>\<app>\config\` to `~/.config/<app>/`. Linux unchanged.

### Added

- Free functions for raw directory lookups, no `AppConfig` type required:
  - `config_dir(app: &str) -> Result<PathBuf, ConfigError>`
  - `data_dir(app: &str) -> Result<PathBuf, ConfigError>`
  - `cache_dir(app: &str) -> Result<PathBuf, ConfigError>` Useful for multi-file
    scenarios where the dir is shared but not every file maps to an `AppConfig`
    impl (e.g. sqeel's `conns/*.toml`, `session.toml`, on-disk results).

### Changed

- Replaced `directories` dependency with `dirs` (used only for `home_dir()`
  lookup). Drops a transitive dep tree (~70KB).
- `xdg_base()` resolver: honors `XDG_CONFIG_HOME` / `XDG_DATA_HOME` /
  `XDG_CACHE_HOME` if set to a non-empty absolute path; otherwise falls back to
  `~/.config` / `~/.local/share` / `~/.cache`. Empty values and relative paths
  are ignored per XDG Base Directory spec.
- 5 new XDG resolver tests in `loader.rs` covering env-var override, empty-value
  fallback, relative-path rejection, and data/cache dir resolution.

[0.2.0]: https://github.com/kryptic-sh/hjkl-config/releases/tag/v0.2.0

## [0.1.1] - 2026-05-03

### Fixed

- CI: `Swatinem/rust-cache@v2` is now `continue-on-error: true` on the test
  matrix. Windows runners occasionally flake during cache restore (race with the
  runner's antivirus); a cache miss must not block tests — they just run slower
  without it. No code changes; consumers can stay on 0.1.x.

[0.1.1]: https://github.com/kryptic-sh/hjkl-config/releases/tag/v0.1.1

## [0.1.0] - 2026-05-03

Initial public release. Shared TOML config loader for hjkl-based apps with XDG
path resolution, span-aware parse errors, layered defaults+overrides, an opt-in
validation hook, and reusable bounds-check helpers.

### Added

- `AppConfig` trait — implementing types declare `APPLICATION` (and optionally
  `QUALIFIER` / `ORGANIZATION` / `FILE`) constants used by
  [`directories::ProjectDirs`]. (Note: `QUALIFIER` / `ORGANIZATION` removed in
  0.2.0 along with the `directories` dependency.)
- `ConfigSource { File(PathBuf), Defaults }` — distinguishes user-loaded config
  from in-memory defaults without writing to disk.
- `load`, `load_from`, `config_dir`, `config_path` free functions for XDG-based
  loading and path resolution. (`config_dir` signature changed in 0.2.0 — see
  breaking changes above.)
- `load_layered<C>(defaults_toml)` and
  `load_layered_from<C>(defaults_toml, path)` — parse a bundled defaults TOML
  (typically embedded via `include_str!()`) as the seed value, then deep-merge a
  user file on top. Lets consumers keep default _values_ in a TOML source-tree
  file rather than in Rust code, satisfying single-source-of-truth.
- `write_default` opt-in helper for apps that want to scaffold a starter config
  on user request — never invoked automatically by `load`.
- `Validate` trait — opt-in consumer-defined validation hook decoupled from
  loading.
- `ValidationError { field, message }` carrying a field name and human-readable
  message, plus reusable bounds-check helpers: `ensure_range`,
  `ensure_non_zero`, `ensure_one_of`, `ensure_non_empty_str`. Each returns
  `ValidationError` on violation with the field name baked in, so consumers can
  compose a `Validate` impl without writing boilerplate per-field.
- `ConfigError` enum with `NoConfigDir`, `Io`, `Write`, `Parse`, and `Invalid`
  variants. `Parse` carries `line`, `col`, and `snippet` for human-readable
  span-aware diagnostics; `Invalid` covers schema-level errors (unknown user
  key, wrong type, malformed bundled defaults) where span info isn't available.
- `ConfigError` is `#[non_exhaustive]` — future variants can be added without a
  major bump.

[0.1.0]: https://github.com/kryptic-sh/hjkl-config/releases/tag/v0.1.0
