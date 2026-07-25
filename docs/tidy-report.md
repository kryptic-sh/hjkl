# Tidy Report

**Project:** hjkl (terminal text editor) **Date:** 2026-07-23 (pruned
2026-07-25) **Scope:** entire codebase

The codebase is clean — clippy with `-D warnings` passes with **zero** errors.
The items below came from an extra pedantic pass over `redundant_clone` /
`or_fun_call` (both allow-by-default nursery lints, not in the gate). All
actionable ones have shipped.

## Resolved (pruned)

| Was | Fix                                                    | Commit     |
| --- | ------------------------------------------------------ | ---------- |
| #1  | status-bar string clones dropped (render hot path)     | `7af516a7` |
| #2  | `Arc::clone(&self.registry)` → `&self.registry`        | `b0dcdfd4` |
| #3  | completion `src.filter_text` clone dropped             | `5d19681e` |
| #4  | `or_insert` → `or_insert_with` (LSP diag sign map)     | `5d19681e` |
| #5  | `unwrap_or` → `unwrap_or_else` in embed/headless       | `5d19681e` |
| #7  | which-key `chord_to_notation(&[KeyEvent])` slice       | `7af516a7` |
| #8  | 52 redundant clones dropped from `apps/hjkl` test code | `c323f553` |
| #9  | 14 production redundant clones (cold paths)            | `5d19681e` |

**#6 — withdrawn** (`apps/hjkl/src/menu.rs:8`): the `#[allow(unused_imports)]`
is load-bearing. `apps/hjkl` is bin-only; `MenuItem` is re-exported but used
only under `#[cfg(test)]`, so in a normal build the re-export is genuinely
unused and the `#[allow]` is what keeps `-D warnings` green. Not a finding.

---

## Residual (outside #8's scope — left intentionally)

`#8` covered `apps/hjkl` test files only. A pedantic
`-W clippy::redundant_clone` sweep still reports ~20 hits elsewhere, none in the
`-D warnings` gate:

- **1 production site** — `apps/hjkl/src/app/lsp_glue.rs:1322` (2 spans,
  `rope_line_str(..).to_string()`). Not auto-fixed — left for manual judgment
  (may be load-bearing; a warm-ish path, not a per-frame one).
- **~18 in other crates** — `hjkl-ex` (7), `hjkl-vim` (6), `hjkl-bonsai` (2),
  `hjkl-app` (1), `hjkl-editor-tui` (1), `hjkl-engine` (1). Mix of test and cold
  production paths, outside this report's original `apps/hjkl` scope. Optional
  cleanup if a future pass wants them.
