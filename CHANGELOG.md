# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). This
project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.4.0] - 2026-05-03

### Added

- 8 new bundled grammars: Python, TypeScript, TSX, Go, YAML, Bash, C, HTML, CSS
  (9 configs total — TypeScript and TSX share the `tree-sitter-typescript`
  crate). All highlight queries pulled directly from upstream grammar crate
  constants; no vendored `.scm` files.
- Umbrella test asserts every registered language compiles its
  `HIGHLIGHTS_QUERY` against its `LANGUAGE` — guards against upstream version
  drift between grammar and queries.

### Changed

- Bundled grammar count grew from 5 to 14. Binary size impact: ~8–12 MB
  release-stripped across the 8 new grammar `.so` artifacts. All grammars remain
  unconditionally compiled in (no feature gates).

## [0.3.1] - 2026-04-30

### Changed

- Migrated `hjkl-tree-sitter` from the `kryptic-sh/hjkl` monorepo into its own
  repository
  ([kryptic-sh/hjkl-tree-sitter](https://github.com/kryptic-sh/hjkl-tree-sitter))
  with full git history preserved.
- Relaxed inter-crate dependency requirements from `=0.3.0` to `0.3` (caret),
  matching the standard SemVer pattern for library dependencies.
- Bumped `toml` to 1.1 (was 0.8) and `ratatui` to 0.30 (was 0.29).

### Added

- Standalone `LICENSE`, `.gitignore`, and `ci.yml` workflow at the repo root.
