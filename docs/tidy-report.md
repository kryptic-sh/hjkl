# Tidy Report

**Project:** hjkl (terminal text editor) **Date:** 2026-07-23 **Scope:** entire
codebase

---

## Findings

### 1. Redundant `.clone()` on render hot path (status bar, every frame)

**`apps/hjkl/src/render.rs:297`** — `search_count_content.clone().into()`
**`apps/hjkl/src/render.rs:315`** — `diag_count_content.clone().into()`

Both `search_count_content` and `diag_count_content` are local `String` values
whose last use is the cloned move into the segment (each is borrowed twice
before — `.chars().count()` and `.is_empty()` — then cloned). The `.clone()` is
redundant; the value can move into the segment. Confirmed by clippy
`redundant_clone` at both lines. Remove `.clone()` on both.

### 2. Redundant `Arc::clone(&self.registry)` in highlight loop

**`crates/hjkl-bonsai/src/highlighter.rs:755,1451`**

`let registry = Arc::clone(&self.registry)` clones an `Arc` every call to
`highlight_range` (and `highlight_injections`). The editor already holds a
reference to `compiled` through `self`, and `registry` is only used as an
argument to predicate/directive lookups during the match loop. Calling
`Arc::clone` is idiomatic for pass-by-value to closures, but here the clone is
unnecessary — the borrow `&self.registry` suffices because `registry` is used
synchronously within the same call stack, not handed off to another thread or
future. (Verified 2026-07-23: the rope text-provider closure at `:1440` captures
only `rope`/`total_bytes`, never `registry`, and the sibling `self` borrows in
the loop are immutable + disjoint, so `&self.registry` borrow-checks — genuinely
removable, not load-bearing.) Replace `Arc::clone(&self.registry)` with
`&self.registry`; no call-site signature change is needed — auto-deref already
lets `&PredicateRegistry` call `get_predicate`/`get_directive`.

Note: clippy does **not** flag this (`Arc::clone` is excluded from
`redundant_clone`), and the payoff is a single atomic increment per call —
negligible. Cleanup, not a perf win.

### 3. Redundant clone in `item_from_lsp`

**`crates/hjkl-completion/src/lib.rs:348`** — `src.filter_text.clone()`

The function takes `src: lsp_types::CompletionItem` by value (move). The
`.clone()` on line 348 is redundant because `filter_text` is the last field read
from `src`. Remove `.clone()` — use `src.filter_text` directly. (Clippy
`redundant_clone`.)

### 4. Eager evaluation inside `or_insert` on LSP diagnostic path

**`apps/hjkl/src/app/lsp_glue.rs:538`** —
`.or_insert((severity, 'E', Style::default(), 0))`

The tuple constructor runs even when the key already exists in the sign map.
Replace with `or_insert_with(|| (severity, 'E', Style::default(), 0))` to defer
construction to the miss path only. (Clippy `or_fun_call`.)

### 5. Eager evaluation in `unwrap_or` in embed/headless dispatch

**`apps/hjkl/src/embed.rs:140`** — `unwrap_or(ExEffect::Unknown(cmd.clone()))`
**`apps/hjkl/src/headless.rs:117`** —
`unwrap_or(ExEffect::Unknown(cmd.to_string()))`

The fallback value is constructed even on the happy path. Replace with
`unwrap_or_else(|| ExEffect::Unknown(cmd.clone()))` /
`unwrap_or_else(|| ExEffect::Unknown(cmd.to_string()))`. (Clippy `or_fun_call`.)

### 6. ~~Unnecessary~~ `#[allow(unused_imports)]` on re-export barrel — **WITHDRAWN**

**`apps/hjkl/src/menu.rs:8`**

> **Verified 2026-07-23 — INCORRECT, the attribute is load-bearing.**
> `apps/hjkl` is a **bin-only** crate (`main.rs`, no `lib.rs`), so a `pub use`
> barrel has no downstream consumer. `MenuItem` is re-exported here but used
> only inside `#[cfg(test)]` code (`app/tests/mouse.rs`), never in a normal
> build — so in a non-test build the re-export is genuinely unused and
> `#[allow(unused_imports)]` is what keeps `cargo clippy -D warnings` green.
> Removing it would raise `unused_imports` and break the gate. Leave it.

### 7. `Chord` construction clones `Vec<KeyEvent>` unnecessarily

**`apps/hjkl/src/render.rs:3020`** —
`hjkl_keymap::Chord(pending.clone()).to_notation(leader)`

`to_notation(&self, ...)` takes `&self`. The `Chord` wrapper exists only to call
a method that borrows it, immediately after. The `pending` Vec is cloned solely
to construct a `Chord` that is dropped on the same line. Two options: (a) add a
free function `hjkl_keymap::chord_to_notation(&[KeyEvent], leader)` that takes a
slice directly, or (b) note that this only runs when the which-key popup is
visible (not every frame) — low impact either way.

### 8. Widespread redundant clones in test code (~50 findings)

**`apps/hjkl/src/app/tests/*.rs`**, **`apps/hjkl/tests/*.rs`**

53 clippy `redundant_clone` warnings in test files (dominated by
`app/tests/ex.rs`, ~31 alone). These are mechanical one-line fixes (remove
`.clone()`). Low priority but easy to auto-fix with `cargo clippy --fix`.

### 9. Widespread redundant clones in cold production paths (~15 findings)

**`apps/hjkl/src/app/ex_dispatch.rs`**, **`apps/hjkl/src/app/explorer.rs`**,
**`apps/hjkl/src/app/picker_glue.rs`**, **`apps/hjkl/src/embed.rs`**,
**`apps/hjkl/src/nvim_api.rs`**

14 `redundant_clone` warnings in non-test production code (`ex_dispatch.rs` 2,
`explorer.rs` 3, `picker_glue.rs` 2, `embed.rs` 4, `nvim_api.rs` 3) on
user-triggered paths (ex commands, picker invocation, nvim-api dispatch) rather
than per-frame paths. Low impact individually; fix as a batch.

---

## Summary

| #   | Impact   | File:Line                         | Action                                                                                                                                |
| --- | -------- | --------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------- |
| 1   | Hot      | `render.rs:297,315`               | Drop `.clone()` on status-bar strings                                                                                                 |
| 2   | Hot      | `highlighter.rs:755,1451`         | Use `&self.registry` instead of `Arc::clone`                                                                                          |
| 3   | Warm     | `completion/src/lib.rs:348`       | Drop `.clone()` on `src.filter_text`                                                                                                  |
| 4   | Warm     | `lsp_glue.rs:538`                 | `or_insert` → `or_insert_with`                                                                                                        |
| 5   | Cold     | `embed.rs:140`, `headless.rs:117` | `unwrap_or` → `unwrap_or_else`                                                                                                        |
| 6   | ~~Dead~~ | `menu.rs:8`                       | **WITHDRAWN — attribute is load-bearing (bin-only crate, `MenuItem` used only in `#[cfg(test)]`); removing it breaks `-D warnings`.** |
| 7   | Low      | `render.rs:3020`                  | Avoid `Chord(pending.clone())` allocation                                                                                             |
| 8   | Low      | `tests/*.rs` (53 sites)           | Auto-fix redundant clones                                                                                                             |
| 9   | Low      | Various production (14 sites)     | Auto-fix redundant clones                                                                                                             |

The codebase is clean — clippy with `-D warnings` passes with **zero** errors.
The mechanical items above (`redundant_clone`, `or_fun_call`) come from an extra
pedantic pass: both lints are **allow-by-default** (nursery-tier) and are not
part of the repo's gate, which is why they coexist with a clean `-D warnings`
build — they are real but do not fail CI. All were reproduced empirically
(2026-07-23). No over-abstraction, dead code, or unnecessary indirection layers
found. The items worth fixing first are the status-bar string clones (#1,
per-frame hot path) and the redundant `Arc::clone` in the highlighter (#2,
per-call cleanup — negligible perf but tidy).
