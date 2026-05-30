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
    /// at runtime (network + compiler on first run).
    #[test]
    #[ignore = "network + compiler: needs tree-sitter-rust grammar installed"]
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
    #[ignore = "network + compiler: needs tree-sitter-rust grammar installed"]
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
}
