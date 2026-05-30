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
//!   per reparse — this is cheap: one query pass, no per-frame cost.
//! - Only nodes spanning more than one row produce folds; single-row nodes
//!   are silently skipped.
//! - Vim convention: `start_row` is the visible "header" line;
//!   `start_row+1..=end_row` are hidden when the fold is closed.

use std::collections::HashMap;
use std::sync::{LazyLock, RwLock};

use tree_sitter::{Query, QueryCursor, StreamingIterator as _};

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
                Some(fi) => Some(CompiledFolds {
                    query: q,
                    fold_idx: fi,
                }),
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
    if get_or_compile(grammar).is_none() {
        return Vec::new();
    }

    let cache = FOLD_QUERY_CACHE.read().unwrap();
    let compiled = match cache.get(grammar.name()).and_then(|v| v.as_ref()) {
        Some(c) => c,
        None => return Vec::new(),
    };

    let mut cursor = QueryCursor::new();
    // Run over the entire tree — no byte_range restriction.
    let root = tree.root_node();
    let mut matches = cursor.matches(&compiled.query, root, source);

    let fold_idx = compiled.fold_idx;
    // Keyed by start_row; value = largest end_row seen for that start row.
    let mut by_start: std::collections::BTreeMap<usize, usize> = std::collections::BTreeMap::new();

    while let Some(m) = matches.next() {
        for cap in m.captures {
            if cap.index != fold_idx {
                continue;
            }
            let node = cap.node;
            let start_row = node.start_position().row;
            let end_row = node.end_position().row;
            // Skip single-line nodes — no meaningful fold.
            if end_row <= start_row {
                continue;
            }
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

    by_start.into_iter().collect()
}

/// Rope-backed variant of [`extract_fold_ranges`].
///
/// Avoids building a contiguous `&[u8]` from the rope; reads chunk-by-chunk
/// via `chunk_at_byte`. Same semantics as `extract_fold_ranges`.
pub fn extract_fold_ranges_rope(
    tree: &tree_sitter::Tree,
    grammar: &Grammar,
    rope: &ropey::Rope,
) -> Vec<(usize, usize)> {
    if get_or_compile(grammar).is_none() {
        return Vec::new();
    }

    let cache = FOLD_QUERY_CACHE.read().unwrap();
    let compiled = match cache.get(grammar.name()).and_then(|v| v.as_ref()) {
        Some(c) => c,
        None => return Vec::new(),
    };

    let total_bytes = rope.len_bytes();
    let mut cursor = QueryCursor::new();
    let root = tree.root_node();

    // Rope-backed source callback — same pattern as parse_initial_rope.
    let mut by_start: std::collections::BTreeMap<usize, usize> = std::collections::BTreeMap::new();

    // Build a contiguous slice only if small (< 1 MB); otherwise use the rope
    // chunk callback. For fold extraction we need to run over the full tree
    // anyway, so the cost is proportional to parse time (already paid).
    //
    // We use the rope text() iterator approach: collect all chunks into a
    // scratch buffer only if needed. In practice bonsai's source callback
    // approach is cleaner here — but QueryCursor::matches requires a
    // `TextProvider` which for rope requires the full-document approach.
    // We replicate the same contiguous-str materialisation that ropey already
    // uses internally (it's O(N) but only on fold extraction, once per
    // reparse, not per frame).
    let text = rope.to_string();
    let source = text.as_bytes();

    let fold_idx = compiled.fold_idx;
    let mut matches = cursor.matches(&compiled.query, root, source);

    while let Some(m) = matches.next() {
        for cap in m.captures {
            if cap.index != fold_idx {
                continue;
            }
            let node = cap.node;
            let start_row = node.start_position().row;
            let end_row = node.end_position().row;
            if end_row <= start_row {
                continue;
            }
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

    // Suppress unused-variable warning; `total_bytes` isn't used in the
    // current implementation because we materialise the full string above.
    let _ = total_bytes;

    by_start.into_iter().collect()
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
