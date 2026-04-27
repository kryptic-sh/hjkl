use std::ops::Range;

use anyhow::{Context, Result};
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator as _};

use crate::registry::LanguageConfig;

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
/// update bookkeeping (incremental parsing is Phase 2; Phase B is full-reparse).
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

/// Stateful syntax highlighter for a single language.
///
/// Owns a `Parser`, the `Language`, a compiled `Query`, and the capture name
/// table. Call `highlight()` to get `HighlightSpan`s for a source buffer.
pub struct Highlighter {
    parser: Parser,
    query: Query,
    capture_names: Vec<String>,
}

impl Highlighter {
    /// Create a new highlighter for the given `LanguageConfig`.
    pub fn new(config: &LanguageConfig) -> Result<Self> {
        let mut parser = Parser::new();
        parser
            .set_language(&config.language)
            .context("failed to set tree-sitter language")?;

        let query = Query::new(&config.language, config.highlights_scm)
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
        })
    }

    /// Parse `source` and return the resulting `Syntax` (parse tree + dirty flag).
    pub fn parse(&mut self, source: &[u8]) -> Option<Syntax> {
        let tree = self.parser.parse(source, None)?;
        Some(Syntax { tree, dirty: false })
    }

    /// Parse `source` and run the highlights query, returning all `HighlightSpan`s
    /// in source order. Spans may overlap when multiple captures apply to the
    /// same range (e.g. a node matched by both `"type"` and `"type.builtin"`).
    pub fn highlight(&mut self, source: &[u8]) -> Vec<HighlightSpan> {
        let Some(tree) = self.parser.parse(source, None) else {
            return Vec::new();
        };

        let mut cursor = QueryCursor::new();
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
                let capture_name = self.capture_names[capture.index as usize].clone();
                spans.push(HighlightSpan {
                    byte_range: start..end,
                    capture: capture_name,
                });
            }
        }

        // Stable sort by start byte so callers can iterate in source order.
        spans.sort_by_key(|s| s.byte_range.start);
        spans
    }

    /// Parse `source` and harvest ERROR / MISSING nodes as `ParseError`s.
    pub fn parse_errors(&mut self, source: &[u8]) -> Vec<ParseError> {
        let Some(tree) = self.parser.parse(source, None) else {
            return Vec::new();
        };
        if !tree.root_node().has_error() {
            return Vec::new();
        }
        let mut errors = Vec::new();
        collect_parse_errors(tree.root_node(), source, &mut errors);
        errors
    }
}

/// Recursively collect ERROR / MISSING nodes from the tree.
fn collect_parse_errors(node: tree_sitter::Node, source: &[u8], out: &mut Vec<ParseError>) {
    if node.is_error() || node.is_missing() {
        let start = node.start_byte();
        let raw_end = node.end_byte().max(start + 1).min(source.len());
        if raw_end > start {
            // Clamp to first line of the error span.
            let line_end = source[start..raw_end]
                .iter()
                .position(|&b| b == b'\n')
                .map(|off| start + off)
                .unwrap_or(raw_end);

            let snippet = std::str::from_utf8(&source[start..line_end])
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
                byte_range: start..line_end,
                message,
            });
            // Don't descend — parent error covers children.
            return;
        }
    }

    if !node.has_error() {
        return;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_parse_errors(child, source, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::LanguageRegistry;

    #[test]
    fn highlights_rust_keyword() {
        let reg = LanguageRegistry::new();
        let config = reg.by_name("rust").unwrap();
        let mut h = Highlighter::new(config).unwrap();
        let spans = h.highlight(b"fn main() {}");
        assert!(
            spans.iter().any(|s| s.capture.starts_with("keyword")),
            "expected a keyword span for 'fn'; got: {spans:#?}"
        );
    }

    #[test]
    fn highlights_rust_string() {
        let reg = LanguageRegistry::new();
        let config = reg.by_name("rust").unwrap();
        let mut h = Highlighter::new(config).unwrap();
        let spans = h.highlight(b"let s = \"hello\";");
        assert!(
            spans.iter().any(|s| s.capture.starts_with("string")),
            "expected a string span; got: {spans:#?}"
        );
    }

    #[test]
    fn highlights_json_string() {
        let reg = LanguageRegistry::new();
        let config = reg.by_name("json").unwrap();
        let mut h = Highlighter::new(config).unwrap();
        let spans = h.highlight(br#"{"key": "value"}"#);
        assert!(!spans.is_empty(), "expected spans for JSON; got none");
    }

    #[test]
    fn highlights_toml_key() {
        let reg = LanguageRegistry::new();
        let config = reg.by_name("toml").unwrap();
        let mut h = Highlighter::new(config).unwrap();
        let spans = h.highlight(b"[package]\nname = \"foo\"\n");
        assert!(!spans.is_empty(), "expected spans for TOML; got none");
    }

    #[test]
    fn highlights_sql_keyword() {
        let reg = LanguageRegistry::new();
        let config = reg.by_name("sql").unwrap();
        let mut h = Highlighter::new(config).unwrap();
        let spans = h.highlight(b"SELECT id FROM users;");
        assert!(!spans.is_empty(), "expected spans for SQL; got none");
    }

    #[test]
    fn highlight_empty_input() {
        let reg = LanguageRegistry::new();
        let config = reg.by_name("rust").unwrap();
        let mut h = Highlighter::new(config).unwrap();
        let spans = h.highlight(b"");
        assert!(spans.is_empty());
    }

    #[test]
    fn parse_returns_syntax() {
        let reg = LanguageRegistry::new();
        let config = reg.by_name("rust").unwrap();
        let mut h = Highlighter::new(config).unwrap();
        let syntax = h.parse(b"fn main() {}");
        assert!(syntax.is_some());
    }

    #[test]
    fn parse_errors_clean_source() {
        let reg = LanguageRegistry::new();
        let config = reg.by_name("rust").unwrap();
        let mut h = Highlighter::new(config).unwrap();
        let errors = h.parse_errors(b"fn main() {}");
        // Well-formed source should produce zero errors.
        assert!(errors.is_empty(), "unexpected parse errors: {errors:#?}");
    }
}
