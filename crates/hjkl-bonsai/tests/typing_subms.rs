//! Test: small edit parse + highlight fits under a reasonable time budget.
//!
//! Requires a compiled grammar. Run with:
//! ```sh
//! cargo test -p hjkl-bonsai --test typing_subms -- --ignored
//! ```

use std::sync::Arc;
use std::time::Instant;

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

/// Open a ~5KB Rust file, apply a tiny one-char insert, parse incrementally,
/// and walk the viewport.  The total must stay under a generous 100 ms budget
/// on a developer laptop (CI is slower; adjust or `#[ignore]` if it flakes).
///
/// The test is marked `#[ignore]` because it needs the Rust grammar from the
/// network. It is not a strict deadline test — it is a smoke-check that the
/// tree-walk path isn't O(file-size) instead of O(viewport-size).
#[test]
#[ignore = "network + compiler: needs tree-sitter-rust grammar — timing may vary on CI"]
fn small_edit_parse_and_walk_under_some_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let (loader, registry, meta) = make_loader(&tmp);

    let spec = registry.by_name("rust").expect("rust in manifest");
    let grammar = Arc::new(
        hjkl_bonsai::runtime::Grammar::load("rust", spec, &loader, &meta)
            .expect("load rust grammar"),
    );

    // Build a ~5KB Rust source.
    let mut src = String::new();
    for i in 0..100 {
        src.push_str(&format!("fn function_{i}(x: u32) -> u32 {{ x + {i} }}\n"));
    }
    let pre: Vec<u8> = src.as_bytes().to_vec();

    let mut h = Highlighter::new(grammar).expect("create highlighter");
    h.parse_initial(&pre);

    // Insert one character at byte 0 ("X" before "fn").
    let post: Vec<u8> = {
        let mut v = vec![b'X'];
        v.extend_from_slice(&pre);
        v
    };
    let edit = tree_sitter::InputEdit {
        start_byte: 0,
        old_end_byte: 0,
        new_end_byte: 1,
        start_position: tree_sitter::Point { row: 0, column: 0 },
        old_end_position: tree_sitter::Point { row: 0, column: 0 },
        new_end_position: tree_sitter::Point { row: 0, column: 1 },
    };

    let t = Instant::now();
    h.edit(&edit);
    assert!(h.parse_incremental(&post), "incremental parse must succeed");
    // Walk first viewport (~10 rows = ~300 bytes).
    let viewport_end = post
        .iter()
        .enumerate()
        .filter(|&(_, b)| *b == b'\n')
        .nth(9)
        .map(|(i, _)| i + 1)
        .unwrap_or(post.len());
    let _spans = h.highlight_range(&post, 0..viewport_end);
    let elapsed = t.elapsed();

    // 100 ms is generous for a ~300-byte viewport walk on any modern machine.
    // If this flakes on slow CI, add `#[ignore]` and document why.
    assert!(
        elapsed.as_millis() < 100,
        "edit+parse+walk took {}ms; expected <100ms",
        elapsed.as_millis()
    );
}
