//! Ratatui adapter for `hjkl-syntax`.
//!
//! Converts [`hjkl_syntax::RenderOutput`] (renderer-agnostic
//! [`hjkl_theme::StyleSpec`] spans) into `ratatui::style::Style`-typed row
//! tables and routes [`hjkl_syntax::DiagSign`]s to [`hjkl_buffer_tui::Sign`]
//! values for gutter rendering.
//!
//! # Quick-start
//!
//! ```rust
//! use hjkl_syntax::{DiagSign, RenderOutput, PerfBreakdown};
//! use hjkl_syntax_tui::{to_ratatui_spans, diag_signs_to_buffer_signs};
//!
//! // An empty output with no spans and no signs.
//! let out = RenderOutput::new(0, vec![], vec![], (0, 0, 10), PerfBreakdown::new());
//! let rows = to_ratatui_spans(&out.spans);
//! assert!(rows.is_empty());
//!
//! let signs = diag_signs_to_buffer_signs(&out.signs);
//! assert!(signs.is_empty());
//! ```

use hjkl_buffer_tui::Sign;
use hjkl_syntax::{DiagSign, RenderOutput, StyleSpec};
use hjkl_theme_tui::ToRatatui;
use ratatui::style::{Color, Style};

// ---------------------------------------------------------------------------
// Public conversion functions
// ---------------------------------------------------------------------------

/// Convert a per-row [`StyleSpec`] span table (as produced by
/// [`hjkl_syntax::RenderOutput::spans`]) into the equivalent
/// `ratatui::style::Style`-typed table consumed by
/// `hjkl_editor_tui::install_ratatui_syntax_spans`.
///
/// Each inner `(byte_start, byte_end, StyleSpec)` triple becomes
/// `(byte_start, byte_end, ratatui::style::Style)`.
///
/// # Examples
///
/// ```rust
/// use hjkl_syntax::StyleSpec;
/// use hjkl_syntax_tui::to_ratatui_spans;
///
/// let spans: Vec<Vec<(usize, usize, StyleSpec)>> = vec![
///     vec![(0, 5, StyleSpec::default())],
///     vec![],
/// ];
/// let rows = to_ratatui_spans(&spans);
/// assert_eq!(rows.len(), 2);
/// assert_eq!(rows[0].len(), 1);
/// assert!(rows[1].is_empty());
/// ```
pub fn to_ratatui_spans(
    spans: &[Vec<(usize, usize, StyleSpec)>],
) -> Vec<Vec<(usize, usize, Style)>> {
    spans
        .iter()
        .map(|row| {
            row.iter()
                .map(|(start, end, spec)| (*start, *end, spec.to_ratatui()))
                .collect()
        })
        .collect()
}

/// Convert a single [`StyleSpec`] to a `ratatui::style::Style`.
///
/// Convenience wrapper around the [`ToRatatui`] trait for callers that work
/// with individual styles rather than the whole span table.
///
/// # Examples
///
/// ```rust
/// use hjkl_syntax::StyleSpec;
/// use hjkl_syntax_tui::spec_to_ratatui;
///
/// let style = spec_to_ratatui(&StyleSpec::default());
/// // Default StyleSpec has no fg/bg and no modifiers; should round-trip.
/// let _ = style;
/// ```
pub fn spec_to_ratatui(spec: &StyleSpec) -> Style {
    spec.to_ratatui()
}

/// Convert [`DiagSign`]s (renderer-agnostic) into [`hjkl_buffer_tui::Sign`]s
/// (ratatui-styled) using the canonical error colour (red foreground).
///
/// Higher-priority signs take precedence when multiple signs land on the
/// same gutter row. The priority from [`DiagSign::priority`] is preserved.
///
/// # Examples
///
/// ```rust
/// use hjkl_syntax::DiagSign;
/// use hjkl_syntax_tui::diag_signs_to_buffer_signs;
///
/// let diags = vec![DiagSign::new(3, 'E', 100)];
/// let signs = diag_signs_to_buffer_signs(&diags);
/// assert_eq!(signs.len(), 1);
/// assert_eq!(signs[0].row, 3);
/// assert_eq!(signs[0].ch, 'E');
/// assert_eq!(signs[0].priority, 100);
/// ```
pub fn diag_signs_to_buffer_signs(signs: &[DiagSign]) -> Vec<Sign> {
    let err_style = Style::default().fg(Color::Red);
    signs
        .iter()
        .map(|s| Sign {
            row: s.row,
            ch: s.ch,
            style: err_style,
            priority: s.priority,
        })
        .collect()
}

/// Convert a full [`RenderOutput`] into the ratatui-typed pair
/// `(spans, signs)` ready for installation into an editor slot.
///
/// Returns the converted span table and the [`hjkl_buffer_tui::Sign`] vec.
/// The order of operations matches the install path in `syntax_glue.rs`.
///
/// # Examples
///
/// ```rust
/// use hjkl_syntax::{RenderOutput, PerfBreakdown};
/// use hjkl_syntax_tui::render_output_to_tui;
///
/// let out = RenderOutput::new(
///     0,
///     vec![vec![]],
///     vec![],
///     (1, 0, 30),
///     PerfBreakdown::new(),
/// );
/// let (spans, signs) = render_output_to_tui(&out);
/// assert_eq!(spans.len(), 1);
/// assert!(signs.is_empty());
/// ```
#[allow(clippy::type_complexity)]
pub fn render_output_to_tui(out: &RenderOutput) -> (Vec<Vec<(usize, usize, Style)>>, Vec<Sign>) {
    let spans = to_ratatui_spans(&out.spans);
    let signs = diag_signs_to_buffer_signs(&out.signs);
    (spans, signs)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use hjkl_syntax::{
        Color as ThemeColor, DiagSign, Modifiers, PerfBreakdown, RenderOutput, StyleSpec,
    };
    use ratatui::style::Modifier;

    fn red_spec() -> StyleSpec {
        StyleSpec {
            fg: Some(ThemeColor {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            }),
            bg: None,
            modifiers: Modifiers::default(),
        }
    }

    fn bold_spec() -> StyleSpec {
        StyleSpec {
            fg: None,
            bg: None,
            modifiers: Modifiers {
                bold: true,
                ..Default::default()
            },
        }
    }

    // --- to_ratatui_spans ---

    #[test]
    fn to_ratatui_spans_empty_input() {
        let result = to_ratatui_spans(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn to_ratatui_spans_empty_rows() {
        let input: Vec<Vec<(usize, usize, StyleSpec)>> = vec![vec![], vec![]];
        let result = to_ratatui_spans(&input);
        assert_eq!(result.len(), 2);
        assert!(result[0].is_empty());
        assert!(result[1].is_empty());
    }

    #[test]
    fn to_ratatui_spans_preserves_byte_offsets() {
        let input = vec![vec![(3, 8, StyleSpec::default())]];
        let result = to_ratatui_spans(&input);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0][0].0, 3);
        assert_eq!(result[0][0].1, 8);
    }

    #[test]
    fn to_ratatui_spans_converts_fg_colour() {
        let input = vec![vec![(0, 5, red_spec())]];
        let result = to_ratatui_spans(&input);
        let style = result[0][0].2;
        assert_eq!(style.fg, Some(ratatui::style::Color::Rgb(255, 0, 0)));
    }

    #[test]
    fn to_ratatui_spans_converts_bold_modifier() {
        let input = vec![vec![(0, 3, bold_spec())]];
        let result = to_ratatui_spans(&input);
        let style = result[0][0].2;
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }

    // --- spec_to_ratatui ---

    #[test]
    fn spec_to_ratatui_default_is_plain() {
        let style = spec_to_ratatui(&StyleSpec::default());
        assert_eq!(style.fg, None);
        assert_eq!(style.bg, None);
    }

    // --- diag_signs_to_buffer_signs ---

    #[test]
    fn diag_signs_empty() {
        let result = diag_signs_to_buffer_signs(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn diag_signs_row_and_ch_preserved() {
        let diags = vec![DiagSign::new(5, 'E', 100), DiagSign::new(12, 'W', 50)];
        let result = diag_signs_to_buffer_signs(&diags);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].row, 5);
        assert_eq!(result[0].ch, 'E');
        assert_eq!(result[0].priority, 100);
        assert_eq!(result[1].row, 12);
        assert_eq!(result[1].ch, 'W');
        assert_eq!(result[1].priority, 50);
    }

    #[test]
    fn diag_signs_use_red_style() {
        let diags = vec![DiagSign::new(0, 'E', 100)];
        let result = diag_signs_to_buffer_signs(&diags);
        assert_eq!(result[0].style.fg, Some(ratatui::style::Color::Red));
    }

    // --- render_output_to_tui ---

    #[test]
    fn render_output_to_tui_empty() {
        let out = RenderOutput::new(0, vec![], vec![], (0, 0, 10), PerfBreakdown::new());
        let (spans, signs) = render_output_to_tui(&out);
        assert!(spans.is_empty());
        assert!(signs.is_empty());
    }

    #[test]
    fn render_output_to_tui_routes_spans_and_signs() {
        let out = RenderOutput::new(
            1,
            vec![vec![(0, 4, red_spec())]],
            vec![DiagSign::new(0, 'E', 100)],
            (3, 0, 10),
            PerfBreakdown::new(),
        );
        let (spans, signs) = render_output_to_tui(&out);
        assert_eq!(spans.len(), 1);
        assert_eq!(
            spans[0][0].2.fg,
            Some(ratatui::style::Color::Rgb(255, 0, 0))
        );
        assert_eq!(signs.len(), 1);
        assert_eq!(signs[0].row, 0);
        assert_eq!(signs[0].style.fg, Some(ratatui::style::Color::Red));
    }
}
