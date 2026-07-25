# Performance Review

**Project:** hjkl (terminal text editor) **Date:** 2026-07-23 (pruned
2026-07-25) **Scope:** entire codebase **Verdict:** Well-optimized for a
terminal editor. All ranked hotspots (P1–P3, P5–P11) shipped — P8 via buffer
reuse after a first approach benchmarked as a regression. P4 closed WONTFIX (no
invalidation-safe memo). **No open items remain.**

## Resolved (pruned)

Full finding text removed once shipped — see the commit for the change.

| Was | Fix                                                                                            | Commit     |
| --- | ---------------------------------------------------------------------------------------------- | ---------- |
| P1  | `HighlightSpan.capture` + `capture_names` → `Arc<str>`                                         | `7393ad29` |
| P2  | per-row `LineCache` in word motions                                                            | `af5ebd8b` |
| P3  | `iskeyword` pre-parsed once via `KeywordSpec`                                                  | `2d37a385` |
| P5  | `evict_stale` uses `HashSet`                                                                   | `b0dcdfd4` |
| P6  | prebuilt capture-name→index `HashMap`                                                          | `b0dcdfd4` |
| P7  | hlsearch painter consults engine per-row cache (no inversion)                                  | `0a83b354` |
| P8  | line-prefetch `String` buffers reused across frames (see detail below)                         | `35b4c61c` |
| P9  | which-key `chord_to_notation(&[KeyEvent])` — no Vec clone                                      | `7af516a7` |
| P10 | `HighlightSpan.metadata` → `Option<Box<HashMap>>` (span 48B→8B, empty normalizes to `None`)    | `66617a18` |
| P11 | one redundant `Range` clone removed (other three load-bearing — `Range` is `Clone` not `Copy`) | `b0dcdfd4` |

---

## Closed, with detail worth keeping

### ⚪ P4 — statusline diag-count memoization — WONTFIX (no clean design)

**`apps/hjkl/src/render.rs:200-234`**

The single-pass diag tally + `Cow<str>` filename already shipped (`7af516a7`).
The remaining idea was to memoize the diag-count string across frames. On
inspection there is no representation that is both cheap and invalidation-safe:

- **Precompute at the write sites** (`lsp_glue.rs:563` assign,
  `buffer_ops.rs:375` clear) — direct `slot.lsp_diags = …` assignments elsewhere
  (tests today; any future writer) bypass the recompute and leave a **stale
  statusline**. Invalidation trap.
- **Fingerprint (len + severity hash) in a `&mut` refresh pass** — the hash is
  itself an **O(n) pass over `lsp_diags` every frame**, the same cost as the
  count loop it would replace; it only saves ~4 tiny `format!`s. Net ~nothing.
- **Leave as-is** — the block already early-returns an empty string when there
  are no diagnostics (the common case → **zero cost**); the count loop + small
  formats run only when diagnostics are actually shown, over a Vec of tens of
  entries.

Magnitude is well below P8's (a few µs on a render that is itself <0.3% of a
16ms frame), and unlike P8 there is no representation that removes the per-frame
work without risking a stale statusline. Not worth an invalidation footgun for a
sub-µs gain. Closed.

### ✅ P8 — `lines_prefetch: Vec<String>` per frame — fixed by buffer reuse (`35b4c61c`)

**`crates/hjkl-buffer-tui/src/render.rs` (`LineScratch` + `line_at`)**

Every frame allocated a `Vec<String>` of `area.height` (~50) lines from the
rope, feeding `Cow::Borrowed` accessors during the render walk.

**Shipped fix (P8-D):** hold the line buffers in a thread-local scratch and
`String::clear()` each entry — clearing retains capacity, so steady-state frames
allocate **nothing**, while keeping the original shape of exactly one
`rope.line()` walk per row. A `Drop` guard returns the `Vec` to the
thread-local, so an early return or panic mid-render can't lose the capacity,
and a nested render would take an empty `Vec` rather than panic on a held
`RefCell` borrow. Measured A/B (`benches/render.rs`, `e9dc813a`), both
significant at p=0.00:

| fixture       | before    | after    | change    |
| ------------- | --------- | -------- | --------- |
| `short_lines` | 42.59 µs  | 41.30 µs | **−2.9%** |
| `long_lines`  | 104.82 µs | 96.82 µs | **−7.9%** |

**Rejected alternative (P8-C) — do not retry:** borrowing each line straight
from the rope via `RopeSlice::as_str()` (borrow single-chunk lines, owned
fallback for multi-chunk). Three variants were benchmarked; the best gave short
lines −4-5% but **regressed long/multi-chunk lines +3.1%**, because `as_str()`
returns `None` there and the chunk-probe is pure overhead on the owned path.
Reverted. Buffer reuse wins because it removes the allocation without adding a
probe — which is why it improves the long-line case that P8-C made worse.

---

## Positive Findings

- **`ChildCache` eviction** prunes to current working set only.
- **`SearchState`** caches per-row byte ranges with `dirty_gen` invalidation.
- **`COMPILED_CACHE`** global `ahash::AHashMap` avoids re-parsing queries.
- **`sync_after_engine_mutation`** compares
  `(buffer, top_row, height, dirty_gen)` to skip redundant recompute.
- **Renderer `line_at`** returns `Cow::Borrowed` for prefetched rows.
- **Tree-sitter `parse_timeout_micros`** bounds parse work on huge files.
- **`parse_incremental`** skipped `changed_ranges` call that was 54% of
  per-keystroke CPU on huge files.
- **Swap file I/O** uses `O_EXCL` + `create_new(true)` and explicit fsync.
- **Subprocess lifecycle** properly timed out, killed, and waited.
