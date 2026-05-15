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
    // ── Phase 7: status-line menu ──────────────────────────────────────────
    /// Restart the LSP server for the current buffer.
    LspRestart,
    /// Open the file picker (`<leader><space>`).
    OpenFilePicker,
    // ── Phase 7: split-border menu ─────────────────────────────────────────
    /// Equalize all splits to 0.5 ratio.
    WindowEqualize,
    /// Close the focused window (`:close`).
    WindowClose,
    // ── Phase 8: picker overlay menu ───────────────────────────────────────
    /// Open the focused picker row (same as Enter).
    PickerOpen,
    /// Open the focused picker row in a horizontal split.
    PickerOpenSplit,
    /// Open the focused picker row in a vertical split.
    PickerOpenVSplit,
    /// Open the focused picker row in a new tab.
    PickerOpenTab,
    /// Copy the focused picker row's path to the system clipboard.
    PickerCopyPath,
    // ── Visual decoration ──────────────────────────────────────────────────
    /// A non-selectable horizontal separator.
    Separator,
    /// A non-selectable informational header label (rendered as dimmed text,
    /// not as a horizontal rule). Used for status-line menu headers like
    /// "Filetype: rust" and "LSP: rust-analyzer".
    Info,
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

/// Build the context menu for a right-click on the status line.
///
/// `ft` is the file-type label (e.g. `"rust"`, `"(none)"`).
/// `lsp_name` is `Some("rust-analyzer")` when a server is attached,
/// `None` otherwise.
pub fn build_status_line_menu(ft: &str, lsp_name: Option<&str>) -> Vec<MenuItem> {
    let ft_label = format!("Filetype: {ft}");
    let lsp_label = match lsp_name {
        Some(name) => format!("LSP: {name}"),
        None => "LSP: (none)".to_string(),
    };
    let has_lsp = lsp_name.is_some();
    vec![
        // Header: filetype info — not selectable.
        MenuItem {
            label: ft_label,
            action: MenuAction::Info,
            enabled: false,
            shortcut_hint: None,
        },
        // Header: LSP server info — not selectable.
        MenuItem {
            label: lsp_label,
            action: MenuAction::Info,
            enabled: false,
            shortcut_hint: None,
        },
        MenuItem::separator(),
        MenuItem {
            label: "Restart LSP".into(),
            action: MenuAction::LspRestart,
            enabled: has_lsp,
            shortcut_hint: None,
        },
        MenuItem::separator(),
        MenuItem::new("Open File…", MenuAction::OpenFilePicker, None),
    ]
}

/// Build the context menu for a right-click on a split border.
pub fn build_split_border_menu() -> Vec<MenuItem> {
    vec![
        MenuItem::new("Equalize Splits", MenuAction::WindowEqualize, None),
        MenuItem::new("Close This Window", MenuAction::WindowClose, None),
    ]
}

/// Build the context menu for a right-click on a picker overlay row.
///
/// `has_path` controls whether the split/tab/copy-path items are enabled.
/// When the focused row has no file-system path (e.g. a git-log entry),
/// those items are shown but disabled.
pub fn build_picker_menu(has_path: bool) -> Vec<MenuItem> {
    vec![
        MenuItem::new("Open", MenuAction::PickerOpen, Some("Enter".into())),
        MenuItem {
            label: "Open in Horizontal Split".into(),
            action: MenuAction::PickerOpenSplit,
            enabled: has_path,
            shortcut_hint: None,
        },
        MenuItem {
            label: "Open in Vertical Split".into(),
            action: MenuAction::PickerOpenVSplit,
            enabled: has_path,
            shortcut_hint: None,
        },
        MenuItem {
            label: "Open in New Tab".into(),
            action: MenuAction::PickerOpenTab,
            enabled: has_path,
            shortcut_hint: None,
        },
        MenuItem::separator(),
        MenuItem {
            label: "Copy Path".into(),
            action: MenuAction::PickerCopyPath,
            enabled: has_path,
            shortcut_hint: None,
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

    // ── bounding_rect screen clamping ───────────────────────────────────────

    /// Regression: when the menu is anchored near the bottom of the screen and
    /// the popup doesn't fit below the anchor, `bounding_rect` must shift the
    /// popup upward so it stays fully visible. The previous Moved-handler bug
    /// used a fake `screen_size` (`anchor + slack`) that artificially extended
    /// the screen below the anchor — so the popup never flipped and the
    /// hover-to-item mapping read the wrong rows.
    #[test]
    fn bounding_rect_anchored_near_bottom_flips_upward() {
        // 24-row screen, anchor at row 22, popup has 6 items + 2 border = 8 rows.
        // Popup height 8 > 24 - 22 = 2 rows remaining, must shift up.
        let items: Vec<MenuItem> = (0..6)
            .map(|i| MenuItem::new(format!("Item {i}"), MenuAction::Paste, None))
            .collect();
        let m = ContextMenu::new(items, (5, 22));
        let screen = Rect::new(0, 0, 80, 24);
        let rect = m.bounding_rect(screen);

        let (_, popup_h) = (rect.width, rect.height);
        assert_eq!(
            popup_h, 8,
            "popup height = 6 items + 2 border rows; got {popup_h}"
        );
        // Popup must FIT inside the screen.
        assert!(
            rect.y + rect.height <= screen.height,
            "bottom edge of popup ({}+{}={}) must not exceed screen height ({}); rect={rect:?}",
            rect.y,
            rect.height,
            rect.y + rect.height,
            screen.height,
        );
        // And must have shifted up from the anchor.
        assert!(
            rect.y < 22,
            "anchor was at y=22 but rect.y={} — popup did not flip upward",
            rect.y,
        );
        assert_eq!(rect.y, 24 - 8, "expected popup to sit flush with bottom");
    }

    /// Mirror: anchor near the RIGHT edge should shift the popup leftward
    /// rather than letting it overflow the screen.
    #[test]
    fn bounding_rect_anchored_near_right_shifts_left() {
        let items = vec![
            MenuItem::new("Reasonably Long Item Label", MenuAction::Paste, None),
            MenuItem::new("Another Long Item Label", MenuAction::Copy, None),
        ];
        let m = ContextMenu::new(items, (75, 5));
        let screen = Rect::new(0, 0, 80, 24);
        let rect = m.bounding_rect(screen);

        assert!(
            rect.x + rect.width <= screen.width,
            "right edge {} must not exceed screen width {}; rect={rect:?}",
            rect.x + rect.width,
            screen.width,
        );
        assert!(
            rect.x < 75,
            "popup must have shifted left from anchor=75; got rect.x={}",
            rect.x,
        );
    }

    /// Hover-on-item invariant: for a popup that has been shifted upward, the
    /// row→item-index math the Moved handler uses (`row - rect.y - 1`) must
    /// produce the right index for the row that visually corresponds to each
    /// item. Pre-fix the Moved handler computed `rect` from a bogus screen
    /// size so this index was off by the flip amount.
    #[test]
    fn row_to_item_index_correct_for_flipped_popup() {
        let items: Vec<MenuItem> = (0..4)
            .map(|i| MenuItem::new(format!("Item {i}"), MenuAction::Paste, None))
            .collect();
        let m = ContextMenu::new(items, (5, 22));
        let screen = Rect::new(0, 0, 80, 24);
        let rect = m.bounding_rect(screen);

        // 4 items + 2 border = 6 rows; rect.y = 24-6 = 18.
        // Item 0 visually sits at row 19 (rect.y + 1, just below the border).
        assert_eq!(rect.y, 18);
        let row0 = rect.y + 1;
        let row3 = rect.y + 4;
        // The Moved handler maps mouse row → item index as `row - rect.y - 1`.
        assert_eq!((row0 - rect.y - 1) as usize, 0);
        assert_eq!((row3 - rect.y - 1) as usize, 3);
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

    // ── build_status_line_menu (Phase 7) ─────────────────────────────────────

    /// The first item's label must contain the filetype string.
    #[test]
    fn build_status_line_menu_includes_filetype_info() {
        let items = build_status_line_menu("rust", Some("rust-analyzer"));
        let ft_item = &items[0];
        assert!(
            ft_item.label.contains("rust"),
            "first item label should contain 'rust', got {:?}",
            ft_item.label
        );
        assert!(
            !ft_item.enabled,
            "filetype info item must not be selectable"
        );
        assert_eq!(
            ft_item.action,
            MenuAction::Info,
            "filetype item uses Info action"
        );
    }

    /// LSP header item (index 1) must reflect the server name when provided.
    #[test]
    fn build_status_line_menu_lsp_name_shown() {
        let items = build_status_line_menu("rust", Some("rust-analyzer"));
        let lsp_item = &items[1];
        assert!(
            lsp_item.label.contains("rust-analyzer"),
            "lsp item label should contain server name, got {:?}",
            lsp_item.label
        );
    }

    /// Restart LSP is disabled when no server is attached.
    #[test]
    fn build_status_line_menu_restart_disabled_when_no_lsp() {
        let items = build_status_line_menu("(none)", None);
        let restart = items
            .iter()
            .find(|it| it.action == MenuAction::LspRestart)
            .expect("LspRestart item must exist");
        assert!(
            !restart.enabled,
            "LspRestart should be disabled when lsp_name is None"
        );
    }

    /// Restart LSP is enabled when a server is attached.
    #[test]
    fn build_status_line_menu_restart_enabled_when_lsp_present() {
        let items = build_status_line_menu("rust", Some("rust-analyzer"));
        let restart = items
            .iter()
            .find(|it| it.action == MenuAction::LspRestart)
            .expect("LspRestart item must exist");
        assert!(
            restart.enabled,
            "LspRestart should be enabled when lsp_name is Some"
        );
    }

    /// Open File… is always enabled.
    #[test]
    fn build_status_line_menu_open_file_always_enabled() {
        let items = build_status_line_menu("(none)", None);
        let open = items
            .iter()
            .find(|it| it.action == MenuAction::OpenFilePicker)
            .expect("OpenFilePicker item must exist");
        assert!(open.enabled, "Open File… should always be enabled");
    }

    // ── build_split_border_menu (Phase 7) ────────────────────────────────────

    /// The split-border menu must contain exactly Equalize and Close (no stubs).
    #[test]
    fn build_split_border_menu_has_equalize_and_close() {
        let items = build_split_border_menu();
        let non_sep: Vec<&MenuItem> = items
            .iter()
            .filter(|it| it.action != MenuAction::Separator && it.action != MenuAction::Info)
            .collect();
        assert_eq!(
            non_sep.len(),
            2,
            "expected exactly 2 real items, got {:?}",
            non_sep.iter().map(|it| &it.action).collect::<Vec<_>>()
        );
        assert_eq!(non_sep[0].action, MenuAction::WindowEqualize);
        assert_eq!(non_sep[1].action, MenuAction::WindowClose);
        assert!(non_sep[0].enabled);
        assert!(non_sep[1].enabled);
    }

    // ── build_picker_menu (Phase 8) ──────────────────────────────────────────

    /// When has_path=true all items are enabled.
    #[test]
    fn build_picker_menu_all_enabled_when_has_path() {
        let items = build_picker_menu(true);
        for it in &items {
            if it.action == MenuAction::Separator {
                continue;
            }
            assert!(
                it.enabled,
                "{:?} should be enabled when has_path=true",
                it.action
            );
        }
    }

    /// When has_path=false, split/tab/copy items are disabled; Open is always enabled.
    #[test]
    fn build_picker_menu_disables_path_items_when_no_path() {
        let items = build_picker_menu(false);
        // Open (Enter) always enabled.
        let open = items
            .iter()
            .find(|it| it.action == MenuAction::PickerOpen)
            .unwrap();
        assert!(open.enabled, "PickerOpen should always be enabled");

        // Path-dependent items must be disabled.
        for action in &[
            MenuAction::PickerOpenSplit,
            MenuAction::PickerOpenVSplit,
            MenuAction::PickerOpenTab,
            MenuAction::PickerCopyPath,
        ] {
            let item = items
                .iter()
                .find(|it| &it.action == action)
                .unwrap_or_else(|| panic!("{action:?} not found"));
            assert!(
                !item.enabled,
                "{action:?} should be disabled when has_path=false"
            );
        }
    }
}
