//! Test: pure scroll (no edits) does not reparse.
//!
//! After the initial parse, scrolling between viewport positions must not
//! call `parse_initial` again. `highlight_range` just walks the retained tree.
//!
//! Requires a compiled grammar. Run with:
//! ```sh
//! cargo test -p hjkl-bonsai --test scroll_no_reparse -- --ignored
//! ```

use std::sync::Arc;

use hjkl_bonsai::Highlighter;
use hjkl_bonsai::parse_counter;
use hjkl_bonsai::runtime::{
    GrammarCompiler, GrammarLoader, GrammarRegistry, ManifestMeta, QuerySourceCache, SourceCache,
};

fn make_loader(tmp: &tempfile::TempDir) -> (GrammarLoader, GrammarRegistry, ManifestMeta) {
    let registry = GrammarRegistry::embedded().expect("embedded manifest must parse");
    let meta = registry.meta().clone();
    let sources = SourceCache::new(tmp.path().join("grammar-cache"));
    let query_sources = QuerySourceCache::new(tmp.path().join("query-cache"));
    let user_dir = tmp.path().join("user");
    let loader = GrammarLoader::new(
        vec![],
        user_dir,
        sources,
        query_sources,
        GrammarCompiler::new(),
    );
    (loader, registry, meta)
}

fn row_starts(source: &[u8]) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (i, &b) in source.iter().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

/// Build a 30-row Rust source so we can scroll between two distinct viewports.
fn make_source() -> Vec<u8> {
    let mut s = String::new();
    for i in 0..30 {
        s.push_str(&format!("fn f{i}() {{ let x = {i}; }}\n"));
    }
    s.into_bytes()
}

/// Viewport 1 = rows 0..10, Viewport 2 = rows 20..30. Scroll vp1→vp2→vp1.
/// Parse counter must not increment after the initial `parse_initial`.
#[test]
#[ignore = "network + compiler: needs tree-sitter-rust grammar"]
fn pure_scroll_does_not_reparse() {
    let tmp = tempfile::tempdir().unwrap();
    let (loader, registry, meta) = make_loader(&tmp);

    let spec = registry.by_name("rust").expect("rust in manifest");
    let grammar = Arc::new(
        hjkl_bonsai::runtime::Grammar::load("rust", spec, &loader, &meta)
            .expect("load rust grammar"),
    );

    let source = make_source();
    let starts = row_starts(&source);
    let row_count = starts.len();

    let mut h = Highlighter::new(grammar).expect("create highlighter");

    parse_counter::reset();

    // Initial parse — counter increments once.
    h.parse_initial(&source);
    let after_initial = parse_counter::get();
    assert_eq!(
        after_initial, 1,
        "parse_initial must increment counter once"
    );

    // Viewport 1: rows 0..10.
    let vp1_start = starts[0];
    let vp1_end = starts.get(10).copied().unwrap_or(source.len());
    let _spans_vp1a = h.highlight_range(&source, vp1_start..vp1_end);

    // Scroll to viewport 2: rows 20..30.
    let vp2_start = starts[20.min(row_count - 1)];
    let vp2_end = starts.get(30).copied().unwrap_or(source.len());
    let _spans_vp2 = h.highlight_range(&source, vp2_start..vp2_end);

    // Scroll back to viewport 1.
    let _spans_vp1b = h.highlight_range(&source, vp1_start..vp1_end);

    let final_count = parse_counter::get();
    assert_eq!(
        final_count, 1,
        "highlight_range must not trigger parse_initial; counter went from 1 to {final_count}"
    );
}
