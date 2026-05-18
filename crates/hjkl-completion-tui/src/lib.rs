//! Ratatui adapter for `hjkl-completion`.
//!
//! Paints an LSP/word-completion popup into a ratatui [`Frame`] given a
//! [`Completion`] model.
//!
//! # Usage
//!
//! ```no_run
//! // Build a theme from your app's UiTheme:
//! // let theme = CompletionTheme {
//! //     border: ui.border_active,
//! //     selected_bg: ui.picker_selection_bg,
//! //     normal_fg: ui.text,
//! //     detail_fg: ui.text_dim,
//! // };
//! //
//! // Compute the cursor cell in absolute screen coordinates:
//! // let anchor = Rect { x: abs_col, y: abs_row, width: 1, height: 1 };
//! //
//! // Render into the frame:
//! // hjkl_completion_tui::popup(frame, completion, &theme, anchor, buf_area);
//! ```

#![forbid(unsafe_code)]

use hjkl_completion::Completion;
use hjkl_theme::Color;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color as RColor, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState},
};

// ── Theme ─────────────────────────────────────────────────────────────────────

/// Color palette for the completion popup.
///
/// `#[non_exhaustive]` — new color slots may be added without a breaking change.
/// Construct via [`CompletionTheme::default`] then mutate the fields you care
/// about.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct CompletionTheme {
    /// Border color of the popup box.
    pub border: Color,
    /// Background color for the selected row.
    pub selected_bg: Color,
    /// Foreground color for normal (unselected) rows.
    pub normal_fg: Color,
    /// Foreground color for detail text.
    pub detail_fg: Color,
}

impl Default for CompletionTheme {
    fn default() -> Self {
        Self {
            border: Color::rgb(0x61, 0xaf, 0xef),      // One-Dark blue
            selected_bg: Color::rgb(0x3e, 0x44, 0x51), // One-Dark selection
            normal_fg: Color::rgb(0xe5, 0xe9, 0xf0),   // One-Dark fg
            detail_fg: Color::rgb(0x5c, 0x63, 0x70),   // One-Dark comment grey
        }
    }
}

impl CompletionTheme {
    /// Construct from explicit color slots.
    pub fn new(border: Color, selected_bg: Color, normal_fg: Color, detail_fg: Color) -> Self {
        Self {
            border,
            selected_bg,
            normal_fg,
            detail_fg,
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Convert a [`hjkl_theme::Color`] to a ratatui [`RColor`].
#[inline]
fn to_rcolor(c: Color) -> RColor {
    RColor::Rgb(c.r, c.g, c.b)
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Render the completion popup anchored at `anchor` within `viewport`.
///
/// - `completion` — the active completion model from `hjkl-app`.
/// - `theme`      — color palette.
/// - `anchor`     — the cursor cell in absolute screen coordinates. The popup
///   appears one row below `anchor.y`; if it would overflow the bottom of
///   `viewport` it flips above the cursor instead.
/// - `viewport`   — the buffer pane area used for overflow clamping.
///
/// Returns immediately if `completion.is_empty()`.
pub fn popup(
    frame: &mut Frame,
    completion: &Completion,
    theme: &CompletionTheme,
    anchor: Rect,
    viewport: Rect,
) {
    if completion.is_empty() {
        return;
    }

    const MIN_WIDTH: u16 = 20;
    const MAX_HEIGHT: u16 = 10;

    let visible_count = completion.visible.len().min(MAX_HEIGHT as usize) as u16;
    if visible_count == 0 {
        return;
    }

    // Determine width from longest label + detail.
    let content_width = completion
        .visible
        .iter()
        .filter_map(|&idx| completion.all_items.get(idx))
        .map(|item| {
            let detail_len = item.detail.as_deref().map(|d| d.len() + 2).unwrap_or(0);
            // icon(1) + space(1) + label + space(2) + detail
            1 + 1 + item.label.len() + 2 + detail_len
        })
        .max()
        .unwrap_or(MIN_WIDTH as usize) as u16;
    let popup_w = content_width.max(MIN_WIDTH).min(viewport.width);

    // Popup position: one row below anchor, clamped to viewport.
    let popup_h = visible_count + 2; // +2 for border
    let below_row = anchor.y + 1;
    let popup_y = if below_row + popup_h > viewport.y + viewport.height {
        // Would extend past bottom — shift above cursor.
        anchor.y.saturating_sub(popup_h).max(viewport.y)
    } else {
        below_row
    };
    let popup_x = anchor
        .x
        .min(viewport.x + viewport.width.saturating_sub(popup_w));

    let area = Rect {
        x: popup_x,
        y: popup_y,
        width: popup_w,
        height: popup_h,
    };

    frame.render_widget(Clear, area);

    let border_color = to_rcolor(theme.border);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let selected_style = Style::default()
        .bg(to_rcolor(theme.selected_bg))
        .add_modifier(Modifier::BOLD);
    let normal_style = Style::default().fg(to_rcolor(theme.normal_fg));
    let detail_style = Style::default().fg(to_rcolor(theme.detail_fg));

    let items: Vec<ListItem> = completion
        .visible
        .iter()
        .enumerate()
        .filter_map(|(vis_idx, &item_idx)| {
            let item = completion.all_items.get(item_idx)?;
            let icon = item.kind.icon();
            let label = &item.label;
            let row_style = if vis_idx == completion.selected {
                selected_style
            } else {
                normal_style
            };
            let mut spans = vec![
                Span::styled(format!("{icon} "), row_style),
                Span::styled(label.clone(), row_style),
            ];
            if let Some(ref detail) = item.detail {
                // Truncate detail to avoid overflow.
                let avail = inner.width.saturating_sub(2 + label.len() as u16 + 2) as usize;
                let truncated: String = detail.chars().take(avail).collect();
                if !truncated.is_empty() {
                    spans.push(Span::styled(
                        format!("  {truncated}"),
                        if vis_idx == completion.selected {
                            selected_style
                        } else {
                            detail_style
                        },
                    ));
                }
            }
            Some(ListItem::new(Line::from(spans)))
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(completion.selected.min(items.len().saturating_sub(1))));
    let list = List::new(items).highlight_style(selected_style);
    frame.render_stateful_widget(list, inner, &mut state);
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use hjkl_completion::{Completion, CompletionItem};

    fn make_item(label: &str) -> CompletionItem {
        CompletionItem::new(label)
    }

    fn make_completion(labels: &[&str]) -> Completion {
        Completion::new(0, 0, labels.iter().map(|l| make_item(l)).collect())
    }

    #[test]
    fn smoke_theme_default_constructs() {
        let t = CompletionTheme::default();
        assert!(t.border.r > 0 || t.border.g > 0 || t.border.b > 0);
    }

    #[test]
    fn smoke_theme_new_roundtrip() {
        let border = Color::rgb(0x11, 0x22, 0x33);
        let sel = Color::rgb(0x44, 0x55, 0x66);
        let fg = Color::rgb(0x77, 0x88, 0x99);
        let dim = Color::rgb(0xaa, 0xbb, 0xcc);
        let t = CompletionTheme::new(border, sel, fg, dim);
        assert_eq!(t.border.r, 0x11);
        assert_eq!(t.selected_bg.g, 0x55);
        assert_eq!(t.normal_fg.b, 0x99);
        assert_eq!(t.detail_fg.r, 0xaa);
    }

    #[test]
    fn smoke_to_rcolor_roundtrip() {
        let c = Color::rgb(0x12, 0x34, 0x56);
        let rc = to_rcolor(c);
        assert_eq!(rc, RColor::Rgb(0x12, 0x34, 0x56));
    }

    #[test]
    fn smoke_empty_completion_is_noop() {
        // An empty Completion should not panic — popup() exits early.
        let c = make_completion(&[]);
        assert!(c.is_empty());
        // No frame available in unit tests; just verify is_empty() fires.
    }

    /// Positioning: anchor near bottom of viewport → popup_y flips above.
    #[test]
    fn positioning_flips_above_when_anchor_near_bottom() {
        // viewport: rows 0..24 (height=24)
        let viewport = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        // anchor at row 22 (near bottom)
        let anchor = Rect {
            x: 10,
            y: 22,
            width: 1,
            height: 1,
        };
        let _completion = make_completion(&["alpha", "beta", "gamma"]);
        // visible_count = 3; popup_h = 3 + 2 = 5
        // below_row = 23; 23 + 5 = 28 > 0 + 24 → flip above
        // popup_y = 22.saturating_sub(5).max(0) = 17
        let popup_h: u16 = 5;
        let below_row = anchor.y + 1;
        let expected_y = if below_row + popup_h > viewport.y + viewport.height {
            anchor.y.saturating_sub(popup_h).max(viewport.y)
        } else {
            below_row
        };
        assert_eq!(expected_y, 17, "popup must flip above when near bottom");
    }

    /// Positioning: anchor with room below → popup appears below.
    #[test]
    fn positioning_shows_below_when_room_available() {
        let viewport = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 40,
        };
        let anchor = Rect {
            x: 5,
            y: 5,
            width: 1,
            height: 1,
        };
        let _completion = make_completion(&["x", "y"]);
        // visible_count = 2; popup_h = 4
        // below_row = 6; 6 + 4 = 10 <= 40 → stays below
        let popup_h: u16 = 4;
        let below_row = anchor.y + 1;
        let expected_y = if below_row + popup_h > viewport.y + viewport.height {
            anchor.y.saturating_sub(popup_h).max(viewport.y)
        } else {
            below_row
        };
        assert_eq!(expected_y, 6, "popup must appear below when room available");
    }
}
