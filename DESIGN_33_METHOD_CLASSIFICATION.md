# Design: Reclassifying the "33-method" Buffer-Method Blocker

Status: **proposal, 0.0.34 → 0.1.0 keystone.** No code changes implied.
Audience: software architect deciding the 0.1.0 contract.
Scope: classify every `hjkl_buffer::Buffer` method the engine reaches but
SPEC's 13-method `Buffer` super-trait does not yet expose, and assign each
to its correct architectural home.

---

## TL;DR

After auditing every `self.buffer.…` / `ed.buffer_mut().…` / `buf.…` call
site in `crates/hjkl-engine/src/{editor.rs, vim.rs, motions.rs}`, the true
count of distinct buffer methods reached is **38**, of which **5 are already
covered by the SPEC sub-traits** (`cursor`, `set_cursor`, `line`, the
`row_count` ↔ `line_count` rename, and the `apply_edit` ↔ trio
`insert_at`/`delete_range`/`replace_range`). That leaves **33 methods to
classify**, matching the "33" the prior agents quoted.

The proposed classification (full table below):

| Bucket                          | Count |
|---------------------------------|-------|
| **Buffer (sub-trait expansion)**| 5     |
| **Editor (state migration)**    | 4     |
| **Host (trait expansion)**      | 6     |
| **Engine-private helper**       | 16    |
| **Delete (no longer needed)**   | 2     |

Post-classification SPEC `Buffer` surface: **18 methods** total
(was 13, cap is <40). Comfortably under cap with room for the future
grapheme/byte conversions called out in SPEC §"Open issues".

---

## 1. Method inventory

The complete list of `hjkl_buffer::Buffer` methods the engine touches.
"Already on SPEC trait?" = the engine could call this through the existing
`Cursor + Query + BufferEdit + Search` surface. Method numbers here are
stable identifiers used by the classification table in §2.

| #  | Method                          | Signature (abbreviated)                                          | Currently used at                                                                                                  | Already on SPEC trait? |
|----|---------------------------------|------------------------------------------------------------------|--------------------------------------------------------------------------------------------------------------------|------------------------|
| 1  | `lines`                         | `(&self) -> &[String]`                                           | editor.rs:729, 825, 1341, 1601, 1676, 1703, 1986, 2104; vim.rs:1419                                                | No (tangentially via `line(idx)` + `line_count`) |
| 2  | `line`                          | `(&self, row) -> Option<&str>`                                   | editor.rs:1551, 1614, 1929, 2004, 2078; vim.rs (assert sites)                                                      | **Yes** (`Query::line`, panics on OOB) |
| 3  | `cursor`                        | `(&self) -> Position`                                            | editor.rs (~20 sites); vim.rs                                                                                      | **Yes** (`Cursor::cursor`) |
| 4  | `dirty_gen`                     | `(&self) -> u64`                                                 | (not reached from engine — host-side only)                                                                          | No |
| 5  | `set_cursor`                    | `(&mut self, Position)`                                          | editor.rs (~10 sites); vim.rs (~12 sites)                                                                          | **Yes** (`Cursor::set_cursor`) |
| 6  | `ensure_cursor_visible`         | `(&mut self, &mut Viewport)`                                     | editor.rs:1788                                                                                                     | No |
| 7  | `cursor_screen_row`             | `(&self, &Viewport) -> Option<usize>`                            | editor.rs:1849, 1875                                                                                               | No |
| 8  | `screen_rows_between`           | `(&self, &Viewport, start, end) -> usize`                        | (engine-internal scrolloff math; called via private helpers)                                                        | No |
| 9  | `max_top_for_height`            | `(&self, &Viewport, height) -> usize`                            | editor.rs:1894                                                                                                     | No |
| 10 | `clamp_position`                | `(&self, Position) -> Position`                                  | buffer_impl.rs:232 (boundary helper)                                                                               | No |
| 11 | `set_spans`                     | `(&mut self, Vec<Vec<Span>>)`                                    | editor.rs:744, 849                                                                                                 | No |
| 12 | `replace_all`                   | `(&mut self, &str)`                                              | editor.rs:2155; vim.rs:3946                                                                                        | No (`BufferEdit::replace_range` over the whole buffer is the trait-equivalent) |
| 13 | `marks`                         | `(&self) -> &BTreeMap<char, Position>`                           | (snapshot path, vim FSM)                                                                                            | No |
| 14 | `spans`                         | `(&self) -> &[Vec<Span>]`                                        | vim.rs:8200, 8219, 8229                                                                                            | No |
| 15 | `as_string`                     | `(&self) -> String`                                              | vim.rs (test asserts only — engine-prod has no callers)                                                             | No (`Query::slice(0..end)` is equivalent) |
| 16 | `row_count`                     | `(&self) -> usize`                                               | editor.rs (~12 sites); vim.rs                                                                                      | **Yes** (`Query::line_count` → same value, just `u32`) |
| 17 | `apply_edit`                    | `(&mut self, Edit) -> Edit`                                      | editor.rs:1049, 2067, 2087, 2094                                                                                   | **Yes** (decomposed into `BufferEdit::insert_at` / `delete_range` / `replace_range`; the *return value* — the inverse — has no SPEC equivalent yet) |
| 18 | `set_search_pattern`            | `(&mut self, Option<Regex>)`                                     | vim.rs:583, 611                                                                                                    | No |
| 19 | `search_pattern`                | `(&self) -> Option<&Regex>`                                      | editor.rs:1632                                                                                                     | No |
| 20 | `set_search_wrap`               | `(&mut self, bool)`                                              | vim.rs:612                                                                                                         | No |
| 21 | `search_wraps`                  | `(&self) -> bool`                                                | buffer_impl.rs (Search trait impl)                                                                                 | No |
| 22 | `search_forward`                | `(&mut self, skip_current) -> bool`                              | vim.rs:657, 2311, 2438                                                                                             | No (semantically `Search::find_next` + `Cursor::set_cursor`) |
| 23 | `search_backward`               | `(&mut self, skip_current) -> bool`                              | vim.rs:659, 2313, 2440                                                                                             | No (semantically `Search::find_prev` + `Cursor::set_cursor`) |
| 24 | `search_matches`                | `(&mut self, row) -> Vec<(usize,usize)>`                         | editor.rs:1635                                                                                                     | No |
| 25 | `folds`                         | `(&self) -> &[Fold]`                                             | (snapshot/persist path)                                                                                            | No |
| 26 | `add_fold`                      | `(&mut self, start, end, closed)`                                | vim.rs:2808, 3300                                                                                                  | No |
| 27 | `remove_fold_at`                | `(&mut self, row) -> bool`                                       | vim.rs:2791                                                                                                        | No |
| 28 | `open_fold_at`                  | `(&mut self, row) -> bool`                                       | vim.rs:2773                                                                                                        | No |
| 29 | `close_fold_at`                 | `(&mut self, row) -> bool`                                       | vim.rs:2776                                                                                                        | No |
| 30 | `toggle_fold_at`                | `(&mut self, row) -> bool`                                       | vim.rs:2779                                                                                                        | No |
| 31 | `open_all_folds`                | `(&mut self)`                                                    | vim.rs:2782                                                                                                        | No |
| 32 | `close_all_folds`               | `(&mut self)`                                                    | vim.rs:2785                                                                                                        | No |
| 33 | `clear_all_folds`               | `(&mut self)`                                                    | vim.rs:2788                                                                                                        | No |
| 34 | `fold_at_row`                   | `(&self, row) -> Option<&Fold>`                                  | editor.rs:394 (via FoldProvider)                                                                                   | No (already routed through `FoldProvider`) |
| 35 | `is_row_hidden`                 | `(&self, row) -> bool`                                           | (via FoldProvider)                                                                                                  | No (already routed through `FoldProvider`) |
| 36 | `next_visible_row`              | `(&self, row) -> Option<usize>`                                  | editor.rs:1858 (via FoldProvider)                                                                                  | No (already routed through `FoldProvider`) |
| 37 | `prev_visible_row`              | `(&self, row) -> Option<usize>`                                  | editor.rs:1883 (via FoldProvider)                                                                                  | No (already routed through `FoldProvider`) |
| 38 | `invalidate_folds_in_range`     | `(&mut self, lo, hi)`                                            | editor.rs:1058                                                                                                     | No |

**Audit total: 38 methods reached.** SPEC-already-covered: 5 (rows 2, 3, 5,
16, 17 — `line`, `cursor`, `set_cursor`, `row_count`/`line_count`,
`apply_edit` decomposed). **33 methods need classification.**

The "33" the prior agents quoted is therefore exactly right.

> Note: `motions.rs` operates over `&Buffer` directly rather than going
> through the trait — but every method it calls is already in the SPEC
> surface (`cursor`/`set_cursor`/`line`/`row_count`) plus a couple of
> `step_forward`/`prev_word_start` private helpers that don't touch the
> buffer's API at all. Motions are not part of the 33.

---

## 2. Classification table

Destinations: **Buffer**, **Editor**, **Host**, **Engine-private**,
**Delete**.

| #  | Method                          | Destination          | Specific home                                  | Justification |
|----|---------------------------------|----------------------|------------------------------------------------|---------------|
| 1  | `lines`                         | Engine-private       | `pub(crate) fn lines<B: Query>(&B) -> Vec<&str>` helper, OR call sites rewritten to `(0..line_count).map(|r| line(r))` | All engine call sites are scaffolded "join lines into a string" or "snapshot to `Vec<String>`" — both expressible over `Query::line_count + Query::line`. Adding `lines()` to the trait would force every backend to materialize a `Vec<String>` slice borrow it doesn't own (rope-of-rope backends would have to allocate). Decompose at the call site. |
| 4  | `dirty_gen`                     | Buffer (sub-trait)   | `Query::dirty_gen(&self) -> u64`               | Render-cache key; consumers (host renderers) need a cheap "did the content move?" probe. Buffer owns content so it owns the gen counter. Already public, costless to publish. |
| 6  | `ensure_cursor_visible`         | Engine-private       | `pub(crate) fn ensure_cursor_visible<B,H>(b,h,…)` over `B: Cursor+Query, H: Host` | Pure scrolloff math; depends on `(cursor, line_count, viewport)` — all three already on the trait surface. Move the function into engine, drop the buffer-side method entirely. The wrap-aware variant currently lives on Buffer because it walks rope segments via `wrap_segments`; that helper stays in `hjkl-buffer::wrap` (free function, takes `&str`). |
| 7  | `cursor_screen_row`             | Engine-private       | `pub(crate) fn cursor_screen_row<B,H>(b,h) -> Option<usize>` | Same shape as #6 — formula over cursor + viewport + per-line wrap segmentation. Free function. |
| 8  | `screen_rows_between`           | Engine-private       | `pub(crate) fn screen_rows_between<B,H>(b,h,start,end) -> usize` | Same. |
| 9  | `max_top_for_height`            | Engine-private       | `pub(crate) fn max_top_for_height<B,H>(b,h,height) -> usize`  | Same. |
| 10 | `clamp_position`                | Engine-private       | `pub(crate) fn clamp_pos<B: Query>(b, Pos) -> Pos`            | Already a tiny helper used only at the buffer-impl boundary (`clamp_to_buf`). Keep it engine-side; takes `Query::line_count + Query::line` to compute. |
| 11 | `set_spans`                     | Host (trait)         | `Host::syntax_highlights(range) -> Vec<Highlight>` already exists; **delete the Buffer-side spans cache**. | Spans are derived from a host-supplied tree (tree-sitter, LSP). The host is the source of truth. Caching by row inside Buffer was a perf shortcut; the SPEC pipeline has the host return highlights for a viewport range each frame. The `Span` opaque-id intern table (`style_table`) lives on Editor; the Highlight pipeline goes through `Host::syntax_highlights`. |
| 12 | `replace_all`                   | Engine-private       | `pub(crate) fn replace_all<B: Query+BufferEdit>(b, &str)` (= `replace_range(0..end_of_buffer, text)`) | One-shot whole-buffer replacement is `BufferEdit::replace_range(Pos::ORIGIN..end, text)`. Engine-private wrapper because the call site (file load, `:%!cmd`) is convenient with an explicit name; no need to bloat the trait. |
| 13 | `marks`                         | Editor (state)       | `Editor::buffer_marks: BTreeMap<char, Pos>` (already partially exists as `vim.marks` for lowercase + `Editor::file_marks` for uppercase) | Marks are vim-FSM state: `m{a-z}` registers are scoped to the editor instance, not the buffer's content. Editor already owns `file_marks` for uppercase; lowercase marks live on `vim.marks`. Consolidate: single `marks: BTreeMap<char, Pos>` on Editor, mark-shift logic stays in Editor's edit pipeline (`shift_marks_after_edit`). Delete buffer-side `marks` storage. |
| 14 | `spans`                         | Delete               | n/a                                            | Read accessor for the same data that #11 publishes. With #11 deleted, this disappears too. Test-only call sites in `vim.rs:8200` etc. migrate to reading `Editor::take_render_frame().highlights` or a test fixture. |
| 15 | `as_string`                     | Engine-private       | `pub(crate) fn as_string<B: Query>(b) -> String` | Engine has no production call site — only test asserts. Either kept as a test helper or replaced with `Query::slice(Pos::ORIGIN..end)`. Definitely not on the trait. |
| 18 | `set_search_pattern`            | Editor (state)       | `Editor::search_state: SearchState`            | Last search pattern, wrap flag, and per-row match cache are FSM state — they survive across buffer mutations and drive `n`/`N`. Buffer is the wrong home: a host with two windows on the same buffer would leak the pattern between them. Move the entire `SearchState` (regex + cache + wrap flag) onto Editor. |
| 19 | `search_pattern`                | Editor (state)       | `Editor::search_state.pattern`                 | Same. |
| 20 | `set_search_wrap`               | Editor (state)       | `Editor::search_state.wrap_around` (or sourced from `Options::wrapscan`) | Same. Note: `wrapscan` is already a SPEC `Options` field — the runtime flag is derived. |
| 21 | `search_wraps`                  | Editor (state)       | `Editor::options().wrapscan`                   | Read accessor for the option. |
| 22 | `search_forward`                | Engine-private       | `pub(crate) fn search_forward<B: Cursor+Search>(b, &SearchState, skip_current) -> bool` | Composition of `Search::find_next` + `Cursor::set_cursor` + the wrap policy. Already expressible over the SPEC sub-traits; just needs to read pattern from `Editor::search_state` rather than `Buffer`. |
| 23 | `search_backward`               | Engine-private       | `pub(crate) fn search_backward<B: Cursor+Search>(b, &SearchState, skip_current) -> bool` | Same. |
| 24 | `search_matches`                | Engine-private       | `pub(crate) fn search_matches<B: Query>(b, &mut SearchState, row) -> Vec<(usize,usize)>` | Lazy per-row regex scan; the cache lives on `SearchState` (Editor-owned per #18). Engine routes the call. |
| 25 | `folds`                         | Engine-private       | `Editor::fold_provider: Box<dyn FoldProvider>` already exists. Folds storage is buffer-internal in 0.0.34; the **enumeration accessor** can stay engine-internal (snapshot path). | Fold *iteration* is on `FoldProvider` already. The full read accessor `&[Fold]` is only needed for snapshot — handled by Editor's snapshot fields (folds round-trip through `EditorSnapshot`). |
| 26 | `add_fold`                      | Host (trait)         | `Host::emit_intent(Self::Intent)` carries fold ops; per-host fold storage. **Engine no longer talks to fold storage directly.** | Fold ops fan out to the host: vim FSM raises `FoldOp::Add(start,end,closed)` (already enumerated in SPEC §"Host trait"); host implements (or ignores). For the in-tree default, `BufferFoldProvider` carries a `&mut Buffer` and applies the op locally — but this lives behind the Host adapter. |
| 27 | `remove_fold_at`                | Host (trait)         | Same — `FoldOp::RemoveAt(row)` intent          | Same. |
| 28 | `open_fold_at`                  | Host (trait)         | Same — `FoldOp::OpenAt(row)` intent            | Same. |
| 29 | `close_fold_at`                 | Host (trait)         | Same — `FoldOp::CloseAt(row)` intent           | Same. |
| 30 | `toggle_fold_at`                | Host (trait)         | Same — `FoldOp::ToggleAt(row)` intent          | Same. |
| 31 | `open_all_folds`                | Host (trait)         | Same — `FoldOp::OpenAll`                       | Same. |
| 32 | `close_all_folds`               | Host (trait)         | Same — `FoldOp::CloseAll`                      | Same. |
| 33 | `clear_all_folds`               | Host (trait)         | Same — `FoldOp::ClearAll`                      | Same. |
| 34 | `fold_at_row`                   | Host (trait)         | `FoldProvider::fold_at_row` (already on `FoldProvider`) | Already done in 0.0.32 (Patch C-β). |
| 35 | `is_row_hidden`                 | Host (trait)         | `FoldProvider::is_row_hidden` (already)        | Already done in 0.0.32. |
| 36 | `next_visible_row`              | Host (trait)         | `FoldProvider::next_visible_row` (already)     | Already done in 0.0.32. |
| 37 | `prev_visible_row`              | Host (trait)         | `FoldProvider::prev_visible_row` (already)     | Already done in 0.0.32. |
| 38 | `invalidate_folds_in_range`     | Host (trait)         | `FoldProvider::invalidate_range(lo, hi)` (new method on existing trait) | Edits drop folds inside the touched range. Today `Editor::mutate_edit` calls this directly on Buffer; route through `FoldProvider`. The default `BufferFoldProvider` forwards to the buffer; hosts with their own fold tree apply locally. |

---

### Notes on the "FoldOp via emit_intent" route

The fold ops (`add_fold`, `open_fold_at`, …) are listed as **Host (trait)**
because SPEC §"Host trait" already enumerates `FoldOp::{Open, Close,
ToggleAt(line)}` as a fan-out variant on `Host::Intent`. The engine emits
the intent; hosts that care about folds (sqeel-tui, buffr, inbx) implement
fold storage on their side, often delegating to a `BufferFoldProvider`
adapter wrapping the buffer.

This frees `hjkl-buffer` of fold ownership entirely — folds become
Host-side state — which neatly matches SPEC §"Out of scope" ("vim logic
must work in GUI hosts… runtime state is owned by the host"). The only
downside is hosts that don't want folds get one extra `FoldOp` enum
variant to ignore; the default `Intent = ()` route makes this trivial.

---

## 3. Per-bucket impact analysis

### 3.1 Buffer (sub-trait expansion)

Adding to the existing sub-traits:

- `Query::dirty_gen(&self) -> u64` (#4) — render-cache key.

That's it. **One method added to `Query`.**

The `find_next`/`find_prev` (already on `Search`) cover the read-side
search work. The buffer-internal `SearchState` cache is gone (moved to
Editor per #18-#21).

> An optional new helper trait `BufferStats` (`dirty_gen`, plus future
> `len_chars`, `grapheme_count`) is **not recommended** for 0.1.0 — single
> method doesn't justify a new sub-trait. Add to `Query` directly.

**Cap check:**

| Trait        | Before | After | Methods                                                           |
|--------------|--------|-------|-------------------------------------------------------------------|
| `Cursor`     | 4      | 4     | `cursor`, `set_cursor`, `byte_offset`, `pos_at_byte`              |
| `Query`      | 4      | 5     | + `dirty_gen`                                                     |
| `BufferEdit` | 3      | 3     | unchanged                                                          |
| `Search`     | 2      | 2     | unchanged                                                          |
| **Buffer**   | **13** | **14**| super-trait union — under cap (40)                                |

> §4 below shows the full surface text.

### 3.2 Editor (state migration)

Moves of *state* from Buffer to Editor:

| Field                                            | Source today                                | Notes |
|--------------------------------------------------|---------------------------------------------|-------|
| `search_state: SearchState`                      | `hjkl_buffer::Buffer::search` (private)     | Whole-cloth move; new module `engine::search_state`. Includes `pattern: Option<Regex>`, `matches: Vec<Vec<(usize,usize)>>`, `generations: Vec<u64>`, `wrap_around: bool`. |
| `marks: BTreeMap<char, Pos>`                     | `hjkl_buffer::Buffer::marks` + `Editor::vim.marks` + `Editor::file_marks` | Consolidate the three existing storages into one. Lowercase = buffer-scope-equivalent; uppercase = file-scope; both round-trip through `EditorSnapshot::marks`. |
| (no new field for spans)                         | n/a                                         | Spans gone (#11). The `style_table` intern table already lives on Editor. |
| (no new field for folds)                         | n/a                                         | Folds gone (delegated to host via `FoldProvider` + `Host::Intent::FoldOp`). |

Editor already owns: `vim`, `undo_stack`, `redo_stack`, `registers`,
`settings`, `file_marks`, `syntax_fold_ranges`, `change_log`, `sticky_col`,
`host`, `last_emitted_mode`. Two of those (`file_marks`, partial-`vim.marks`)
collapse into the new consolidated `marks`.

**Net field delta on Editor: +1 (`search_state`), −1 (`file_marks` merges
into `marks`), so +0 conceptual fields — but 4 distinct kinds of state move
in.**

Synchronization: Editor's edit pipeline (`mutate_edit`) is already the funnel
that updates `change_log`, `vim.last_edit_pos`, `vim.change_list`. It picks
up two new responsibilities:

1. Invalidate `search_state.matches[row]` for rows touched by the edit
   (cheap — same `dirty_gen`-style invalidation the buffer does today).
2. Shift `marks` whose row is at-or-after the edit's start row by the
   row delta the edit produced (already done for `file_marks` via
   `shift_marks_after_edit`; just generalize over the unified map).

### 3.3 Host (trait expansion)

New on `Host` (or on `FoldProvider`, which is plumbed through `Host` once
`Editor<B,H>` flips):

| Method                                          | Trait           | Default impl?                  | Hosts must provide |
|-------------------------------------------------|-----------------|--------------------------------|---------------------|
| `FoldProvider::invalidate_range(lo, hi)`        | `FoldProvider`  | `NoopFoldProvider` no-ops it   | sqeel, buffr (folds matter); inbx (no folds) uses default |
| `Host::Intent::FoldOp(FoldOp)` enum variant     | (associated type) | Hosts opt-in via Intent type | sqeel adds `FoldOp` to `SqeelIntent`; buffr/inbx ignore |

The engine raises **one `Host::emit_intent(FoldOp::…)` call per `z…`
keystroke**. Hosts that want vim folds wrap a `BufferFoldProvider` and
apply the op; hosts that don't (inbx) can ignore the variant.

**Consumer-side impact:**

- **sqeel-tui:** add `FoldOp` to `SqeelIntent`; in the dispatch loop,
  match `FoldOp::Open(row) => buffer_fold_provider.open_fold_at(row)`,
  etc. **~10 LOC.** Plus implement `FoldProvider::invalidate_range` on
  the in-tree adapter (forward to buffer). **~5 LOC.**

- **buffr-modal:** same shape as sqeel — add `FoldOp` to its intent, route
  to a `BufferFoldProvider`. **~10 LOC.**

- **inbx:** has no folds and no need for them. `Intent = ()` consumers can
  set `FoldOp` to a uninhabited type (or use SPEC's existing
  `Host::Intent` associated type to opt out entirely by not naming
  `FoldOp` in their intent enum). **0 LOC** if `FoldOp` lives in a
  per-host Intent.

> Open question 8.2 below: should `FoldOp` live in *engine* (canonical
> shape, every host names it) or in *each host* (hosts that don't care
> never see it)? Recommendation: engine ships a canonical `FoldOp` enum,
> hosts that care embed it in their Intent variant, hosts that don't
> ignore it.

No new `Host`-level methods (the existing `viewport` / `viewport_mut` /
`syntax_highlights` / `emit_intent` / `emit_cursor_shape` cover everything).
The viewport/syntax pipeline already lives there.

### 3.4 Engine-private helpers

The bulk of the 33 — **16 methods** become free functions inside
`hjkl-engine/src/{viewport_math,search,buffer_helpers}.rs` (new modules).
Categorized:

**Viewport / scrolloff math (`viewport_math.rs`):**
- `ensure_cursor_visible` (#6)
- `cursor_screen_row` (#7)
- `screen_rows_between` (#8)
- `max_top_for_height` (#9)

These four are pure rope-walking + wrap-segment math over `(B: Cursor +
Query, H: Host)`. They consume `Host::viewport_mut` for write-back. The
rope-segmentation primitive (`hjkl_buffer::wrap::wrap_segments`) is already
a free function and stays in `hjkl-buffer`.

**Search execution (`search.rs` inside engine, distinct from
`hjkl-buffer::search`):**
- `search_forward` (#22)
- `search_backward` (#23)
- `search_matches` (#24)

Composition over `B: Search` + `&mut SearchState` (which now lives on
Editor).

**Buffer helpers (`buffer_helpers.rs`):**
- `lines` (#1) — small wrapper or call-site rewrite
- `clamp_position` (#10)
- `replace_all` (#12)
- `as_string` (#15)

**Fold storage adapter (in `buffer_impl.rs`, already exists):**
- `BufferFoldProvider::invalidate_range(lo, hi)` (#38) — new method on
  the existing adapter.

That's 13 explicit; the count of 16 also includes the in-place
`BufferFoldProvider` shims for the seven fold-mutation methods (#26-#33)
that it forwards from `Host::emit_intent(FoldOp::…)` to the buffer's
inherent fold methods. Those buffer-inherent methods don't disappear —
they just stop being on a public trait surface. They remain on
`hjkl_buffer::Buffer` as `pub fn`s for the host adapter to call.

### 3.5 Delete

- `spans` (#14) — dies with #11. Test sites switch to reading the engine
  render frame's `highlights` field.
- `marks` (#13 — the *Buffer-side* storage) dies. Editor takes over.

(Listing them as separate "Delete" entries is a slight overcount; the
inventory entry "Buffer-side spans" effectively covers two methods —
`set_spans` and `spans`. The classification table treats them
individually because they're separate API surface area.)

---

## 4. Updated SPEC trait surface

```rust
trait Cursor {                              // 4 methods (unchanged)
    fn cursor(&self) -> Pos;
    fn set_cursor(&mut self, pos: Pos);
    fn byte_offset(&self, pos: Pos) -> usize;
    fn pos_at_byte(&self, byte: usize) -> Pos;
}

trait Query {                               // 5 methods (was 4; +dirty_gen)
    fn line_count(&self) -> u32;
    fn line(&self, idx: u32) -> &str;
    fn len_bytes(&self) -> usize;
    fn slice(&self, range: Range<Pos>) -> Cow<'_, str>;
    fn dirty_gen(&self) -> u64;             // NEW
}

trait BufferEdit {                          // 3 methods (unchanged)
    fn insert_at(&mut self, pos: Pos, text: &str);
    fn delete_range(&mut self, range: Range<Pos>);
    fn replace_range(&mut self, range: Range<Pos>, replacement: &str);
}

trait Search {                              // 2 methods (unchanged)
    fn find_next(&self, from: Pos, pat: &Regex) -> Option<Range<Pos>>;
    fn find_prev(&self, from: Pos, pat: &Regex) -> Option<Range<Pos>>;
}

trait Buffer:                               // super-trait union: 14 methods
    Cursor + Query + BufferEdit + Search + Sealed + Send {}

// Total Buffer surface: 14 methods (was 13). Cap is 40. Headroom: 26.

trait Host {                                // 11 methods (unchanged shape)
    type Intent;
    fn write_clipboard(&mut self, text: String);
    fn read_clipboard(&mut self) -> Option<String>;
    fn now(&self) -> core::time::Duration;
    fn should_cancel(&self) -> bool { false }
    fn prompt_search(&mut self) -> Option<String>;
    fn display_line_for(&self, pos: Pos) -> u32 { pos.line }
    fn pos_for_display(&self, line: u32, col: u32) -> Pos { Pos { line, col } }
    fn syntax_highlights(&self, range: Range<Pos>) -> Vec<Highlight> { vec![] }
    fn emit_cursor_shape(&mut self, shape: CursorShape);
    fn viewport(&self) -> &Viewport;
    fn viewport_mut(&mut self) -> &mut Viewport;
    fn emit_intent(&mut self, intent: Self::Intent);
}

trait FoldProvider {                        // 5 methods (was 4; +invalidate_range)
    fn next_visible_row(&self, row: usize, row_count: usize) -> Option<usize>;
    fn prev_visible_row(&self, row: usize) -> Option<usize>;
    fn is_row_hidden(&self, row: usize) -> bool;
    fn fold_at_row(&self, row: usize) -> Option<(usize, usize, bool)>;
    fn invalidate_range(&mut self, start_row: usize, end_row: usize);  // NEW
}
```

**No new sub-traits required.** Single-method additions to `Query` and
`FoldProvider`.

---

## 5. Editor field-state additions

```rust
pub struct Editor<'a, B: Buffer, H: Host> {
    // … existing fields kept …

    /// Search state (pattern + per-row match cache + wrap flag).
    /// Was `hjkl_buffer::Buffer::search` (private) — promoted to
    /// engine-FSM ownership so multi-window hosts don't share a
    /// pattern across panes on the same buffer.
    pub(crate) search_state: SearchState,           // NEW (moved)

    /// Vim marks `m{a-z}` (lowercase, buffer-scoped) +
    /// `m{A-Z}` (uppercase, file-scoped, survive `set_content`).
    /// Consolidates today's `vim.marks` + `Editor::file_marks` +
    /// `hjkl_buffer::Buffer::marks` into one map.
    pub(crate) marks: BTreeMap<char, Pos>,          // NEW (consolidated)

    // … existing `host`, `vim`, `registers`, etc. unchanged …
}

pub(crate) struct SearchState {
    pub pattern: Option<regex::Regex>,
    pub matches: Vec<Vec<(usize, usize)>>,  // matches[row] = [(byte_start, byte_end)]
    pub generations: Vec<u64>,              // matches[row] cached at this dirty_gen
    pub wrap_around: bool,                  // = options.wrapscan at write time
}
```

Lifetimes: both fields live `'static` relative to the Editor struct itself
— neither borrows from the buffer. `marks` and `search_state` round-trip
through `EditorSnapshot` so the wire format gets one bump for v0.1.0
freeze (snapshot v4: adds `marks`, `search_pattern_text`).

---

## 6. Consumer impact

### sqeel (uses folds, search, buffer)

- `SqeelIntent` gains a `FoldOp(FoldOp)` variant, where `FoldOp` is the
  engine-shipped enum from §3.3. Match arm in the host event loop:
  ```rust
  SqeelIntent::FoldOp(op) => self.fold_provider.apply(op),
  ```
- `SqeelIntent` already owns LSP variants per SPEC; the new variant slots
  in alongside.
- Constructor: `Editor::new(buffer, host, options)` per SPEC §"Editor
  surface". sqeel currently uses the legacy `Editor::new(KeybindingMode)`;
  it'll switch to the SPEC constructor in the same patch that flips
  `Editor` generic.
- `BufferFoldProvider` adapter (already in `crate::buffer_impl`) gains
  `invalidate_range`. sqeel uses it as-is.
- Search: search-prompt logic on sqeel-tui's host already implements
  `Host::prompt_search`. No change there. The Editor-side search-state
  migration is invisible to sqeel.

**Estimated sqeel patch size: ~30 LOC** (host-Intent enum + match arm +
constructor switch).

### buffr-modal (modal editor on top of buffr)

- Same shape as sqeel for folds. Adds `FoldOp` to its intent enum.
- Constructor: same SPEC constructor switch.
- buffr-modal already implements the full SPEC `Host` trait per
  `crate::types::Host` doc comment. No new methods to add.

**Estimated buffr patch size: ~25 LOC.**

### inbx (no folds; mail UI)

- Doesn't currently embed `hjkl-engine`'s vim FSM (uses `hjkl-buffer`
  alone for read-only buffer ops). If/when it adopts the engine, it can
  pick `Intent = ()` and ignore folds entirely. Until then, no impact.

**Estimated inbx patch size: 0 LOC.**

---

## 7. Phasing recommendation

A 5-step rollout. Each step lands as a single 0.0.x patch with green
tests; the 0.1.0 cut is the final one.

### Step 1 (0.0.35): Search state migration

- Add `Editor::search_state: SearchState`.
- Add `pub(crate)` engine-private `search_forward` / `search_backward` /
  `search_matches` free functions over `(B: Cursor+Search+Query, &mut
  SearchState)`.
- Migrate `vim.rs` callers from `ed.buffer_mut().search_forward(true)` to
  `engine::search::search_forward(ed.buffer_mut(), &mut ed.search_state,
  true)` (or analogous).
- Mark `hjkl_buffer::Buffer::{search_forward, search_backward,
  search_matches, set_search_pattern, search_pattern, set_search_wrap,
  search_wraps}` `#[deprecated(note = "moved to engine; will be removed
  in 0.1.0")]`. They stay alive but unused.
- `EditorSnapshot::VERSION` → 4 (carries `search_pattern_text`).

**Risk:** medium. Touches FSM control-flow. Test surface: every `/`,
`?`, `n`, `N`, `*`, `#` keystroke. Existing 448 tests cover the matrix.

### Step 2 (0.0.36): Marks consolidation

- Add `Editor::marks: BTreeMap<char, Pos>`.
- Migrate `vim.marks` (lowercase) + `Editor::file_marks` (uppercase) +
  read accessors that today reach `Buffer::marks` to the consolidated
  field.
- `EditorSnapshot::VERSION` → 5 (carries unified `marks`).

**Risk:** low. Marks are read/write-isolated; no FSM control-flow changes.

### Step 3 (0.0.37): Spans → Host pipeline

- Delete `Buffer::set_spans` / `Buffer::spans` from the inherent impl.
- All call sites in `Editor::install_syntax_spans` /
  `install_ratatui_syntax_spans` rewrite to populate the engine's
  highlight pipeline directly (the existing `highlights_for_line` already
  composes search + spans; spans get sourced from
  `Host::syntax_highlights` going forward).
- Test fixtures in `vim.rs:8200` switch to reading
  `Editor::render_frame()` highlights or stubbing
  `Host::syntax_highlights` directly.

**Risk:** medium. Touches the syntax-highlight side of render. Done
behind the existing `ratatui` feature flag — non-ratatui hosts are
already on the engine-native path.

### Step 4 (0.0.38): Folds → Host intent

- Add canonical `FoldOp` enum to `crate::types`.
- Engine raises `host.emit_intent(FoldOp::…)` instead of calling
  `buffer_mut().{open,close,toggle,…}_fold_at`.
- Add `FoldProvider::invalidate_range`.
- Migrate `Editor::mutate_edit`'s call to `buffer.invalidate_folds_in_range`
  → `fold_provider.invalidate_range`.
- Hosts (sqeel, buffr) update their Intent enums + dispatch.
- Default `NoopFoldProvider` no-ops `invalidate_range`.

**Risk:** medium. Coordinated change with downstream consumers. Land in
sqeel/buffr in the same release train.

### Step 5 (0.1.0): The flip

- Add `Query::dirty_gen` (one method).
- Drop `EngineHost` shim trait.
- Flip `Editor` to `Editor<'a, B: Buffer, H: Host>` per SPEC §"Editor
  surface".
- Replace internal calls to engine-private helpers `lines` / `as_string`
  / `replace_all` / `clamp_position` / `ensure_cursor_visible` /
  `cursor_screen_row` / `screen_rows_between` / `max_top_for_height`
  with their free-function homes.
- Delete the now-unused `hjkl_buffer::Buffer` inherent methods that are
  no longer reached: `set_spans`, `spans`, `marks`,
  `search_*` (still callable from `hjkl-buffer` users; non-engine
  consumers retain the API but engine doesn't depend on it).
- Cut the 0.1.0 release. Seal the trait surface per SPEC §"Stability
  commitments".

**Risk:** high. Generic flip is the largest LOC change of the five. By
Step 5 every other migration is done, so the `Editor::buffer` field can
go from `hjkl_buffer::Buffer` to `B: Buffer` mechanically.

---

## 8. Open design questions

### 8.1 Should `dirty_gen` be on `Query` or on a new helper trait?

Recommendation: **on `Query`** (cheap, one method, every backend trivially
provides it). New `BufferStats` sub-trait is overkill for one method and
adds a super-trait constraint to `Buffer`.

Risk if wrong: minor — adding a new sub-trait pre-1.0 is allowed.

### 8.2 Should `FoldOp` live in engine or in each host's Intent?

Recommendation: **engine ships a canonical `FoldOp` enum in
`crate::types::FoldOp`.** Hosts embed it in their Intent enum (variant
named `FoldOp(FoldOp)`); hosts that don't care never name it. Keeps the
fold semantics centrally documented.

Alternative: leave it entirely up to hosts. Rejected because folds are
vim-shaped — the FSM raises specific ops (`zo`/`zc`/`za`/`zR`/`zM`/`zE`)
that need a stable enum.

### 8.3 Where does the `BufferFoldProvider`'s mutability boundary live?

In Step 4, the engine raises `FoldOp::Open(row)` via `host.emit_intent`.
The host then needs to mutate fold state — but the host **already holds
`&mut self`** during the dispatch loop. The natural shape is:

```rust
impl Host for SqeelHost {
    fn emit_intent(&mut self, intent: SqeelIntent) {
        match intent {
            SqeelIntent::FoldOp(op) => self.fold_provider.apply(op),
            // ...
        }
    }
}
```

This works as long as `FoldProvider` mutating methods (`apply(FoldOp)` or
direct `open_fold_at(row)`) are on the trait. Today `FoldProvider` is
**read-only**. Step 4 needs to add either:

a. `FoldProvider::apply(&mut self, FoldOp)` — single dispatch entry.
b. Six methods: `open_fold_at`, `close_fold_at`, `toggle_fold_at`, etc.

Recommendation: **(a) `apply(FoldOp)`.** Smaller surface, easier to
extend. The match-on-FoldOp lives inside the host's adapter, where it
has access to whatever fold storage shape it uses.

### 8.4 Search-state cache: bytes or chars?

Today `hjkl_buffer::SearchState::matches[row]` stores `(byte_start,
byte_end)` per match. Engine-side, the SPEC `Search::find_next` returns
`Range<Pos>` (chars/graphemes). Migrating the cache means a per-row
char⇄byte conversion or storing both indexings.

Recommendation: **store byte ranges** (matches the `regex` crate's
output, no extra work) and **convert at read time** in
`highlights_for_line` (where the conversion already happens). This
preserves the regex engine's native indexing through the cache and
avoids double-counting in `dirty_gen` mismatch detection.

### 8.5 Does `apply_edit`'s inverse return value need a SPEC home?

`Buffer::apply_edit` returns the inverse `Edit` for the host to push onto
an undo stack. SPEC's `BufferEdit` doesn't surface this — engine's
`Editor::mutate_edit` *itself* returns the inverse. Recommendation:
**leave `BufferEdit` as the imperative trio (insert/delete/replace)**;
the inverse computation lives in `Editor::mutate_edit`, which can synthesize
the inverse from the `Edit` it receives (or call `Buffer::apply_edit`
internally on the canonical impl). Buffers that want a faster inverse
path can override at the inherent-impl level.

This is the cleanest split: `BufferEdit` describes "I can mutate"; the
undo bookkeeping is pure engine.

### 8.6 What happens to the `lines` method's borrow shape?

Today `Buffer::lines() -> &[String]` returns a slice of owned strings.
Some engine call sites (`editor.rs:1703`'s snapshot path, `editor.rs:1341`'s
content-arc path) genuinely need to clone or borrow N lines as one
contiguous slice. `Query::line(idx)` returns `&str` per-line; iterating
allocates an iterator each time but doesn't allocate text.

Recommendation: **rewrite call sites to iterate
`(0..line_count).map(line)`**. The few perf-sensitive call sites
(snapshot, content-arc) can `collect::<Vec<&str>>()` once and bypass
ownership entirely. No need to add `lines()` to the trait; that would
foreclose backends that don't have a flat `Vec<String>` storage.

---

## Appendix A: Call-site summary

| Method                       | Engine call sites |
|------------------------------|-------------------|
| `cursor`                     | ~20               |
| `set_cursor`                 | ~22               |
| `lines`                      | 9                 |
| `line`                       | 8                 |
| `row_count`                  | ~12               |
| `apply_edit`                 | 4                 |
| `cursor_screen_row`          | 2                 |
| `max_top_for_height`         | 1                 |
| `set_spans`                  | 2                 |
| `replace_all`                | 2                 |
| `search_pattern`             | 1                 |
| `search_forward`/`backward`  | 6                 |
| Fold mutators (#26-#33)      | 9                 |
| `invalidate_folds_in_range`  | 1                 |
| `next_visible_row` (provider)| 1                 |
| `prev_visible_row` (provider)| 1                 |
| `fold_at_row`     (provider) | 1                 |
| `ensure_cursor_visible`      | 1                 |

Total individual call sites touched: ~95. The bulk is in `editor.rs`;
`vim.rs` contributes ~30 (mostly `set_cursor` and the fold mutators).

---

## Appendix B: Why this passes the SPEC §"<40-method cap" test

SPEC §"`Buffer` trait surface" caps the union `Cursor + Query +
BufferEdit + Search` at 40 methods (rationale: keep the seal-impl
contract small enough that pre-1.0 patch-bump trait-surface evolution
stays tractable for downstream consumers).

Today: 13 methods.
After this proposal: 14 methods (one new method on `Query`).
Headroom: 26 methods.

The proposal **leans on the headroom only once** (`dirty_gen`). Every
other "33-method" migration goes to Editor / Host / engine-private —
exactly because those are the architectural homes of that data. The cap
is preserved by *not* trying to keep all 33 on Buffer; it's preserved by
recognizing that most weren't Buffer's responsibility to begin with.
