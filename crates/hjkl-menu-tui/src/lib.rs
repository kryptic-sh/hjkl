//! Ratatui adapter for `hjkl-menu`.
//!
//! Paints a [`ContextMenu`] into a ratatui [`Frame`] using [`MenuTheme`] for
//! styling. The popup is a floating bordered box clamped to `screen_size`.
//!
//! # Usage
//!
//! ```rust,no_run
//! // (requires a real ratatui terminal — compile-checked, not run in CI)
//! use hjkl_menu::{build_code_menu, ContextMenu};
//! use hjkl_menu_tui::{MenuTheme, bounding_rect, render};
//! // Build menu:
//! // let items = build_code_menu(true, true);
//! // let menu  = ContextMenu::new(items, (col, row));
//! // Compute rect and render:
//! // let screen = frame.area();
//! // let rect   = bounding_rect(&menu, screen);
//! // render(frame, &menu, screen, &MenuTheme::default());
//! ```

#![forbid(unsafe_code)]

use hjkl_menu::{ContextMenu, MenuAction, MenuItem};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

// ── MenuTheme ─────────────────────────────────────────────────────────────────

/// Color palette for the context menu.
///
/// `#[non_exhaustive]` — new color slots may be added in minor releases.
/// Construct via [`MenuTheme::default`] then override individual fields.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct MenuTheme {
    /// Border color.
    pub border: Color,
    /// Foreground for normal (not selected, not disabled) items.
    pub normal_fg: Color,
    /// Foreground and background for the highlighted row.
    pub selected_fg: Color,
    /// Background for the highlighted row.
    pub selected_bg: Color,
    /// Foreground for disabled items and hint text.
    pub dimmed_fg: Color,
    /// Foreground for separator lines.
    pub separator_fg: Color,
}

impl Default for MenuTheme {
    fn default() -> Self {
        Self {
            border: Color::Gray,
            normal_fg: Color::White,
            selected_fg: Color::Black,
            selected_bg: Color::White,
            dimmed_fg: Color::DarkGray,
            separator_fg: Color::DarkGray,
        }
    }
}

impl MenuTheme {
    /// Construct from explicit values.
    pub fn new(
        border: Color,
        normal_fg: Color,
        selected_fg: Color,
        selected_bg: Color,
        dimmed_fg: Color,
        separator_fg: Color,
    ) -> Self {
        Self {
            border,
            normal_fg,
            selected_fg,
            selected_bg,
            dimmed_fg,
            separator_fg,
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Convert `hjkl_menu::ContextMenu` geometry to a ratatui [`Rect`] clamped
/// to `screen_size`.
///
/// This is a thin wrapper around [`ContextMenu::bounding_rect`] that converts
/// from `(u16, u16, u16, u16)` to `Rect`.
///
/// ```rust
/// use hjkl_menu::{ContextMenu, MenuItem, MenuAction};
/// use hjkl_menu_tui::bounding_rect;
/// use ratatui::layout::Rect;
///
/// let items = vec![MenuItem::new("Copy", MenuAction::Copy, None)];
/// let menu  = ContextMenu::new(items, (10, 5));
/// let screen = Rect::new(0, 0, 80, 24);
/// let r = bounding_rect(&menu, screen);
/// assert!(r.x + r.width  <= screen.width);
/// assert!(r.y + r.height <= screen.height);
/// ```
pub fn bounding_rect(menu: &ContextMenu, screen_size: Rect) -> Rect {
    let (x, y, w, h) = menu.bounding_rect(screen_size.width, screen_size.height);
    // Re-add the screen origin (handles non-zero x/y terminals).
    Rect {
        x: screen_size.x + x,
        y: screen_size.y + y,
        width: w,
        height: h,
    }
}

/// Render `menu` as a floating bordered popup inside `screen_size`.
///
/// Call this after all other widgets so the popup floats above them.
pub fn render(frame: &mut Frame, menu: &ContextMenu, screen_size: Rect, theme: &MenuTheme) {
    if menu.items.is_empty() {
        return;
    }

    let rect = bounding_rect(menu, screen_size);

    // Clear the cells behind the popup.
    frame.render_widget(Clear, rect);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let content_w = inner.width;
    for (i, item) in menu.items.iter().enumerate() {
        let row_y = inner.y + i as u16;
        if row_y >= inner.y + inner.height {
            break;
        }
        let row_rect = Rect {
            x: inner.x,
            y: row_y,
            width: content_w,
            height: 1,
        };
        render_item(frame, item, i, menu.selected, content_w, row_rect, theme);
    }
}

/// Render a single menu row into `row_rect`.
fn render_item(
    frame: &mut Frame,
    item: &MenuItem,
    idx: usize,
    selected: usize,
    content_w: u16,
    row_rect: Rect,
    theme: &MenuTheme,
) {
    // ── Separator ──────────────────────────────────────────────────────────
    if item.action == MenuAction::Separator {
        let sep: String = "─".repeat(content_w as usize);
        let para = Paragraph::new(sep).style(Style::default().fg(theme.separator_fg));
        frame.render_widget(para, row_rect);
        return;
    }

    // ── Info header (dimmed, not highlighted) ──────────────────────────────
    if item.action == MenuAction::Info {
        let line = Line::from(vec![
            Span::raw(" "),
            Span::styled(item.label.clone(), Style::default().fg(theme.dimmed_fg)),
        ]);
        frame.render_widget(Paragraph::new(line), row_rect);
        return;
    }

    let is_selected = idx == selected;
    let is_disabled = !item.enabled;

    let label_style = if is_disabled {
        Style::default().fg(theme.dimmed_fg)
    } else if is_selected {
        Style::default()
            .fg(theme.selected_fg)
            .bg(theme.selected_bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.normal_fg)
    };

    let hint_style = if is_disabled {
        Style::default().fg(theme.dimmed_fg)
    } else if is_selected {
        Style::default().fg(theme.dimmed_fg).bg(theme.selected_bg)
    } else {
        Style::default().fg(theme.dimmed_fg)
    };

    let label = &item.label;
    let hint = item.shortcut_hint.as_deref().unwrap_or("");
    let hint_len = if hint.is_empty() { 0 } else { hint.len() + 2 };
    let gap = (content_w as usize).saturating_sub(label.len() + hint_len + 1);

    let line = if hint.is_empty() {
        Line::from(vec![
            Span::raw(" "),
            Span::styled(label.clone(), label_style),
        ])
    } else {
        let spaces = " ".repeat(gap.max(1));
        Line::from(vec![
            Span::raw(" "),
            Span::styled(label.clone(), label_style),
            Span::raw(spaces),
            Span::styled(hint.to_string(), hint_style),
        ])
    };

    let row_bg = if is_selected {
        Style::default().bg(theme.selected_bg)
    } else {
        Style::default()
    };
    frame.render_widget(Paragraph::new(line).style(row_bg), row_rect);
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use hjkl_menu::{
        ContextMenu, MenuAction, MenuItem, build_code_menu, build_picker_menu,
        build_split_border_menu, build_status_line_menu, build_tab_menu,
    };

    fn make_menu() -> ContextMenu {
        let items = vec![
            MenuItem::new("Cut", MenuAction::Cut, None),
            MenuItem::new("Copy", MenuAction::Copy, None),
            MenuItem::separator(),
            MenuItem::new("Paste", MenuAction::Paste, None),
        ];
        ContextMenu::new(items, (0, 0))
    }

    // ── bounding_rect ───────────────────────────────────────────────────────

    #[test]
    fn bounding_rect_stays_inside_screen() {
        let items: Vec<_> = (0..6)
            .map(|i| MenuItem::new(format!("Item {i}"), MenuAction::Paste, None))
            .collect();
        let menu = ContextMenu::new(items, (5, 22));
        let screen = Rect::new(0, 0, 80, 24);
        let r = bounding_rect(&menu, screen);
        assert!(
            r.x + r.width <= screen.width,
            "right edge must not exceed screen width"
        );
        assert!(
            r.y + r.height <= screen.height,
            "bottom edge must not exceed screen height"
        );
    }

    #[test]
    fn bounding_rect_near_bottom_flips_upward() {
        let items: Vec<_> = (0..6)
            .map(|i| MenuItem::new(format!("Item {i}"), MenuAction::Paste, None))
            .collect();
        let menu = ContextMenu::new(items, (5, 22));
        let screen = Rect::new(0, 0, 80, 24);
        let r = bounding_rect(&menu, screen);
        assert_eq!(r.height, 8, "6 items + 2 border = 8");
        assert!(
            r.y < 22,
            "popup must flip above anchor row 22; got y={}",
            r.y
        );
        assert_eq!(r.y, 24 - 8);
    }

    #[test]
    fn bounding_rect_near_right_shifts_left() {
        let items = vec![
            MenuItem::new("Reasonably Long Item Label", MenuAction::Paste, None),
            MenuItem::new("Another Long Item Label", MenuAction::Copy, None),
        ];
        let menu = ContextMenu::new(items, (75, 5));
        let screen = Rect::new(0, 0, 80, 24);
        let r = bounding_rect(&menu, screen);
        assert!(
            r.x + r.width <= screen.width,
            "right edge {} must not exceed 80",
            r.x + r.width
        );
        assert!(
            r.x < 75,
            "must have shifted left from anchor 75; got x={}",
            r.x
        );
    }

    #[test]
    fn bounding_rect_with_offset_screen_origin() {
        // Screen doesn't start at (0,0) — bounding_rect should add origin offset.
        let items = vec![MenuItem::new("Copy", MenuAction::Copy, None)];
        let menu = ContextMenu::new(items, (0, 0));
        let screen = Rect::new(5, 3, 80, 24);
        let r = bounding_rect(&menu, screen);
        // Origin offset must be included in x/y.
        assert!(r.x >= 5, "x must be >= screen origin x=5; got {}", r.x);
        assert!(r.y >= 3, "y must be >= screen origin y=3; got {}", r.y);
    }

    // ── row→item index math (regression for flipped-popup hover) ───────────

    #[test]
    fn row_to_item_index_correct_for_flipped_popup() {
        let items: Vec<_> = (0..4)
            .map(|i| MenuItem::new(format!("Item {i}"), MenuAction::Paste, None))
            .collect();
        let menu = ContextMenu::new(items, (5, 22));
        let screen = Rect::new(0, 0, 80, 24);
        let r = bounding_rect(&menu, screen);
        // 4 items + 2 border = 6 rows; rect.y = 24-6 = 18.
        assert_eq!(r.y, 18);
        let row0 = r.y + 1;
        let row3 = r.y + 4;
        assert_eq!((row0 - r.y - 1) as usize, 0);
        assert_eq!((row3 - r.y - 1) as usize, 3);
    }

    // ── MenuTheme ───────────────────────────────────────────────────────────

    #[test]
    fn menu_theme_default_fields() {
        let t = MenuTheme::default();
        assert_eq!(t.border, Color::Gray);
        assert_eq!(t.selected_bg, Color::White);
        assert_eq!(t.selected_fg, Color::Black);
    }

    #[test]
    fn menu_theme_new_roundtrip() {
        let t = MenuTheme::new(
            Color::Red,
            Color::Green,
            Color::Blue,
            Color::Yellow,
            Color::Magenta,
            Color::Cyan,
        );
        assert_eq!(t.border, Color::Red);
        assert_eq!(t.normal_fg, Color::Green);
        assert_eq!(t.selected_fg, Color::Blue);
        assert_eq!(t.selected_bg, Color::Yellow);
        assert_eq!(t.dimmed_fg, Color::Magenta);
        assert_eq!(t.separator_fg, Color::Cyan);
    }

    // ── Builder smoke tests (ensure builders produce non-empty menus) ────────

    #[test]
    fn build_code_menu_non_empty() {
        assert!(!build_code_menu(true, true).is_empty());
    }

    #[test]
    fn build_status_line_menu_non_empty() {
        assert!(!build_status_line_menu("rust", Some("rust-analyzer")).is_empty());
    }

    #[test]
    fn build_split_border_menu_non_empty() {
        assert!(!build_split_border_menu().is_empty());
    }

    #[test]
    fn build_picker_menu_non_empty() {
        assert!(!build_picker_menu(true).is_empty());
    }

    #[test]
    fn build_tab_menu_non_empty() {
        assert!(!build_tab_menu(true).is_empty());
    }

    // ── ContextMenu from make_menu is well-formed ────────────────────────────

    #[test]
    fn make_menu_initial_selection_is_selectable() {
        let m = make_menu();
        let item = &m.items[m.selected];
        assert!(item.is_selectable());
    }
}
