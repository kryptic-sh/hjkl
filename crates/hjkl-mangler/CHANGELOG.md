# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). This
project adheres to [Semantic Versioning](https://semver.org/) once it reaches
0.1.0; the 0.0.x series is a churn phase where breaking changes may land on
patch bumps.

## [Unreleased]

## [0.1.0] - 2026-05-16

### Added

- `Formatter` trait — `format(source, project_root, range)` returns
  `Result<String, FormatError>`.
- `formatter_for_path(&Path) -> Option<Box<dyn Formatter>>` — dispatches by file
  extension across 8 built-in tools (rustfmt, prettier, gofmt, ruff, stylua,
  shfmt, taplo, black).
- `RangeSpec { start_row, end_row }` for native per-formatter range flags
  (rustfmt `--file-lines`, prettier `--range-start/--range-end`, stylua
  `--range-start/--range-end`, ruff `--range`).
- `FormatWorker` — async per-buffer format dispatch with stale-result drop via
  `dirty_gen` and per-`BufferId` dedup.
- `is_tool_installed(tool)` / `probe_tool(tool)` — host-side availability
  checks. `is_tool_installed` uses spawn-success (compatible with shells like
  dash that don't grok `--version`); `probe_tool` runs `--version` and returns
  exit-code diagnostics.
- `FORMAT_TIMEOUT = 30s` — covers large-file rustfmt runs without prematurely
  killing the child.
- `StdinFormatter`, `PrettierFormatter`, `RustFormatter`, `StyluaFormatter`,
  `RuffFormatter`, `BlackFormatter` concrete impls — drain stdout/stderr on
  background threads before writing stdin (required for outputs above the 64 KiB
  pipe buffer).

[Unreleased]: https://github.com/kryptic-sh/hjkl-mangler/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/kryptic-sh/hjkl-mangler/releases/tag/v0.1.0
