# Code Review — 2026-08-04

Scope: full codebase (working tree was clean; nothing pending). Depth: low —
high-confidence findings only, each verified by tracing against the real code
(and, where behavior claims were made, against real nvim 0.12.4). 14 findings
survived verification; 5 candidates were disproved and are listed under Cleared.

## Findings (most severe first)

### 1. HIGH — Ex range `$` (and `%`) resolves to ropey's phantom trailing row on newline-terminated buffers, so `:$d` / `:$-1d` delete the wrong thing

`crates/hjkl-ex/src/range.rs:176-178` — `resolve_address` computes the last line
as `editor.buffer().row_count()`, which includes the phantom empty final row
ropey synthesizes for a trailing `\n` (`"a\nb\n"` → 3 rows). `content_row_count`
in the engine (`crates/hjkl-engine/src/motions.rs:98-107`) exists precisely to
skip that row, but the ex range resolver uses the raw count.

```
Repro: buffer "a\nb\n" (3 ropey rows: a, b, phantom); :$d
Expect: "a\n" (vim deletes line 2)
Actual: "a\nb" — row 2 (phantom) is deleted, which removes the
        trailing newline instead of line "b"
```

`:$` also parks the cursor on the phantom row, and `:$-1d` deletes `b` where vim
deletes `a`. The `%` form is affected for register reads: `%y` yanks
`"a\nb\n\n"` (an extra empty line) because the range includes the phantom row.
Same root hits `:2,$d` on a two-line buffer. Every `$`-based address on a
newline-terminated file is off by one.

### 2. HIGH — `ensure_cursor_visible` (soft-wrap path) underflow-panics on a stale cursor after another view shrinks the shared buffer

`crates/hjkl-buffer/src/buffer.rs:228,244` — the scroll loop bounds `next`
against the **raw** `self.cursor.row` while `screen` was computed from the
**clamped** cursor row (`cursor_screen_row_from` clamps at `buffer.rs:438`).
When the cursor is stale — a state the code explicitly supports, see the comment
at `buffer.rs:435-437` and the regression test
`cursor_screen_row_survives_shrink_from_other_view` — the subtraction at line
244 (`screen -= wrap_segments(...).len()`) can underflow `usize`.

```
Repro: shared buffer "aa\nbb\n" + "x"*28 (3 rows); view B sets cursor
       (5, 26) (row 5 now stale after view A's replace_all); viewport
       wrap=Char, text_width=4, height=6; ensure_cursor_visible
Expect: viewport scrolls to the clamped cursor row (2)
Actual: panic "attempt to subtract with overflow" (debug) / wraps to
        usize::MAX and rope_line_str(3) panics on the 3-row rope
        (release)
```

### 3. HIGH — `n` (and `*`) is permanently stuck when the cursor sits on a multi-byte char at the match start

`crates/hjkl-engine/src/search.rs:506-511` — "skip the current cell" advances
the anchor by one **byte** (`pos_at_byte(from_byte + 1)`), and `pos_at_byte`
(`crates/hjkl-engine/src/buffer_impl.rs:109-114`) rounds a mid-char byte _down_
to the enclosing char's start. On the first byte of a multi-byte char, `+1`
lands inside that char and rounds back to the cursor itself, so `find_next`
re-finds the current match.

```
Repro: buffer "éé", /é, then n
Expect: cursor advances to the second é (col 2 / byte 3; real nvim
        moves to 1-based col 3)
Actual: cursor stays at (0,0) on every n — search navigation is
        permanently stuck on multibyte text
```

The same `byte_offset`/`pos_at_byte` mismatch also feeds
`resolve_search_address` in the ex range resolver
(`crates/hjkl-ex/src/range.rs:251-252`).

### 4. MEDIUM — `nvim_get_current_buf` returns id 0 for the initial buffer, colliding with the "0 = current buffer" wildcard

`apps/hjkl/src/app/mod.rs:2235` (`let buffer_id: BufferId = 0;`), `mod.rs:2336`
(`next_buffer_id: 1`), `apps/hjkl/src/nvim_api.rs:64-86` (`param_buf`: id 0 →
`None` → "current buffer"), `nvim_api.rs:669` (`nvim_get_current_buf` returns
`buf_handle(app.nvim_current_buffer_id())`). The module's own contract
(`nvim_api.rs:19-22`) says "Buffer ids start at 1 … a Nil, missing, or 0 handle
means the current buffer" — so the initial buffer's own handle is
indistinguishable from the wildcard.

```
Repro: fresh `hjkl --nvim-api`; nvim_get_current_buf → Ext(0,[0]);
       nvim_create_buf → 1; nvim_set_current_buf(1); then
       nvim_set_current_buf(Ext(0,[0])) — or nvim_buf_get_name(0)
Expect: focus returns to the first buffer / its name returned
Actual: param_buf maps 0 → None, so set_current_buf re-resolves the
        current id and no-ops; buf_get_name returns the *current*
        buffer's name. Any client that captured the first buffer's
        handle (plugin state keyed by bufnr, nvim_buf_get_var, …)
        silently retargets the current buffer forever
```

### 5. MEDIUM — `N` does not wrap to the last match when the current match starts at buffer byte 0

`crates/hjkl-engine/src/search.rs:548-563` — when `find_prev` returns a match
whose start equals the cursor and `byte_offset(m.start) == 0`, the code returns
`None`. The comment at line 555 says "fall through to wrap" — it doesn't.

```
Repro: buffer "foo\nfoo", pattern foo, cursor on the first match
       (0,0); N with wrapscan on
Expect: cursor wraps to (1,0) (real nvim wraps to the last match)
Actual: no movement, no wrap — cursor stays at (0,0)
```

### 6. MEDIUM — `w` / `W` / `e` from the last word of a newline-terminated buffer land on the phantom empty row

`crates/hjkl-engine/src/motions.rs:1061-1074` — `step_forward` wraps into the
next row using raw `read_row_count`, which includes the phantom row; the
whitespace-skip loop in `next_word_start` (`motions.rs:1182-1191`) then stops
there because `char_at` returns `None` (not `Some(Space)`) on the phantom row,
so the motion returns `(last_row+1, 0)` as its target. Every other vertical
motion uses `content_row_count` to skip the phantom row (`motions.rs:81-107`).

```
Repro: buffer "abc def\n", cursor at (0,6) (the f), w
Expect: no next word — cursor stays at (0,6) (real nvim stays on
        line 1)
Actual: cursor lands on (1,0), a line vim doesn't have; x/p/r/$ on
        it target the empty phantom row
```

### 7. MEDIUM — `s` (substitute char) never writes the deleted text to the unnamed register

`crates/hjkl-vim/src/vim/bridges.rs:177-202` — `substitute_char_bridge` deletes
via raw `mutate_edit` with no `record_delete`/`record_yank_to_host`, unlike `x`
/ `X` (`crates/hjkl-vim/src/vim/command.rs:180-240`) and `S` (routes through
`change_linewise_rows`). Vim's `s` is `cl` — the deleted text lands in `"`.

```
Repro: buffer "abc\nxyz\n"; yy on line 2; cursor (0,0); sx<Esc>; then p
Expect: pastes "a" (the substituted char; real nvim's unnamed
        register is "a" after sx)
Actual: pastes the stale "abc\n" from the earlier yy
```

### 8. MEDIUM — Visual-block `A` appends at `right + 1` (one column past nvim's append point) and can leave phantom padding on an empty insert

`crates/hjkl-vim/src/normal.rs:291-300` passes `col = right + 1`;
`crates/hjkl-vim/src/editor_ext.rs:1742-1750` pads the top row to `col` before
the insert session. Real nvim appends at the block's right edge (col `right`),
so the pad is one space too wide. Additionally the pad is applied
unconditionally and only consumed when text is actually typed
(`crates/hjkl-vim/src/vim/comment.rs:236` gates replication on
`!inserted.is_empty()`), so an immediate `<Esc>` leaves the pad in the buffer.

```
Repro: buffer "ab\nc\n", cursor (0,0), <C-v>jllA<Esc> (nothing typed)
Expect: buffer unchanged "ab\nc\n" (real nvim leaves it unchanged)
Actual: buffer becomes "ab \nc\n" — a stray space appended to the
        top row by a command that typed nothing
```

With text typed, hjkl's append lands at col 3 where nvim's lands at col 2
(`ab x` vs `abx` for `<C-v>jllAx`).

### 9. MEDIUM — `:s` with a newline in the replacement bypasses `mutate_edit`, so marks, jumplist and folds below the change are never rebased

`crates/hjkl-engine/src/substitute.rs:356` and `:575` call
`buffer_mut().replace_all(...)` directly. The only place marks/global
marks/folds/jumplist are rebased for a row-count change is `mutate_edit` →
`shift_marks_after_edit`
(`crates/hjkl-engine/src/editor.rs:2768-2769, 2780-2836`); `replace_all`
(`crates/hjkl-buffer/src/buffer.rs:329-344`) only clamps the cursor.

```
Repro: 10-line buffer, ma on line 9 (0-based 8), :%s/a/b\r/ (each
       matched line gains a trailing newline), then 'a
Expect: jumps to the row that now holds the old line-9 text (shifted
        down by the inserted lines)
Actual: lands on the stale row the mark was recorded at, now holding
        different text; '.'/changelist likewise not updated
```

### 10. MEDIUM — `nvim_buf_get_text` silently returns `[]` for an inverted range where its sibling `nvim_buf_set_text` rejects the same input

`apps/hjkl/src/nvim_api.rs:1586-1590` — `start_row`/`end_row` are clamped and
collected with `for row in start_row..=end_row`; when `start_row > end_row` the
loop never runs and an empty array is sent as success. The sibling
`nvim_buf_set_text` returns an error for the identical input
(`nvim_api.rs:1706- 1711`, with a pinning test at `nvim_api.rs:5335`).

```
Repro: buffer with ≥ 2 lines; nvim_buf_get_text(buf, 2, 0, 1, 0, {})
Expect: error ("start is higher than end", same class set_text
        rejects)
Actual: ok [] — silently masks the caller's bug
```

### 11. LOW — comment-marker label/trail spans can start or end mid-multi-byte-char

`crates/hjkl-bonsai/src/comment_markers.rs:201`
(`label_start = m.word_start.saturating_sub(1)`) and `:218-228`
(`trail_end = m.word_end + 1`); same in `apply_rope` at `:471` / `:487-491`.
`word_start` is a byte offset; subtracting one byte lands inside a multi-byte
char when the char before the marker word is non-ASCII. The emitted
`HighlightSpan.byte_range` is then not on a char boundary — consumers that slice
the row string at those offsets panic, and the highlight itself is wrong.

```
Repro: comment "// éTODO: fix" (é = 2 bytes: 3-4); word_start = 5
       (the T); label_start = 4 = the second byte of é
Expect: label span starts at a char boundary (5) or snaps to one
Actual: span byte_range 4..9 begins mid-char; row[4..] panics
```

### 12. LOW — indent-guide and diagnostic-overlay passes map doc rows to screen rows without accounting for diff-filler rows, painting N rows too high

`crates/hjkl-buffer-tui/src/render.rs:904-905` (indent guides walk
`ig_screen_row += 1` per non-hidden doc row) and `:984-995` (diag overlay counts
only non-hidden rows) don't consult `self.diff_filler`, while the main render
loop paints filler rows above each real line and advances `screen_row` for each
(`render.rs:608-623`). The EOL-hint and cursorcolumn passes are filler-aware.

```
Repro: diff pair where side A has 2 lines existing only on side B (2
       filler rows above doc row 5); focus that window with
       indent_guides_enabled on
Expect: guides on doc rows 3-5 paint at screen rows 5-7 (after the
        fillers)
Actual: they paint at screen rows 3-5 — 2 rows too high, onto the
        filler/tinted rows; diag underlines are off by the same
        offset
```

### 13. LOW — markdown table can render wider than the viewport, contradicting the documented invariant

`crates/hjkl-markdown-tui/src/lib.rs:399-407` — the width fit floors each column
to a minimum of 3 _after_ the proportional scale, so the sum of column widths
can exceed `budget`. The comment at `:357-359` claims "the table never renders
wider than the viewport".

```
Repro: to_lines(&parse("| aaaa | bbbb |\n|---|---|\n| 1 | 2 |\n"),
       &MdTheme::default(), 10)
Expect: every rendered line ≤ 10 cells
Actual: rendered row is 13 cells wide (two columns forced to 3 each
        plus padding/borders); the host clips the right edge
```

### 14. LOW — `HexColorPass::apply_range` skips the left-boundary check at the range start

`crates/hjkl-bonsai/src/hex_color.rs:71-93` scans `&bytes[start..end]` with no
look-behind, and `try_scan_hex` (`:206-214`) treats the range start as a
boundary (`i > 0`). The sibling `apply_range_rope` (`:97-141`) explicitly widens
by one byte "so left/right boundary checks work", so the two disagree and
`apply_range` violates the documented rule (a `#` preceded by a hex digit must
not match).

```
Repro: bytes "abc#fff", apply_range(spans, bytes, 3..7)
Expect: no span (preceding char 'c' is a hex digit)
Actual: span emitted for #fff; apply_range_rope on the same input
        emits nothing
```

## Cleared

- `[count]iw` selects one fewer word than vim — **disproved**: verified against
  real nvim that `v1iw..v5iw` on "foo bar baz" selects "foo", "foo ", "foo bar",
  "foo bar ", "foo bar baz" — nvim counts word/whitespace _runs_, which is
  exactly what hjkl's implementation produces.
- `dgn` deletes one char past a match ending at column 0 — **disproved**:
  matches cannot span newlines in this model (`rope_line_str` excludes line
  separators, `find_next` is per-line), so `end.col == 0` only occurs for
  zero-width matches at line start, where real nvim's `dgn` also deletes one
  char (`dgn` with pattern `^` on "ab\ncd\n" → "b\ncd\n").
- Visual-block empty `I`/`A` leaves the cursor off the block edge —
  **disproved**: real nvim leaves the cursor at 1-based col 2 after empty
  block-`I` and col 3 after empty block-`A`, exactly where hjkl's step-back
  lands; the claimed "expected" positions do not match nvim.
- fuzzy `score` fast path is outranked by scattered matches for long needles —
  **disproved**: the contiguous fast path is only reached when the needle
  appears literally, which forces coverage 100% (`pct = 100`); the `PCT_SCALE`
  (100,000×) dominant term means no scattered match can outrank it.
- `hjkl-fs-watch` `debounce / 2` truncates to zero for debounce < 2ms causing a
  busy spin — **disproved**: `Duration / u32` divides the nanosecond count
  exactly (`1ms / 2 = 500µs`, verified by compiling), so the ticker is never
  zero for any realistic debounce.

## Hardening

- The phantom-row class of bug (#1, #6) recurs because ex ranges
  (`range.rs:176`), word motions (`motions.rs:1061-1074`) and the buffer's own
  `row_count` each maintain their own convention about the phantom final row. A
  single shared "content row count" helper (the engine already has
  `content_row_count`, `motions.rs:98`) used by every consumer would close the
  whole class.
- `param_buf` treats 0 as the current-buffer wildcard while `param_win`
  explicitly treats 0 as a real window id (`nvim_api.rs:94-95`). The asymmetry
  is documented but nothing enforces that buffer ids actually start at 1 — the
  initial buffer breaks the contract (#4).
- `:s` bypasses `mutate_edit` (#9) — the invariant "every content mutation goes
  through the funnel that rebases marks/jumplist/folds" is held by convention,
  and `replace_all` is the escape hatch. A comment at the `replace_all` call
  sites naming the invariant would help the next editor.

## Coverage

Scope: full codebase (clean working tree, `main` branch). Reviewed via four
read-only sub-agents, every reported candidate then re-verified by re-reading
the cited lines and, for behavior claims, testing against real nvim 0.12.4.

Reviewed:

- `apps/hjkl/src/app/` core (event_loop, window, types, prompt, chord_routing,
  hop, count_prefix, viewport_sync in full; parts of ex_dispatch, ex_host_cmds,
  lsp_glue, explorer, explorer_reconcile, quickfix, mouse, syntax_glue,
  keymap_build, dock, buffer_ops, mod.rs, diff_mode) and `nvim_api.rs` (dispatch
  surface).
- `hjkl-vim`, `hjkl-vim-tui`, `hjkl-vim-types` — in full.
- `hjkl-engine` (search, motions, buffer_impl, registers, substitute 1-800,
  parts of editor.rs) and `hjkl-ex` (range, parse, global, builtins 617-1414,
  registry, effect, complete 187-316, setopt 1-200).
- `hjkl-clipboard` (core, not backends), `hjkl-bonsai` (comment_markers,
  hex_color, highlighter 772-1460, rope_slice, predicate, rainbow, lib),
  `hjkl-buffer` (in full), `hjkl-fs`, `hjkl-fs-watch`, `hjkl-fuzzy`,
  `hjkl-config`, `hjkl-app` (config, editorconfig, modeline, git, swap, trash,
  undofile, picker_sources/git partial), and partial reads across the remaining
  crates (keymap, picker, completion, which-key, tabs, markdown, markdown-tui,
  buffer-tui render, lsp, css, mangler, layout 1-2000, statusline, vim-types,
  compat-oracle, theme color, xdg, kitty, form fsm/field, menu 490-619,
  hover-tui, holler-tui 60-179, editor-tui, prompt-tui, lang comment, anvil
  store/installer).

Not reviewed (GAPs):

- `apps/hjkl/src/` non-app files (main.rs, render.rs, completion.rs,
  keymap_actions.rs, headless.rs, picker_sources.rs, picker_git.rs,
  start_screen.rs) — the planned review agent for this slice was never spawned;
  the report moved to the fix phase per instruction.
- `apps/hjkl/src/app/` remainder: diff.rs, fs_watch.rs, git_hunks.rs, keymap.rs,
  mappings_dispatch.rs, pending_actions.rs, picker_glue.rs,
  confirm_substitute.rs, dispatch.rs, engine_actions.rs, and the unread portions
  of ex_dispatch/ex_host_cmds/lsp_glue/explorer/explorer_reconcile/
  quickfix/mouse/syntax_glue/keymap_build/dock/buffer_ops/mod.rs.
- `hjkl-engine`: editor.rs lines 900-1830/2130-2430/2900-4677/5031-7376,
  substitute.rs 800-1511, options_registry, policy, discipline.
- `hjkl-ex`: builtins.rs 1-617 and 1415-5190, shell, listings, setopt 200-1411,
  complete remainder.
- `hjkl-clipboard/backend/*` (wayland/x11/macos/windows/darwin), `hjkl-bonsai`
  highlighter 1-772 and 1461-2583, builtins, folds, theme, runtime/\*.
- All `tests/` directories were excluded by instruction.
- CI gate (fmt/clippy/build/nextest) was not run during the review phase; it
  will be run as part of the fix phase.
