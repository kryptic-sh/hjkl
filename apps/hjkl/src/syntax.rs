//! `SyntaxLayer` — tree-sitter highlight computation for the TUI binary.
//!
//! Owns a `LanguageRegistry`, an optional `Highlighter`, and a `Theme`.
//! Call [`SyntaxLayer::set_language_for_path`] after opening a file, then
//! [`SyntaxLayer::apply_edits`] for each frame's queued
//! [`hjkl_engine::ContentEdit`] batch and
//! [`SyntaxLayer::parse_and_render`] to produce per-row styled spans
//! ready for [`hjkl_engine::Editor::install_ratatui_syntax_spans`].

use std::path::Path;

use hjkl_engine::Query;
use hjkl_tree_sitter::{DotFallbackTheme, Highlighter, InputEdit, LanguageRegistry, Point, Theme};

/// Cached `(source, row_starts)` keyed off buffer identity (dirty_gen +
/// shape). Built once per buffer mutation and reused across scroll-only
/// frames so per-frame work is just the viewport-scoped highlight query.
struct RenderCache {
    dirty_gen: u64,
    len_bytes: usize,
    line_count: u32,
    source: String,
    row_starts: Vec<usize>,
    /// dirty_gen of the buffer when `parse_incremental` last ran
    /// successfully. When the current `dirty_gen` matches, the retained
    /// tree is still valid and we can skip the reparse entirely.
    parsed_dirty_gen: Option<u64>,
}

/// Per-`App` syntax highlighting layer.
pub struct SyntaxLayer {
    registry: LanguageRegistry,
    highlighter: Option<Highlighter>,
    theme: Box<dyn Theme>,
    cache: Option<RenderCache>,
}

impl SyntaxLayer {
    /// Create a new layer with no language attached and the given theme.
    pub fn new(theme: Box<dyn Theme>) -> Self {
        Self {
            registry: LanguageRegistry::new(),
            highlighter: None,
            theme,
            cache: None,
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

    /// Swap the active theme. Next render call will use the new theme.
    pub fn set_theme(&mut self, theme: Box<dyn Theme>) {
        self.theme = theme;
    }

    /// Drop the retained tree on the underlying highlighter (if any) so
    /// the next `parse_and_render` does a cold parse.
    pub fn reset(&mut self) {
        if let Some(h) = self.highlighter.as_mut() {
            h.reset();
        }
    }

    /// Fan a batch of engine `ContentEdit`s into the retained tree via
    /// `Highlighter::edit`. No-op when no language is attached.
    pub fn apply_edits(&mut self, edits: &[hjkl_engine::ContentEdit]) {
        let Some(h) = self.highlighter.as_mut() else {
            return;
        };
        for e in edits {
            let ie = InputEdit {
                start_byte: e.start_byte,
                old_end_byte: e.old_end_byte,
                new_end_byte: e.new_end_byte,
                start_position: Point {
                    row: e.start_position.0 as usize,
                    column: e.start_position.1 as usize,
                },
                old_end_position: Point {
                    row: e.old_end_position.0 as usize,
                    column: e.old_end_position.1 as usize,
                },
                new_end_position: Point {
                    row: e.new_end_position.0 as usize,
                    column: e.new_end_position.1 as usize,
                },
            };
            h.edit(&ie);
        }
    }

    /// Reparse the buffer (incremental when a tree is retained, cold
    /// otherwise) and run the highlights query scoped to the viewport
    /// byte range. Returns per-row styled spans sized to the **full**
    /// row count so stale rows clear correctly when content shrinks.
    ///
    /// Returns `None` when no language is attached.
    pub fn parse_and_render(
        &mut self,
        buffer: &impl Query,
        viewport_top: usize,
        viewport_height: usize,
    ) -> Option<Vec<Vec<(usize, usize, ratatui::style::Style)>>> {
        let highlighter = self.highlighter.as_mut()?;

        let dg = buffer.dirty_gen();
        let lb = buffer.len_bytes();
        let lc = buffer.line_count();
        let row_count = lc as usize;

        // Rebuild source + row_starts only when the buffer has changed.
        // Pure scroll frames hit the cache and skip the O(N) string join +
        // newline scan, leaving only the viewport-scoped highlight query.
        let needs_rebuild = match &self.cache {
            Some(c) => c.dirty_gen != dg || c.len_bytes != lb || c.line_count != lc,
            None => true,
        };
        if needs_rebuild {
            let mut source = String::with_capacity(lb);
            for r in 0..row_count {
                if r > 0 {
                    source.push('\n');
                }
                source.push_str(buffer.line(r as u32));
            }
            let mut row_starts: Vec<usize> = vec![0];
            for (i, &b) in source.as_bytes().iter().enumerate() {
                if b == b'\n' {
                    row_starts.push(i + 1);
                }
            }
            self.cache = Some(RenderCache {
                dirty_gen: dg,
                len_bytes: lb,
                line_count: lc,
                source,
                row_starts,
                parsed_dirty_gen: None,
            });
        }
        let bytes = self.cache.as_ref().unwrap().source.as_bytes();

        // Skip the reparse entirely when the buffer hasn't mutated since
        // the last successful parse — the retained tree is still valid,
        // we only need the viewport-scoped query.
        let parse_needed = self
            .cache
            .as_ref()
            .and_then(|c| c.parsed_dirty_gen)
            .map(|g| g != dg)
            .unwrap_or(true);

        if parse_needed {
            // Cold parse on first frame (no retained tree) so we always
            // succeed regardless of file size; subsequent frames go through
            // the timed incremental path.
            if highlighter.tree().is_none() {
                highlighter.parse_initial(bytes);
            } else if !highlighter.parse_incremental(bytes) {
                // Timed-out parse: skip this frame's render. Spans table from
                // the previous render is still installed on the editor; next
                // frame will retry. Return None so the caller doesn't reinstall
                // an empty / stale table.
                return None;
            }
            self.cache.as_mut().unwrap().parsed_dirty_gen = Some(dg);
        }
        let cache = self.cache.as_ref().expect("cache populated above");
        let bytes = cache.source.as_bytes();
        let row_starts = &cache.row_starts;

        // Compute viewport byte range. byte_of_row clamps past-end to
        // len_bytes so the +1 row beyond the visible range is safe.
        let vp_start = buffer.byte_of_row(viewport_top);
        let vp_end_row = viewport_top + viewport_height + 1;
        let vp_end = buffer.byte_of_row(vp_end_row).min(bytes.len());
        let vp_end = vp_end.max(vp_start);

        let flat_spans = highlighter.highlight_range(bytes, vp_start..vp_end);

        let mut by_row: Vec<Vec<(usize, usize, ratatui::style::Style)>> =
            vec![Vec::new(); row_count];

        for span in &flat_spans {
            let style = match self.theme.style(span.capture()) {
                Some(s) => s.to_ratatui(),
                None => continue,
            };

            let span_start = span.byte_range.start;
            let span_end = span.byte_range.end;

            let start_row = row_starts
                .partition_point(|&rs| rs <= span_start)
                .saturating_sub(1);

            let mut row = start_row;
            while row < row_count {
                let row_byte_start = row_starts[row];
                let row_byte_end = row_starts
                    .get(row + 1)
                    .map(|&s| s.saturating_sub(1))
                    .unwrap_or(bytes.len());

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

#[cfg(test)]
mod tests {
    use super::*;
    use hjkl_buffer::Buffer;
    use std::path::Path;

    #[test]
    fn parse_and_render_small_rust_buffer() {
        let buf = Buffer::from_str("fn main() { let x = 1; }\n");
        let mut layer = default_layer();
        assert!(layer.set_language_for_path(Path::new("a.rs")));
        let by_row = layer.parse_and_render(&buf, 0, 10).unwrap();
        assert_eq!(by_row.len(), buf.row_count());
        assert!(
            by_row.iter().any(|r| !r.is_empty()),
            "expected at least one styled span"
        );
    }

    #[test]
    fn parse_and_render_returns_none_without_language() {
        let buf = Buffer::from_str("hello world");
        let mut layer = default_layer();
        // Unknown extension — no language attached.
        assert!(!layer.set_language_for_path(Path::new("a.unknownext")));
        assert!(layer.parse_and_render(&buf, 0, 10).is_none());
    }

    #[test]
    fn apply_edits_with_no_language_is_noop() {
        let mut layer = default_layer();
        let edits = vec![hjkl_engine::ContentEdit {
            start_byte: 0,
            old_end_byte: 0,
            new_end_byte: 1,
            start_position: (0, 0),
            old_end_position: (0, 0),
            new_end_position: (0, 1),
        }];
        layer.apply_edits(&edits);
    }

    #[test]
    fn incremental_path_matches_cold_for_small_edit() {
        // Simulate the app loop: parse, edit, parse_and_render, compare
        // to a fresh cold parse of the post-edit buffer.
        let pre = Buffer::from_str("fn main() { let x = 1; }");
        let mut layer = default_layer();
        layer.set_language_for_path(Path::new("a.rs"));
        let _ = layer.parse_and_render(&pre, 0, 10).unwrap();

        // Apply an edit: insert "Y" at byte 3 ("fn ⎀main…").
        layer.apply_edits(&[hjkl_engine::ContentEdit {
            start_byte: 3,
            old_end_byte: 3,
            new_end_byte: 4,
            start_position: (0, 3),
            old_end_position: (0, 3),
            new_end_position: (0, 4),
        }]);
        let post = Buffer::from_str("fn Ymain() { let x = 1; }");
        let inc = layer.parse_and_render(&post, 0, 10).unwrap();

        let mut cold_layer = default_layer();
        cold_layer.set_language_for_path(Path::new("a.rs"));
        let cold = cold_layer.parse_and_render(&post, 0, 10).unwrap();

        assert_eq!(inc, cold);
    }
}

#[cfg(test)]
mod perf_smoke {
    use super::*;
    use hjkl_buffer::Buffer;
    use std::path::Path;
    use std::time::Instant;

    /// Smoke perf: open /tmp/big.rs (100k stub fns, ~1.3MB), parse +
    /// scroll-render 100 times. Skipped when the file isn't present.
    /// Not a regression gate — eyeballs only via `--nocapture`.
    #[test]
    fn big_rs_smoke() {
        let path = Path::new("/tmp/big.rs");
        if !path.exists() {
            eprintln!("/tmp/big.rs not present; skipping perf smoke");
            return;
        }
        let content = std::fs::read_to_string(path).unwrap();
        let buf = Buffer::from_str(content.strip_suffix('\n').unwrap_or(&content));
        let mut layer = default_layer();
        assert!(layer.set_language_for_path(path));

        let t0 = Instant::now();
        let _ = layer.parse_and_render(&buf, 0, 50);
        eprintln!("first parse_and_render: {:?}", t0.elapsed());

        let t0 = Instant::now();
        for top in 0..100 {
            let _ = layer.parse_and_render(&buf, top * 100, 50);
        }
        eprintln!(
            "100 viewport scrolls (incremental, no edits): {:?}",
            t0.elapsed()
        );
    }
}
