//! Context menu widget for right-click interactions (Phase 2).
//!
//! Provides [`ContextMenu`] — a floating, keyboard- and mouse-navigable
//! context menu rendered on top of the editor pane. Actions are represented
//! by [`MenuAction`]; separators are `MenuAction::Separator`.
//!
//! Round A: clipboard + tab management.
//! Round B: LSP actions (Go-to-def, References, Hover, Rename, Code Actions, Format).

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

// ── MenuAction ────────────────────────────────────────────────────────────────

/// Each selectable item in the context menu maps to one of these variants.
///
/// `Separator` is a non-selectable divider rendered as a horizontal rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuAction {
    // ── Clipboard ──────────────────────────────────────────────────────────
    Copy,
    Cut,
    Paste,
    // ── Tab management ─────────────────────────────────────────────────────
    TabClose,
    TabCloseOthers,
    TabCloseRight,
    TabCloseLeft,
    // ── LSP ────────────────────────────────────────────────────────────────
    LspGotoDefinition,
    LspGotoReferences,
    LspHover,
    LspRename,
    LspCodeActions,
    LspFormat,
    // ── Visual decoration ──────────────────────────────────────────────────
    /// A non-selectable horizontal separator.
    Separator,
}

// ── MenuItem ──────────────────────────────────────────────────────────────────

/// One row in the context menu.
#[derive(Clone, Debug)]
pub struct MenuItem {
    /// Display label shown to the user.
    pub label: String,
    /// The action to invoke when this item is selected.
    pub action: MenuAction,
    /// Whether the item can be selected and invoked. Disabled items are
    /// rendered in a dimmed style and skipped by keyboard navigation.
    pub enabled: bool,
    /// Optional short-cut hint shown right-aligned (e.g. `"y"`, `"Ctrl+C"`).
    pub shortcut_hint: Option<String>,
}

impl MenuItem {
    /// Convenience constructor for an enabled item.
    pub fn new(
        label: impl Into<String>,
        action: MenuAction,
        shortcut_hint: impl Into<Option<String>>,
    ) -> Self {
        Self {
            label: label.into(),
            action,
            enabled: true,
            shortcut_hint: shortcut_hint.into(),
        }
    }

    /// Convenience constructor for a separator row.
    #[allow(dead_code)]
    pub fn separator() -> Self {
        Self {
            label: String::new(),
            action: MenuAction::Separator,
            enabled: false,
            shortcut_hint: None,
        }
    }
}

// ── ContextMenu ───────────────────────────────────────────────────────────────

/// Floating context menu. Rendered on top of all other widgets.
///
/// `selected` always points to an item that is selectable (not a separator
/// and not disabled). Navigation wraps at the bottom; saturates at the top.
pub struct ContextMenu {
    /// All rows — separators included.
    pub items: Vec<MenuItem>,
    /// Index of the currently highlighted row.
    pub selected: usize,
    /// Screen position of the top-left corner (col, row).
    pub anchor: (u16, u16),
}

impl ContextMenu {
    /// Construct a new menu. The initial `selected` is the first selectable item.
    pub fn new(items: Vec<MenuItem>, anchor: (u16, u16)) -> Self {
        let selected = items
            .iter()
            .position(|it| it.enabled && it.action != MenuAction::Separator)
            .unwrap_or(0);
        Self {
            items,
            selected,
            anchor,
        }
    }

    /// Move the highlight one row up, skipping separators and disabled items.
    /// Saturates at the first selectable item (does not wrap).
    pub fn move_up(&mut self) {
        let current = self.selected;
        // Walk backward from current-1.
        if current == 0 {
            return;
        }
        for idx in (0..current).rev() {
            if self.items[idx].enabled && self.items[idx].action != MenuAction::Separator {
                self.selected = idx;
                return;
            }
        }
    }

    /// Move the highlight one row down, skipping separators and disabled items.
    /// Wraps to the first selectable item when already at the last one.
    pub fn move_down(&mut self) {
        let len = self.items.len();
        let start = self.selected + 1;
        // First: try to find a selectable item below.
        for idx in start..len {
            if self.items[idx].enabled && self.items[idx].action != MenuAction::Separator {
                self.selected = idx;
                return;
            }
        }
        // Wrap: find first selectable from top.
        for idx in 0..len {
            if self.items[idx].enabled && self.items[idx].action != MenuAction::Separator {
                self.selected = idx;
                return;
            }
        }
    }

    /// Return the action for the currently selected row, or `None` when the
    /// selected item is disabled or a separator.
    pub fn selected_action(&self) -> Option<MenuAction> {
        let item = self.items.get(self.selected)?;
        if !item.enabled || item.action == MenuAction::Separator {
            return None;
        }
        Some(item.action.clone())
    }

    /// Compute the bounding rectangle the menu occupies on screen.
    ///
    /// Used by the event loop to detect "clicked outside" mouse events so the
    /// menu can be dismissed.
    pub fn bounding_rect(&self, screen_size: Rect) -> Rect {
        let (popup_w, popup_h) = self.dimensions();
        let (ax, ay) = self.anchor;
        let x = ax.min(screen_size.x + screen_size.width.saturating_sub(popup_w));
        let y = ay.min(screen_size.y + screen_size.height.saturating_sub(popup_h));
        Rect {
            x,
            y,
            width: popup_w,
            height: popup_h,
        }
    }

    /// Compute (width, height) of the popup box (including border).
    fn dimensions(&self) -> (u16, u16) {
        let content_w = self
            .items
            .iter()
            .map(|it| {
                if it.action == MenuAction::Separator {
                    return 0u16;
                }
                let hint_len = it
                    .shortcut_hint
                    .as_deref()
                    .map(|h| h.len() + 2)
                    .unwrap_or(0);
                (it.label.len() + hint_len) as u16
            })
            .max()
            .unwrap_or(8);

        // Add 2 for left-pad + right-pad inside the border.
        let popup_w = content_w + 4;
        // One row per item, +2 for top/bottom border.
        let popup_h = self.items.len() as u16 + 2;
        (popup_w, popup_h)
    }

    /// Render the context menu as a floating box.
    ///
    /// `screen_size` should be the full terminal area so the popup can be
    /// clamped to stay fully visible.
    pub fn render(&self, frame: &mut Frame, screen_size: Rect) {
        if self.items.is_empty() {
            return;
        }

        let rect = self.bounding_rect(screen_size);

        // Clear the area below the popup first.
        frame.render_widget(Clear, rect);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Gray));
        let inner = block.inner(rect);
        frame.render_widget(block, rect);

        // Render each item row inside the border.
        let content_w = inner.width;
        for (i, item) in self.items.iter().enumerate() {
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

            if item.action == MenuAction::Separator {
                // Draw a horizontal rule.
                let sep: String = "─".repeat(content_w as usize);
                let sep_style = Style::default().fg(Color::DarkGray);
                let para = Paragraph::new(sep).style(sep_style);
                frame.render_widget(para, row_rect);
                continue;
            }

            let is_selected = i == self.selected;
            let is_disabled = !item.enabled;

            let label_style = if is_disabled {
                Style::default().fg(Color::DarkGray)
            } else if is_selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };

            // Build line: left-pad + label + gap + shortcut (right-aligned).
            let label = &item.label;
            let hint = item.shortcut_hint.as_deref().unwrap_or("");
            let hint_len = if hint.is_empty() { 0 } else { hint.len() + 2 };
            // Gap between label and hint.
            let gap = (content_w as usize).saturating_sub(label.len() + hint_len + 1); // 1 left-pad
            let line = if hint.is_empty() {
                Line::from(vec![
                    Span::raw(" "),
                    Span::styled(label.clone(), label_style),
                ])
            } else {
                let spaces = " ".repeat(gap.max(1));
                let hint_style = if is_disabled {
                    Style::default().fg(Color::DarkGray)
                } else if is_selected {
                    Style::default().fg(Color::DarkGray).bg(Color::White)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                Line::from(vec![
                    Span::raw(" "),
                    Span::styled(label.clone(), label_style),
                    Span::raw(spaces),
                    Span::styled(hint.to_string(), hint_style),
                ])
            };

            let para = Paragraph::new(line).style(if is_selected {
                Style::default().bg(Color::White)
            } else {
                Style::default()
            });
            frame.render_widget(para, row_rect);
        }
    }
}

// ── Menu builder helpers ──────────────────────────────────────────────────────

/// Build the context menu for a right-click in the Code or Gutter zone.
///
/// Cut / Copy are enabled only when a visual selection is active (`has_sel`).
/// LSP items are shown but disabled when no language server is attached to
/// the active buffer (`has_lsp = false`).
pub fn build_code_menu(has_sel: bool, has_lsp: bool) -> Vec<MenuItem> {
    vec![
        // ── Clipboard ──────────────────────────────────────────────────────
        MenuItem {
            label: "Cut".into(),
            action: MenuAction::Cut,
            enabled: has_sel,
            shortcut_hint: Some("x".into()),
        },
        MenuItem {
            label: "Copy".into(),
            action: MenuAction::Copy,
            enabled: has_sel,
            shortcut_hint: Some("y".into()),
        },
        MenuItem::new("Paste", MenuAction::Paste, Some("p".into())),
        // ── Separator ──────────────────────────────────────────────────────
        MenuItem::separator(),
        // ── LSP: navigation ────────────────────────────────────────────────
        MenuItem {
            label: "Go to Definition".into(),
            action: MenuAction::LspGotoDefinition,
            enabled: has_lsp,
            shortcut_hint: Some("gd".into()),
        },
        MenuItem {
            label: "Go to References".into(),
            action: MenuAction::LspGotoReferences,
            enabled: has_lsp,
            shortcut_hint: Some("gr".into()),
        },
        MenuItem {
            label: "Hover".into(),
            action: MenuAction::LspHover,
            enabled: has_lsp,
            shortcut_hint: Some("K".into()),
        },
        // ── Separator ──────────────────────────────────────────────────────
        MenuItem::separator(),
        // ── LSP: edits ─────────────────────────────────────────────────────
        MenuItem {
            label: "Rename Symbol".into(),
            action: MenuAction::LspRename,
            enabled: has_lsp,
            shortcut_hint: Some("<leader>rn".into()),
        },
        MenuItem {
            label: "Code Actions".into(),
            action: MenuAction::LspCodeActions,
            enabled: has_lsp,
            shortcut_hint: Some("<leader>ca".into()),
        },
        MenuItem {
            label: "Format Document".into(),
            action: MenuAction::LspFormat,
            enabled: has_lsp,
            shortcut_hint: Some(":LspFormat".into()),
        },
    ]
}

/// Build the context menu for a right-click on the tab bar.
pub fn build_tab_menu(more_than_one_tab: bool) -> Vec<MenuItem> {
    vec![
        MenuItem {
            label: "Close Tab".into(),
            action: MenuAction::TabClose,
            enabled: more_than_one_tab,
            shortcut_hint: None,
        },
        MenuItem {
            label: "Close Other Tabs".into(),
            action: MenuAction::TabCloseOthers,
            enabled: more_than_one_tab,
            shortcut_hint: None,
        },
        MenuItem {
            label: "Close Tabs to the Right".into(),
            action: MenuAction::TabCloseRight,
            enabled: more_than_one_tab,
            shortcut_hint: None,
        },
        MenuItem {
            label: "Close Tabs to the Left".into(),
            action: MenuAction::TabCloseLeft,
            enabled: more_than_one_tab,
            shortcut_hint: None,
        },
    ]
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_menu() -> ContextMenu {
        let items = vec![
            MenuItem::new("Cut", MenuAction::Cut, None),
            MenuItem::new("Copy", MenuAction::Copy, None),
            MenuItem::separator(),
            MenuItem::new("Paste", MenuAction::Paste, None),
        ];
        ContextMenu::new(items, (0, 0))
    }

    // ── move_down skips separators ──────────────────────────────────────────

    #[test]
    fn move_down_skips_separator() {
        // Items: Cut(0), Copy(1), Sep(2), Paste(3).
        // Start at Copy(1), move_down should land on Paste(3).
        let mut m = make_menu();
        m.selected = 1;
        m.move_down();
        assert_eq!(m.selected, 3, "expected Paste at idx 3, got {}", m.selected);
    }

    // ── move_up from top saturates ──────────────────────────────────────────

    #[test]
    fn move_up_from_top_saturates() {
        let mut m = make_menu();
        m.selected = 0;
        m.move_up();
        assert_eq!(m.selected, 0, "should saturate at 0");
    }

    // ── move_down from bottom wraps ─────────────────────────────────────────

    #[test]
    fn move_down_from_bottom_wraps_to_top() {
        let mut m = make_menu();
        m.selected = 3; // Paste (last selectable)
        m.move_down();
        assert_eq!(m.selected, 0, "should wrap to Cut at idx 0");
    }

    // ── selected_action returns correct variant ─────────────────────────────

    #[test]
    fn selected_action_copy() {
        let mut m = make_menu();
        m.selected = 1; // Copy
        assert_eq!(m.selected_action(), Some(MenuAction::Copy));
    }

    #[test]
    fn selected_action_separator_is_none() {
        let mut m = make_menu();
        m.selected = 2; // Separator
        assert_eq!(m.selected_action(), None);
    }

    #[test]
    fn selected_action_disabled_is_none() {
        let items = vec![MenuItem {
            label: "Cut".into(),
            action: MenuAction::Cut,
            enabled: false,
            shortcut_hint: None,
        }];
        let m = ContextMenu::new(items, (0, 0));
        assert_eq!(m.selected_action(), None);
    }

    // ── initial selected skips leading separators ───────────────────────────

    #[test]
    fn initial_selected_skips_separator() {
        let items = vec![
            MenuItem::separator(),
            MenuItem::new("Copy", MenuAction::Copy, None),
        ];
        let m = ContextMenu::new(items, (0, 0));
        assert_eq!(m.selected, 1);
    }

    // ── build_code_menu ─────────────────────────────────────────────────────

    #[test]
    fn build_code_menu_with_selection_enables_cut_copy() {
        let items = build_code_menu(true, false);
        assert!(items[0].enabled); // Cut
        assert!(items[1].enabled); // Copy
        assert!(items[2].enabled); // Paste
    }

    #[test]
    fn build_code_menu_no_selection_disables_cut_copy() {
        let items = build_code_menu(false, false);
        assert!(!items[0].enabled); // Cut
        assert!(!items[1].enabled); // Copy
        assert!(items[2].enabled); // Paste always enabled
    }

    // ── build_code_menu LSP items ───────────────────────────────────────────

    /// All 6 LSP items are present and enabled when `has_lsp = true`.
    #[test]
    fn build_code_menu_includes_lsp_items_when_lsp_attached() {
        let items = build_code_menu(false, true);
        let lsp_actions = [
            MenuAction::LspGotoDefinition,
            MenuAction::LspGotoReferences,
            MenuAction::LspHover,
            MenuAction::LspRename,
            MenuAction::LspCodeActions,
            MenuAction::LspFormat,
        ];
        for action in &lsp_actions {
            let item = items
                .iter()
                .find(|it| &it.action == action)
                .unwrap_or_else(|| panic!("{action:?} not found in menu"));
            assert!(
                item.enabled,
                "{action:?} should be enabled when has_lsp=true"
            );
        }
    }

    /// All 6 LSP items are present but disabled when `has_lsp = false`.
    #[test]
    fn build_code_menu_disables_lsp_items_when_no_lsp() {
        let items = build_code_menu(false, false);
        let lsp_actions = [
            MenuAction::LspGotoDefinition,
            MenuAction::LspGotoReferences,
            MenuAction::LspHover,
            MenuAction::LspRename,
            MenuAction::LspCodeActions,
            MenuAction::LspFormat,
        ];
        for action in &lsp_actions {
            let item = items
                .iter()
                .find(|it| &it.action == action)
                .unwrap_or_else(|| panic!("{action:?} not found in menu"));
            assert!(
                !item.enabled,
                "{action:?} should be disabled when has_lsp=false"
            );
        }
    }

    /// Menu order: Cut/Copy/Paste, sep, GotoDef/GotoRef/Hover, sep,
    /// Rename/CodeActions/Format.
    #[test]
    fn build_code_menu_separator_layout() {
        let items = build_code_menu(true, true);
        // Flatten to action sequence ignoring separators, then re-check positions.
        let expected_order = [
            MenuAction::Cut,
            MenuAction::Copy,
            MenuAction::Paste,
            MenuAction::LspGotoDefinition,
            MenuAction::LspGotoReferences,
            MenuAction::LspHover,
            MenuAction::LspRename,
            MenuAction::LspCodeActions,
            MenuAction::LspFormat,
        ];
        let non_sep: Vec<&MenuAction> = items
            .iter()
            .filter(|it| it.action != MenuAction::Separator)
            .map(|it| &it.action)
            .collect();
        assert_eq!(non_sep.len(), expected_order.len());
        for (got, want) in non_sep.iter().zip(expected_order.iter()) {
            assert_eq!(*got, want, "order mismatch");
        }

        // Verify two separators exist and that they appear after Paste and after Hover.
        let sep_positions: Vec<usize> = items
            .iter()
            .enumerate()
            .filter(|(_, it)| it.action == MenuAction::Separator)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(sep_positions.len(), 2, "expected exactly 2 separators");
        // sep after Paste (index 2), so sep at index 3.
        assert_eq!(items[sep_positions[0]].action, MenuAction::Separator);
        assert_eq!(items[sep_positions[0] - 1].action, MenuAction::Paste);
        // sep after Hover, before Rename.
        assert_eq!(items[sep_positions[1] + 1].action, MenuAction::LspRename);
    }

    // ── build_tab_menu ──────────────────────────────────────────────────────

    #[test]
    fn build_tab_menu_single_tab_disables_close() {
        let items = build_tab_menu(false);
        assert!(!items[0].enabled); // Close Tab
    }

    #[test]
    fn build_tab_menu_multi_tab_enables_close() {
        let items = build_tab_menu(true);
        assert!(items[0].enabled); // Close Tab
        assert!(items[1].enabled); // Close Other Tabs
    }
}
