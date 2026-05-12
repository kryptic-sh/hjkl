//! Integration tests for tree-sitter language injection support.
//!
//! These tests fetch grammars from the network and require a C compiler, so
//! they are marked `#[ignore]`. Run with:
//!
//! ```sh
//! cargo test -p hjkl-bonsai --test injection -- --ignored
//! ```

use std::sync::Arc;

use hjkl_bonsai::Highlighter;
use hjkl_bonsai::runtime::Grammar;
use hjkl_bonsai::runtime::{
    GrammarCompiler, GrammarLoader, GrammarRegistry, ManifestMeta, QuerySourceCache, SourceCache,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn registry_and_meta() -> (GrammarRegistry, ManifestMeta) {
    let registry = GrammarRegistry::embedded().expect("embedded manifest must parse");
    let meta = registry.meta().clone();
    (registry, meta)
}

fn make_loader(tmp: &tempfile::TempDir) -> GrammarLoader {
    let sources = SourceCache::new(tmp.path().join("grammar-cache"));
    let query_sources = QuerySourceCache::new(tmp.path().join("query-cache"));
    let user_dir = tmp.path().join("user");
    GrammarLoader::new(
        vec![],
        user_dir,
        sources,
        query_sources,
        GrammarCompiler::new(),
    )
}

fn load_grammar(
    name: &str,
    loader: &GrammarLoader,
    registry: &GrammarRegistry,
    meta: &ManifestMeta,
) -> Arc<Grammar> {
    let spec = registry
        .by_name(name)
        .unwrap_or_else(|| panic!("{name} not in embedded manifest"));
    Arc::new(Grammar::load(name, spec, loader, meta).unwrap_or_else(|e| {
        panic!("failed to load {name} grammar: {e:#}");
    }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Markdown buffer with a fenced Rust code block. With injection support the
/// inner `fn main() {}` should produce at least one span with a capture that
/// starts with `keyword` or `function`, falling inside the Rust code's byte
/// range.
#[test]
#[ignore = "network + compiler: fetches markdown + rust grammars"]
fn markdown_rust_injection_produces_rust_spans() {
    let tmp = tempfile::tempdir().unwrap();
    let (registry, meta) = registry_and_meta();
    let loader = make_loader(&tmp);

    let markdown_grammar = load_grammar("markdown", &loader, &registry, &meta);
    let rust_grammar = load_grammar("rust", &loader, &registry, &meta);

    // Confirm the markdown grammar loaded an injections query.
    assert!(
        markdown_grammar.injections_scm().is_some(),
        "markdown grammar should ship an injections.scm"
    );

    let source: &[u8] = b"# Title\n\n```rust\nfn main() {}\n```\n";

    // Byte range of "fn main() {}" inside the source.
    let rust_start = source
        .windows(2)
        .position(|w| w == b"fn")
        .expect("'fn' not found in test source");
    let rust_end = rust_start + b"fn main() {}".len();

    let mut highlighter = Highlighter::new(markdown_grammar).unwrap();
    let spans = highlighter.highlight_with_injections(source, |name| {
        if name == "rust" {
            Some(rust_grammar.clone())
        } else {
            None
        }
    });

    // At least one child span must fall inside the Rust code range and carry a
    // Rust-specific capture (keyword.* or function.*).
    let has_rust_span = spans.iter().any(|s| {
        s.byte_range.start >= rust_start
            && s.byte_range.end <= rust_end + 1 // +1: fence newline tolerance
            && (s.capture.starts_with("keyword") || s.capture.starts_with("function"))
    });

    assert!(
        has_rust_span,
        "expected at least one keyword/function span inside the Rust code range \
         ({rust_start}..{rust_end}); got:\n{spans:#?}"
    );
}

/// Negative case: resolver returns `None` for every language → output should
/// only contain markdown captures (no Rust-specific captures like `keyword.*`
/// inside the fenced block).
#[test]
#[ignore = "network + compiler: fetches markdown grammar"]
fn markdown_null_resolver_yields_only_markdown_spans() {
    let tmp = tempfile::tempdir().unwrap();
    let (registry, meta) = registry_and_meta();
    let loader = make_loader(&tmp);

    let markdown_grammar = load_grammar("markdown", &loader, &registry, &meta);

    let source: &[u8] = b"# Title\n\n```rust\nfn main() {}\n```\n";

    let rust_start = source
        .windows(2)
        .position(|w| w == b"fn")
        .expect("'fn' not found in test source");
    let rust_end = rust_start + b"fn main() {}".len();

    let mut highlighter = Highlighter::new(markdown_grammar).unwrap();
    let spans = highlighter.highlight_with_injections(source, |_name| {
        None // never resolve
    });

    // No Rust keyword/function captures should appear inside the rust range.
    let has_rust_specific = spans.iter().any(|s| {
        s.byte_range.start >= rust_start
            && s.byte_range.end <= rust_end + 1
            && (s.capture.starts_with("keyword") || s.capture.starts_with("function"))
    });

    assert!(
        !has_rust_specific,
        "with null resolver, expected no rust-specific spans inside the fenced range; got:\n{spans:#?}"
    );
}

/// Child-highlighter cache test: a markdown buffer with 3 fenced Rust blocks
/// called 10 times should only trigger ≤ 3 child `parse_initial` calls (one
/// per unique block), not 30 (one per block per frame).
///
/// The parent markdown parse counts are excluded by resetting the counter
/// *after* the first highlight call, which seeds the parent tree; subsequent
/// calls should not re-parse children whose content is unchanged.
#[test]
#[ignore = "network + compiler: fetches markdown + rust grammars"]
fn child_cache_avoids_repeated_parses() {
    let tmp = tempfile::tempdir().unwrap();
    let (registry, meta) = registry_and_meta();
    let loader = make_loader(&tmp);

    let markdown_grammar = load_grammar("markdown", &loader, &registry, &meta);
    let rust_grammar = load_grammar("rust", &loader, &registry, &meta);

    // Three distinct fenced Rust blocks so we can distinguish per-block
    // caching from trivial no-op behaviour.
    let source: &[u8] = b"# Doc\n\n\
        ```rust\nfn one() {}\n```\n\n\
        Some prose.\n\n\
        ```rust\nfn two() { let x = 1; }\n```\n\n\
        More prose.\n\n\
        ```rust\nfn three() -> u32 { 42 }\n```\n";

    let mut highlighter = Highlighter::new(markdown_grammar).unwrap();
    let resolver = |name: &str| -> Option<Arc<Grammar>> {
        if name == "rust" {
            Some(rust_grammar.clone())
        } else {
            None
        }
    };

    // First call: seeds the parent tree + parses all 3 child blocks.
    hjkl_bonsai::parse_counter::reset();
    highlighter.highlight_range_with_injections(source, 0..source.len(), resolver);
    let after_first = hjkl_bonsai::parse_counter::get();
    // Sanity: first call must have parsed the 3 child blocks (the parent
    // parse_initial runs before this, which we reset, so we only count from here).
    assert!(
        after_first >= 3,
        "expected ≥ 3 parses on first call (one per block); got {after_first}"
    );

    // Calls 2–10: content unchanged → zero additional child parses.
    hjkl_bonsai::parse_counter::reset();
    for _ in 0..9 {
        highlighter.highlight_range_with_injections(source, 0..source.len(), resolver);
    }
    let cached_parses = hjkl_bonsai::parse_counter::get();
    assert!(
        cached_parses <= 3,
        "expected ≤ 3 child parses across 9 repeat calls (cache should hit); got {cached_parses}"
    );
}

/// `highlight_with_injections` on a grammar that has no `injections.scm`
/// (e.g. the rust grammar itself) should behave identically to `highlight`.
#[test]
#[ignore = "network + compiler: fetches rust grammar"]
fn no_injection_grammar_behaves_like_highlight() {
    let tmp = tempfile::tempdir().unwrap();
    let (registry, meta) = registry_and_meta();
    let loader = make_loader(&tmp);

    // Rust's own injections.scm (from its grammar source) uses
    // (#set! injection.language ...) without @injection.language captures,
    // so our v1 implementation skips those matches. The output should
    // therefore be identical to a plain highlight() call.
    let rust_grammar = load_grammar("rust", &loader, &registry, &meta);

    let source = b"fn main() { let x = 42; }";

    let mut h1 = Highlighter::new(rust_grammar.clone()).unwrap();
    let plain = h1.highlight(source);

    let mut h2 = Highlighter::new(rust_grammar).unwrap();
    let with_inj = h2.highlight_with_injections(source, |_| None);

    assert_eq!(
        plain, with_inj,
        "highlight_with_injections with null resolver must equal highlight"
    );
}
