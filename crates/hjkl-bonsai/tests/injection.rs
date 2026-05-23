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
    // `highlight_range_with_injections` assumes the parent parse tree exists.
    highlighter.parse_initial(source);
    let resolver = |name: &str| -> Option<Arc<Grammar>> {
        if name == "rust" {
            Some(rust_grammar.clone())
        } else {
            None
        }
    };

    // First call: parses all 3 child blocks (the parent tree was already
    // seeded above so we don't count it here).
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

/// `highlight_with_injections` on a grammar whose `injections.scm` only uses
/// the `(#set! injection.language ...)` directive form (no
/// `@injection.language` captures) should — with a null resolver — produce
/// the same span set as `highlight()`. The directive form IS detected as of
/// the regression below, but a null resolver still drops the injection.
#[test]
#[ignore = "network + compiler: fetches rust grammar"]
fn no_injection_grammar_behaves_like_highlight() {
    let tmp = tempfile::tempdir().unwrap();
    let (registry, meta) = registry_and_meta();
    let loader = make_loader(&tmp);

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

/// Regression: tree-sitter-markdown's injections.scm wires
/// `((inline) @injection.content (#set! injection.language "markdown_inline"))`,
/// `((html_block) @injection.content (#set! injection.language "html"))`,
/// `((minus_metadata) @injection.content (#set! injection.language "yaml"))`,
/// and `((plus_metadata) @injection.content (#set! injection.language "toml"))`.
/// Before the `(#set! injection.language ...)` directive support landed, the
/// highlighter only consumed the capture form, so paragraph-inline markdown
/// got no `markdown_inline` injection — and italic / strong / inline code /
/// inline links rendered without highlighting.
///
/// This test asserts the resolver is called with `"markdown_inline"` when a
/// paragraph is present in the markdown source.
#[test]
#[ignore = "network + compiler: fetches markdown grammar"]
fn markdown_inline_injection_directive_form_fires_resolver() {
    use std::cell::RefCell;

    let tmp = tempfile::tempdir().unwrap();
    let (registry, meta) = registry_and_meta();
    let loader = make_loader(&tmp);

    let markdown_grammar = load_grammar("markdown", &loader, &registry, &meta);

    // Paragraph text triggers an (inline) node → markdown_inline injection
    // via the directive form. Use prose that does not look like a fence so
    // the (fenced_code_block …) pattern (capture-form @injection.language)
    // is NOT what we're observing.
    let source: &[u8] = b"# Title\n\nSome *italic* prose with `code`.\n";

    let asked: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let mut highlighter = Highlighter::new(markdown_grammar).unwrap();
    let _ = highlighter.highlight_with_injections(source, |name: &str| {
        asked.borrow_mut().push(name.to_string());
        None
    });

    let names = asked.borrow();
    assert!(
        names.iter().any(|n| n == "markdown_inline"),
        "resolver must be asked for `markdown_inline` (via #set! directive) on \
         a paragraph; got: {names:?}"
    );
}

/// Companion regression for the scoped variant: the
/// `(#set! injection.language ...)` directive form must work in
/// `highlight_range_with_injections` as well as the full-buffer walker.
#[test]
#[ignore = "network + compiler: fetches markdown grammar"]
fn markdown_inline_injection_directive_form_fires_in_range_variant() {
    use std::cell::RefCell;

    let tmp = tempfile::tempdir().unwrap();
    let (registry, meta) = registry_and_meta();
    let loader = make_loader(&tmp);

    let markdown_grammar = load_grammar("markdown", &loader, &registry, &meta);

    let source: &[u8] = b"# Title\n\nSome *italic* prose with `code`.\n";

    let asked: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let mut highlighter = Highlighter::new(markdown_grammar).unwrap();
    // The scoped variant assumes the parse tree exists — callers always
    // invoke `parse_initial` (or `highlight()`) first; we mirror that here.
    highlighter.parse_initial(source);
    let _ = highlighter.highlight_range_with_injections(source, 0..source.len(), |name: &str| {
        asked.borrow_mut().push(name.to_string());
        None
    });

    let names = asked.borrow();
    assert!(
        names.iter().any(|n| n == "markdown_inline"),
        "scoped variant must also fire the directive-form resolver; got: {names:?}"
    );
}

/// HTML's injections.scm uses ONLY directive-form `(#set! injection.language
/// "css")` / `(... "javascript")` with no `@injection.language` capture at
/// all. Regression for a bug where the highlighter early-returned when the
/// query had no language capture, skipping the directive-form fallback and
/// leaving CSS/JS inside `<style>`/`<script>` unhighlighted.
#[test]
#[ignore = "network + compiler: fetches html grammar"]
fn html_directive_only_injection_query_fires_resolver() {
    use std::cell::RefCell;

    let tmp = tempfile::tempdir().unwrap();
    let (registry, meta) = registry_and_meta();
    let loader = make_loader(&tmp);

    let html_grammar = load_grammar("html", &loader, &registry, &meta);
    assert!(
        html_grammar.injections_scm().is_some(),
        "html grammar must ship an injections.scm",
    );
    // Verify the query is genuinely directive-only — if upstream html ever
    // adds an @injection.language capture this assertion will turn this
    // test into a less-useful overlap with the markdown directive test;
    // failing here is the signal to update.
    let inj = html_grammar.injections_scm().unwrap();
    assert!(
        !inj.contains("@injection.language"),
        "test assumes html injections.scm has no @injection.language capture; \
         got:\n{inj}"
    );

    let source: &[u8] = b"<html><head><style>body { color: red; }</style></head></html>";

    let asked: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let mut highlighter = Highlighter::new(html_grammar).unwrap();
    let _ = highlighter.highlight_with_injections(source, |name: &str| {
        asked.borrow_mut().push(name.to_string());
        None
    });

    let names = asked.borrow();
    assert!(
        names.iter().any(|n| n == "css"),
        "directive-only injection query must still fire the resolver; got: {names:?}"
    );
}

/// Same as above but for the scoped variant — must produce identical
/// behaviour so a viewport-restricted render doesn't silently lose
/// directive-form injections.
#[test]
#[ignore = "network + compiler: fetches html grammar"]
fn html_directive_only_injection_query_fires_in_range_variant() {
    use std::cell::RefCell;

    let tmp = tempfile::tempdir().unwrap();
    let (registry, meta) = registry_and_meta();
    let loader = make_loader(&tmp);

    let html_grammar = load_grammar("html", &loader, &registry, &meta);
    let source: &[u8] = b"<html><head><style>body { color: red; }</style></head></html>";

    let asked: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let mut highlighter = Highlighter::new(html_grammar).unwrap();
    highlighter.parse_initial(source);
    let _ = highlighter.highlight_range_with_injections(source, 0..source.len(), |name: &str| {
        asked.borrow_mut().push(name.to_string());
        None
    });

    let names = asked.borrow();
    assert!(
        names.iter().any(|n| n == "css"),
        "scoped variant must also fire directive-form resolver; got: {names:?}"
    );
}
