//! Floem adapter for `hjkl-menu`.
//!
//! Renders a [`ContextMenu`] as a floem view: a floating popup positioned at
//! the menu's anchor, styled per-row via [`MenuThemeGui`]. Mirrors the
//! structure of the ratatui adapter `hjkl-menu-tui` — a thin
//! [`IntoView`](floem::IntoView) builder ([`menu_view`]) delegates all row
//! logic to a pure, unit-tested helper ([`menu_rows`]) so the display logic
//! can be tested without a running floem application.
//!
//! # Quick start
//!
//! ```rust,no_run
//! use floem::reactive::RwSignal;
//! use hjkl_menu::{build_code_menu, ContextMenu};
//! use hjkl_menu_gui::{menu_view, MenuThemeGui};
//!
//! let items = build_code_menu(true, true);
//! let menu = RwSignal::new(ContextMenu::new(items, (10, 5)));
//! let _view = menu_view(menu, MenuThemeGui::default());
//! ```

#![forbid(unsafe_code)]

use floem::{
    IntoView,
    peniko::Color,
    reactive::{RwSignal, SignalUpdate, SignalWith},
    views::{Decorators, container, dyn_stack, empty, stack},
};
use hjkl_menu::{ContextMenu, MenuAction};

// ── MenuThemeGui ────────────────────────────────────────────────────────────

/// Colour configuration for [`menu_view`].
///
/// `#[non_exhaustive]` — new colour slots may be added in minor releases
/// without breaking existing construction sites (use [`MenuThemeGui::new`] or
/// [`MenuThemeGui::default`] + field mutation).
#[non_exhaustive]
#[derive(Debug, Clone, Copy)]
pub struct MenuThemeGui {
    /// Foreground for normal (not selected, not disabled) rows.
    pub normal_fg: Color,
    /// Background for normal rows and the popup itself.
    pub normal_bg: Color,
    /// Foreground for the highlighted row.
    pub selected_fg: Color,
    /// Background for the highlighted row.
    pub selected_bg: Color,
    /// Colour of separator rules.
    pub separator_fg: Color,
    /// Foreground for disabled/inert rows.
    pub disabled_fg: Color,
}

impl MenuThemeGui {
    /// Build a theme from explicit colour values.
    ///
    /// ```rust
    /// use floem::peniko::Color;
    /// use hjkl_menu_gui::MenuThemeGui;
    ///
    /// let theme = MenuThemeGui::new(
    ///     Color::rgb8(0, 0, 0),
    ///     Color::rgb8(1, 1, 1),
    ///     Color::rgb8(2, 2, 2),
    ///     Color::rgb8(3, 3, 3),
    ///     Color::rgb8(4, 4, 4),
    ///     Color::rgb8(5, 5, 5),
    /// );
    /// assert_eq!(theme.normal_fg, Color::rgb8(0, 0, 0));
    /// ```
    pub fn new(
        normal_fg: Color,
        normal_bg: Color,
        selected_fg: Color,
        selected_bg: Color,
        separator_fg: Color,
        disabled_fg: Color,
    ) -> Self {
        Self {
            normal_fg,
            normal_bg,
            selected_fg,
            selected_bg,
            separator_fg,
            disabled_fg,
        }
    }
}

impl Default for MenuThemeGui {
    fn default() -> Self {
        Self {
            normal_fg: Color::rgb8(0xcd, 0xd6, 0xf4), // Catppuccin Mocha text
            normal_bg: Color::rgb8(0x1e, 0x1e, 0x2e), // base
            selected_fg: Color::rgb8(0x1e, 0x1e, 0x2e),
            selected_bg: Color::rgb8(0xcd, 0xd6, 0xf4),
            separator_fg: Color::rgb8(0x45, 0x47, 0x5a), // surface1
            disabled_fg: Color::rgb8(0x6c, 0x70, 0x86),  // overlay0
        }
    }
}

// ── Pure display-logic helpers ───────────────────────────────────────────────

/// A single menu row as displayed by [`menu_view`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuRow {
    /// Display label. Empty for separator rows.
    pub label: String,
    /// Whether this row is the currently-highlighted row.
    pub selected: bool,
    /// Whether this row is a non-selectable horizontal separator.
    pub separator: bool,
    /// Whether this row can be clicked/selected (mirrors
    /// [`MenuItem::is_selectable`]: `false` for separators, info headers,
    /// and disabled items).
    pub enabled: bool,
}

/// Derive the display rows for `menu`.
///
/// This is a thin, pure mapping over [`ContextMenu::items`] — it performs no
/// layout and touches no floem types, so it can be unit-tested without a
/// running floem application.
///
/// ```rust
/// use hjkl_menu::{ContextMenu, MenuItem, MenuAction};
/// use hjkl_menu_gui::menu_rows;
///
/// let items = vec![
///     MenuItem::new("Copy", MenuAction::Copy, None),
///     MenuItem::separator(),
///     MenuItem::new("Paste", MenuAction::Paste, None),
/// ];
/// let menu = ContextMenu::new(items, (0, 0));
/// let rows = menu_rows(&menu);
/// assert_eq!(rows.len(), 3);
/// assert!(rows[0].selected);
/// assert!(rows[1].separator);
/// assert!(!rows[1].enabled);
/// ```
pub fn menu_rows(menu: &ContextMenu) -> Vec<MenuRow> {
    menu.items
        .iter()
        .enumerate()
        .map(|(idx, item)| MenuRow {
            label: item.label.clone(),
            selected: idx == menu.selected,
            separator: item.action == MenuAction::Separator,
            enabled: item.is_selectable(),
        })
        .collect()
}

// ── Layout constants ──────────────────────────────────────────────────────────

/// Pixel width of one `ContextMenu` layout column.
///
/// [`hjkl_menu::ContextMenu`] measures geometry in terminal-style character
/// cells (inherited from the ratatui adapter's model). Floem lays out with
/// real pixel coordinates, so this constant is a fixed-scale stand-in for
/// slice 1 rather than a measured glyph width. A future slice can replace it
/// with a value derived from the active font/theme.
const CHAR_WIDTH_PX: f64 = 8.0;

/// Pixel height of one `ContextMenu` layout row. See [`CHAR_WIDTH_PX`].
const ROW_HEIGHT_PX: f64 = 20.0;

/// Fallback screen size in cells used to clamp the popup on-screen via
/// [`ContextMenu::bounding_rect`] when the caller has no real viewport size
/// handy. Mirrors `hjkl-tabs-gui`'s `DEFAULT_MAX_WIDTH` stand-in; a future
/// slice can replace this with a reactive size derived from the window.
const DEFAULT_SCREEN_COLS: u16 = 240;
const DEFAULT_SCREEN_ROWS: u16 = 67;

// ── menu_view ─────────────────────────────────────────────────────────────────

/// Build a floem view for the context menu, including click-outside
/// dismissal.
///
/// `menu` is a reactive signal owned by the caller. Clicking a selectable row
/// updates `menu.selected` to that row's index; callers read
/// [`ContextMenu::selected_action`] afterwards to dispatch the action.
/// Clicking anywhere outside the popup clears `menu.items`, which is the
/// convention this crate uses to mean "menu closed" (an empty menu renders
/// nothing further up the caller's view tree).
///
/// The view itself contains no row-selection logic — everything renderable
/// is computed by [`menu_rows`], which mirrors `hjkl-menu-tui::render`.
pub fn menu_view(menu: RwSignal<ContextMenu>, theme: MenuThemeGui) -> impl IntoView {
    let rows = dyn_stack(
        move || menu.with(|m| menu_rows(m).into_iter().enumerate().collect::<Vec<_>>()),
        |(idx, _row): &(usize, MenuRow)| *idx,
        move |(idx, row): (usize, MenuRow)| {
            if row.separator {
                return empty()
                    .style(move |s| {
                        s.width_full()
                            .height(1.0)
                            .margin_vert(4.0)
                            .background(theme.separator_fg)
                    })
                    .into_any();
            }

            let label_text = row.label.clone();
            let (fg, bg) = if !row.enabled {
                (theme.disabled_fg, theme.normal_bg)
            } else if row.selected {
                (theme.selected_fg, theme.selected_bg)
            } else {
                (theme.normal_fg, theme.normal_bg)
            };

            floem::views::label(move || label_text.clone())
                .style(move |s| {
                    s.width_full()
                        .padding_horiz(8.0)
                        .padding_vert(4.0)
                        .color(fg)
                        .background(bg)
                })
                .on_click_stop(move |_| {
                    if row.enabled {
                        menu.update(|m| m.selected = idx);
                    }
                })
                .into_any()
        },
    )
    .style(|s| s.flex_col().width_full());

    let popup = container(rows).style(move |s| {
        let (x, y, w, h) = menu.with(|m| m.bounding_rect(DEFAULT_SCREEN_COLS, DEFAULT_SCREEN_ROWS));
        s.absolute()
            .z_index(1000)
            .inset_left(f64::from(x) * CHAR_WIDTH_PX)
            .inset_top(f64::from(y) * ROW_HEIGHT_PX)
            .width(f64::from(w) * CHAR_WIDTH_PX)
            .height(f64::from(h) * ROW_HEIGHT_PX)
            .background(theme.normal_bg)
            .border(1.0)
            .border_color(theme.separator_fg)
    });

    // Click-outside dismissal: a full-bleed backdrop rendered behind the
    // popup (lower z-index) that clears the menu on click. The backdrop only
    // intercepts clicks — it is visually transparent.
    let backdrop = empty()
        .style(move |s| {
            s.absolute()
                .z_index(999)
                .inset_left(0.0)
                .inset_top(0.0)
                .width_full()
                .height_full()
        })
        .on_click_stop(move |_| {
            menu.update(|m| m.items.clear());
        });

    stack((backdrop, popup))
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use hjkl_menu::{ContextMenu, MenuAction, MenuItem};

    fn make_menu() -> ContextMenu {
        // `MenuItem` is `#[non_exhaustive]`, so a disabled non-separator item
        // is built via `new` (always `enabled: true`) then a field mutation —
        // mutating an already-owned instance's pub fields is fine; only
        // struct-literal construction is blocked outside the defining crate.
        let mut paste = MenuItem::new("Paste", MenuAction::Paste, None);
        paste.enabled = false;

        let items = vec![
            MenuItem::new("Cut", MenuAction::Cut, None),
            MenuItem::new("Copy", MenuAction::Copy, None),
            MenuItem::separator(),
            paste,
        ];
        ContextMenu::new(items, (0, 0))
    }

    // ── menu_rows: labels ────────────────────────────────────────────────────

    #[test]
    fn row_labels_match_item_labels() {
        let m = make_menu();
        let rows = menu_rows(&m);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].label, "Cut");
        assert_eq!(rows[1].label, "Copy");
        assert_eq!(rows[2].label, "");
        assert_eq!(rows[3].label, "Paste");
    }

    // ── menu_rows: selected tracks ContextMenu::selected ────────────────────

    #[test]
    fn initial_selected_row_is_first_selectable() {
        let m = make_menu();
        let rows = menu_rows(&m);
        assert!(rows[0].selected, "Cut is the first selectable item");
        assert!(!rows[1].selected);
        assert!(!rows[2].selected);
        assert!(!rows[3].selected);
    }

    #[test]
    fn selected_row_tracks_move_down() {
        let mut m = make_menu();
        m.move_down(); // Cut -> Copy
        let rows = menu_rows(&m);
        assert!(!rows[0].selected);
        assert!(rows[1].selected, "Copy should be selected after move_down");
    }

    #[test]
    fn move_down_skips_separator_and_disabled_paste() {
        let mut m = make_menu();
        m.move_down(); // Cut -> Copy
        m.move_down(); // Copy -> wraps back to Cut (Sep + disabled Paste skipped)
        let rows = menu_rows(&m);
        assert!(
            rows[0].selected,
            "should wrap back to Cut, skipping the separator and disabled Paste"
        );
    }

    // ── menu_rows: separator rows ────────────────────────────────────────────

    #[test]
    fn separator_row_is_marked_separator_and_not_enabled() {
        let m = make_menu();
        let rows = menu_rows(&m);
        assert!(rows[2].separator);
        assert!(!rows[2].enabled);
        assert!(!rows[2].selected);
    }

    #[test]
    fn non_separator_rows_are_not_marked_separator() {
        let m = make_menu();
        let rows = menu_rows(&m);
        assert!(!rows[0].separator);
        assert!(!rows[1].separator);
        assert!(!rows[3].separator);
    }

    // ── menu_rows: disabled/inert items ──────────────────────────────────────

    #[test]
    fn disabled_item_is_not_enabled() {
        let m = make_menu();
        let rows = menu_rows(&m);
        assert!(!rows[3].enabled, "Paste was constructed with enabled=false");
    }

    #[test]
    fn enabled_selectable_items_are_enabled() {
        let m = make_menu();
        let rows = menu_rows(&m);
        assert!(rows[0].enabled);
        assert!(rows[1].enabled);
    }

    #[test]
    fn info_header_is_not_enabled_even_if_technically_enabled_field() {
        // `MenuItem::new` always sets `enabled: true`, which is exactly the
        // "technically enabled" case this test guards against: `Info` rows
        // must still be treated as inert by `menu_rows`.
        let items = vec![MenuItem::new("Filetype: rust", MenuAction::Info, None)];
        let m = ContextMenu::new(items, (0, 0));
        let rows = menu_rows(&m);
        assert!(
            !rows[0].enabled,
            "Info rows are inert regardless of the enabled field"
        );
        assert!(!rows[0].separator, "Info rows are not separators");
    }

    #[test]
    fn empty_menu_has_no_rows() {
        let m = ContextMenu::new(vec![], (0, 0));
        assert!(menu_rows(&m).is_empty());
    }

    // ── MenuThemeGui ─────────────────────────────────────────────────────────

    #[test]
    fn theme_default_constructs() {
        let t = MenuThemeGui::default();
        assert_eq!(t.selected_bg, Color::rgb8(0xcd, 0xd6, 0xf4));
    }

    #[test]
    fn theme_new_roundtrip() {
        let t = MenuThemeGui::new(
            Color::rgb8(1, 2, 3),
            Color::rgb8(4, 5, 6),
            Color::rgb8(7, 8, 9),
            Color::rgb8(10, 11, 12),
            Color::rgb8(13, 14, 15),
            Color::rgb8(16, 17, 18),
        );
        assert_eq!(t.normal_fg, Color::rgb8(1, 2, 3));
        assert_eq!(t.normal_bg, Color::rgb8(4, 5, 6));
        assert_eq!(t.selected_fg, Color::rgb8(7, 8, 9));
        assert_eq!(t.selected_bg, Color::rgb8(10, 11, 12));
        assert_eq!(t.separator_fg, Color::rgb8(13, 14, 15));
        assert_eq!(t.disabled_fg, Color::rgb8(16, 17, 18));
    }

    // ── menu_view (smoke: must construct without panicking) ─────────────────

    #[test]
    fn menu_view_constructs() {
        let menu = RwSignal::new(make_menu());
        let _view = menu_view(menu, MenuThemeGui::default());
    }

    #[test]
    fn menu_view_constructs_for_empty_menu() {
        let menu = RwSignal::new(ContextMenu::new(vec![], (0, 0)));
        let _view = menu_view(menu, MenuThemeGui::default());
    }
}
