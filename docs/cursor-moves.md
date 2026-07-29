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

The selected design makes curswant semantics part of every cursor move:

```rust
pub enum Move {
    Vertical   { row: usize },              // j/k: READS curswant, clamps, leaves it
    Jump       { row: usize, col: usize },  // search/gg/G/marks/click: SETS curswant
    Horizontal { col: usize },              // h/l/w/b/$/^: SETS curswant
    Raw        { row: usize, col: usize },  // must NOT disturb curswant
}

impl Editor { pub fn move_cursor(&mut self, m: Move); }
```

The reserved end state for raw primitives is crate-internal use, keeping
`buf_set_cursor_rc` and `View::set_cursor` unreachable from vim/app code. This
explains why every migrated motion requires an explicit variant; unfinished work
is tracked in `docs/backlog.md`.

`Raw` is deliberately conspicuous. Today the forgetful option is the one that
looks like the default; naming it inverts that, and it stays greppable for
review.

## Phases

Phases 0–1 shipped separately; remaining work is tracked in `docs/backlog.md`.

| #   | Phase                              | Scope                                                                                                                                                     |
| --- | ---------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 0   | Debug invariant (done, `1fb21010`) | Debug-only assertion in the key-dispatch loop: if the cursor moved and the motion was not vertical, curswant must equal the cursor column. No API change. |
| 1   | `Move` API (done, `c1627b9e`)      | Add `Move` + `Editor::move_cursor` implemented over today's primitives. Nothing migrated yet.                                                             |

Remaining phases and their acceptance criteria are consolidated in
`docs/backlog.md`.

**Phase 0 comes first on purpose.** It is the safety net for the migration
itself: a site classified into the wrong variant during phases 2–4 trips the
assertion in the 5400-test suite rather than shipping as a silent behaviour
change.

## What phase 0 found

The unconditional assertion the plan called for is **not enableable**: it
produces 236 violations across 158 tests. The shipped check is therefore scoped
to plain motions — no chord/count in flight, `dirty_gen` unchanged, mode
unchanged across the key, no search prompt open. Every guard is structural, not
a suppression list, so a motion added later is covered the day it is written.

Two design calls made during implementation, both kept:

- **State-based, not key-based.** Classifying the key is unsound: `j` is
  vertical bare, a target in `dj`/`fj`/`rj`, and a literal in Insert. The check
  instead tests the states the rules can produce — after a move `sticky_col`
  must be `None`, equal to the landed column, or greater with the cursor clamped
  to the row end.
- **Fires on the transition, not the state.** It triggers only when a keystroke
  goes from a legal pair to an illegal one. Without that, class D below was
  blamed for staleness `J` had left a keystroke earlier — the assertion would
  have pointed the next phase at the wrong file.

Observed phase-0 violations:

| Class | Count | What                                                                                                                                                                                                                                                                                                           |
| ----- | ----- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A     | 170   | Insert mode: every printable char, `Tab`, `<C-w>`. One systemic site.                                                                                                                                                                                                                                          |
| B     | 66    | Normal-mode operators/edits: `y{motion}`, `d{motion}`, `J`, `p`/`P`, `x`/`X`, `~`, `>`, `.`, `u`, `<C-a>`. The operator path never reaches `apply_sticky_col`.                                                                                                                                                 |
| C     | 12    | Visual-mode `y` — invisible to a `dirty_gen` check since yank makes no edit.                                                                                                                                                                                                                                   |
| D     | 1     | **A genuine motion-path bug**: `<C-e>` moves between rows without routing through motion dispatch, parking the cursor past end-of-line. `is_vertical_motion` already lists `ScreenUp`/`ScreenDown`; the key binding just doesn't go through it. Confirmed against nvim: `<C-e>` preserves curswant and clamps. |

After excluding A–C, the surviving motion-path violations numbered **one** — the
pre-existing motion code was in better shape than the ~186 unmaintained call
sites suggested.

## The real cost

The ~186 sites expose the migration's classification cost. A mechanical
translation to `Raw` would compile while preserving the bug class. Variant-count
reporting and raw-move justification are tracked in `docs/backlog.md`.

## Invariants established by phases 0–1

- The compat oracle remained ALL-pass; its corpus was not edited to make either
  phase pass. It covers search/curswant directly with five expectations taken
  from headless nvim.
- The pty e2e suite served as the cursor-behavior safety net.
- Phase 0 was the only phase allowed to change behavior; later assertion changes
  indicate a migration misclassification.
