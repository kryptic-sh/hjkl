//! Ratatui adapter for [`hjkl_tabs`].
//!
//! Renders a [`TabBar<Id>`] into a single terminal row with:
//!
//! - Active tab: bold, accent-background highlight.
//! - Dirty marker: `●` prefix on modified tabs.
//! - Separator: `│` between adjacent tabs.
//! - Overflow indicators: `<` when tabs are hidden to the left, `>` when
//!   hidden to the right.
//!
//! # Usage
//!
//! ```no_run
//! // let mut bar: hjkl_tabs::TabBar<u32> = hjkl_tabs::TabBar::new();
//! // bar.open(1, "main.rs".to_string());
//! // let theme = hjkl_tabs_tui::TabBarTheme::default();
//! // hjkl_tabs_tui::render(frame, area, &bar, &theme);
//! ```

#![forbid(unsafe_code)]

use hjkl_tabs::TabBar;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

/// Colour / style configuration for [`render`].
///
/// `#[non_exhaustive]` — new fields may be added in minor releases without
/// breaking existing construction sites (use [`TabBarTheme::new`] or
/// [`TabBarTheme::default`] + field mutation).
///
/// The default palette is a dark theme (catppuccin-inspired):
/// - `active_fg` / `active_bg`: white on blue.
/// - `inactive_fg`: dimmed text, transparent background.
/// - `sep_fg`: mid-grey separator.
/// - `overflow_fg`: accent cyan for `<` / `>` indicators.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct TabBarTheme {
    /// Foreground colour of the active (focused) tab.
    pub active_fg: Color,
    /// Background colour of the active tab.
    pub active_bg: Color,
    /// Foreground colour of inactive tabs.
    pub inactive_fg: Color,
    /// Background colour of inactive tabs (distinct from the bar background so
    /// inactive tabs read as raised chips rather than blending in).
    pub inactive_bg: Color,
    /// Foreground colour of the `│` tab separator.
    pub sep_fg: Color,
    /// Foreground colour of the `<` / `>` overflow indicators.
    pub overflow_fg: Color,
}

impl TabBarTheme {
    /// Build a theme from explicit colour values.
    ///
    /// ```
    /// use ratatui::style::Color;
    /// use hjkl_tabs_tui::TabBarTheme;
    ///
    /// let theme = TabBarTheme::new(
    ///     Color::White,
    ///     Color::Blue,
    ///     Color::DarkGray,
    ///     Color::Black,
    ///     Color::Gray,
    ///     Color::Cyan,
    /// );
    /// assert_eq!(theme.active_fg, Color::White);
    /// ```
    pub fn new(
        active_fg: Color,
        active_bg: Color,
        inactive_fg: Color,
        inactive_bg: Color,
        sep_fg: Color,
        overflow_fg: Color,
    ) -> Self {
        Self {
            active_fg,
            active_bg,
            inactive_fg,
            inactive_bg,
            sep_fg,
            overflow_fg,
        }
    }
}

impl Default for TabBarTheme {
    fn default() -> Self {
        Self {
            active_fg: Color::Rgb(0x2e, 0x34, 0x40),
            active_bg: Color::Rgb(0x5e, 0x81, 0xac),
            inactive_fg: Color::Rgb(0x81, 0x8a, 0x9a),
            inactive_bg: Color::Rgb(0x3b, 0x42, 0x52),
            sep_fg: Color::Rgb(0x4c, 0x56, 0x6a),
            overflow_fg: Color::Rgb(0x88, 0xc0, 0xd0),
        }
    }
}

/// Render a [`TabBar`] into a single terminal row.
///
/// The rendered line is: `[<] tab1 │ tab2 │ … [>]` where `<`/`>` appear
/// only when tabs are hidden on that side.  The active tab is rendered with
/// bold + `active_bg` background.  Dirty tabs display a `●` prefix.
///
/// `area.height` is ignored; only one row is painted regardless.
///
/// # Example
///
/// ```no_run
/// use hjkl_tabs::TabBar;
/// use hjkl_tabs_tui::{TabBarTheme, render};
///
/// // Typically called inside a `Terminal::draw` closure:
/// // render(frame, area, &bar, &TabBarTheme::default());
/// ```
pub fn render<Id: Eq + Clone>(
    frame: &mut Frame,
    area: Rect,
    bar: &TabBar<Id>,
    theme: &TabBarTheme,
) {
    let line = build_line(area.width, bar, theme);
    frame.render_widget(Paragraph::new(line), area);
}

/// Build the [`Line`] that [`render`] paints without requiring a [`Frame`].
///
/// Useful when the caller wants to embed the tab bar into a composite layout
/// or test the output without a terminal backend.
///
/// ```
/// use hjkl_tabs::TabBar;
/// use hjkl_tabs_tui::{TabBarTheme, build_line};
///
/// let mut bar: TabBar<u32> = TabBar::new();
/// bar.open(1, "a.rs".to_string());
/// bar.open(2, "b.rs".to_string());
/// let line = build_line(40, &bar, &TabBarTheme::default());
/// // The line must contain a span mentioning "a.rs" or "b.rs".
/// let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
/// assert!(text.contains("a.rs") || text.contains("b.rs"));
/// ```
pub fn build_line<Id: Eq + Clone>(
    width: u16,
    bar: &TabBar<Id>,
    theme: &TabBarTheme,
) -> Line<'static> {
    if bar.is_empty() {
        return Line::from(vec![]);
    }

    let active_style = Style::default()
        .fg(theme.active_fg)
        .bg(theme.active_bg)
        .add_modifier(Modifier::BOLD);
    let inactive_style = Style::default().fg(theme.inactive_fg).bg(theme.inactive_bg);
    let sep_style = Style::default().fg(theme.sep_fg);
    let overflow_style = Style::default()
        .fg(theme.overflow_fg)
        .add_modifier(Modifier::BOLD);

    let (visible_tabs, left_overflow, right_overflow) = bar.visible(width);

    let mut spans: Vec<Span<'static>> = Vec::new();

    if left_overflow {
        spans.push(Span::styled("<".to_string(), overflow_style));
    }

    let active_idx = bar.active_index();
    let tabs_slice_start = if left_overflow {
        // Find the index of the first visible tab in the full bar.
        visible_tabs
            .first()
            .and_then(|first| bar.tabs.iter().position(|t| std::ptr::eq(t, *first)))
            .unwrap_or(0)
    } else {
        0
    };

    for (i, tab) in visible_tabs.iter().enumerate() {
        if i > 0 || left_overflow {
            spans.push(Span::styled("│".to_string(), sep_style));
        }

        // Resolve the absolute index in `bar.tabs` to decide if this tab is active.
        let abs_idx = tabs_slice_start + i;
        let is_active = active_idx == Some(abs_idx);

        let label = format!(" {} ", tab.display_label());
        let style = if is_active {
            active_style
        } else {
            inactive_style
        };
        spans.push(Span::styled(label, style));
    }

    if right_overflow {
        spans.push(Span::styled(">".to_string(), overflow_style));
    }

    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hjkl_tabs::TabBar;
    use ratatui::{Terminal, backend::TestBackend};

    fn make_bar(ids_titles: &[(u32, &str)]) -> TabBar<u32> {
        let mut bar = TabBar::new();
        for &(id, title) in ids_titles {
            bar.open(id, title.to_string());
        }
        bar
    }

    fn rendered_text(width: u16, bar: &TabBar<u32>) -> String {
        let backend = TestBackend::new(width, 1);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| {
                let area = Rect::new(0, 0, width, 1);
                render(frame, area, bar, &TabBarTheme::default());
            })
            .expect("draw");
        let buf = terminal.backend().buffer().clone();
        (0..width)
            .map(|x| {
                buf.cell((x, 0))
                    .map(|c| c.symbol().to_string())
                    .unwrap_or_default()
            })
            .collect()
    }

    #[test]
    fn empty_bar_renders_nothing() {
        let bar: TabBar<u32> = TabBar::new();
        let line = build_line(80, &bar, &TabBarTheme::default());
        assert!(line.spans.is_empty());
    }

    #[test]
    fn single_tab_appears_in_output() {
        let bar = make_bar(&[(1, "main.rs")]);
        let text = rendered_text(40, &bar);
        assert!(text.contains("main.rs"), "output: {text:?}");
    }

    #[test]
    fn two_tabs_both_appear() {
        let bar = make_bar(&[(1, "a.rs"), (2, "b.rs")]);
        let text = rendered_text(40, &bar);
        assert!(text.contains("a.rs"), "output: {text:?}");
        assert!(text.contains("b.rs"), "output: {text:?}");
    }

    #[test]
    fn active_tab_is_bold_in_spans() {
        let mut bar = make_bar(&[(1, "active.rs"), (2, "other.rs")]);
        // Explicitly focus the first tab so "active.rs" is the active one.
        bar.focus(&1);
        let line = build_line(80, &bar, &TabBarTheme::default());
        // The active tab span should carry BOLD modifier.
        let has_bold = line.spans.iter().any(|s| {
            s.content.contains("active.rs") && s.style.add_modifier.contains(Modifier::BOLD)
        });
        assert!(has_bold, "active tab must be bold");
    }

    #[test]
    fn dirty_tab_shows_marker_in_spans() {
        let mut bar: TabBar<u32> = TabBar::new();
        bar.open(1, "dirty.rs".to_string());
        if let Some(t) = bar.active_mut() {
            t.dirty = true;
        }
        let line = build_line(80, &bar, &TabBarTheme::default());
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains('●'), "dirty marker must appear: {text:?}");
    }

    #[test]
    fn separator_appears_between_tabs() {
        let bar = make_bar(&[(1, "a"), (2, "b"), (3, "c")]);
        let line = build_line(80, &bar, &TabBarTheme::default());
        let sep_count = line
            .spans
            .iter()
            .filter(|s| s.content.contains('│'))
            .count();
        assert!(
            sep_count >= 2,
            "need separators between 3 tabs, got {sep_count}"
        );
    }

    #[test]
    fn overflow_left_indicator_present() {
        let mut bar: TabBar<u32> = TabBar::new();
        for i in 0..20u32 {
            bar.open(i, format!("longfilename{i}.rs"));
        }
        bar.focus(&15);
        let line = build_line(30, &bar, &TabBarTheme::default());
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        // Either left or right overflow indicator expected given narrow width + 20 tabs.
        assert!(
            text.contains('<') || text.contains('>'),
            "expected overflow indicator with narrow width: {text:?}"
        );
    }

    #[test]
    fn theme_new_constructor() {
        let theme = TabBarTheme::new(
            Color::White,
            Color::Blue,
            Color::DarkGray,
            Color::Black,
            Color::Gray,
            Color::Cyan,
        );
        assert_eq!(theme.active_fg, Color::White);
        assert_eq!(theme.active_bg, Color::Blue);
        assert_eq!(theme.inactive_fg, Color::DarkGray);
        assert_eq!(theme.inactive_bg, Color::Black);
        assert_eq!(theme.sep_fg, Color::Gray);
        assert_eq!(theme.overflow_fg, Color::Cyan);
    }

    #[test]
    fn theme_default_is_dark_palette() {
        let t = TabBarTheme::default();
        // Active bg should be the catppuccin blue.
        assert_eq!(t.active_bg, Color::Rgb(0x5e, 0x81, 0xac));
    }

    #[test]
    fn build_line_inactive_tab_not_bold() {
        let mut bar = make_bar(&[(1, "active.rs"), (2, "inactive.rs")]);
        bar.focus(&1);
        let line = build_line(80, &bar, &TabBarTheme::default());
        // Find the span for "inactive.rs" — should not have BOLD.
        let inactive_bold = line.spans.iter().any(|s| {
            s.content.contains("inactive.rs") && s.style.add_modifier.contains(Modifier::BOLD)
        });
        assert!(!inactive_bold, "inactive tab must not be bold");
    }

    #[test]
    fn render_does_not_panic_on_zero_width() {
        let bar = make_bar(&[(1, "x.rs")]);
        let backend = TestBackend::new(0, 1);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        // Should not panic.
        terminal
            .draw(|frame| {
                let area = Rect::new(0, 0, 0, 1);
                render(frame, area, &bar, &TabBarTheme::default());
            })
            .expect("draw");
    }

    #[test]
    fn all_tabs_visible_when_width_sufficient() {
        let bar = make_bar(&[(1, "a"), (2, "b"), (3, "c")]);
        let line = build_line(200, &bar, &TabBarTheme::default());
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains(" a "), "{text:?}");
        assert!(text.contains(" b "), "{text:?}");
        assert!(text.contains(" c "), "{text:?}");
        // No overflow indicators when everything fits.
        assert!(!text.contains('<'), "{text:?}");
        assert!(!text.contains('>'), "{text:?}");
    }
}
