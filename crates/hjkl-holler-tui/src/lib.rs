//! Ratatui adapter for `hjkl-holler`.
//!
//! Renders active toast notifications as a floating stack in the top-right
//! corner of the terminal with severity-coloured borders and a soft
//! `Modifier::DIM` fade in the last 500 ms before dismissal.
//!
//! # Quick start
//!
//! ```rust,no_run
//! // (requires a real ratatui terminal — compile-checked, not run in CI)
//! use hjkl_holler::HollerBus;
//! use hjkl_holler_tui::{HollerLayout, render_active};
//! // frame and area come from your ratatui setup
//! ```

use hjkl_holler::{HollerBus, Severity};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use std::time::SystemTime;

// ── HollerLayout ──────────────────────────────────────────────────────────────

/// Layout configuration for the toast stack renderer.
///
/// `#[non_exhaustive]` — new fields may be added in minor releases.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct HollerLayout {
    /// Maximum width of a single toast popup in terminal columns (border
    /// included).
    pub max_width: u16,
    /// Maximum number of toasts shown simultaneously.
    pub max_visible: usize,
    /// `(top, right)` margin from the terminal edge in terminal cells.
    pub margin: (u16, u16),
}

impl HollerLayout {
    /// Construct with explicit values.
    pub fn new(max_width: u16, max_visible: usize, margin: (u16, u16)) -> Self {
        Self {
            max_width,
            max_visible,
            margin,
        }
    }
}

impl Default for HollerLayout {
    fn default() -> Self {
        Self {
            max_width: 48,
            max_visible: 5,
            margin: (1, 1),
        }
    }
}

// ── Severity colour mapping ───────────────────────────────────────────────────

fn severity_color(sev: Severity) -> Color {
    match sev {
        Severity::Info => Color::Rgb(0x89, 0xb4, 0xfa), // catppuccin blue
        Severity::Warn => Color::Rgb(0xf9, 0xe2, 0xaf), // catppuccin yellow
        Severity::Error => Color::Rgb(0xf3, 0x8b, 0xa8), // catppuccin red
        _ => Color::Gray,
    }
}

// ── render_active ─────────────────────────────────────────────────────────────

/// Render the active toast stack into `frame`.
///
/// Toasts are stacked from the top-right of `area`, newest on top.
/// Each toast is a floating bordered box whose body is word-wrapped to
/// `layout.max_width - 2` columns. In the last 500 ms before expiry the
/// toast is dimmed with [`Modifier::DIM`].
///
/// `now` is passed in so callers control the clock (useful in tests).
pub fn render_active(
    frame: &mut Frame,
    area: Rect,
    bus: &HollerBus,
    layout: &HollerLayout,
    now: SystemTime,
) {
    if area.width < 8 || area.height < 3 {
        return;
    }

    let (margin_top, margin_right) = layout.margin;

    // Collect active toasts, newest first (reverse push order).
    let toasts: Vec<_> = bus
        .active(now)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .take(layout.max_visible)
        .collect();

    if toasts.is_empty() {
        return;
    }

    let popup_width = layout
        .max_width
        .min(area.width.saturating_sub(margin_right));
    let inner_width = popup_width.saturating_sub(2); // subtract border cells

    // Walk down from `margin_top`, placing each toast below the previous.
    let mut next_y = area.y.saturating_add(margin_top);
    let right_edge = area.x + area.width;
    let popup_x = right_edge.saturating_sub(popup_width + margin_right);

    for toast in toasts {
        // Wrap body text to inner_width.
        let body = toast.display_body();
        let wrapped_lines = wrap_text(&body, inner_width as usize);
        let content_h = wrapped_lines.len().max(1) as u16;
        let popup_h = content_h + 2; // top + bottom border

        if next_y + popup_h > area.y + area.height {
            break; // no room for more toasts
        }

        let rect = Rect {
            x: popup_x,
            y: next_y,
            width: popup_width,
            height: popup_h,
        };

        // Dim modifier in last 500 ms.
        let base_style = if toast.is_fading(now) {
            Style::default().add_modifier(Modifier::DIM)
        } else {
            Style::default()
        };

        let border_color = severity_color(toast.severity);
        let border_style = base_style.fg(border_color);

        let title = format!(" {} ", toast.severity.label());
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(Span::styled(title, border_style));

        let inner = block.inner(rect);

        // Build ratatui Lines for the wrapped body.
        let lines: Vec<Line<'static>> = wrapped_lines
            .into_iter()
            .map(|l| Line::from(Span::styled(l, base_style)))
            .collect();

        frame.render_widget(Clear, rect);
        frame.render_widget(block, rect);

        let para = Paragraph::new(lines).wrap(Wrap { trim: false });
        frame.render_widget(para, inner);

        next_y += popup_h;
    }
}

// ── wrap_text ─────────────────────────────────────────────────────────────────

/// Word-wrap `text` to `max_cols` columns. Returns owned `String` lines.
///
/// Words longer than `max_cols` are hard-split at char boundaries so the
/// line count matches what actually fits — otherwise the popup height is
/// under-counted and the tail of the notification gets clipped.
fn wrap_text(text: &str, max_cols: usize) -> Vec<String> {
    if max_cols == 0 {
        return vec![text.to_string()];
    }
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut current = String::new();
        let mut current_len = 0usize; // chars, not bytes
        for word in paragraph.split_whitespace() {
            let word_len = word.chars().count();
            if word_len > max_cols {
                // Hard-split an unbreakable word (long path, URL, …).
                if !current.is_empty() {
                    lines.push(std::mem::take(&mut current));
                    current_len = 0;
                }
                for ch in word.chars() {
                    if current_len == max_cols {
                        lines.push(std::mem::take(&mut current));
                        current_len = 0;
                    }
                    current.push(ch);
                    current_len += 1;
                }
            } else if current.is_empty() {
                current.push_str(word);
                current_len = word_len;
            } else if current_len + 1 + word_len <= max_cols {
                current.push(' ');
                current.push_str(word);
                current_len += 1 + word_len;
            } else {
                lines.push(std::mem::take(&mut current));
                current.push_str(word);
                current_len = word_len;
            }
        }
        if !current.is_empty() {
            lines.push(current);
        }
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use hjkl_holler::{HollerBus, Severity};

    #[test]
    fn holler_layout_default() {
        let l = HollerLayout::default();
        assert_eq!(l.max_width, 48);
        assert_eq!(l.max_visible, 5);
        assert_eq!(l.margin, (1, 1));
    }

    #[test]
    fn holler_layout_new() {
        let l = HollerLayout::new(60, 3, (2, 2));
        assert_eq!(l.max_width, 60);
        assert_eq!(l.max_visible, 3);
        assert_eq!(l.margin, (2, 2));
    }

    #[test]
    fn severity_color_info_is_blue() {
        let c = severity_color(Severity::Info);
        assert!(matches!(c, Color::Rgb(0x89, 0xb4, 0xfa)));
    }

    #[test]
    fn severity_color_warn_is_yellow() {
        let c = severity_color(Severity::Warn);
        assert!(matches!(c, Color::Rgb(0xf9, 0xe2, 0xaf)));
    }

    #[test]
    fn severity_color_error_is_red() {
        let c = severity_color(Severity::Error);
        assert!(matches!(c, Color::Rgb(0xf3, 0x8b, 0xa8)));
    }

    #[test]
    fn wrap_text_short_line_unchanged() {
        let lines = wrap_text("hello world", 40);
        assert_eq!(lines, vec!["hello world"]);
    }

    #[test]
    fn wrap_text_wraps_long_line() {
        let lines = wrap_text("one two three four five", 10);
        for l in &lines {
            assert!(l.len() <= 10 || !l.contains(' '), "too long: {l:?}");
        }
        assert!(lines.len() >= 2, "should have wrapped");
    }

    #[test]
    fn wrap_text_hard_splits_overlong_word() {
        // Regression: a word longer than max_cols used to land on a single
        // line, under-counting the popup height and clipping the toast body.
        let long = "x".repeat(25);
        let lines = wrap_text(&long, 10);
        assert_eq!(lines.len(), 3, "25 chars at 10 cols → 3 lines: {lines:?}");
        for l in &lines {
            assert!(l.chars().count() <= 10, "too long: {l:?}");
        }
        assert_eq!(lines.concat(), long, "no characters may be lost");
    }

    #[test]
    fn wrap_text_counts_chars_not_bytes() {
        // 8 two-byte chars fit in 10 cols on one line.
        let s = "éééééééé";
        let lines = wrap_text(s, 10);
        assert_eq!(lines, vec![s.to_string()]);
    }

    #[test]
    fn wrap_text_empty_string() {
        let lines = wrap_text("", 40);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn wrap_text_zero_max_cols() {
        // Should not panic.
        let lines = wrap_text("hello", 0);
        assert!(!lines.is_empty());
    }

    #[test]
    fn render_active_empty_bus_no_panic() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let bus = HollerBus::new();
        let layout = HollerLayout::default();
        let now = SystemTime::now();

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                render_active(frame, area, &bus, &layout, now);
            })
            .unwrap();
        // No active toasts → render_active returns early without drawing anything.
        assert_eq!(bus.active(now).count(), 0);
    }

    #[test]
    fn max_visible_respected() {
        let mut bus = HollerBus::new();
        for i in 0..10 {
            bus.push(Severity::Info, format!("msg {i}"));
        }
        let now = SystemTime::now();
        let layout = HollerLayout::new(40, 3, (1, 1));
        let visible: Vec<_> = bus
            .active(now)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .take(layout.max_visible)
            .collect();
        assert_eq!(visible.len(), 3);
    }
}
