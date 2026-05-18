# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). This
project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.1] - 2026-05-17

### Removed

- Dropped unused `lsp-types` dep (flagged by `cargo machete`; no public API
  change — the type was never re-exported and was eliminated when uri.rs moved
  to plain `url::Url`).

## [0.1.0] - 2026-05-06

### Added

- Initial extraction from `kryptic-sh/hjkl` umbrella into a standalone crate.
- LSP client foundation: per-language server lifecycle manager with full
  text-sync (open / change / save / close) wired onto buffer edits.
- `LspManager` — spawns, supervises, and restarts language server processes;
  routes JSON-RPC requests and responses via `crossbeam-channel`.
- `LspRuntime` — async tokio task driving the JSON-RPC codec over the server's
  stdin/stdout pipes.
- `ServerConfig` + `WorkspaceConfig` — per-language server configuration with
  bundled defaults for rust-analyzer, pyright, typescript-language-server,
  clangd, gopls, and lua-language-server.
- URI utilities (`uri.rs`) — path ↔ `lsp_types::Url` conversion with
  cross-platform absolute-path handling.
- `EventSink` — typed channel for surfacing LSP events (diagnostics, hover
  responses, completion lists, code-action menus) to the host application.

[Unreleased]: https://github.com/kryptic-sh/hjkl-lsp/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/kryptic-sh/hjkl-lsp/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/kryptic-sh/hjkl-lsp/releases/tag/v0.1.0
