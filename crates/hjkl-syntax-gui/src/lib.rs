//! Floem/cosmic-text adapter for `hjkl-syntax`.
//!
//! Converts [`hjkl_syntax::RenderOutput`] (renderer-agnostic
//! [`hjkl_theme::StyleSpec`] spans) into cosmic-text styling attributes and
//! routes [`hjkl_syntax::DiagSign`]s into an owned, renderer-agnostic
//! [`GuiSign`] for gutter rendering.
//!
//! Mirrors the structure of the ratatui adapter `hjkl-syntax-tui`: one pure,
//! unit-tested conversion function per span/sign type, with no floem `View`
//! or window state anywhere in this crate.
//!
//! # Owned-type choice
//!
//! `cosmic_text::Attrs<'a>` is borrow-tied (it holds a `Family<'a>`), which
//! makes it awkward to return from a plain conversion function. Rather than
//! fight that lifetime — or take on `cosmic_text::AttrsOwned`'s font-family
//! plumbing, which this crate has no opinion about — conversion targets a
//! small owned [`GuiStyle`] struct that carries only the fields
//! [`hjkl_theme::StyleSpec`] can produce (`fg`, `bg`, `weight`, `style`,
//! `underline`). Callers that need a real `cosmic_text::Attrs` /
//! `floem::text::Attrs` can build one from `GuiStyle`'s fields directly at
//! the call site, once a font family is known.
//!
//! [`hjkl_theme::Modifiers::reverse`] and
//! [`hjkl_theme::Modifiers::strikethrough`] have no cosmic-text attribute
//! equivalent (cosmic-text does not render reverse-video or strikethrough
//! itself) and are dropped during conversion, mirroring how the ratatui
//! adapter drops the alpha channel when converting `Color`.
//!
//! # Quick-start
//!
//! ```rust
//! use hjkl_syntax::{DiagSign, RenderOutput, PerfBreakdown};
//! use hjkl_syntax_gui::{to_gui_spans, diag_signs_to_gui_signs};
//!
//! // An empty output with no spans and no signs.
//! let out = RenderOutput::new(0, vec![], vec![], (0, 0, 10), PerfBreakdown::new());
//! let rows = to_gui_spans(&out.spans);
//! assert!(rows.is_empty());
//!
//! let signs = diag_signs_to_gui_signs(&out.signs);
//! assert!(signs.is_empty());
//! ```

use cosmic_text::{Color as CosmicColor, Style as CosmicStyle, Weight as CosmicWeight};
use hjkl_syntax::{Color as ThemeColor, DiagSign, RenderOutput, StyleSpec};

// ---------------------------------------------------------------------------
// GuiStyle — owned, cosmic-text-flavoured style
// ---------------------------------------------------------------------------

/// Owned, renderer-facing style produced from a [`StyleSpec`].
///
/// Unlike `cosmic_text::Attrs<'a>` this carries no lifetime, so it can be
/// returned freely from conversion functions and stored in span tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuiStyle {
    /// Foreground colour, when the span sets one.
    pub fg: Option<CosmicColor>,
    /// Background colour, when the span sets one.
    pub bg: Option<CosmicColor>,
    /// Font weight — [`CosmicWeight::BOLD`] when [`hjkl_theme::Modifiers::bold`]
    /// is set, [`CosmicWeight::NORMAL`] otherwise.
    pub weight: CosmicWeight,
    /// Font style — [`CosmicStyle::Italic`] when
    /// [`hjkl_theme::Modifiers::italic`] is set, [`CosmicStyle::Normal`]
    /// otherwise.
    pub style: CosmicStyle,
    /// Whether the span should be underlined. cosmic-text's `Attrs` has no
    /// underline field, so callers rendering an underline must draw it
    /// themselves using this flag.
    pub underline: bool,
}

impl Default for GuiStyle {
    fn default() -> Self {
        Self {
            fg: None,
            bg: None,
            weight: CosmicWeight::NORMAL,
            style: CosmicStyle::Normal,
            underline: false,
        }
    }
}

/// Convert a [`hjkl_theme::Color`] to a `cosmic_text::Color`.
///
/// Both are RGBA; no data is dropped (unlike the ratatui adapter, which
/// drops alpha because `ratatui::style::Color` has no alpha channel).
fn color_to_cosmic(c: ThemeColor) -> CosmicColor {
    CosmicColor::rgba(c.r, c.g, c.b, c.a)
}

// ---------------------------------------------------------------------------
// Public conversion functions
// ---------------------------------------------------------------------------

/// Convert a single [`StyleSpec`] to a [`GuiStyle`].
///
/// # Examples
///
/// ```rust
/// use hjkl_syntax::StyleSpec;
/// use hjkl_syntax_gui::style_spec_to_gui;
///
/// let style = style_spec_to_gui(&StyleSpec::default());
/// assert_eq!(style.fg, None);
/// assert_eq!(style.bg, None);
/// assert!(!style.underline);
/// ```
pub fn style_spec_to_gui(spec: &StyleSpec) -> GuiStyle {
    GuiStyle {
        fg: spec.fg.map(color_to_cosmic),
        bg: spec.bg.map(color_to_cosmic),
        weight: if spec.modifiers.bold {
            CosmicWeight::BOLD
        } else {
            CosmicWeight::NORMAL
        },
        style: if spec.modifiers.italic {
            CosmicStyle::Italic
        } else {
            CosmicStyle::Normal
        },
        underline: spec.modifiers.underline,
    }
}

/// Convert a per-row [`StyleSpec`] span table into the equivalent
/// [`GuiStyle`]-typed table.
///
/// Each `(byte_start, byte_end, StyleSpec)` triple becomes
/// `(byte_start, byte_end, GuiStyle)`. Mirrors
/// `hjkl_syntax_tui::to_ratatui_spans`'s per-row shape.
///
/// # Examples
///
/// ```rust
/// use hjkl_syntax::StyleSpec;
/// use hjkl_syntax_gui::row_spans_to_gui;
///
/// let row: Vec<(usize, usize, StyleSpec)> = vec![(0, 5, StyleSpec::default())];
/// let result = row_spans_to_gui(&row);
/// assert_eq!(result.len(), 1);
/// assert_eq!(result[0].0, 0);
/// assert_eq!(result[0].1, 5);
/// ```
pub fn row_spans_to_gui(row: &[(usize, usize, StyleSpec)]) -> Vec<(usize, usize, GuiStyle)> {
    row.iter()
        .map(|(start, end, spec)| (*start, *end, style_spec_to_gui(spec)))
        .collect()
}

/// Convert a full per-row [`StyleSpec`] span table (as produced by
/// [`hjkl_syntax::RenderOutput::spans`]) into the equivalent
/// [`GuiStyle`]-typed table.
///
/// # Examples
///
/// ```rust
/// use hjkl_syntax::StyleSpec;
/// use hjkl_syntax_gui::to_gui_spans;
///
/// let spans: Vec<Vec<(usize, usize, StyleSpec)>> = vec![
///     vec![(0, 5, StyleSpec::default())],
///     vec![],
/// ];
/// let rows = to_gui_spans(&spans);
/// assert_eq!(rows.len(), 2);
/// assert_eq!(rows[0].len(), 1);
/// assert!(rows[1].is_empty());
/// ```
pub fn to_gui_spans(
    spans: &[Vec<(usize, usize, StyleSpec)>],
) -> Vec<Vec<(usize, usize, GuiStyle)>> {
    spans.iter().map(|row| row_spans_to_gui(row)).collect()
}

// ---------------------------------------------------------------------------
// GuiSign — owned gutter mark
// ---------------------------------------------------------------------------

/// Owned gutter mark produced from a [`DiagSign`].
///
/// This is a renderer-agnostic stand-in for whatever floem gutter-mark type
/// a later slice introduces (the floem editor gutter is not wired up yet —
/// see issue #150 slice 1). Callers wire `row`/`glyph`/`priority`/`color`
/// into their own gutter widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuiSign {
    /// Document row (0-indexed).
    pub row: usize,
    /// Gutter character (e.g. `'E'` for a syntax error).
    pub glyph: char,
    /// Gutter priority — higher wins when multiple signs land on the same row.
    pub priority: u8,
    /// Colour to render the glyph with.
    pub color: CosmicColor,
}

/// Convert [`DiagSign`]s (renderer-agnostic) into [`GuiSign`]s using the
/// canonical error colour (red foreground), mirroring
/// `hjkl_syntax_tui::diag_signs_to_buffer_signs`.
///
/// Higher-priority signs take precedence when multiple signs land on the
/// same gutter row. The priority from [`DiagSign::priority`] is preserved.
///
/// # Examples
///
/// ```rust
/// use hjkl_syntax::DiagSign;
/// use hjkl_syntax_gui::diag_signs_to_gui_signs;
///
/// let diags = vec![DiagSign::new(3, 'E', 100)];
/// let signs = diag_signs_to_gui_signs(&diags);
/// assert_eq!(signs.len(), 1);
/// assert_eq!(signs[0].row, 3);
/// assert_eq!(signs[0].glyph, 'E');
/// assert_eq!(signs[0].priority, 100);
/// ```
pub fn diag_signs_to_gui_signs(signs: &[DiagSign]) -> Vec<GuiSign> {
    let err_color = CosmicColor::rgb(255, 0, 0);
    signs
        .iter()
        .map(|s| GuiSign {
            row: s.row,
            glyph: s.ch,
            priority: s.priority,
            color: err_color,
        })
        .collect()
}

/// Convert a full [`RenderOutput`] into the cosmic-text-typed pair
/// `(spans, signs)` ready for installation into a floem editor slot.
///
/// # Examples
///
/// ```rust
/// use hjkl_syntax::{RenderOutput, PerfBreakdown};
/// use hjkl_syntax_gui::render_output_to_gui;
///
/// let out = RenderOutput::new(
///     0,
///     vec![vec![]],
///     vec![],
///     (1, 0, 30),
///     PerfBreakdown::new(),
/// );
/// let (spans, signs) = render_output_to_gui(&out);
/// assert_eq!(spans.len(), 1);
/// assert!(signs.is_empty());
/// ```
#[allow(clippy::type_complexity)]
pub fn render_output_to_gui(
    out: &RenderOutput,
) -> (Vec<Vec<(usize, usize, GuiStyle)>>, Vec<GuiSign>) {
    let spans = to_gui_spans(&out.spans);
    let signs = diag_signs_to_gui_signs(&out.signs);
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

    fn italic_spec() -> StyleSpec {
        StyleSpec {
            fg: None,
            bg: None,
            modifiers: Modifiers {
                italic: true,
                ..Default::default()
            },
        }
    }

    fn underline_spec() -> StyleSpec {
        StyleSpec {
            fg: None,
            bg: None,
            modifiers: Modifiers {
                underline: true,
                ..Default::default()
            },
        }
    }

    fn bg_spec() -> StyleSpec {
        StyleSpec {
            fg: None,
            bg: Some(ThemeColor {
                r: 10,
                g: 20,
                b: 30,
                a: 255,
            }),
            modifiers: Modifiers::default(),
        }
    }

    // --- style_spec_to_gui ---

    #[test]
    fn style_spec_to_gui_default_is_plain() {
        let style = style_spec_to_gui(&StyleSpec::default());
        assert_eq!(style.fg, None);
        assert_eq!(style.bg, None);
        assert_eq!(style.weight, CosmicWeight::NORMAL);
        assert_eq!(style.style, CosmicStyle::Normal);
        assert!(!style.underline);
    }

    #[test]
    fn style_spec_to_gui_converts_fg_colour() {
        let style = style_spec_to_gui(&red_spec());
        assert_eq!(style.fg, Some(CosmicColor::rgba(255, 0, 0, 255)));
    }

    #[test]
    fn style_spec_to_gui_converts_bg_colour() {
        let style = style_spec_to_gui(&bg_spec());
        assert_eq!(style.bg, Some(CosmicColor::rgba(10, 20, 30, 255)));
    }

    #[test]
    fn style_spec_to_gui_converts_bold_modifier() {
        let style = style_spec_to_gui(&bold_spec());
        assert_eq!(style.weight, CosmicWeight::BOLD);
    }

    #[test]
    fn style_spec_to_gui_converts_italic_modifier() {
        let style = style_spec_to_gui(&italic_spec());
        assert_eq!(style.style, CosmicStyle::Italic);
    }

    #[test]
    fn style_spec_to_gui_converts_underline_modifier() {
        let style = style_spec_to_gui(&underline_spec());
        assert!(style.underline);
    }

    #[test]
    fn style_spec_to_gui_no_bold_is_normal_weight() {
        let style = style_spec_to_gui(&StyleSpec::default());
        assert_eq!(style.weight, CosmicWeight::NORMAL);
    }

    // --- row_spans_to_gui ---

    #[test]
    fn row_spans_to_gui_empty_row() {
        let row: Vec<(usize, usize, StyleSpec)> = vec![];
        let result = row_spans_to_gui(&row);
        assert!(result.is_empty());
    }

    #[test]
    fn row_spans_to_gui_preserves_byte_offsets() {
        let row = vec![(3, 8, StyleSpec::default())];
        let result = row_spans_to_gui(&row);
        assert_eq!(result[0].0, 3);
        assert_eq!(result[0].1, 8);
    }

    #[test]
    fn row_spans_to_gui_multi_span_row() {
        let row = vec![
            (0, 3, bold_spec()),
            (3, 6, red_spec()),
            (6, 9, italic_spec()),
        ];
        let result = row_spans_to_gui(&row);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].2.weight, CosmicWeight::BOLD);
        assert_eq!(result[1].2.fg, Some(CosmicColor::rgba(255, 0, 0, 255)));
        assert_eq!(result[2].2.style, CosmicStyle::Italic);
    }

    // --- to_gui_spans ---

    #[test]
    fn to_gui_spans_empty_input() {
        let result = to_gui_spans(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn to_gui_spans_empty_rows() {
        let input: Vec<Vec<(usize, usize, StyleSpec)>> = vec![vec![], vec![]];
        let result = to_gui_spans(&input);
        assert_eq!(result.len(), 2);
        assert!(result[0].is_empty());
        assert!(result[1].is_empty());
    }

    #[test]
    fn to_gui_spans_converts_fg_colour() {
        let input = vec![vec![(0, 5, red_spec())]];
        let result = to_gui_spans(&input);
        let style = result[0][0].2;
        assert_eq!(style.fg, Some(CosmicColor::rgba(255, 0, 0, 255)));
    }

    // --- diag_signs_to_gui_signs ---

    #[test]
    fn diag_signs_empty() {
        let result = diag_signs_to_gui_signs(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn diag_signs_row_and_glyph_preserved() {
        let diags = vec![DiagSign::new(5, 'E', 100), DiagSign::new(12, 'W', 50)];
        let result = diag_signs_to_gui_signs(&diags);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].row, 5);
        assert_eq!(result[0].glyph, 'E');
        assert_eq!(result[0].priority, 100);
        assert_eq!(result[1].row, 12);
        assert_eq!(result[1].glyph, 'W');
        assert_eq!(result[1].priority, 50);
    }

    #[test]
    fn diag_signs_use_red_color() {
        let diags = vec![DiagSign::new(0, 'E', 100)];
        let result = diag_signs_to_gui_signs(&diags);
        assert_eq!(result[0].color, CosmicColor::rgb(255, 0, 0));
    }

    // --- render_output_to_gui ---

    #[test]
    fn render_output_to_gui_empty() {
        let out = RenderOutput::new(0, vec![], vec![], (0, 0, 10), PerfBreakdown::new());
        let (spans, signs) = render_output_to_gui(&out);
        assert!(spans.is_empty());
        assert!(signs.is_empty());
    }

    #[test]
    fn render_output_to_gui_routes_spans_and_signs() {
        let out = RenderOutput::new(
            1,
            vec![vec![(0, 4, red_spec())]],
            vec![DiagSign::new(0, 'E', 100)],
            (3, 0, 10),
            PerfBreakdown::new(),
        );
        let (spans, signs) = render_output_to_gui(&out);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0][0].2.fg, Some(CosmicColor::rgba(255, 0, 0, 255)));
        assert_eq!(signs.len(), 1);
        assert_eq!(signs[0].row, 0);
        assert_eq!(signs[0].color, CosmicColor::rgb(255, 0, 0));
    }
}
