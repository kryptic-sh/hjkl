//! Which-key popup helpers.
//!
//! [`entries_for`] queries the live keymap for the direct children of a given
//! pending prefix, replacing the old static tables.  [`should_show`] drives
//! the idle-expiry check.

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
/// Includes both terminal bindings (with their own description) and
/// prefix-only entries (submenu nodes — rendered with description `"…"`).
pub fn entries_for(
    km: &hjkl_keymap::Keymap<crate::keymap_actions::AppAction, HjklMode>,
    mode: HjklMode,
    prefix: &[KeyEvent],
    leader: char,
) -> Vec<Entry> {
    let chord = Chord(prefix.to_vec());
    let mut entries: Vec<Entry> = km
        .children_all(mode, &chord)
        .into_iter()
        .map(|(ev, binding)| {
            let key = format_key(ev, leader);
            let desc = match binding {
                Some(b) => b.desc.clone(),
                None => "\u{2026}".to_string(), // "…" — indicates a submenu
            };
            Entry { key, desc }
        })
        .collect();

    entries.sort_by(|a, b| a.key.cmp(&b.key));
    entries
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
}
