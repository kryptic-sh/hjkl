//! `SyntaxLayer` — tree-sitter highlight computation for the TUI binary.
//!
//! Owns a `LanguageRegistry`, an optional `Highlighter`, and a `Theme`.
//! Call [`SyntaxLayer::set_language_for_path`] after opening a file, then
//! [`SyntaxLayer::recompute`] after every buffer mutation to get per-row
//! styled spans ready for [`hjkl_engine::Editor::install_ratatui_syntax_spans`].

use std::path::Path;

use hjkl_engine::Query;
use hjkl_tree_sitter::{DotFallbackTheme, Highlighter, LanguageRegistry, Theme};

/// Per-`App` syntax highlighting layer.
pub struct SyntaxLayer {
    registry: LanguageRegistry,
    highlighter: Option<Highlighter>,
    theme: Box<dyn Theme>,
}

impl SyntaxLayer {
    /// Create a new layer with no language attached and the given theme.
    pub fn new(theme: Box<dyn Theme>) -> Self {
        Self {
            registry: LanguageRegistry::new(),
            highlighter: None,
            theme,
        }
    }

    /// Detect the language for `path` and attach a `Highlighter`.
    ///
    /// Returns `true` when a language was found and the highlighter was set.
    /// Returns `false` (and clears the highlighter) for unknown extensions.
    pub fn set_language_for_path(&mut self, path: &Path) -> bool {
        match self.registry.detect_for_path(path) {
            Some(config) => match Highlighter::new(config) {
                Ok(h) => {
                    self.highlighter = Some(h);
                    true
                }
                Err(_) => {
                    self.highlighter = None;
                    false
                }
            },
            None => {
                self.highlighter = None;
                false
            }
        }
    }

    /// Swap the active theme. Next `recompute` call will use the new theme.
    pub fn set_theme(&mut self, theme: Box<dyn Theme>) {
        self.theme = theme;
    }

    /// Run the highlighter over `buffer` and return per-row styled spans.
    ///
    /// Returns `None` when no language is attached (caller skips install).
    /// Span format: `(byte_start_in_row, byte_end_in_row, ratatui::style::Style)`.
    pub fn recompute(
        &mut self,
        buffer: &impl Query,
    ) -> Option<Vec<Vec<(usize, usize, ratatui::style::Style)>>> {
        let highlighter = self.highlighter.as_mut()?;

        // Serialize buffer to bytes the same way the save path does.
        let row_count = buffer.line_count() as usize;
        let source: Vec<u8> = if row_count == 0 {
            Vec::new()
        } else {
            let mut s = String::new();
            for i in 0..row_count as u32 {
                if i > 0 {
                    s.push('\n');
                }
                s.push_str(buffer.line(i));
            }
            s.push('\n');
            s.into_bytes()
        };

        if source.is_empty() {
            // Return empty per-row table so the engine clears stale spans.
            return Some(Vec::new());
        }

        let flat_spans = highlighter.highlight(&source);

        // Build a newline-offset table for O(1) row/col lookup.
        let mut row_starts: Vec<usize> = vec![0];
        for (i, &b) in source.iter().enumerate() {
            if b == b'\n' {
                row_starts.push(i + 1);
            }
        }
        let mut by_row: Vec<Vec<(usize, usize, ratatui::style::Style)>> =
            vec![Vec::new(); row_count];

        for span in &flat_spans {
            let style = match self.theme.style(span.capture()) {
                Some(s) => s.to_ratatui(),
                None => continue,
            };

            let span_start = span.byte_range.start;
            let span_end = span.byte_range.end;

            // Find the first row that contains span_start.
            let start_row = row_starts
                .partition_point(|&rs| rs <= span_start)
                .saturating_sub(1);

            // Iterate over rows covered by this span.
            let mut row = start_row;
            while row < row_count {
                let row_byte_start = row_starts[row];
                let row_byte_end = row_starts
                    .get(row + 1)
                    .map(|&s| s.saturating_sub(1)) // exclude the '\n'
                    .unwrap_or(source.len());

                if row_byte_start >= span_end {
                    break;
                }

                let local_start = span_start.saturating_sub(row_byte_start);
                let local_end = span_end.min(row_byte_end) - row_byte_start;

                if local_end > local_start {
                    by_row[row].push((local_start, local_end, style));
                }

                row += 1;
            }
        }

        Some(by_row)
    }
}

/// Build the default dark `SyntaxLayer`.
pub fn default_layer() -> SyntaxLayer {
    SyntaxLayer::new(Box::new(DotFallbackTheme::dark()))
}
