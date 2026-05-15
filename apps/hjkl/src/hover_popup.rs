//! Hover popup widget for LSP `textDocument/hover` results (Phase 5 mouse support).
//!
//! Displays multi-line text (markdown stripped by the LSP glue layer) in a
//! floating box anchored at the mouse position. No interactive elements —
//! dismissed by mouse move, any key press, or an 8-second auto-fade.

use std::time::Instant;

use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

/// A floating popup that displays multi-line text (typically markdown from
/// `textDocument/hover`). Sits above the editor, dismissed by mouse move or
/// any key press. No interactive elements.
pub struct HoverPopup {
    /// Raw (already-stripped) hover text.
    pub content: String,
    /// (col, row) cell where the mouse rested — used for anchoring.
    pub anchor: (u16, u16),
    /// Maximum width of the popup (including border).
    pub max_width: u16,
    /// Maximum height of the popup (including border).
    pub max_height: u16,
    /// Instant the popup was first shown — used for 8-second auto-fade.
    pub displayed_at: Instant,
}

/// Duration before a stationary hover popup is automatically dismissed.
pub const HOVER_AUTO_FADE: std::time::Duration = std::time::Duration::from_secs(8);

impl HoverPopup {
    /// Construct a new popup anchored at `anchor` (terminal cell col, row).
    pub fn new(content: String, anchor: (u16, u16)) -> Self {
        Self {
            content,
            anchor,
            max_width: 60,
            max_height: 12,
            displayed_at: Instant::now(),
        }
    }

    /// Returns `true` when the 8-second auto-fade deadline has passed.
    pub fn is_expired(&self) -> bool {
        self.displayed_at.elapsed() >= HOVER_AUTO_FADE
    }

    /// Compute the on-screen rect, clamping to stay inside `screen`.
    ///
    /// Preferred position: one row below and at the same column as `anchor`.
    /// If there is not enough vertical room below, flip so the popup renders
    /// above the anchor row. Horizontally, shift left if the popup would
    /// overflow the right edge.
    pub fn bounding_rect(&self, screen: Rect) -> Rect {
        let (ax, ay) = self.anchor;

        // Compute content dimensions.
        let content_w = self.content_width();
        let content_h = self.content_height();

        // Total popup size including border (1 cell each side).
        let popup_w = (content_w + 2).min(self.max_width).max(4);
        let popup_h = (content_h + 2).min(self.max_height).max(3);

        // Horizontal: prefer anchor col, shift left if overflowing.
        let x = if ax + popup_w <= screen.x + screen.width {
            ax
        } else {
            (screen.x + screen.width).saturating_sub(popup_w)
        };

        // Vertical: prefer one row below anchor; flip above if no room.
        let below_y = ay.saturating_add(1);
        let y = if below_y + popup_h <= screen.y + screen.height {
            below_y
        } else {
            // Not enough room below — render above the anchor.
            ay.saturating_sub(popup_h)
        };

        Rect {
            x,
            y,
            width: popup_w,
            height: popup_h,
        }
    }

    /// Render the popup into `frame`. `screen` is the full terminal area.
    pub fn render(&self, frame: &mut Frame, screen: Rect, theme: &crate::theme::AppTheme) {
        let rect = self.bounding_rect(screen);
        frame.render_widget(Clear, rect);

        let ui = &theme.ui;
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(ui.border_active))
            .title(" hover ");
        let inner = block.inner(rect);
        frame.render_widget(block, rect);

        let para = Paragraph::new(self.content.clone())
            .style(Style::default().fg(ui.text).bg(ui.panel_bg))
            .wrap(Wrap { trim: false });
        frame.render_widget(para, inner);
    }

    /// Width of the widest line in `content` (capped at `max_width - 2`).
    fn content_width(&self) -> u16 {
        let max_inner = self.max_width.saturating_sub(2);
        self.content
            .lines()
            .map(|l| l.len() as u16)
            .max()
            .unwrap_or(8)
            .min(max_inner)
    }

    /// Number of wrapped display lines for `content` given `max_width - 2`
    /// inner space, capped at `max_height - 2`.
    fn content_height(&self) -> u16 {
        let inner_w = self.max_width.saturating_sub(2).max(1) as usize;
        let max_inner = self.max_height.saturating_sub(2).max(1) as usize;
        let mut rows = 0usize;
        for line in self.content.lines() {
            // Each source line occupies at least one display row; long lines
            // wrap into additional rows.
            let chars = line.len();
            let wrapped = if chars == 0 {
                1
            } else {
                chars.div_ceil(inner_w)
            };
            rows += wrapped;
            if rows >= max_inner {
                return max_inner as u16;
            }
        }
        rows.max(1) as u16
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(w: u16, h: u16) -> Rect {
        Rect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        }
    }

    #[test]
    fn bounding_rect_stays_on_screen_right_edge() {
        // Anchor near the right edge; popup should shift left to stay on screen.
        let popup = HoverPopup::new("hello world\nsecond line".into(), (75, 5));
        // popup.max_width = 60, content_width ~= 11, so popup_w = 13
        let r = popup.bounding_rect(screen(80, 24));
        assert!(
            r.x + r.width <= 80,
            "popup right edge {} overflows screen width 80",
            r.x + r.width
        );
    }

    #[test]
    fn bounding_rect_flips_when_no_vertical_room() {
        // Anchor near the bottom — popup must render above the anchor.
        let popup = HoverPopup::new("line1\nline2\nline3".into(), (0, 22));
        let r = popup.bounding_rect(screen(80, 24));
        // With anchor at row 22 in a 24-row screen: below_y = 23, popup_h >= 5
        // (3 content + 2 border). 23 + 5 > 24 → must flip above.
        assert!(
            r.y < 22,
            "popup y {} should be above anchor row 22 when near bottom",
            r.y
        );
    }
}
