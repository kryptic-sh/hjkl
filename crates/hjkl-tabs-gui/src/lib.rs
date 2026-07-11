//! Floem adapter for [`hjkl_tabs`].
//!
//! Renders a [`TabBar<Id>`] as a floem row of tab chips with:
//!
//! - Active tab: bold-weight text on an accent background.
//! - Dirty marker: `●` prefix on modified tabs (via [`Tab::display_label`]).
//! - Overflow indicators: `<` / `>` chips shown when tabs are hidden to the
//!   left / right.
//! - Click-to-focus: clicking a tab chip calls [`TabBar::focus`] on the
//!   backing signal.
//!
//! Mirrors the structure of the ratatui adapter `hjkl-tabs-tui` — a thin
//! [`IntoView`] builder ([`tab_bar_view`]) delegates all layout decisions to
//! [`hjkl_tabs::TabBar::visible`] and a pure, unit-tested helper
//! ([`tab_cells`]) so the logic can be tested without a running floem
//! application.
//!
//! # Quick start
//!
//! ```rust,no_run
//! use floem::reactive::RwSignal;
//! use hjkl_tabs::TabBar;
//! use hjkl_tabs_gui::{TabBarThemeGui, tab_bar_view};
//!
//! let mut bar: TabBar<u32> = TabBar::new();
//! bar.open(1, "main.rs".to_string());
//! let bar = RwSignal::new(bar);
//! let _view = tab_bar_view(bar, TabBarThemeGui::default());
//! ```

#![forbid(unsafe_code)]

use floem::{
    IntoView,
    peniko::Color,
    reactive::{RwSignal, SignalUpdate, SignalWith},
    views::{Decorators, dyn_stack, h_stack, label},
};
use hjkl_tabs::TabBar;

/// Column budget used when computing tab overflow for [`tab_bar_view`].
///
/// [`hjkl_tabs::TabBar::visible`] measures widths in terminal-style character
/// columns (inherited from the ratatui adapter's model). Floem lays out tab
/// chips with real pixel widths, so this constant is a stand-in "how many
/// tabs fit" budget for slice 1 rather than a measured viewport width. A
/// future slice can replace it with a reactive width derived from the
/// window/pane size.
const DEFAULT_MAX_WIDTH: u16 = 200;

// ── TabBarThemeGui ──────────────────────────────────────────────────────────

/// Colour configuration for [`tab_bar_view`].
///
/// `#[non_exhaustive]` — new fields may be added in minor releases without
/// breaking existing construction sites (use [`TabBarThemeGui::new`] or
/// [`TabBarThemeGui::default`] + field mutation).
///
/// The default palette mirrors `hjkl-tabs-tui`'s dark (catppuccin-inspired)
/// theme:
/// - `active_fg` / `active_bg`: dark text on blue.
/// - `inactive_fg` / `inactive_bg`: dimmed text on a raised chip background.
/// - `dirty_fg`: warm accent for the `●` dirty marker.
/// - `overflow_fg`: accent cyan for the `<` / `>` indicators.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct TabBarThemeGui {
    /// Foreground colour of the active (focused) tab.
    pub active_fg: Color,
    /// Background colour of the active tab.
    pub active_bg: Color,
    /// Foreground colour of inactive tabs.
    pub inactive_fg: Color,
    /// Background colour of inactive tabs (distinct from the bar background
    /// so inactive tabs read as raised chips rather than blending in).
    pub inactive_bg: Color,
    /// Foreground colour of the `●` dirty marker embedded in a tab's label.
    pub dirty_fg: Color,
    /// Foreground colour of the `<` / `>` overflow indicator chips.
    pub overflow_fg: Color,
}

impl TabBarThemeGui {
    /// Build a theme from explicit colour values.
    ///
    /// ```rust
    /// use floem::peniko::Color;
    /// use hjkl_tabs_gui::TabBarThemeGui;
    ///
    /// let theme = TabBarThemeGui::new(
    ///     Color::rgb8(0, 0, 0),
    ///     Color::rgb8(1, 1, 1),
    ///     Color::rgb8(2, 2, 2),
    ///     Color::rgb8(3, 3, 3),
    ///     Color::rgb8(4, 4, 4),
    ///     Color::rgb8(5, 5, 5),
    /// );
    /// assert_eq!(theme.active_fg, Color::rgb8(0, 0, 0));
    /// ```
    pub fn new(
        active_fg: Color,
        active_bg: Color,
        inactive_fg: Color,
        inactive_bg: Color,
        dirty_fg: Color,
        overflow_fg: Color,
    ) -> Self {
        Self {
            active_fg,
            active_bg,
            inactive_fg,
            inactive_bg,
            dirty_fg,
            overflow_fg,
        }
    }
}

impl Default for TabBarThemeGui {
    fn default() -> Self {
        Self {
            active_fg: Color::rgb8(0x2e, 0x34, 0x40),
            active_bg: Color::rgb8(0x5e, 0x81, 0xac),
            inactive_fg: Color::rgb8(0x81, 0x8a, 0x9a),
            inactive_bg: Color::rgb8(0x3b, 0x42, 0x52),
            dirty_fg: Color::rgb8(0xeb, 0xcb, 0x8b),
            overflow_fg: Color::rgb8(0x88, 0xc0, 0xd0),
        }
    }
}

// ── Pure display-logic helpers ────────────────────────────────────────────

/// A single tab chip as displayed by [`tab_bar_view`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabCell<Id> {
    /// The tab's opaque identifier, forwarded so [`tab_bar_view`] can wire up
    /// click-to-focus without a second pass over [`TabBar::tabs`].
    pub id: Id,
    /// Display label, already including the `●` dirty prefix when applicable
    /// (see [`hjkl_tabs::Tab::display_label`]).
    pub label: String,
    /// Whether this is the currently-focused tab.
    pub active: bool,
    /// Whether the underlying buffer has unsaved changes.
    pub dirty: bool,
}

/// Compute the visible tab chips for `bar` within `max_width` columns.
///
/// Delegates layout entirely to [`hjkl_tabs::TabBar::visible`] — this
/// function only maps the returned slice into display-ready [`TabCell`]s and
/// resolves which one is active. Returns `(cells, left_overflow,
/// right_overflow)`, mirroring `TabBar::visible`'s tuple shape.
///
/// ```rust
/// use hjkl_tabs::TabBar;
/// use hjkl_tabs_gui::tab_cells;
///
/// let mut bar: TabBar<u32> = TabBar::new();
/// bar.open(1, "a.rs".to_string());
/// bar.open(2, "b.rs".to_string());
/// bar.focus(&1);
///
/// let (cells, left_overflow, right_overflow) = tab_cells(&bar, 80);
/// assert_eq!(cells.len(), 2);
/// assert!(cells[0].active);
/// assert!(!cells[1].active);
/// assert!(!left_overflow);
/// assert!(!right_overflow);
/// ```
pub fn tab_cells<Id: Eq + Clone>(
    bar: &TabBar<Id>,
    max_width: u16,
) -> (Vec<TabCell<Id>>, bool, bool) {
    let (visible_tabs, left_overflow, right_overflow) = bar.visible(max_width);
    let active_idx = bar.active_index();

    // Resolve the absolute index of the first visible tab so activeness can
    // be checked against `bar.active_index()` even when the visible window
    // has scrolled away from the start (mirrors `hjkl-tabs-tui::build_line`).
    let tabs_slice_start = if left_overflow {
        visible_tabs
            .first()
            .and_then(|first| bar.tabs.iter().position(|t| std::ptr::eq(t, *first)))
            .unwrap_or(0)
    } else {
        0
    };

    let cells = visible_tabs
        .iter()
        .enumerate()
        .map(|(i, tab)| {
            let abs_idx = tabs_slice_start + i;
            TabCell {
                id: tab.id.clone(),
                label: tab.display_label(),
                active: active_idx == Some(abs_idx),
                dirty: tab.dirty,
            }
        })
        .collect();

    (cells, left_overflow, right_overflow)
}

// ── tab_bar_view ────────────────────────────────────────────────────────────

/// Build a floem view for the tab bar.
///
/// `bar` is a reactive signal owned by the caller (typically updated on tab
/// open/close/focus). The view contains no layout logic of its own — visible
/// tabs, active state, and overflow flags are all computed by [`tab_cells`],
/// which in turn defers to [`hjkl_tabs::TabBar::visible`].
///
/// Clicking a tab chip focuses it via [`TabBar::focus`].
pub fn tab_bar_view<Id: Eq + Clone + std::hash::Hash + 'static>(
    bar: RwSignal<TabBar<Id>>,
    theme: TabBarThemeGui,
) -> impl IntoView {
    let left_overflow = label(|| "<".to_string()).style(move |s| {
        let visible = bar.with(|b| tab_cells(b, DEFAULT_MAX_WIDTH).1);
        s.color(theme.overflow_fg)
            .padding_horiz(4.0)
            .apply_if(!visible, |s| s.hide())
    });

    let tabs = dyn_stack(
        move || bar.with(|b| tab_cells(b, DEFAULT_MAX_WIDTH).0),
        |cell: &TabCell<Id>| cell.id.clone(),
        move |cell: TabCell<Id>| {
            let TabCell {
                id,
                label: text,
                active,
                dirty,
            } = cell;
            let (fg, bg) = if active {
                (theme.active_fg, theme.active_bg)
            } else {
                (theme.inactive_fg, theme.inactive_bg)
            };
            let dirty_fg = theme.dirty_fg;

            label(move || text.clone())
                .style(move |s| {
                    let s = s
                        .padding_horiz(8.0)
                        .padding_vert(4.0)
                        .color(fg)
                        .background(bg);
                    if dirty { s.color(dirty_fg) } else { s }
                })
                .on_click_stop(move |_| {
                    bar.update(|b| b.focus(&id));
                })
        },
    )
    .style(|s| s.flex_row());

    let right_overflow = label(|| ">".to_string()).style(move |s| {
        let visible = bar.with(|b| tab_cells(b, DEFAULT_MAX_WIDTH).2);
        s.color(theme.overflow_fg)
            .padding_horiz(4.0)
            .apply_if(!visible, |s| s.hide())
    });

    h_stack((left_overflow, tabs, right_overflow)).style(|s| s.width_full())
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn bar_with(ids_titles: &[(usize, &str)]) -> TabBar<usize> {
        let mut bar = TabBar::new();
        for &(id, title) in ids_titles {
            bar.open(id, title.to_string());
        }
        bar
    }

    #[test]
    fn empty_bar_has_no_cells() {
        let bar: TabBar<usize> = TabBar::new();
        let (cells, lo, ro) = tab_cells(&bar, 80);
        assert!(cells.is_empty());
        assert!(!lo);
        assert!(!ro);
    }

    #[test]
    fn single_tab_cell_label() {
        let bar = bar_with(&[(1, "main.rs")]);
        let (cells, _, _) = tab_cells(&bar, 80);
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].label, "main.rs");
        assert_eq!(cells[0].id, 1);
    }

    #[test]
    fn multiple_tabs_all_appear_in_order() {
        let bar = bar_with(&[(1, "a.rs"), (2, "b.rs"), (3, "c.rs")]);
        let (cells, _, _) = tab_cells(&bar, 200);
        assert_eq!(cells.len(), 3);
        assert_eq!(cells[0].label, "a.rs");
        assert_eq!(cells[1].label, "b.rs");
        assert_eq!(cells[2].label, "c.rs");
    }

    #[test]
    fn active_flag_marks_focused_tab_only() {
        let mut bar = bar_with(&[(1, "a.rs"), (2, "b.rs")]);
        bar.focus(&1);
        let (cells, _, _) = tab_cells(&bar, 80);
        assert!(cells[0].active);
        assert!(!cells[1].active);
    }

    #[test]
    fn active_flag_follows_focus_change() {
        let mut bar = bar_with(&[(1, "a.rs"), (2, "b.rs")]);
        bar.focus(&2);
        let (cells, _, _) = tab_cells(&bar, 80);
        assert!(!cells[0].active);
        assert!(cells[1].active);
    }

    #[test]
    fn dirty_flag_propagates_from_tab() {
        let mut bar = bar_with(&[(1, "a.rs"), (2, "b.rs")]);
        if let Some(t) = bar.tabs.first_mut() {
            t.dirty = true;
        }
        let (cells, _, _) = tab_cells(&bar, 80);
        assert!(cells[0].dirty);
        assert!(!cells[1].dirty);
    }

    #[test]
    fn dirty_tab_label_includes_marker() {
        let mut bar = bar_with(&[(1, "a.rs")]);
        if let Some(t) = bar.tabs.first_mut() {
            t.dirty = true;
        }
        let (cells, _, _) = tab_cells(&bar, 80);
        assert_eq!(cells[0].label, "● a.rs");
    }

    #[test]
    fn no_overflow_when_all_tabs_fit() {
        let bar = bar_with(&[(1, "a"), (2, "b"), (3, "c")]);
        let (cells, lo, ro) = tab_cells(&bar, 200);
        assert_eq!(cells.len(), 3);
        assert!(!lo);
        assert!(!ro);
    }

    #[test]
    fn overflow_indicators_set_at_small_width() {
        let mut bar: TabBar<usize> = TabBar::new();
        for i in 0..20usize {
            bar.open(i, format!("longfilename{i}.rs"));
        }
        bar.focus(&15);
        let (cells, lo, ro) = tab_cells(&bar, 20);
        assert!(!cells.is_empty());
        assert!(lo || ro, "expected overflow with narrow width");
    }

    #[test]
    fn active_tab_always_present_in_narrow_view() {
        let mut bar: TabBar<usize> = TabBar::new();
        for i in 0..20usize {
            bar.open(i, format!("longfilename{i}.rs"));
        }
        bar.focus(&15);
        let (cells, _, _) = tab_cells(&bar, 20);
        assert!(cells.iter().any(|c| c.id == 15 && c.active));
    }

    #[test]
    fn theme_new_constructor() {
        let theme = TabBarThemeGui::new(
            Color::rgb8(0, 0, 0),
            Color::rgb8(1, 1, 1),
            Color::rgb8(2, 2, 2),
            Color::rgb8(3, 3, 3),
            Color::rgb8(4, 4, 4),
            Color::rgb8(5, 5, 5),
        );
        assert_eq!(theme.active_fg, Color::rgb8(0, 0, 0));
        assert_eq!(theme.active_bg, Color::rgb8(1, 1, 1));
        assert_eq!(theme.inactive_fg, Color::rgb8(2, 2, 2));
        assert_eq!(theme.inactive_bg, Color::rgb8(3, 3, 3));
        assert_eq!(theme.dirty_fg, Color::rgb8(4, 4, 4));
        assert_eq!(theme.overflow_fg, Color::rgb8(5, 5, 5));
    }

    #[test]
    fn theme_default_is_dark_palette() {
        let t = TabBarThemeGui::default();
        assert_eq!(t.active_bg, Color::rgb8(0x5e, 0x81, 0xac));
    }

    #[test]
    fn tab_bar_view_constructs_without_panicking() {
        let bar = bar_with(&[(1, "a.rs"), (2, "b.rs")]);
        let signal = RwSignal::new(bar);
        let _view = tab_bar_view(signal, TabBarThemeGui::default());
    }
}
