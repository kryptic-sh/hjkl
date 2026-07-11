//! Crossterm `KeyEvent` ↔ [`hjkl_keymap::KeyEvent`] adapter.
//!
//! Wraps the crossterm input boundary so TUI renderer code is the only place
//! that imports `crossterm`. Future renderer adapters (e.g. `hjkl-keymap-floem`)
//! follow the same pattern for their respective input layer.
//!
//! Unsupported crossterm key codes (e.g. media keys, modifier-only events)
//! are mapped to `None` so callers can skip them.

use crossterm::event::{KeyCode as CtKeyCode, KeyEvent as CtKeyEvent, KeyModifiers as CtKeyMods};
use hjkl_keymap::{KeyCode, KeyEvent, KeyModifiers};

/// Convert a crossterm `KeyEvent` to a `hjkl_keymap::KeyEvent`.
///
/// Returns `None` for key event kinds that are not presses (e.g. `Release`,
/// `Repeat` on platforms that distinguish them), or for key codes that have
/// no meaningful representation in the keymap (e.g. `Null`, `CapsLock`).
pub fn from_crossterm(ev: &CtKeyEvent) -> Option<KeyEvent> {
    // Only handle key presses — ignore release/repeat if the platform sends them.
    use crossterm::event::KeyEventKind;
    if ev.kind == KeyEventKind::Release {
        return None;
    }

    let code = ct_code_to_keymap(ev.code)?;
    let mut modifiers = ct_mods_to_keymap(ev.modifiers);
    // BackTab is Shift-Tab by definition. crossterm 0.29 pairs BackTab with
    // SHIFT on every platform path today, but inject it explicitly so the
    // mapping can't silently collapse to plain Tab if a backend omits it.
    if ev.code == CtKeyCode::BackTab {
        modifiers |= KeyModifiers::SHIFT;
    }
    // SHIFT for plain Char events is redundant — the case is already in the
    // char (vim convention). Some terminals (kitty, foot, wezterm w/ kitty
    // keyboard protocol) deliver `'B' + SHIFT`; others deliver `'B' + NONE`.
    // Normalize so bindings registered as `ch('B')` match either delivery.
    // SHIFT remains distinguishing for non-Char codes (Tab → Shift-Tab, etc.)
    if matches!(code, KeyCode::Char(_)) {
        modifiers.remove(KeyModifiers::SHIFT);
    }
    Some(KeyEvent::new(code, modifiers))
}

fn ct_code_to_keymap(code: CtKeyCode) -> Option<KeyCode> {
    Some(match code {
        CtKeyCode::Char(c) => KeyCode::Char(c),
        CtKeyCode::Enter => KeyCode::Enter,
        CtKeyCode::Esc => KeyCode::Esc,
        CtKeyCode::Tab => KeyCode::Tab,
        CtKeyCode::BackTab => {
            // BackTab is <S-Tab> — represented as Tab + SHIFT modifier.
            // `from_crossterm` injects SHIFT for BackTab explicitly.
            KeyCode::Tab
        }
        CtKeyCode::Backspace => KeyCode::Backspace,
        CtKeyCode::Delete => KeyCode::Delete,
        CtKeyCode::Insert => KeyCode::Insert,
        CtKeyCode::Up => KeyCode::Up,
        CtKeyCode::Down => KeyCode::Down,
        CtKeyCode::Left => KeyCode::Left,
        CtKeyCode::Right => KeyCode::Right,
        CtKeyCode::Home => KeyCode::Home,
        CtKeyCode::End => KeyCode::End,
        CtKeyCode::PageUp => KeyCode::PageUp,
        CtKeyCode::PageDown => KeyCode::PageDown,
        CtKeyCode::F(n) => KeyCode::F(n),
        // Unsupported / no-op codes.
        CtKeyCode::Null
        | CtKeyCode::CapsLock
        | CtKeyCode::ScrollLock
        | CtKeyCode::NumLock
        | CtKeyCode::PrintScreen
        | CtKeyCode::Pause
        | CtKeyCode::Menu
        | CtKeyCode::KeypadBegin
        | CtKeyCode::Media(_)
        | CtKeyCode::Modifier(_) => return None,
        // Catch-all for any future crossterm variants.
        #[allow(unreachable_patterns)]
        _ => return None,
    })
}

fn ct_mods_to_keymap(mods: CtKeyMods) -> KeyModifiers {
    let mut out = KeyModifiers::NONE;
    if mods.contains(CtKeyMods::SHIFT) {
        out |= KeyModifiers::SHIFT;
    }
    if mods.contains(CtKeyMods::CONTROL) {
        out |= KeyModifiers::CTRL;
    }
    if mods.contains(CtKeyMods::ALT) {
        out |= KeyModifiers::ALT;
    }
    out
}

/// Convert a `hjkl_keymap::KeyEvent` back to a `crossterm::event::KeyEvent`
/// for replaying unbound sequences or user maps to the engine.
pub fn to_crossterm(ev: &KeyEvent) -> CtKeyEvent {
    // `<S-Tab>` is represented internally as `Tab` + SHIFT (see
    // `from_crossterm`), but crossterm's canonical form is `BackTab`. Emit that
    // so a replayed unbound Shift-Tab matches what the terminal actually
    // delivers (and round-trips through `from_crossterm`) instead of the
    // never-emitted `Tab` + SHIFT.
    if ev.code == KeyCode::Tab && ev.modifiers.contains(KeyModifiers::SHIFT) {
        let mut mods = CtKeyMods::NONE;
        if ev.modifiers.contains(KeyModifiers::CTRL) {
            mods |= CtKeyMods::CONTROL;
        }
        mods |= CtKeyMods::SHIFT;
        if ev.modifiers.contains(KeyModifiers::ALT) {
            mods |= CtKeyMods::ALT;
        }
        return CtKeyEvent::new(CtKeyCode::BackTab, mods);
    }
    let code = match ev.code {
        KeyCode::Char(c) => CtKeyCode::Char(c),
        KeyCode::Enter => CtKeyCode::Enter,
        KeyCode::Esc => CtKeyCode::Esc,
        KeyCode::Tab => CtKeyCode::Tab,
        KeyCode::Backspace => CtKeyCode::Backspace,
        KeyCode::Delete => CtKeyCode::Delete,
        KeyCode::Insert => CtKeyCode::Insert,
        KeyCode::Up => CtKeyCode::Up,
        KeyCode::Down => CtKeyCode::Down,
        KeyCode::Left => CtKeyCode::Left,
        KeyCode::Right => CtKeyCode::Right,
        KeyCode::Home => CtKeyCode::Home,
        KeyCode::End => CtKeyCode::End,
        KeyCode::PageUp => CtKeyCode::PageUp,
        KeyCode::PageDown => CtKeyCode::PageDown,
        KeyCode::F(n) => CtKeyCode::F(n),
    };
    let mut mods = CtKeyMods::NONE;
    if ev.modifiers.contains(KeyModifiers::CTRL) {
        mods |= CtKeyMods::CONTROL;
    }
    if ev.modifiers.contains(KeyModifiers::SHIFT) {
        mods |= CtKeyMods::SHIFT;
    }
    if ev.modifiers.contains(KeyModifiers::ALT) {
        mods |= CtKeyMods::ALT;
    }
    CtKeyEvent::new(code, mods)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode as CK, KeyEvent as CKE, KeyEventKind, KeyModifiers as CM};

    fn ct_key(code: CK, mods: CM) -> CKE {
        CKE::new(code, mods)
    }

    #[test]
    fn shift_stripped_for_uppercase_char() {
        // Kitty-style: 'B' + SHIFT.
        let ev = from_crossterm(&ct_key(CK::Char('B'), CM::SHIFT)).unwrap();
        assert_eq!(ev, KeyEvent::new(KeyCode::Char('B'), KeyModifiers::NONE));
    }

    #[test]
    fn shift_stripped_for_shifted_symbol() {
        // '<' (shift-comma on US layout) sometimes arrives with SHIFT.
        let ev = from_crossterm(&ct_key(CK::Char('<'), CM::SHIFT)).unwrap();
        assert_eq!(ev, KeyEvent::new(KeyCode::Char('<'), KeyModifiers::NONE));
    }

    #[test]
    fn ctrl_preserved_with_char() {
        let ev = from_crossterm(&ct_key(CK::Char('w'), CM::CONTROL)).unwrap();
        assert_eq!(ev, KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CTRL));
    }

    #[test]
    fn ctrl_shift_with_char_keeps_only_ctrl() {
        // Edge case: Ctrl-Shift-A on kitty arrives as Char('A') + CTRL|SHIFT.
        // We strip SHIFT (case encodes it) but keep CTRL.
        let ev = from_crossterm(&ct_key(CK::Char('A'), CM::CONTROL | CM::SHIFT)).unwrap();
        assert_eq!(ev, KeyEvent::new(KeyCode::Char('A'), KeyModifiers::CTRL));
    }

    #[test]
    fn shift_preserved_for_tab() {
        let ev = from_crossterm(&ct_key(CK::Tab, CM::SHIFT)).unwrap();
        assert_eq!(ev, KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT));
    }

    #[test]
    fn backtab_maps_to_shift_tab_even_without_shift_bit() {
        // crossterm normally pairs BackTab with SHIFT, but the mapping must
        // not collapse to plain Tab if a backend omits the modifier.
        let ev = from_crossterm(&ct_key(CK::BackTab, CM::NONE)).unwrap();
        assert_eq!(ev, KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT));

        let ev = from_crossterm(&ct_key(CK::BackTab, CM::SHIFT)).unwrap();
        assert_eq!(ev, KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT));
    }

    #[test]
    fn shift_tab_serializes_back_to_backtab() {
        // Internally <S-Tab> is Tab+SHIFT; converting back to crossterm must
        // yield the canonical BackTab (what the terminal emits), not Tab+SHIFT.
        let ev = KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT);
        let ct = to_crossterm(&ev);
        assert_eq!(ct.code, CK::BackTab, "Shift-Tab must serialize to BackTab");
        assert!(ct.modifiers.contains(CM::SHIFT));
        // Round-trips: BackTab → Tab+SHIFT → BackTab.
        assert_eq!(from_crossterm(&ct).unwrap(), ev);
    }

    #[test]
    fn plain_tab_serializes_to_tab() {
        let ev = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
        let ct = to_crossterm(&ev);
        assert_eq!(ct.code, CK::Tab);
        assert!(!ct.modifiers.contains(CM::SHIFT));
    }

    #[test]
    fn shift_preserved_for_f_key() {
        let ev = from_crossterm(&ct_key(CK::F(5), CM::SHIFT)).unwrap();
        assert_eq!(ev, KeyEvent::new(KeyCode::F(5), KeyModifiers::SHIFT));
    }

    #[test]
    fn release_returns_none() {
        let mut k = ct_key(CK::Char('a'), CM::NONE);
        k.kind = KeyEventKind::Release;
        assert!(from_crossterm(&k).is_none());
    }
}
