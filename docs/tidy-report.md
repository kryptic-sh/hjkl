# Tidy Report

**Project:** hjkl (terminal text editor) **Date:** 2026-07-23 (pruned
2026-07-25) **Scope:** entire codebase

The codebase is clean — clippy with `-D warnings` passes with **zero** errors.
The items below came from an extra pedantic pass over `redundant_clone` /
`or_fun_call` (both allow-by-default nursery lints, not in the gate). All
actionable ones shipped; one remains as optional cleanup.

## Resolved (pruned)

| Was | Fix                                                | Commit     |
| --- | -------------------------------------------------- | ---------- |
| #1  | status-bar string clones dropped (render hot path) | `7af516a7` |
| #2  | `Arc::clone(&self.registry)` → `&self.registry`    | `b0dcdfd4` |
| #3  | completion `src.filter_text` clone dropped         | `5d19681e` |
| #4  | `or_insert` → `or_insert_with` (LSP diag sign map) | `5d19681e` |
| #5  | `unwrap_or` → `unwrap_or_else` in embed/headless   | `5d19681e` |
| #7  | which-key `chord_to_notation(&[KeyEvent])` slice   | `7af516a7` |
| #9  | 14 production redundant clones (cold paths)        | `5d19681e` |

**#6 — withdrawn** (`apps/hjkl/src/menu.rs:8`): the `#[allow(unused_imports)]`
is load-bearing. `apps/hjkl` is bin-only; `MenuItem` is re-exported but used
only under `#[cfg(test)]`, so in a normal build the re-export is genuinely
unused and the `#[allow]` is what keeps `-D warnings` green. Not a finding.

---

## Open

### #8 — Redundant clones in test code (~53 findings) — deferred

**`apps/hjkl/src/app/tests/*.rs`**, **`apps/hjkl/tests/*.rs`**

53 clippy `redundant_clone` warnings in test files (`app/tests/ex.rs` ~31
alone). Mechanical one-line removals. A nursery lint not in the `-D warnings`
gate, zero runtime impact; auto-`--fix` across 50+ test files would need
individual review for modest benefit. Left as optional cleanup.
