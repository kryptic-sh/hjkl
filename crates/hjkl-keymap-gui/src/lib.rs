//! Floem `KeyEvent` → [`hjkl_keymap::KeyEvent`] adapter.
//!
//! Wraps the floem input boundary so GUI renderer code is the only place
//! that imports `floem::keyboard`. Mirrors the `hjkl-keymap-tui` crossterm
//! adapter's shape and normalization rules for the floem-based GUI
//! renderer.
//!
//! Unsupported floem keys (dead keys, unidentified keys, modifier-only
//! presses) and key-release events are mapped to `None` so callers can skip
//! them.
//!
//! # No `to_floem`
//!
//! `hjkl-keymap-tui` also provides `to_crossterm` for replaying unbound
//! sequences back through the input boundary. There is no floem equivalent
//! here: `floem::keyboard::KeyEvent` wraps `winit::event::KeyEvent`, which
//! has a private, platform-specific field and no public constructor. There
//! is no safe way to synthesize one outside of `winit` itself.

use floem::keyboard::{Key, Modifiers as FloemModifiers, NamedKey};
use hjkl_keymap::{KeyCode, KeyEvent, KeyModifiers};

/// Convert a floem `KeyEvent` to a `hjkl_keymap::KeyEvent`.
///
/// Returns `None` for key events that are not presses (key-release), or for
/// keys that have no meaningful representation in the keymap (dead keys,
/// unidentified keys, modifier-only presses).
pub fn from_floem(ev: &floem::keyboard::KeyEvent) -> Option<KeyEvent> {
    // Only handle key presses — ignore releases. `ElementState::is_pressed`
    // is an inherent method, so this works without naming `ElementState`
    // itself (it lives in `winit`, which floem does not re-export).
    if !ev.key.state.is_pressed() {
        return None;
    }

    let code = floem_key_to_keymap(&ev.key.logical_key)?;
    Some(build_key_event(code, ev.modifiers))
}

/// Combine a mapped [`KeyCode`] with floem modifiers into a
/// `hjkl_keymap::KeyEvent`, applying the same SHIFT normalization
/// `hjkl-keymap-tui` uses.
fn build_key_event(code: KeyCode, mods: FloemModifiers) -> KeyEvent {
    let mut modifiers = floem_mods_to_keymap(mods);
    // SHIFT for plain Char events is redundant — the case is already in the
    // char (vim convention). floem's `Key::Character` already reflects the
    // shifted glyph (e.g. `'B'` rather than `'b'` + SHIFT), so normalize it
    // away to match bindings registered as `ch('B')`. SHIFT remains
    // distinguishing for non-Char codes (e.g. `<S-Tab>`).
    if matches!(code, KeyCode::Char(_)) {
        modifiers.remove(KeyModifiers::SHIFT);
    }
    KeyEvent::new(code, modifiers)
}

/// Map a floem logical `Key` to a `hjkl_keymap::KeyCode`.
///
/// Returns `None` for keys with no meaningful representation in the keymap
/// (dead keys, unidentified keys, and named keys the keymap has no code
/// for, e.g. pure modifier keys, media keys, `CapsLock`).
fn floem_key_to_keymap(key: &Key) -> Option<KeyCode> {
    Some(match key {
        Key::Character(s) => KeyCode::Char(s.chars().next()?),
        Key::Named(named) => match named {
            NamedKey::Enter => KeyCode::Enter,
            NamedKey::Escape => KeyCode::Esc,
            NamedKey::Tab => KeyCode::Tab,
            NamedKey::Backspace => KeyCode::Backspace,
            NamedKey::Delete => KeyCode::Delete,
            NamedKey::Insert => KeyCode::Insert,
            NamedKey::ArrowUp => KeyCode::Up,
            NamedKey::ArrowDown => KeyCode::Down,
            NamedKey::ArrowLeft => KeyCode::Left,
            NamedKey::ArrowRight => KeyCode::Right,
            NamedKey::Home => KeyCode::Home,
            NamedKey::End => KeyCode::End,
            NamedKey::PageUp => KeyCode::PageUp,
            NamedKey::PageDown => KeyCode::PageDown,
            NamedKey::F1 => KeyCode::F(1),
            NamedKey::F2 => KeyCode::F(2),
            NamedKey::F3 => KeyCode::F(3),
            NamedKey::F4 => KeyCode::F(4),
            NamedKey::F5 => KeyCode::F(5),
            NamedKey::F6 => KeyCode::F(6),
            NamedKey::F7 => KeyCode::F(7),
            NamedKey::F8 => KeyCode::F(8),
            NamedKey::F9 => KeyCode::F(9),
            NamedKey::F10 => KeyCode::F(10),
            NamedKey::F11 => KeyCode::F(11),
            NamedKey::F12 => KeyCode::F(12),
            NamedKey::Space => KeyCode::Char(' '),
            // Unsupported / no-op named keys (modifiers, media keys,
            // CapsLock, higher F-keys the keymap has no dedicated use for,
            // etc.).
            _ => return None,
        },
        // Dead keys and unidentified keys have no keymap representation.
        Key::Dead(_) | Key::Unidentified(_) => return None,
    })
}

fn floem_mods_to_keymap(mods: FloemModifiers) -> KeyModifiers {
    let mut out = KeyModifiers::NONE;
    if mods.shift() {
        out |= KeyModifiers::SHIFT;
    }
    if mods.control() {
        out |= KeyModifiers::CTRL;
    }
    if mods.alt() {
        out |= KeyModifiers::ALT;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shift_stripped_for_uppercase_char() {
        let ev = build_key_event(KeyCode::Char('B'), FloemModifiers::SHIFT);
        assert_eq!(ev, KeyEvent::new(KeyCode::Char('B'), KeyModifiers::NONE));
    }

    #[test]
    fn shift_stripped_for_shifted_symbol() {
        let ev = build_key_event(KeyCode::Char('<'), FloemModifiers::SHIFT);
        assert_eq!(ev, KeyEvent::new(KeyCode::Char('<'), KeyModifiers::NONE));
    }

    #[test]
    fn ctrl_preserved_with_char() {
        let ev = build_key_event(KeyCode::Char('w'), FloemModifiers::CONTROL);
        assert_eq!(ev, KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CTRL));
    }

    #[test]
    fn ctrl_shift_with_char_keeps_only_ctrl() {
        let ev = build_key_event(
            KeyCode::Char('A'),
            FloemModifiers::CONTROL | FloemModifiers::SHIFT,
        );
        assert_eq!(ev, KeyEvent::new(KeyCode::Char('A'), KeyModifiers::CTRL));
    }

    #[test]
    fn shift_preserved_for_tab() {
        let ev = build_key_event(KeyCode::Tab, FloemModifiers::SHIFT);
        assert_eq!(ev, KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT));
    }

    #[test]
    fn shift_preserved_for_f_key() {
        let ev = build_key_event(KeyCode::F(5), FloemModifiers::SHIFT);
        assert_eq!(ev, KeyEvent::new(KeyCode::F(5), KeyModifiers::SHIFT));
    }

    #[test]
    fn alt_preserved_with_char() {
        let ev = build_key_event(KeyCode::Char('x'), FloemModifiers::ALT);
        assert_eq!(ev, KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT));
    }

    #[test]
    fn named_key_maps_to_keycode() {
        assert_eq!(
            floem_key_to_keymap(&Key::Named(NamedKey::Enter)),
            Some(KeyCode::Enter)
        );
        assert_eq!(
            floem_key_to_keymap(&Key::Named(NamedKey::Escape)),
            Some(KeyCode::Esc)
        );
        assert_eq!(
            floem_key_to_keymap(&Key::Named(NamedKey::Backspace)),
            Some(KeyCode::Backspace)
        );
        assert_eq!(
            floem_key_to_keymap(&Key::Named(NamedKey::Tab)),
            Some(KeyCode::Tab)
        );
        assert_eq!(
            floem_key_to_keymap(&Key::Named(NamedKey::ArrowUp)),
            Some(KeyCode::Up)
        );
        assert_eq!(
            floem_key_to_keymap(&Key::Named(NamedKey::ArrowDown)),
            Some(KeyCode::Down)
        );
        assert_eq!(
            floem_key_to_keymap(&Key::Named(NamedKey::ArrowLeft)),
            Some(KeyCode::Left)
        );
        assert_eq!(
            floem_key_to_keymap(&Key::Named(NamedKey::ArrowRight)),
            Some(KeyCode::Right)
        );
        assert_eq!(
            floem_key_to_keymap(&Key::Named(NamedKey::Home)),
            Some(KeyCode::Home)
        );
        assert_eq!(
            floem_key_to_keymap(&Key::Named(NamedKey::End)),
            Some(KeyCode::End)
        );
        assert_eq!(
            floem_key_to_keymap(&Key::Named(NamedKey::PageUp)),
            Some(KeyCode::PageUp)
        );
        assert_eq!(
            floem_key_to_keymap(&Key::Named(NamedKey::PageDown)),
            Some(KeyCode::PageDown)
        );
        assert_eq!(
            floem_key_to_keymap(&Key::Named(NamedKey::Delete)),
            Some(KeyCode::Delete)
        );
        assert_eq!(
            floem_key_to_keymap(&Key::Named(NamedKey::Insert)),
            Some(KeyCode::Insert)
        );
        assert_eq!(
            floem_key_to_keymap(&Key::Named(NamedKey::F5)),
            Some(KeyCode::F(5))
        );
    }

    #[test]
    fn character_key_maps_to_char_code() {
        assert_eq!(
            floem_key_to_keymap(&Key::Character("a".into())),
            Some(KeyCode::Char('a'))
        );
    }

    #[test]
    fn space_named_key_maps_to_char_space() {
        assert_eq!(
            floem_key_to_keymap(&Key::Named(NamedKey::Space)),
            Some(KeyCode::Char(' '))
        );
    }

    #[test]
    fn unmappable_named_key_returns_none() {
        assert_eq!(floem_key_to_keymap(&Key::Named(NamedKey::CapsLock)), None);
    }

    #[test]
    fn dead_key_returns_none() {
        assert_eq!(floem_key_to_keymap(&Key::Dead(Some('a'))), None);
    }

    #[test]
    fn empty_character_string_returns_none() {
        assert_eq!(floem_key_to_keymap(&Key::Character("".into())), None);
    }

    #[test]
    fn modifiers_translate_independently() {
        assert_eq!(
            floem_mods_to_keymap(FloemModifiers::empty()),
            KeyModifiers::NONE
        );
        assert_eq!(
            floem_mods_to_keymap(FloemModifiers::SHIFT),
            KeyModifiers::SHIFT
        );
        assert_eq!(
            floem_mods_to_keymap(FloemModifiers::CONTROL),
            KeyModifiers::CTRL
        );
        assert_eq!(floem_mods_to_keymap(FloemModifiers::ALT), KeyModifiers::ALT);
        assert_eq!(
            floem_mods_to_keymap(
                FloemModifiers::SHIFT | FloemModifiers::CONTROL | FloemModifiers::ALT
            ),
            KeyModifiers::SHIFT | KeyModifiers::CTRL | KeyModifiers::ALT
        );
    }
}
