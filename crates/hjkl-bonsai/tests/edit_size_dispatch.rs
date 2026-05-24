//! Tests for edit-size–based dispatch behaviour.
//!
//! The refactor routes small edits (<1KB byte delta) through a synchronous
//! parse+walk path and large edits (≥1KB) through the async worker.
//! These tests validate the Highlighter layer's ability to handle both cases
//! correctly, independent of the app-level event_loop routing.
//!
//! Requires a compiled grammar. Run with:
//! ```sh
//! cargo test -p hjkl-bonsai --test edit_size_dispatch -- --ignored
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

/// Small edit (<1KB delta): `tree.edit` + `parse_incremental` + `highlight_range`
/// must complete without spawning a worker (all synchronous).  We verify this
/// by asserting the retained tree is present immediately after `parse_incremental`
/// (no async gap) and that spans are produced.
#[test]
#[ignore = "network + compiler: needs tree-sitter-rust grammar"]
fn small_edit_sync_parse_produces_spans() {
    let tmp = tempfile::tempdir().unwrap();
    let (loader, registry, meta) = make_loader(&tmp);
    let spec = registry.by_name("rust").expect("rust in manifest");
    let grammar = Arc::new(
        hjkl_bonsai::runtime::Grammar::load("rust", spec, &loader, &meta)
            .expect("load rust grammar"),
    );

    let pre = b"fn main() { let x = 1; }";
    let mut h = Highlighter::new(grammar).expect("create highlighter");
    parse_counter::reset();
    h.parse_initial(pre);
    assert_eq!(parse_counter::get(), 1, "one parse_initial");
    assert!(
        h.tree().is_some(),
        "tree must be present after parse_initial"
    );

    // Small edit: insert one byte at position 3 (delta = 1 < 1024).
    let post = b"fn Ymain() { let x = 1; }";
    let byte_delta: usize = 1; // new_end_byte - start_byte
    assert!(byte_delta < 1024, "test must use a sub-1KB edit");

    let edit = tree_sitter::InputEdit {
        start_byte: 3,
        old_end_byte: 3,
        new_end_byte: 4,
        start_position: tree_sitter::Point { row: 0, column: 3 },
        old_end_position: tree_sitter::Point { row: 0, column: 3 },
        new_end_position: tree_sitter::Point { row: 0, column: 4 },
    };

    h.edit(&edit);
    assert!(h.tree().is_some(), "tree must survive edit()");

    // Synchronous incremental parse — mimics what the small-edit sync path does.
    let ok = h.parse_incremental(post);
    assert!(ok, "incremental parse must succeed for small edit");
    assert!(
        h.tree().is_some(),
        "tree must be present after parse_incremental"
    );

    // Walk viewport — must produce spans immediately (no async gap).
    let spans = h.highlight_range(post, 0..post.len());
    assert!(
        !spans.is_empty(),
        "highlight_range must return spans after sync parse"
    );

    // parse_counter must still be 1: `parse_incremental` does NOT call `parse_initial`.
    assert_eq!(
        parse_counter::get(),
        1,
        "parse_incremental must not increment the parse_counter (that's only parse_initial)"
    );
}

/// Large edit (≥1KB delta): after a large replacement the retained tree must
/// still be updated via `parse_incremental` and spans must be produced for the
/// new source. This validates the Highlighter handles large edits correctly
/// regardless of how the app routes the request.
#[test]
#[ignore = "network + compiler: needs tree-sitter-rust grammar"]
fn large_edit_incremental_parse_still_produces_spans() {
    let tmp = tempfile::tempdir().unwrap();
    let (loader, registry, meta) = make_loader(&tmp);
    let spec = registry.by_name("rust").expect("rust in manifest");
    let grammar = Arc::new(
        hjkl_bonsai::runtime::Grammar::load("rust", spec, &loader, &meta)
            .expect("load rust grammar"),
    );

    // Start with a small file.
    let pre = b"fn main() {}";
    let mut h = Highlighter::new(grammar).expect("create highlighter");
    h.parse_initial(pre);

    // Build a large replacement (≥1024 bytes inserted).
    let mut large_insert = String::from("fn main() {\n");
    for i in 0..40 {
        large_insert.push_str(&format!("    let var_{i} = {i} * 2;\n"));
    }
    large_insert.push('}');
    let post = large_insert.as_bytes();
    let byte_delta = post.len().saturating_sub(pre.len());
    assert!(
        byte_delta >= 1024,
        "test must use a ≥1KB edit (got {byte_delta})"
    );

    // tree.edit: we approximate by telling the tree the old content was replaced.
    let edit = tree_sitter::InputEdit {
        start_byte: 0,
        old_end_byte: pre.len(),
        new_end_byte: post.len(),
        start_position: tree_sitter::Point { row: 0, column: 0 },
        old_end_position: tree_sitter::Point {
            row: 0,
            column: pre.len(),
        },
        new_end_position: tree_sitter::Point {
            row: post.iter().filter(|&&b| b == b'\n').count(),
            column: 1,
        },
    };
    h.edit(&edit);

    // parse_incremental must succeed even for large edits.
    let ok = h.parse_incremental(post);
    assert!(ok, "parse_incremental must succeed for large edit");

    // highlight_range must return spans against the new tree.
    let spans = h.highlight_range(post, 0..post.len());
    assert!(
        !spans.is_empty(),
        "highlight_range must return spans after large-edit incremental parse"
    );
    assert!(
        spans.iter().any(|s| s.capture().starts_with("keyword")),
        "expected keyword spans in large-edit result"
    );
}
