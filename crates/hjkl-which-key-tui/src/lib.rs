//! Ratatui adapter for `hjkl-which-key`.
//!
//! Paints a which-key popup into a ratatui [`Frame`] given a [`PopupLayout`]
//! produced by [`hjkl_which_key::layout`].
//!
//! # Usage
//!
//! ```no_run
//! // Build entries + layout:
//! // let entries = hjkl_which_key::entries_for(&km, mode, &prefix, leader);
//! // let l = hjkl_which_key::layout(&entries, area.width);
//! //
//! // Build a theme from your app's UiTheme:
//! // let theme = PopupTheme { border: ui.border_active_color, ... };
//! //
//! // Render into the frame:
//! // hjkl_which_key_tui::render(frame, &l, "root", &theme, area);
//! ```

#![forbid(unsafe_code)]

use hjkl_theme::Color;
use hjkl_theme_tui::ToRatatui;
use hjkl_which_key::PopupLayout;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

// ── Theme ─────────────────────────────────────────────────────────────────────

/// Color palette for the which-key popup.
///
/// `#[non_exhaustive]` — new color slots may be added without a breaking change.
/// Construct via [`PopupTheme::default`] then mutate the fields you care about.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct PopupTheme {
    /// Border + key highlight color.
    pub border: Color,
    /// Dimmed description text color.
    pub desc: Color,
}

impl Default for PopupTheme {
    fn default() -> Self {
        Self {
            border: Color::rgb(0x61, 0xaf, 0xef), // One-Dark blue
            desc: Color::rgb(0x5c, 0x63, 0x70),   // One-Dark comment grey
        }
    }
}

impl PopupTheme {
    /// Construct from explicit border and desc colors.
    pub fn new(border: Color, desc: Color) -> Self {
        Self { border, desc }
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Render the which-key popup anchored at the bottom of `buf_area`.
///
/// - `layout`  — geometry produced by [`hjkl_which_key::layout`].
/// - `header`  — prefix notation shown in the border title (e.g. `"root"`, `"g"`).
/// - `theme`   — color palette.
/// - `buf_area` — the pane area the popup anchors to (popup sits at its bottom).
pub fn render(
    frame: &mut Frame,
    layout: &PopupLayout,
    header: &str,
    theme: &PopupTheme,
    buf_area: Rect,
) {
    if layout.visible.is_empty() {
        return;
    }

    let popup_y = buf_area.y + buf_area.height.saturating_sub(layout.popup_h);
    let area = Rect {
        x: buf_area.x,
        y: popup_y,
        width: layout.popup_w,
        height: layout.popup_h,
    };

    frame.render_widget(Clear, area);

    let border_color = theme.border.to_ratatui();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(format!(" {header} "));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let key_style = Style::default()
        .fg(border_color)
        .add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(theme.desc.to_ratatui());

    let col_width = layout.col_width as usize;
    let mut lines: Vec<Line> = Vec::with_capacity(layout.rows);

    for row in 0..layout.rows {
        let mut spans: Vec<Span> = Vec::new();
        for col in 0..layout.cols {
            let idx = row * layout.cols + col;
            if let Some(entry) = layout.visible.get(idx) {
                let entry_str = format!("{} {}", entry.key, entry.desc);
                let padding = " ".repeat(col_width.saturating_sub(entry_str.len()));
                spans.push(Span::styled(entry.key.clone(), key_style));
                spans.push(Span::styled(
                    format!(" {}{}", entry.desc, padding),
                    desc_style,
                ));
            }
        }
        lines.push(Line::from(spans));
    }

    let para = Paragraph::new(lines);
    frame.render_widget(para, inner);
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use hjkl_which_key::{Entry, layout};

    #[test]
    fn smoke_theme_default_constructs() {
        let t = PopupTheme::default();
        // border is a non-zero color
        assert!(t.border.r > 0 || t.border.g > 0 || t.border.b > 0);
    }

    #[test]
    fn smoke_to_rcolor_roundtrip() {
        // Pins the conversion this crate used to implement locally; it now
        // routes through `hjkl-theme-tui` and must be unchanged.
        use ratatui::style::Color as RColor;
        let c = Color::rgb(0x12, 0x34, 0x56);
        assert_eq!(c.to_ratatui(), RColor::Rgb(0x12, 0x34, 0x56));
    }

    #[test]
    fn smoke_render_empty_does_not_panic() {
        // layout with no entries → visible is empty → render is a no-op.
        let entries: Vec<Entry> = vec![];
        let l = layout(&entries, 80);
        assert!(l.visible.is_empty());
        // render itself requires a Frame which needs a backend — just verify
        // that layout produces the expected shape so the guard fires correctly.
        assert_eq!(l.rows, 0);
    }

    #[test]
    fn smoke_render_layout_shape() {
        let entries = vec![Entry::new("x", "exit"), Entry::new("w", "write")];
        let l = layout(&entries, 80);
        assert_eq!(l.visible.len(), 2);
        assert!(l.rows >= 1);
        assert!(l.popup_h >= 4); // at least 1 row + 3
    }
}
