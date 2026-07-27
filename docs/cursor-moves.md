# Cursor moves carry their own curswant semantics (2026-07-27)

## Why

Moving the cursor and maintaining `sticky_col` (vim's `curswant`) are two
separate actions today, and nothing forces the second:

| primitive           | non-test sites | maintains curswant? |
| ------------------- | -------------- | ------------------- |
| `buf_set_cursor_rc` | 106            | **no**              |
| `View::set_cursor`  | 80             | **no** (documented) |
| `jump_cursor`       | 107            | yes (`= col`)       |
| `set_sticky_col`    | 33             | manual follow-up    |
| `apply_sticky_col`  | 1              | the vim catch-all   |

That is ~186 cursor moves which do not maintain curswant, each a potential
instance of the bug fixed in `c022a3a4`: `/pattern<CR>` moved the cursor without
resetting curswant, so the next `j` snapped back to the pre-search column.

The rule was never unknown — it is written correctly in two places already
(`apply_sticky_col`: "Everything else — search, gg/G, word jumps — lands at the
match's own column"; `jump_cursor`: "every explicit jump … search hit, click").
It was **unenforced**. Four call sites reached the search advance and only the
one routed through the vim motion dispatch was right.

## Target shape

Make "what does this do to curswant?" a required argument of moving, not a
follow-up call:

```rust
pub enum Move {
    Vertical   { row: usize },              // j/k: READS curswant, clamps, leaves it
    Jump       { row: usize, col: usize },  // search/gg/G/marks/click: SETS curswant
    Horizontal { col: usize },              // h/l/w/b/$/^: SETS curswant
    Raw        { row: usize, col: usize },  // must NOT disturb curswant
}

impl Editor { pub fn move_cursor(&mut self, m: Move); }
```

Then **seal the raw primitives**: `buf_set_cursor_rc` and `View::set_cursor`
become crate-internal, unreachable from vim/app code. A new motion then cannot
forget — it must name a variant to compile.

`Raw` is deliberately conspicuous. Today the forgetful option is the one that
looks like the default; naming it inverts that, and it stays greppable for
review.

## Phases

One commit each, gated (`clippy -D warnings`, `fmt`, full `nextest` incl. the
pty e2e suite, compat oracle ALL-pass), pushed with CI green before the next.

| #   | Phase              | Scope                                                                                                                                                     |
| --- | ------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 0   | Debug invariant    | Debug-only assertion in the key-dispatch loop: if the cursor moved and the motion was not vertical, curswant must equal the cursor column. No API change. |
| 1   | `Move` API         | Add `Move` + `Editor::move_cursor` implemented over today's primitives. Nothing migrated yet.                                                             |
| 2   | Migrate the engine | `hjkl-engine`'s own motions move through `move_cursor`.                                                                                                   |
| 3   | Migrate vim        | `hjkl-vim`: motion, command, bridges, visual, operator. The bulk of the sites.                                                                            |
| 4   | Migrate the app    | `apps/hjkl`.                                                                                                                                              |
| 5   | Seal               | Raw primitives crate-internal; `set_sticky_col` off the public surface; `apply_sticky_col` shrinks to the vertical clamp.                                 |

**Phase 0 comes first on purpose.** It is the safety net for the migration
itself: a site classified into the wrong variant during phases 2–4 trips the
assertion in the 5400-test suite rather than shipping as a silent behaviour
change.

## The real cost

~186 sites each need a decision about which variant they are. That is the work,
and it is also the point: those decisions exist today, unrecorded. A mechanical
translation that picks `Raw` everywhere would compile, pass, and preserve the
bug class exactly — so **`Raw` must be justified per site**, not used as the
default landing zone. Phases 2–4 should report how many sites landed in each
variant, and any `Raw` needs a one-line reason.

## Invariants

- Compat oracle stays ALL-pass; its corpus is never edited to make a change
  pass. It now covers search/curswant directly (5 cases, expectations taken from
  headless nvim).
- The pty e2e suite is the real safety net for cursor behaviour.
- No phase except 0 may change behaviour. If an assertion has to change, that is
  a misclassification — stop and report it.
