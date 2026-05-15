# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). This
project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.7.1] - 2026-05-15

### Added

- `lua-match?` and `not-lua-match?` built-in predicates. Lua patterns are
  translated to Rust `regex` at eval time; unsupported constructs (`%b`) fall
  back to permissive (match passes). Eliminates the
  `unknown predicate — match still emitted predicate="lua-match?"` warning that
  appeared when opening Lua files.

### Changed

- Added `regex = "1"` dependency (already a transitive dep; no compile-time
  regression).

## [0.7.0] - 2026-05-15

### Changed

- Migrated theme parsing onto `hjkl-theme` + `hjkl-theme-tui`. The bespoke
  `Style` struct, `parse_hex_color`, `RawStyle`, and
  `TryFrom<RawStyle> for Style` are removed; all parsing and palette resolution
  is now handled by `hjkl-theme`.
- `bonsai::Style` is removed. Callers should use `hjkl_theme::StyleSpec`
  directly. The type is re-exported from `hjkl-bonsai` as `StyleSpec` for
  convenience.
- `Theme::style` now returns `Option<&hjkl_theme::StyleSpec>` (borrowed) instead
  of `Option<Style>` (owned clone).
- `Style::to_ratatui` is removed. Callers needing a `ratatui::style::Style`
  should add `hjkl-theme-tui` to their own deps and call
  `hjkl_theme_tui::ToRatatui::to_ratatui()` on the `StyleSpec`.
- Theme TOML schema updated: capture keys are `@`-prefixed tree-sitter names
  (`@keyword`, `@function`, …). Modifiers use the array form
  `modifiers = ["bold", "italic"]`. Palette refs use `$name` syntax. See
  `themes/default-dark.toml` as the canonical reference. Old flat themes
  (without `@` prefix) need conversion before they will parse.
- `ratatui` removed from `hjkl-bonsai` direct dependencies.

## [0.6.2] - 2026-05-12

### Performance

- `Highlighter` now caches child highlighters across
  `highlight_range_with_injections` and `highlight_with_injections` calls. Keys
  are `(language_name, content_range_start, content_range_end)` with a content
  hash for drift detection: if the source slice at a given range changes, the
  entry is evicted and rebuilt; if it is unchanged the parse tree and highlight
  query are reused at zero parse cost. Stale entries (code blocks that have been
  deleted or scrolled out of view) are evicted after each call. For a markdown
  file with N fenced code blocks, this reduces child parse cost from O(N ×
  frames) to O(N) after the first render frame. Resolves
  [hjkl#65](https://github.com/kryptic-sh/hjkl/issues/65).

## [0.6.1] - 2026-05-10

### Changed

- Replaced inlined XDG resolver in `src/runtime/xdg.rs` with `hjkl-xdg = "0.1"`
  dep. `xdg::data_home()` and `xdg::cache_home()` retain the same callable shape
  via `pub use`.

## [0.6.0] - 2026-05-09

### Added

- Predicate/directive dispatcher giving helix + nvim-treesitter parity over
  stock tree-sitter without forking the parser. New module `predicate` exposes
  the `MatchContext`, `Predicate`, `Directive`, `MatchMetadata`, `MetaValue`,
  `PredicateArg`, and `PredicateRegistry` types, plus `predicate_fn` /
  `directive_fn` closure sugar so consumers register handlers without naming a
  struct.
- New module `builtins` ships parser-agnostic implementations of `contains?`,
  `has-ancestor?`, `has-parent?` (predicates) and `set!` (literal +
  capture-target forms), `offset!`, `trim!`, `gsub!` (directives). Builtins are
  pre-registered by `PredicateRegistry::with_builtins`.
- `Highlighter::with_registry(grammar, registry)` constructor for consumers that
  want to extend the default registry. `Highlighter::new` is unchanged and uses
  `with_builtins` internally.
- `HighlightSpan` gains a `metadata: HashMap<String, MetaValue>` field carrying
  per-capture metadata produced by directives. The pre-existing `byte_range` and
  `capture` fields are unchanged.
- `query_sanitize::extract_capture_set_directives` — pre-extracts
  `(#set! @cap key val)` forms (which stock tree-sitter rejects at compile) into
  `Vec<CaptureSetDirective>` keyed by pattern index, returning the rewritten
  compilable query alongside the extracted directives. The highlighter
  re-applies them at match-iteration time so the directive semantics are
  preserved instead of dropped. Resolves
  [hjkl-bonsai#4](https://github.com/kryptic-sh/hjkl-bonsai/issues/4).

### Changed

- `sanitize_highlights` is now a fallback path: if pre-extraction still leaves
  an uncompilable query, the legacy strip-the-form behavior runs. The function
  signature is unchanged.
- Unknown predicates encountered during match iteration are no longer fatal.
  They are logged once per name via `tracing::warn!` (deduped through a
  `OnceLock<Mutex<HashSet>>`) and the match is still emitted. Mirrors helix and
  nvim-treesitter's graceful-degradation behavior.

### Fixed

- Paren-balanced excision of `(#set! @capture ...)` forms in
  `query_sanitize::sanitize_highlights`. The previous line-based stripper
  removed the entire line, which silently ate a closing `)` belonging to the
  enclosing pattern when the directive's `)` shared a line with the outer
  group's `)` — most visibly in the resolved nvim-treesitter html highlights.
  The new scanner tracks paren depth and string literals, excising only the
  `(#set! @cap ...)` subexpression and leaving surrounding parens intact.
  Resolves [hjkl-bonsai#3](https://github.com/kryptic-sh/hjkl-bonsai/issues/3).

## [0.5.4] - 2026-05-06

### Fixed

- Per-key mutex dedup in `SourceCache` and `QuerySourceCache`: two workers
  racing on a shared Helix `QuerySourceCache` staging dir are now serialised
  with an `Arc<Mutex<()>>` per language key; after acquiring the lock the
  freshness check is repeated so a second caller skips a duplicate clone that
  already completed.

### Added

- `AsyncGrammarLoader::in_flight_names() -> Vec<String>` — snapshot method
  returning the names of all grammars currently being loaded. Intended for
  driving a global spinner in TUI hosts.

## [0.5.3] - 2026-05-05

### Added

- `Grammar::load_from_path(name, so)` — fast path that skips the `GrammarLoader`
  chain and goes straight to `dlopen` + query reads when the `.so`,
  `<name>.scm`, and optional `<name>.injections.scm` are already on disk
  together. Used by consumers that complete an `AsyncGrammarLoader` job and need
  to materialize the `Grammar` from the resolved path without re-running
  freshness checks.

  (v0.5.2 announced this addition but its commit accidentally staged only the
  manifest + changelog; the function body was never committed. Use 0.5.3+.)

## [0.5.1] - 2026-05-05

### Added

- `AsyncGrammarLoader`, `LoadHandle`, and `LoadError` in `runtime::async_loader`
  (re-exported at `runtime` level). Wraps `GrammarLoader` in a 2-worker thread
  pool with mpsc-as-pool dispatch. Multiple concurrent `load_async("rust", …)`
  calls share one in-flight clone+compile job — no duplicate work. Sync
  `GrammarLoader::load` is unchanged; consumers pick the API they need.
  Addresses [hjkl#17](https://github.com/kryptic-sh/hjkl/issues/17) — the
  grammar-load freeze reported in the v0.6.0 release notes.

## [0.5.0] - 2026-05-05

### Added

- `Highlighter::highlight_range_with_injections` — viewport-scoped variant of
  `highlight_with_injections`. Skips parsing (caller-driven), restricts both the
  parent-span query and the injection-query walk to `byte_range`, and clips
  translated child spans to the same range. Same merge semantics: child spans
  replace parent spans inside injected ranges.

## [0.4.1] - 2026-05-04

### Changed

- `.gitignore` now excludes the `build/` directory used by
  `xtask build-grammars` for compiled grammar artifacts (`.so`/`.scm`/`.rev`).

## [0.4.0] - 2026-05-04

### Breaking

- **`LangSpec.query_dir: String` removed.** Replaced by
  `query_source: QuerySource` (enum: `Helix | NvimTreesitter`) and
  `query_subdir: Option<String>`. Callers constructing `LangSpec` manually must
  update field names. `bonsai.toml` entries that used `query_dir = "..."` now
  use `query_source = "helix"` or `query_source = "nvim_treesitter"`.
- **`GrammarLoader::new` takes a fifth argument `QuerySourceCache`.** All call
  sites must supply one (use `QuerySourceCache::user_default()` for the standard
  path).
- **`GrammarLoader::user_default`, `GrammarLoader::load`,
  `GrammarLoader::lookup_fresh`, and `Grammar::load` now take `&ManifestMeta`.**
  Callers obtain this from `Manifest::meta` or `GrammarRegistry::meta()`.
- **`.rev` sidecar format changed from `<rev>:abi<N>` to
  `<rev>:<query_short_rev>:abi<N>`.** Old two-field sidecars parse as stale →
  all existing user-dir grammars will be recompiled on first use after upgrade.
- **`bonsai.toml` now requires a `[meta]` block** with `helix_repo`,
  `helix_rev`, `nvim_treesitter_repo`, and `nvim_treesitter_rev`.

### Changed

- Query sources are now helix and nvim-treesitter repos (curators), not each
  grammar repo's own `queries/` directory. Queries are fetched via sparse-clone
  into `<XDG_CACHE_HOME>/bonsai/query-sources/{helix,nvim-treesitter}-<rev>/`.
- `; inherits: foo,bar` directives in helix/nvim-treesitter query files are
  expanded at install time: parent content is concatenated before child content,
  transitively. The resolved single-file `.scm` is what gets installed as
  `<name>.scm` in the user dir.
- `xtask sync-bonsai` derives `query_source` from merge provenance: helix-only
  grammars get `QuerySource::Helix`; nvim-only or shared grammars get
  `QuerySource::NvimTreesitter` (nvim-treesitter has more comprehensive queries
  for shared langs). No network probes — two HEAD-SHA fetches and done. The
  `[meta]` block is written with pinned HEAD SHAs of both source repos.
- `GrammarRegistry::meta()` added — returns `&ManifestMeta` so callers can build
  a `GrammarLoader` with one registry lookup.

## [0.3.0] - 2026-05-03

### Breaking

- **Grammar storage subdir renamed `hjkl/` → `bonsai/`.** System dirs are now
  `/usr/share/bonsai/grammars/` + `/usr/local/share/bonsai/grammars/`; user data
  dir is `<user_data>/bonsai/grammars/`; user cache dir is
  `<user_cache>/bonsai/grammars/`. Existing grammars under `hjkl/grammars/` are
  not migrated — they will be re-fetched and re-compiled into the new
  `bonsai/grammars/` location on first use. Distro packagers must update their
  PKGBUILD / spec / control files to install grammars under
  `usr/share/bonsai/grammars/`.
- **`<user_data>` / `<user_cache>` now follow XDG-everywhere.** Resolution
  honors `$XDG_DATA_HOME` / `$XDG_CACHE_HOME` first (with `~/.local/share` /
  `~/.cache` fallback) on every platform. macOS users move from
  `~/Library/Application Support/hjkl/grammars/` +
  `~/Library/Caches/hjkl/grammars/` to `~/.local/share/bonsai/grammars/` +
  `~/.cache/bonsai/grammars/`. Windows users move from
  `%APPDATA%\hjkl\grammars\` + `%LOCALAPPDATA%\hjkl\grammars\` to the same XDG
  paths. Linux users move from `hjkl/` to `bonsai/` subdirs but stay on XDG. The
  resolver is a private 30-line module — bonsai doesn't pull `hjkl-config` so it
  stays usable without the rest of the hjkl umbrella.

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

[Unreleased]: https://github.com/kryptic-sh/hjkl-bonsai/compare/v0.7.1...HEAD
[0.7.1]: https://github.com/kryptic-sh/hjkl-bonsai/compare/v0.7.0...v0.7.1
[0.7.0]: https://github.com/kryptic-sh/hjkl-bonsai/compare/v0.6.2...v0.7.0
[0.6.2]: https://github.com/kryptic-sh/hjkl-bonsai/compare/v0.6.1...v0.6.2
[0.6.1]: https://github.com/kryptic-sh/hjkl-bonsai/releases/tag/v0.6.1
[0.6.0]: https://github.com/kryptic-sh/hjkl-bonsai/releases/tag/v0.6.0
[0.5.4]: https://github.com/kryptic-sh/hjkl-bonsai/releases/tag/v0.5.4
[0.5.3]: https://github.com/kryptic-sh/hjkl-bonsai/releases/tag/v0.5.3
[0.5.2]: https://github.com/kryptic-sh/hjkl-bonsai/releases/tag/v0.5.2
[0.5.1]: https://github.com/kryptic-sh/hjkl-bonsai/releases/tag/v0.5.1
[0.5.0]: https://github.com/kryptic-sh/hjkl-bonsai/releases/tag/v0.5.0
[0.4.1]: https://github.com/kryptic-sh/hjkl-bonsai/releases/tag/v0.4.1
[0.4.0]: https://github.com/kryptic-sh/hjkl-bonsai/releases/tag/v0.4.0
[0.3.0]: https://github.com/kryptic-sh/hjkl-bonsai/releases/tag/v0.3.0
[0.2.1]: https://github.com/kryptic-sh/hjkl-bonsai/releases/tag/v0.2.1
[0.2.0]: https://github.com/kryptic-sh/hjkl-bonsai/releases/tag/v0.2.0
[0.1.0]: https://github.com/kryptic-sh/hjkl-bonsai/releases/tag/v0.1.0
[0.5.0]: https://github.com/kryptic-sh/hjkl-bonsai/releases/tag/v0.5.0
[0.4.0]: https://github.com/kryptic-sh/hjkl-bonsai/releases/tag/v0.4.0
[0.3.1]: https://github.com/kryptic-sh/hjkl-bonsai/releases/tag/v0.3.1
