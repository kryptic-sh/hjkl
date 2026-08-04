# Performance Review — 2026-08-04

Scope: full codebase (clean working tree at the review point). Findings are
ranked by impact; each was verified by tracing the caller to a named frequency
and re-reading the cited lines. Depth: low — high-confidence, hot-path findings
only. This is a report only; no code was changed.

## Findings (worst first)

### 1. Wrapped scrolloff is O(n²) per keystroke — `crates/hjkl-engine/src/editor.rs:4241-4268`

`ensure_scrolloff_vertical`'s step-2 loop recomputes `cursor_screen_row_from`
from the current `top_row` on every iteration, then advances `top_row` by one
visible row. Each call is O(distance) (a `String` alloc + `wrap_segments` per
row). A big soft-wrapped jump (`G`, `gg`, large `j`) is O(distance²) — a
10k-screen-row wrapped buffer is ~10⁸ row-walks per keystroke: a hang.

The O(distance) fix already exists, in the wrong place: `viewport_math.rs:61-91`
(`ensure_cursor_visible`) documents exactly this incremental-delta approach
("This is O(distance) rather than recomputing … which made a big soft-wrapped
jump O(distance²)") — but that function is only reached on the `height == 0`
fallback (editor.rs:4097-4111). `ensure_scrolloff_vertical` never uses it.

**Fix:** port the incremental-delta walk (subtract each dropped row's
`wrap_segments` length from `screen` once, advance `top_row` by visible rows)
into `ensure_scrolloff_vertical`'s step-2/step-3 loops (editor.rs:4241, 4271),
instead of re-running `cursor_screen_row_from` per step.

### 2. `n`/`N`/`/`/`?` allocate one String per row scanned — `crates/hjkl-engine/src/buffer_impl.rs:325, 377`

`Search::find_next` / `find_prev` loop rows calling `rope_line_str(&rope, row)`
(one heap alloc per row) then `pat.find(&line)`. `n` on a 100k-line buffer with
a match near the end = 100k allocations per keystroke. Hot: every search repeat,
`*`, and the ex `/pat/` address.

**Fix:** run the regex once over the already-cached joined document
(`Buffer::content_joined`, `buffer.rs:401`, cached per `dirty_gen`) with
`find_at`, or iterate rope byte-slices. Side benefit: enables cross-line
matching, which per-line scanning can't do.

### 3. Sentence/tag text objects materialize the whole buffer per keystroke — `crates/hjkl-vim/src/vim/text_object.rs:308, 441, 75-77`

`let mut chars: Vec<char> = rope.chars().collect();` flattens the entire
document on every `is`/`as` (line 308) and `it`/`at` (line 441).
`sentence_boundary` (line 75-77) builds `Vec<Vec<char>>` — a String AND a
`Vec<char>` per row, whole buffer. A 100k-line file makes `(`/`)`/`is`/`it` a
multi-second stall and a tens-of-MB allocation per keypress.

**Fix:** scan directionally from the cursor with lazy per-row reads and
early-exit at the first boundary; use `rope.line(r)` slices (ropey `RopeSlice`
derefs to `&str`, no alloc) instead of `rope_line_to_str`.

### 4. Picker re-scores every candidate per keystroke (and per frame while streaming) — `crates/hjkl-picker/src/picker.rs:293-323`

Each keystroke re-lowercases the query, then for every candidate allocates
`(m_lower, index_map)` + a `Vec<usize>` positions tuple, scores, and sorts
`count` candidates, truncating to 500. While 50k candidates stream in this can
re-run per frame. Hot: every picker query keystroke with large candidate sets.

**Fix:** cache the lowercase+index-map per candidate (invalidate on source
change, not per query); score only the delta or a bounded window; reuse the
`scored` Vec capacity across keystrokes.

### 5. Per-row line String + wrap recompute in every screen-row walk — `crates/hjkl-engine/src/viewport_math.rs:86,128,174`; `crates/hjkl-buffer/src/buffer.rs:252-253,287-288,311-312,457-458`

`cursor_screen_row_from`, `max_top_for_height`, `ensure_cursor_visible` and the
buffer-side twins each call `Query::line` (String alloc per row) then
`wrap_segments` per row. `cursor_screen_row_from` runs per render frame for
cursor-block placement and per keystroke from the scrolloff walk;
`max_top_for_height` walks from the last row upward on every wrapped/folded
scroll. Under wrap that's O(visible distance) allocations per frame/keystroke.

**Fix:** borrow `rope.line(r)` slices instead of `Query::line`; hoist per-row
wrap heights into a `dirty_gen`-keyed row cache (same shape as
`SearchState::matches`), since wrap heights only change when a row's text
changes. Trade: memory for speed.

### 6. `buffer-tui` re-scans fold/sign lists per row in every frame — `crates/hjkl-buffer-tui/src/render.rs:626,630,920,925,462,1233`

The render loop calls `folds.iter().any(|f| f.hides(doc_row))` (and the sign
list) per row; with F folds that's O(rows×F) per frame across both the main pass
and the indent-guide pass. Hot: every frame in a folded buffer.

**Fix:** sort folds once and use a per-frame interval index, or precompute the
hidden set for the visible range in one pass.

### 7. Comment-marker pass materializes a ~100KB String and backward-scans up to 500 lines per recompute — `crates/hjkl-bonsai/src/comment_markers.rs:392`

`apply_rope` builds `window_str: String` (up to `CAP * 200` ≈ 100KB) plus runs
`seed_active`'s 500-line backward scan on every keystroke recompute. Hot: each
edit in a comment-heavy file re-runs the pass.

**Fix:** cap the seed scan lazily (stop at the first marker found, don't always
walk the full window); materialize a smaller window (proportional to the
comment's own extent, not the fixed 100KB cap).

### 8. Full-document String per expr-fold pass — `crates/hjkl-bonsai/src/folds.rs:440`

`let text = rope.to_string();` materializes the whole document for the fold
query per reparse (per edit). The comment notes it's "O(N) once per reparse (not
per frame)", which is true, but on a 100k-line file that's a 1MB+ String per
keystroke, and it's needed for the injection query AND every region slice.

**Fix:** keep the full materialization (tree-sitter needs contiguous text) but
reuse the buffer across calls keyed by `dirty_gen`, or slice per-region from a
single materialization. Lower priority than the per-frame items.

### 9. Event loop draws on every 120ms idle poll with no needs-draw flag — `apps/hjkl/src/app/event_loop.rs:1981`

`terminal.draw(...)` runs unconditionally on each poll wake, even with no state
change, purely to service the 120ms timeout (blame debounce, start-screen
expiry). Idle CPU burn per second on an untouched editor.

**Fix:** track a dirty flag (set on input, cursor move, async result) and skip
`terminal.draw` when nothing changed, keeping only the timeout-driven
bookkeeping.

### 10. Diag lists scanned 3× per window per frame + Vec alloc — `apps/hjkl/src/render.rs:199-232, 298-306, 399-419`

The diag overlay lists are re-filtered/re-scanned multiple times per window per
frame with fresh allocations. Hot: every frame in a buffer with diagnostics.

**Fix:** precompute a per-window per-frame sorted overlay list once and pass it
by reference to the passes.

### Minor (one line each)

- `hjkl-engine/src/motions.rs:1056` — `LineCache::char_at` does
  `.chars().nth(pos.col)` (O(col) re-decode) on every char step of `w`/`b`/`e`;
  a `w` across a 10k-char minified line is ~50M decodes. Keep a per-line char
  cursor in the cache.
- `hjkl-engine/src/buffer_impl.rs` `Search::find_*` — see #2; the row-loop is
  the same allocation pattern.
- `hjkl-engine/src/substitute.rs:505` — `collect_substitute_matches` allocates a
  String per row across the range (100k allocs for `:%s///g`); per-command, so
  lower impact.
- `hjkl-engine/src/motions.rs:281,350,369,668,691` — `{`/`}`/`[[`/`]]`/`%` do a
  String/Vec alloc per row scanned; use `line_bytes(row) == 0` for the emptiness
  test (the codebase already does this at `content_row_count`, motions.rs:102)
  and rope-slice borrows for `%`.
- `hjkl-engine/src/motions.rs:876-931` — `gj`/`gk` re-wrap the whole line per
  segment step; compute `wrap_segments` once per row for the motion.
- `hjkl-buffer/src/buffer.rs` `buf_line_chars` (buf_helpers.rs:85-87) allocates
  a full line String just to `.chars().count()`; called 3× per `j`/`k`.
- `hjkl-engine/src/search.rs:613` — `search_matches(...).to_vec()` clones the
  cached Vec per visible row per frame; return a borrow.
- `crates/hjkl-buffer-tui/src/render.rs:1544-1548` — O(cells×matches) search-hit
  test per frame.
- `crates/hjkl-buffer-tui/src/render.rs:1740-1770` — per-cell span filter+sort
  per frame.
- `crates/hjkl-completion/src/lib.rs:155-176` — per-keystroke re-lowercase +
  `Vec<char>` per completion item.

## Cleared

- `SearchState::matches` invalidates all rows on any edit (single `dirty_gen`),
  so each insert keystroke re-scans the viewport — confirmed viewport-bounded,
  acceptable.
- Undo tree delta+keyframe replay, `content_joined`/`byte_len` caches,
  `wrap_segments` scratch reuse, `line_bytes`-based row-count clamp, and the
  single-threaded (non-contended) locks — all confirmed fine.

## Coverage

Reviewed: `hjkl-buffer` (content, wrap, buffer, edit), `hjkl-engine`
(buffer_impl, buf_helpers, cursor_move, search, motions, editor, viewport_math,
substitute, registers, rope_util), `hjkl-vim` (text_object, step, editor_ext,
insert_ops, motion grep), `hjkl-vim-tui`, `hjkl-buffer-tui` (render),
`hjkl-bonsai` (comment_markers, folds, hex_color, rope_slice, highlighter),
`apps/hjkl` (event_loop, render, app core), `hjkl-picker`, `hjkl-completion`,
`hjkl-fs`, `hjkl-fs-watch`, `hjkl-fuzzy`, `hjkl-lsp`, `hjkl-ex`, `hjkl-keymap`,
`hjkl-css`, `hjkl-markdown`, `hjkl-markdown-tui`, `hjkl-layout`, `hjkl-syntax`,
`hjkl-clipboard` (core only), `hjkl-engine-tui`, `hjkl-editor-tui`,
`hjkl-syntax-tui`.

Not reviewed (GAPs): `hjkl-clipboard/backend/*` (cold user-invoked path, read
only to confirm it's off the render/keystroke paths),
`apps/hjkl/src/app/ explorer.rs` (4.5k lines, read via its render-cache entry
point only), the remaining `hjkl-buffer` modules (folds, geom, listchars,
motion, search, selection, span, engine_types), and the unread `hjkl-engine`
modules (abbrev, discipline, input, keymap_motion, options_registry, policy,
selection_shift, tag). All `tests/` directories excluded. None of the cost
figures were measured with a profiler — the frequencies and sizes are traced
from the code, and the worst offenders (items 1, 2, 3, 5) would each benefit
from a profile confirmation before the fix is sized.
