//! Renderer-agnostic which-key popup model.
//!
//! Provides [`Entry`] (a single key+desc row), [`entries_for`] (builds the
//! entry list from a live keymap + engine FSM descriptors), [`should_show`]
//! (idle-expiry check), [`format_key`] (vim-notation formatter), and
//! [`layout`] / [`PopupLayout`] (pure column-layout geometry — no painting).
//!
//! Rendering is left to adapter crates (`hjkl-which-key-tui`, etc.).

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use hjkl_keymap::{Chord, KeyEvent};

// ── Public types ──────────────────────────────────────────────────────────────

/// A single binding entry shown in the which-key popup.
///
/// `#[non_exhaustive]` — new display fields (e.g. icon, group) may be added
/// in minor releases without breaking existing struct-literal construction.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The key character(s) rendered in vim notation (e.g. `"t"`, `"<C-w>"`).
    pub key: String,
    /// Short human-readable description (e.g. `"next tab"`, `"→ :echo hi"`).
    pub desc: String,
}

impl Entry {
    /// Construct a new [`Entry`].
    pub fn new(key: impl Into<String>, desc: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            desc: desc.into(),
        }
    }
}

/// Geometry produced by [`layout`]. Pure math — no painting.
///
/// `#[non_exhaustive]` — new geometry fields may be added without a breaking
/// change.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PopupLayout {
    /// Number of visible columns.
    pub cols: usize,
    /// Number of content rows (capped at [`MAX_POPUP_ROWS`]).
    pub rows: usize,
    /// Width of each column in terminal cells (includes inter-column padding).
    pub col_width: u16,
    /// Total popup height (1 header + rows + 2 border = rows + 3).
    pub popup_h: u16,
    /// Total popup width (== the `width` passed to [`layout`]).
    pub popup_w: u16,
    /// The entries that fit within the grid (at most `cols * rows`).
    pub visible: Vec<Entry>,
}

/// Maximum content rows in the popup.
pub const MAX_POPUP_ROWS: usize = 12;

// ── Core functions ────────────────────────────────────────────────────────────

/// Render a single [`KeyEvent`] to a user-friendly vim-notation string.
///
/// The `leader` character is needed so the leader key itself renders as
/// `<leader>` rather than the bare character.
pub fn format_key(ev: KeyEvent, leader: char) -> String {
    Chord(vec![ev]).to_notation(leader)
}

/// Truncate `s` to at most `max_chars` Unicode scalar values, appending `…`
/// if truncated.
pub fn truncate_desc(s: &str, max_chars: usize) -> String {
    let mut chars = s.chars();
    let collected: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{collected}…")
    } else {
        collected
    }
}

/// Query `km` for the direct children of `prefix` in `mode` and return
/// them as which-key [`Entry`] values, sorted alphabetically by key string.
///
/// Merges engine FSM built-in descriptors (from [`hjkl_vim::descriptors`])
/// with the app keymap entries. App entries win on conflict so that `:nmap`
/// user bindings shadow built-ins with their own description.
///
/// Includes both terminal bindings (with their own description) and
/// prefix-only entries (submenu nodes — rendered with description `"…"`).
///
/// `A` is the consumer's action type (e.g. `AppAction`).
pub fn entries_for<A: Clone, M>(
    km: &hjkl_keymap::Keymap<A, M>,
    mode: M,
    prefix: &[KeyEvent],
    leader: char,
) -> Vec<Entry>
where
    M: hjkl_keymap::Mode + Into<hjkl_vim::Mode>,
{
    let vim_mode: hjkl_vim::Mode = mode.into();
    let mut by_key: BTreeMap<String, Entry> = BTreeMap::new();

    // 1. Engine descriptors first (lower priority — app keymap overrides below).
    for d in hjkl_vim::descriptors::children_for(vim_mode, prefix) {
        let key = format_key(d.key, leader);
        let desc = d.desc.unwrap_or("\u{2026}").to_string();
        by_key.insert(key.clone(), Entry { key, desc });
    }

    // 2. App keymap second — overrides engine entries on conflict.
    let chord = Chord(prefix.to_vec());
    for (ev, binding) in km.children_all(mode, &chord) {
        let key = format_key(ev, leader);
        let desc = match binding {
            Some(b) => b.desc.clone(),
            None => "\u{2026}".to_string(), // "…" — indicates a submenu
        };
        by_key.insert(key.clone(), Entry { key, desc });
    }

    // BTreeMap already sorts by key string — collect preserves that order.
    by_key.into_values().collect()
}

/// Pure function: should the which-key popup be shown right now?
///
/// Returns `true` when a prefix has been pending for at least `delay`
/// and which-key is enabled. Extracted here so tests can drive
/// `now` without mocking `Instant::now()`.
pub fn should_show(
    pending_at: Option<Instant>,
    delay: Duration,
    enabled: bool,
    now: Instant,
) -> bool {
    if !enabled {
        return false;
    }
    match pending_at {
        Some(at) => now.duration_since(at) >= delay,
        None => false,
    }
}

/// Compute the column layout for a which-key popup of a given terminal width.
///
/// `width` is the available terminal width in cells (the full popup width,
/// before subtracting border). Returns a [`PopupLayout`] with pure geometry
/// — no painting.
///
/// Cap: at most [`MAX_POPUP_ROWS`] content rows; excess entries are dropped.
pub fn layout(entries: &[Entry], width: u16) -> PopupLayout {
    if entries.is_empty() {
        return PopupLayout {
            cols: 1,
            rows: 0,
            col_width: 0,
            popup_h: 3,
            popup_w: width,
            visible: vec![],
        };
    }
    let entry_width = entries
        .iter()
        .map(|e| e.key.len() + 1 + e.desc.len()) // key + space + desc
        .max()
        .unwrap_or(8) as u16;
    // Each column: entry_width + 2 padding between columns.
    let col_width = entry_width + 2;
    let available_width = width.saturating_sub(2); // subtract border
    let cols = (available_width / col_width).max(1) as usize;
    let rows_needed = entries.len().div_ceil(cols);
    let rows = rows_needed.min(MAX_POPUP_ROWS);
    let popup_h = rows as u16 + 3; // 1 header + rows + 2 border
    let popup_w = width;
    let visible = entries.iter().take(cols * rows).cloned().collect();
    PopupLayout {
        cols,
        rows,
        col_width,
        popup_h,
        popup_w,
        visible,
    }
}

// ── Feature stubs ─────────────────────────────────────────────────────────────

/// GUI adapter stub (no-op until `apps/hjkl-gui` needs it).
///
/// Enable with `features = ["gui"]` in your `Cargo.toml`. Currently a marker
/// only — the actual GUI render lives in a future `hjkl-which-key-gui` crate.
#[cfg(feature = "gui")]
pub mod gui {
    /// Placeholder: GUI render not yet implemented.
    pub fn render_gui() {
        // future: floem adapter
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // ── should_show tests ─────────────────────────────────────────────────

    #[test]
    fn should_show_returns_false_when_disabled() {
        let at = Instant::now() - Duration::from_secs(2);
        assert!(!should_show(
            Some(at),
            Duration::from_millis(500),
            false,
            Instant::now()
        ));
    }

    #[test]
    fn should_show_returns_false_when_no_prefix() {
        assert!(!should_show(
            None,
            Duration::from_millis(500),
            true,
            Instant::now()
        ));
    }

    #[test]
    fn should_show_returns_false_before_delay() {
        let at = Instant::now();
        assert!(!should_show(Some(at), Duration::from_millis(500), true, at));
    }

    #[test]
    fn should_show_returns_true_after_delay() {
        let at = Instant::now() - Duration::from_secs(2);
        assert!(should_show(
            Some(at),
            Duration::from_millis(500),
            true,
            Instant::now()
        ));
    }

    // ── layout tests ─────────────────────────────────────────────────────

    #[test]
    fn layout_empty_entries_gives_one_col() {
        let l = layout(&[], 80);
        assert_eq!(l.cols, 1);
        assert_eq!(l.rows, 0);
        assert_eq!(l.popup_h, 3); // 0 + 3
    }

    #[test]
    fn layout_single_entry_one_row() {
        let entries = vec![Entry::new("x", "some desc")];
        let l = layout(&entries, 80);
        assert_eq!(l.rows, 1);
        assert_eq!(l.visible.len(), 1);
        assert_eq!(l.popup_h, 4); // 1 + 3
    }

    #[test]
    fn layout_caps_at_max_rows() {
        // 15 entries in single col → 15 rows → capped to MAX_POPUP_ROWS.
        // Use a narrow width so only 1 col fits.
        let entries: Vec<Entry> = (0..15).map(|i| Entry::new(format!("k{i}"), "x")).collect();
        let l = layout(&entries, 10);
        assert_eq!(l.rows, MAX_POPUP_ROWS);
        assert!(l.visible.len() <= l.cols * l.rows);
    }

    #[test]
    fn layout_multi_col_packs_correctly() {
        // 6 entries, each 3 chars wide (key "a" + " " + desc "x" = 3), col_width = 5.
        // At width 80: available = 78, cols = 78/5 = 15 → all 6 fit in 1 row.
        let entries: Vec<Entry> = (0..6)
            .map(|i| Entry::new(char::from(b'a' + i), "x"))
            .collect();
        let l = layout(&entries, 80);
        assert!(
            l.cols >= 6,
            "expected >= 6 cols at width 80, got {}",
            l.cols
        );
        assert_eq!(l.rows, 1);
        assert_eq!(l.visible.len(), 6);
    }

    // ── truncate_desc tests ───────────────────────────────────────────────

    #[test]
    fn truncate_desc_short_string_unchanged() {
        assert_eq!(truncate_desc("hi", 40), "hi");
    }

    #[test]
    fn truncate_desc_at_limit_unchanged() {
        let s: String = "x".repeat(40);
        assert_eq!(truncate_desc(&s, 40), s);
    }

    #[test]
    fn truncate_desc_over_limit_appends_ellipsis() {
        let s: String = "x".repeat(41);
        let result = truncate_desc(&s, 40);
        assert!(result.ends_with('…'));
        assert_eq!(result.chars().count(), 41); // 40 x's + ellipsis
    }
}
