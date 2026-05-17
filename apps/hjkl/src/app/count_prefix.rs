//! Count-prefix buffering helpers.
//!
//! Owns: [`App::flush_pending_count_to_engine`] — drains the digit count
//! accumulated before a Normal-mode chord and replays each digit to the active
//! editor as bare `Char` key events.

use super::App;

impl App {
    // ── Count-prefix helpers ──────────────────────────────────────────────

    /// Drain the pending digit count and replay each digit to the active
    /// editor as a bare `Char` key event.  No-ops when the count is empty
    /// (drain returns an empty string), so callers may omit an
    /// `is_empty` guard if they prefer — the existing guards are kept at
    /// call sites for clarity and symmetry with the surrounding flow.
    pub(crate) fn flush_pending_count_to_engine(&mut self) {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let digits = self.pending_count.drain_as_digits();
        for d in digits.chars() {
            hjkl_vim::handle_key(
                &mut self.active_mut().editor,
                KeyEvent::new(KeyCode::Char(d), KeyModifiers::NONE),
            );
        }
    }
}
