# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). This
project adheres to [Semantic Versioning](https://semver.org/) once it reaches
0.1.0; the 0.0.x series is a churn phase where breaking changes may land on
patch bumps.

## [Unreleased]

## [0.0.24] - 2026-04-26

### Added

- **`undo_break_on_motion` real semantics.** Insert-mode arrow keys
  (`Left`/`Right`/`Up`/`Down`/`Home`/`End`) now break the active undo
  group when the toggle is on (vim default). With `:set noundobreak`,
  the entire insert run stays in one group. Mouse-position handling is
  intentionally deferred — wiring it requires routing mouse events
  through `vim::step` first.
- **`crossterm` is now an optional dependency of `hjkl-engine`.**
  Default features include `crossterm` so existing consumers keep
  working unchanged. `cargo build -p hjkl-engine --no-default-features`
  is clean. `Editor::handle_key(KeyEvent)` and the internal
  `crossterm_to_input` helper sit behind `#[cfg(feature = "crossterm")]`.
  `Editor::feed_input(PlannedInput)` was refactored to convert SPEC
  inputs directly to engine inputs (no longer routed through a
  synthetic crossterm `KeyEvent`) — usable from the no-crossterm
  surface.

### Changed

- `EditorSnapshot::VERSION` documentation now states the lock policy
  explicitly: 0.0.x bumps freely, **0.1.0 freezes the wire format**,
  0.2.0+ structural changes require a major bump. Same wording added
  to `crates/hjkl-engine/SPEC.md` under "Stability commitments →
  Snapshot wire format".

## [0.0.23] - 2026-04-26

### Added (potentially breaking)

- **`iskeyword` now drives buffer-level word motions.** `Buffer` carries
  the spec via new `Buffer::iskeyword` / `Buffer::set_iskeyword`. The
  module-level `is_word` predicate is now spec-aware; `char_kind` reads
  the spec from `&Buffer`. `w` / `b` / `e` / `ge` (and `W` / `B` / `E`)
  classify chars against the live spec — completes the partial wiring
  from 0.0.22 (which only honoured iskeyword for engine-side `*` / `#`).
- `hjkl-buffer` now exports `is_keyword_char(c, spec)` as the
  single-source parser; `hjkl-engine` re-uses it via re-export instead
  of carrying its own copy.
- New `Editor::set_iskeyword(spec)` syncs `Settings::iskeyword` and
  pushes the spec onto the buffer in one shot. `apply_options` and
  `:set iskeyword=...` route through it.

### Changed

- The default `Buffer::iskeyword` is `"@,48-57,_,192-255"` (vim parity).
  Previously hardcoded as `c.is_alphanumeric() || c == '_'`. The new
  default classifies the same set of ASCII chars but adds Unicode
  alphabetic coverage (vim's `@` token uses `is_alphabetic`); buffers
  with non-ASCII alphabetic content may see slightly different word
  boundaries.

## [0.0.22] - 2026-04-26

### Added

- `:set timeoutlen=N` / `:set tm=N` — multi-key sequence timeout. When
  the user pauses longer than the budget between keys, any pending
  prefix (`g`-prefix, operator-pending, register selector, count) is
  cleared before dispatching the new key. New `Settings::timeout_len:
  Duration` (default 1000ms). New `VimState::clear_pending_prefix()`
  helper. Uses `std::time::Instant::now()` directly; TODO comment
  flags swap to `Host::now()` once the trait extraction lands.
- `:set iskeyword=...` / `:set isk=...` — vim-flavoured word-character
  spec. Engine-side `*` / `#` word pickup now honours it via the new
  `is_keyword_char` parser (`@`, `_`, `N-M` ranges, bare codes,
  literal punctuation). Buffer-level `w` / `b` / `e` motions still use
  the hardcoded predicate; TODO in `hjkl-buffer::motion` flags the
  remaining plumbing for the 0.1.0 trait extraction.
- `:set undobreak` / `:set noundobreak` — toggle for breaking the
  undo group on insert-mode motions. Field wired through Settings +
  Options bridge; engine doesn't yet break the undo group on motions,
  so the toggle is a forward-compat no-op today.
- `:set` listing surfaces `timeoutlen`, `iskeyword`, `undobreak`
  columns. Golden snapshot updated.

## [0.0.21] - 2026-04-26

### Added

- `:set readonly` / `:set ro` honoured by the engine. `Editor::mutate_edit`
  short-circuits when `Settings::readonly` is true: no buffer change, no
  dirty flag, no undo entry, no change-log emission. Returns a self-inverse
  no-op so callers pushing the inverse onto an undo stack still get a
  structurally valid round trip.
- `:set autoindent` / `:set ai` honoured by the insert-mode Enter handler.
  When on (vim default), Enter copies the leading whitespace of the current
  line onto the new line. New `Settings::autoindent` field defaults to
  `true` (vim parity) — a behaviour change from prior 0.0.x where Enter
  inserted a bare newline. Set `:set noai` to restore the old behaviour.
- `:set undolevels=N` / `:set ul=N` honoured by `push_undo` and the
  redo path. Older entries pruned beyond the cap. New `Settings::undo_levels`
  (default 1000); `0` is treated as unlimited. New `Editor::undo_stack_len`
  test accessor.
- `:set` listing surfaces `undolevels`, `autoindent`, `readonly` columns.
  Golden snapshot updated.

## [0.0.20] - 2026-04-26

### Added

- `wrapscan` honoured by `/` and `?` searches. When off, search stops
  at end-of-buffer (forward) or beginning-of-buffer (backward) instead
  of wrapping. Default on (vim parity). New `Buffer::set_search_wrap` /
  `Buffer::search_wraps` accessors on `hjkl-buffer`. Wired through
  `Settings::wrapscan`, `Options::wrapscan`, and `:set wrapscan` /
  `:set ws` / `:set nowrapscan`.
- `:set` listing includes `wrapscan=on/off`. Golden snapshot updated.

## [0.0.19] - 2026-04-26

### Added

- `smartcase` honoured by `/` and `?` searches. When `ignorecase` is on
  and the pattern contains an uppercase letter, the search compiles
  case-sensitive (matches vim's combined `ignorecase` + `smartcase`
  behaviour). Wired through `Settings::smartcase`, `Options::smartcase`,
  and `:set smartcase` / `:set scs` / `:set nosmartcase`.
- `:set` listing now includes `smartcase=on/off`. Golden snapshot
  updated.

## [0.0.18] - 2026-04-26

### Added

- `expandtab` honoured by the insert-mode Tab key. When `Settings::expandtab`
  is true, Tab inserts `tabstop` spaces; otherwise a literal `\t` (existing
  behaviour). Wired through `Options::expandtab`, `current_options` /
  `apply_options`, `:set expandtab` / `:set noexpandtab` / `:set et`.
- `:set` listing now includes `expandtab=on/off`. Golden snapshot updated.

## [0.0.17] - 2026-04-26

### Added

- `Options::textwidth` (u32, default 79) — engine-native bridge for the
  legacy `Settings::textwidth` driving `gq{motion}` reflow. Wired through
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

[Unreleased]: https://github.com/kryptic-sh/hjkl/compare/v0.0.0...HEAD
[0.0.0]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.0
