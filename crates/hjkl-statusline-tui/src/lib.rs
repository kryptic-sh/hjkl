//! Ratatui adapter for `hjkl-statusline`.
//!
//! Converts a [`Bar`] to a `ratatui::text::Line` and paints it via
//! `Paragraph` into the supplied `Rect`.
//!
//! # Usage
//!
//! ```no_run
//! // let bar: hjkl_statusline::Bar = build_normal_statusline(&app_state);
//! // hjkl_statusline_tui::render(frame, &bar, area);
//! ```

#![forbid(unsafe_code)]

use hjkl_statusline::{Bar, Color, Modifiers, Segment, Style};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color as RColor, Modifier as RMod, Style as RStyle},
    text::{Line, Span},
    widgets::Paragraph,
};

/// Convert an agnostic [`Color`] to a ratatui [`RColor`].
#[inline]
pub fn to_ratatui_color(c: Color) -> RColor {
    RColor::Rgb(c.r, c.g, c.b)
}

/// Convert agnostic [`Modifiers`] to ratatui [`RMod`].
#[inline]
pub fn to_ratatui_modifier(m: Modifiers) -> RMod {
    let mut out = RMod::empty();
    if m.bold {
        out |= RMod::BOLD;
    }
    if m.italic {
        out |= RMod::ITALIC;
    }
    out
}

/// Convert an agnostic [`Style`] to a ratatui [`RStyle`].
#[inline]
pub fn to_ratatui_style(s: Style) -> RStyle {
    let mut out = RStyle::default();
    if let Some(fg) = s.fg {
        out = out.fg(to_ratatui_color(fg));
    }
    if let Some(bg) = s.bg {
        out = out.bg(to_ratatui_color(bg));
    }
    out = out.add_modifier(to_ratatui_modifier(s.modifiers));
    out
}

/// Convert a [`Segment`] to a ratatui [`Span`].
fn segment_to_span(seg: &Segment) -> Span<'static> {
    match seg {
        Segment::Text { content, style } => Span::styled(content.clone(), to_ratatui_style(*style)),
        // Future variants (Icon, Separator, PowerlineChevron, …) fall back to empty.
        _ => Span::raw(String::new()),
    }
}

/// Render a [`Bar`] into `area` using the supplied ratatui [`Frame`].
///
/// Calls [`Bar::layout`] with `area.width`, converts each segment to a
/// `Span`, and renders via `Paragraph`. The bar is always exactly one row.
pub fn render(frame: &mut Frame, bar: &Bar, area: Rect) {
    let segments = bar.layout(area.width);
    let spans: Vec<Span<'static>> = segments.iter().map(segment_to_span).collect();
    let line = Line::from(spans);
    frame.render_widget(Paragraph::new(line), area);
}

/// Convert a [`Bar`] to a ratatui [`Line`] at the given `width`.
///
/// Useful when the caller already manages its own widget construction.
pub fn to_line(bar: &Bar, width: u16) -> Line<'static> {
    let segments = bar.layout(width);
    let spans: Vec<Span<'static>> = segments.iter().map(segment_to_span).collect();
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hjkl_statusline::{
        Bar, Color, Segment, StatusTheme, Style, StyleExt, cursor_segment, mode_segment,
    };
    use ratatui::{Terminal, backend::TestBackend};

    /// Build a test theme via mutation — `StatusTheme` is `#[non_exhaustive]`
    /// so struct literals are not permitted outside the defining crate.
    fn test_theme() -> StatusTheme {
        let mut t = StatusTheme::new(Color::rgb(0x2a, 0x32, 0x40), Color::rgb(0xe5, 0xe9, 0xf0));
        t.fill_bg = Color::rgb(0x1e, 0x22, 0x2a);
        t.mode_normal_bg = Color::rgb(0x5e, 0x81, 0xac);
        t.mode_normal_fg = Color::rgb(0x2e, 0x34, 0x40);
        t.mode_insert_bg = Color::rgb(0x7e, 0xe7, 0x87);
        t.mode_insert_fg = Color::rgb(0x2e, 0x34, 0x40);
        t.mode_visual_bg = Color::rgb(0xd0, 0x8e, 0x4b);
        t.mode_visual_fg = Color::rgb(0x2e, 0x34, 0x40);
        t.dirty_fg = Color::rgb(0xeb, 0xcb, 0x8b);
        t.readonly_fg = Color::rgb(0xbf, 0x61, 0x6a);
        t.new_file_fg = Color::rgb(0xa3, 0xbe, 0x8c);
        t.recording_bg = Color::rgb(0xbf, 0x61, 0x6a);
        t.recording_fg = Color::rgb(0x2e, 0x34, 0x40);
        t
    }

    #[test]
    fn render_emits_one_line_widget() {
        let theme = test_theme();
        let width: u16 = 40;
        let height: u16 = 1;

        let mut bar = Bar {
            fill_style: Style::default_style().bg(theme.fill_bg).fg(theme.fg),
            ..Default::default()
        };
        bar.left.push(mode_segment("NORMAL", &theme));
        bar.left.push(Segment::Text {
            content: " [No Name] ".to_string(),
            style: Style::default_style().bg(theme.bg).fg(theme.fg),
        });
        bar.right.push(cursor_segment(0, 0, &theme));

        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| {
                let area = Rect::new(0, 0, width, height);
                render(frame, &bar, area);
            })
            .expect("draw");

        let buf = terminal.backend().buffer().clone();
        // Collect all rendered chars.
        let rendered: String = (0..width)
            .map(|x| {
                buf.cell((x, 0))
                    .map(|c| c.symbol().to_string())
                    .unwrap_or_default()
            })
            .collect();

        assert!(
            rendered.contains("NORMAL"),
            "mode label must appear: {rendered:?}"
        );
        assert!(
            rendered.contains("[No Name]"),
            "filename must appear: {rendered:?}"
        );
        assert!(
            rendered.contains("1:1"),
            "cursor position must appear: {rendered:?}"
        );
    }

    #[test]
    fn to_line_width_matches() {
        let theme = test_theme();
        let width: u16 = 50;
        let mut bar = Bar {
            fill_style: Style::default_style().bg(theme.fill_bg).fg(theme.fg),
            ..Default::default()
        };
        bar.left.push(mode_segment("INSERT", &theme));
        bar.right.push(cursor_segment(5, 3, &theme));

        let line = to_line(&bar, width);
        let total: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
        assert_eq!(total, width as usize, "line width must equal bar width");
    }

    #[test]
    fn to_ratatui_color_roundtrip() {
        let c = Color::rgb(0x5e, 0x81, 0xac);
        let rc = to_ratatui_color(c);
        assert_eq!(rc, RColor::Rgb(0x5e, 0x81, 0xac));
    }

    #[test]
    fn to_ratatui_style_bold_and_italic() {
        let s = Style::default_style()
            .fg(Color::rgb(0xff, 0x00, 0x00))
            .bold()
            .italic();
        let rs = to_ratatui_style(s);
        assert!(rs.add_modifier.contains(RMod::BOLD));
        assert!(rs.add_modifier.contains(RMod::ITALIC));
        assert_eq!(rs.fg, Some(RColor::Rgb(0xff, 0x00, 0x00)));
    }
}
