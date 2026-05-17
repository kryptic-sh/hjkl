//! Renderer-agnostic notification bus: severity-tagged toasts with a
//! ring-buffer history and auto-dismiss TTLs.
//!
//! No TUI or renderer types are referenced here — the ratatui adapter lives
//! in `hjkl-holler-tui`.
//!
//! # Quick start
//!
//! ```rust
//! use hjkl_holler::{HollerBus, Severity};
//! use std::time::SystemTime;
//!
//! let mut bus = HollerBus::new();
//! bus.info("file saved");
//! bus.warn("trailing whitespace");
//! bus.error("E45: readonly option is set");
//!
//! let now = SystemTime::now();
//! let active: Vec<_> = bus.active(now).collect();
//! assert_eq!(active.len(), 3);
//! ```

use std::collections::VecDeque;
use std::time::{Duration, SystemTime};

// ── Severity ──────────────────────────────────────────────────────────────────

/// Toast severity level.
///
/// Controls the TTL (how long the toast stays in the active stack) and the
/// border colour used by the TUI renderer.
///
/// `#[non_exhaustive]` — new variants may be added in minor releases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Severity {
    /// Neutral acknowledgement or echo. TTL 2 s.
    Info,
    /// Non-fatal warning. TTL 4 s.
    Warn,
    /// Hard error or failure. TTL 6 s.
    Error,
}

impl Severity {
    /// Default TTL for this severity level.
    pub fn default_ttl(self) -> Duration {
        match self {
            Severity::Info => Duration::from_secs(2),
            Severity::Warn => Duration::from_secs(4),
            Severity::Error => Duration::from_secs(6),
        }
    }

    /// Short uppercase label for display.
    pub fn label(self) -> &'static str {
        match self {
            Severity::Info => "INFO",
            Severity::Warn => "WARN",
            Severity::Error => "ERROR",
        }
    }
}

// ── Holler ────────────────────────────────────────────────────────────────────

/// A single notification entry.
///
/// `#[non_exhaustive]` — new fields may be added in minor releases.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Holler {
    /// Monotonically increasing identifier within one `HollerBus` instance.
    pub id: u64,
    /// Wall-clock time when this notification was pushed.
    pub ts: SystemTime,
    /// Severity level.
    pub severity: Severity,
    /// Notification body text.
    pub body: String,
    /// How long this notification stays in the active (visible) stack.
    pub ttl: Duration,
    /// How many times a duplicate consecutive message was suppressed.
    /// `1` means this is the original (no duplicates collapsed into it yet).
    pub count: u32,
    /// Whether the user explicitly dismissed this entry before its TTL expired.
    pub dismissed: bool,
}

impl Holler {
    /// Returns `true` when this notification has expired or been dismissed.
    ///
    /// ```rust
    /// use hjkl_holler::HollerBus;
    /// use std::time::SystemTime;
    ///
    /// let mut bus = HollerBus::new();
    /// bus.info("test");
    /// let h = bus.history().next().unwrap();
    /// assert!(!h.is_expired(SystemTime::now()));
    /// ```
    pub fn is_expired(&self, now: SystemTime) -> bool {
        if self.dismissed {
            return true;
        }
        now.duration_since(self.ts)
            .map(|elapsed| elapsed >= self.ttl)
            .unwrap_or(false)
    }

    /// Returns `true` when this notification is within the last 500 ms of its
    /// TTL — used by the renderer to apply a soft-fade (`Modifier::DIM`).
    pub fn is_fading(&self, now: SystemTime) -> bool {
        if self.dismissed {
            return true;
        }
        let Ok(elapsed) = now.duration_since(self.ts) else {
            return false;
        };
        if elapsed >= self.ttl {
            return true;
        }
        self.ttl.saturating_sub(elapsed) < Duration::from_millis(500)
    }

    /// Formatted body including duplicate-count badge when > 1.
    pub fn display_body(&self) -> String {
        if self.count > 1 {
            format!("{} (\u{d7}{})", self.body, self.count)
        } else {
            self.body.clone()
        }
    }
}

// ── HollerBus ─────────────────────────────────────────────────────────────────

/// Maximum number of entries retained in the history ring.
pub const DEFAULT_HISTORY_CAP: usize = 200;

/// Notification bus: push messages in, query active/history out.
///
/// `#[non_exhaustive]` — new fields may be added in minor releases.
///
/// # Example
///
/// ```rust
/// use hjkl_holler::HollerBus;
/// use std::time::SystemTime;
///
/// let mut bus = HollerBus::new();
/// let id = bus.info("saved");
/// assert_eq!(bus.active(SystemTime::now()).count(), 1);
/// bus.dismiss(id);
/// assert_eq!(bus.active(SystemTime::now()).count(), 0);
/// ```
#[non_exhaustive]
pub struct HollerBus {
    /// Ring-buffer of all pushed notifications (capped at [`DEFAULT_HISTORY_CAP`]).
    pub history: VecDeque<Holler>,
    /// Counter for the next notification id.
    pub next_id: u64,
}

impl Default for HollerBus {
    fn default() -> Self {
        Self::new()
    }
}

impl HollerBus {
    /// Create a new empty bus with the default history capacity.
    pub fn new() -> Self {
        Self {
            history: VecDeque::with_capacity(DEFAULT_HISTORY_CAP),
            next_id: 0,
        }
    }

    /// Push a notification with an explicit severity and body.
    ///
    /// Consecutive duplicate bodies (same body as the most recent entry) are
    /// collapsed: the existing entry's count is incremented instead of
    /// inserting a new one. Returns the id of the affected entry.
    ///
    /// ```rust
    /// use hjkl_holler::{HollerBus, Severity};
    ///
    /// let mut bus = HollerBus::new();
    /// let id1 = bus.push(Severity::Info, "same");
    /// let id2 = bus.push(Severity::Info, "same");
    /// assert_eq!(id1, id2, "duplicate collapses into first entry");
    /// assert_eq!(bus.history().next().unwrap().count, 2);
    /// ```
    pub fn push(&mut self, severity: Severity, body: impl Into<String>) -> u64 {
        let body: String = body.into();
        let ttl = severity.default_ttl();

        // Throttle: collapse consecutive duplicate body + severity pairs.
        if let Some(last) = self.history.back_mut()
            && last.body == body
            && last.severity == severity
        {
            last.count = last.count.saturating_add(1);
            // Reset ts and dismissed so the toast reappears.
            last.ts = SystemTime::now();
            last.dismissed = false;
            return last.id;
        }

        let id = self.next_id;
        self.next_id += 1;

        let entry = Holler {
            id,
            ts: SystemTime::now(),
            severity,
            body,
            ttl,
            count: 1,
            dismissed: false,
        };

        if self.history.len() >= DEFAULT_HISTORY_CAP {
            self.history.pop_front();
        }
        self.history.push_back(entry);
        id
    }

    /// Push an `Info` notification (TTL 2 s).
    pub fn info(&mut self, body: impl Into<String>) -> u64 {
        self.push(Severity::Info, body)
    }

    /// Push a `Warn` notification (TTL 4 s).
    pub fn warn(&mut self, body: impl Into<String>) -> u64 {
        self.push(Severity::Warn, body)
    }

    /// Push an `Error` notification (TTL 6 s).
    pub fn error(&mut self, body: impl Into<String>) -> u64 {
        self.push(Severity::Error, body)
    }

    /// Iterator over notifications whose TTL has not yet expired, in
    /// push order (oldest first). Passes `now` to each entry's
    /// [`Holler::is_expired`] check so callers control the clock.
    pub fn active(&self, now: SystemTime) -> impl Iterator<Item = &Holler> {
        self.history.iter().filter(move |h| !h.is_expired(now))
    }

    /// Iterator over all entries in the history ring, oldest first.
    pub fn history(&self) -> impl Iterator<Item = &Holler> {
        self.history.iter()
    }

    /// Explicitly dismiss the notification with the given id.
    /// No-op if the id is not found.
    pub fn dismiss(&mut self, id: u64) {
        if let Some(h) = self.history.iter_mut().find(|h| h.id == id) {
            h.dismissed = true;
        }
    }

    /// Dismiss all currently-active (non-expired, non-dismissed) notifications.
    pub fn clear_active(&mut self) {
        let now = SystemTime::now();
        for h in &mut self.history {
            if !h.is_expired(now) {
                h.dismissed = true;
            }
        }
    }

    /// Return the body of the most recent notification, or `None` if the
    /// history is empty. Used in tests to mirror the old `status_message` checks.
    pub fn last_body(&self) -> Option<&str> {
        self.history.back().map(|h| h.body.as_str())
    }

    /// Return the body of the most recent notification, or `""`.
    /// Convenience for test assertions.
    pub fn last_body_or_empty(&self) -> &str {
        self.last_body().unwrap_or("")
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> SystemTime {
        SystemTime::now()
    }

    #[test]
    fn info_warn_error_default_ttl() {
        assert_eq!(Severity::Info.default_ttl(), Duration::from_secs(2));
        assert_eq!(Severity::Warn.default_ttl(), Duration::from_secs(4));
        assert_eq!(Severity::Error.default_ttl(), Duration::from_secs(6));
    }

    #[test]
    fn push_returns_incrementing_ids() {
        let mut bus = HollerBus::new();
        let a = bus.info("a");
        let b = bus.info("b");
        let c = bus.info("c");
        assert!(a < b && b < c);
    }

    #[test]
    fn active_returns_non_expired_entries() {
        let mut bus = HollerBus::new();
        bus.info("hello");
        assert_eq!(bus.active(now()).count(), 1);
    }

    #[test]
    fn active_excludes_expired_entries() {
        let mut bus = HollerBus::new();
        // Manually insert a past-TTL entry.
        let entry = Holler {
            id: 0,
            ts: SystemTime::UNIX_EPOCH, // far in the past
            severity: Severity::Info,
            body: "old".into(),
            ttl: Duration::from_secs(1),
            count: 1,
            dismissed: false,
        };
        bus.history.push_back(entry);
        bus.next_id = 1;
        assert_eq!(bus.active(now()).count(), 0);
    }

    #[test]
    fn dismiss_removes_from_active() {
        let mut bus = HollerBus::new();
        let id = bus.info("test");
        assert_eq!(bus.active(now()).count(), 1);
        bus.dismiss(id);
        assert_eq!(bus.active(now()).count(), 0);
    }

    #[test]
    fn dismiss_unknown_id_is_noop() {
        let mut bus = HollerBus::new();
        bus.info("ok");
        bus.dismiss(999); // no such id
        assert_eq!(bus.active(now()).count(), 1);
    }

    #[test]
    fn clear_active_dismisses_all() {
        let mut bus = HollerBus::new();
        bus.info("a");
        bus.warn("b");
        bus.error("c");
        assert_eq!(bus.active(now()).count(), 3);
        bus.clear_active();
        assert_eq!(bus.active(now()).count(), 0);
    }

    #[test]
    fn history_returns_all_entries() {
        let mut bus = HollerBus::new();
        bus.info("x");
        bus.warn("y");
        // history includes all, not just active
        assert_eq!(bus.history().count(), 2);
    }

    #[test]
    fn duplicate_consecutive_collapses_count() {
        let mut bus = HollerBus::new();
        let id1 = bus.info("same message");
        let id2 = bus.info("same message");
        let id3 = bus.info("same message");
        assert_eq!(id1, id2);
        assert_eq!(id2, id3);
        assert_eq!(bus.history().count(), 1);
        assert_eq!(bus.history().next().unwrap().count, 3);
    }

    #[test]
    fn different_body_does_not_collapse() {
        let mut bus = HollerBus::new();
        bus.info("a");
        bus.info("b");
        assert_eq!(bus.history().count(), 2);
    }

    #[test]
    fn different_severity_same_body_does_not_collapse() {
        let mut bus = HollerBus::new();
        bus.info("msg");
        bus.warn("msg");
        assert_eq!(bus.history().count(), 2);
    }

    #[test]
    fn history_cap_evicts_oldest() {
        let mut bus = HollerBus::new();
        for i in 0..DEFAULT_HISTORY_CAP + 10 {
            bus.info(format!("msg {i}"));
        }
        assert_eq!(bus.history().count(), DEFAULT_HISTORY_CAP);
        // Oldest entries were evicted; most recent should be last.
        let last = bus.history().last().unwrap();
        assert!(last.body.ends_with(&format!("{}", DEFAULT_HISTORY_CAP + 9)));
    }

    #[test]
    fn display_body_shows_count_badge() {
        let mut bus = HollerBus::new();
        bus.info("dup");
        bus.info("dup");
        let entry = bus.history().next().unwrap();
        assert_eq!(entry.count, 2);
        assert!(
            entry.display_body().contains("(×2)"),
            "got: {}",
            entry.display_body()
        );
    }

    #[test]
    fn display_body_no_badge_when_count_one() {
        let mut bus = HollerBus::new();
        bus.info("single");
        let entry = bus.history().next().unwrap();
        assert_eq!(entry.display_body(), "single");
    }

    #[test]
    fn is_fading_false_when_fresh() {
        let mut bus = HollerBus::new();
        bus.info("fresh");
        let h = bus.history().next().unwrap();
        assert!(!h.is_fading(now()));
    }

    #[test]
    fn is_expired_false_when_fresh() {
        let mut bus = HollerBus::new();
        bus.info("fresh");
        let h = bus.history().next().unwrap();
        assert!(!h.is_expired(now()));
    }

    #[test]
    fn is_expired_true_when_dismissed() {
        let mut bus = HollerBus::new();
        let id = bus.info("x");
        bus.dismiss(id);
        let h = bus.history().next().unwrap();
        assert!(h.is_expired(now()));
    }

    #[test]
    fn severity_labels() {
        assert_eq!(Severity::Info.label(), "INFO");
        assert_eq!(Severity::Warn.label(), "WARN");
        assert_eq!(Severity::Error.label(), "ERROR");
    }

    #[test]
    fn last_body_or_empty_empty_bus() {
        let bus = HollerBus::new();
        assert_eq!(bus.last_body_or_empty(), "");
    }

    #[test]
    fn last_body_returns_most_recent() {
        let mut bus = HollerBus::new();
        bus.info("first");
        bus.info("second");
        assert_eq!(bus.last_body(), Some("second"));
    }

    #[test]
    fn default_constructs_empty_bus() {
        let bus = HollerBus::default();
        assert_eq!(bus.history().count(), 0);
        assert_eq!(bus.active(now()).count(), 0);
    }
}
