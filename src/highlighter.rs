//! Stateful syntax highlighter built on top of a runtime-loaded [`Grammar`].
//!
//! A [`Highlighter`] owns a `Parser` + compiled `Query` for one language and
//! keeps a reference to the [`Grammar`] alive (so the underlying `dlopen`-ed
//! shared library outlives any tree the parser produces).

use std::ops::Range;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use tree_sitter::{ParseOptions, Parser, Query, QueryCursor, StreamingIterator as _};

use crate::runtime::Grammar;

/// A byte-range tagged with the tree-sitter capture name that applies to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightSpan {
    /// Byte range in the source buffer.
    pub byte_range: Range<usize>,
    /// The capture name from the highlights.scm query, e.g. `"keyword.control"`.
    pub capture: String,
}

impl HighlightSpan {
    /// The capture name as a `&str` slice.
    pub fn capture(&self) -> &str {
        &self.capture
    }
}

/// A parse error harvested from tree-sitter's ERROR / MISSING nodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// Byte range of the error node (clamped to the first line).
    pub byte_range: Range<usize>,
    /// Human-readable description, e.g. `"unexpected \`foo\`"`.
    pub message: String,
}

/// The parsed syntax tree for a buffer, plus a dirty flag for incremental
/// update bookkeeping.
pub struct Syntax {
    pub(crate) tree: tree_sitter::Tree,
    pub dirty: bool,
}

impl Syntax {
    /// Access the underlying tree-sitter `Tree`.
    pub fn tree(&self) -> &tree_sitter::Tree {
        &self.tree
    }
}

/// Default parser timeout for `parse_incremental`, in microseconds.
/// `0` = no timeout (fast path that takes the direct `Parser::parse`
/// call instead of the streaming callback form).
const DEFAULT_PARSE_TIMEOUT_MICROS: u64 = 0;

/// Stateful syntax highlighter for a single language.
///
/// Owns a `Parser`, a compiled `Query`, and a reference-counted handle on the
/// [`Grammar`] so the underlying shared library cannot drop while a parse
/// tree is live.
pub struct Highlighter {
    parser: Parser,
    query: Query,
    capture_names: Vec<String>,
    tree: Option<tree_sitter::Tree>,
    parse_timeout_micros: u64,
    /// Held to keep the dlopen-ed shared library alive. Field order matters
    /// (parse trees reference data inside `_grammar`'s `Library`); placing
    /// `_grammar` last guarantees it drops after `tree` and `query`.
    _grammar: Arc<Grammar>,
}

impl Highlighter {
    /// Create a new highlighter for `grammar`'s language using its bundled
    /// `highlights.scm`.
    pub fn new(grammar: Arc<Grammar>) -> Result<Self> {
        let mut parser = Parser::new();
        parser
            .set_language(grammar.language())
            .context("failed to set tree-sitter language")?;

        let query = Query::new(grammar.language(), grammar.highlights_scm())
            .context("failed to compile highlights.scm query")?;

        let capture_names: Vec<String> = query
            .capture_names()
            .iter()
            .map(|s| s.to_string())
            .collect();

        Ok(Self {
            parser,
            query,
            capture_names,
            tree: None,
            parse_timeout_micros: DEFAULT_PARSE_TIMEOUT_MICROS,
            _grammar: grammar,
        })
    }

    /// Apply an `InputEdit` to the retained tree, if any. No-op when the
    /// highlighter has no retained tree.
    pub fn edit(&mut self, edit: &tree_sitter::InputEdit) {
        if let Some(tree) = self.tree.as_mut() {
            tree.edit(edit);
        }
    }

    /// Reparse `source` against the retained tree (if any) under the
    /// configured timeout. Returns `true` on success, replacing the
    /// retained tree. Returns `false` on timeout, leaving the previous
    /// retained tree in place.
    ///
    /// **Important:** when this returns `false`, do not call
    /// [`Highlighter::highlight_range`] until a subsequent
    /// `parse_incremental` succeeds — the retained tree is stale relative
    /// to `source`.
    pub fn parse_incremental(&mut self, source: &[u8]) -> bool {
        if self.parse_timeout_micros == 0 {
            let result = self.parser.parse(source, self.tree.as_ref());
            return match result {
                Some(t) => {
                    self.tree = Some(t);
                    true
                }
                None => false,
            };
        }
        let deadline = Instant::now() + std::time::Duration::from_micros(self.parse_timeout_micros);
        let mut progress = move |_state: &tree_sitter::ParseState| {
            if Instant::now() >= deadline {
                return std::ops::ControlFlow::Break(());
            }
            std::ops::ControlFlow::Continue(())
        };
        let mut opts = ParseOptions::new().progress_callback(&mut progress);
        let bytes = source;
        let len = bytes.len();
        let result = self.parser.parse_with_options(
            &mut |i, _| {
                if i < len {
                    &bytes[i..]
                } else {
                    Default::default()
                }
            },
            self.tree.as_ref(),
            Some(opts.reborrow()),
        );
        match result {
            Some(t) => {
                self.tree = Some(t);
                true
            }
            None => false,
        }
    }

    /// Parse `source` from scratch with the parser timeout disabled. Used on
    /// initial load and after `reset()`.
    pub fn parse_initial(&mut self, source: &[u8]) {
        let result = self.parser.parse(source, None);
        if let Some(t) = result {
            self.tree = Some(t);
        }
    }

    /// Run the highlights query against the retained tree, scoped to
    /// `byte_range`. Returns spans whose byte range overlaps `byte_range`,
    /// sorted by start byte. Empty when there's no retained tree.
    pub fn highlight_range(
        &mut self,
        source: &[u8],
        byte_range: Range<usize>,
    ) -> Vec<HighlightSpan> {
        let Some(tree) = self.tree.as_ref() else {
            return Vec::new();
        };

        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(byte_range.clone());
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut spans: Vec<HighlightSpan> = Vec::new();
        while let Some(m) = matches.next() {
            for capture in m.captures {
                let node = capture.node;
                let start = node.start_byte();
                let end = node.end_byte();
                if start >= end || end > source.len() {
                    continue;
                }
                if start >= byte_range.end || end <= byte_range.start {
                    continue;
                }
                let capture_name = self.capture_names[capture.index as usize].clone();
                spans.push(HighlightSpan {
                    byte_range: start..end,
                    capture: capture_name,
                });
            }
        }

        spans.sort_by_key(|s| s.byte_range.start);
        spans
    }

    /// Walk the retained tree and collect ERROR / MISSING nodes whose byte
    /// range intersects `byte_range`.
    pub fn parse_errors_range(
        &mut self,
        source: &[u8],
        byte_range: Range<usize>,
    ) -> Vec<ParseError> {
        let Some(tree) = self.tree.as_ref() else {
            return Vec::new();
        };
        if !tree.root_node().has_error() {
            return Vec::new();
        }
        let mut errors = Vec::new();
        collect_parse_errors(tree.root_node(), source, &byte_range, &mut errors);
        errors
    }

    /// Read accessor for the retained tree.
    pub fn tree(&self) -> Option<&tree_sitter::Tree> {
        self.tree.as_ref()
    }

    /// Override the parser timeout used by `parse_incremental`. `0` disables
    /// the timeout.
    pub fn set_parse_timeout_micros(&mut self, micros: u64) {
        self.parse_timeout_micros = micros;
    }

    /// Drop the retained tree.
    pub fn reset(&mut self) {
        self.tree = None;
    }

    /// Parse `source` and return the resulting `Syntax`. Standalone — does
    /// not touch the retained tree.
    pub fn parse(&mut self, source: &[u8]) -> Option<Syntax> {
        let tree = self.parser.parse(source, None)?;
        Some(Syntax { tree, dirty: false })
    }

    /// Parse `source` and run the highlights query, returning all
    /// `HighlightSpan`s in source order.
    pub fn highlight(&mut self, source: &[u8]) -> Vec<HighlightSpan> {
        if self.tree.is_none() {
            self.parse_initial(source);
        } else if !self.parse_incremental(source) {
            return Vec::new();
        }
        self.highlight_range(source, 0..source.len())
    }

    /// Parse `source` and harvest ERROR / MISSING nodes as `ParseError`s.
    pub fn parse_errors(&mut self, source: &[u8]) -> Vec<ParseError> {
        if self.tree.is_none() {
            self.parse_initial(source);
        } else if !self.parse_incremental(source) {
            return Vec::new();
        }
        self.parse_errors_range(source, 0..source.len())
    }
}

fn collect_parse_errors(
    node: tree_sitter::Node,
    source: &[u8],
    range: &Range<usize>,
    out: &mut Vec<ParseError>,
) {
    let n_start = node.start_byte();
    let n_end = node.end_byte();
    if n_end <= range.start || n_start >= range.end {
        return;
    }
    if node.is_error() || node.is_missing() {
        let raw_end = n_end.max(n_start + 1).min(source.len());
        if raw_end > n_start {
            let line_end = source[n_start..raw_end]
                .iter()
                .position(|&b| b == b'\n')
                .map(|off| n_start + off)
                .unwrap_or(raw_end);

            let snippet = std::str::from_utf8(&source[n_start..line_end])
                .unwrap_or("")
                .trim();
            let kind = node.kind();
            let message = if node.is_missing() {
                if kind.is_empty() {
                    "missing token".to_string()
                } else {
                    format!("missing `{kind}`")
                }
            } else if snippet.is_empty() {
                "unexpected token".to_string()
            } else {
                let trimmed: String = snippet.chars().take(60).collect();
                format!("unexpected `{trimmed}`")
            };

            out.push(ParseError {
                byte_range: n_start..line_end,
                message,
            });
            return;
        }
    }

    if !node.has_error() {
        return;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_parse_errors(child, source, range, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{GrammarCompiler, GrammarLoader, LangSpec, SourceCache};

    fn c_grammar_loader() -> (Arc<Grammar>, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let sources = SourceCache::new(tmp.path().join("cache"));
        let user_dir = tmp.path().join("user");
        let loader = GrammarLoader::new(vec![], user_dir, sources, GrammarCompiler::new());

        let spec = LangSpec {
            git_url: "https://github.com/tree-sitter/tree-sitter-c".into(),
            git_rev: "2a265d69a4caf57108a73ad2ed1e6922dd2f998c".into(),
            subpath: None,
            extensions: vec!["c".into()],
            c_files: vec!["src/parser.c".into()],
            query_dir: "queries".into(),
            source: None,
        };

        let g = Grammar::load("c", &spec, &loader).unwrap();
        (Arc::new(g), tmp)
    }

    /// All highlighter tests need a real grammar (network clone + cc compile).
    /// Run with: `cargo test -p hjkl-bonsai -- --ignored`.
    #[test]
    #[ignore = "network + compiler"]
    fn highlights_c_keyword() {
        let (g, _tmp) = c_grammar_loader();
        let mut h = Highlighter::new(g).unwrap();
        let spans = h.highlight(b"int main() { return 0; }");
        assert!(
            spans.iter().any(|s| s.capture.starts_with("keyword")),
            "expected a keyword span; got: {spans:#?}"
        );
    }

    #[test]
    #[ignore = "network + compiler"]
    fn highlight_empty_input() {
        let (g, _tmp) = c_grammar_loader();
        let mut h = Highlighter::new(g).unwrap();
        let spans = h.highlight(b"");
        assert!(spans.is_empty());
    }

    #[test]
    #[ignore = "network + compiler"]
    fn parse_returns_syntax() {
        let (g, _tmp) = c_grammar_loader();
        let mut h = Highlighter::new(g).unwrap();
        let syntax = h.parse(b"int main() {}");
        assert!(syntax.is_some());
    }

    #[test]
    #[ignore = "network + compiler"]
    fn parse_errors_clean_source() {
        let (g, _tmp) = c_grammar_loader();
        let mut h = Highlighter::new(g).unwrap();
        let errors = h.parse_errors(b"int main() {}");
        assert!(errors.is_empty(), "unexpected parse errors: {errors:#?}");
    }

    #[test]
    #[ignore = "network + compiler"]
    fn incremental_edit_matches_cold_parse() {
        let (g, _tmp) = c_grammar_loader();
        let pre: &[u8] = b"int main() {}";
        let post: &[u8] = b"int Xmain() {}";

        let mut h_inc = Highlighter::new(g.clone()).unwrap();
        h_inc.parse_initial(pre);
        let edit = tree_sitter::InputEdit {
            start_byte: 4,
            old_end_byte: 4,
            new_end_byte: 5,
            start_position: tree_sitter::Point { row: 0, column: 4 },
            old_end_position: tree_sitter::Point { row: 0, column: 4 },
            new_end_position: tree_sitter::Point { row: 0, column: 5 },
        };
        h_inc.edit(&edit);
        assert!(h_inc.parse_incremental(post));
        let inc_spans = h_inc.highlight_range(post, 0..post.len());

        let mut h_cold = Highlighter::new(g).unwrap();
        let cold_spans = h_cold.highlight(post);

        assert_eq!(inc_spans, cold_spans);
    }

    #[test]
    #[ignore = "network + compiler"]
    fn reset_clears_tree() {
        let (g, _tmp) = c_grammar_loader();
        let mut h = Highlighter::new(g).unwrap();
        h.parse_initial(b"int main() {}");
        assert!(h.tree().is_some());
        h.reset();
        assert!(h.tree().is_none());
    }
}
