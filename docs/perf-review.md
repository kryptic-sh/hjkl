# Performance Review

**Project:** hjkl (terminal text editor) **Date:** 2026-07-23 (pruned
2026-07-25) **Scope:** entire codebase **Verdict:** Well-optimized for a
terminal editor. All ranked hotspots (P1–P3, P5–P7, P9–P11) have shipped; P8 was
benchmarked and dropped (WONTFIX); only one design-gated item (P4 memo) remains.

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
| P9  | which-key `chord_to_notation(&[KeyEvent])` — no Vec clone                                      | `7af516a7` |
| P10 | `HighlightSpan.metadata` → `Option<Box<HashMap>>` (span 48B→8B, empty normalizes to `None`)    | `66617a18` |
| P11 | one redundant `Range` clone removed (other three load-bearing — `Range` is `Clone` not `Copy`) | `b0dcdfd4` |

---

## Open findings

### 🟠 P4 — statusline `format!` allocations per frame — _partial_

**`apps/hjkl/src/render.rs:150,158,161,183,188,220-236,243,326`**

Single-pass diag tally + `Cow<str>` filename shipped (`7af516a7`). **Still
open:** cross-frame memoization of the diag counts (recomputed each frame from
`lsp_diags`, which changes only on LSP notifications). Deferred deliberately —
needs an invalidation-key design (what to key the memo on so it drops when
diagnostics change). Not started pending that decision.

### ⚪ P8 — `lines_prefetch: Vec<String>` per frame — measured, WONTFIX

**`crates/hjkl-buffer-tui/src/render.rs` (`line_at` closure)**

Every frame allocates a `Vec<String>` of `area.height` (~50) lines from the
rope, feeding `Cow::Borrowed` accessors during the render walk. The proposed fix
(P8-C) was to borrow each line directly from the rope via `RopeSlice::as_str()`
(borrow when the line is single-chunk, owned fallback for multi-chunk long
lines), eliminating the per-frame Vec.

**Benchmarked and dropped** (see `benches/render.rs`, added `e9dc813a`). Best
variant (borrow + O(1) `strip_suffix`, slice reused in the owned branch to avoid
a double tree-walk): **short lines −4-5%** (~41.7µs → ~39.7µs, saves the ~50
String allocs) but **long/multi-chunk lines +3.1%** (~102µs → ~105.5µs — the
`as_str()` chunk-probe that returns `None` is pure overhead on the owned path).
Absolute scale is single-digit µs on a render that is itself <0.3% of a 16ms
frame budget, and the change regresses long-line/minified-file editing. Net not
worth the complexity + regression vector — the prefetch stays. The bench is kept
for future perf work.

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
