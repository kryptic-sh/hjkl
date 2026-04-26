# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). This
project adheres to [Semantic Versioning](https://semver.org/) once it reaches
0.1.0; the 0.0.x series is a churn phase where breaking changes may land on
patch bumps.

## [Unreleased]

## [0.0.41] - 2026-04-27

### Patch C-δ.6 — editor.rs / vim.rs reach replacement

Second of three patches before 0.1.0. Reroutes the `self.buffer.…` reaches in
`crates/hjkl-engine/src/editor.rs` and `crates/hjkl-engine/src/vim.rs` through
the SPEC trait surface (`Cursor` / `Query` / `BufferEdit` / `Search`) so that
`Editor`'s body could compile against any `B: Buffer` once the generic flip
lands. `Editor::buffer` is still typed as `hjkl_buffer::Buffer` for now — the
generic flip is the FINAL patch (0.1.0).

#### Reaches eliminated

- editor.rs: 68 → 6 (one is a comment line, four are intentionally-resistant
  reaches documented below).
- vim.rs (non-test): 2 → 0.

#### Trait additions

- `BufferEdit::replace_all(&mut self, text: &str)` — single-shot whole-buffer
  rebuild. Default impl forwards to
  `replace_range(Pos::ORIGIN..Pos { line: u32::MAX, col: u32::MAX }, text)` so
  non-canonical backends compile without the override; the canonical
  `hjkl_buffer::Buffer` impl forwards to its existing inherent `replace_all`
  fast path. Justified: `Editor::set_content` / `Editor::restore` /
  snapshot-replay paths express "replace whole buffer" cleanly only with this
  primitive — synthesising the same operation through `replace_range` requires
  the caller to know the buffer's end position, which the trait surface doesn't
  expose. Buffer trait surface is now 14 methods (cap is 40, defensive limit
  20).

#### New free helpers

`crates/hjkl-engine/src/editor.rs` now carries six private cast helpers
(`buf_cursor_rc` / `buf_cursor_row` / `buf_cursor_pos` / `buf_set_cursor_rc` /
`buf_row_count` / `buf_line` / `buf_lines_to_vec`) that wrap the trait calls and
translate at the `Pos { line: u32, col: u32 }` ⇄
`Position { row: usize, col: usize }` boundary. Same pattern as motion.rs's
`read_cursor` / `write_cursor` / `read_line` introduced in 0.0.40. Localized the
cast plumbing so the call-site diff stayed terse.

#### Resistant reaches (documented in source)

Four `self.buffer.…` reaches in `editor.rs` could not be lifted onto the trait
surface without breaking SPEC §"motions don't belong on `Buffer`" or expanding
the trait beyond the discipline cap:

- `Editor::mutate_edit` line 1298 — `self.buffer.apply_edit(edit) -> inverse`.
  The undo funnel consumes a `hjkl_buffer::Edit` value type and depends on the
  inherent method's "return the inverse edit" contract. Lifting requires either
  `BufferEdit::apply_edit(&mut self, edit: Self::Edit) -> Self::Edit` (an
  associated type expansion) or relocating the entire change-log + fold-
  invalidation pipeline. Deferred to 0.1.0.
- `Editor::ensure_cursor_in_scrolloff` line 2036 —
  `Buffer::ensure_cursor_visible`.
- `Editor::ensure_scrolloff_wrap` lines 2095, 2121 —
  `Buffer::cursor_screen_row`.
- `Editor::ensure_scrolloff_wrap` line 2142 — `Buffer::max_top_for_height`.

The last three are viewport-math helpers SPEC explicitly excludes from the
`Buffer` trait surface ("motions don't belong on `Buffer` — they're computed
over the buffer, not delegated to it"). 0.1.0 relocates them as engine-side free
fns over `B: Query [+ FoldProvider] + Viewport`; deferred to keep this patch's
diff bounded.

#### Tests

684 (was 682; +2 for `BufferEdit::replace_all` — one round-trip on the canonical
`hjkl_buffer::Buffer` impl, one mock-buffer compile-test that locks in the
default impl's `replace_range(ORIGIN..MAX, text)` routing).

#### Roadmap

- **0.1.0 — `Editor<B, H>` flip + freeze.** Replace
  `Editor::buffer: hjkl_buffer::Buffer` with `B: Buffer`, drop the `EngineHost`
  shim, unify the constructor surface, lift the four resistant reaches above
  through trait expansions / free-fn relocation.

## [0.0.40] - 2026-04-26

### Patch C-δ.5 — motion bound lift

First of three remaining patches before 0.1.0. Lifts every motion function in
`crates/hjkl-engine/src/motions.rs` from a concrete `&mut hjkl_buffer::Buffer`
parameter to a generic `B: Cursor + Query` trait bound. Fold-aware vertical /
screen-vertical motions take a separate `&dyn FoldProvider` argument so callers
thread their own fold storage. `Editor` itself stays concrete for now — the
generic flip is the FINAL patch (0.1.0).

#### Lifted surface

- 24 public motion fns now bound `B: Cursor + Query`:
  - Horizontal: `move_left`, `move_right_in_line`, `move_right_to_end`,
    `move_line_start`, `move_first_non_blank`, `move_line_end`,
    `move_last_non_blank`, `move_paragraph_prev`, `move_paragraph_next`.
  - Vertical (fold-aware): `move_up`, `move_down`, `move_screen_up`,
    `move_screen_down` — each takes `folds: &dyn FoldProvider`.
  - Anchor: `move_top`, `move_bottom`.
  - Word motions: `move_word_fwd`, `move_word_back`, `move_word_end`,
    `move_word_end_back`.
  - Find / match: `match_bracket`, `find_char_on_line`.
  - Viewport-relative: `move_viewport_top`, `move_viewport_middle`,
    `move_viewport_bottom`.
- 10 private helpers (`step_forward`, `step_back`, `next_word_start`,
  `prev_word_start`, `next_word_end`, `prev_word_end`, `char_at`,
  `char_kind_or_space`, `next_char_kind_in_row`, `end_of_buffer`) are now
  generic over `B: Query + ?Sized`.

#### Cast plumbing

Motion bodies still operate on
`hjkl_buffer::Position { row: usize, col: usize }` arithmetic internally. The
trait-boundary cast happens at three inlined helpers (`read_cursor`,
`write_cursor`, `read_line`) that translate the engine's grapheme-indexed
`Pos { line: u32, col: u32 }` to / from `Position`. Keeps the diff localized and
lets the grapheme story land later without re-touching every motion.

`Query::line` panics out-of-bounds whereas the pre-0.0.40
`Buffer::line(row) -> Option<&str>` returned `None`; `read_line` bound-checks
via `Query::line_count` first and returns `Option<&str>` so the existing
`unwrap_or("")` pattern in motion bodies stays intact.

#### `SnapshotFoldProvider`

Folds are stored on the buffer; the existing `BufferFoldProvider<'_>` borrows
the buffer immutably. Motions need `&mut B` for cursor writes, so the
immutable-and-mutable fold-vs- buffer borrow conflicts at the call site. New
`crate::buffer_impl::SnapshotFoldProvider` clones the fold list + `row_count`
from the buffer once, then implements `FoldProvider` over the snapshot —
decoupled from the buffer's lifetime, so the caller can re-borrow `&mut buf` for
the motion fn. Fold lists are tiny in practice; the clone is cheap.

#### Mock-buffer compile-test

A `MockBuf` test impl in `motions.rs` (non-canonical `Cursor + Query` struct)
drives `move_left`, `move_right_in_line`, `move_line_end`, `move_line_start`,
`move_word_fwd`, `move_top`, `move_bottom`. Locks in that the lift is on the
trait surface — not pinned to the canonical `hjkl_buffer::Buffer`.

#### Roadmap

- **0.0.41 — editor.rs reach replacement.** Lift the residual concrete
  `hjkl_buffer::Buffer` reaches in `editor.rs` (`Editor::set_content`'s
  `Buffer::from_str`, `Settings::wrap`'s `hjkl_buffer::Wrap`, viewport-math
  helpers `cursor_screen_row` / `max_top_for_height` / `screen_rows_between` /
  `ensure_cursor_visible` currently called as buffer-inherent methods) onto the
  trait surface
  - engine-private free functions.
- **0.1.0 — `Editor<B, H>` flip + freeze.** Replace
  `Editor::buffer: hjkl_buffer::Buffer` with `B: Buffer`, drop the `EngineHost`
  shim, unify the constructor surface, seal the trait contract per SPEC
  §"Stability commitments".

### Test count

682 (was 681 in 0.0.39 — +1 mock-buffer compile-test).

## [0.0.39] - 2026-04-26

### Patch C-δ.5 — `Query::dirty_gen` lands; 0.1.0 cut deferred

Fifth and final pre-1.0 keystone patch. The four prior patches (0.0.35 → 0.0.38)
extracted search state, marks, span pipeline, and fold mutation off the
`hjkl_buffer::Buffer` trait surface, leaving the planned 0.1.0 freeze contingent
on three things: (1) one trait expansion (`Query::dirty_gen`), (2) the `Editor`
generic flip (`Editor<'a, B: Buffer, H: Host>` per SPEC §"Editor surface"), and
(3) constructor unification + `EngineHost` shim removal.

This patch lands (1) cleanly. (2) and (3) are deferred to 0.1.0 proper after the
design doc's stop threshold tripped during the flip audit: `Editor` still hits
residual concrete `hjkl_buffer:: Buffer` reaches that the prior patches didn't
fully absorb (most notably `Editor::set_content`'s `Buffer::from_str`, the
`Settings::wrap` field's `hjkl_buffer::Wrap` type, and the ~143 `self.buffer.…`
internal call sites in `vim.rs` whose enclosing fns take `&mut Editor<'_>`). Per
the doc's stop guidance, ship `dirty_gen` now, defer the flip by one patch, keep
tests green.

#### Trait additions

- `Query::dirty_gen(&self) -> u64` — monotonic mutation counter. Read-only ops
  leave it untouched; insert / delete / replace / set-content bump it. Default
  impl returns `0` so non-canonical backends compile without a caching story;
  the canonical `hjkl_buffer::Buffer` impl forwards to the existing
  `Buffer::dirty_gen` inherent counter (in place since 0.0.0, used internally
  for render-cache invalidation).

#### Why `Query` and not a new sub-trait

Per the design doc's resolved question 8.1: a single-method helper trait
(`BufferStats` etc.) is overkill. Every backend trivially provides a counter;
living on `Query` keeps the
`Buffer: Cursor + Query + BufferEdit + Search + sealed::Sealed` super-trait
surface count at 14 methods total (well under the SPEC <40 cap).

#### Sealed surface

`mod sealed { pub(crate) trait Sealed {} }` is intact; `hjkl_buffer::Buffer` is
the canonical (and only) impl of `Sealed` in the family. External consumers
cannot impl `Buffer` pre-1.0; the seal carries through to the 0.1.0 freeze.

#### Snapshot wire format

`EditorSnapshot::VERSION` stays at `4` (last bumped in 0.0.36 for the unified
`marks` field). 0.1.0 will lock this number; 0.0.39 does not.

#### What's deferred to 0.1.0

- `Editor<'a>` →
  `Editor<'a, B: Buffer = hjkl_buffer::Buffer, H: Host = DefaultHost>` generic
  flip with default type params for back-compat at consumer call sites.
- Drop `EngineHost` shim (replaced by typed `H: Host`).
- Unify `Editor::new(KeybindingMode)` / `Editor::with_host` /
  `Editor::with_options` into a single SPEC-shaped
  `Editor::new(buf, host, options)`.
- Add a `BufferEdit::set_all(&mut self, &str)` (or equivalent) so
  `Editor::set_content` can drop its `Buffer::from_str` reach.
- Migrate `Settings::wrap: hjkl_buffer::Wrap` to the engine-native
  `crate::types::WrapMode`; collapse the two enums.
- Audit + classify the residual ~143 `self.buffer.…` reaches in `vim.rs` against
  the trait surface.
- Seal the `PUBLIC_API.md` baseline as the immutable freeze contract.

#### Public-API delta vs 0.0.38

Net: one new trait method (`Query::dirty_gen`) + one new impl line on
`hjkl_buffer::Buffer`. No removals, no signature changes. `PUBLIC_API.md`
regenerated against the 0.0.39 surface.

## [0.0.38] - 2026-04-26

### Patch C-δ.4 — fold mutation through `FoldProvider::apply(FoldOp)`

Fourth of the 5-patch sequence to 0.1.0 (per
`DESIGN_33_METHOD_CLASSIFICATION.md` step 4). The seven buffer-side fold
mutation methods (`add_fold` / `remove_fold_at` / `open_fold_at` /
`close_fold_at` / `toggle_fold_at` / `open_all_folds` / `close_all_folds` /
`clear_all_folds` / `invalidate_folds_in_range`) stay on `hjkl_buffer::Buffer`
for adapter use, but the **engine no longer calls them directly**. Every `z…`
keystroke, every `:fold*` Ex command, and the edit pipeline's
"edits-inside-a-fold open it" invalidation now route through a single canonical
surface: `hjkl_engine::FoldOp` carried via `Editor::apply_fold_op` and
dispatched through `FoldProvider::apply`.

`FoldOp` is engine-canonical (per the design doc's resolved question 8.2): hosts
don't invent their own fold-op enums. Hosts that want to observe the dispatch
(for separate fold trees, LSP folding ranges, or batching / dedup) drain
`Editor::take_fold_ops()` each step and fan out to their own `Host::Intent`
variant if desired. Hosts that just want the in-tree buffer fold storage updated
do nothing — the engine applies every op locally via `BufferFoldProviderMut`.

#### Engine additions

- `hjkl_engine::FoldOp` — canonical fold-mutation enum in `hjkl_engine::types`.
  Variants: `Add { start_row, end_row, closed }`, `RemoveAt(row)`,
  `OpenAt(row)`, `CloseAt(row)`, `ToggleAt(row)`, `OpenAll`, `CloseAll`,
  `ClearAll`, `Invalidate { start_row, end_row }`. `#[non_exhaustive]` so future
  `z…` keystrokes can extend it.
- `FoldProvider::apply(&mut self, op: FoldOp)` — new trait method on the
  existing `FoldProvider` surface (default impl: no-op, so read-only / stub
  providers don't need to override).
- `FoldProvider::invalidate_range(&mut self, start_row, end_row)` — default impl
  forwards to `apply(FoldOp::Invalidate { … })`.
- `hjkl_engine::BufferFoldProviderMut<'a>` — mutable fold-provider adapter
  wrapping `&'a mut hjkl_buffer::Buffer`. Implements the full `FoldProvider`
  trait; `apply` dispatches to the underlying buffer's fold methods.
- `Editor::apply_fold_op(FoldOp)` — single dispatch surface for every engine
  fold mutation. Queues the op for host observation AND applies it locally
  against the buffer.
- `Editor::take_fold_ops() -> Vec<FoldOp>` — host drain, mirrors the existing
  `take_lsp_intent` pattern.

#### Engine surface changes

- Vim FSM `z…` keystrokes (`zo`, `zc`, `za`, `zR`, `zM`, `zE`, `zd`,
  `zf{motion}`, visual-mode `zf`, operator-driven `zf` over a motion span) all
  route through `Editor::apply_fold_op`. Behaviour unchanged.
- `:foldsyntax` and `:foldindent` Ex commands route through
  `Editor::apply_fold_op` as well.
- `Editor::mutate_edit`'s "drop folds touched by this edit" call now goes
  through `apply_fold_op(FoldOp::Invalidate { … })` instead of the buffer's
  `invalidate_folds_in_range` directly. The edit pipeline's invalidation is
  therefore observable by hosts via `take_fold_ops`.

#### Buffer surface

- The seven fold mutation methods on `hjkl_buffer::Buffer` (`add_fold`,
  `remove_fold_at`, `open_fold_at`, `close_fold_at`, `toggle_fold_at`,
  `open_all_folds`, `close_all_folds`, `clear_all_folds`,
  `invalidate_folds_in_range`) **remain `pub`**. Per the design doc: "Those
  buffer-inherent methods don't disappear — they remain on `hjkl_buffer::Buffer`
  as `pub fn`s for the host adapter to call." `BufferFoldProviderMut::apply`
  calls into them; engine FSM no longer does. Audit confirmed sqeel / buffr /
  inbx don't call any of these directly today, so no consumer migration is
  required for this patch.
- The fold _read_ methods (`next_visible_row`, `prev_visible_row`,
  `is_row_hidden`, `fold_at_row`) also remain on `Buffer`;
  `BufferFoldProvider::new(&buffer)` is the canonical access path. These
  hard-delete at 0.1.0 alongside the `Editor<B, H>` generic flip.

#### Roadmap

The 0.0.x trait-extraction sequence is on step 4 of 5:

1. ✅ 0.0.34 (Patch C-δ.1) — viewport relocated to Host.
2. ✅ 0.0.35 — search FSM state moved to Editor.
3. ✅ 0.0.36 — named marks consolidated onto Editor.
4. ✅ 0.0.37 (Patch C-δ.3) — spans + search-pattern out of Buffer.
5. ✅ **0.0.38 (this release)** — fold mutation through
   `Host::emit_intent`-equivalent surface (`Editor::apply_fold_op` queues for
   host observation; `BufferFoldProviderMut::apply` handles the local dispatch).
6. ⏭ **0.1.0 (next cut)** — `Editor<'a, B: Buffer, H: Host>` generic flip +
   freeze. The `EngineHost` object-safe shim disappears; `BufferFoldProvider`'s
   read methods (`next_visible_row` etc.) relocate off `hjkl_buffer::Buffer`
   onto engine-side free functions over `B: Cursor + Query`.

## [0.0.37] - 2026-04-26

### Patch C-δ.4 — spans → Host syntax pipeline + search-pattern mirror removal

Third of the 5-patch sequence to 0.1.0 (per `DESIGN_33_METHOD_CLASSIFICATION.md`
step 3). Two pieces of state that 0.0.34/0.0.35 left as bridges between
`hjkl_buffer::Buffer` and the engine — the per-row syntax span cache and the
active `/` search pattern — move out of the buffer entirely. Spans now live on
`Editor::buffer_spans` (sourced from `Host::syntax_highlights` /
`install_syntax_spans`); the search regex lives on
`Editor::search_state.pattern` and reaches the renderer through a new
`BufferView::search_pattern` field.

#### Buffer surface — breaking

- `hjkl_buffer::Buffer::set_spans(&mut self, Vec<Vec<Span>>)` — **removed**.
  Spans are no longer cached on the buffer.
- `hjkl_buffer::Buffer::spans(&self) -> &[Vec<Span>]` — **removed**.
- `hjkl_buffer::Buffer::set_spans_for_test` — **removed** (was `pub(crate)`;
  trips no public API).
- `hjkl_buffer::Buffer::search_pattern(&self) -> Option<&Regex>` — **removed**
  (was `#[deprecated]` since 0.0.35).
- `hjkl_buffer::Buffer::set_search_pattern(&mut self, Option<Regex>)` —
  **removed** (was `#[deprecated]` since 0.0.35).
- `hjkl_buffer::Buffer::set_search_wrap(&mut self, bool)` — **removed** (was
  `#[deprecated]` since 0.0.35).
- `hjkl_buffer::Buffer::search_wraps(&self) -> bool` — **removed**.
- `hjkl_buffer::Buffer::search_forward(&mut self, bool) -> bool` — **removed**
  (was `#[deprecated]` since 0.0.35).
- `hjkl_buffer::Buffer::search_backward(&mut self, bool) -> bool` — **removed**
  (was `#[deprecated]` since 0.0.35).
- `hjkl_buffer::Buffer::search_matches(&mut self, usize) -> Vec<(usize, usize)>`
  — **removed** (was `#[deprecated]` since 0.0.35).
- The per-buffer `spans: Vec<Vec<Span>>` field and the `search: SearchState`
  field are deleted; the entire `hjkl_buffer/src/search.rs` module is gone.

#### `BufferView` surface — breaking

`hjkl_buffer::BufferView` gains two fields (struct literals must add them):

- `pub spans: &'a [Vec<Span>]` — per-row syntax spans the host computed for this
  frame. Rows beyond `spans.len()` get default styling. Sourced from
  `Editor::buffer_spans()`. Pass `&[]` for hosts without syntax integration.
- `pub search_pattern: Option<&'a regex::Regex>` — active `/` regex; renderer
  paints `search_bg` under matches. Sourced from
  `Editor::search_state().pattern.as_ref()`. Pass `None` to disable hlsearch.

#### Engine additions

- `hjkl_engine::Editor::buffer_spans() -> &[Vec<hjkl_buffer::Span>]` — replaces
  `editor.buffer().spans()` for hosts feeding spans into `BufferView`. Populated
  by `Editor::install_syntax_spans` / `Editor::install_ratatui_syntax_spans`.

#### Engine surface changes

- `Editor::set_search_pattern` no longer mirrors onto the buffer. Hosts that fed
  the regex into `BufferView` via `editor.buffer().search_pattern()` switch to
  `editor.search_state().pattern.as_ref()`.

#### `Search` trait impl change

- The in-tree `Search::find_next` / `Search::find_prev` impl on `RopeBuffer`
  always wraps. Wrap policy (`wrapscan`) lives on
  `Editor::search_state.wrap_around`; the engine's `search_forward` /
  `search_backward` free functions short-circuit before invoking the trait when
  wrap is disabled.

#### Migration table

| Before                                                         | After                                                                                                                 |
| -------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------- |
| `editor.buffer().spans()`                                      | `editor.buffer_spans()`                                                                                               |
| `editor.buffer().search_pattern()`                             | `editor.search_state().pattern.as_ref()`                                                                              |
| `buffer.set_spans(spans)`                                      | `editor.install_syntax_spans(spans)` / `install_ratatui_syntax_spans` (host-driven)                                   |
| `buffer.set_search_pattern(pat)`                               | `editor.set_search_pattern(pat)`                                                                                      |
| `buffer.search_forward(skip)` / `buffer.search_backward(skip)` | `editor.search_advance_forward(skip)` / `editor.search_advance_backward(skip)`                                        |
| `buffer.search_matches(row)`                                   | `hjkl_engine::search::search_matches(buf, &mut state, dirty_gen, row)`                                                |
| `buffer.set_search_wrap(wrap)`                                 | `editor.search_state_mut().wrap_around = wrap;` (or set `Settings::wrapscan`)                                         |
| `BufferView { …, conceals: c }`                                | `BufferView { …, conceals: c, spans: editor.buffer_spans(), search_pattern: editor.search_state().pattern.as_ref() }` |

#### Roadmap

- **0.0.38 (Patch C-δ.5)** — fold mutation through `Host::emit_intent(FoldOp)`.
  The `add_fold` / `open_fold_at` / `close_fold_at` / `toggle_fold_at` /
  `open_all_folds` / `close_all_folds` / `clear_all_folds` /
  `invalidate_folds_in_range` calls in vim FSM route through the host's intent
  enum; buffer keeps the inherent fold-storage methods so the in-tree
  `BufferFoldProvider` adapter still works, but the engine no longer reaches
  them directly.
- **0.1.0** — `Editor<B, H>` generic flip + trait freeze. `Query::dirty_gen`
  lands; `EngineHost` shim disappears; inherent-buffer-method callers migrate to
  the SPEC trait surface; the seal is set.

## [0.0.36] - 2026-04-26

### Patch C-δ.3 — named-marks consolidation onto `Editor`

Second of the 5-patch sequence to 0.1.0 (per
`DESIGN_33_METHOD_CLASSIFICATION.md` step 2). The three former storages for
vim's `m{a-zA-Z}` named marks collapse into a single
`Editor::marks: BTreeMap<char, (usize, usize)>`:

- `hjkl_buffer::Buffer::marks` — was an unused dead field (no reads, no writes,
  no callers) carried over from a pre-0.0.x prototype. **Hard-deleted**.
- `hjkl_engine::vim::VimState::marks` — held lowercase (`'a`–`'z`) marks. Field
  deleted; FSM routes through `Editor::set_mark` / `Editor::mark`.
- `hjkl_engine::Editor::file_marks` — held uppercase (`'A`–`'Z`) marks. Field
  renamed and merged into the unified map.

Mark-shift-on-edit semantic (marks pinned at a row drop when that row is
deleted; marks past the affected band shift by the row delta) is preserved — the
existing `Editor::shift_marks_after_edit` pipeline now operates on the single
map in one pass instead of two.

#### Engine additions

- `Editor::mark(char) -> Option<(usize, usize)>` — unified lookup for both
  lowercase and uppercase marks.
- `Editor::set_mark(char, (usize, usize))` — unified setter.
- `Editor::clear_mark(char)` — unified remove.
- `Editor::marks() -> impl Iterator<Item = (char, (usize, usize))>` —
  deterministic (BTreeMap-ordered) iteration over every set mark, replacing the
  prior `buffer_marks()` + `file_marks()` pair.

#### Engine call-site relocation

- `vim::handle_set_mark` (`m{a-zA-Z}` keystroke) writes via `Editor::set_mark`
  regardless of case.
- `vim::handle_goto_mark` (`'{a-zA-Z}` / `` `{a-zA-Z} ``) reads via
  `Editor::mark`.
- `hjkl-editor::ex` `:marks` listing iterates `Editor::marks()` directly; no
  separate uppercase merge pass.

#### Buffer surface — breaking

- `hjkl_buffer::Buffer::marks(&self) -> &BTreeMap<char, Position>` is
  **deleted**. The backing field carried no state (no buffer code ever wrote to
  it) and no consumer in this workspace called it; the design doc step 2
  prescribes hard delete.

#### Snapshot wire format

- `EditorSnapshot::file_marks: HashMap<char, (u32, u32)>` →
  `EditorSnapshot::marks: BTreeMap<char, (u32, u32)>`. Carries both lowercase
  and uppercase marks (lowercase round-trips for the first time as a side-effect
  of the consolidation).
- `EditorSnapshot::VERSION` `3` → `4`.

#### Editor surface — soft deprecations

The 0.0.35 deprecation pattern continues: existing accessors stay compiling but
warn so consumers can migrate at their own pace. Removal queued for 0.1.0.

- `Editor::buffer_mark` → use `Editor::mark`.
- `Editor::buffer_marks` → use `Editor::marks` (the unified iterator includes
  both lowercase and uppercase entries; old callers wanting lowercase-only can
  `.filter(|(c, _)| c.is_ascii_lowercase())`).
- `Editor::file_marks` stays compiling (filters to uppercase from the unified
  map) but is no longer the canonical accessor.

#### Migration

| Before                                 | After                                                 |
| -------------------------------------- | ----------------------------------------------------- | ------- | ------------------------ |
| `editor.buffer_mark('a')`              | `editor.mark('a')`                                    |
| `editor.buffer_marks()`                | `editor.marks()` (now includes uppercase too)         |
| `editor.file_marks()` (uppercase iter) | `editor.marks().filter(                               | (c, \_) | c.is_ascii_uppercase())` |
| `buffer.marks()` (engine-internal)     | `editor.marks()`                                      |
| `EditorSnapshot { file_marks, .. }`    | `EditorSnapshot { marks, .. }` (BTreeMap, both cases) |

Direct `hjkl_buffer::Buffer::marks` callers (none in this workspace or any known
consumer): no replacement — the field never held useful state. If a host was
reading it, the answer was always empty. Hosts wanting the FSM's marks read
`editor.marks()`.

#### Roadmap

- **0.0.37 (Patch C-δ.4)** — spans → Host syntax pipeline (delete
  `Buffer::set_spans` / `Buffer::spans`; delete the search-pattern bridge added
  in 0.0.35).
- **0.0.38 (Patch C-δ.5)** — fold mutation through `Host::emit_intent(FoldOp)`;
  `FoldProvider::apply(FoldOp)` and `FoldProvider::invalidate_range` land.
- **0.1.0 (Patch C-ε)** — `Editor<'a, B: Buffer, H: Host>` generic flip +
  freeze; delete the deprecated buffer search methods + the deprecated
  `Editor::buffer_mark` / `buffer_marks` accessors; `cargo public-api` baseline.

Workspace bumps `0.0.35` → `0.0.36`. Member crate pins (`=0.0.35` → `=0.0.36`)
and `Cargo.lock` updated. Test count stays at 674.

## [0.0.35] - 2026-04-26

### Patch C-δ.2 — search state on `Editor`

First of the 5-patch sequence to 0.1.0 (per `DESIGN_33_METHOD_CLASSIFICATION.md`
step 1). The search FSM state (pattern + per-row match cache + `wrapscan` flag)
moves out of `hjkl_buffer::Buffer` and onto `hjkl_engine::Editor`. Multi-window
hosts that share a buffer between panes no longer leak the "current search"
across windows that happen to share content.

#### Engine additions

- `hjkl_engine::search` (new public module) — `SearchState` struct (pattern,
  forward-direction flag, per-row matches cache, generations, `wrap_around`)
  plus three free functions over `B: Cursor + Query + Search`:

  ```rust
  pub fn search_forward<B>(buf: &mut B, state: &mut SearchState, skip_current: bool) -> bool;
  pub fn search_backward<B>(buf: &mut B, state: &mut SearchState, skip_current: bool) -> bool;
  pub fn search_matches<B>(buf: &B, state: &mut SearchState, dirty_gen: u64, row: usize) -> Vec<(usize, usize)>;
  ```

- `Editor::search_state(&self)` / `Editor::search_state_mut(&mut self)` — borrow
  the FSM state.
- `Editor::set_search_pattern(Option<Regex>)` — install a pattern; clears the
  cached matches and bridges the regex to the buffer's (deprecated)
  `set_search_pattern` so the in-tree `BufferView` hlsearch render path keeps
  painting until 0.0.37 lands the spans → Host pipeline.
- `Editor::search_advance_forward(skip_current)` /
  `Editor::search_advance_backward(skip_current)` — `n` / `N` drivers; thin
  wrappers over the free functions.

#### Engine call-site relocation

- `vim::push_search_pattern` writes to `Editor::search_state` (and bridges to
  the buffer for the renderer).
- `Motion::SearchNext`, `word_at_cursor_search`, the search-prompt Enter
  handler, and `enter_search` no longer call `buffer.search_*` /
  `buffer.set_search_pattern`. They route through `Editor::search_advance_*` /
  `Editor::set_search_pattern`.
- `Editor::highlights_for_line` reads the active pattern from
  `self.search_state` and pulls match runs through
  `crate::search::search_matches`.
- `hjkl_editor::ex` `:noh` clears via `Editor::set_search_pattern(None)`.

#### Buffer surface — soft deprecations (not deletes)

The design doc step 1 prescribes `#[deprecated]` (not removal) so the in-tree
`BufferView` hlsearch render path and direct `hjkl_buffer::Buffer` callers
(sqeel-tui's results-list highlight, buffr-modal) keep compiling. Removal is
queued for 0.1.0.

The following buffer-inherent methods are now
`#[deprecated(since = "0.0.35", note = "...")]`:

- `Buffer::set_search_pattern`
- `Buffer::search_pattern`
- `Buffer::set_search_wrap`
- `Buffer::search_forward`
- `Buffer::search_backward`
- `Buffer::search_matches`

The `Search::find_next` / `Search::find_prev` SPEC trait methods stay
non-deprecated — they're pure observers, caller-owned regex, SPEC-compliant. The
`search_wraps()` accessor stays alive (un- deprecated) because the in-tree
`Search` impl still reads it; the wrap policy migrates to a `Search` parameter
at 0.1.0.

#### Migration

Callers driving search through `Editor` (sqeel via `:` ex-mode + the engine FSM,
buffr-modal, inbx) keep working unchanged — the engine FSM mutated state lives
on `Editor` and the bridge keeps `buffer.search_pattern()` mirrored for the
renderer.

Direct `hjkl_buffer::Buffer::search_*` callers (rare) get a deprecation lint:

| Before                         | After                                                                            |
| ------------------------------ | -------------------------------------------------------------------------------- |
| `buffer.set_search_pattern(p)` | `editor.set_search_pattern(p)` (or `editor.search_state_mut().set_pattern(p)`)   |
| `buffer.search_pattern()`      | `editor.search_state().pattern.as_ref()`                                         |
| `buffer.search_forward(skip)`  | `editor.search_advance_forward(skip)` (or `hjkl_engine::search::search_forward`) |
| `buffer.search_backward(skip)` | `editor.search_advance_backward(skip)`                                           |
| `buffer.search_matches(row)`   | `hjkl_engine::search::search_matches(&buf, &mut state, dgen, row)`               |
| `buffer.set_search_wrap(b)`    | `editor.search_state_mut().wrap_around = b`                                      |

The `BufferView` hlsearch render path inside `hjkl-buffer` still uses
`Buffer::search_pattern()` internally — this is intentional for the bridge
period and disappears in 0.0.37 when the spans → Host pipeline lands.

#### Roadmap

- **0.0.36 (Patch C-δ.3)** — marks consolidation (`vim.marks` +
  `Editor::file_marks` + `Buffer::marks` → unified
  `Editor::marks: BTreeMap<char, Pos>`).
- **0.0.37 (Patch C-δ.4)** — spans → Host syntax pipeline (delete
  `Buffer::set_spans` / `Buffer::spans`; delete the search-pattern bridge added
  in this patch).
- **0.0.38 (Patch C-δ.5)** — fold mutation through `Host::emit_intent(FoldOp)`;
  `FoldProvider::apply(FoldOp)` and `FoldProvider::invalidate_range` land.
- **0.1.0 (Patch C-ε)** — `Editor<'a, B: Buffer, H: Host>` generic flip +
  freeze; delete the deprecated buffer search methods; `cargo public-api`
  baseline.

Workspace bumps `0.0.34` → `0.0.35`. Member crate pins (`=0.0.34` → `=0.0.35`)
and `Cargo.lock` updated. Test count went from 669 to 674 (+5 from the new
`hjkl_engine::search` module unit tests).

## [0.0.34] - 2026-04-26

### Patch C-δ.1 — viewport relocation onto `Host`

Architectural lock: **viewport lives on `Host`, not `Buffer`, not `Editor`.**
Vim logic must run in GUI hosts (variable-width fonts, pixel canvases, soft-wrap
by pixel) as well as TUI hosts; the runtime viewport state is expressed in
cells/rows/cols and is owned by the host. Buffer-side wrap math (rope-walking)
stays in `hjkl-buffer` and now consumes a `&Viewport` parameter.

This is a focused subset of the 0.1.0 keystone (per the prior C-δ
stop-and-report). Motions-generic (Phase D) and `Editor<B, H>` flip (Phase B)
ship in **0.0.35 / Patch C-δ.2**, then 0.1.0 freeze.

#### Buffer changes (breaking)

- `Buffer::viewport()` and `Buffer::viewport_mut()` are **deleted**. The
  `viewport: Viewport` field is gone from `Buffer`.
- `Buffer::ensure_cursor_visible(&mut self)` now takes `&mut Viewport`:
  ```rust
  // before
  buffer.ensure_cursor_visible();
  // after
  buffer.ensure_cursor_visible(&mut viewport);
  ```
- `Buffer::cursor_screen_row(&self) -> Option<usize>` →
  `Buffer::cursor_screen_row(&self, viewport: &Viewport) -> Option<usize>`.
- `Buffer::screen_rows_between(&self, start, end)` →
  `Buffer::screen_rows_between(&self, viewport: &Viewport, start, end)`.
- `Buffer::max_top_for_height(&self, height)` →
  `Buffer::max_top_for_height(&self, viewport: &Viewport, height)`.
- The `Viewport` struct itself stays in `hjkl-buffer` (it depends on
  `hjkl_buffer::{Wrap, Position}` and the rope-walking `wrap_segments` math),
  and is re-exported from `hjkl_engine::types::Viewport` so SPEC consumers keep
  their import path. The placeholder shape (`top_line/height/scroll_off`) on
  `hjkl_engine::types::Viewport` is replaced by the working shape
  (`top_row/top_col/width/height/wrap/text_width`).

#### Search auto-scroll change (path **a** chosen)

- `Buffer::search_forward` / `search_backward` no longer call
  `ensure_cursor_visible` after a hit. Search becomes a pure observer that moves
  the cursor only. Engine call sites re-apply visibility through
  `Editor::ensure_cursor_in_scrolloff` (which already runs at end-of-step).
  Hosts that drive the buffer directly should follow `search_*` with
  `buffer.ensure_cursor_visible(&mut viewport)`.

Rationale: cleaner separation; the alternative (path b — adding a
`viewport: &mut Viewport` arg to four search methods) churned the API more
without buying anything since the engine already re-runs scrolloff math.

#### Renderer change (breaking)

- `hjkl_buffer::BufferView` gains a `viewport: &'a Viewport` field.
  ```rust
  let view = hjkl_buffer::BufferView {
      buffer: &buf,
      viewport: &my_viewport,   // NEW
      // ...rest unchanged
  };
  ```

#### `Host` trait grows viewport methods

```rust
pub trait Host: Send {
    // ...existing...
    fn viewport(&self) -> &Viewport;
    fn viewport_mut(&mut self) -> &mut Viewport;
}
```

Mirrored on `EngineHost` (the object-safe slice the boxed editor host uses).
`DefaultHost` carries a `viewport: Viewport` field defaulting to 80×24, plus a
`DefaultHost::with_viewport(vp)` constructor for non-default sizes.

#### Engine call-site relocation

All ~15 reaches in `editor.rs` from `self.buffer.viewport*()` route to
`self.host.viewport*()`. Scrolloff math (`ensure_cursor_in_scrolloff`,
`ensure_scrolloff_wrap`) splits the disjoint `(self.buffer, self.host)` borrow
cleanly. `Editor::set_viewport_top`, `scroll_viewport`, `scroll_cursor_to`,
snapshot/restore, `cursor_screen_row` getter, and the mouse hit-test all read
viewport from the host.

Motion bodies that read viewport (`H` / `M` / `L` and `gj` / `gk`'s wrap path)
gained a `&Viewport` parameter; vim FSM dispatch sites copy
`*ed.host().viewport()` (Viewport is `Copy`) and pass it in to keep the
disjoint-borrow story clean.

#### Migration cheat-sheet

| Crate / file:line                                        | Before                                        | After                                                                                 |
| -------------------------------------------------------- | --------------------------------------------- | ------------------------------------------------------------------------------------- |
| **sqeel** `sqeel-tui/src/lib.rs:785,786`                 | `editor.buffer().viewport().top_row`          | `editor.host().viewport().top_row`                                                    |
| **sqeel** `sqeel-tui/src/lib.rs:3571,3572`               | `editor.buffer().viewport().top_*`            | `editor.host().viewport().top_*`                                                      |
| **sqeel** `sqeel-tui/src/lib.rs:4373`                    | `let v = editor.buffer_mut().viewport_mut();` | `let v = editor.host_mut().viewport_mut();`                                           |
| **sqeel** `sqeel-tui/src/lib.rs:4427`                    | `BufferView { buffer: editor.buffer(), … }`   | `BufferView { buffer: editor.buffer(), viewport: editor.host().viewport(), … }`       |
| **sqeel** `sqeel-tui/src/host.rs` (`SqeelHost`)          | impl missing `viewport`/`viewport_mut`        | add `viewport: hjkl_buffer::Viewport` field + impl                                    |
| **buffr** `crates/buffr-modal/src/host.rs` (`BuffrHost`) | impl missing `viewport`/`viewport_mut`        | same — add field + impl                                                               |
| **inbx** `apps/inbx/src/runtime/*`                       | uses `runtime::Editor` re-exports only        | host impls (if any) need viewport methods; no direct `Buffer::viewport()` calls today |

Workspace bumps `0.0.33` → `0.0.34`. Member crate pins (`=0.0.33` → `=0.0.34`)
and `Cargo.lock` updated.

**Next patch (0.0.35 / C-δ.2):** motions-generic (`B: Cursor + Query`) +
`Editor<'a, B: Buffer, H: Host>` flip. Then 0.1.0 cut (Patch C-ε): seal the
`Buffer` trait family, freeze `EditorSnapshot::VERSION`, take the
`cargo public-api` baseline.

## [0.0.33] - 2026-04-26

### Patch C-γ (partial) — fold relocation + SPEC constructor preview

This patch was scoped as the 0.1.0 keystone (Editor generic flip, motion bound
lift, fold relocation, freeze contract) but **stops at 0.0.33** because two of
the three deferred troikas trip the agent-plan stop thresholds:

- **Phase B (Editor `<B, H>` generic flip)** — `editor.rs` (3094 lines) and
  `vim.rs` (8800 lines) reach into ~46 distinct `hjkl_buffer::Buffer` methods,
  most of them outside the SPEC trait surface (viewport/render/wrap/cache).
  Generic-ifying without a private engine-internal super-trait — or relocating
  ~33 helpers into the engine — is multi-thousand-LOC churn that can't land in
  one coherent patch. **Stop threshold #2** fires: ship Phase A + E and let
  0.1.0 wait one more patch.
- **Phase D (motion bodies generic over `Cursor + Query + …`)** — the screen
  motions (`move_screen_vertical`, `step_screen`, `move_viewport_*`) call
  `buf.viewport()`. SPEC.md §"`Buffer` trait surface" explicitly relocates
  viewport off `Buffer` onto `Host`, so motions can't be generic over
  `B: Cursor + Query` without a host-supplied viewport accessor that doesn't
  exist yet. **Stop threshold #3** fires: track the SPEC delta and let 0.1.0
  land it together.

The bits that compose cleanly today land here. The 0.1.0 cut becomes Patch C-δ.

#### Phase A (preview) — `Editor::with_options(buffer, host, options)`

The SPEC-shaped constructor lands under a non-clashing name so 0.0.x consumers'
`Editor::new(KeybindingMode)` keeps compiling:

```rust
pub fn with_options<H: Host + 'a>(
    buffer: hjkl_buffer::Buffer,
    host: H,
    options: hjkl_engine::Options,
) -> Editor<'a>
```

Internally it translates SPEC `Options` into the legacy `Settings` struct (the
two are field-isomorphic except for type widths and `WrapMode` vs
`hjkl_buffer::Wrap`). At 0.1.0 (Patch C-δ) this constructor renames to plain
`Editor::new`, the `<B, H>` generics flip in place, and the legacy
`Editor::new(KeybindingMode)` / `Editor::with_host(km, host)` shims get deleted.

Migration today:

```rust
// 0.0.32 (no change required at 0.0.33)
let mut e = Editor::new(KeybindingMode::Vim);

// 0.0.33 SPEC-flavoured (optional, future-proof)
let mut e = Editor::with_options(
    hjkl_buffer::Buffer::new(),
    hjkl_engine::DefaultHost::new(),
    hjkl_engine::Options::default(),
);
```

#### Phase E (partial) — fold relocation in `editor.rs::ensure_scrolloff_wrap`

`Editor::ensure_scrolloff_wrap` now reads visible-row iteration through
`crate::buffer_impl::BufferFoldProvider` instead of calling
`hjkl_buffer::Buffer::next_visible_row` / `prev_visible_row` directly. The
borrow-checker conflict that blocked Patch C-β is resolved by scoping the
`BufferFoldProvider` borrow to a `let { … }` block that drops before the mutable
`viewport_mut()` write. No behaviour change — `BufferFoldProvider` forwards
directly to the buffer's fold storage.

The `motions.rs` fold call sites (`move_vertical`, `move_screen_vertical`,
`step_screen`) remain on the concrete `Buffer::next_visible_row` /
`prev_visible_row` API. Relocating them requires either Phase D's motion generic
flip (blocked above) or a wider motion API that takes
`folds: &dyn FoldProvider`, which causes a re-borrow conflict against the
`&mut Buffer` motion parameter when the provider wraps the same buffer. That's
an entry on the Patch C-δ punch list.

#### Phase F (preview) — `PUBLIC_API.md` baseline

`crates/hjkl-engine/PUBLIC_API.md` ships as the reference baseline for the 0.1.0
surface diff. Generated with
`cargo +nightly public-api -p hjkl-engine --simplified` (toolchain installed
locally, not vendored). 2030 lines of public surface today; the freeze contract
trims the deprecated shims (`Editor::new(KeybindingMode)`,
`Editor::with_host(km, host)`) and pins the rest at 0.1.0.

#### Phase G — version pin

Workspace bumps `0.0.32` → `0.0.33`. Member crate pins (`=0.0.32` → `=0.0.33`).

### Deferred to Patch C-δ (the real 0.1.0)

- Phase B: `Editor<'a, B: Buffer = hjkl_buffer::Buffer, H: Host = DefaultHost>`
  generic flip with default type params. Likely needs a private engine-internal
  `BufferExt` trait or relocation of viewport/render helpers out of
  `hjkl_buffer::Buffer`.
- Phase D: `motions::*` generic over `B: Cursor + Query + BufferEdit` plus a
  `folds: &dyn FoldProvider` parameter on fold-aware motions and a host-supplied
  viewport accessor on screen-relative motions.
- Phase E (rest): `motions.rs` fold call sites relocated through `FoldProvider`
  (gated on Phase D).
- Phase F (real): `cargo public-api` CI gate. Trim deprecated shims.
- Phase A (rename): `Editor::with_options` → `Editor::new`. Delete
  `Editor::with_host` and `Editor::new(KeybindingMode)`.
- Phase G: 0.0.33 → 0.1.0; SPEC.md freeze contract; `=0.0.33` → `=0.1.0` on
  consumer pins.

Consumers (`sqeel`, `buffr`, `inbx`) keep building unchanged at 0.0.33: bump
`hjkl-*` pins from `=0.0.32` to `=0.0.33`. No source changes required.

## [0.0.32] - 2026-04-26

### Patch C-β (partial) — name freeze + additive `FoldProvider`

This patch is the breaking-rename slice of the planned 0.1.0 keystone. The 0.1.0
cut itself slipped to Patch C-γ because the deeper restructuring it requires
(Editor generic flip, motion bound lifts, fold-iteration relocation) hits
borrow-checker constraints that can't be undone without rewiring `Editor`'s
field layout. Per the `BCTP`-style stop thresholds in the agent plan, we land
the bits that compose cleanly today (Phases 1, 2, additive 4) and ship the
larger flip together as Patch C-γ.

#### Phase 1 — `#[deprecated]` aliases removed

The 0.0.31 prefixed-name aliases are gone:

| 0.0.31 (deprecated)            | 0.0.32                    |
| ------------------------------ | ------------------------- |
| `hjkl_engine::SpecBuffer`      | `hjkl_engine::Buffer`     |
| `hjkl_engine::SpecBufferEdit`  | `hjkl_engine::BufferEdit` |
| `hjkl_engine::EditOp`          | `hjkl_engine::Edit`       |
| `hjkl_engine::PlannedViewport` | `hjkl_engine::Viewport`   |

Consumers still naming the prefixed forms get hard compile errors. Pin bumps
(`=0.0.31` → `=0.0.32`) plus the rename are the migration.

#### Phase 2 — `Editor` `_xy` / `_xywh` naming asymmetries resolved

At 0.1.0 freeze the unprefixed name belongs to the engine-native, ratatui-free
variant ("engine never imports ratatui" per SPEC.md §"Style"). Ratatui-flavoured
variants take an `_in_rect` suffix or the `ratatui_` prefix. **Breaking** —
consumers calling these methods need source changes:

| 0.0.31                                                                | 0.0.32                                                |
| --------------------------------------------------------------------- | ----------------------------------------------------- |
| `Editor::mouse_click_xy(area_x, area_y, col, row)`                    | `Editor::mouse_click(area_x, area_y, col, row)`       |
| `Editor::mouse_click(area, col, row)` (`#[cfg(feature = "ratatui")]`) | `Editor::mouse_click_in_rect(area, col, row)`         |
| `Editor::mouse_extend_drag_xy(area_x, area_y, col, row)`              | `Editor::mouse_extend_drag(area_x, area_y, col, row)` |
| `Editor::mouse_extend_drag(area, col, row)`                           | `Editor::mouse_extend_drag_in_rect(area, col, row)`   |
| `Editor::cursor_screen_pos_xywh(x, y, w, h)`                          | `Editor::cursor_screen_pos(x, y, w, h)`               |
| `Editor::cursor_screen_pos(area)`                                     | `Editor::cursor_screen_pos_in_rect(area)`             |
| `Editor::install_engine_syntax_spans(spans)` (engine-native `Style`)  | `Editor::install_syntax_spans(spans)`                 |
| `Editor::install_syntax_spans(spans)` (ratatui `Style`)               | `Editor::install_ratatui_syntax_spans(spans)`         |
| `Editor::intern_engine_style(s)` (engine-native `Style`)              | `Editor::intern_style(s)`                             |
| `Editor::intern_style(s)` (ratatui `Style`)                           | `Editor::intern_ratatui_style(s)`                     |

No deprecation aliases — Rust forbids overloading and the rename collisions
under feature gates make a back-compat shim impossible.

#### Phase 4 (additive only) — `FoldProvider` trait shipped, call sites NOT relocated

The fold-iteration trait and its provider types land additively:

```rust
pub trait FoldProvider: Send {
    fn next_visible_row(&self, row: usize, row_count: usize) -> Option<usize>;
    fn prev_visible_row(&self, row: usize) -> Option<usize>;
    fn is_row_hidden(&self, row: usize) -> bool;
    fn fold_at_row(&self, row: usize) -> Option<(usize, usize, bool)>;
}

pub struct NoopFoldProvider;          // every row visible
pub struct BufferFoldProvider<'a>;    // wraps `&hjkl_buffer::Buffer`
```

Re-exported at
`hjkl_engine::{FoldProvider, NoopFoldProvider, BufferFoldProvider}` and
`hjkl_editor::spec::{FoldProvider, NoopFoldProvider, BufferFoldProvider}`.

The engine call sites (`editor.rs::scroll_*`, `motions.rs::move_vertical`,
`motions.rs::move_screen_vertical`) are **NOT** relocated in this patch. Reason:
motions take `&mut Buffer`, and constructing a `BufferFoldProvider` from the
same buffer would create a `&Buffer` aliasing the `&mut`. Threading a separate
fold provider through requires the `Editor<B, H>` generic flip (Phase 6) so the
host owns the provider on a different field. That work ships as Patch C-γ
alongside motion bound lifts.

The trait surface is stable now — Patch C-γ flips the call sites without
re-touching public API.

### Deferred to Patch C-γ

- **Phase 3** — `Editor::new(buffer, host, options)` per SPEC. The current
  `Editor::new(KeybindingMode)` shim stays; ~74 in-engine test sites use it. The
  new constructor only delivers value paired with the generic flip.
- **Phase 5** — motion bound lift to `B: Cursor + Query`. Bodies stay concrete
  over `hjkl_buffer::Buffer` until Phase 6 lands.
- **Phase 6** — `Editor<'a, B = hjkl_buffer::Buffer, H = DefaultHost>` generic
  flip. Touches every method on `Editor` (~3500 LOC); the highest-risk phase.
- **Phase 7** — SPEC freeze + `cargo public-api` baseline (gated on Phase 6).
- **Phase 8** — `0.1.0` version cut (gated on Phases 3-7).

### Migration

Consumers calling the renamed methods need source changes. The full mapping is
in the Phase 2 table above. Typical hits in TUI hosts:

```diff
-editor.mouse_click(area, col, row);
+editor.mouse_click_in_rect(area, col, row);

-editor.cursor_screen_pos(area);
+editor.cursor_screen_pos_in_rect(area);

-editor.install_syntax_spans(spans);          // ratatui Style
+editor.install_ratatui_syntax_spans(spans);

-editor.intern_style(ratatui_style);
+editor.intern_ratatui_style(ratatui_style);

-editor.install_engine_syntax_spans(spans);    // engine Style
+editor.install_syntax_spans(spans);

-editor.intern_engine_style(engine_style);
+editor.intern_style(engine_style);
```

Pin bumps: `=0.0.31` → `=0.0.32` in consumer `Cargo.toml`s.

### Test counts

- `cargo test --workspace`: **668 passed** (unchanged).
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`: green.
- `cargo check --no-default-features`: green.

### SPEC.md

No structural change. `FoldProvider` is documented in §"Out of scope" / `Host`
discussions of SPEC.md as the future trait surface — the trait now exists
additively; relocation of call sites lands at Patch C-γ.

## [0.0.31] - 2026-04-26

### Changed (public-API rename pass — pre-0.1.0 freeze prep)

The 0.1.0 cut freezes the trait surface; once frozen, renames need a
semver-major bump. This patch is the last cheap window in the 0.0.x churn series
to clean up names that got shoehorned in mid-refactor (Phase 5 trait extraction,
0.0.26).

Every rename ships with a `#[deprecated]` type alias at the OLD name so
consumers pinning `=0.0.30` keep building unchanged. The deprecated aliases are
deleted at the 0.1.0 cut (Patch C-β).

#### `hjkl_engine` re-export rename table

| 0.0.30                         | 0.0.31                    | Why                                                                                                                                                       |
| ------------------------------ | ------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `hjkl_engine::SpecBuffer`      | `hjkl_engine::Buffer`     | The "Spec" prefix was a 0.0.26 stop-gap — `crate::types::Buffer` doesn't clash with anything at the engine root, so the SPEC-named re-export wins.        |
| `hjkl_engine::SpecBufferEdit`  | `hjkl_engine::BufferEdit` | Same reasoning. The trait-vs-value-type clash (`BufferEdit` trait vs `Edit` struct) lives inside `crate::types`; at the engine root no clash exists.      |
| `hjkl_engine::EditOp`          | `hjkl_engine::Edit`       | The `EditOp` rename was needed because `hjkl_buffer::Edit` is also a value type, but `hjkl_buffer::Edit` isn't re-exported from `hjkl_engine` — no clash. |
| `hjkl_engine::PlannedViewport` | `hjkl_engine::Viewport`   | Nothing else uses the `Viewport` name at the engine root — the "Planned" prefix was redundant.                                                            |

#### Concerns evaluated, decisions, and "leave as-is" rationale

| Concern                                                         | Decision      | Why                                                                                                                                                                                                                                                                                                                                           |
| --------------------------------------------------------------- | ------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `hjkl_engine::PlannedInput`                                     | leave         | `crate::input::Input` (legacy) and `crate::types::Input` (SPEC) are both reachable; `PlannedInput` is a useful disambiguation. The legacy `Input` goes away at 0.1.0; rename then.                                                                                                                                                            |
| `hjkl_engine::types::sealed`                                    | leave         | Already `pub(crate)`. Verified no public surface leaks the seal.                                                                                                                                                                                                                                                                              |
| `Editor::new` vs `Editor::with_host`                            | leave         | `Editor::new` keeps the back-compat `DefaultHost` shim; `with_host` is the real one. Patch C-β at 0.1.0 swaps to `Editor::new(buffer, host, options)` per SPEC.                                                                                                                                                                               |
| `EngineHost` vs `Host`                                          | leave         | `EngineHost` is the object-safe slice the boxed-trait-object slot needs. The name carries useful intent — "this is the engine's internal slice".                                                                                                                                                                                              |
| `Editor::mouse_click(Rect)` vs `mouse_click_xy`                 | leave         | Rust forbids overloading; renaming `mouse_click_xy` → `mouse_click` while keeping the `Rect` form requires the `Rect` form to take a different name, breaking `editor.mouse_click(rect, …)` call sites. The `_xy` suffix carries genuine signal ("raw x/y, no Rect"); ratatui-Rect is a sugar layer. **Documented for SPEC review at 0.1.0.** |
| `Editor::install_syntax_spans` vs `install_engine_syntax_spans` | leave         | Same shape — rename while keeping back-compat would require feature-gating two methods with the same name. Defer to 0.1.0.                                                                                                                                                                                                                    |
| `Editor::cursor_screen_pos(Rect)` vs `cursor_screen_pos_xywh`   | leave         | Same.                                                                                                                                                                                                                                                                                                                                         |
| `Editor::intern_style(ratatui)` vs `intern_engine_style`        | leave         | Same — rename plus alias produces a same-name conflict under feature combinations. Defer to 0.1.0.                                                                                                                                                                                                                                            |
| `pub mod motions`                                               | leave (`pub`) | Curated re-export at the engine root would pollute the namespace with 24 names. The explicit module path (`hjkl_engine::motions::move_word_fwd`) is the right shape.                                                                                                                                                                          |

The five `Editor` method asymmetries (`mouse_click`/`cursor_screen_pos` /
`install_syntax_spans` / `intern_style` and the drag pair) are **flagged for
SPEC review at 0.1.0**. The naming asymmetry is real, but resolving it cleanly
requires a breaking change (Rust's no-overloading rule prevents a same-name
deprecated alias under feature gates). The 0.1.0 cut is the right place to pick
the canonical names and break.

### Migration (downstream consumers)

No source change required — every renamed re-export ships with a `#[deprecated]`
type alias at the old name. Consumers see `#[deprecated]`-flavoured warnings and
update at their leisure:

```text
warning: use of deprecated type alias `hjkl_engine::SpecBuffer`:
         renamed to `hjkl_engine::Buffer`
```

Pin bumps (`=0.0.30` → `=0.0.31`) in consumer `Cargo.toml`s suffice. At 0.1.0,
the deprecated aliases are deleted and the `#[deprecated]` warnings turn into
hard compile errors — schedule the swap before then.

### Test counts

- `cargo test --workspace`: **668 passed** (unchanged from 0.0.30 — the rename
  pass is a no-op for runtime behaviour).
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`: green
  (no internal call sites trigger the new deprecation warnings).
- `cargo check --no-default-features`: green.

### SPEC.md

No change. The SPEC names (`Buffer`, `BufferEdit`, `Edit`, `Viewport`) already
match the new re-exports — this patch makes the actual API match the SPEC, which
is the whole point.

## [0.0.30] - 2026-04-26

### Changed (Patch C-α — motion / viewport-helper relocation)

- **24 vim motion helpers and 3 viewport-relative motions move from
  `hjkl_buffer::Buffer` to a new `hjkl_engine::motions` module.** Per [SPEC.md],
  "motions don't belong on `Buffer` — they're computed over the buffer, not
  delegated to it". The bodies are now free functions taking
  `&mut hjkl_buffer::Buffer` (concrete; the planned 0.1.0 freeze patch lifts the
  bound to `B: Cursor + Query` once the fold-iteration helpers move to
  `Host::FoldProvider`). Engine FSM (`vim.rs`) and `editor.rs` now call e.g.
  `hjkl_engine::motions::move_word_fwd(&mut ed.buffer, false, count, &iskeyword)`
  rather than `ed.buffer.move_word_fwd(false, count, &iskeyword)`. Relocated
  motions:
  - Horizontal: `move_left`, `move_right_in_line`, `move_right_to_end`,
    `move_line_start`, `move_first_non_blank`, `move_line_end`,
    `move_last_non_blank`, `move_paragraph_prev`, `move_paragraph_next`.
  - Vertical: `move_up`, `move_down`, `move_screen_up`, `move_screen_down`,
    `move_top`, `move_bottom`.
  - Word: `move_word_fwd`, `move_word_back`, `move_word_end`,
    `move_word_end_back`.
  - Find / match: `match_bracket`, `find_char_on_line`.
  - Viewport-relative: `move_viewport_top`, `move_viewport_middle`,
    `move_viewport_bottom`.
- **Tests for the 24 motions follow the bodies into
  `hjkl_engine::motions::tests`.** They verify identical semantics (sticky-col
  carry, fold-aware vertical stepping, wrap-aware screen motion) against the
  same `hjkl_buffer::Buffer` they exercised before.
- **`hjkl_buffer::wrap` is now a public module** so the engine motion module can
  call `wrap::wrap_segments` / `wrap::segment_for_col` directly. Previously
  crate-private; the only consumer was the buffer itself (motion + render). The
  exported surface stays minimal — two free functions and the `Wrap` enum
  (already re-exported).
- **`hjkl_buffer::motion` shrinks to host the `is_keyword_char` parser only**
  (still re-exported from the crate root unchanged). The `iskeyword`-spec parser
  has no buffer dependency, so it stays alongside the data it parses; the engine
  motion module re-uses it via `hjkl_buffer::is_keyword_char`.

### Removed (breaking — `hjkl_buffer::Buffer` inherent API)

The 24 inherent motion methods on `hjkl_buffer::Buffer` are gone. Hosts that
called them directly (rather than through `hjkl_engine::Editor`) swap to the
engine free-function form:

| 0.0.29                                 | 0.0.30                                                       |
| -------------------------------------- | ------------------------------------------------------------ |
| `buf.move_left(n)`                     | `hjkl_engine::motions::move_left(&mut buf, n)`               |
| `buf.move_word_fwd(big, n, isk)`       | `hjkl_engine::motions::move_word_fwd(&mut buf, big, n, isk)` |
| `buf.move_up(n, &mut sticky)`          | `hjkl_engine::motions::move_up(&mut buf, n, &mut sticky)`    |
| `buf.match_bracket()`                  | `hjkl_engine::motions::match_bracket(&mut buf)`              |
| `buf.find_char_on_line(ch, fwd, till)` | `hjkl_engine::motions::find_char_on_line(&mut buf, …)`       |
| `buf.move_viewport_top(off)`           | `hjkl_engine::motions::move_viewport_top(&mut buf, off)`     |
| (… same shape for the remaining 18 …)  | (… same shape …)                                             |

The fold-iteration helpers (`next_visible_row`, `prev_visible_row`,
`is_row_hidden`, `fold_at_row`, `folds`, `add_fold`, `open_fold_at`,
`close_fold_at`, `toggle_fold_at`, `open_all_folds`, `close_all_folds`,
`clear_all_folds`, `remove_fold_at`, `invalidate_folds_in_range`) **stay on
`hjkl_buffer::Buffer`** for now — see "Deferred to follow-up" below.

### Migration (downstream consumers)

Sqeel, buffr, and inbx **need only a pin bump** — their source compiles
unchanged because they all drive motions through `hjkl_engine::Editor` rather
than calling buffer motion methods directly. Bump the `=0.0.29` pin in each
consumer's root `Cargo.toml` to `=0.0.30`:

```toml
hjkl-engine = "=0.0.30"
hjkl-buffer = "=0.0.30"
hjkl-editor = "=0.0.30"   # if used
hjkl-ratatui = "=0.0.30"  # if used
```

If a host did call a buffer motion inherent method (e.g. embedding host code
that bypasses the engine FSM), swap to the `hjkl_engine::motions::*` free
function with a `&mut buffer` first parameter.

### Deferred to follow-up (the 0.1.0 freeze)

Patch C's full scope envisaged folding three changes into one cut:

1. **Motion / viewport-helper relocation** ✅ landed in this release.
2. **Fold provider relocation onto `Host`** (Path X) — _deferred_. Audit
   reconfirmed the fold storage on `hjkl_buffer::Buffer` is tightly coupled to
   the buffer's dirty-gen + render-cache invariants; moving the _iteration
   helpers_ without first hoisting the _storage_ buys a half-done split. The
   motion module currently calls `buf.next_visible_row` / `buf.prev_visible_row`
   directly until the fold storage moves to a `Host::FoldProvider`. Tracking
   issue: the 0.1.0 patch will introduce `FoldProvider` on `Host`, move the
   iteration helpers, and lift the motion bound to `B: Cursor + Query`.
3. **`Editor<'a, B: Buffer = …, H: Host = …>` generic flip** — _deferred_.
   Requires (2) to land first; the motion bodies still take the concrete
   `hjkl_buffer::Buffer`, so the generic parameter has no teeth yet.

Splitting the patch lets 0.0.30 ship the relocation cleanly today; the 0.1.0 cut
bundles fold relocation + generic flip + freeze contract in one step. Per
CHANGELOG `[0.0.27]`'s "Better to land a clean 0.0.30 than a broken 0.1.0".

### Test counts

- `cargo test --workspace`: **668 passed** (was 663 — +5 from new motion-module
  relocation tests + iskeyword unit coverage).
- `cargo check --workspace --no-default-features`: green.

### Roadmap

- **Patch C-α — this release (0.0.30)**: 24 motions + 3 viewport- relative
  motions hosted in `hjkl_engine::motions` as free functions.
- **Patch C-β / 0.1.0**: `Host::FoldProvider`, fold-iteration helper relocation,
  motion bound lift to `B: Cursor + Query`, `Editor<B, H>` generic flip with
  default type params for back-compat, public surface freezes.

[SPEC.md]:
  https://github.com/kryptic-sh/hjkl/blob/main/crates/hjkl-engine/SPEC.md

## [0.0.29] - 2026-04-26

### Added (Patch B — `Host` trait wired into `Editor`)

- **`Editor` now carries a `host: Box<dyn EngineHost + 'a>` slot.** Engine
  side-channels for clipboard, cursor-shape, and time read/write through the
  SPEC `Host` trait instead of inherent fields. Patch C (0.1.0) replaces the
  boxed object with a generic `Editor<'a, B: Buffer, H: Host>` parameter; this
  patch wires the plumbing without flipping the public type.
- **`Editor::with_host(keybinding_mode, host)`** — new constructor taking any
  `H: Host + 'a`. Hosts that need real clipboard / cursor-shape / `now()`
  plumbing call this; the legacy `Editor::new(keybinding_mode)` is preserved as
  a back-compat shim that wires `DefaultHost::new()` internally so 0.0.28-era
  callers keep building unchanged.
- **`Editor::host()` / `Editor::host_mut()`** — accessors returning the
  object-safe slice `&dyn EngineHost` / `&mut dyn EngineHost`. Tests use these
  to assert the recorded clipboard / cursor-shape sequence; production code
  rarely needs them.
- **`crate::types::EngineHost`** — object-safe slice of the SPEC `Host` trait
  used internally so `Editor` can box-erase host implementations without naming
  the host's `Intent` associated type. Not implemented directly — blanket-impl
  forwards from any `H: Host`.
- **`crate::types::DefaultHost`** — no-op `Host` implementation. Suitable for
  tests, headless embedding, or any host that doesn't yet need clipboard /
  cursor-shape / cancellation plumbing. `write_clipboard` stores in-memory
  (round-trip-only); `now` returns `Instant::now()` elapsed since construction;
  `prompt_search` returns `None`; `emit_cursor_shape` records the most recent
  shape (readable via `DefaultHost::last_cursor_shape`); `emit_intent` discards
  (`type Intent = ();`).

### Changed (engine ↔ host side-channel rewiring)

- **Yank / cut paths now drive `Host::write_clipboard`.** Every
  `last_yank = Some(text)` site in the FSM (yank, delete, change, substitute,
  paste-to- cut, blockwise yank, mouse-cut funnel) now also queues the payload
  onto the host's clipboard. The legacy `Editor::last_yank: Option<String>`
  field is retained so 0.0.28-era hosts that drained it directly keep working;
  it will be removed at 0.1.0.
- **`:set timeoutlen` math now reads `Host::now()`.** The chord-timeout (`gg` /
  `dd` / `<C-w>v` etc.) compares two `Duration` values from `Host::now()` rather
  than two `Instant::now()` snapshots, so macro replay / headless drivers stay
  deterministic regardless of wall-clock skew.
  `VimState::last_input_at: Option<Instant>` is preserved for snapshot tests
  that observe it directly; the new
  `VimState::last_input_host_at: Option<Duration>` field carries the
  host-monotonic reading the timeout check itself uses.
- **Cursor-shape emit on every mode transition.** `Editor::feed_input` and
  `Editor::handle_key` call a new internal `emit_cursor_shape_if_changed()`
  after each input step. Insert mode emits `CursorShape::Bar`, every other
  public mode emits `CursorShape::Block`. The emit is debounced — only fires
  when `vim_mode()` actually changes. Hosts implement `Host::emit_cursor_shape`
  to repaint accordingly.

### Migration (downstream consumers)

- **No source change required for hosts using `Editor::new(...)`.** The
  back-compat shim wires `DefaultHost` internally; clipboard becomes a no-op
  (still round-trips through `last_yank`), cursor-shape goes to a recorder slot,
  `now()` reads wall-clock elapsed.
- **Hosts already implementing `hjkl_engine::Host`** (e.g. `SqeelHost`,
  `BuffrHost`) plug straight in via `Editor::with_host(..., host)`. Their
  `write_clipboard` / `emit_cursor_shape` / `now()` will start receiving
  engine-driven events the moment the host swaps `Editor::new(...)` →
  `Editor::with_host(..., host)`.
- **Fold provider (`next_visible_row` / `prev_visible_row` / `is_row_hidden` /
  `fold_at_line`) is NOT moved to `Host` in this patch.** Audit found the
  iteration is tightly coupled to buffer-private fold storage — relocating the
  iteration without first hoisting the storage buys nothing and risks a
  half-done split. Fold storage stays on `hjkl_buffer::Buffer` for now; the
  engine's existing `Buffer::next_visible_row` / `prev_visible_row` calls
  remain. Patch C (0.1.0) revisits this once the motion / fold / viewport-scroll
  helpers are relocated as free functions over `B: Cursor + Query`.

### Roadmap

- **Patch A (0.0.28)**: ✅ `sticky_col` + `iskeyword` off `Buffer`.
- **Patch B (this release, 0.0.29)**: ✅ `Host` wiring — clipboard, cursor-shape
  emit, `Host::now()`. Fold provider deferred to Patch C.
- **Patch C (0.1.0)**: `Editor<'a, B: Buffer = ..., H: Host = ...>` flip,
  fold-iteration relocation, motion / fold / viewport-scroll helpers relocated
  as free functions over `B: Cursor + Query`, public surface freezes.

## [0.0.28] - 2026-04-26

### Changed (Patch A — `sticky_col` + `iskeyword` hoist to `Editor`)

- **`sticky_col` (vim's `curswant`) is now stored on `Editor`.** The single
  source of truth for the desired vertical-motion column moves out of both
  `hjkl_buffer::Buffer` and the engine-internal `VimState`. New accessors:
  - `Editor::sticky_col() -> Option<usize>`
  - `Editor::set_sticky_col(Option<usize>)` Buffer motion methods that need the
    sticky value (`Buffer::move_up` / `move_down` / `move_screen_up` /
    `move_screen_down`) now take a `&mut Option<usize>` parameter so the caller
    owns the storage.
- **`iskeyword` is now stored only on `Editor::settings.iskeyword`.** Buffer no
  longer mirrors it. `Editor::set_iskeyword(...)` keeps working
  (source-compatible with 0.0.27) but no longer writes back into the buffer.
  Buffer word motions (`Buffer::move_word_fwd` / `move_word_back` /
  `move_word_end` / `move_word_end_back`) now take `iskeyword: &str` as a
  parameter so the host can change it without re-publishing onto the buffer.
- This unblocks Patch C (`Editor<B: Buffer, H: Host>` generic-ification at
  0.1.0): the audit identified `sticky_col` and `iskeyword` as vim-FSM concerns
  that don't belong on the SPEC `Buffer` trait surface. They had to come off
  `Buffer` before the FSM-internal motion helpers can be relocated into the
  engine as free functions over `B: Cursor + Query`.

### Removed (breaking — `hjkl_buffer::Buffer` public API)

- `Buffer::sticky_col()` — read `Editor::sticky_col()` instead.
- `Buffer::set_sticky_col(...)` — call `Editor::set_sticky_col(...)` instead.
- `Buffer::iskeyword()` — read `Editor::settings.iskeyword` instead.
- `Buffer::set_iskeyword(...)` — call `Editor::set_iskeyword(...)` (which now
  only mutates `Editor::settings.iskeyword`) instead.
- The `pub fn refresh_sticky_col_from_cursor` helper on `Buffer` is gone;
  horizontal motions no longer touch a buffer-side sticky field. The engine's
  existing `apply_sticky_col` already manages this from the Editor side.
- `Buffer::move_up`, `move_down`, `move_screen_up`, `move_screen_down` —
  signature changed to take `sticky_col: &mut Option<usize>`. Callers mirroring
  the engine pattern thread `&mut editor.sticky_col` through.
- `Buffer::move_word_fwd`, `move_word_back`, `move_word_end`,
  `move_word_end_back` — signature changed to take `iskeyword: &str` as the
  third / fourth positional argument.

### Migration (downstream consumers)

The buffer's `sticky_col` / `iskeyword` storage was an implementation detail
mirrored from `Editor` since 0.0.23. **No known consumer reads or writes these
fields directly** — sqeel, buffr, and inbx use the editor- level accessors. If a
host did call `buffer.sticky_col()` / `buffer.set_sticky_col(...)` /
`buffer.iskeyword()` / `buffer.set_iskeyword(...)` directly, swap to the
matching `Editor` methods listed above. The `:set iskeyword=...` ex command
keeps working end-to-end via `Editor::set_iskeyword`.

If a host called `Buffer::move_up` / `move_down` / `move_screen_up` /
`move_screen_down` / `move_word_*` directly (rather than through the engine's
motion grammar), thread the new `sticky_col` / `iskeyword` parameters through.

### Roadmap

- **Patch A — this release (0.0.28)**: `sticky_col` + `iskeyword` off `Buffer`.
- **Patch B (0.0.29)**: `Host` wiring — clipboard, cursor-shape emit, fold
  provider, `host.now()`. Lifts the remaining engine ↔ host side-channels onto
  the SPEC `Host` trait surface.
- **Patch C (0.1.0)**: `Editor<'a, B: Buffer = ..., H: Host = ...>` flip, motion
  / fold / viewport-scroll helpers relocated into the engine as free functions
  over `B: Cursor + Query`, public surface freezes.

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
