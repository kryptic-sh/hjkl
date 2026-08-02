//! Tree-sitter fold extraction.
//!
//! Bundles `folds.scm` queries for supported languages and exposes
//! [`extract_fold_ranges`] / [`extract_fold_ranges_rope`] on [`Highlighter`]
//! to compute `(start_row, end_row)` pairs for auto-folding.
//!
//! ## Design
//!
//! - Queries are bundled via `include_str!`, keyed by grammar name.
//! - The compiled `Query` is cached in a process-global map keyed by
//!   grammar name (cheap — at most N entries for bundled grammars).
//! - Fold extraction runs over the FULL tree (not viewport-bounded), once
//!   per reparse — one query pass, no per-frame cost.
//! - Only nodes spanning more than one row produce folds; single-row nodes
//!   are silently skipped.
//! - Vim convention: `start_row` is the visible "header" line;
//!   `start_row+1..=end_row` are hidden when the fold is closed.
//! - Injected languages fold too, via
//!   [`extract_fold_ranges_rope_with_injections`]: each region the host's
//!   `injections.scm` reports is parsed with its own grammar and folded with
//!   that language's query, and the rows are offset into the host document.
//!   Regions are memoised by content ([`InjectedFoldCache`]) so an edit
//!   re-parses only the region it touched.

use std::collections::{BTreeMap, HashMap};
use std::ops::Range;
use std::sync::{Arc, LazyLock, RwLock};

use tree_sitter::{Query, QueryCursor, StreamingIterator as _};

use crate::highlighter::{INJ_CONTENT_CAPTURE, INJ_LANG_CAPTURE};
use crate::runtime::Grammar;

// ---------------------------------------------------------------------------
// Bundled query content
// ---------------------------------------------------------------------------

/// Return the bundled `folds.scm` source for `lang`, or `None` when we
/// don't ship one.
pub fn builtin_folds(lang: &str) -> Option<&'static str> {
    match lang {
        "rust" => Some(include_str!("../queries/folds/rust.scm")),
        "python" => Some(include_str!("../queries/folds/python.scm")),
        "javascript" => Some(include_str!("../queries/folds/javascript.scm")),
        "typescript" => Some(include_str!("../queries/folds/typescript.scm")),
        "go" => Some(include_str!("../queries/folds/go.scm")),
        "c" => Some(include_str!("../queries/folds/c.scm")),
        "cpp" => Some(include_str!("../queries/folds/cpp.scm")),
        "json" => Some(include_str!("../queries/folds/json.scm")),
        "bash" => Some(include_str!("../queries/folds/bash.scm")),
        "lua" => Some(include_str!("../queries/folds/lua.scm")),
        "java" => Some(include_str!("../queries/folds/java.scm")),
        "html" => Some(include_str!("../queries/folds/html.scm")),
        "css" => Some(include_str!("../queries/folds/css.scm")),
        "scss" => Some(include_str!("../queries/folds/scss.scm")),
        "yaml" => Some(include_str!("../queries/folds/yaml.scm")),
        "toml" => Some(include_str!("../queries/folds/toml.scm")),
        "ruby" => Some(include_str!("../queries/folds/ruby.scm")),
        "php" => Some(include_str!("../queries/folds/php.scm")),
        // `bonsai.toml` carries BOTH `[language.c-sharp]` (helix queries) and
        // `[language.c_sharp]` (nvim-treesitter queries) for the same grammar,
        // and `GrammarRegistry` resolves an extension to the alphabetically
        // first entry — so `.cs` loads under the name `c-sharp`. Keying only
        // `c_sharp` here left every C# buffer with zero folds.
        "c_sharp" | "c-sharp" => Some(include_str!("../queries/folds/c_sharp.scm")),
        "markdown" => Some(include_str!("../queries/folds/markdown.scm")),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Compiled-query cache
// ---------------------------------------------------------------------------

/// Cache of compiled fold queries keyed by grammar name.
///
/// `None` = no bundled query for this grammar (or compilation failed).
static FOLD_QUERY_CACHE: LazyLock<RwLock<HashMap<String, Option<CompiledFolds>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Compiled fold query + the index of the `@fold` capture.
struct CompiledFolds {
    query: Query,
    fold_idx: u32,
    /// Per pattern index: does it carry `(#trim! @fold)`? See
    /// [`fold_end_row`] for what trimming does and why it is opt-in.
    trim_patterns: Box<[bool]>,
}

// SAFETY: tree_sitter::Query has unsafe Send+Sync impls.
unsafe impl Send for CompiledFolds {}
unsafe impl Sync for CompiledFolds {}

/// Get-or-compile the fold query for `grammar`. Returns `None` when no
/// bundled query exists for this grammar or compilation fails.
fn get_or_compile(grammar: &Grammar) -> Option<()> {
    // Fast read path.
    {
        let r = FOLD_QUERY_CACHE.read().unwrap();
        if r.contains_key(grammar.name()) {
            return r.get(grammar.name()).and_then(|v| v.as_ref()).map(|_| ());
        }
    }
    // Slow compile path — only reaches here on the first call per grammar.
    let src = builtin_folds(grammar.name())?;
    let compiled = match Query::new(grammar.language(), src) {
        Ok(q) => {
            let names = q.capture_names();
            let fold_idx = names.iter().position(|n| *n == "fold").map(|i| i as u32);
            match fold_idx {
                Some(fi) => {
                    // `(#trim! @fold)` is a directive, so tree-sitter reports
                    // it as a general predicate rather than applying it.
                    // Record which patterns asked for it; `fold_end_row` does
                    // the trimming for matches of those patterns only.
                    let trim_patterns: Box<[bool]> = (0..q.pattern_count())
                        .map(|pi| {
                            q.general_predicates(pi).iter().any(|p| {
                                &*p.operator == "trim!"
                                    && p.args.iter().any(|a| {
                                        matches!(a, tree_sitter::QueryPredicateArg::Capture(c) if *c == fi)
                                    })
                            })
                        })
                        .collect();
                    Some(CompiledFolds {
                        query: q,
                        fold_idx: fi,
                        trim_patterns,
                    })
                }
                None => {
                    tracing::warn!(
                        grammar = grammar.name(),
                        "folds.scm missing @fold capture — fold extraction disabled"
                    );
                    None
                }
            }
        }
        Err(e) => {
            tracing::warn!(
                grammar = grammar.name(),
                error = %e,
                "folds.scm failed to compile — fold extraction disabled"
            );
            None
        }
    };

    let present = compiled.is_some();
    FOLD_QUERY_CACHE
        .write()
        .unwrap()
        .insert(grammar.name().to_string(), compiled);
    if present { Some(()) } else { None }
}

// ---------------------------------------------------------------------------
// Fold range end adjustment
// ---------------------------------------------------------------------------

/// Last row a fold should hide, given the captured node's end position.
///
/// A node's `end_position` is exclusive, so two adjustments are needed before
/// it names a row to hide. Both only bite on grammars whose blocks are
/// delimited by line structure rather than a closing token — markdown is the
/// visible case; C-like grammars end their nodes ON the `}` and come through
/// unchanged.
///
/// - **End at column 0 covers nothing on that row.** The node stopped at the
///   previous row's newline. tree-sitter-markdown's `(section)` ends at the
///   start of the NEXT heading, so without this every section fold hid the
///   following section's heading line, and the last one ran one row past the
///   end of the buffer.
/// - **Trailing blank rows are trimmed when `trim` is set**, which the caller
///   takes from `(#trim! @fold)` on the matched pattern. Markdown's query
///   asks for it, so a section ending with a blank line before the next
///   heading folds to its last line of text and the blank separator stays
///   visible. Queries that do NOT ask keep the blank rows, matching vim: a
///   TOML table folds through the blank line before the next `[table]`.
///
/// `is_blank(row)` reports whether that row is empty or all whitespace.
/// Trimming stops at `start_row`, which is never trimmed away — the caller
/// still drops the fold when the result is not past `start_row`.
fn fold_end_row(
    start_row: usize,
    end_row: usize,
    end_col: usize,
    trim: bool,
    is_blank: impl Fn(usize) -> bool,
) -> usize {
    let mut end = if end_col == 0 && end_row > 0 {
        end_row - 1
    } else {
        end_row
    };
    if trim {
        while end > start_row && is_blank(end) {
            end -= 1;
        }
    }
    end
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Compute fold ranges from the full parse tree using a bundled `folds.scm`.
///
/// Returns a sorted `Vec<(start_row, end_row)>` (both inclusive, 0-based).
/// Only nodes spanning more than one row are included. Deduplicated by
/// `start_row` — when two captures share a start row, the larger span wins.
///
/// Returns an empty vec when:
/// - No bundled folds.scm for this grammar.
/// - Compilation failed.
/// - `tree` is `None`.
///
/// **Bounded**: one `QueryCursor` pass over the full tree — O(N nodes).
/// No recursion, no unbounded loops.
pub fn extract_fold_ranges(
    tree: &tree_sitter::Tree,
    grammar: &Grammar,
    source: &[u8],
) -> Vec<(usize, usize)> {
    let mut by_start: BTreeMap<usize, usize> = BTreeMap::new();
    collect_folds_into(&mut by_start, tree, grammar, source, 0);
    by_start.into_iter().collect()
}

/// Run `grammar`'s bundled fold query over `tree` (parsed from `source`) and
/// merge the results into `by_start`, shifting every row by `row_offset`.
///
/// `row_offset` is what makes injected regions land in the host document's
/// coordinates: a region that begins on host row 42 passes `row_offset = 42`,
/// so a fold at region-relative row 3 is recorded at host row 45.
///
/// Everything else — the `end_col == 0` rule and `(#trim! @fold)`'s blank-row
/// walk — is evaluated in `source`'s OWN coordinates, which for an injected
/// region means the injected text's rows and columns. That is deliberate:
/// column 0 of a region row is column 0 of the corresponding host row for
/// every row after the first (a region is a contiguous byte slice, so each of
/// its rows past the first starts exactly where the host's row starts), and
/// no multi-row fold can END on region row 0.
///
/// Rows are deduplicated by start row, largest span winning — see
/// [`extract_fold_ranges`].
fn collect_folds_into(
    by_start: &mut BTreeMap<usize, usize>,
    tree: &tree_sitter::Tree,
    grammar: &Grammar,
    source: &[u8],
    row_offset: usize,
) {
    if get_or_compile(grammar).is_none() {
        return;
    }

    let cache = FOLD_QUERY_CACHE.read().unwrap();
    let Some(compiled) = cache.get(grammar.name()).and_then(|v| v.as_ref()) else {
        return;
    };

    let mut cursor = QueryCursor::new();
    // Run over the entire tree — no byte_range restriction.
    let root = tree.root_node();
    let mut matches = cursor.matches(&compiled.query, root, source);

    // Row → is-blank lookup for `fold_end_row`. One pass over the source,
    // same order of cost as the query itself, and only on reparse.
    let blank_rows: Vec<bool> = source
        .split(|&b| b == b'\n')
        .map(|line| match std::str::from_utf8(line) {
            Ok(s) => s.chars().all(char::is_whitespace),
            Err(_) => line.iter().all(u8::is_ascii_whitespace),
        })
        .collect();
    let is_blank = |row: usize| blank_rows.get(row).copied().unwrap_or(true);

    let fold_idx = compiled.fold_idx;

    while let Some(m) = matches.next() {
        let trim = compiled
            .trim_patterns
            .get(m.pattern_index)
            .copied()
            .unwrap_or(false);
        for cap in m.captures {
            if cap.index != fold_idx {
                continue;
            }
            let node = cap.node;
            let start_row = node.start_position().row;
            let end_row = fold_end_row(
                start_row,
                node.end_position().row,
                node.end_position().column,
                trim,
                is_blank,
            );
            // Skip single-line nodes — no meaningful fold.
            if end_row <= start_row {
                continue;
            }
            let (start_row, end_row) = (start_row + row_offset, end_row + row_offset);
            by_start
                .entry(start_row)
                .and_modify(|e| {
                    if end_row > *e {
                        *e = end_row;
                    }
                })
                .or_insert(end_row);
        }
    }
}

/// Rope-backed variant of [`extract_fold_ranges`].
///
/// Same semantics as `extract_fold_ranges`, and — like it — folds only the
/// TOP-LEVEL tree with the top-level grammar. Use
/// [`extract_fold_ranges_rope_with_injections`] to also fold embedded
/// languages.
pub fn extract_fold_ranges_rope(
    tree: &tree_sitter::Tree,
    grammar: &Grammar,
    rope: &ropey::Rope,
) -> Vec<(usize, usize)> {
    let mut cache = InjectedFoldCache::default();
    extract_fold_ranges_rope_with_injections(tree, grammar, rope, None, &mut cache, |_| None)
}

/// Per-buffer memo of the folds an injected region produced, keyed by
/// `(language, content hash)` and holding **region-relative** rows.
///
/// Fold extraction runs once per reparse, i.e. once per edit. Without this,
/// every edit re-parses every injected region in the document — for a Rust
/// file, whose `injections.scm` injects Rust into each multi-line macro body,
/// that measured as a doubling of the whole fold pass (37.5 ms → 77.0 ms on
/// `hjkl-engine/src/editor.rs`) for two extra folds. Keying on the content
/// hash means an edit only re-parses the ONE region it touched; keying on the
/// content rather than the byte range means text that merely moved (rows
/// inserted above it) still hits.
///
/// After each extraction the map is pruned to the keys that extraction
/// actually used, so it cannot outgrow the document's live injection set.
#[derive(Default)]
pub struct InjectedFoldCache {
    entries: HashMap<(String, u64), Vec<(usize, usize)>>,
}

impl InjectedFoldCache {
    /// Number of memoised regions. Test/instrumentation hook.
    #[doc(hidden)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when nothing is memoised.
    #[doc(hidden)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Thread-local count of injected regions actually PARSED (i.e. memo misses).
///
/// Test instrumentation, like [`crate::parse_counter`]: it is the only way to
/// tell a memo hit from a silent recompute, since both produce identical fold
/// ranges. Compiled in all modes, hidden from the docs.
#[doc(hidden)]
pub mod injected_parse_counter {
    use std::cell::Cell;

    thread_local! {
        static COUNT: Cell<u64> = const { Cell::new(0) };
    }

    pub(super) fn increment() {
        COUNT.with(|c| c.set(c.get() + 1));
    }

    /// Read the current counter value.
    pub fn get() -> u64 {
        COUNT.with(Cell::get)
    }

    /// Reset the counter to zero.
    pub fn reset() {
        COUNT.with(|c| c.set(0));
    }
}

/// [`extract_fold_ranges_rope`] plus folds from every injected region.
///
/// For each region `injection_query` reports (a ` ```rust ` block in markdown,
/// a `run: |` shell script in a workflow YAML), the region's own grammar is
/// resolved through `resolve`, the region text is parsed with it, THAT
/// language's fold query runs over the region, and the resulting rows are
/// shifted into the host document's coordinates before being merged with the
/// host's own folds.
///
/// Degradation is per-region and silent, matching "no folds for this
/// language": a region whose language has no bundled fold query is skipped
/// **before** `resolve` is called (so an uninstalled grammar is never fetched
/// just to find out we could not have folded it), and a region whose grammar
/// `resolve` cannot produce is skipped too. Neither aborts the host's folds.
///
/// Merging is the same `by_start` map the host path uses, so a host fold and
/// an injected fold that share a start row collapse to the widest of the two.
/// That is deliberate and matches what the consumer can represent:
/// `View::set_auto_folds` keys folds by start row as well, so a second fold on
/// the same row would be dropped downstream regardless (`docs/backlog.md`
/// §1.4b).
///
/// Only ONE level of injection is descended — an injection inside an injected
/// region (bash inside YAML inside a markdown fence) is not folded.
///
/// **Cost**, once per reparse and never per frame: one extra query pass over
/// the host tree to find the regions, plus — for each region that is not
/// already in `cache` — one parse of that region's bytes. See
/// [`InjectedFoldCache`] for the measured numbers.
pub fn extract_fold_ranges_rope_with_injections<F>(
    tree: &tree_sitter::Tree,
    grammar: &Grammar,
    rope: &ropey::Rope,
    injection_query: Option<&Query>,
    cache: &mut InjectedFoldCache,
    mut resolve: F,
) -> Vec<(usize, usize)>
where
    F: FnMut(&str) -> Option<Arc<Grammar>>,
{
    // `QueryCursor::matches` needs a `TextProvider`; for a rope that means one
    // contiguous materialisation. O(N) once per reparse (not per frame), and
    // the same buffer is reused for the injection query and every region
    // slice below.
    let text = rope.to_string();
    let source = text.as_bytes();

    let mut by_start: BTreeMap<usize, usize> = BTreeMap::new();
    collect_folds_into(&mut by_start, tree, grammar, source, 0);

    let Some(inj_query) = injection_query else {
        return by_start.into_iter().collect();
    };

    // One parser + grammar handle per injected language, reused across that
    // language's regions. The `Arc<Grammar>` must be kept alive alongside the
    // parser: it owns the dlopen'd shared library the parser's `Language`
    // points into.
    let mut parsers: HashMap<String, (Arc<Grammar>, tree_sitter::Parser)> = HashMap::new();

    // Keys this extraction actually used — everything else is evicted below.
    let mut live: std::collections::HashSet<(String, u64)> = std::collections::HashSet::new();

    for (lang, range) in injection_regions(tree, inj_query, source) {
        // Two cheap gates before anything expensive runs.
        //
        // A single-row region cannot produce a fold at all — `collect_folds_into`
        // drops every capture whose `end_row <= start_row`. Skipping those here
        // is not an optimisation of a rare case: Rust injects itself into every
        // `macro_invocation`'s token tree, so a normal Rust file has hundreds of
        // one-line `println!(…)` / `vec![…]` regions, and parsing each one cost
        // more than the whole host extraction (measured: 37.5 ms → 77.0 ms on
        // `hjkl-engine/src/editor.rs`, for two extra folds).
        if rope.byte_to_line(range.start) == rope.byte_to_line(range.end.saturating_sub(1)) {
            continue;
        }
        // No bundled fold query for this language means the region can produce
        // nothing either, so do not pay `resolve` (which may clone + compile a
        // grammar) for it.
        if builtin_folds(&lang).is_none() {
            continue;
        }
        let sub = &source[range.clone()];
        let key = (lang, crate::highlighter::hash_bytes(sub));
        // Region-relative row 0 is this host row. THIS is what puts an
        // injected fold in the host document's coordinates — the cached rows
        // below are region-relative, so the same block of code memoised once
        // still lands correctly after rows are inserted above it.
        let row_offset = rope.byte_to_line(range.start);

        if !cache.entries.contains_key(&key) {
            let entry = match parsers.get_mut(&key.0) {
                Some(e) => e,
                None => {
                    let Some(child) = resolve(&key.0) else {
                        continue;
                    };
                    let mut parser = tree_sitter::Parser::new();
                    if parser.set_language(child.language()).is_err() {
                        continue;
                    }
                    parsers.entry(key.0.clone()).or_insert((child, parser))
                }
            };
            let (child, parser) = (&entry.0, &mut entry.1);
            injected_parse_counter::increment();
            let Some(sub_tree) = parser.parse(sub, None) else {
                continue;
            };
            let mut region_folds: BTreeMap<usize, usize> = BTreeMap::new();
            collect_folds_into(&mut region_folds, &sub_tree, child, sub, 0);
            cache
                .entries
                .insert(key.clone(), region_folds.into_iter().collect());
        }

        // Unwrap-free: just inserted above when it was missing.
        if let Some(region_folds) = cache.entries.get(&key) {
            for &(s, e) in region_folds {
                let (s, e) = (s + row_offset, e + row_offset);
                by_start
                    .entry(s)
                    .and_modify(|prev| {
                        if e > *prev {
                            *prev = e;
                        }
                    })
                    .or_insert(e);
            }
            live.insert(key);
        }
    }

    // Bound the memo at this document's live injection set.
    cache.entries.retain(|k, _| live.contains(k));

    by_start.into_iter().collect()
}

// ---------------------------------------------------------------------------
// Injected regions
// ---------------------------------------------------------------------------

/// Discover every injected region in `tree` as `(language, byte_range)`.
///
/// Mirrors the region discovery in
/// [`Highlighter::highlight_range_with_injections_rope`][hl], with two
/// differences: it runs over the WHOLE document (folds are not viewport
/// bounded), and it applies `(#offset! @injection.content …)`, which the
/// highlight path ignores. That directive is load-bearing here — a YAML
/// `run: |` block scalar's node starts ON the `|`, and nvim-treesitter's YAML
/// injections shift the content start one column right to drop it. Feeding
/// the `|` to the bash parser makes the whole script one ERROR node with no
/// folds inside it.
///
/// [hl]: crate::Highlighter::highlight_range_with_injections_rope
fn injection_regions(
    tree: &tree_sitter::Tree,
    inj_query: &Query,
    source: &[u8],
) -> Vec<(String, Range<usize>)> {
    let names = inj_query.capture_names();
    let lang_idx = names
        .iter()
        .position(|n| *n == INJ_LANG_CAPTURE)
        .map(|i| i as u32);
    let Some(content_idx) = names.iter().position(|n| *n == INJ_CONTENT_CAPTURE) else {
        return Vec::new();
    };
    let content_idx = content_idx as u32;

    let mut out: Vec<(String, Range<usize>)> = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(inj_query, tree.root_node(), source);

    while let Some(m) = matches.next() {
        let mut lang_name: Option<String> = None;
        let mut content_range: Option<Range<usize>> = None;

        for cap in m.captures {
            if Some(cap.index) == lang_idx {
                let (s, e) = (cap.node.start_byte(), cap.node.end_byte());
                if s < e && e <= source.len() {
                    lang_name = std::str::from_utf8(&source[s..e])
                        .ok()
                        .map(|s| s.trim().to_string());
                }
            } else if cap.index == content_idx {
                let (s, e) = (cap.node.start_byte(), cap.node.end_byte());
                if s < e && e <= source.len() {
                    content_range = Some(s..e);
                }
            }
        }

        // Pattern-level `(#set! injection.language "foo")`.
        if lang_name.is_none() {
            for prop in inj_query.property_settings(m.pattern_index) {
                if prop.key.as_ref() == INJ_LANG_CAPTURE
                    && let Some(v) = prop.value.as_deref()
                {
                    lang_name = Some(v.trim().to_string());
                    break;
                }
            }
        }

        let (Some(name), Some(range)) = (lang_name, content_range) else {
            continue;
        };
        if name.is_empty()
            || name.len() > 64
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            continue;
        }
        let range = apply_offset_directive(inj_query, m.pattern_index, content_idx, source, range);
        if range.start < range.end {
            out.push((name, range));
        }
    }
    out
}

/// Apply `(#offset! @injection.content srow scol erow ecol)` to `range`.
///
/// The four arguments are row/column DELTAS on the capture's start and end
/// positions, in tree-sitter's own units (columns are byte columns). Returns
/// `range` unchanged when the pattern carries no `#offset!` for this capture,
/// when the argument list is not the expected shape, or when applying it would
/// invert or escape the range.
fn apply_offset_directive(
    query: &Query,
    pattern_index: usize,
    capture_idx: u32,
    source: &[u8],
    range: Range<usize>,
) -> Range<usize> {
    use tree_sitter::QueryPredicateArg;

    for pred in query.general_predicates(pattern_index) {
        if &*pred.operator != "offset!" {
            continue;
        }
        let mut args = pred.args.iter();
        match args.next() {
            Some(QueryPredicateArg::Capture(c)) if *c == capture_idx => {}
            _ => continue,
        }
        let deltas: Vec<i64> = args
            .filter_map(|a| match a {
                QueryPredicateArg::String(s) => s.parse::<i64>().ok(),
                _ => None,
            })
            .collect();
        let [srow, scol, erow, ecol] = deltas[..] else {
            continue;
        };
        let start = shift_byte(source, range.start, srow, scol);
        let end = shift_byte(source, range.end, erow, ecol);
        if start < end {
            return start..end;
        }
        return range;
    }
    range
}

/// Move `byte` by `drow` lines and `dcol` byte-columns within `source`.
///
/// Rows move by scanning for newlines; the column delta is then applied to the
/// resulting offset. The result is clamped into `0..=source.len()` and nudged
/// forward to the next UTF-8 char boundary so the caller can slice with it.
fn shift_byte(source: &[u8], byte: usize, drow: i64, dcol: i64) -> usize {
    let mut pos = byte.min(source.len());
    // Row deltas: forward to the byte after the next newline, backward to the
    // byte after the previous one.
    for _ in 0..drow.unsigned_abs() {
        if drow > 0 {
            match source[pos..].iter().position(|&b| b == b'\n') {
                Some(off) => pos += off + 1,
                None => {
                    pos = source.len();
                    break;
                }
            }
        } else {
            // Step back over the newline that ends the previous row, then to
            // the start of that row.
            let before = &source[..pos.saturating_sub(1)];
            match before.iter().rposition(|&b| b == b'\n') {
                Some(off) => pos = off + 1,
                None => {
                    pos = 0;
                    break;
                }
            }
        }
    }
    pos = if dcol >= 0 {
        pos.saturating_add(dcol as usize).min(source.len())
    } else {
        pos.saturating_sub(dcol.unsigned_abs() as usize)
    };
    // Never land inside a multi-byte char.
    while pos < source.len() && (source[pos] & 0xC0) == 0x80 {
        pos += 1;
    }
    pos
}

// ---------------------------------------------------------------------------
// Marker folds (foldmethod=marker) — grammar-independent
// ---------------------------------------------------------------------------

/// The default vim fold markers (`foldmarker={{{,}}}`).
pub const DEFAULT_FOLD_MARKER_OPEN: &str = "{{{";
pub const DEFAULT_FOLD_MARKER_CLOSE: &str = "}}}";

/// The universal "region" marker pair (`#region` / `#endregion`), recognized in
/// addition to [`DEFAULT_FOLD_MARKER_OPEN`] under `foldmethod=marker`. This is
/// the Visual Studio / VS Code convention and appears across many comment
/// syntaxes (`// #region`, `# #region`, `<!-- #region -->`, `;; #region`).
///
/// Note: `#endregion` does not start with `#region`, so the two never collide
/// in the left-to-right scan.
pub const DEFAULT_REGION_MARKER_OPEN: &str = "#region";
pub const DEFAULT_REGION_MARKER_CLOSE: &str = "#endregion";

/// Scan `rope` for vim-style marker folds (`open` / `close`, default
/// `{{{` / `}}}`).
///
/// This is **grammar-independent** — it works on any text (including plain
/// `.txt`) because the markers are matched literally, not via tree-sitter.
/// Markers normally live inside comments (`// {{{`, `# }}}`, `-- {{{`) but,
/// matching vim, the marker text is found anywhere on a line regardless of
/// comment syntax.
///
/// Pairing is stack-based: each `open` pushes the current row, each `close`
/// pops the most-recent open and emits `(start_row, end_row)`. Within a single
/// line, markers are processed left-to-right (so `}}} … {{{` on one line closes
/// then re-opens). A `close` with no matching `open` is ignored; an unclosed
/// `open` at end-of-file is dropped — exactly like vim.
///
/// An optional level digit after a marker (vim's `{{{1`) is accepted and
/// ignored — folds are paired by nesting, not by explicit level.
///
/// Returns sorted `(start_row, end_row)` pairs (both inclusive, 0-based),
/// deduplicated by `start_row` (largest span wins) to match
/// [`extract_fold_ranges`]. Single-line folds (`open` and `close` on the same
/// row) are skipped.
///
/// **Bounded**: one linear pass over the rope — O(N bytes). No recursion.
pub fn extract_marker_fold_ranges_rope(
    rope: &ropey::Rope,
    open: &str,
    close: &str,
) -> Vec<(usize, usize)> {
    extract_marker_fold_ranges_rope_multi(rope, &[(open, close)])
}

/// Multi-pair variant of [`extract_marker_fold_ranges_rope`].
///
/// Scans `rope` for every `(open, close)` marker pair in `pairs`, pairing each
/// pair independently (its own stack), and merges all results into one sorted,
/// `start_row`-deduplicated list (largest span wins). Use this to recognize the
/// configured `foldmarker` *and* the universal `#region` / `#endregion` pair in
/// a single call. Empty-delimiter pairs are skipped.
///
/// **Bounded**: `pairs.len()` linear passes over the rope — O(P·N bytes).
pub fn extract_marker_fold_ranges_rope_multi(
    rope: &ropey::Rope,
    pairs: &[(&str, &str)],
) -> Vec<(usize, usize)> {
    // Keyed by start_row; value = largest end_row seen — mirrors the TS path.
    let mut by_start: std::collections::BTreeMap<usize, usize> = std::collections::BTreeMap::new();

    for &(open, close) in pairs {
        let ob = open.as_bytes();
        let cb = close.as_bytes();
        if ob.is_empty() || cb.is_empty() {
            continue;
        }

        let mut stack: Vec<usize> = Vec::new();
        let mut scratch = String::new();
        for (row, line) in rope.lines().enumerate() {
            // Borrow the line bytes without allocating when the chunk is contiguous.
            let bytes: &[u8] = match line.as_str() {
                Some(s) => s.as_bytes(),
                None => {
                    scratch.clear();
                    scratch.extend(line.chunks());
                    scratch.as_bytes()
                }
            };

            let mut i = 0;
            while i < bytes.len() {
                if bytes[i..].starts_with(ob) {
                    stack.push(row);
                    i += ob.len();
                } else if bytes[i..].starts_with(cb) {
                    if let Some(start) = stack.pop()
                        && row > start
                    {
                        by_start
                            .entry(start)
                            .and_modify(|e| {
                                if row > *e {
                                    *e = row;
                                }
                            })
                            .or_insert(row);
                    }
                    i += cb.len();
                } else {
                    i += 1;
                }
            }
        }
    }

    by_start.into_iter().collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── fold_end_row ────────────────────────────────────────────────────

    /// A node ending at column 0 covers nothing on that row. Markdown's
    /// `(section)` ends at the start of the NEXT heading, so without this
    /// every section fold hid the following heading line.
    #[test]
    fn fold_end_row_drops_a_row_the_node_only_touches_at_column_0() {
        let never_blank = |_| false;
        assert_eq!(fold_end_row(4, 14, 0, false, never_blank), 13);
        // Ending mid-row (a `}` at column 0 is still column 0; a `}` indented
        // is not) keeps the row — that is the C-like case.
        assert_eq!(fold_end_row(4, 14, 1, false, never_blank), 14);
        assert_eq!(fold_end_row(4, 14, 12, false, never_blank), 14);
    }

    /// Trailing blank rows come off only when the pattern asked for it with
    /// `(#trim! @fold)`. Markdown asks; TOML does not, and vim folds a TOML
    /// table through the blank line before the next `[table]`.
    #[test]
    fn fold_end_row_trims_trailing_blanks_only_when_asked() {
        // rows 12 and 13 blank, 11 is text.
        let blank = |row: usize| row == 12 || row == 13;
        assert_eq!(fold_end_row(4, 14, 0, true, blank), 11, "trim requested");
        assert_eq!(fold_end_row(4, 14, 0, false, blank), 13, "no trim");
    }

    /// Trimming never eats the fold's own header row: a section whose body is
    /// entirely blank collapses to `start == end`, which the callers drop.
    #[test]
    fn fold_end_row_trim_stops_at_the_start_row() {
        let all_blank = |_| true;
        assert_eq!(fold_end_row(7, 10, 0, true, all_blank), 7);
    }

    /// Row 0 has no previous row to fall back to.
    #[test]
    fn fold_end_row_handles_row_zero() {
        assert_eq!(fold_end_row(0, 0, 0, true, |_| false), 0);
    }

    // ── shift_byte (the `#offset!` primitive) ───────────────────────────

    #[test]
    fn shift_byte_applies_a_column_delta() {
        // "ab\ncd\n": +1 from the start of row 1 lands on `d`.
        let src = b"ab\ncd\n";
        assert_eq!(shift_byte(src, 3, 0, 1), 4);
        assert_eq!(shift_byte(src, 3, 0, -1), 2);
    }

    #[test]
    fn shift_byte_applies_a_row_delta() {
        let src = b"ab\ncd\nef\n";
        // From the start of row 0, +1 row is the start of row 1 (`c` at 3).
        assert_eq!(shift_byte(src, 0, 1, 0), 3);
        assert_eq!(shift_byte(src, 0, 2, 0), 6);
        // Back from row 2 to row 1, and combined with a column delta.
        assert_eq!(shift_byte(src, 6, -1, 0), 3);
        assert_eq!(shift_byte(src, 6, -1, 1), 4);
    }

    #[test]
    fn shift_byte_clamps_at_both_ends() {
        let src = b"ab\ncd\n";
        assert_eq!(shift_byte(src, 3, 9, 0), src.len(), "past EOF clamps");
        assert_eq!(shift_byte(src, 3, -9, 0), 0, "before BOF clamps");
        assert_eq!(shift_byte(src, 5, 0, 99), src.len());
        assert_eq!(shift_byte(src, 1, 0, -99), 0);
    }

    /// A column delta must never leave the offset inside a multi-byte char —
    /// the caller slices `source[range]` with it.
    #[test]
    fn shift_byte_never_lands_inside_a_multibyte_char() {
        let src = "aé\n".as_bytes(); // a=1 byte, é=2 bytes at 1..3
        assert_eq!(
            shift_byte(src, 1, 0, 1),
            3,
            "skips past é rather than into it"
        );
        assert!(std::str::from_utf8(&src[shift_byte(src, 1, 0, 1)..]).is_ok());
    }

    #[test]
    fn builtin_folds_rust_is_some() {
        assert!(
            builtin_folds("rust").is_some(),
            "bundled rust folds.scm must exist"
        );
    }

    #[test]
    fn builtin_folds_unknown_is_none() {
        assert!(builtin_folds("nope_unknown_lang_xyz").is_none());
    }

    /// Every bundled fold query must be REACHABLE from a file extension.
    ///
    /// `builtin_folds` is keyed by the name `GrammarRegistry` resolved the
    /// buffer's extension to — not by the file name of the `.scm`. When two
    /// manifest entries claim the same extension the registry keeps the
    /// alphabetically first, so a query keyed on the other name is dead: `.cs`
    /// resolves to `c-sharp`, and `folds/c_sharp.scm` was keyed only on
    /// `c_sharp`, leaving every C# buffer with zero folds and nothing to
    /// notice it. This walks the real embedded manifest, so a future manifest
    /// entry that steals an extension reds the build instead.
    ///
    /// Needs no grammar or network — the registry is a parse of `bonsai.toml`.
    #[test]
    fn every_bundled_fold_query_is_reachable_by_extension() {
        use crate::runtime::GrammarRegistry;

        let registry = GrammarRegistry::embedded().expect("embedded registry");
        let mut dead = Vec::new();
        for (name, spec) in registry.manifest().iter() {
            if builtin_folds(name).is_none() {
                continue;
            }
            // At least one of this language's own extensions must resolve to a
            // name that still has a fold query.
            let reachable = spec.extensions.iter().any(|ext| {
                registry
                    .name_for_ext(ext)
                    .is_some_and(|resolved| builtin_folds(resolved).is_some())
            });
            if !reachable {
                let stolen: Vec<_> = spec
                    .extensions
                    .iter()
                    .map(|e| (e.clone(), registry.name_for_ext(e).unwrap_or("<none>")))
                    .collect();
                dead.push(format!("{name}: {stolen:?}"));
            }
        }
        assert!(
            dead.is_empty(),
            "bundled fold queries unreachable from any extension \
             (ext -> the language the registry actually resolves it to): {dead:?}"
        );
    }

    /// Verify that the bundled rust folds.scm compiles against the tree-sitter
    /// Rust language grammar and that a simple multi-line `fn` yields at least
    /// one fold range. Requires the tree-sitter-rust grammar to be available
    /// at runtime (network + compiler on first run). Gated `#[ignore]` like
    /// every other grammar-loading test in the workspace — the plain
    /// `--workspace` CI lane excludes it; the dedicated "grammar tests" CI job
    /// (Linux+macOS, where a C compiler + network are available) runs it via
    /// `cargo nextest --run-ignored all`.
    #[test]
    #[ignore = "network + compiler: clones tree-sitter-rust then builds it"]
    fn rust_fold_query_extracts_fn_range() {
        use crate::runtime::{GrammarLoader, GrammarRegistry};
        use std::sync::Arc;

        // This test mirrors the end-to-end grammar load pattern.
        let registry = GrammarRegistry::embedded().expect("embedded registry");
        let loader = GrammarLoader::user_default(registry.meta()).expect("user loader");
        let spec = registry.by_name("rust").expect("rust in manifest");
        let grammar = Arc::new(
            crate::runtime::Grammar::load("rust", spec, &loader, registry.meta())
                .expect("load rust grammar"),
        );

        // A multi-line Rust function — should yield a fold over rows 0..=3.
        let source = b"fn hello() {\n    let x = 1;\n    x\n}\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(grammar.language())
            .expect("set language");
        let tree = parser.parse(source, None).expect("parse");

        let ranges = extract_fold_ranges(&tree, &grammar, source);
        // The `fn hello()` block spans rows 0..=3 (inclusive).
        // There may also be a (block) child match — we check that at least one
        // range covers (0, 3).
        assert!(
            ranges.iter().any(|&(s, e)| s == 0 && e == 3),
            "expected fold (0, 3) for a 4-line fn, got: {ranges:?}"
        );
    }

    /// `injection_regions` must report the fenced block's content, and must
    /// apply `(#offset! @injection.content …)` when the pattern carries one.
    ///
    /// Nothing in the currently installed query set uses `#offset!` on
    /// `@injection.content` (javascript's is on a different capture), so
    /// without this test the offset branch would never run — and the day a
    /// grammar repo adds one (nvim-treesitter's YAML query offsets the `|` of
    /// a `run: |` block scalar out of the injected text) it would be feeding
    /// the child parser a leading delimiter with nothing to notice it.
    #[test]
    #[ignore = "network + compiler: clones tree-sitter-markdown then builds it"]
    fn injection_regions_apply_the_offset_directive() {
        use crate::runtime::{GrammarLoader, GrammarRegistry};

        let registry = GrammarRegistry::embedded().expect("embedded registry");
        let loader = GrammarLoader::user_default(registry.meta()).expect("user loader");
        let spec = registry.by_name("markdown").expect("markdown in manifest");
        let grammar = crate::runtime::Grammar::load("markdown", spec, &loader, registry.meta())
            .expect("load markdown grammar");

        // Byte 8 is the `f` of `fn` — right after "```rust\n".
        let source = b"```rust\nfn f() {}\n```\n";
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(grammar.language()).unwrap();
        let tree = parser.parse(source, None).unwrap();

        let base = r#"(fenced_code_block (code_fence_content) @injection.content
                        (#set! injection.language "rust"))"#;
        let offset = r#"(fenced_code_block (code_fence_content) @injection.content
                        (#set! injection.language "rust")
                        (#offset! @injection.content 0 1 0 0))"#;
        let plain = Query::new(grammar.language(), base).expect("compile plain query");
        let regions = injection_regions(&tree, &plain, source);
        assert_eq!(regions.len(), 1, "one fenced block, got {regions:?}");
        assert_eq!(regions[0].0, "rust");
        assert_eq!(regions[0].1.start, 8, "content starts after the info line");

        let with_offset = Query::new(grammar.language(), offset).expect("compile offset query");
        let shifted = injection_regions(&tree, &with_offset, source);
        assert_eq!(shifted.len(), 1);
        assert_eq!(
            shifted[0].1.start,
            regions[0].1.start + 1,
            "a `0 1 0 0` offset must move the content start one column right"
        );
        assert_eq!(
            shifted[0].1.end, regions[0].1.end,
            "a zero end-delta must leave the end alone"
        );
    }

    /// Idempotency: calling extract_fold_ranges twice on the same tree must
    /// produce the same result (no global mutation, no state leakage).
    #[test]
    #[ignore = "network + compiler: clones tree-sitter-rust then builds it"]
    fn rust_fold_extraction_is_idempotent() {
        use crate::runtime::{GrammarLoader, GrammarRegistry};
        use std::sync::Arc;

        let registry = GrammarRegistry::embedded().expect("embedded registry");
        let loader = GrammarLoader::user_default(registry.meta()).expect("user loader");
        let spec = registry.by_name("rust").expect("rust in manifest");
        let grammar = Arc::new(
            crate::runtime::Grammar::load("rust", spec, &loader, registry.meta())
                .expect("load rust grammar"),
        );

        let source = b"fn a() {\n    1\n}\nfn b() {\n    2\n}\n";
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(grammar.language()).unwrap();
        let tree = parser.parse(source, None).unwrap();

        let r1 = extract_fold_ranges(&tree, &grammar, source);
        let r2 = extract_fold_ranges(&tree, &grammar, source);
        assert_eq!(r1, r2, "fold extraction must be idempotent");
    }

    /// Every bundled `folds.scm` MUST compile against its real grammar.
    ///
    /// `tree_sitter::Query::new` is all-or-nothing: a single invalid node type
    /// fails the ENTIRE query, silently disabling folds for that language (this
    /// is the `type_alias` bug that shipped once). `get_or_compile` swallows the
    /// error and returns `None`, so the only way to catch a bad query is to
    /// compile it against the real grammar — which needs network + a C compiler.
    /// Runs in the dedicated "grammar tests" CI job via `--run-ignored all`.
    #[test]
    #[ignore = "network + compiler: clones each grammar then builds it"]
    fn all_bundled_fold_queries_compile() {
        use crate::runtime::{GrammarLoader, GrammarRegistry};

        let langs = [
            "rust",
            "python",
            "javascript",
            "typescript",
            "go",
            "c",
            "cpp",
            "json",
            "bash",
            "lua",
            "java",
            "html",
            "css",
            "scss",
            "yaml",
            "toml",
            "ruby",
            "php",
            "c_sharp",
            // The name `.cs` actually resolves to — see
            // `every_bundled_fold_query_is_reachable_by_extension`.
            "c-sharp",
            "markdown",
        ];

        let registry = GrammarRegistry::embedded().expect("embedded registry");
        let loader = GrammarLoader::user_default(registry.meta()).expect("user loader");

        for lang in langs {
            let src =
                builtin_folds(lang).unwrap_or_else(|| panic!("builtin_folds missing for `{lang}`"));
            let spec = registry
                .by_name(lang)
                .unwrap_or_else(|| panic!("`{lang}` not in manifest"));
            // Grammar load clones + compiles over the network; CI runners flake
            // on the git fetch (shared-cache races, transient "os error 2"). That
            // is an infra failure, NOT a query bug — skip the grammar rather than
            // red the build. The query-correctness assertion below still runs for
            // every grammar that DID load, which is the point: catch an invalid
            // node type (the `type_alias` class of bug).
            let grammar = match crate::runtime::Grammar::load(lang, spec, &loader, registry.meta())
            {
                Ok(g) => g,
                Err(e) => {
                    eprintln!("skip `{lang}` — grammar load failed (infra, not query): {e}");
                    continue;
                }
            };
            // The real assertion: the query compiles. A bad node type errors here.
            let q = tree_sitter::Query::new(grammar.language(), src);
            assert!(
                q.is_ok(),
                "folds/{lang}.scm failed to compile: {:?}",
                q.err()
            );
            // And it actually defines a @fold capture.
            let q = q.unwrap();
            assert!(
                q.capture_names().contains(&"fold"),
                "folds/{lang}.scm has no @fold capture"
            );
        }
    }

    // ── Marker folds (grammar-free — run in the normal lane) ─────────────────

    fn markers(src: &str) -> Vec<(usize, usize)> {
        let rope = ropey::Rope::from_str(src);
        extract_marker_fold_ranges_rope(&rope, DEFAULT_FOLD_MARKER_OPEN, DEFAULT_FOLD_MARKER_CLOSE)
    }

    #[test]
    fn marker_simple_pair() {
        // rows: 0 open, 1 body, 2 close
        let got = markers("// region {{{\nbody\n// }}}\n");
        assert_eq!(got, vec![(0, 2)]);
    }

    #[test]
    fn marker_nested_pairs() {
        // 0 {{{, 1 {{{, 2 }}}, 3 }}}  → inner (1,2), outer (0,3)
        let got = markers("a {{{\nb {{{\nc }}}\nd }}}\n");
        assert_eq!(got, vec![(0, 3), (1, 2)]);
    }

    #[test]
    fn marker_unmatched_close_ignored() {
        let got = markers("foo\n}}}\nbar\n");
        assert!(got.is_empty(), "stray close must be ignored, got {got:?}");
    }

    #[test]
    fn marker_unclosed_open_dropped() {
        let got = markers("{{{\nbody\nmore\n");
        assert!(got.is_empty(), "unclosed open must be dropped, got {got:?}");
    }

    #[test]
    fn marker_single_line_skipped() {
        // open and close on the same row → no multi-line fold
        let got = markers("inline {{{ }}} end\n");
        assert!(
            got.is_empty(),
            "single-line marker must be skipped, got {got:?}"
        );
    }

    #[test]
    fn marker_level_digit_ignored() {
        // vim's `{{{1` / `}}}1` — digit accepted and ignored
        let got = markers("a {{{1\nb\nc }}}1\n");
        assert_eq!(got, vec![(0, 2)]);
    }

    #[test]
    fn marker_close_then_open_same_line() {
        // 0 {{{, 1 "}}} {{{" closes (0,1) then opens, 2 }}} closes (1,2)
        let got = markers("a {{{\nb }}} {{{\nc }}}\n");
        assert_eq!(got, vec![(0, 1), (1, 2)]);
    }

    #[test]
    fn marker_works_without_comment_syntax() {
        // Grammar-independent: bare markers, no comment leader.
        let got = markers("{{{\nx\ny\n}}}\n");
        assert_eq!(got, vec![(0, 3)]);
    }

    #[test]
    fn marker_empty_delims_returns_empty() {
        let rope = ropey::Rope::from_str("{{{\n}}}\n");
        assert!(extract_marker_fold_ranges_rope(&rope, "", "}}}").is_empty());
        assert!(extract_marker_fold_ranges_rope(&rope, "{{{", "").is_empty());
    }

    // ── #region / #endregion + multi-pair ────────────────────────────────────

    fn multi(src: &str, pairs: &[(&str, &str)]) -> Vec<(usize, usize)> {
        extract_marker_fold_ranges_rope_multi(&ropey::Rope::from_str(src), pairs)
    }

    #[test]
    fn region_markers_fold() {
        let got = multi(
            "// #region Foo\nbody\nmore\n// #endregion\n",
            &[(DEFAULT_REGION_MARKER_OPEN, DEFAULT_REGION_MARKER_CLOSE)],
        );
        assert_eq!(got, vec![(0, 3)]);
    }

    #[test]
    fn region_endregion_does_not_false_match_region_open() {
        // `#endregion` must NOT be parsed as a `#region` open — else the stack
        // would never balance. A lone `#endregion` is a stray close → ignored.
        let got = multi(
            "#endregion\nbody\n",
            &[(DEFAULT_REGION_MARKER_OPEN, DEFAULT_REGION_MARKER_CLOSE)],
        );
        assert!(
            got.is_empty(),
            "stray #endregion must be ignored, got {got:?}"
        );
    }

    #[test]
    fn multi_pair_recognizes_both_curly_and_region() {
        // Curly block at rows 0..=2, region block at rows 3..=5.
        let src = "a {{{\nb\nc }}}\n// #region\nx\n// #endregion\n";
        let got = multi(
            src,
            &[
                (DEFAULT_FOLD_MARKER_OPEN, DEFAULT_FOLD_MARKER_CLOSE),
                (DEFAULT_REGION_MARKER_OPEN, DEFAULT_REGION_MARKER_CLOSE),
            ],
        );
        assert_eq!(got, vec![(0, 2), (3, 5)]);
    }

    #[test]
    fn multi_pair_same_start_keeps_largest_span() {
        // Both pairs open on row 0; the larger span (region close on row 4) wins
        // over the curly close on row 2 at the shared start_row.
        let src = "{{{ #region\nx\n}}}\ny\n#endregion\n";
        let got = multi(
            src,
            &[
                (DEFAULT_FOLD_MARKER_OPEN, DEFAULT_FOLD_MARKER_CLOSE),
                (DEFAULT_REGION_MARKER_OPEN, DEFAULT_REGION_MARKER_CLOSE),
            ],
        );
        assert_eq!(
            got,
            vec![(0, 4)],
            "largest span must win at shared start_row"
        );
    }
}
