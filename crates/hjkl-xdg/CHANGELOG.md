# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). This
project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.0] - 2026-05-10

### Added

- `config_home()` — resolves `$XDG_CONFIG_HOME` or falls back to `~/.config`.
- `data_home()` — resolves `$XDG_DATA_HOME` or falls back to `~/.local/share`.
- `cache_home()` — resolves `$XDG_CACHE_HOME` or falls back to `~/.cache`.
- `config_dir(app)` — resolves `<config_home>/<app>`.
- `data_dir(app)` — resolves `<data_home>/<app>`.
- `cache_dir(app)` — resolves `<cache_home>/<app>`.
- `Error` enum with `NoHomeDir` variant for when `$HOME` cannot be resolved.
- Pure `resolve_xdg` internal helper — no I/O or env access, enabling
  parallel-safe unit tests without `std::env::set_var`.
- XDG spec compliance: relative-path env values are ignored per spec; only
  absolute paths are honored.
- Uniform cross-platform behavior: deliberately avoids platform-native dirs
  (`~/Library/Application Support`, `%APPDATA%`) for consistent CLI layouts.
