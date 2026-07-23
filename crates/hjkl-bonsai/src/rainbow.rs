//! Rainbow bracket overlay.
//!
//! Bundles `rainbows.scm` queries for 8 languages and exposes
//! [`rainbow_spans`] on [`Highlighter`] to emit depth-tagged
//! [`HighlightSpan`]s that `build_by_row_range` maps to palette colours.
//!
//! ## Design
//!
//! - Queries are bundled via `include_str!`, keyed by grammar name.
//! - The compiled `Query` is cached in a process-global map keyed by
//!   grammar name (cheap — at most 8 distinct entries).
//! - Depth is computed by walking each bracket node's ancestor chain and
//!   counting ancestors captured by the scope pattern.  Ancestor chains are
//!   short (tree depth), so this is O(viewport_brackets × tree_depth) with
//!   no document-wide scan.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::{LazyLock, RwLock};

use tree_sitter::{Query, QueryCursor, StreamingIterator as _};

use crate::HighlightSpan;
use crate::predicate::MetaValue;
use crate::runtime::Grammar;

// ---------------------------------------------------------------------------
// Bundled query content
// ---------------------------------------------------------------------------

/// Return the bundled `rainbows.scm` source for `lang`, or `None` when we
/// don't ship one. Handles common name aliases.
pub fn builtin_rainbows(lang: &str) -> Option<&'static str> {
    let canonical = match lang {
        "js" | "javascript" => "javascript",
        "ts" | "typescript" => "typescript",
        "py" | "python" => "python",
        "c++" | "cc" | "cpp" => "cpp",
        "rust" => "rust",
        "json" => "json",
        "c" => "c",
        "go" => "go",
        other => other,
    };
    match canonical {
        "rust" => Some(include_str!("../queries/rainbows/rust.scm")),
        "javascript" => Some(include_str!("../queries/rainbows/javascript.scm")),
        "typescript" => Some(include_str!("../queries/rainbows/typescript.scm")),
        "python" => Some(include_str!("../queries/rainbows/python.scm")),
        "json" => Some(include_str!("../queries/rainbows/json.scm")),
        "c" => Some(include_str!("../queries/rainbows/c.scm")),
        "cpp" => Some(include_str!("../queries/rainbows/cpp.scm")),
        "go" => Some(include_str!("../queries/rainbows/go.scm")),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Compiled-query cache
// ---------------------------------------------------------------------------

/// Cache of compiled rainbow queries keyed by grammar name.
///
/// Grammar name is stable per process (loaded once). The cache grows to
/// at most 8 entries (one per supported language) and is never evicted.
static RAINBOW_QUERY_CACHE: LazyLock<RwLock<HashMap<String, Option<CompiledRainbow>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Compiled rainbow query + capture indices for scope and bracket.
struct CompiledRainbow {
    query: Query,
    scope_idx: u32,
    bracket_idx: u32,
}

// SAFETY: tree_sitter::Query has unsafe Send+Sync impls.
unsafe impl Send for CompiledRainbow {}
unsafe impl Sync for CompiledRainbow {}

/// Get-or-compile the rainbow query for `grammar`. Returns `None` when no
/// bundled query exists for this grammar or compilation fails.
fn get_or_compile(grammar: &Grammar) -> Option<()> {
    // Fast read path.
    {
        let r = RAINBOW_QUERY_CACHE.read().unwrap();
        if r.contains_key(grammar.name()) {
            return r.get(grammar.name()).and_then(|v| v.as_ref()).map(|_| ());
        }
    }
    // Slow compile path.
    let src = builtin_rainbows(grammar.name())?;
    let compiled = match Query::new(grammar.language(), src) {
        Ok(q) => {
            let names = q.capture_names();
            let scope_idx = names
                .iter()
                .position(|n| *n == "rainbow.scope")
                .map(|i| i as u32);
            let bracket_idx = names
                .iter()
                .position(|n| *n == "rainbow.bracket")
                .map(|i| i as u32);
            match (scope_idx, bracket_idx) {
                (Some(s), Some(b)) => Some(CompiledRainbow {
                    query: q,
                    scope_idx: s,
                    bracket_idx: b,
                }),
                _ => {
                    tracing::warn!(
                        grammar = grammar.name(),
                        "rainbow query missing expected captures"
                    );
                    None
                }
            }
        }
        Err(e) => {
            tracing::warn!(grammar = grammar.name(), error = %e, "rainbow query failed to compile");
            None
        }
    };

    let present = compiled.is_some();
    RAINBOW_QUERY_CACHE
        .write()
        .unwrap()
        .insert(grammar.name().to_string(), compiled);
    if present { Some(()) } else { None }
}

// ---------------------------------------------------------------------------
// Capture name + metadata key
// ---------------------------------------------------------------------------

/// Capture name emitted for rainbow bracket spans.
pub const RAINBOW_BRACKET_CAPTURE: &str = "rainbow.bracket";

/// Metadata key carrying the nesting depth (`MetaValue::Int`).
pub const RAINBOW_DEPTH_KEY: &str = "rainbow.depth";

// ---------------------------------------------------------------------------
// Main entry point — called from walk_rows / Highlighter
// ---------------------------------------------------------------------------

/// Compute rainbow bracket spans for the byte range `range` of `source`.
///
/// Requires a retained `tree` and a compiled rainbow query for `grammar`.
/// Returns an empty vec when either is absent.
///
/// ## Depth algorithm
///
/// For each `@rainbow.bracket` node in the viewport:
/// 1. Walk the node's parent chain to the root.
/// 2. For each ancestor, run the scope query restricted to a single byte
///    (`ancestor.start_byte()..ancestor.start_byte()+1`) and check whether
///    the ancestor node id is captured.
/// 3. Depth = count of ancestors that match the scope capture.
///
/// This is O(viewport_brackets × tree_depth) — typically O(N × 20).
/// No document-wide scan; fully viewport-bounded for bracket collection.
pub fn rainbow_spans(
    tree: &tree_sitter::Tree,
    grammar: &Grammar,
    source: &[u8],
    range: Range<usize>,
) -> Vec<HighlightSpan> {
    // Ensure query is compiled (or known-absent).
    if get_or_compile(grammar).is_none() {
        return Vec::new();
    }

    let cache = RAINBOW_QUERY_CACHE.read().unwrap();
    let compiled = match cache.get(grammar.name()).and_then(|v| v.as_ref()) {
        Some(c) => c,
        None => return Vec::new(),
    };

    let mut cursor = QueryCursor::new();
    cursor.set_byte_range(range.clone());
    let root = tree.root_node();
    let mut matches = cursor.matches(&compiled.query, root, source);

    let bracket_idx = compiled.bracket_idx;

    // Collect bracket nodes from the viewport.
    let mut bracket_nodes: Vec<tree_sitter::Node<'_>> = Vec::new();
    while let Some(m) = matches.next() {
        for cap in m.captures {
            if cap.index == bracket_idx {
                bracket_nodes.push(cap.node);
            }
        }
    }
    // Release the borrow on `compiled` before we need the cursor again below.
    drop(matches);
    drop(cache);

    // For each bracket node, walk ancestors and count scope matches.
    let mut spans: Vec<HighlightSpan> = Vec::with_capacity(bracket_nodes.len());
    for bracket in bracket_nodes {
        let depth = count_scope_depth(bracket, grammar, source);
        let start = bracket.start_byte();
        let end = bracket.end_byte();
        if start >= end || end > source.len() {
            continue;
        }
        let mut metadata = HashMap::new();
        metadata.insert(RAINBOW_DEPTH_KEY.to_string(), MetaValue::Int(depth as i64));
        spans.push(HighlightSpan {
            byte_range: start..end,
            capture: RAINBOW_BRACKET_CAPTURE.to_string(),
            metadata,
        });
    }
    spans
}

/// Walk `node`'s ancestor chain and count how many ancestors are captured by
/// the `@rainbow.scope` pattern for `grammar`.
///
/// Uses a fresh `QueryCursor` scoped to each ancestor's start byte so we
/// avoid a full-document scan.
fn count_scope_depth(node: tree_sitter::Node<'_>, grammar: &Grammar, source: &[u8]) -> usize {
    let cache = RAINBOW_QUERY_CACHE.read().unwrap();
    let compiled = match cache.get(grammar.name()).and_then(|v| v.as_ref()) {
        Some(c) => c,
        None => return 0,
    };

    let mut depth = 0usize;
    let mut current = node;

    while let Some(parent) = current.parent() {
        // Run the scope query restricted to a 1-byte window at the ancestor's
        // start so only this specific ancestor can produce scope matches.
        let anc_start = parent.start_byte();
        let anc_end = (anc_start + 1).min(source.len());
        if anc_start >= source.len() {
            current = parent;
            continue;
        }

        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(anc_start..anc_end);
        let mut matches = cursor.matches(&compiled.query, parent, source);
        let scope_idx = compiled.scope_idx;

        let mut is_scope = false;
        while let Some(m) = matches.next() {
            for cap in m.captures {
                if cap.index == scope_idx && cap.node.id() == parent.id() {
                    is_scope = true;
                    break;
                }
            }
            if is_scope {
                break;
            }
        }
        if is_scope {
            depth += 1;
        }

        current = parent;
    }

    depth
}

// ---------------------------------------------------------------------------
// Rope variant — same logic but reads from ropey::Rope
// ---------------------------------------------------------------------------

/// Rope-backed text provider for tree-sitter `QueryCursor::matches`.
///
/// Materialises only the requested node's byte range from the rope's B-tree
/// chunks — never the full document.
fn rope_node_chunks(rope: &ropey::Rope, start: usize, end: usize) -> impl Iterator<Item = Vec<u8>> {
    // Snap to char boundaries: a stale retained tree can hand us offsets that
    // split a multi-byte char in the current rope, which `byte_slice` panics
    // on. Aligned nodes are unaffected (identity).
    let range = crate::rope_slice::safe_char_range(rope, start, end);
    if range.is_empty() {
        Box::new(std::iter::empty()) as Box<dyn Iterator<Item = Vec<u8>>>
    } else {
        let bytes: Vec<u8> = rope
            .byte_slice(range)
            .chunks()
            .flat_map(|c| c.as_bytes().iter().copied())
            .collect();
        Box::new(std::iter::once(bytes))
    }
}

/// Like [`rainbow_spans`] but reads source from a `ropey::Rope`.
///
/// Uses a rope-backed `TextProvider` closure for both the bracket-collection
/// cursor and each ancestor-depth cursor — no whole-document materialisation.
pub fn rainbow_spans_rope(
    tree: &tree_sitter::Tree,
    grammar: &Grammar,
    rope: &ropey::Rope,
    range: Range<usize>,
) -> Vec<HighlightSpan> {
    if get_or_compile(grammar).is_none() {
        return Vec::new();
    }

    let rope_len = rope.len_bytes();
    let win_start = range.start.min(rope_len);
    let win_end = range.end.min(rope_len);
    if win_start >= win_end {
        return Vec::new();
    }

    let cache = RAINBOW_QUERY_CACHE.read().unwrap();
    let compiled = match cache.get(grammar.name()).and_then(|v| v.as_ref()) {
        Some(c) => c,
        None => return Vec::new(),
    };

    let bracket_idx = compiled.bracket_idx;
    let scope_idx = compiled.scope_idx;
    let root = tree.root_node();

    // --- Bracket collection: rope TextProvider, viewport-bounded ---
    let mut cursor = QueryCursor::new();
    cursor.set_byte_range(win_start..win_end);
    let mut matches = cursor.matches(&compiled.query, root, |node: tree_sitter::Node| {
        let s = node.start_byte();
        let e = node.end_byte().min(rope_len);
        rope_node_chunks(rope, s, e)
    });

    let mut bracket_nodes: Vec<tree_sitter::Node<'_>> = Vec::new();
    while let Some(m) = matches.next() {
        for cap in m.captures {
            if cap.index == bracket_idx {
                bracket_nodes.push(cap.node);
            }
        }
    }
    drop(matches);

    // --- Depth walk: rope TextProvider per ancestor query ---
    let mut spans: Vec<HighlightSpan> = Vec::with_capacity(bracket_nodes.len());
    for bracket in bracket_nodes {
        // Walk ancestor chain counting scope matches.
        let mut depth = 0usize;
        let mut current = bracket;
        while let Some(parent) = current.parent() {
            let anc_start = parent.start_byte();
            let anc_end = (anc_start + 1).min(rope_len);
            if anc_start < rope_len {
                let mut anc_cursor = QueryCursor::new();
                anc_cursor.set_byte_range(anc_start..anc_end);
                let mut anc_matches =
                    anc_cursor.matches(&compiled.query, parent, |node: tree_sitter::Node| {
                        let s = node.start_byte();
                        let e = node.end_byte().min(rope_len);
                        rope_node_chunks(rope, s, e)
                    });
                let mut is_scope = false;
                while let Some(m) = anc_matches.next() {
                    for cap in m.captures {
                        if cap.index == scope_idx && cap.node.id() == parent.id() {
                            is_scope = true;
                            break;
                        }
                    }
                    if is_scope {
                        break;
                    }
                }
                if is_scope {
                    depth += 1;
                }
            }
            current = parent;
        }

        let start = bracket.start_byte();
        let end = bracket.end_byte();
        if start >= end || end > rope_len {
            continue;
        }
        let mut metadata = HashMap::new();
        metadata.insert(RAINBOW_DEPTH_KEY.to_string(), MetaValue::Int(depth as i64));
        spans.push(HighlightSpan {
            byte_range: start..end,
            capture: RAINBOW_BRACKET_CAPTURE.to_string(),
            metadata,
        });
    }

    spans
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_rainbows_known_langs() {
        assert!(builtin_rainbows("rust").is_some());
        assert!(builtin_rainbows("javascript").is_some());
        assert!(builtin_rainbows("typescript").is_some());
        assert!(builtin_rainbows("python").is_some());
        assert!(builtin_rainbows("json").is_some());
        assert!(builtin_rainbows("c").is_some());
        assert!(builtin_rainbows("cpp").is_some());
        assert!(builtin_rainbows("go").is_some());
    }

    #[test]
    fn builtin_rainbows_unknown_returns_none() {
        assert!(builtin_rainbows("cobol").is_none());
        assert!(builtin_rainbows("brainfuck").is_none());
        assert!(builtin_rainbows("").is_none());
    }

    #[test]
    fn builtin_rainbows_resolves_aliases() {
        // js → javascript
        assert_eq!(builtin_rainbows("js"), builtin_rainbows("javascript"));
        // ts → typescript
        assert_eq!(builtin_rainbows("ts"), builtin_rainbows("typescript"));
        // py → python
        assert_eq!(builtin_rainbows("py"), builtin_rainbows("python"));
        // cc / c++ → cpp
        assert_eq!(builtin_rainbows("cc"), builtin_rainbows("cpp"));
        assert_eq!(builtin_rainbows("c++"), builtin_rainbows("cpp"));
    }
}
