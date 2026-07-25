# Performance Review

**Project:** hjkl (terminal text editor) **Date:** 2026-07-23 (pruned
2026-07-25) **Scope:** entire codebase **Verdict:** Well-optimized for a
terminal editor. All ranked hotspots (P1–P3, P5–P7, P9–P11) shipped; P8 (bench)
and P4 (invalidation trap) were analyzed and closed WONTFIX. **No open items
remain.**

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

## Analyzed & closed

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

Magnitude is below P8's (which the `benches/render.rs` A/B proved is
single-digit µs on a render that is <0.3% of a 16ms frame). Not worth a
per-frame O(n) pass or an invalidation footgun for a sub-µs gain. Closed.

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
