# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). This
project adheres to [Semantic Versioning](https://semver.org/) once it reaches
0.1.0; the 0.0.x series is a churn phase where breaking changes may land on
patch bumps.

## [Unreleased]

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

[Unreleased]: https://github.com/kryptic-sh/hjkl/compare/v0.0.0...HEAD
[0.0.0]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.0
