# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). This
project adheres to [Semantic Versioning](https://semver.org/) once it reaches
0.1.0; the 0.0.x series is a churn phase where breaking changes may land on
patch bumps.

## [Unreleased]

## [0.0.28] - 2026-04-26

### Changed (Patch A — `sticky_col` + `iskeyword` hoist to `Editor`)

- **`sticky_col` (vim's `curswant`) is now stored on `Editor`.** The single
  source of truth for the desired vertical-motion column moves out of both
  `hjkl_buffer::Buffer` and the engine-internal `VimState`. New accessors:
  - `Editor::sticky_col() -> Option<usize>`
  - `Editor::set_sticky_col(Option<usize>)`
  Buffer motion methods that need the sticky value
  (`Buffer::move_up` / `move_down` / `move_screen_up` / `move_screen_down`)
  now take a `&mut Option<usize>` parameter so the caller owns the storage.
- **`iskeyword` is now stored only on `Editor::settings.iskeyword`.** Buffer
  no longer mirrors it. `Editor::set_iskeyword(...)` keeps working
  (source-compatible with 0.0.27) but no longer writes back into the buffer.
  Buffer word motions (`Buffer::move_word_fwd` / `move_word_back` /
  `move_word_end` / `move_word_end_back`) now take `iskeyword: &str` as a
  parameter so the host can change it without re-publishing onto the buffer.
- This unblocks Patch C (`Editor<B: Buffer, H: Host>` generic-ification at
  0.1.0): the audit identified `sticky_col` and `iskeyword` as vim-FSM
  concerns that don't belong on the SPEC `Buffer` trait surface. They had
  to come off `Buffer` before the FSM-internal motion helpers can be
  relocated into the engine as free functions over `B: Cursor + Query`.

### Removed (breaking — `hjkl_buffer::Buffer` public API)

- `Buffer::sticky_col()` — read `Editor::sticky_col()` instead.
- `Buffer::set_sticky_col(...)` — call `Editor::set_sticky_col(...)`
  instead.
- `Buffer::iskeyword()` — read `Editor::settings.iskeyword` instead.
- `Buffer::set_iskeyword(...)` — call `Editor::set_iskeyword(...)` (which
  now only mutates `Editor::settings.iskeyword`) instead.
- The `pub fn refresh_sticky_col_from_cursor` helper on `Buffer` is gone;
  horizontal motions no longer touch a buffer-side sticky field. The
  engine's existing `apply_sticky_col` already manages this from the
  Editor side.
- `Buffer::move_up`, `move_down`, `move_screen_up`, `move_screen_down` —
  signature changed to take `sticky_col: &mut Option<usize>`. Callers
  mirroring the engine pattern thread `&mut editor.sticky_col` through.
- `Buffer::move_word_fwd`, `move_word_back`, `move_word_end`,
  `move_word_end_back` — signature changed to take `iskeyword: &str` as
  the third / fourth positional argument.

### Migration (downstream consumers)

The buffer's `sticky_col` / `iskeyword` storage was an implementation
detail mirrored from `Editor` since 0.0.23. **No known consumer reads or
writes these fields directly** — sqeel, buffr, and inbx use the editor-
level accessors. If a host did call `buffer.sticky_col()` /
`buffer.set_sticky_col(...)` / `buffer.iskeyword()` /
`buffer.set_iskeyword(...)` directly, swap to the matching `Editor`
methods listed above. The `:set iskeyword=...` ex command keeps working
end-to-end via `Editor::set_iskeyword`.

If a host called `Buffer::move_up` / `move_down` / `move_screen_up` /
`move_screen_down` / `move_word_*` directly (rather than through the
engine's motion grammar), thread the new `sticky_col` / `iskeyword`
parameters through.

### Roadmap

- **Patch A — this release (0.0.28)**: `sticky_col` + `iskeyword` off
  `Buffer`.
- **Patch B (0.0.29)**: `Host` wiring — clipboard, cursor-shape emit,
  fold provider, `host.now()`. Lifts the remaining engine ↔ host
  side-channels onto the SPEC `Host` trait surface.
- **Patch C (0.1.0)**: `Editor<'a, B: Buffer = ..., H: Host = ...>`
  flip, motion / fold / viewport-scroll helpers relocated into the
  engine as free functions over `B: Cursor + Query`, public surface
  freezes.

## [0.0.27] - 2026-04-26

### Added (canonical `Buffer` impl)

- **`impl Buffer for hjkl_buffer::Buffer`** lands in a new
  `hjkl-engine::buffer_impl` module, wiring all four sub-traits onto the in-tree
  rope-backed buffer. The seal (`crate::types::sealed::Sealed`) flips from
  `mod sealed` to `pub(crate) mod sealed` so the canonical impl can name the
  marker; downstream remains locked out (the module is still crate-private).
  - `Cursor`: `cursor`, `set_cursor`, `byte_offset`, `pos_at_byte` —
    Pos⇄Position conversion lives in the impl. `byte_offset` and `pos_at_byte`
    walk the rope's line table; the round-trip identity is covered by the new
    `cursor_byte_offset_and_inverse` test.
  - `Query`: `line_count`, `line`, `len_bytes`, `slice`. Single-line slices
    borrow (returning `Cow::Borrowed`); multi-line slices allocate a join.
    `Query::line` panics on out-of-bounds per SPEC, where
    `hjkl_buffer::Buffer::line` returns `Option`.
  - `BufferEdit`: `insert_at`, `delete_range`, `replace_range`. Routed through
    `Buffer::apply_edit` so the buffer's existing dirty-gen + fold + render
    cache invalidation paths fire as expected.
  - `Search`: `find_next` / `find_prev`. Caller-owned `regex::Regex` per SPEC;
    honours the buffer's `wrapscan` setting. Distinct from the buffer's own
    `search_forward` / `search_backward` (which mutate the cursor); the trait
    variants are pure observers returning the matched range.
- **Compile-time assertion** that the in-tree buffer satisfies the SPEC trait
  surface — `assert_buffer::<hjkl_buffer::Buffer>()` runs in
  `buffer_impl::tests::rope_buffer_implements_spec_buffer`. If the trait surface
  diverges from the impl, the test fails to compile.

### Deferred — `Editor<B: Buffer, H: Host>` generic-ification

The original 0.0.27 plan was to generic-ify the `Editor` struct over `B: Buffer`
in this same patch. **Audit of the engine→buffer call surface during
implementation found 46 distinct concrete `hjkl_buffer::Buffer` methods reached
from `editor.rs` + `vim.rs`** (motions: 24; folds: 8; viewport: 4;
search-cursor: 4; misc: 6). Only 13 of those map onto the SPEC trait surface —
the rest are **explicitly out of scope** per SPEC.md ("motions don't belong on
`Buffer` — they're computed over the buffer, not delegated to it").

Generic-ifying `Editor` therefore requires the **24-motion relocation** SPEC.md
describes — moving the motion / fold / viewport-scroll helpers out of
`hjkl-buffer` and into `hjkl-engine` as free functions over `B: Cursor + Query`.
That's a multi-thousand-LOC, multi-module refactor and lands as its own patch.
Not blocking the canonical impl; downstream callers can already write
`fn f<B: hjkl_engine::SpecBuffer>(…)` against the trait.

### Migration

- No public-API changes. Consumers pinning `=0.0.26` keep building unchanged.
  `hjkl_engine::SpecBuffer` (re-export of `crate::types::Buffer`) is the
  canonical trait-bound for new code; existing concrete `&hjkl_buffer::Buffer`
  callers can continue using the buffer's inherent methods.
- 0.0.x series continues — `Editor<B, H>` generic-ification requires hoisting
  `sticky_col` (curswant), `iskeyword`, fold-aware row iteration, and viewport
  state out of `Buffer` first (none are on the SPEC trait surface, none can be
  added without violating SPEC's <40-method cap). Next patches: A (0.0.28)
  curswant + iskeyword off `Buffer` into `Editor`; B (0.0.29) `Host` wiring
  (clipboard, cursor-shape emit, fold provider, `host.now()`); C (0.1.0)
  `Editor<'a, B: Buffer = …, H: Host = …>` flip + freeze.

## [0.0.26] - 2026-04-26

### Added (Phase 5 trait extraction — keystone for 0.1.0)

- **`ratatui` is now an optional dependency of `hjkl-engine`.** Default features
  include `ratatui` so existing consumers keep building unchanged.
  `cargo build -p hjkl-engine --no-default-features` is clean (combines with the
  `crossterm`-optional landing in 0.0.24 to make wasm / no_std hosts viable).
  Engine-internal helpers `engine_style_to_ratatui` / `ratatui_style_to_engine`,
  plus the public `Editor::intern_style`, `Editor::style_table`,
  `Editor::install_syntax_spans`, and the `Rect`-flavoured `Editor::mouse_click`
  / `mouse_extend_drag` / `cursor_screen_pos` all sit behind
  `#[cfg(feature = "ratatui")]`.
- **Ratatui-free Editor surface for non-terminal hosts.** New ratatui-free
  equivalents always available regardless of feature flags:
  - `Editor::install_engine_syntax_spans` — engine-native
    [`crate::types::Style`] flavour of `install_syntax_spans`.
  - `Editor::mouse_click_xy` / `mouse_extend_drag_xy` / `cursor_screen_pos_xywh`
    — same semantics as the `Rect` versions but take `(x, y[, width, height])`
    directly.

  When the `ratatui` feature is off the engine maintains a parallel
  `Vec<crate::types::Style>` style intern table; `intern_engine_style` /
  `engine_style_at` continue to round-trip ids without any ratatui dependency.

- **`hjkl-editor` gains `ratatui` and `crossterm` features.** Default-on for
  back-compat; both flow through to `hjkl-engine`. `hjkl-editor`'s `hjkl-buffer`
  dep no longer pins the `ratatui` feature unconditionally — it now flows
  through the editor's `ratatui` feature, so a downstream consumer disabling it
  truly drops ratatui from the dep graph.
- **`hjkl-ratatui` is now the canonical adapter crate.** Pulls `hjkl-engine`
  with the `ratatui` feature on, so adding `hjkl-ratatui` to a host
  automatically lights up the ratatui-flavoured Editor surface.
- **SPEC trait surface lands on `hjkl_engine::types`.** Per
  `crates/hjkl-engine/SPEC.md`:
  - `Cursor`: `cursor`, `set_cursor`, `byte_offset`, `pos_at_byte`.
  - `Query`: `line_count`, `line`, `len_bytes`, `slice` (returns
    `std::borrow::Cow<'_, str>` so contiguous backends avoid the allocation).
  - `BufferEdit`: `insert_at`, `delete_range`, `replace_range`. (Distinct trait
    name from the existing `Edit` value type to avoid a naming clash.)
  - `Search`: `find_next`, `find_prev` (caller-owned `regex::Regex` per SPEC
    "Open issues").
  - `Buffer`: super-trait of all four, sealed via private
    `mod sealed { pub trait Sealed {} }` so downstream cannot `impl Buffer` from
    outside the engine family pre-1.0.

  Re-exported from `hjkl_engine` (`SpecBuffer`, `Cursor`, `Query`,
  `SpecBufferEdit`, `Search`) and
  `hjkl_editor::spec::{Buffer, Cursor, Query, BufferEdit, Search}`. Trait
  declarations only — wiring the generic `Editor<B: Buffer, H: Host>` over the
  in-tree `hjkl_buffer::Buffer` is deferred to a follow-up patch (the impl needs
  to thread Pos⇄Position conversions through the FSM, which is large enough to
  warrant its own bump).

- Insert-mode mouse-click undo-break parity tests. Two new unit tests in
  `hjkl-engine::editor` lock in the 0.0.25 wiring: with `undo_break_on_motion`
  on (default), a click during an insert session splits the undo group so a
  single `u` only reverses the post-click run; with `:set noundobreak`, the
  entire pre/post-click insert collapses into one group.

### Migration

Consumers pinning `=0.0.25` continue to build unchanged when they upgrade. Wasm
/ no_std hosts can now drop both `crossterm` and `ratatui` via:

```toml
hjkl-editor = { version = "=0.0.26", default-features = false, features = ["serde"] }
```

…and reach the engine-native syntax-span / mouse / cursor APIs through the `_xy`
/ `install_engine_syntax_spans` / `intern_engine_style` methods listed above.

## [0.0.25] - 2026-04-26

### Added

- `impl From<crossterm::KeyEvent> for Input` (gated on the `crossterm` feature).
  Idiomatic conversion replaces the previously private `crossterm_to_input` free
  fn — the latter remains as a one-line delegating wrapper for the in-tree
  ratatui-coupled callers.
- Mouse-position clicks now break the active insert-mode undo group when
  `undo_break_on_motion` is on, completing the parity gap noted in 0.0.24.
  `Editor::mouse_click` calls the same `break_undo_group_in_insert` helper used
  by arrow-key motions.
- Options round-trip proptest now exercises every settings-backed field:
  `tabstop`, `shiftwidth`, `textwidth`, `expandtab`, `ignorecase`, `smartcase`,
  `wrapscan`, `autoindent`, `undo_break_on_motion`, `readonly`, `undo_levels`,
  `timeout_len`, `iskeyword`, `wrap`. Catches future bridge regressions.

## [0.0.24] - 2026-04-26

### Added

- **`undo_break_on_motion` real semantics.** Insert-mode arrow keys
  (`Left`/`Right`/`Up`/`Down`/`Home`/`End`) now break the active undo group when
  the toggle is on (vim default). With `:set noundobreak`, the entire insert run
  stays in one group. Mouse-position handling is intentionally deferred — wiring
  it requires routing mouse events through `vim::step` first.
- **`crossterm` is now an optional dependency of `hjkl-engine`.** Default
  features include `crossterm` so existing consumers keep working unchanged.
  `cargo build -p hjkl-engine --no-default-features` is clean.
  `Editor::handle_key(KeyEvent)` and the internal `crossterm_to_input` helper
  sit behind `#[cfg(feature = "crossterm")]`. `Editor::feed_input(PlannedInput)`
  was refactored to convert SPEC inputs directly to engine inputs (no longer
  routed through a synthetic crossterm `KeyEvent`) — usable from the
  no-crossterm surface.

### Changed

- `EditorSnapshot::VERSION` documentation now states the lock policy explicitly:
  0.0.x bumps freely, **0.1.0 freezes the wire format**, 0.2.0+ structural
  changes require a major bump. Same wording added to
  `crates/hjkl-engine/SPEC.md` under "Stability commitments → Snapshot wire
  format".

## [0.0.23] - 2026-04-26

### Added (potentially breaking)

- **`iskeyword` now drives buffer-level word motions.** `Buffer` carries the
  spec via new `Buffer::iskeyword` / `Buffer::set_iskeyword`. The module-level
  `is_word` predicate is now spec-aware; `char_kind` reads the spec from
  `&Buffer`. `w` / `b` / `e` / `ge` (and `W` / `B` / `E`) classify chars against
  the live spec — completes the partial wiring from 0.0.22 (which only honoured
  iskeyword for engine-side `*` / `#`).
- `hjkl-buffer` now exports `is_keyword_char(c, spec)` as the single-source
  parser; `hjkl-engine` re-uses it via re-export instead of carrying its own
  copy.
- New `Editor::set_iskeyword(spec)` syncs `Settings::iskeyword` and pushes the
  spec onto the buffer in one shot. `apply_options` and `:set iskeyword=...`
  route through it.

### Changed

- The default `Buffer::iskeyword` is `"@,48-57,_,192-255"` (vim parity).
  Previously hardcoded as `c.is_alphanumeric() || c == '_'`. The new default
  classifies the same set of ASCII chars but adds Unicode alphabetic coverage
  (vim's `@` token uses `is_alphabetic`); buffers with non-ASCII alphabetic
  content may see slightly different word boundaries.

## [0.0.22] - 2026-04-26

### Added

- `:set timeoutlen=N` / `:set tm=N` — multi-key sequence timeout. When the user
  pauses longer than the budget between keys, any pending prefix (`g`-prefix,
  operator-pending, register selector, count) is cleared before dispatching the
  new key. New `Settings::timeout_len: Duration` (default 1000ms). New
  `VimState::clear_pending_prefix()` helper. Uses `std::time::Instant::now()`
  directly; TODO comment flags swap to `Host::now()` once the trait extraction
  lands.
- `:set iskeyword=...` / `:set isk=...` — vim-flavoured word-character spec.
  Engine-side `*` / `#` word pickup now honours it via the new `is_keyword_char`
  parser (`@`, `_`, `N-M` ranges, bare codes, literal punctuation). Buffer-level
  `w` / `b` / `e` motions still use the hardcoded predicate; TODO in
  `hjkl-buffer::motion` flags the remaining plumbing for the 0.1.0 trait
  extraction.
- `:set undobreak` / `:set noundobreak` — toggle for breaking the undo group on
  insert-mode motions. Field wired through Settings + Options bridge; engine
  doesn't yet break the undo group on motions, so the toggle is a forward-compat
  no-op today.
- `:set` listing surfaces `timeoutlen`, `iskeyword`, `undobreak` columns. Golden
  snapshot updated.

## [0.0.21] - 2026-04-26

### Added

- `:set readonly` / `:set ro` honoured by the engine. `Editor::mutate_edit`
  short-circuits when `Settings::readonly` is true: no buffer change, no dirty
  flag, no undo entry, no change-log emission. Returns a self-inverse no-op so
  callers pushing the inverse onto an undo stack still get a structurally valid
  round trip.
- `:set autoindent` / `:set ai` honoured by the insert-mode Enter handler. When
  on (vim default), Enter copies the leading whitespace of the current line onto
  the new line. New `Settings::autoindent` field defaults to `true` (vim parity)
  — a behaviour change from prior 0.0.x where Enter inserted a bare newline. Set
  `:set noai` to restore the old behaviour.
- `:set undolevels=N` / `:set ul=N` honoured by `push_undo` and the redo path.
  Older entries pruned beyond the cap. New `Settings::undo_levels` (default
  1000); `0` is treated as unlimited. New `Editor::undo_stack_len` test
  accessor.
- `:set` listing surfaces `undolevels`, `autoindent`, `readonly` columns. Golden
  snapshot updated.

## [0.0.20] - 2026-04-26

### Added

- `wrapscan` honoured by `/` and `?` searches. When off, search stops at
  end-of-buffer (forward) or beginning-of-buffer (backward) instead of wrapping.
  Default on (vim parity). New `Buffer::set_search_wrap` /
  `Buffer::search_wraps` accessors on `hjkl-buffer`. Wired through
  `Settings::wrapscan`, `Options::wrapscan`, and `:set wrapscan` / `:set ws` /
  `:set nowrapscan`.
- `:set` listing includes `wrapscan=on/off`. Golden snapshot updated.

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

[Unreleased]: https://github.com/kryptic-sh/hjkl/compare/v0.0.0...HEAD
[0.0.0]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.0
