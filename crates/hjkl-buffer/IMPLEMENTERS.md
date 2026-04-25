# hjkl-buffer — Implementer Notes

This document is for **callers of `hjkl_buffer::Buffer` and adjacent types**.
Required reading before relying on the API in production. Once phase 5 trait
extraction lands, the same invariants apply to anyone implementing the `Buffer`
trait directly.

> Pre-1.0: signatures may shift between patch versions. Invariants below are the
> load-bearing semantics — they will not silently change without a CHANGELOG
> entry and a deliberate version bump.

## Position semantics

[`Position`] is `(row, col)` where:

- `row` is zero-based, in **logical lines** (newline-separated). Wrapping is a
  render-only concern; no `Position` ever points at a display line.
- `col` is zero-based, **byte index within the line's UTF-8 string** — not
  graphemes, not display columns. Width-aware motions go through helpers in
  `motion.rs`; do not synthesize `Position` from a column count without
  consulting them.

### Bounds

A `Position` is **valid** for a buffer iff:

- `row < buffer.lines().len()`
- `col <= buffer.line(row).unwrap().len()` (one past EOL is allowed — insert
  mode lives there).

Pass an out-of-bounds `Position` to `set_cursor` and the buffer clamps to the
nearest valid one via `clamp_position`. Pass one to `apply_edit` and the edit is
rejected (returns the no-op inverse).

### Sticky column

`Buffer` tracks an optional sticky column for `j` / `k` motions: the target
column to land in once the cursor reaches a line long enough to honor it. Never
reset it manually outside motion code — it survives `set_cursor` for that exact
reason.

## Edit invariants

`apply_edit` is the **only** way to mutate buffer text. It returns the inverse
`Edit` so the caller can push to an undo stack.

Invariants you must hold when constructing an `Edit`:

- `InsertChar { at, ch }`: `at` valid; `ch` is a single Unicode scalar.
  Multi-grapheme content must use `InsertStr`.
- `InsertStr { at, text }`: `at` valid. `text` may contain `\n` — the buffer
  splits on newline. CR (`\r`) is preserved as-is; the host is responsible for
  CRLF normalization before insert.
- `DeleteRange { start, end, kind }`: `start <= end` in document order. `kind`
  (`Char` / `Line`) controls whether trailing newlines are consumed:
  - `Char`: byte-precise; preserves enclosing newlines.
  - `Line`: extends `end` to include the line's trailing `\n` so a full-line
    delete leaves no orphan blank line.
- `JoinLines { row, count, with_space }`: `row + count - 1` must be a valid row.
  `count >= 1`.
- `Replace`: composed delete + insert; same constraints apply per part.

After `apply_edit`:

- `dirty_gen()` is incremented exactly once.
- The cursor is repositioned to a sensible place for the edit kind (insert lands
  past the inserted content; delete lands at the start). Callers that need to
  override the new cursor must `set_cursor` immediately after.
- All `Position`s the caller is holding from before the edit may be invalid.
  Re-derive from row / col deltas, or from a mark; do not cache.

## Marks

Lowercase marks (`'a`–`'z`) are per-buffer; uppercase marks (`'A`–`'Z`) are
global and live outside the buffer (host responsibility).

When `apply_edit` shifts text, lowercase marks track the edit:

- Insert at a position before a mark advances the mark by the inserted length.
- Delete a range strictly before a mark advances by the deleted length
  (negative).
- Delete a range that covers a mark removes the mark.

This is the only state cluster that survives raw text mutation — syntax spans,
search matches, and cursor screen rows are all recomputed lazily.

## Folds

Folds are **byte-range** spans, not row spans. `Fold { start, end }` covers
`[start, end]` inclusive. Host renders folds as collapsed single-line stubs; the
buffer never elides them on its own — `lines()` always returns the underlying
logical text.

Add / remove / toggle goes through `add_fold` / `remove_fold_at` /
`toggle_fold_at`. Open-all / close-all (`zR` / `zM`) modify a separate "open"
set; folds keep their definitions.

## Viewport

`Viewport { top, height, wrap, scroll_off }` is an **input** to
`ensure_cursor_visible`, not a derived value. The host writes `top` and `height`
per render frame; the buffer clamps the cursor inside.

`scroll_off` is honored after `ensure_cursor_visible` runs; it is not a hard
constraint at smaller heights.

`Wrap::None` / `Wrap::Char` / `Wrap::Word` change which screen-row arithmetic
the buffer uses. Switching mid-session is supported but the host must call
`ensure_cursor_visible` afterwards.

## Search

`set_search_pattern(Some(regex))` arms the search; `search_forward` /
`search_backward` advance the cursor to the next / previous match.
`skip_current` means "if the cursor is currently inside a match, treat it as
not-current and find the one strictly after / before".

Wraparound: the buffer wraps automatically. There is no `Options` flag here yet
— the engine layer's `:set wrapscan` controls behavior once it ships against
this API.

`search_matches(row)` returns all matches on a single row, used by the render
path for `'hlsearch'`-style highlighting. Cheap when called with the same `row`
repeatedly within the same `dirty_gen`.

## Spans

Style spans are opaque-id tuples: `Span { start, end, style: u32 }`. The buffer
does not own colors. The host (engine layer or terminal frontend) keeps the
table mapping `style: u32` to a renderable type.

`set_spans(rows)` replaces all spans for the affected rows in a single call.
Spans are cleared on `apply_edit` for the affected row(s) and must be
re-installed by the host's syntax pipeline.

## Render path (ratatui feature)

When the `ratatui` feature is enabled, `BufferView` implements
`ratatui::widgets::Widget`. The widget is **single-pass** — text, selection,
gutter signs, and styled spans all paint together. There is no separate
`Paragraph` or layout step.

`StyleResolver` hooks: `Selection`, `SearchMatch`, `IncSearch`, `MatchParen`,
plus an opaque `Syntax(u32)` lookup. Implement against your own theme.

## Testing your `Buffer` use

Property tests are encouraged for any non-trivial caller. The crate ships its
own test suite; reuse `Buffer::from_str` to construct fixtures from inline
strings.

Things worth proving:

- After any sequence of valid edits + their inverses, the buffer returns to its
  original `lines()`.
- For any valid `Position` and motion call, the resulting cursor is itself
  valid.
- `dirty_gen()` strictly increases across mutations and stays constant across
  read-only queries.

## Why so many invariants?

Most of them follow from one rule: **the engine layer treats
`hjkl_buffer::Buffer` as the source of truth for text content**. Any divergence
between cached state (engine-side selections, undo stacks, search matches) and
the buffer's `lines()` is a bug. The invariants above are the contract that lets
the engine cache aggressively without risking that divergence.

Open issues: <https://github.com/kryptic-sh/hjkl/issues>.
