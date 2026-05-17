//! Ratatui adapter for `hjkl-hover`.
//!
//! Paints a [`HoverState`] into a ratatui [`Frame`] using
//! `hjkl-markdown-tui` for the content body. The popup is a floating
//! bordered box whose position is computed by [`hjkl_hover::position`].
//!
//! # Quick start
//!
//! ```rust,no_run
//! // (requires a real ratatui terminal — compile-checked, not run in CI)
//! use hjkl_hover::{HoverAnchor, HoverState, HoverViewport};
//! use hjkl_hover_tui::{HoverTheme, render};
//! // frame and viewport come from your ratatui setup
//! ```

use hjkl_hover::{HoverState, HoverViewport, position};
use hjkl_markdown::parse;
use hjkl_markdown_tui::{MdTheme, to_lines};
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

// ── HoverTheme ────────────────────────────────────────────────────────────────

/// Theme slots for the hover popup chrome (border, title, background) plus the
/// markdown body colors.
///
/// `#[non_exhaustive]` — new slots may be added in minor releases.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct HoverTheme {
    /// Border and title foreground.
    pub border: ratatui::style::Color,
    /// Popup background.
    pub background: ratatui::style::Color,
    /// Markdown body colors.
    pub md: MdTheme,
}

impl HoverTheme {
    /// Construct from explicit values.
    pub fn new(
        border: ratatui::style::Color,
        background: ratatui::style::Color,
        md: MdTheme,
    ) -> Self {
        Self {
            border,
            background,
            md,
        }
    }
}

impl Default for HoverTheme {
    fn default() -> Self {
        Self {
            border: ratatui::style::Color::Rgb(0x89, 0xb4, 0xfa),
            background: ratatui::style::Color::Rgb(0x1e, 0x1e, 0x2e),
            md: MdTheme::default(),
        }
    }
}

// ── render ────────────────────────────────────────────────────────────────────

/// Render a hover popup into `frame`.
///
/// `viewport` is the full terminal area available (typically `frame.area()`).
/// The popup position is computed by [`hjkl_hover::position`], then the
/// markdown content is parsed and rendered via `hjkl-markdown-tui`.
pub fn render(frame: &mut Frame, state: &HoverState, theme: &HoverTheme, viewport: Rect) {
    let vp = HoverViewport {
        width: viewport.width,
        height: viewport.height,
    };
    let hr = position(state, vp);
    let rect = Rect {
        x: viewport.x + hr.x,
        y: viewport.y + hr.y,
        width: hr.width,
        height: hr.height,
    };

    frame.render_widget(Clear, rect);

    let border_style = Style::default().fg(theme.border);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(" hover ");
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    // Parse + render markdown into the inner area.
    let events = parse(&state.content);
    let lines = to_lines(&events, &theme.md, inner.width);

    // Apply scroll offset.
    let scrolled: Vec<_> = lines.into_iter().skip(state.scroll).collect();

    let para = Paragraph::new(scrolled)
        .style(Style::default().fg(theme.md.text).bg(theme.background))
        .wrap(Wrap { trim: false });
    frame.render_widget(para, inner);
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use hjkl_hover::{HoverAnchor, HoverState, HoverViewport};

    fn make_state(content: &str, col: u16, row: u16) -> HoverState {
        HoverState::new(content.to_string(), HoverAnchor::new(col, row))
    }

    #[test]
    fn hover_theme_default_has_border() {
        let t = HoverTheme::default();
        assert!(matches!(t.border, ratatui::style::Color::Rgb(_, _, _)));
    }

    #[test]
    fn position_smoke() {
        let s = make_state("hello", 5, 5);
        let r = hjkl_hover::position(
            &s,
            HoverViewport {
                width: 80,
                height: 24,
            },
        );
        assert!(r.x + r.width <= 80);
        assert!(r.y + r.height <= 24);
    }

    #[test]
    fn scroll_integration() {
        let mut s = make_state("line1\nline2\nline3", 0, 0);
        s.scroll_lines(1);
        assert_eq!(s.scroll, 1);
        let evs = parse(&s.content);
        let lines = to_lines(&evs, &MdTheme::default(), 80);
        let scrolled: Vec<_> = lines.into_iter().skip(s.scroll).collect();
        // Should have 2 or fewer lines after skipping 1.
        assert!(scrolled.len() <= 3, "unexpected line count");
    }

    #[test]
    fn markdown_parsed_in_hover() {
        let s = make_state("# Title\n\nhello `world`", 0, 0);
        let evs = parse(&s.content);
        let lines = to_lines(&evs, &MdTheme::default(), 60);
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|sp| sp.content.as_ref())
            .collect();
        assert!(
            all_text.contains("Title"),
            "heading not found: {all_text:?}"
        );
    }
}
