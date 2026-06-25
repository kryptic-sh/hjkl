//! Kitty keyboard protocol support for crossterm-based kryptic-sh TUIs.
//!
//! This crate is toolkit-agnostic (crossterm-only) and is intentionally
//! reusable across all kryptic-sh TUI binaries — **hjkl**, **sqeel**, and
//! any future crossterm-based tool — without pulling in ratatui or any
//! app-level crate.
//!
//! # What this crate does
//!
//! The [Kitty keyboard protocol] adds a "progressive enhancement" flag layer
//! on top of the traditional terminal key encoding. This crate uses only the
//! lowest flag: [`DISAMBIGUATE_ESCAPE_CODES`].
//!
//! ## [`DISAMBIGUATE_ESCAPE_CODES`]
//!
//! Under this flag the terminal sends **unambiguous** CSI-u sequences for
//! keys that would otherwise alias:
//!
//! | Key | Traditional byte | CSI-u |
//! |-----|-----------------|-------|
//! | Ctrl+[ | `\x1b` (= Esc!) | `\x1b[91;5u` |
//! | Ctrl+I | `\x09` (= Tab!) | `\x1b[105;5u` |
//! | Ctrl+M | `\x0d` (= Enter!) | `\x1b[109;5u` |
//!
//! This makes it possible to bind Ctrl+[, Ctrl+I, Ctrl+M independently of
//! Esc, Tab, and Enter (important for VSCode-discipline editors that bind
//! these as indent/outdent/comment chords).
//!
//! **Vim disciplines MUST normalize** these back to Esc/Tab/Enter so that
//! `Ctrl+[ = Esc` (exit insert), etc. continue to work. Use
//! [`normalize_legacy`] in the vim key dispatch path.
//!
//! # Safety of unconditional push
//!
//! [`enable`] pushes the enhancement flags **without** calling
//! [`is_supported`]. This is intentional: the blocking terminal round-trip
//! performed by `supports_keyboard_enhancement()` hangs PTY harnesses, SSH
//! connections to non-responsive terminals, and similar environments.
//! Pushing the flags to a non-supporting terminal is harmless — the terminal
//! ignores the escape sequence and continues sending legacy bytes. Only call
//! [`is_supported`] when you genuinely need to branch on support (e.g. in a
//! user-facing diagnostic), never in the startup path.
//!
//! [Kitty keyboard protocol]: https://sw.kovidgoyal.net/kitty/keyboard-protocol/
//! [`DISAMBIGUATE_ESCAPE_CODES`]: crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES

use crossterm::{
    event::{
        KeyCode, KeyEvent, KeyModifiers, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
        PushKeyboardEnhancementFlags,
    },
    execute,
};

/// Push `DISAMBIGUATE_ESCAPE_CODES` onto the terminal's keyboard enhancement
/// flag stack.
///
/// Call this once during terminal setup, **unconditionally** — do NOT gate on
/// [`is_supported`] in the startup path (see crate-level docs).
///
/// Paired with [`disable`] in teardown.
///
/// # Errors
///
/// Propagates any I/O error from writing the escape sequence to `w`.
pub fn enable<W: std::io::Write>(w: &mut W) -> std::io::Result<()> {
    execute!(
        w,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )
}

/// Pop the keyboard enhancement flags, restoring the previous state.
///
/// Call this in teardown (mirror of [`enable`]).
///
/// # Errors
///
/// Propagates any I/O error from writing the escape sequence to `w`.
pub fn disable<W: std::io::Write>(w: &mut W) -> std::io::Result<()> {
    execute!(w, PopKeyboardEnhancementFlags)
}

/// Map disambiguated Ctrl+\[, Ctrl+I, Ctrl+M back to their legacy aliases.
///
/// Under `DISAMBIGUATE_ESCAPE_CODES` the terminal sends distinct CSI-u
/// sequences for these keys, so crossterm decodes them as:
///
/// | Received CSI-u | crossterm `KeyEvent` | This function returns |
/// |---|---|---|
/// | `\x1b[91;5u`  | `Char('[')` + CONTROL | `Esc`  (no modifiers) |
/// | `\x1b[105;5u` | `Char('i')` + CONTROL | `Tab`  (no modifiers) |
/// | `\x1b[73;5u`  | `Char('I')` + CONTROL | `Tab`  (no modifiers) |
/// | `\x1b[109;5u` | `Char('m')` + CONTROL | `Enter` (no modifiers) |
/// | `\x1b[77;5u`  | `Char('M')` + CONTROL | `Enter` (no modifiers) |
///
/// **Apply this in vim-mode key dispatch only.** VSCode-discipline editors
/// should pass the raw event through so that Ctrl+[ can be bound to outdent,
/// Ctrl+I to something else, etc.
///
/// All other keys are returned unchanged.
pub fn normalize_legacy(key: KeyEvent) -> KeyEvent {
    if key.modifiers == KeyModifiers::CONTROL {
        match key.code {
            // Ctrl+[ → Esc (vim: exit insert / return to normal)
            KeyCode::Char('[') => {
                return KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
            }
            // Ctrl+I / Ctrl+Shift+I → Tab
            KeyCode::Char('i') | KeyCode::Char('I') => {
                return KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
            }
            // Ctrl+M / Ctrl+Shift+M → Enter
            KeyCode::Char('m') | KeyCode::Char('M') => {
                return KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
            }
            _ => {}
        }
    }
    key
}

/// Check whether the current terminal supports the Kitty keyboard protocol.
///
/// # Warning — BLOCKING
///
/// This function performs a **blocking terminal round-trip** (it writes a
/// query escape sequence and reads the response). It will **hang** in:
/// - PTY harnesses (no controlling terminal)
/// - SSH sessions to non-responsive terminals
/// - Any context without a live terminal on stdin/stdout
///
/// **Do NOT call this in the startup path.** hjkl and other kryptic-sh tools
/// call [`enable`] unconditionally without querying support first — pushing
/// the flags to a non-supporting terminal is a harmless no-op.
///
/// Only call this in explicit user-facing diagnostics (e.g. a `:checkterm`
/// command) where the caller knows a real terminal is present and can handle
/// a brief pause.
///
/// # Errors
///
/// Propagates I/O errors from the terminal round-trip.
pub fn is_supported() -> std::io::Result<bool> {
    crossterm::terminal::supports_keyboard_enhancement()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    // ── normalize_legacy ────────────────────────────────────────────────────

    #[test]
    fn normalize_ctrl_bracket_to_esc() {
        let key = KeyEvent::new(KeyCode::Char('['), KeyModifiers::CONTROL);
        let out = normalize_legacy(key);
        assert_eq!(out.code, KeyCode::Esc);
        assert_eq!(out.modifiers, KeyModifiers::NONE);
    }

    #[test]
    fn normalize_ctrl_i_lowercase_to_tab() {
        let key = KeyEvent::new(KeyCode::Char('i'), KeyModifiers::CONTROL);
        let out = normalize_legacy(key);
        assert_eq!(out.code, KeyCode::Tab);
        assert_eq!(out.modifiers, KeyModifiers::NONE);
    }

    #[test]
    fn normalize_ctrl_i_uppercase_to_tab() {
        let key = KeyEvent::new(KeyCode::Char('I'), KeyModifiers::CONTROL);
        let out = normalize_legacy(key);
        assert_eq!(out.code, KeyCode::Tab);
        assert_eq!(out.modifiers, KeyModifiers::NONE);
    }

    #[test]
    fn normalize_ctrl_m_lowercase_to_enter() {
        let key = KeyEvent::new(KeyCode::Char('m'), KeyModifiers::CONTROL);
        let out = normalize_legacy(key);
        assert_eq!(out.code, KeyCode::Enter);
        assert_eq!(out.modifiers, KeyModifiers::NONE);
    }

    #[test]
    fn normalize_ctrl_m_uppercase_to_enter() {
        let key = KeyEvent::new(KeyCode::Char('M'), KeyModifiers::CONTROL);
        let out = normalize_legacy(key);
        assert_eq!(out.code, KeyCode::Enter);
        assert_eq!(out.modifiers, KeyModifiers::NONE);
    }

    #[test]
    fn normalize_plain_bracket_passes_through() {
        // Plain '[' without CONTROL must not be touched.
        let key = KeyEvent::new(KeyCode::Char('['), KeyModifiers::NONE);
        let out = normalize_legacy(key);
        assert_eq!(out.code, KeyCode::Char('['));
        assert_eq!(out.modifiers, KeyModifiers::NONE);
    }

    #[test]
    fn normalize_ctrl_a_passes_through() {
        let key = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL);
        let out = normalize_legacy(key);
        assert_eq!(out.code, KeyCode::Char('a'));
        assert_eq!(out.modifiers, KeyModifiers::CONTROL);
    }

    #[test]
    fn normalize_plain_x_passes_through() {
        let key = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        let out = normalize_legacy(key);
        assert_eq!(out.code, KeyCode::Char('x'));
        assert_eq!(out.modifiers, KeyModifiers::NONE);
    }

    // ── enable / disable emit bytes ─────────────────────────────────────────

    // ANSI byte emission is unix/ANSI-terminal-specific: crossterm routes the
    // keyboard-enhancement commands through the ANSI path on unix, but on the
    // Windows console they error / write nothing (the protocol is unsupported
    // there — hjkl degrades to legacy). Gate the byte-shape assertions to unix;
    // `normalize_legacy` (the cross-platform logic) is covered above.
    #[cfg(unix)]
    #[test]
    fn enable_emits_nonempty_csi_sequence() {
        let mut buf = Vec::<u8>::new();
        enable(&mut buf).expect("enable into Vec");
        assert!(!buf.is_empty(), "enable must write bytes");
        // Must start with ESC [
        assert_eq!(&buf[..2], b"\x1b[", "enable output must start with ESC [");
    }

    #[cfg(unix)]
    #[test]
    fn disable_emits_nonempty_csi_sequence() {
        let mut buf = Vec::<u8>::new();
        disable(&mut buf).expect("disable into Vec");
        assert!(!buf.is_empty(), "disable must write bytes");
        assert_eq!(&buf[..2], b"\x1b[", "disable output must start with ESC [");
    }
}
