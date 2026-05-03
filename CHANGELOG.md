# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). This
project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.2.1] - 2026-05-03

### Changed

- README rewritten for the 0.2.0 runtime loader API: dropped the bundled-
  grammar narrative, added the lookup chain, distro packaging recipe via
  `cargo xtask build-grammars`, and the per-platform user-dir table. Affects the
  rendered crates.io page only — no code changes.

## [0.2.0] - 2026-05-03

Major rework: runtime grammar loading. The 27 baked `tree-sitter-*` dependencies
are gone; grammars are now resolved at runtime from a manifest of 421 languages
(sourced from helix + nvim-treesitter).

### Breaking

- Removed `LanguageRegistry`, `LanguageConfig`, `detect_language_for_path`, and
  the entire `src/languages/` tree of bundled grammars.
- `Highlighter::new` now takes `Arc<runtime::Grammar>` instead of
  `&LanguageConfig`. The `Grammar` is what owns the `dlopen`-ed
  `libloading::Library` plus the highlights query.
- 27 `tree-sitter-*` crate dependencies removed (rust, python, go, ts, c, cpp,
  java, php, ruby, swift, lua, dart, r, make, xml, diff, c-sharp, html, css,
  javascript, bash, yaml, json, toml-ng, sequel, md). Consumers wanting a
  pre-loaded grammar should use the new `runtime::GrammarLoader` chain.

### Added

- `runtime` module with the new loader API:
  - `Manifest` / `GrammarRegistry` — parse `bonsai.toml`, look up `LangSpec` by
    name or file extension.
  - `SourceCache` — clone the upstream grammar repo on demand into
    `$XDG_CACHE_HOME/hjkl/grammars/<name>-<rev>/`.
  - `GrammarCompiler` — compile the cloned `c_files` into
    `<source_root>/<name>.<so|dylib|dll>` via `cc` / `c++` (overridable via
    `$CC` / `$CXX`).
  - `GrammarLoader` — walks the lookup chain (system → user → on-demand build)
    and installs the result as `<user_dir>/<name>.{so,scm,rev}`.
  - `Grammar::load(name, spec, loader)` — the high-level entry that `dlopen`s
    the parser and reads the highlights query.
- `bonsai.toml` shipped with the crate: 421 languages with pinned `git_url`,
  `git_rev`, `c_files`, `query_dir`, `extensions`, etc.
- `<name>.rev` sidecar in the install dir records `<git_rev>:abi<N>` so the
  loader detects stale installs (manifest-rev or tree-sitter ABI bump) and
  recompiles in place.
- `cargo xtask sync-bonsai` regenerates `bonsai.toml` from the upstream sources.
  `cargo xtask build-grammars` pre-builds grammars into a flat install dir for
  distro packagers.

### Changed

- Library rlib shrinks 8.5 MB → 721 KB (release) — almost the entire saving
  comes from dropping the baked grammars.
- Install layout (and corresponding `GrammarLoader::user_default`):
  - `<system>/hjkl/grammars/` — `/usr/share`, `/usr/local/share` on Unix.
    Distro-shipped, never rev-checked.
  - `<user_data>/hjkl/grammars/` — install target for on-demand builds. Each
    grammar lives as a flat triple: `<name>.so`, `<name>.scm`, `<name>.rev`.
  - `<user_cache>/hjkl/grammars/` — source clones, one dir per `<name>-<rev>`
    combo. Transient.
- User directories resolved via the `dirs` crate, so Windows / macOS get the
  right platform paths instead of bailing on `$HOME not set`.
- comment-markers tests are now `#[ignore]`-gated — they need a real
  `tree-sitter-rust` grammar (network clone + cc compile) which isn't
  appropriate for the default `cargo test` lane.

### Removed

- `src/languages/` (27 modules) — replaced by the runtime loader.
- `src/registry.rs` (`LanguageRegistry` / `LanguageConfig`) — replaced by
  `runtime::GrammarRegistry` + `runtime::Grammar`.

### Dependencies

- Added: `libloading 0.8`, `tree-sitter-language 0.1`, `dirs 6`.
- Removed: all 27 `tree-sitter-*` grammar crates listed above.

## [0.1.0] - 2026-05-03

### Changed

- **Renamed from `hjkl-tree-sitter`.** The github repo
  `kryptic-sh/hjkl-tree-sitter` was renamed to `kryptic-sh/hjkl-bonsai` (GitHub
  auto-redirects the old URL, so prior issues/PRs/stars are preserved). The
  `hjkl-tree-sitter` crate on crates.io stays as a deprecated artifact at
  `0.5.0` — this crate continues the same code lineage under the new name,
  restarting at `0.1.0`.
- Public API is unchanged: `DotFallbackTheme`, `Highlighter`,
  `LanguageRegistry`, `Theme`, `CommentMarkerPass`, `HighlightSpan`,
  `ParseError`, `Syntax` all remain in the root namespace. Migration is
  `s/hjkl_tree_sitter/hjkl_bonsai/g` in import paths.

## Pre-rename history

Releases below were published as `hjkl-tree-sitter` on crates.io. The full
history is preserved in this repo (renamed from `kryptic-sh/hjkl-tree-sitter` on
2026-05-03).

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

[Unreleased]: https://github.com/kryptic-sh/hjkl-bonsai/compare/v0.2.1...HEAD
[0.2.1]: https://github.com/kryptic-sh/hjkl-bonsai/releases/tag/v0.2.1
[0.2.0]: https://github.com/kryptic-sh/hjkl-bonsai/releases/tag/v0.2.0
[0.1.0]: https://github.com/kryptic-sh/hjkl-bonsai/releases/tag/v0.1.0
[0.5.0]: https://github.com/kryptic-sh/hjkl-bonsai/releases/tag/v0.5.0
[0.4.0]: https://github.com/kryptic-sh/hjkl-bonsai/releases/tag/v0.4.0
[0.3.1]: https://github.com/kryptic-sh/hjkl-bonsai/releases/tag/v0.3.1
