//! Which-key binding registry and idle-expiry helper.
//!
//! Provides static tables of key → description for every prefix that the
//! app handles (leader, `g`, `]`, `[`, `<C-w>`).  These tables are used
//! by the which-key popup renderer in [`crate::render`].
//!
//! The public [`should_show`] function encapsulates the idle-expiry check
//! so it can be unit-tested without mocking `Instant`.

use std::time::{Duration, Instant};

/// A single binding entry shown in the which-key popup.
pub struct Entry {
    /// The key character(s) the user presses after the prefix (e.g. `"t"`, `"gs"`).
    pub key: &'static str,
    /// Short human-readable description (e.g. `"next tab"`).
    pub desc: &'static str,
}

/// Which prefix is currently pending.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Prefix {
    /// `<Space>` (or configured leader key).
    Leader,
    /// `g` motion prefix.
    G,
    /// `]` bracket prefix.
    BracketRight,
    /// `[` bracket prefix.
    BracketLeft,
    /// `<C-w>` window-motion prefix.
    CtrlW,
}

/// Return all entries for a given prefix.
pub fn entries(prefix: Prefix) -> &'static [Entry] {
    match prefix {
        Prefix::Leader => LEADER_ENTRIES,
        Prefix::G => G_ENTRIES,
        Prefix::BracketRight => BRACKET_RIGHT_ENTRIES,
        Prefix::BracketLeft => BRACKET_LEFT_ENTRIES,
        Prefix::CtrlW => CTRL_W_ENTRIES,
    }
}

/// Return the display label for a given prefix (used as popup header).
pub fn label(prefix: Prefix) -> &'static str {
    match prefix {
        Prefix::Leader => "<leader>",
        Prefix::G => "g",
        Prefix::BracketRight => "]",
        Prefix::BracketLeft => "[",
        Prefix::CtrlW => "<C-w>",
    }
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

// ── Static binding tables ─────────────────────────────────────────────────

static LEADER_ENTRIES: &[Entry] = &[
    Entry {
        key: "<leader>",
        desc: "file picker",
    },
    Entry {
        key: "f",
        desc: "file picker",
    },
    Entry {
        key: "b",
        desc: "buffer picker",
    },
    Entry {
        key: "/",
        desc: "grep picker",
    },
    Entry {
        key: "g",
        desc: "git commands…",
    },
    Entry {
        key: "gs",
        desc: "git status",
    },
    Entry {
        key: "gl",
        desc: "git log",
    },
    Entry {
        key: "gb",
        desc: "git branches",
    },
    Entry {
        key: "gB",
        desc: "git file history",
    },
    Entry {
        key: "gS",
        desc: "git stashes",
    },
    Entry {
        key: "gt",
        desc: "git tags",
    },
    Entry {
        key: "gr",
        desc: "git remotes",
    },
    Entry {
        key: "d",
        desc: "show diagnostic",
    },
    Entry {
        key: "ca",
        desc: "code actions",
    },
    Entry {
        key: "rn",
        desc: "rename symbol",
    },
];

static G_ENTRIES: &[Entry] = &[
    Entry {
        key: "g",
        desc: "top of buffer",
    },
    Entry {
        key: "j",
        desc: "display-line down",
    },
    Entry {
        key: "k",
        desc: "display-line up",
    },
    Entry {
        key: "t",
        desc: "next tab",
    },
    Entry {
        key: "T",
        desc: "prev tab",
    },
    Entry {
        key: "d",
        desc: "goto definition",
    },
    Entry {
        key: "D",
        desc: "goto declaration",
    },
    Entry {
        key: "r",
        desc: "goto references",
    },
    Entry {
        key: "i",
        desc: "goto implementation",
    },
    Entry {
        key: "y",
        desc: "goto type def",
    },
];

static BRACKET_RIGHT_ENTRIES: &[Entry] = &[
    Entry {
        key: "b",
        desc: "next buffer",
    },
    Entry {
        key: "d",
        desc: "next diagnostic",
    },
    Entry {
        key: "D",
        desc: "next error",
    },
];

static BRACKET_LEFT_ENTRIES: &[Entry] = &[
    Entry {
        key: "b",
        desc: "prev buffer",
    },
    Entry {
        key: "d",
        desc: "prev diagnostic",
    },
    Entry {
        key: "D",
        desc: "prev error",
    },
];

static CTRL_W_ENTRIES: &[Entry] = &[
    Entry {
        key: "h",
        desc: "focus left",
    },
    Entry {
        key: "j",
        desc: "focus down",
    },
    Entry {
        key: "k",
        desc: "focus up",
    },
    Entry {
        key: "l",
        desc: "focus right",
    },
    Entry {
        key: "w",
        desc: "focus next",
    },
    Entry {
        key: "W",
        desc: "focus prev",
    },
    Entry {
        key: "c",
        desc: "close window",
    },
    Entry {
        key: "q",
        desc: "quit/close",
    },
    Entry {
        key: "o",
        desc: "close others",
    },
    Entry {
        key: "x",
        desc: "swap with sibling",
    },
    Entry {
        key: "T",
        desc: "move to new tab",
    },
    Entry {
        key: "n",
        desc: "new split",
    },
    Entry {
        key: "+",
        desc: "taller",
    },
    Entry {
        key: "-",
        desc: "shorter",
    },
    Entry {
        key: ">",
        desc: "wider",
    },
    Entry {
        key: "<",
        desc: "narrower",
    },
    Entry {
        key: "=",
        desc: "equalize",
    },
    Entry {
        key: "_",
        desc: "maximize height",
    },
    Entry {
        key: "|",
        desc: "maximize width",
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn g_entries_contain_required_keys() {
        let e = entries(Prefix::G);
        let keys: Vec<&str> = e.iter().map(|e| e.key).collect();
        assert!(keys.contains(&"t"), "missing gt");
        assert!(keys.contains(&"T"), "missing gT");
        assert!(keys.contains(&"d"), "missing gd");
        assert!(keys.contains(&"r"), "missing gr");
    }

    #[test]
    fn ctrl_w_entries_contain_required_keys() {
        let e = entries(Prefix::CtrlW);
        let keys: Vec<&str> = e.iter().map(|e| e.key).collect();
        assert!(keys.contains(&"j"), "missing j");
        assert!(keys.contains(&"k"), "missing k");
        assert!(keys.contains(&"h"), "missing h");
        assert!(keys.contains(&"l"), "missing l");
        assert!(keys.contains(&"+"), "missing +");
        assert!(keys.contains(&"-"), "missing -");
    }

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
