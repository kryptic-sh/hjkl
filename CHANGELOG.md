# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). This
project adheres to [Semantic Versioning](https://semver.org/) once it reaches
0.1.0; the 0.0.x series is a churn phase where breaking changes may land on
patch bumps.

## [Unreleased]

### Added

- `:set number` / `:set relativenumber` (and `nu` / `rnu` / `nonu` / `nornu` /
  `nu!` / `rnu!` aliases) toggle the line-number gutter at runtime. Combined
  `nu rnu` enables vim's hybrid mode: cursor row shows its absolute number,
  others show the offset.
- `:set numberwidth=N` / `:set nuw=N` (1..=20, default 4) — minimum gutter width
  in cells, matching vim's `'numberwidth'` option.
- `~` tilde markers paint at the first text column on every screen row past
  end-of-buffer, matching vim's `NonText` rendering. New `non_text` theme color
  (default `#4a5266`).

## [0.11.5] - 2026-05-06

### Added

- **`hjkl-splash` crate, extracted to a standalone repo + crates.io publish.**
  The HJKL splash-screen animation moved out of `apps/hjkl/src/start_screen.rs`
  into a rendering-agnostic crate at
  [kryptic-sh/hjkl-splash](https://github.com/kryptic-sh/hjkl-splash) so other
  kryptic-sh projects can reuse the same cursor-trail-on-letterforms animation
  in TUI or GUI frontends. Core API emits pure `SplashCell` items via an
  iterator; an optional `ratatui` feature ships a
  `From<Rgb> for ratatui::style::Color` adapter. The hjkl letterforms + path are
  bundled as `presets::hjkl`. Wired in via the existing submodule pattern
  - `[patch.crates-io]`.

### Changed

- **Splash now wall-clock driven (via hjkl-splash 0.2.0).** Animation tick is
  derived from `Instant::now()` inside the crate; consumer code no longer calls
  `screen.advance()`, so the animation cannot stall when high-frequency events
  (mouse motion, focus, resize) starve the event-loop timeout branch.
  `apps/hjkl/src/start_screen.rs` is now a thin ratatui adapter; the
  per-iteration `advance()` call in `apps/hjkl/src/app/event_loop.rs` is
  removed.
- **Compat-oracle: graduate substitute cases to a dedicated nvim-api tier
  (closes #26).** New `corpus/nvim_api_tier.toml` holds the four substitute
  cases that previously sat in `known_divergences.toml`; a new
  `nvim_api_tier_passes` test asserts them via the `hjkl --nvim-api` subprocess
  driver on every CI run (no `HJKL_ORACLE_NVIM_API` gate). The old
  `substitute_via_nvim_api` test is removed; `known_divergences.toml` now tracks
  no active divergences. Driver gains cross-platform binary discovery
  (`std::env::consts::EXE_SUFFIX`) and drops the redundant `echo 1` sync barrier
  — hjkl's `nvim_input` / `nvim_command` handlers process synchronously, so the
  awaited response already implies a settled state.

## [0.11.4] - 2026-05-05

### Added

- **Grammar-load status indicator (closes hjkl#17 acceptance gap).** The status
  line now shows `loading grammar: <name>…` while an async grammar load is in
  flight for the active buffer, and `grammar load failed: <name> — <error>` for
  5 seconds when a load fails. Both use the same takeover pattern as the
  `recording @r` indicator.
- **`:qall` / `:qall!` / `:wqall` / `:wqall!` ex commands (closes #27).**
  hjkl-editor 0.4.0 → 0.4.1 adds dispatch arms for the qall family that the
  canonical-name table already advertised. Reverts the `:q!` workaround in
  nvim-api tests + compat-oracle introduced in v0.11.3.

## [0.11.3] - 2026-05-05

### Added

- **`--nvim-api` msgpack-rpc surface (phase 3 of #26).** New flag boots a
  msgpack-rpc server speaking the neovim wire protocol. Existing `nvim-rs`
  clients can target hjkl as a drop-in subprocess replacement for
  `nvim --headless --embed`. Implemented methods: `nvim_get_current_buf`,
  `nvim_get_current_win`, `nvim_buf_set_lines`, `nvim_buf_get_lines`,
  `nvim_win_set_cursor`, `nvim_win_get_cursor`, `nvim_input`, `nvim_command`,
  `nvim_get_mode`, `nvim_call_function` (`getreg` only). Compat-oracle gains an
  opt-in `HJKL_ORACLE_NVIM_API=1` mode that drives hjkl via msgpack-rpc; the
  four substitute cases pass on this path (still in `known_divergences` for the
  in-process driver since the vim FSM cannot dispatch `:` from a key-replay).
- **Non-blocking grammar loads (hjkl#17 follow-up).** `set_language_for_path` no
  longer blocks the UI thread on first-edit clone+compile. New `GrammarRequest`
  / `SetLanguageOutcome` enums; pending loads tracked on `SyntaxLayer` and
  drained each tick via `poll_pending_loads`. Cache / on-disk fast paths
  preserved — only true clone+compile cases now defer.

### Changed

- **hjkl-bonsai 0.5.0 → 0.5.3.** Adds `AsyncGrammarLoader` (2-worker pool with
  in-flight dedup) and `Grammar::load_from_path` for skipping the loader chain
  when the `.so` + queries are already on disk. Sync `GrammarLoader::load`
  unchanged.
- **hjkl-clipboard 0.5.1 → 0.5.3.** Wayland data_source `send` callback no
  longer blocks the UI thread when the paste receiver doesn't drain; adds
  O_NONBLOCK + POLLOUT deferred drain with a 5s deadline reaper. Self-paste
  short-circuits in `do_get` when we own the source. Fixes
  kryptic-sh/hjkl-clipboard#4 (downstream kryptic-sh/buffr#34).

### Fixed

- `--embed` JSON-RPC + `--nvim-api` msgpack-rpc tests sent `:qa!` for shutdown
  but ex.rs canonicalizes `qa!` to `qall!` and has no handler arm — server
  returned an error, never quit, tests hung 5+ minutes per case. Switched to
  `:q!`. Proper `qall` family handler tracked in #27.

## [0.11.2] - 2026-05-05

### Added

- **`recording @r` status indicator.** The status line now shows `recording @r`
  while a q-macro is being recorded, matching vim's native indicator.
- **`--headless` / `-c CMD` script runner (phase 1 of #26).** Launch hjkl with
  `--headless +cmd` or `-c CMD` to execute ex commands non-interactively and
  exit. Enables scripted batch processing without a terminal.
- **JSON-RPC 2.0 server over stdin/stdout (phase 2 of #26).** `--embed` flag
  starts an embedded RPC server, allowing external tools (LSP clients, editors,
  scripts) to drive hjkl over a structured protocol.

## [0.11.1] - 2026-05-05

### Added

- **`:s` / `:%s` / `:<n>,<m>s` / `:'<,'>s` substitute ex-command.** Vim-style
  pattern + replacement with `g` (all on line), `i` (case-insensitive), `I`
  (force case-sensitive) flags. `c` flag is parsed and silently ignored (no
  confirm UI in v1). `&` and `\1`..`\9` work in the replacement. Empty pattern
  (`:s//rep/`) reuses the last `/` search. Status line shows
  `<N> substitutions on <M> lines`. v1 limitations: `/` is the only delimiter,
  no `\v` very-magic (regex syntax is Rust's `regex` crate). Powered by new
  `hjkl_engine::substitute` module.

### Fixed

Eight vim-compat divergences caught by the cron compat-oracle (#24 closed):

- `x` / `X` now write deleted chars to the unnamed register `"`, so `xp` swap
  and other delete-then-paste idioms round-trip.
- `G` clamps to the last content row instead of landing on a phantom row past
  the trailing newline.
- `dd` on the last line clamps the cursor to the new last line instead of
  leaving it past EOF.
- `d$` cursor lands on the last char of the new line, not one column past.
- `u` after an insert clamps the cursor to the last valid column in Normal mode.
- `da"` eats trailing whitespace (or leading if no trailing) per
  `:help text-objects`, instead of leaving a double space.
- `daB` cursor placement matches vim on multi-line brace blocks.
- `diB` preserves the surrounding newlines on multi-line brace blocks
  (`{\n body \n}` → `{\n}`, not `{}`).

### Changed

- `hjkl-engine` 0.3.6 → 0.3.8 (substitute parser/applier in 0.3.7; divergence
  fixes in 0.3.8).
- `hjkl-editor` 0.3.3 → 0.4.0 (`ExEffect::Substituted` carries `lines_changed`
  for the vim-accurate status message).

## [0.11.0] - 2026-05-05

### Added

- **Markdown fenced code blocks render with sub-language highlighting.**
  ` ```rust ` / ` ```python ` / etc. inside `.md` buffers now show the inner
  code with the target language's tree-sitter highlights instead of plain
  `markup.raw.block`. Powered by `hjkl-bonsai` 0.5's new
  `Highlighter::highlight_with_injections` / `highlight_range_with_injections`
  methods plus a per-process child grammar cache in `LanguageDirectory`. First
  fence of an unseen language pays a one-time clone+compile (worker thread, off
  the input path); subsequent renders are a cache hit.
- **Homebrew tap auto-publish** for `hjkl` on tag push. New
  `pkg/homebrew/hjkl.rb.in` template + `brew-tap` job in `release.yml` renders
  the formula with the just-uploaded macOS sha256s and pushes it to
  `kryptic-sh/homebrew-tap`. Install with `brew install kryptic-sh/tap/hjkl`.
- **`hjkl-compat-oracle` crate** (workspace-only, `publish = false`) — headless
  neovim diff harness for vim-compat regression testing. Spawns
  `nvim --headless --embed` per case, drives both nvim and the hjkl engine
  through identical key inputs, diffs buffer/cursor/mode/registers. Tier 1
  corpus covers 44 motion/operator/text-object/count/insert/undo/register cases
  in `corpus/tier1.toml`. 8 confirmed engine divergences surfaced and tracked
  separately in `corpus/known_divergences.toml` + issue #24. Wired into cron CI
  (`.github/workflows/cron.yml`). Closes #23.
- AUR + Alpine `.apk` install paths added to README and the marketing site.

### Fixed

- `hjkl-engine` 0.3.6: `pos_at_byte` no longer panics on byte indices that land
  inside a multi-byte UTF-8 codepoint. Caught by the cargo-fuzz `handle_key`
  target on a Cyrillic-seeded input after the fuzz workspace + patch-deps
  plumbing in `crates/hjkl-engine/fuzz` was repaired.

### Changed

- `hjkl-bonsai` 0.4.1 → 0.5.0 (new injection methods).
- `hjkl-engine` 0.3.4 → 0.3.6 (added `decode_macro` re-export at crate root in
  0.3.5; UTF-8 fix in 0.3.6).
- Marketing site refreshed for v0.10.x: nine ecosystem crates including
  `hjkl-config`, `<leader>g` git surface tagline, `.apk` in install line.

## [0.10.1] - 2026-05-05

### Docs

- Bump `hjkl-buffer` 0.3.4 → 0.3.5. Inlines former `IMPLEMENTERS.md` invariants
  into rustdoc on the actual types and methods (`Position`, `Edit` + variants,
  `Fold`, `Viewport`, `Span`, `Buffer::set_cursor` / `clamp_position` /
  `ensure_cursor_visible`, `BufferView` render module). Now renders on docs.rs
  next to each symbol and shows up in IDE hover. No binary behavior change.

## [0.10.0] - 2026-05-05

### Added

- **Git pickers under `<leader>g`.** Lazygit-adjacent surface, all bound to the
  `<leader>g` chord:
  - `<leader>gs` — status picker. Modified / staged / untracked entries; preview
    shows the working-tree diff (body + `+`/`-`/space prefix).
  - `<leader>gl` — log picker. Hash dimmed yellow, conventional-commit prefix
    colored by type, lazygit-style author initials with deterministic per-author
    color, preserves chronological sort on empty query.
  - `<leader>gb` — branches picker. Locals bucketed before namespaced and
    remotes; checkout does a pre-flight conflict check (diff HEAD↔target ∩
    workdir status) and aborts with a path preview instead of letting libgit2
    return an opaque `class=Checkout (20); code=Conflict (-13)`.
  - `<leader>gB` — file history picker for the current buffer's path.
  - `<leader>gS` — stash picker. `Alt+P` pops, `Alt+D` drops, Enter applies.
  - `<leader>gt` — tags picker. Sorted by tagger time desc with alpha tiebreak;
    Enter checks out the tag's commit (detached HEAD).
  - `<leader>gr` — remotes picker. Lists configured remotes with branch counts;
    Enter fetches.
- **Auto-reload buffers from disk on focus regain.** `Event::FocusGained`
  triggers `checktime_all()`; non-dirty buffers whose mtime+len changed are
  reloaded silently. Dirty buffers and deleted-on-disk files are flagged with
  vim-style `[changed on disk]` / `[deleted]` suffixes in the status line.
  `:checktime` ex command available for manual sweep. `:write!` overrides the
  `E13: file has changed on disk` guard.
- **Colored git commit header in preview.** `commit` / `Author` / `Date` /
  subject lines styled distinctly; conventional-commit prefix in the subject
  picks up the same color used by the log picker.
- **Drop pristine default buffer when first real file opens.** Empty unnamed
  unmodified default slot is closed automatically once the first `:edit`
  succeeds, so the buffer list stays clean.
- Animated splash background now inherits the terminal background across themes
  (carry-over polish from 0.9.3).

### Changed

- **`hjkl-picker` 0.3 → 0.4.** Picks up the
  `PickerAction::Custom(Box<dyn Any + Send>)` refactor that drops app-specific
  variants (`OpenPath`, `ShowCommit`, `CheckoutBranch`, etc.) from the library,
  plus `handle_key` / `label_styles` / `preserve_source_order` source hooks.
  App-side `AppAction` enum now carries all hjkl-specific intents and is
  downcast in `dispatch_picker_action`.
- **Git status picker moved app-side.** `GitStatusSource` removed from
  `hjkl-picker` to keep the library free of `git2`. Now lives in
  `apps/hjkl/src/picker_git.rs` alongside the other git pickers.
- **`hjkl-bonsai` 0.4.0 → 0.4.1.** Adds `build/` to the crate's `.gitignore` so
  compiled grammar artifacts no longer pollute `git status`.
- Sub-dep patch bumps (no behavior change in this app, picked up via caret):
  `hjkl-buffer` 0.3.4, `hjkl-clipboard` 0.5.1, `hjkl-editor` 0.3.3,
  `hjkl-engine` 0.3.4, `hjkl-form` 0.3.3, `hjkl-ratatui` 0.3.3.

### Fixed

- Branch + log pickers preserve their source-defined sort (locals-first,
  chronological) on empty query instead of falling back to alphabetical.
- Git status preview rendered headers only — now includes the diff body and
  `+`/`-`/space prefix per hunk line.
- Picker fuzzy-match highlight positions are aligned to the visible label
  (post-prefix/icon) rather than the raw entry text.
- `checktime_all()` now runs after `<leader>gb` branch checkout so reloaded
  buffers reflect the new tree without manual `:checktime`.

## [0.9.3] - 2026-05-04

### Added

- Animated start screen on `hjkl` launched without a file argument: centered
  `HJKL` figlet with a cursor walking the letterforms in vim-motion order (h → j
  → k → l), trailing fading `h`/`j`/`k`/`l` glyphs. Any non-Ctrl-C keypress
  dismisses; the dismissing key falls through to normal handling so `:` opens
  the command bar on the same press. Splash inherits the terminal background to
  match the editor body across themes.

### Changed

- Bump `hjkl-bonsai` dep from `"0.3"` to `"0.4"`. Picks up the breaking schema
  refactor where highlight queries are sourced from helix + nvim-treesitter (the
  curated upstreams) rather than each grammar repo's own `queries/` dir.
  Resolves silent partial-install failures for grammars whose upstream layout
  doesn't match the prior hardcoded `query_dir` (xml/dtd were affected at the
  pinned revs).

### Fixed

- Cron CI: `cargo install cargo-fuzz` no longer passes `--locked`. The
  cargo-fuzz published `Cargo.lock` pinned an old `rustix` that uses internal
  `rustc_layout_scalar_valid_range_*` attributes nightly now rejects, breaking
  the fuzz harness install.

## [0.9.2] - 2026-05-03

### Fixed

- `Cargo.lock` updated to match the bumped `hjkl` package version. 0.9.1 shipped
  with the lockfile still pinned to `hjkl 0.9.0`, so
  `cargo build --release --locked --bin hjkl` (the release.yml command) failed
  with "cannot update the lock file because --locked was passed".

## [0.9.1] - 2026-05-03

### Fixed

- `Cargo.lock` regenerated cleanly from `cargo build` instead of partial
  `cargo generate-lockfile`. 0.9.0 release CI failed on
  `cargo zigbuild --locked` because the lockfile was out of sync with the bonsai
  0.3 + apps/hjkl pin bump. No source changes vs 0.9.0.

## [0.9.0] - 2026-05-03

### Changed

- **`hjkl-bonsai` 0.2 → 0.3.** Tree-sitter grammar storage subdir renamed
  `hjkl/grammars/` → `bonsai/grammars/`, and macOS/Windows now follow
  XDG-everywhere instead of `~/Library/Application Support` / `%APPDATA%`.
  Existing grammars under the old paths are not migrated — hjkl re-fetches and
  re-compiles them into `~/.local/share/bonsai/grammars/` on first use. Distro
  packagers shipping pre-built grammars must move from
  `/usr/share/hjkl/grammars/` to `/usr/share/bonsai/grammars/` (the AUR PKGBUILD
  here doesn't ship grammars, so no PKGBUILD change). See
  `crates/hjkl-bonsai/CHANGELOG.md` for full detail.

## [0.8.1] - 2026-05-03

### Added

- **Alpine `.apk` package** in `pkg/alpine/APKBUILD.in`, built in
  `.github/workflows/release.yml` inside an `alpine:latest` container off the
  `x86_64-unknown-linux-musl` release tarball and uploaded to the GitHub release
  alongside the `.deb` / `.rpm` / `.tar.gz` artifacts. Install with
  `apk add --allow-untrusted ./hjkl-*.apk`. Tracks
  [#18](https://github.com/kryptic-sh/hjkl/issues/18).

## [0.8.0] - 2026-05-03

### Added

- **New `hjkl-config` crate** in the workspace (also published as a standalone
  submodule at
  [kryptic-sh/hjkl-config](https://github.com/kryptic-sh/hjkl-config)): shared
  TOML config loader for hjkl-based apps. XDG path resolution, span-aware parse
  errors (line/col/snippet), opt-in `Validate` hook, plus
  `load_layered`/`load_layered_from` for bundled-defaults + user-overrides
  deep-merge. Reusable bounds-check helpers (`ensure_range`, `ensure_non_zero`,
  `ensure_one_of`, `ensure_non_empty_str`) returning `ValidationError` with
  field names baked in.
- **User config support in the `hjkl` editor.** Reads
  `$XDG_CONFIG_HOME/hjkl/config.toml` (or `--config <PATH>` to override).
  Defaults bundled into the binary via `include_str!()` from
  [`apps/hjkl/src/config.toml`](apps/hjkl/src/config.toml) — the source-tree
  file is the single source of truth for defaults; no default values live in
  Rust code. User file is deep-merged on top: only overridden fields need to
  appear there. Unknown keys are an error.
  - Wired settings: `editor.leader` (replaces hardcoded `Space`),
    `editor.tab_width` / `editor.expandtab` (fallback when no `.editorconfig`
    matches), `editor.huge_file_threshold` (replaces `HUGE_FILE_LINES = 50_000`
    const in syntax_glue), `theme.name` (currently only `"dark"` bundled;
    unknown names warn and fall back).
  - `Config::validate()` bounds-checks `tab_width ∈ 1..=16` and
    `huge_file_threshold > 0`. Surfaced via `hjkl: config validation: …` on
    startup; exits with code 2 on failure.
  - Slot 0 gets the user-config Options reapplied via `App::with_config` so
    overrides take effect on the first opened buffer (not just `:e`-opened new
    slots). Readonly state on existing slots is preserved across the swap.
  - 5 new validation tests, 2 new `--config` end-to-end pipeline tests, 2 new
    `with_config` smoke tests covering slot-0 and readonly-preservation.

### Changed

- **`hjkl` CLI migrated from hand-rolled parser to clap derive.** Behavior
  preserved: `-R` / `--readonly`, positional `[FILES]...`, vim-style `+N`,
  `+/PATTERN`, `+perf`, `+picker` all still work. `+`-prefixed tokens are
  pre-processed out of `argv` before clap sees it (clap doesn't natively parse
  `+` flags). `--help` / `--version` are now generated by clap and honor
  `CARGO_PKG_VERSION`.

### Added

- `hjkl --help` now renders an ASCII-art banner (figlet "ANSI Regular" font)
  plus the package version inline. Banner lives in `apps/hjkl/src/art.txt`,
  embedded via `include_str!`. Regenerate with
  `figlet -f "ANSI Regular" hjkl > apps/hjkl/src/art.txt`.
- CLI smoke tests: `--version` returns `CARGO_PKG_VERSION`, long-form help
  contains the embedded art block and the version, vim-token splitter separates
  `+N`/`+/foo` from clap-handled args, vim-token applier sets
  line/pattern/perf/picker correctly.
- Edge-case tests for vim-style tokens: bare `+` survives into the clap stream
  as a positional, `--` ends vim-token processing (`hjkl -- +42` opens a file
  literally named `+42`), repeated `+N` / `+/PAT` overwrite (last-write-wins),
  unknown `+cmd` produces a warning string, `+/` (empty pattern) currently sets
  `pattern = Some("")` (documented quirk), end-to-end `parse_argv` round-trips
  mixed flags + tokens + warnings.

### Changed (internal)

- `parse_args` extracted into pure
  `parse_argv(raw: Vec<String>) -> Result<(Args, Vec<String>)>` for testability;
  the env+stderr wrapper remains as `parse_args` for `main` to call.
  `apply_vim_tokens` now returns warnings instead of printing to stderr.

## [0.7.0] - 2026-05-03

Adopts `hjkl-clipboard` 0.5.0 — the `Backend` trait went public, with new
`BackendKind` / `Capabilities` introspection plus async variants and the
`MockBackend` / `SshAwareBackend` extensions. Umbrella consumes the new API in
two places.

### Added

- New `:clipboard` ex command — prints the active backend kind plus the active
  capability flags
  (`WRITE READ CLEAR AVAILABLE PRIMARY IMAGE RICH_TEXT URI_LIST ASYNC_WRITE …`)
  to the status line. Useful for diagnosing why a yank/paste failed silently
  (e.g. confirming the OSC 52 fallback is active over SSH).
- `TuiHost::clipboard()` accessor exposing the cached `Clipboard` so the
  ex-dispatch layer can introspect without round-tripping the engine.

### Changed

- **`TuiHost::read_clipboard` is capability-aware.** Returns `None` immediately
  when the active backend doesn't advertise `Capabilities::READ` (OSC 52 over
  SSH, mocks without `preset_get`, etc), avoiding a guaranteed `UnsupportedMime`
  round-trip through the Wayland/X11 thread.
- `TuiHost::write_clipboard` checks `Capabilities::WRITE` before attempting the
  set, so a misconfigured mock backend that advertises no write capability
  silently no-ops instead of recording garbage calls.
- `hjkl-clipboard` dep `0.4` → `0.5` (caret-minor — `0.4` does not accept
  `0.5.x`).

## [0.6.0] - 2026-05-03

Migrates the umbrella binary onto `hjkl-bonsai` 0.2.x's runtime grammar loader.
Grammars are no longer baked into the binary; they're cloned, compiled, and
installed on demand the first time the editor encounters a language. Distros
that pre-populate `/usr/share/hjkl/grammars/` skip the on-demand path entirely.

### Changed

- **Release binary shrinks 31 MB → 5.1 MB.** The 27 baked `tree-sitter-*`
  grammar crates that bonsai 0.1.x bundled are gone.
- New `apps/hjkl/src/lang.rs` `LanguageDirectory` facade wraps
  `bonsai::runtime::{GrammarRegistry, GrammarLoader}` and caches loaded
  `Arc<Grammar>` per-name. `App` owns one `Arc<LanguageDirectory>`;
  `SyntaxLayer` and the three `Highlighted*Source` pickers all share it so each
  language `dlopen`s once per process.
- `SyntaxWorker` IPC now ships `Arc<Grammar>` (Send+Sync via tree-sitter's
  `unsafe impl`s + `libloading::Library`'s thread-safety) in place of
  `&'static LanguageConfig`.
- First-ever edit of an unknown file extension now blocks for ~1–3 s on a
  `git clone` + `cc` compile. Subsequent edits of the same language hit the
  user-data install (`<user_data>/hjkl/grammars/`) instantly. System-shipped
  grammars skip the build entirely.
- On-disk layout reorganized (see hjkl-bonsai 0.2.0 changelog for full detail).
  Existing `~/.local/share/hjkl/grammars/sources/` and
  `~/.cache/hjkl/build-grammars/` from v0.5.0 are now orphan and safe to delete.

### Fixed

- Cross-platform user-directory resolution: Windows / macOS no longer bail with
  `$HOME not set` because the loader now uses the `dirs` crate.

### Internal

- Tests that need a real grammar (network clone + cc compile of
  tree-sitter-rust) are gated behind `#[ignore]` so the default `cargo test`
  lane stays offline. Run them with `cargo test -p hjkl -- --ignored`.

## [0.5.0] - 2026-05-03

### Added

- TOML-driven UI + syntax theme matching the `hjkl.kryptic.sh` palette. Themes
  load from baked `themes/{ui,syntax}-dark.toml` at startup;
  `:set background={dark,light}` swaps live.
- Migrated from the legacy `hjkl-tree-sitter` crate to the renamed `hjkl-bonsai`
  0.1.x (same baked-grammar API, just rebadged). No code changes for end users.

### Fixed

- `:wq` (and `:x`) refuse to exit when the save fails. `do_save` / `save_slot`
  now return `bool`; `E32` (no filename), `E45` (readonly), and IO errors no
  longer silently quit and lose unsaved content.

## [0.4.6] - 2026-05-03

### Fixed

- v0.4.5 release failed (all 7 builds): the submodule pointer for
  `hjkl-clipboard` was stale (commit `6170ad0`, pre-0.4.8) but the lockfile
  recorded `hjkl-clipboard 0.4.8`, so `cargo build --locked` in CI rejected the
  mismatch. Pointer advanced to the v0.4.8 tag.

## [0.4.5] - 2026-05-03

### Fixed

- Bumped `hjkl-clipboard` 0.4 → 0.4.8 to pull in the Wayland bind fix. Clipboard
  now works on sway/wlroots and Hyprland (`FIRST_CLIENT_ID = 4` matches
  libwayland-client; older value of 100 was rejected by those compositors with a
  cryptic `"invalid arguments for wl_registry#2.bind"`).

## [0.4.4] - 2026-05-03

### Fixed

- `aur-bin` release job: include `pkg/aur/PKGBUILD-bin.in` in the repo. The AUR
  `.gitignore` allowlist matched only the literal name `PKGBUILD`, silently
  filtering the template file out of the v0.4.3 tag. Job failed at the sed
  render step.

## [0.4.3] - 2026-05-03

### Added

- Auto-publish `hjkl-bin` to AUR on every release. Mirrors buffr's pattern:
  archlinux container fetches sha256 sidecars from the GitHub release tarballs,
  renders `pkg/aur/PKGBUILD-bin.in`, generates `.SRCINFO`, and pushes to
  `aur.archlinux.org/hjkl-bin.git` via the org-level `AUR_SSH_KEY` secret.
  Targets gnu x86_64 + aarch64.

## [0.4.2] - 2026-05-03

### Added

- 8 new tree-sitter languages bundled via `hjkl-tree-sitter` 0.4.0: Python,
  TypeScript, TSX, Go, YAML, Bash, C, HTML, CSS. Auto-detected by file
  extension; highlighting works out of the box for all 14 supported languages.

### Changed

- Bumped `hjkl-tree-sitter` 0.3 → 0.4. Binary size grows ~8–12 MB
  release-stripped from the additional grammar `.so` artifacts.

## [0.4.1] - 2026-05-03

### Changed

- Doc cleanup pass across all submodule READMEs: dropped "spec frozen", "Buffer
  trait sealed", "engine SPEC types" and other stale rhetoric inherited from the
  pre-0.1.0 era.
- `apps/hjkl/src/host.rs` doc comment now describes the clipboard as in-house
  cross-platform rather than "native per-platform".
- `CONTRIBUTING.md` Releases section rewritten — `release-plz` and lockstep
  workspace versioning have been gone for a while; documents the current manual
  BCTP-per-submodule flow.
- Submodule pointer + lockfile bumps: `hjkl-buffer` 0.3.3, `hjkl-engine` 0.3.3,
  `hjkl-clipboard` 0.4.7, `hjkl-tree-sitter` 0.3.2, `hjkl-form` 0.3.2,
  `hjkl-ratatui` 0.3.2, `hjkl-picker` 0.3.2 (all README-only patch releases).

## [0.4.0] - 2026-05-03

### Changed

- **Bumped `hjkl-clipboard` 0.3 → 0.4.** Our in-house clipboard crate replaced
  its `arboard` dependency with a hand-rolled implementation built for the
  kryptic-sh ecosystem. Wayland paste now works on KDE / wlroots / Hyprland
  sessions where the previous backend silently lost the selection. See the
  [`hjkl-clipboard`](https://crates.io/crates/hjkl-clipboard) changelog for the
  full breakdown.
- **`TuiHost` clipboard adapter migrated to the 0.4 API.** `Clipboard::new()` is
  now fallible — wrapped in `Option<Clipboard>` so probe failure degrades to a
  no-op rather than aborting startup. `set_text` / `get_text` replaced with
  typed `set` / `get` over `Selection::Clipboard` + `MimeType::Text`.

### Removed

- **`SPEC.md`** removed from the `hjkl-engine` repo. The trait surface is
  documented inline via rustdoc and published to docs.rs; the parallel
  hand-maintained spec was a churn liability. Umbrella + per-crate READMEs point
  at [docs.rs/hjkl-engine](https://docs.rs/hjkl-engine) instead.

### Internal

- Lockfile shrunk ~371 lines as the `arboard` transitive tree (`wayland-client`,
  `x11rb`, `image`, etc.) was dropped. Smaller `cargo install hjkl` build.
- Bumped `hjkl-engine`, `hjkl-buffer`, `hjkl-editor` 0.3.1 → 0.3.2 (doc-only:
  SPEC.md drop + reference cleanup; no API changes).

## [0.3.4] - 2026-04-30

### Added

- `.rpm` packages for Fedora, RHEL, openSUSE, and other RPM-based distributions
  on x86_64 and aarch64. Built via `cargo-generate-rpm` on the same linux-gnu
  pipeline as the existing `.deb` packages. Install with
  `dnf install ./hjkl-0.3.4-1.x86_64.rpm` (or `aarch64`).

## [0.3.3] - 2026-04-30

### Fixed

- Release CI publish-crates job: only publishes the umbrella `hjkl` app crate.
  Previous logic looped over the eight `hjkl-*` library crates at the workspace
  version, but those ship from their own `kryptic-sh/hjkl-*` repos at
  independent versions, so the loop failed for any version mismatch. The v0.3.2
  GitHub Release was cut with artifacts but never reached crates.io because of
  this; v0.3.3 ships the fix and the matching crates.io upload.

## [0.3.2] - 2026-04-30 [YANKED]

GitHub Release exists with the new artifacts listed below, but
`cargo install hjkl` was never bumped past 0.3.1 — the publish-crates job in
release.yml had a stale loop over now-independent submodule crates and failed
before reaching the umbrella `hjkl` crate. v0.3.3 ships the fix and the matching
crates.io upload.

### Added

- Release matrix expanded from 4 → 7 targets. New artifacts:
  `aarch64-unknown-linux-gnu` (Graviton, Pi, ARM laptops),
  `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` (statically
  linked, distro-agnostic, Alpine, Docker scratch images).
- `.deb` packages on both linux-gnu targets via `cargo-deb` —
  `hjkl_0.3.3-1_amd64.deb` and `hjkl_0.3.3-1_arm64.deb` attached to each GitHub
  Release alongside `.sha256` checksums.
- `[package.metadata.binstall]` so `cargo binstall hjkl` Just Works.
- Homebrew tap at
  [`kryptic-sh/homebrew-tap`](https://github.com/kryptic-sh/homebrew-tap):
  `brew install kryptic-sh/tap/hjkl` (manual bump per release).

## [0.3.1] - 2026-04-30

### Changed

- Extracted each `crates/hjkl-*` library into its own `kryptic-sh/hjkl-*`
  repository with full git history preserved. Each library now publishes
  independently to crates.io and is consumed via caret version requirements
  (`hjkl-buffer = "0.3"`, etc.) instead of workspace path deps.
- The `kryptic-sh/hjkl` repo now mounts those crates back under `crates/*` as
  git submodules pinned to `v0.3.1` tags, so a single workspace build still
  touches every layer for development.
- Bumped sibling deps to their `0.3.1` releases: `crossterm` 0.29, `ratatui`
  0.30, `criterion` 0.8, `toml` 1.1.

## [0.3.0] - 2026-04-28

### Added

- **Multi-buffer support** in `apps/hjkl`: open many files at once
  (`hjkl a.rs b.rs c.rs`); tab line at top of screen when more than one buffer
  is open; switch buffers with `:bn` / `:bp` / `:bd[!]` / `:bfirst` / `:blast` /
  `:b N` / `:b name` / `:ls` / `:buffers`; alt buffer via `Ctrl-^` or `:b#`;
  cycle with `Shift-H` / `Shift-L` and `gt` / `gT` / `]b` / `[b`; bulk save/quit
  with `:wa` / `:qa[!]` / `:wqa[!]`; helix-style `:q` closes the active slot
  when more than one buffer is open rather than exiting.
- **Fuzzy file picker** (`<Space><Space>` / `<Space>f` / `:picker` /
  `hjkl +picker`) with syntax-highlighted preview pane.
- **Buffer picker** (`<Space>b` / `:bpicker`) — switch open buffers via the same
  fuzzy UI.
- **Grep picker** (`<Space>/` / `:rg <pattern>`) — ripgrep-backed content search
  with grep and findstr fallbacks for platforms without ripgrep; preview pane
  scrolls to and highlights the match line.
- **Multi-file CLI** — `hjkl a.rs b.rs c.rs` opens all files as named slots.
- **Tab line** at the top of the screen listing all open buffers; rendered only
  when more than one buffer is open.
- **Tree-sitter syntax highlighting** in the buffer pane and picker preview
  (Rust, Markdown, JSON, TOML, SQL bundled via `hjkl-tree-sitter`).
- **Comment marker overlay** (`hjkl-tree-sitter::CommentMarkerPass`) — annotates
  `TODO` / `FIXME` / `FIX` / `NOTE` / `INFO` / `WARN` markers with distinct
  highlight styles; consecutive single-line comments inherit the marker across
  continuation lines.
- **Smart indent** — Enter, `o`, and `O` bump indent one level after `{`, `(`,
  or `[`; a leading close brace on the new line auto-dedents.
- **`.editorconfig` support** — `indent_style`, `indent_size`, `tab_width`, and
  `max_line_length` are applied automatically on file open.
- **`hjkl-picker` crate** (`crates/hjkl-picker`) — the entire picker subsystem
  is extracted into a standalone reusable crate with no direct dependency on
  `hjkl-engine`. Provides `Picker`, `PickerLogic` trait, `FileSource`,
  `RgSource`, `PreviewSpans`, and the fuzzy `score` function.
- **Shared braille spinner** (`hjkl-ratatui::spinner::frame`) — 10-frame braille
  animation at ~8 Hz via a monotonic epoch; used in picker loading indicators.
- **Shared register bank** across buffer slots — yank in one slot, paste in
  another; `dd` and other linewise ops write to the unnamed register so
  cross-buffer paste works correctly.
- **Info popup overlay** for `:reg`, `:marks`, `:jumps`, `:changes` — multi-line
  ex-command output renders as a centered floating popup.
- **Status line additions**: `REC@r` badge while recording a macro; pending
  count and pending operator block; search count `[n/m]`.
- **Cursor-line background** in the editor pane (subtle blue-grey); suppressed
  during `:` and `/` prompts.
- **`Gutter::line_offset`** field in `hjkl-buffer` — enables windowed preview
  snapshots to display original document line numbers in the gutter.
- **`Viewport::tab_width`** field — carries the active `tabstop` value through
  the render pipeline.
- **`:set softtabstop=N`** (`sts`) — Backspace deletes a soft tab as a unit; Tab
  fills to the next softtabstop boundary.

### Changed

- **Default tab settings** flipped to modern four-space soft tabs:
  `tabstop=4 shiftwidth=4 softtabstop=4 expandtab=on smartindent=on`. To revert
  to traditional vi defaults:
  `:set noexpandtab tabstop=8 shiftwidth=8 softtabstop=0`.
- **`:set tabstop=N`** is now threaded through `Viewport.tab_width` to the
  renderer and cursor position math end-to-end.
- **Picker prompt symbol** changed from `>` to `/` to match search semantics.
- **`PickerLogic` trait** gains `preview_top_row`, `preview_match_row`, and
  `preview_line_offset` so windowed sources can position, highlight, and
  label-offset the preview pane correctly.
- **Label spacing** across all pickers is uniform (2-cell prefix pad, no leader
  arrow).

### Fixed

- **Tab rendering** — tab characters expand to spaces aligned to tab stops;
  `cursor_screen_pos` accounts for tab visual width.
- **`dd` resets `sticky_col`** so subsequent `j` / `k` lands on the first
  non-blank column rather than the deleted line's column.
- **Paste linewise** reads from the unnamed register slot rather than a
  per-editor cache, fixing cross-buffer linewise paste.
- **Grep picker preview** is no longer empty (status-tag misuse) and now scrolls
  to the match line with correct file line numbers in the gutter.

## [0.1.1] - 2026-04-27

### Fixed

- `hjkl-editor` shell filter (`%!cmd`) now tolerates the child closing stdin
  before all input is consumed. Previously a `BrokenPipe` write error would
  short-circuit and mask the child's actual exit status (e.g. `%!exit 5`
  reported "cannot write to `exit 5`: Broken pipe" instead of "command exited
  5"). Now `BrokenPipe` falls through to `wait_with_output()` so the real exit
  status wins; other write errors still surface. Fixes a flaky CI failure on
  `shell_filter_failing_command_errors`.

### CI

- Replaced `release-plz.yml` with a tag-driven `release.yml` matching the
  org-wide canonical pattern. Runs fmt/clippy/test as a quality gate, then
  publishes the 4 hjkl crates to crates.io in dep order via an idempotent shell
  loop (curl-precheck + `cargo publish --locked`). Fires on
  `git push origin vX.Y.Z`.

## [0.1.0] - 2026-04-27

### Patch C-δ — Editor generic flip + SPEC freeze

The first stability-locked release. Folds the 0.0.20 — 0.0.42 churn (none
published to crates.io) into a single 0.1.0 cut: SPEC trait scaffolding, the
Buffer/Cursor/Query/BufferEdit/Search trait split, viewport relocation onto
`Host` (driven by GUI/TUI cross-platform requirement), motion + editor + vim
reach lifts, the `Editor` generic flip, removal of the pre-0.1.0 dyn-host shim,
and the SPEC freeze.

docs.rs surfaces the canonical API per upload.

#### Pre-1.0 trait-extraction arc (folded into 0.1.0)

23 unpublished patches (0.0.20 — 0.0.42) led up to the freeze. The arc themes:

- **Buffer surface discipline** (0.0.20 — 0.0.31): SPEC trait scaffolding;
  `Cursor` / `Query` / `BufferEdit` / `Search` component traits; `Sealed`
  super-trait gating downstream impls; <40-method cap on `Buffer`.
- **Viewport lift to Host** (0.0.32 — 0.0.34): `Buffer::viewport` field deleted;
  viewport storage + accessors moved to the `Host` trait so vim logic works in
  GUIs (pixel canvas, variable-width fonts) as well as TUIs (cell grid).
- **Editor field consolidation** (0.0.35 — 0.0.39): marks
  (`BTreeMap<char, Pos>`) and search state migrated from `Buffer` to `Editor`;
  `Buffer::dirty_gen` added for invalidation; `FoldProvider` + `FoldOp` lifted
  onto canonical engine surface.
- **Generic body** (0.0.40 — 0.0.42): 24 motion fns lifted to
  `B: Cursor + Query [+ &dyn FoldProvider]`; 70 + 151 internal `self.buffer.…` /
  `ed.buffer().…` reaches in editor.rs + vim.rs routed through the trait
  surface; viewport-math fns relocated as engine-side free fns;
  `apply_buffer_edit` seam centralized as the single concrete-on-`hjkl_buffer`
  funnel.

Buffer trait final shape: 14 methods. The full per-patch detail lives in git
(`git log v0.0.19..v0.1.0`); CHANGELOG entries for those patches are folded here
to keep crates.io history clean.

#### BREAKING — `Editor` generic over `B` + `H`

```rust
// 0.0.42 (and earlier):
pub struct Editor<'a> { /* concrete buffer + boxed dyn host */ }

// 0.1.0:
pub struct Editor<
    B: hjkl_engine::types::Buffer = hjkl_buffer::Buffer,
    H: hjkl_engine::types::Host = hjkl_engine::types::DefaultHost,
> { /* typed buffer + typed host */ }
```

The `'a` lifetime parameter (vestigial since the textarea field was ripped) is
removed. Defaults match the in-tree canonical impls so most call sites that
named `Editor` without a type parameter continue to type-check. Call sites that
wrote `Editor<'_>` / `Editor<'static>` need to drop the lifetime.

The vim FSM (`crate::vim` free functions, `Editor::mutate_edit`, the change-log
emitter, and the undo machinery) is bound to the canonical buffer:

```rust
impl<H: hjkl_engine::types::Host> Editor<hjkl_buffer::Buffer, H> {
    /* most methods */
}
```

The fully generic `<B: Buffer, H: Host>` impl exposes only universal accessors
(`buffer()` / `buffer_mut()` / `host()` / `host_mut()`). Custom buffer backends
compile against the trait but cannot run the vim FSM at 0.1.0 — see SPEC.md
§"Out of scope at 0.1.0" for the rationale (lifting `BufferEdit::apply_edit`
onto an associated type is post-0.1.0 work).

#### BREAKING — constructor

```rust
// 0.0.42:
Editor::new(KeybindingMode::Vim)
Editor::with_host(KeybindingMode::Vim, host)
Editor::with_options(buffer, host, options)

// 0.1.0:
Editor::new(buffer, host, options)
```

The legacy three-constructor surface is gone. There is no `#[deprecated]` shim —
every consumer migrates by passing the buffer, host, and `crate::types::Options`
explicitly. Call sites that don't need a custom host pass
`crate::types::DefaultHost::new()`; call sites that don't need custom options
pass `crate::types::Options::default()`.

The `Options::default()` defaults are SPEC-faithful (vim parity, `shiftwidth=8`
/ `tabstop=8`); the pre-0.1.0 `Settings::default()` defaulted to `shiftwidth=2`
(sqeel-derived). Tests and consumers that relied on `shiftwidth=2` need to
construct an `Options` with `shiftwidth: 2` explicitly.

#### BREAKING — `EngineHost` removed

The pre-0.1.0 object-safe shim trait (`EngineHost`) and its blanket
`impl<H: Host>` are gone. Hosts implement `Host` directly; the `Editor<B, H>`
generic carries the typed slot. `Editor::host()` now returns `&H` (was
`&dyn EngineHost`); `Editor::host_mut()` returns `&mut H`. Callers using the
host accessor need `crate::types::Host` in scope to call its methods through
trait dispatch.

#### BREAKING — `Buffer` trait sealed (preserved)

The `Buffer` super-trait is sealed via the private
`crate::types::sealed::Sealed` trait (already in place since 0.0.31). The
`Sealed` trait is now confirmed `pub(crate)`; downstream cannot
`impl Buffer for MyType` after this change. The canonical `hjkl_buffer::Buffer`
keeps its sealed-marker impl in `crate::buffer_impl`.

#### `apply_buffer_edit` decision

The seam between the engine and `hjkl_buffer::Buffer` for the mutate-edit
channel stays concrete on `&mut hjkl_buffer::Buffer` per the option (c) decision
documented in 0.0.42's CHANGELOG. SPEC.md §"Out of scope at 0.1.0" calls this
out explicitly: lifting onto `BufferEdit::Op` (an associated `type Edit;`) is
post-0.1.0 work — it forces every backend to design its own rich-edit enum and
rewrites the change-log machinery in terms of `B::Op`. The single seam at
`crate::buf_helpers::apply_buffer_edit` is the migration funnel for 0.2.0.

#### `EditorSnapshot::VERSION` frozen

`EditorSnapshot::VERSION` (currently `4`) is locked for the entire 0.1.x line.
Hosts persisting editor state between sessions can rely on the wire format being
stable; 0.2.0+ structural changes require `VERSION++` together with a
major-version bump.

#### SPEC.md frozen

`crates/hjkl-engine/SPEC.md` carries an explicit "0.1.0 (frozen 2026-04-27)"
header. The trait surface (14 `Buffer` methods across `Cursor` / `Query` /
`BufferEdit` / `Search`), `Host` trait surface, `FoldProvider` + `FoldOp`,
`Options`, `EditorSnapshot`, and the `Editor::new(buffer, host, options)`
constructor are all part of the frozen contract. Explicit non-goals: viewport
math on `Buffer`, `Editor`'s apply-edit funnel as part of the public trait
surface, and any host-flavoured fold-op enum (engine-canonical only).

#### `PUBLIC_API.md` regenerated

`crates/hjkl-engine/PUBLIC_API.md` is regenerated against 0.1.0 with the
simplified `cargo +nightly public-api` output. Top-level diff vs the 0.0.39
baseline:

- `Editor<'a>` → `Editor<B: Buffer, H: Host>` (every method now carries the new
  type params; the vim FSM impl is on `Editor<hjkl_buffer::Buffer, H>`).
- `Editor::new(KeybindingMode)` removed; `Editor::new(buffer, host, Options)`
  added.
- `Editor::with_host` / `Editor::with_options` removed.
- `EngineHost` trait + blanket impl removed.
- `motions::*` free functions now generic over `B: Cursor + Query [+ Search]`
  (vs concrete `&mut hjkl_buffer::Buffer` at 0.0.39 — they were lifted in 0.0.40
  but the PUBLIC_API.md hadn't been refreshed since 0.0.33 baseline).
- `BufferEdit::replace_all` added (already landed in 0.0.41; surfaced in this
  release's PUBLIC_API.md regeneration).

#### Tests

684 (unchanged from 0.0.42). One test (`bare_set_reports_current_values`)
updated to assert `shiftwidth=8` per the SPEC-faithful default; one golden
snapshot (`golden_ex__set_listing.snap`) regenerated for the same reason. The
vim-FSM-internal `editor_with` helper explicitly sets `shiftwidth: 2` so the
indent / outdent assertions don't churn.

#### Migration

Consumers updating from 0.0.x:

```rust
// before
let mut editor = Editor::new(KeybindingMode::Vim);

// after
use hjkl_engine::types::{DefaultHost, Options};
let mut editor = Editor::new(
    hjkl_buffer::Buffer::new(),
    DefaultHost::new(),
    Options::default(),
);
```

For consumers that wrote `&mut Editor<'_>` in fn signatures:

```rust
// before
fn step(ed: &mut Editor<'_>, input: Input) { ... }

// after
fn step<H: hjkl_engine::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    input: Input,
) { ... }
```

For consumers that constructed an `Editor` with a custom host:

```rust
// before
let mut editor = Editor::with_host(KeybindingMode::Vim, my_host);

// after
let mut editor = Editor::new(
    hjkl_buffer::Buffer::new(),
    my_host,
    hjkl_engine::types::Options::default(),
);
```

## [0.0.19] - 2026-04-26

### Added

- `smartcase` honoured by `/` and `?` searches. When `ignorecase` is on and the
  pattern contains an uppercase letter, the search compiles case-sensitive
  (matches vim's combined `ignorecase` + `smartcase` behaviour). Wired through
  `Settings::smartcase`, `Options::smartcase`, and `:set smartcase` / `:set scs`
  / `:set nosmartcase`.
- `:set` listing now includes `smartcase=on/off`. Golden snapshot updated.

## [0.0.18] - 2026-04-26

### Added

- `expandtab` honoured by the insert-mode Tab key. When `Settings::expandtab` is
  true, Tab inserts `tabstop` spaces; otherwise a literal `\t` (existing
  behaviour). Wired through `Options::expandtab`, `current_options` /
  `apply_options`, `:set expandtab` / `:set noexpandtab` / `:set et`.
- `:set` listing now includes `expandtab=on/off`. Golden snapshot updated.

## [0.0.17] - 2026-04-26

### Added

- `Options::textwidth` (u32, default 79) — engine-native bridge for the legacy
  `Settings::textwidth` driving `gq{motion}` reflow. Wired through
  `current_options` / `apply_options` and `set_by_name("tw"|"textwidth")`.

## [0.0.16] - 2026-04-26

### Added

- `Options::wrap: WrapMode` (engine-native equivalent of `hjkl_buffer::Wrap`).
  `Editor::current_options` / `apply_options` map between `WrapMode` and
  `hjkl_buffer::Wrap` at the boundary.
- `set_by_name` / `get_by_name` recognise vim's `wrap` and `linebreak` (`lbr`)
  toggles. Combined state collapses into the single `WrapMode` field:
  `wrap=off → None`, `wrap=on,lbr=off → Char`, `wrap=on,lbr=on → Word`.

## [0.0.15] - 2026-04-26

### Added

- IncSearch highlight emission. `Editor::highlights_for_line` now branches:
  active `/` or `?` prompt → `HighlightKind::IncSearch` for live-preview
  matches; committed pattern → `SearchMatch` (existing behaviour). Hosts can
  paint live-preview distinctly from committed-search.
- Insta golden snapshots for ex-command output
  (`crates/hjkl-editor/tests/golden_ex.rs`): `:registers`, `:marks`, bare
  `:set`. Catches user-visible text format churn.

## [0.0.14] - 2026-04-26

### Changed (potentially breaking)

**Trait sealing pass.** Every `#[doc(hidden)] pub` item exposed for cross-crate
ex.rs reach is now sealed behind a proper public method. Hosts that were poking
at `Editor`'s internal fields (and ignoring the `#[doc(hidden)]` warning) now go
through the methods.

Field visibility flipped from `pub` to `pub(crate)`:

- `Editor::vim`, `Editor::registers`, `Editor::settings`, `Editor::file_marks`,
  `Editor::syntax_fold_ranges`, `Editor::undo_stack`, `Editor::change_log`.

`VimState::last_edit_pos`, `jump_back`, `marks` flipped back to `pub(super)` (no
longer reachable from outside hjkl-engine). `vim::do_undo` / `vim::do_redo`
flipped from `pub` to `pub(crate)`; the crate-root re-export is gone.

### Added (replacing the sealed surface)

New Editor methods covering everything ex.rs (and any other host) previously
reached via raw fields:

- `Editor::syntax_fold_ranges() -> &[(usize, usize)]`
- `Editor::file_marks()` — iterator over uppercase marks
- `Editor::buffer_mark(c) -> Option<(usize, usize)>`
- `Editor::buffer_marks()` — iterator over lowercase marks
- `Editor::last_jump_back() -> Option<(usize, usize)>`
- `Editor::last_edit_pos() -> Option<(usize, usize)>`
- `Editor::pop_last_undo() -> bool`
- `Editor::undo()` / `Editor::redo()`

Previously `#[doc(hidden)]` methods on `Editor` are now plain `pub`:

- `jump_cursor`, `mutate_edit`, `push_undo`, `restore`, `settings_mut`.

Fresh rustdoc covers every promoted method.

### Migration

Code that read fields directly should switch to method calls. For write-side
mutation (`undo_stack.pop()` etc.), `pop_last_undo()` is the supported
replacement.

## [0.0.13] - 2026-04-26

### Added

- `Editor::feed_input(PlannedInput) -> bool` — SPEC Input dispatch. Bridges
  hosts that don't carry crossterm (buffr CEF, future GUI shells) into the
  engine. Char + Key variants route to handle_key; Mouse / Paste / FocusGained /
  FocusLost / Resize fall through.

## [0.0.12] - 2026-04-26

### Added

- `Editor::intern_engine_style(types::Style) -> u32` — SPEC-typed style
  interning. Same opaque ids as the ratatui-flavoured `intern_style`; both share
  the underlying table.
- `Editor::engine_style_at(id) -> Option<types::Style>` — looks up an interned
  style by id, returns it as a SPEC type. Hosts that don't depend on ratatui
  (buffr, future GUI shells) reach this surface for syntax-span installation.

## [0.0.11] - 2026-04-26

### Added

- `Editor::take_changes() -> Vec<EditOp>` — pull-model SPEC change drain. Editor
  accumulates EditOp records on every mutate_edit; take_changes drains the
  queue. Best-effort mapping for compound buffer edits (JoinLines, InsertBlock,
  etc.) emits a placeholder covering the touched range.
- `Editor::current_options() -> Options` and `Editor::apply_options(&Options)`
  bridge between SPEC Options and legacy Settings. Lets hosts read/write engine
  config through the SPEC API.

## [0.0.10] - 2026-04-26

### Added

- `hjkl-engine::types::OptionValue { Bool, Int, String }` — typed value carrier
  for the `:set` parser.
- `Options::set_by_name(name, OptionValue) -> Result<(), EngineError>` and
  `Options::get_by_name(name) -> Option<OptionValue>`. Vim-style short aliases
  supported (`ts`, `sw`, `et`, `isk`, `ic`, `scs`, `hls`, `is`, `ws`, `ai`,
  `tm`, `ul`, `ro`).

## [0.0.9] - 2026-04-26

### Changed (breaking the 0.0.8 snapshot wire format)

- `EditorSnapshot::VERSION` bumped to `3`. Adds a
  `file_marks: HashMap<char, (u32, u32)>` field carrying the uppercase / "file"
  marks (`'A`–`'Z`). Survives `set_content`, so hosts persisting between tab
  swaps round-trip mark state. 0.0.8 snapshots fail `restore_snapshot` with
  `EngineError::SnapshotVersion`.

## [0.0.8] - 2026-04-26

### Changed (breaking the 0.0.7 snapshot wire format)

- `EditorSnapshot::VERSION` bumped to `2`. The struct gains a
  `registers: Registers` field carrying vim's `""`, `"0`, `"1`–`"9`, `"a`–`"z`,
  and `"+`/`"*` slots. 0.0.7 snapshots fail `restore_snapshot` with
  `EngineError::SnapshotVersion`.
- `Slot` and `Registers` derive `Serialize` / `Deserialize` behind the `serde`
  feature.

## [0.0.7] - 2026-04-26

### Added

- `hjkl-engine::types::RenderFrame` — borrow-style render frame the host
  consumes once per redraw. Coarse today: mode + cursor + cursor_shape +
  viewport_top + line_count.
- `Editor::render_frame()` builder.
- `Editor::highlights_for_line(u32)` — SPEC `Highlight` emission with
  `HighlightKind::SearchMatch` for search hits.
- `Editor::selection_highlight()` — bridges the active visual selection to a
  SPEC `Highlight` with `HighlightKind::Selection`. None outside visual modes;
  visual-line / visual-block collapse to their bounding char range.

### Changed

- `CursorShape` now derives `Hash` so `RenderFrame` can derive it.

## [0.0.6] - 2026-04-26

### Added

- `hjkl-engine::types::EditorSnapshot` — coarse serde-friendly snapshot of
  editor state for host persistence. Carries `version`, `mode`, `cursor`,
  `lines`, `viewport_top`. Bumps the snapshot `EditorSnapshot::VERSION` constant
  to track wire-format compat.
- `hjkl-engine::types::SnapshotMode` — status-line mode summary embedded in the
  snapshot.
- `Editor::take_snapshot()` — produces an `EditorSnapshot` at the current state.
- `Editor::restore_snapshot(snap)` — restores from a snapshot; returns
  `EngineError::SnapshotVersion` on wire-format mismatch.

## [0.0.5] - 2026-04-26

### Changed

- **`ex.rs` relocated from `hjkl-engine` to `hjkl-editor`.** Ex commands now
  live in the crate they belong to. Consumers reach `ex` via
  `hjkl_editor::runtime::ex` (unchanged surface — the facade was already routing
  there).
- `hjkl-editor` gains `regex` as a direct dep (ex uses it for `:s/pat/.../`) and
  `crossterm` as a dev-dep.
- `mark_dirty_after_ex` is now a free function. Ex callsites that previously
  wrote `editor.mark_dirty_after_ex()` now write `mark_dirty_after_ex(editor)`.

### Added (engine internal — sealed at 0.1.0)

Several `pub(super)` / `pub(crate)` items on `Editor` and `VimState` gained
`#[doc(hidden)] pub` visibility so ex commands can reach them across the crate
boundary:

- `Editor`: `vim`, `undo_stack`, `registers`, `settings`, `file_marks`,
  `syntax_fold_ranges` fields; `settings_mut`, `mutate_edit`, `push_undo`,
  `restore`, `jump_cursor` methods.
- `VimState`: `last_edit_pos`, `jump_back`, `marks` fields.
- `vim::do_undo`, `vim::do_redo` re-exported at the crate root.

These are explicit churn-phase exposures and will be sealed under the 0.1.0
trait extraction. Do not rely on them.

### Migrated tests

5 vim+ex integration tests (`gqq` reflow, `gq` motion, paragraph break
preservation, `gqq` undo, `:marks` listing) moved from
`crates/hjkl-engine/src/vim.rs` to
`crates/hjkl-editor/tests/vim_ex_integration.rs`. cargo dev-dep cycles between
hjkl-engine and hjkl-editor produce duplicate type IDs, so they must run from
the editor side.

## [0.0.4] - 2026-04-26

### Changed

- Workspace `homepage` set to <https://hjkl.kryptic.sh>.
- Per-crate READMEs now carry CI / crates.io version / docs.rs / License /
  Website badges and a Website + Source line.

## [0.0.3] - 2026-04-26

### Added

- `hjkl-engine::Editor::take_content_change()` — pull-model coarse change
  observation. Returns `Some(Arc<String>)` if content changed since the last
  call, `None` otherwise. Drains the dirty flag. Bridges the gap until SPEC's
  `take_changes() -> Vec<EditOp>` ships with full edit-path instrumentation.
- `hjkl-engine::types::Viewport` (re-exported as `PlannedViewport` at the crate
  root to disambiguate from `hjkl_buffer::Viewport`).
- `hjkl-engine::types::BufferId` opaque newtype.
- 513-case proptest harness for the FSM (`tests/proptest_fsm.rs`): random
  keystroke sequences never panic, and three Escapes always return to Normal
  mode.

## [0.0.2] - 2026-04-26

### Added

- `hjkl-engine::types` extended with the planned 0.1.0 trait surface: `Options`,
  `EngineError`, `Modifiers`, `SpecialKey`, `MouseEvent`, `MouseKind`, `Input`,
  `Host` trait. All additive — coexists with the legacy runtime types in
  `hjkl-engine::editor`.
- `hjkl-editor`: real facade crate (was placeholder). Exposes three modules:
  `buffer`, `runtime`, `spec`. Consumers depend on hjkl-editor alone instead of
  all three downstream crates.
- `hjkl-buffer/IMPLEMENTERS.md`: caller invariants documentation.
- `hjkl-buffer` criterion benches under the `budgets` harness:
  `insert_char_1MB_buffer`, `search_next_10k_lines`, `cold_load_10MB`.
- `hjkl-buffer` proptest roundtrip property suite for `apply_edit` (768 random
  scenarios per test run).

### Changed

- `hjkl-engine::types::Edit` re-exported from the crate root as `EditOp` to
  disambiguate from `hjkl_buffer::Edit`.

## [0.0.1] - 2026-04-26

### Added

- `hjkl-buffer`: full sqeel-buffer port with cursor, edits, motions, folds,
  viewport, search. ratatui Widget impl behind optional `ratatui` feature.
  Default features off — buffer is UI-agnostic.
- `hjkl-engine`: full sqeel-vim port with vim FSM, ex commands, registers,
  dot-repeat, marks. ratatui + crossterm currently mandatory; phase 5 trait
  extraction will move them behind features.
- `hjkl-engine::types` module: SPEC core types (`Pos`, `Selection`,
  `SelectionKind`, `SelectionSet`, `Edit`, `Mode`, `CursorShape`, `Style`,
  `Color`, `Attrs`, `Highlight`, `HighlightKind`). Additive alongside the legacy
  public API; trait extraction wires the FSM and Editor onto these
  progressively.

### Changed

- `hjkl-editor` and `hjkl-ratatui`: still placeholder; ship 0.0.1 to keep
  lockstep workspace version.

## [0.0.0] - 2026-04-26

### Added

- Initial placeholder release. Reserves `hjkl-engine`, `hjkl-buffer`,
  `hjkl-editor`, and `hjkl-ratatui` names on crates.io. No public API.
- `MIGRATION.md` — extraction plan and design rationale.

[Unreleased]: https://github.com/kryptic-sh/hjkl/compare/v0.11.5...HEAD
[0.11.5]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.11.5
[0.11.4]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.11.4
[0.11.3]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.11.3
[0.11.2]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.11.2
[0.11.1]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.11.1
[0.11.0]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.11.0
[0.10.1]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.10.1
[0.10.0]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.10.0
[0.9.3]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.9.3
[0.9.2]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.9.2
[0.9.1]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.9.1
[0.9.0]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.9.0
[0.8.1]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.8.1
[0.8.0]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.8.0
[0.7.0]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.7.0
[0.6.0]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.6.0
[0.5.0]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.5.0
[0.4.6]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.4.6
[0.4.5]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.4.5
[0.4.4]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.4.4
[0.4.3]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.4.3
[0.4.2]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.4.2
[0.4.1]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.4.1
[0.4.0]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.4.0
[0.3.4]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.3.4
[0.3.3]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.3.3
[0.3.2]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.3.2
[0.3.1]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.3.1
[0.3.0]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.3.0
[0.1.1]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.1.1
[0.1.0]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.1.0
[0.0.19]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.19
[0.0.18]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.18
[0.0.17]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.17
[0.0.16]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.16
[0.0.15]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.15
[0.0.14]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.14
[0.0.13]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.13
[0.0.12]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.12
[0.0.11]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.11
[0.0.10]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.10
[0.0.9]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.9
[0.0.8]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.8
[0.0.7]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.7
[0.0.6]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.6
[0.0.5]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.5
[0.0.4]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.4
[0.0.3]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.3
[0.0.2]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.2
[0.0.1]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.1
[0.0.0]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.0
