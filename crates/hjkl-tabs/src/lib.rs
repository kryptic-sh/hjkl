//! Renderer-agnostic tab bar data model.
//!
//! # Naming note
//!
//! [`hjkl_layout`] already defines a `Tab` struct that represents a
//! **window-split tab** — a spatial layout tree with a focused window. That
//! concept is entirely different from the types here.
//!
//! `hjkl-tabs::Tab<Id>` is a **UI widget tab**: a lightweight handle with a
//! display title and a dirty marker, used for browser-style buffer tabs in
//! editors and tooling (sqeel SQL buffers, hjkl multi-buffer, buffr web tabs).
//!
//! # Usage
//!
//! ```
//! use hjkl_tabs::{Tab, TabBar};
//!
//! let mut bar: TabBar<u32> = TabBar::new();
//! bar.open(1, "main.rs".to_string());
//! bar.open(2, "lib.rs".to_string());
//! bar.focus(&1);
//!
//! assert_eq!(bar.active().map(|t| t.title.as_str()), Some("main.rs"));
//! assert_eq!(bar.len(), 2);
//! ```
//!
//! Truncation/overflow: call [`TabBar::visible`] to get the subset of tabs
//! that fit within a given terminal width.
//!
//! # Rendering
//!
//! Hand a `TabBar` to a renderer adapter — `hjkl-tabs-tui` for ratatui,
//! `hjkl-tabs-gui` for floem/egui (see issue #8). This crate has **zero**
//! renderer dependencies.

#![forbid(unsafe_code)]

/// A single browser-style editor tab.
///
/// `#[non_exhaustive]` — new fields may be added in minor releases without
/// breaking existing construction sites (which must use [`Tab::new`]).
///
/// # Naming note
///
/// This is distinct from `hjkl_layout::Tab`, which represents a spatial
/// window-split layout tree. `hjkl_tabs::Tab<Id>` is a lightweight UI-widget
/// handle: title + dirty marker.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Tab<Id> {
    /// Opaque identifier supplied by the caller. Used to locate the tab in
    /// [`TabBar`] operations without exposing the positional index.
    pub id: Id,
    /// Human-readable display title (e.g. a filename or buffer name).
    pub title: String,
    /// Whether the buffer associated with this tab has unsaved changes.
    pub dirty: bool,
    /// Optional filetype icon glyph rendered in its own span before the title
    /// (e.g. a Nerd-Font devicon). `None` → no icon cell. UI-toolkit-agnostic:
    /// the renderer ([`hjkl_tabs_tui`]) owns glyph placement + spacing.
    pub icon: Option<String>,
    /// Optional RGB colour for [`Self::icon`] (the devicon colour). `None` →
    /// the icon inherits the tab's foreground. Stored as a raw `(r, g, b)`
    /// triple so this crate stays free of any rendering-toolkit colour type.
    pub icon_color: Option<(u8, u8, u8)>,
}

impl<Id: Clone + Eq> Tab<Id> {
    /// Create a new tab with `id` and `title`; `dirty` starts as `false`.
    ///
    /// ```
    /// use hjkl_tabs::Tab;
    ///
    /// let t: Tab<u32> = Tab::new(42, "README.md".to_string());
    /// assert_eq!(t.id, 42);
    /// assert_eq!(t.title, "README.md");
    /// assert!(!t.dirty);
    /// ```
    pub fn new(id: Id, title: String) -> Self {
        Self {
            id,
            title,
            dirty: false,
            icon: None,
            icon_color: None,
        }
    }

    /// Returns the display label used when rendering.
    ///
    /// Prepends a dirty marker (`● `) when the tab is dirty.
    ///
    /// ```
    /// use hjkl_tabs::Tab;
    ///
    /// let mut t: Tab<u32> = Tab::new(1, "foo.rs".to_string());
    /// assert_eq!(t.display_label(), "foo.rs");
    ///
    /// t.dirty = true;
    /// assert_eq!(t.display_label(), "● foo.rs");
    /// ```
    pub fn display_label(&self) -> String {
        if self.dirty {
            format!("● {}", self.title)
        } else {
            self.title.clone()
        }
    }

    /// Char width of this tab's icon cell — the glyph plus one trailing space —
    /// or `0` when the tab has no icon. The renderer paints the icon as a
    /// separate span (`"{icon} "`) so its colour is independent of the label.
    pub fn icon_width(&self) -> usize {
        self.icon
            .as_ref()
            .map(|i| i.chars().count() + 1)
            .unwrap_or(0)
    }

    /// Pixel-free width of this tab's padded cell: `" {icon} {label} "` in
    /// chars (the icon segment is omitted when absent).
    ///
    /// The two spaces (one on each side) are the inter-tab padding used by the
    /// renderer. Width accounts for the `●` dirty-marker prefix when present and
    /// the optional icon cell.
    pub fn cell_width(&self) -> usize {
        // " {icon }{label} " = 2 padding + icon cell + label chars
        2 + self.icon_width() + self.display_label().chars().count()
    }
}

/// A horizontal strip of editor tabs.
///
/// `#[non_exhaustive]` — new fields may be added in minor releases without
/// breaking existing construction sites (which must use [`TabBar::new`]).
///
/// Tabs are ordered by insertion. The active tab is tracked by index into
/// `tabs`; all focus operations update `active`.
///
/// # Example
///
/// ```
/// use hjkl_tabs::TabBar;
///
/// let mut bar: TabBar<&str> = TabBar::new();
/// bar.open("a", "alpha.rs".to_string());
/// bar.open("b", "beta.rs".to_string());
/// // After two opens the active is "b" (index 1). focus_next wraps to "a".
/// bar.focus_next();
/// assert_eq!(bar.active().map(|t| t.id), Some("a"));
/// ```
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct TabBar<Id> {
    /// Ordered list of tabs.
    pub tabs: Vec<Tab<Id>>,
    /// Index of the currently focused tab, or `None` when the bar is empty.
    pub active: Option<usize>,
}

impl<Id: Eq + Clone> TabBar<Id> {
    /// Create an empty [`TabBar`].
    pub fn new() -> Self {
        Self {
            tabs: Vec::new(),
            active: None,
        }
    }

    /// Number of tabs in the bar.
    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    /// `true` when no tabs are open.
    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    /// Open a new tab with `id` and `title`, replacing an existing tab if
    /// one with the same `id` is already present (updates its title, leaves
    /// `dirty` unchanged). Focus moves to the opened/updated tab.
    ///
    /// ```
    /// use hjkl_tabs::TabBar;
    ///
    /// let mut bar: TabBar<u32> = TabBar::new();
    /// bar.open(1, "foo.rs".to_string());
    /// bar.open(2, "bar.rs".to_string());
    /// assert_eq!(bar.len(), 2);
    ///
    /// // Re-opening the same id updates the title and refocuses.
    /// bar.open(1, "foo-renamed.rs".to_string());
    /// assert_eq!(bar.len(), 2);
    /// assert_eq!(bar.active().map(|t| t.title.as_str()), Some("foo-renamed.rs"));
    /// ```
    pub fn open(&mut self, id: Id, title: String) {
        if let Some(pos) = self.tabs.iter().position(|t| t.id == id) {
            self.tabs[pos].title = title;
            self.active = Some(pos);
        } else {
            self.tabs.push(Tab::new(id, title));
            self.active = Some(self.tabs.len() - 1);
        }
    }

    /// Close the tab with `id`. If the closed tab was active, focus shifts to
    /// the nearest remaining tab (prefers the tab at the same index, then the
    /// one before it).
    ///
    /// ```
    /// use hjkl_tabs::TabBar;
    ///
    /// let mut bar: TabBar<u32> = TabBar::new();
    /// bar.open(1, "a.rs".to_string());
    /// bar.open(2, "b.rs".to_string());
    /// bar.open(3, "c.rs".to_string());
    /// bar.focus(&1);
    /// bar.close(&1);
    /// // Focus moves to what was previously index 1 (now index 0).
    /// assert_eq!(bar.active().map(|t| t.id), Some(2));
    /// ```
    pub fn close(&mut self, id: &Id) {
        if let Some(pos) = self.tabs.iter().position(|t| &t.id == id) {
            self.tabs.remove(pos);
            self.active = if self.tabs.is_empty() {
                None
            } else {
                let new_idx = pos.min(self.tabs.len() - 1);
                Some(new_idx)
            };
        }
    }

    /// Move focus to the next tab, wrapping around to the first.
    ///
    /// No-op when the bar is empty.
    ///
    /// ```
    /// use hjkl_tabs::TabBar;
    ///
    /// let mut bar: TabBar<u32> = TabBar::new();
    /// bar.open(1, "a".to_string());
    /// bar.open(2, "b".to_string());
    /// bar.focus(&1);
    /// bar.focus_next();
    /// assert_eq!(bar.active().map(|t| t.id), Some(2));
    /// bar.focus_next(); // wraps
    /// assert_eq!(bar.active().map(|t| t.id), Some(1));
    /// ```
    pub fn focus_next(&mut self) {
        if let Some(idx) = self.active {
            self.active = Some((idx + 1) % self.tabs.len());
        }
    }

    /// Move focus to the previous tab, wrapping around to the last.
    ///
    /// No-op when the bar is empty.
    ///
    /// ```
    /// use hjkl_tabs::TabBar;
    ///
    /// let mut bar: TabBar<u32> = TabBar::new();
    /// bar.open(1, "a".to_string());
    /// bar.open(2, "b".to_string());
    /// bar.focus(&2);
    /// bar.focus_prev();
    /// assert_eq!(bar.active().map(|t| t.id), Some(1));
    /// bar.focus_prev(); // wraps
    /// assert_eq!(bar.active().map(|t| t.id), Some(2));
    /// ```
    pub fn focus_prev(&mut self) {
        if let Some(idx) = self.active {
            let n = self.tabs.len();
            self.active = Some((idx + n - 1) % n);
        }
    }

    /// Focus the tab with the given `id`. No-op if `id` is not present.
    ///
    /// ```
    /// use hjkl_tabs::TabBar;
    ///
    /// let mut bar: TabBar<u32> = TabBar::new();
    /// bar.open(1, "a".to_string());
    /// bar.open(2, "b".to_string());
    /// bar.focus(&1);
    /// assert_eq!(bar.active().map(|t| t.id), Some(1));
    /// ```
    pub fn focus(&mut self, id: &Id) {
        if let Some(pos) = self.tabs.iter().position(|t| &t.id == id) {
            self.active = Some(pos);
        }
    }

    /// Return a reference to the currently active tab, or `None` if empty.
    ///
    /// ```
    /// use hjkl_tabs::TabBar;
    ///
    /// let mut bar: TabBar<u32> = TabBar::new();
    /// assert!(bar.active().is_none());
    /// bar.open(7, "readme".to_string());
    /// assert_eq!(bar.active().map(|t| t.id), Some(7));
    /// ```
    pub fn active(&self) -> Option<&Tab<Id>> {
        self.active.and_then(|i| self.tabs.get(i))
    }

    /// Return a mutable reference to the currently active tab, or `None`.
    pub fn active_mut(&mut self) -> Option<&mut Tab<Id>> {
        self.active.and_then(|i| self.tabs.get_mut(i))
    }

    /// Return the index of the active tab, or `None`.
    pub fn active_index(&self) -> Option<usize> {
        self.active
    }

    /// Compute the subset of tabs that fit within `max_width` terminal columns.
    ///
    /// Each tab occupies `cell_width()` columns (padded label).  Separator
    /// characters (`│`, 1 col each) are inserted between adjacent tabs. When
    /// the full strip overflows `max_width`:
    ///
    /// - A `<` indicator (1 col) is prepended if there are hidden tabs to the
    ///   left of the visible window.
    /// - A `>` indicator (1 col) is appended if there are hidden tabs to the
    ///   right of the visible window.
    ///
    /// The active tab is always kept visible. The visible window grows
    /// outward from the active tab until `max_width` is exhausted.
    ///
    /// Returns `(slice, left_overflow, right_overflow)`.
    ///
    /// ```
    /// use hjkl_tabs::TabBar;
    ///
    /// let mut bar: TabBar<u32> = TabBar::new();
    /// for i in 0..10u32 {
    ///     bar.open(i, format!("file{i}.rs"));
    /// }
    /// bar.focus(&5);
    /// let (visible, lo, ro) = bar.visible(20);
    /// // Active tab is always present in the slice.
    /// assert!(visible.iter().any(|t| t.id == 5));
    /// // With only 20 cols some tabs must overflow.
    /// assert!(lo || ro || visible.len() == bar.len());
    /// ```
    pub fn visible(&self, max_width: u16) -> (Vec<&Tab<Id>>, bool, bool) {
        if self.tabs.is_empty() {
            return (Vec::new(), false, false);
        }

        let max = max_width as usize;
        let n = self.tabs.len();
        let active_idx = self.active.unwrap_or(0);

        // Fast path: everything fits without any overflow indicators.
        let total_no_overflow = self.total_display_width();
        if total_no_overflow <= max {
            let refs: Vec<&Tab<Id>> = self.tabs.iter().collect();
            return (refs, false, false);
        }

        // Need to scroll — reserve 1 col on each potentially-overflowed side.
        // We try to center on the active tab.
        let indicator_budget = 2; // worst case: `<` + `>`
        let budget = max.saturating_sub(indicator_budget);

        // Grow outward from active_idx until budget exhausted.
        let mut lo = active_idx;
        let mut hi = active_idx;
        let mut used = self.tabs[active_idx].cell_width();

        loop {
            let expanded = if lo > 0 {
                let w = self.tabs[lo - 1].cell_width() + 1; // +1 sep
                if used + w <= budget {
                    used += w;
                    lo -= 1;
                    true
                } else {
                    false
                }
            } else {
                false
            };

            let expanded_hi = if hi + 1 < n {
                let w = self.tabs[hi + 1].cell_width() + 1; // +1 sep
                if used + w <= budget {
                    used += w;
                    hi += 1;
                    true
                } else {
                    false
                }
            } else {
                false
            };

            if !expanded && !expanded_hi {
                break;
            }
        }

        let left_overflow = lo > 0;
        let right_overflow = hi + 1 < n;
        let refs: Vec<&Tab<Id>> = self.tabs[lo..=hi].iter().collect();
        (refs, left_overflow, right_overflow)
    }

    /// Total display width of all tabs rendered without overflow indicators.
    ///
    /// Accounts for one `│` separator between adjacent tabs.
    fn total_display_width(&self) -> usize {
        if self.tabs.is_empty() {
            return 0;
        }
        let cells: usize = self.tabs.iter().map(|t| t.cell_width()).sum();
        let seps = self.tabs.len() - 1;
        cells + seps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar_with(ids: &[u32], titles: &[&str]) -> TabBar<u32> {
        let mut bar = TabBar::new();
        for (&id, &title) in ids.iter().zip(titles.iter()) {
            bar.open(id, title.to_string());
        }
        bar
    }

    #[test]
    fn cell_width_accounts_for_icon() {
        let mut t: Tab<u32> = Tab::new(1, "a.rs".to_string());
        let base = t.cell_width(); // 2 padding + "a.rs" = 6
        assert_eq!(base, 6);
        assert_eq!(t.icon_width(), 0);
        t.icon = Some("R".to_string());
        // icon cell = glyph + one space = 2.
        assert_eq!(t.icon_width(), 2);
        assert_eq!(t.cell_width(), base + 2);
    }

    #[test]
    fn new_tab_has_no_icon() {
        let t: Tab<u32> = Tab::new(1, "a.rs".to_string());
        assert!(t.icon.is_none());
        assert!(t.icon_color.is_none());
    }

    #[test]
    fn new_bar_is_empty() {
        let bar: TabBar<u32> = TabBar::new();
        assert!(bar.is_empty());
        assert_eq!(bar.len(), 0);
        assert!(bar.active().is_none());
    }

    #[test]
    fn open_adds_tab_and_focuses() {
        let mut bar: TabBar<u32> = TabBar::new();
        bar.open(10, "ten.rs".to_string());
        assert_eq!(bar.len(), 1);
        assert_eq!(bar.active().unwrap().id, 10);
        assert_eq!(bar.active().unwrap().title, "ten.rs");
    }

    #[test]
    fn open_same_id_updates_title() {
        let mut bar = bar_with(&[1, 2], &["a.rs", "b.rs"]);
        bar.focus(&1);
        bar.open(1, "a-new.rs".to_string());
        assert_eq!(bar.len(), 2);
        assert_eq!(bar.active().unwrap().title, "a-new.rs");
    }

    #[test]
    fn close_removes_tab_and_adjusts_focus() {
        let mut bar = bar_with(&[1, 2, 3], &["a", "b", "c"]);
        bar.focus(&2);
        bar.close(&2);
        assert_eq!(bar.len(), 2);
        // Focus should land on what was index 2 (now "c" at index 1).
        assert_eq!(bar.active().unwrap().id, 3);
    }

    #[test]
    fn close_last_tab_yields_none_active() {
        let mut bar: TabBar<u32> = TabBar::new();
        bar.open(1, "only".to_string());
        bar.close(&1);
        assert!(bar.is_empty());
        assert!(bar.active().is_none());
    }

    #[test]
    fn focus_next_wraps() {
        let mut bar = bar_with(&[1, 2, 3], &["a", "b", "c"]);
        bar.focus(&3);
        bar.focus_next();
        assert_eq!(bar.active().unwrap().id, 1);
    }

    #[test]
    fn focus_prev_wraps() {
        let mut bar = bar_with(&[1, 2, 3], &["a", "b", "c"]);
        bar.focus(&1);
        bar.focus_prev();
        assert_eq!(bar.active().unwrap().id, 3);
    }

    #[test]
    fn focus_unknown_id_is_noop() {
        let mut bar = bar_with(&[1, 2], &["a", "b"]);
        bar.focus(&1);
        bar.focus(&99);
        // active stays on 1
        assert_eq!(bar.active().unwrap().id, 1);
    }

    #[test]
    fn close_unknown_id_is_noop() {
        let mut bar = bar_with(&[1, 2], &["a", "b"]);
        bar.close(&99);
        assert_eq!(bar.len(), 2);
    }

    #[test]
    fn display_label_no_dirty() {
        let t: Tab<u32> = Tab::new(1, "foo.rs".to_string());
        assert_eq!(t.display_label(), "foo.rs");
    }

    #[test]
    fn display_label_dirty() {
        let mut t: Tab<u32> = Tab::new(1, "foo.rs".to_string());
        t.dirty = true;
        assert_eq!(t.display_label(), "● foo.rs");
    }

    #[test]
    fn cell_width_accounts_for_padding() {
        let t: Tab<u32> = Tab::new(1, "ab".to_string());
        // " ab " = 4
        assert_eq!(t.cell_width(), 4);
    }

    #[test]
    fn visible_all_fit() {
        let bar = bar_with(&[1, 2, 3], &["a", "b", "c"]);
        let (vis, lo, ro) = bar.visible(200);
        assert_eq!(vis.len(), 3);
        assert!(!lo);
        assert!(!ro);
    }

    #[test]
    fn visible_active_always_present() {
        let mut bar: TabBar<u32> = TabBar::new();
        for i in 0..20u32 {
            bar.open(i, format!("file{i}.rs"));
        }
        bar.focus(&15);
        let (vis, _, _) = bar.visible(20);
        assert!(vis.iter().any(|t| t.id == 15));
    }

    #[test]
    fn visible_overflow_indicators() {
        let mut bar: TabBar<u32> = TabBar::new();
        for i in 0..10u32 {
            bar.open(i, format!("file{i}.rs"));
        }
        bar.focus(&5);
        let (vis, lo, ro) = bar.visible(20);
        // At 20 cols with 10 long-ish tabs there must be overflow on at least one side.
        assert!(
            lo || ro || vis.len() == bar.len(),
            "expected overflow or all visible"
        );
    }

    #[test]
    fn active_mut_allows_dirty_toggle() {
        let mut bar: TabBar<u32> = TabBar::new();
        bar.open(1, "x.rs".to_string());
        if let Some(t) = bar.active_mut() {
            t.dirty = true;
        }
        assert!(bar.active().unwrap().dirty);
    }

    #[test]
    fn active_index_tracks_focus() {
        let mut bar = bar_with(&[10, 20, 30], &["a", "b", "c"]);
        bar.focus(&20);
        assert_eq!(bar.active_index(), Some(1));
    }

    #[test]
    fn tab_default_not_dirty() {
        let t: Tab<i32> = Tab::new(-1, "test".to_string());
        assert!(!t.dirty);
    }

    #[test]
    fn bar_default_equals_new() {
        let a: TabBar<u32> = TabBar::default();
        let b: TabBar<u32> = TabBar::new();
        assert_eq!(a.len(), b.len());
        assert_eq!(a.active, b.active);
    }

    #[test]
    fn open_multiple_then_close_first() {
        let mut bar = bar_with(&[1, 2, 3, 4], &["a", "b", "c", "d"]);
        bar.focus(&1);
        bar.close(&1);
        // Focus should now be on id=2 (was index 1, now index 0).
        assert_eq!(bar.active().unwrap().id, 2);
        assert_eq!(bar.len(), 3);
    }

    #[test]
    fn focus_cycle_full_round_trip() {
        let mut bar = bar_with(&[1, 2, 3], &["a", "b", "c"]);
        bar.focus(&1);
        bar.focus_next(); // -> 2
        bar.focus_next(); // -> 3
        bar.focus_next(); // -> 1 (wrap)
        assert_eq!(bar.active().unwrap().id, 1);
    }

    #[test]
    fn focus_prev_cycle_full_round_trip() {
        let mut bar = bar_with(&[1, 2, 3], &["a", "b", "c"]);
        bar.focus(&3);
        bar.focus_prev(); // -> 2
        bar.focus_prev(); // -> 1
        bar.focus_prev(); // -> 3 (wrap)
        assert_eq!(bar.active().unwrap().id, 3);
    }

    #[test]
    fn total_display_width_single_tab() {
        let bar = bar_with(&[1], &["ab"]);
        // " ab " = 4, no separators
        assert_eq!(bar.total_display_width(), 4);
    }

    #[test]
    fn total_display_width_two_tabs() {
        let bar = bar_with(&[1, 2], &["ab", "cd"]);
        // " ab " + "|" + " cd " = 4 + 1 + 4 = 9
        assert_eq!(bar.total_display_width(), 9);
    }
}
