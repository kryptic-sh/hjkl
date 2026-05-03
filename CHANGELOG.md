# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). This
project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.0] - 2026-05-03

### Changed

- **Renamed from `hjkl-tree-sitter`.** The github repo `kryptic-sh/hjkl-tree-sitter`
  was renamed to `kryptic-sh/hjkl-bonsai` (GitHub auto-redirects the old URL,
  so prior issues/PRs/stars are preserved). The `hjkl-tree-sitter` crate on
  crates.io stays as a deprecated artifact at `0.5.0` — this crate continues
  the same code lineage under the new name, restarting at `0.1.0`.
- Public API is unchanged: `DotFallbackTheme`, `Highlighter`, `LanguageRegistry`,
  `Theme`, `CommentMarkerPass`, `HighlightSpan`, `ParseError`, `Syntax` all
  remain in the root namespace. Migration is `s/hjkl_tree_sitter/hjkl_bonsai/g`
  in import paths.

## Pre-rename history

Releases below were published as `hjkl-tree-sitter` on crates.io. The full
history is preserved in this repo (renamed from `kryptic-sh/hjkl-tree-sitter`
on 2026-05-03).

## [0.5.0] - 2026-05-03

### Added

- 13 new bundled grammars: JavaScript, C++, C#, Java, PHP, Ruby, Swift, Lua,
  Dart, R, Make, XML, Diff. Bundled grammar count grows from 14 to 27.
- JavaScript covers `.js`, `.mjs`, `.cjs`, `.jsx` (the same grammar handles
  JSX-light syntax).
- C++ routes `.cpp`, `.cc`, `.cxx`, `.hpp`, `.hxx`, `.hh` — `.h` stays on C
  since most C projects use `.h` for headers.
- PHP uses `LANGUAGE_PHP` (handles embedded HTML), not `LANGUAGE_PHP_ONLY`.

### Changed

- Binary footprint grows ~17 MB release-stripped from the new grammars (`hjkl`
  umbrella binary 14.8 MB → 31.8 MB). This is the practical ceiling for the
  bake-in approach; future language additions will move to the upcoming `bonsai`
  runtime grammar loader.

### Deprecated

- This crate is being superseded by
  [`bonsai`](https://github.com/kryptic-sh/bonsai), which will replace bundled
  grammars with a Helix-style compile-on-demand loader (`cc` on user's machine,
  cache to `~/.cache/hjkl/`). Distros will ship pre-compiled `.so` files in
  `/usr/share/hjkl/runtime/grammars/`. `hjkl-tree-sitter` 0.5.x is the last
  bundled-grammar release; the next hjkl umbrella will switch to bonsai.

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
