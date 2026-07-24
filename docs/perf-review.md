# Performance Review

**Project:** hjkl (terminal text editor) **Date:** 2026-07-23 **Scope:** entire
codebase **Verdict:** The codebase is well-optimized for a terminal editor. Two
hotspots stand out: the syntax highlighter's per-capture string clones and the
motion system's per-character line allocations. Below, ranked by impact.

> **Verified 2026-07-23 against source:** all of P1–P11 still point at live code
> — **none is stale**. The recent perf commits (`3f84befb`, `9fa78517`,
> `c4b1b6a3`) fixed _adjacent_ paths (per-cell painter allocations,
> `render_window`/explorer clones, and the engine-side warm-cache line-alloc
> skip in `search.rs`) but did not touch any of these eleven sites. In
> particular **P7 is not stale**: the buffer-tui renderer still calls
> `search_match_ranges` directly and never consults the engine's
> `SearchState::matches_for` cache.

> **Status 2026-07-24 — implemented:**
>
> - **P1** ✅ `HighlightSpan.capture` + `capture_names` → `Arc<str>`, interned
>   once; hot-loop clone is now a refcount bump (`7393ad29`).
> - **P2** ✅ per-row `LineCache` in word motions — per-char whole-line clones
>   collapsed to per-row (`af5ebd8b`).
> - **P3** ✅ `iskeyword` pre-parsed once per motion via `KeywordSpec`
>   (`2d37a385`).
> - **P4** ⚠️ _partial_ — single-pass diag tally + `Cow` filename shipped
>   (`7af516a7`); cross-frame memoization of counts intentionally **not** done
>   (needs an invalidation-key design → left for a decision).
> - **P5** ✅ `evict_stale` uses `HashSet` (`b0dcdfd4`).
> - **P6** ✅ prebuilt capture-name→index `HashMap` (`b0dcdfd4`).
> - **P9** ✅ `chord_to_notation(&[KeyEvent])` — no which-key Vec clone
>   (`7af516a7`).
> - **P11** ✅ one redundant `Range` clone removed; the doc's premise was wrong
>   — `Range<usize>` is `Clone`, **not** `Copy`, so the other three are
>   load-bearing and stay (`b0dcdfd4`).
>
> **Deferred (need a decision, not mechanical):**
>
> - **P7** — the buffer-tui renderer cannot reach `SearchState::matches_for`:
>   `hjkl-buffer-tui` depends only on `hjkl-buffer`, not `hjkl-engine`. Wiring
>   the cache in means either a layering inversion or plumbing engine-computed
>   ranges through the widget API — an architecture call. Current cost is
>   bounded (single-pass regex over ≤ viewport visible lines). **Needs input.**
> - **P8** (`lines_prefetch` Vec/frame) and **P10** (`HashMap` metadata/span) —
>   the report already rates these low / deliberate tradeoffs; left as-is.

---

## Findings

### 🔴 P1 — Per-capture `String` clone in highlight inner loop

**`crates/hjkl-bonsai/src/highlighter.rs:892`** —
`capture_names[capture.index as usize].clone()`

Inside the hottest loop in the codebase (every tree-sitter match × every capture
within the match), each capture clones its name as a `String`. The
`HighlightSpan.capture` field (line 54) is typed `String`, forcing every span to
own a copy. For a 10k-token file, this produces ~10k `String` allocations per
highlight pass — per keystroke.

The `capture_names` `Vec<String>` is borrowed from `self.compiled` and outlives
the loop. The clone is unnecessary for any consumer that only needs `&str`.

**Fix:** Change `HighlightSpan.capture: String` to either (a) a `u32` index into
`capture_names`, or (b) `Arc<str>` shared from the compiled artifacts. Either
eliminates the per-span allocation. The index approach also avoids the
`iter().position()` scan at line 868.

---

### 🔴 P2 — `read_line` clones entire line per character during word motions

**`crates/hjkl-engine/src/motions.rs:941-942`** (`read_line_opt` at `:58-63`)

`char_at` calls `read_line_opt(buf, pos.row)` which returns `Option<String>` — a
full `String` clone of the line from the rope, via `Query::line`. Inside word
motion loops (`next_word_start:958`, `next_word_end:1103`), `char_at` is called
once per character examined. Scanning 200 chars across word boundaries means 200
full-line `String` allocations.

A 10k-char line scanned by `w`/`b`/`e` allocates 10k copies of that line's text.

**Fix:** Add a `char_at` variant (or change `read_line_opt`) that returns `&str`
by borrowing from the rope, avoiding the `String` allocation. The `Query::line`
→ `String` path forces the allocation; use `rope_line_str` (which returns
`String`) only once per row, or provide a rope reference that avoids the copy.

---

### 🔴 P3 — `is_keyword_char` re-parses `iskeyword` spec on every call

**`crates/hjkl-buffer/src/motion.rs:20-51`**

`is_keyword_char(c, spec)` calls `spec.split(',')` on every invocation — it
parses the `iskeyword` option string (e.g. `"@,48-57,_,192-255"`) from scratch
per character. Called from `char_kind` → `is_word` during every `w`/`b`/`e`/`ge`
character step. 200 chars × 4 tokens = 800 `split` iterations per word motion.

The spec never changes during editing; it's an option set once at startup.

**Fix:** Pre-parse the spec into a `Vec<Token>` (or `fn(char) -> bool` closure)
once when the option is set. Callers then use the pre-parsed form.

---

### 🟠 P4 — `format!` allocations on every render frame (statusline)

**`apps/hjkl/src/render.rs:150,158,161,183,188,220-236,243,326`**

The statusline is rendered every frame (~60 fps) and contains 8+ `format!`
calls, each allocating a temporary `String`. The diagnostic-count block (lines
204–237) does 4 separate `.filter().count()` passes over `lsp_diags` plus up to
5 `format!` allocations — all on data that changes only on LSP notifications
(rare).

The filename `to_owned()` at line 149, the position/percentage `format!` at
lines 158/161, and the loading-label `format!` at line 326 all run
unconditionally.

**Fix:** Pre-compute and memoize diag counts, invalidating on LSP change.
Replace filename clone with `Cow<str>`. Accumulate statusline parts into a
single `String` with `clear()` + `write!` (or `push_str`) instead of per-segment
`format!`.

---

### 🟠 P5 — `Vec::contains` linear scan in highlight cache eviction

**`crates/hjkl-bonsai/src/highlighter.rs:212,216`**

`evict_stale` operates on `cache_langs: Vec<String>` and
`cache_hashes: Vec<u64>` built at lines 1118–1119. The retain closures do:

- `keep_langs.iter().any(|kk| kk == k)` — O(|map| × |keep_langs|) per pass
- `keep_hashes.contains(h)` — O(|hashes| × |keep_hashes|) per pass

For documents with many injection blocks, both become quadratic. Build
`cache_langs` and `cache_hashes` as `HashSet` instead of `Vec` for O(1) lookup.
The `map` and `collect` at line 1118 can be replaced by `HashSet::from_iter`.

---

### 🟠 P6 — `capture_names.iter().position()` O(n) scan in nested match loop

**`crates/hjkl-bonsai/src/highlighter.rs:868`**

For each match that has pre-extracted directives, this scans the capture-names
Vec to find the index matching a capture name. Capture names are small (~10–50),
but this is inside a loop that already iterates all matches × all pre_extracted
directives.

**Fix:** Pre-build a `HashMap<&str, u32>` from name → capture index once at
compile time. Reuse across all matches. Eliminated entirely if P1 switches
capture names to indices.

---

### 🟡 P7 — Render-time `row_search_ranges` re-runs regex per visible line

**`crates/hjkl-buffer-tui/src/render.rs:1068-1072`**

Every frame with an active search runs `search_match_ranges(re, line)` on each
visible line. The engine's `SearchState` (`crates/hjkl-engine/src/search.rs`)
already caches per-row byte ranges keyed by `dirty_gen`. The renderer
independently re-does the regex work per frame, bypassing the cache.

**Fix:** Consult `SearchState::matches_for(row)` instead of calling
`search_match_ranges` directly. Cache gives O(1) lookup on hit, O(regex_scan) on
miss.

---

### 🟡 P8 — `lines_prefetch: Vec<String>` allocates every frame

**`crates/hjkl-buffer-tui/src/render.rs:483-485`**

Every frame allocates a `Vec<String>` of `area.height` (~50) cloned `String`s
from the rope. Used to feed `Cow::Borrowed` accessors during the render walk
loop, which avoids further per-line clones. This is a deliberate tradeoff
(documentation at line 491: "Avoids a String clone per visible row") and 50
allocations per frame is acceptable, but worth noting.

**Fix:** Could be avoided with a `rope.slice()` that returns borrowed data, or a
lightweight line-buffer struct reused across frames. Low priority — the 50-line
allocation is cheap compared to the highlight work.

---

### 🟡 P9 — `which_key` `pending.clone()` + `Chord` allocation per frame

**`apps/hjkl/src/render.rs:3020`** —
`hjkl_keymap::Chord(pending.clone()).to_notation(leader)`

When the which-key popup is visible, each frame clones the pending key Vec and
formats it to a notation string. The pending keys change only on keypress, not
every frame.

**Fix:** Cache the formatted header string, invalidate on prefix change.

---

### ⚪ P10 — `HighlightSpan.metadata: HashMap<String, MetaValue>` per span

**`crates/hjkl-bonsai/src/highlighter.rs:57,897-910`**

Every `HighlightSpan` carries a `HashMap`. The empty-map path is short-circuited
at line 897–910 (common case returns `HashMap::new()`, which is zero-alloc until
first insert) — good. Spans WITH metadata (from `#set!` directives, rare in
practice) allocate per span. Directives are typically only present in injection
patterns.

**Fix:** Low priority. Could use `Box<HashMap<…>>` or `Option` to save 48 bytes
per span in the common no-metadata case.

---

### ⚪ P11 — `Range<usize>.clone()` — Range is Copy (cosmetic)

**`crates/hjkl-bonsai/src/highlighter.rs:752,1125,1140,1171`**

`Range<usize>` implements `Copy`. The `.clone()` calls compile to identical code
but signal a misunderstanding. No runtime cost — cosmetic only.

---

## Data Structure Audit

| Location                 | Current                    | Issue                        | Fix                       |
| ------------------------ | -------------------------- | ---------------------------- | ------------------------- |
| `highlighter.rs:54`      | `capture: String`          | Per-span alloc               | `u32` index or `Arc<str>` |
| `highlighter.rs:1118`    | `cache_langs: Vec<String>` | O(n) `.any()` in retain      | `HashSet<String>`         |
| `highlighter.rs:1119`    | `cache_hashes: Vec<u64>`   | O(n) `.contains()` in retain | `HashSet<u64>`            |
| `highlighter.rs:357`     | `AHashMap<u64, Arc<…>>`    | Good — fast hasher           | —                         |
| `highlighter.rs:163,176` | Nested `HashMap` for cache | Good — O(1)                  | —                         |
| `motion.rs:20`           | `spec.split(',')` per call | Re-parsed per char           | Pre-parse once            |

## Positive Findings

- **`ChildCache` eviction** correctly prunes to current working set only.
- **`SearchState`** caches per-row byte ranges with `dirty_gen` invalidation.
- **`COMPILED_CACHE`** global `ahash::AHashMap` avoids re-parsing queries.
- **`sync_after_engine_mutation`** compares
  `(buffer, top_row, height, dirty_gen)` to skip redundant recompute.
- **`folds_override`** uses `saturating_add(1)` to avoid wrapping.
- **Renderer `line_at`** returns `Cow::Borrowed` for prefetched rows, avoiding
  per-cell cloning.
- **Tree-sitter `parse_timeout_micros`** bounds parse work on huge files.
- **`parse_incremental`** skipped `changed_ranges` call that was 54% of
  per-keystroke CPU on huge files.
- **Swap file I/O** uses `O_EXCL` + `create_new(true)` and explicit fsync.
- **Subprocess lifecycle** properly timed out, killed, and waited.

---

## Summary

| Rank | File:Line                   | Issue                                     | Hot Path               | Impact |
| ---- | --------------------------- | ----------------------------------------- | ---------------------- | ------ |
| P1   | `highlighter.rs:892`        | `capture_name.clone()` per capture        | Every keystroke        | 🔴     |
| P2   | `motions.rs:69,941`         | `read_line` clones line per char          | Every word motion      | 🔴     |
| P3   | `buffer/motion.rs:20`       | `is_keyword_char` re-parses spec per char | Every word motion      | 🔴     |
| P4   | `render.rs:150-326`         | 8+ `format!` per frame                    | Every frame            | 🟠     |
| P5   | `highlighter.rs:212,216`    | `Vec::contains` in evict_stale            | Every highlight pass   | 🟠     |
| P6   | `highlighter.rs:868`        | `.position()` scan in nested loop         | Every highlight pass   | 🟠     |
| P7   | `buffer-tui/render.rs:1068` | Regex re-scan bypasses engine cache       | Every frame (search)   | 🟡     |
| P8   | `buffer-tui/render.rs:483`  | `lines_prefetch` Vec per frame            | Every frame            | 🟡     |
| P9   | `render.rs:3020`            | `pending.clone()` per frame               | Which-key visible      | 🟡     |
| P10  | `highlighter.rs:57`         | `HashMap` per span (metadata)             | Rare (directives only) | ⚪     |
| P11  | `highlighter.rs:752,etc`    | `Range.clone()` (Copy type)               | Cosmetic               | ⚪     |
