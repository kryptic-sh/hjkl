//! Test: opening a file and walking the first viewport produces highlight spans.
//!
//! Requires a compiled grammar. Run with:
//! ```sh
//! cargo test -p hjkl-bonsai --test open_full_window_parsed -- --ignored
//! ```

use std::sync::Arc;

use hjkl_bonsai::Highlighter;
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

/// Open a Rust file, walk the first viewport, assert spans cover keywords.
/// The parent-spans cache is gone — `highlight_range` walks the tree directly.
#[test]
#[ignore = "network + compiler: needs tree-sitter-rust grammar"]
fn open_file_first_walk_produces_spans_for_visible_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let (loader, registry, meta) = make_loader(&tmp);

    let spec = registry.by_name("rust").expect("rust in manifest");
    let grammar = Arc::new(
        hjkl_bonsai::runtime::Grammar::load("rust", spec, &loader, &meta)
            .expect("load rust grammar"),
    );

    let source = b"fn main() {\n    let x = 1;\n    println!(\"{x}\");\n}\n";
    let mut h = Highlighter::new(grammar).expect("create highlighter");

    // Simulate file open: parse initial.
    h.parse_initial(source);

    // Walk first viewport (all rows visible).
    let spans = h.highlight_range(source, 0..source.len());

    assert!(
        !spans.is_empty(),
        "expected highlight spans for a Rust snippet; got none"
    );
    assert!(
        spans.iter().any(|s| s.capture().starts_with("keyword")),
        "expected at least one keyword span; captures: {:?}",
        spans.iter().map(|s| s.capture()).collect::<Vec<_>>()
    );
    assert!(
        spans.iter().any(|s| s.capture().starts_with("function")),
        "expected at least one function span; captures: {:?}",
        spans.iter().map(|s| s.capture()).collect::<Vec<_>>()
    );
}
