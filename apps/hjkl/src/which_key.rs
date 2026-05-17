//! Which-key popup helpers.
//!
//! [`entries_for`] queries the live keymap for the direct children of a given
//! pending prefix, replacing the old static tables.  [`should_show`] drives
//! the idle-expiry check.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use hjkl_keymap::{Chord, KeyEvent};

use crate::app::keymap::HjklMode;

/// A single binding entry shown in the which-key popup.
pub struct Entry {
    /// The key character(s) rendered in vim notation (e.g. `"t"`, `"<C-w>"`).
    pub key: String,
    /// Short human-readable description (e.g. `"next tab"`).
    pub desc: String,
}

/// Render a single [`KeyEvent`] to a user-friendly vim-notation string.
///
/// The `leader` character is needed so the leader key itself renders as
/// `<leader>` rather than the bare character.
pub fn format_key(ev: KeyEvent, leader: char) -> String {
    // Delegate to the chord serialiser which already handles all the cases.
    Chord(vec![ev]).to_notation(leader)
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
pub fn entries_for(
    km: &hjkl_keymap::Keymap<crate::keymap_actions::AppAction, HjklMode>,
    mode: HjklMode,
    prefix: &[KeyEvent],
    leader: char,
) -> Vec<Entry> {
    // `HjklMode` is `hjkl_vim::Mode` (re-exported alias) so pass directly.
    let vim_mode: hjkl_vim::Mode = mode;
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
/// and which-key is enabled.  Extracted here so tests can drive
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // ── Helper ────────────────────────────────────────────────────────────────

    fn empty_keymap() -> hjkl_keymap::Keymap<crate::keymap_actions::AppAction, HjklMode> {
        hjkl_keymap::Keymap::new(' ')
    }

    // ── should_show tests ─────────────────────────────────────────────────────

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
        // No time has elapsed, delay not reached
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

    // ── entries_for merge tests ───────────────────────────────────────────────

    #[test]
    fn entries_include_engine_descriptors_at_root() {
        let km = empty_keymap();
        let entries = entries_for(&km, HjklMode::Normal, &[], ' ');
        let keys: Vec<&str> = entries.iter().map(|e| e.key.as_str()).collect();
        // Basic motions from the engine FSM must appear.
        for k in ["h", "j", "k", "l", "i", "a", "w", "b"] {
            assert!(keys.contains(&k), "entries_for missing engine key '{k}'");
        }
    }

    #[test]
    fn entries_include_g_prefix_engine_children() {
        let km = empty_keymap();
        // Pressing 'g' shows sub-prefix popup.
        let entries = entries_for(&km, HjklMode::Normal, &[KeyEvent::char('g')], ' ');
        let keys: Vec<&str> = entries.iter().map(|e| e.key.as_str()).collect();
        assert!(!entries.is_empty(), "g-prefix popup should be non-empty");
        assert!(keys.contains(&"g"), "g-prefix missing 'gg' entry");
        assert!(keys.contains(&"j"), "g-prefix missing 'gj' entry");
    }

    #[test]
    fn entries_include_z_prefix_engine_children() {
        let km = empty_keymap();
        let entries = entries_for(&km, HjklMode::Normal, &[KeyEvent::char('z')], ' ');
        let keys: Vec<&str> = entries.iter().map(|e| e.key.as_str()).collect();
        assert!(!entries.is_empty(), "z-prefix popup should be non-empty");
        assert!(keys.contains(&"z"), "z-prefix missing 'zz' entry");
    }

    #[test]
    fn app_entry_shadows_engine_entry() {
        // Register an app binding for 'i' in Normal mode with a custom desc.
        let mut km: hjkl_keymap::Keymap<crate::keymap_actions::AppAction, HjklMode> =
            hjkl_keymap::Keymap::new(' ');
        km.add(
            HjklMode::Normal,
            "i",
            crate::keymap_actions::AppAction::OpenFilePicker,
            "custom insert desc",
        )
        .expect("add failed");
        let entries = entries_for(&km, HjklMode::Normal, &[], ' ');
        // The 'i' entry should have the app's description, not the engine's.
        let i_entry = entries.iter().find(|e| e.key == "i").expect("missing 'i'");
        assert_eq!(
            i_entry.desc, "custom insert desc",
            "app desc should override engine desc for 'i'"
        );
    }

    #[test]
    fn entries_sorted_by_key() {
        let km = empty_keymap();
        let entries = entries_for(&km, HjklMode::Normal, &[], ' ');
        let keys: Vec<&str> = entries.iter().map(|e| e.key.as_str()).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted, "entries should be sorted by key string");
    }
}
