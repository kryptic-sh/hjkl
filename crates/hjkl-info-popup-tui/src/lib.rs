//! Ratatui adapter for `hjkl-info-popup`.
//!
//! Renders an [`InfoPopup`] into a ratatui [`Frame`] as a centered bordered
//! overlay.  Plain-text content is rendered as-is; markdown content
//! (`ContentKind::Markdown`) is parsed and rendered via `hjkl-markdown-tui`.
//! The popup chrome (border, title) is controlled via [`InfoPopupTheme`].
//!
//! # Quick start
//!
//! ```rust,no_run
//! // (requires a real ratatui terminal — compile-checked, not run in CI)
//! use hjkl_info_popup::InfoPopup;
//! use hjkl_info_popup_tui::{InfoPopupTheme, render};
//! // frame and buf_area come from your ratatui setup
//! ```

use hjkl_info_popup::{ContentKind, InfoPopup, InfoViewport, geometry};
use hjkl_markdown::parse;
use hjkl_markdown_tui::{MdTheme, to_lines};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    widgets::{Block, Borders, Clear, Paragraph},
};

// ── InfoPopupTheme ────────────────────────────────────────────────────────────

/// Theme slots for the info popup chrome.
///
/// `#[non_exhaustive]` — new slots may be added in minor releases.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct InfoPopupTheme {
    /// Border and title foreground colour.
    pub border: Color,
    /// Markdown body colours (used when `ContentKind::Markdown`).
    pub md: MdTheme,
}

impl InfoPopupTheme {
    /// Construct from an explicit border colour with default markdown theme.
    pub fn new(border: Color) -> Self {
        Self {
            border,
            md: MdTheme::default(),
        }
    }

    /// Construct with explicit border and markdown theme.
    pub fn with_md(border: Color, md: MdTheme) -> Self {
        Self { border, md }
    }
}

impl Default for InfoPopupTheme {
    fn default() -> Self {
        Self {
            // Catppuccin Mocha blue — matches the default hover popup border.
            border: Color::Rgb(0x89, 0xb4, 0xfa),
            md: MdTheme::default(),
        }
    }
}

// ── render ────────────────────────────────────────────────────────────────────

/// Render an [`InfoPopup`] into `frame` positioned over `buf_area`.
///
/// `buf_area` is the full buffer pane rect (not the whole terminal).  The popup
/// geometry is computed by [`hjkl_info_popup::geometry`] and applied relative to
/// `buf_area.x`/`buf_area.y`.
///
/// - `ContentKind::Plain` — content rendered as a plain `Paragraph`.
/// - `ContentKind::Markdown` — content parsed with `hjkl-markdown` and
///   converted to styled lines via `hjkl-markdown-tui`.
pub fn render(frame: &mut Frame, popup: &InfoPopup, theme: &InfoPopupTheme, buf_area: Rect) {
    let vp = InfoViewport::new(buf_area.width, buf_area.height);
    let ir = geometry(popup, vp);
    let area = Rect {
        x: buf_area.x + ir.x,
        y: buf_area.y + ir.y,
        width: ir.width,
        height: ir.height,
    };

    frame.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .title(popup.title.clone());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    match popup.kind {
        ContentKind::Plain => {
            let para = Paragraph::new(popup.content.clone());
            frame.render_widget(para, inner);
        }
        ContentKind::Markdown => {
            let events = parse(&popup.content);
            let lines = to_lines(&events, &theme.md, inner.width);
            let para = Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false });
            frame.render_widget(para, inner);
        }
        // Future ContentKind variants: fall back to plain rendering.
        _ => {
            let para = Paragraph::new(popup.content.clone());
            frame.render_widget(para, inner);
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use hjkl_info_popup::{ContentKind, InfoPopup, InfoViewport, geometry};

    #[test]
    fn theme_default_has_rgb_border() {
        let t = InfoPopupTheme::default();
        assert!(matches!(t.border, Color::Rgb(_, _, _)));
    }

    #[test]
    fn theme_new_stores_border() {
        let t = InfoPopupTheme::new(Color::Rgb(0xff, 0x00, 0x00));
        assert_eq!(t.border, Color::Rgb(0xff, 0x00, 0x00));
    }

    #[test]
    fn geometry_inside_viewport() {
        let p = InfoPopup::new("reg", "hello\nworld");
        let vp = InfoViewport::new(80, 24);
        let r = geometry(&p, vp);
        assert!(r.x + r.width <= 80);
        assert!(r.y + r.height <= 24);
    }

    #[test]
    fn geometry_relative_offset() {
        let p = InfoPopup::new("marks", "a\nb");
        let vp = InfoViewport::new(80, 24);
        let ir = geometry(&p, vp);
        let buf_area = Rect {
            x: 5,
            y: 2,
            width: 80,
            height: 24,
        };
        let area_x = buf_area.x + ir.x;
        let area_y = buf_area.y + ir.y;
        assert!(area_x >= 5, "x must be at least buf_area.x");
        assert!(area_y >= 2, "y must be at least buf_area.y");
    }

    #[test]
    fn popup_title_matches_info_popup() {
        let p = InfoPopup::new("jumps", "content");
        assert_eq!(p.title, " jumps ");
    }

    #[test]
    fn popup_line_count() {
        let p = InfoPopup::new("changes", "line1\nline2\nline3\nline4");
        assert_eq!(p.line_count(), 4);
    }

    #[test]
    fn markdown_kind_parsed_by_hjkl_markdown() {
        let p = InfoPopup::markdown("hover", "# Title\n\nhello `world`");
        assert_eq!(p.kind, ContentKind::Markdown);
        // Verify the markdown parses to events that contain the heading text.
        let evs = parse(&p.content);
        let lines = to_lines(&evs, &MdTheme::default(), 60);
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|sp| sp.content.as_ref())
            .collect();
        assert!(
            all_text.contains("Title"),
            "heading not found in: {all_text:?}"
        );
    }
}
