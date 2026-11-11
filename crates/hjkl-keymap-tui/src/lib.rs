//! ttyr `KeyEvent` ↔ [`hjkl_keymap::KeyEvent`] adapter.
//!
//! Wraps the terminal input boundary so TUI renderer code is the only place
//! that imports `ttyr`. Future renderer adapters (e.g. `hjkl-keymap-floem`)
//! follow the same pattern for their respective input layer.
//!
//! Unsupported ttyr key codes (e.g. media keys, modifier-only events)
//! are mapped to `None` so callers can skip them.

use hjkl_keymap::{KeyCode, KeyEvent, KeyModifiers};
use ttyr::event::{KeyCode as TtKeyCode, KeyEvent as TtKeyEvent, KeyModifiers as TtKeyMods};

/// Convert a ttyr `KeyEvent` to a `hjkl_keymap::KeyEvent`.
///
/// Returns `None` for key releases, or for key codes that have no meaningful
/// representation in the keymap (e.g. `Null`, `CapsLock`). A repeat counts
/// as a press.
pub fn from_ttyr(ev: &TtKeyEvent) -> Option<KeyEvent> {
    use ttyr::event::KeyEventKind;
    if ev.kind == KeyEventKind::Release {
        return None;
    }

    // A character key is the character it typed: Shift+a is `A` whether the
    // terminal sent `A` (legacy), `a` + Shift (kitty flag 1) or both (flag 4).
    let code = match ev.key {
        TtKeyCode::Char(_) => KeyCode::Char(ev.logical_char()?),
        other => tt_code_to_keymap(other)?,
    };
    let mut modifiers = tt_mods_to_keymap(ev.logical_modifiers());
    // BackTab is Shift-Tab by definition. ttyr pairs BackTab with SHIFT, but
    // inject it explicitly so the mapping can't silently collapse to plain
    // Tab if an event omits it.
    if ev.key == TtKeyCode::BackTab {
        modifiers |= KeyModifiers::SHIFT;
    }
    // SHIFT for plain Char events is redundant — the case is already in the
    // char (vim convention). Where Shift is left over (Alt+Shift+1 from a
    // kitty terminal reporting only `1`, or a hand-built `'B'` + SHIFT), drop
    // it so bindings registered as `ch('B')` match either delivery.
    // SHIFT remains distinguishing for non-Char codes (Tab → Shift-Tab, etc.)
    if matches!(code, KeyCode::Char(_)) {
        modifiers.remove(KeyModifiers::SHIFT);
    }
    Some(KeyEvent::new(code, modifiers))
}

fn tt_code_to_keymap(code: TtKeyCode) -> Option<KeyCode> {
    Some(match code {
        TtKeyCode::Char(c) => KeyCode::Char(c),
        TtKeyCode::Enter => KeyCode::Enter,
        TtKeyCode::Esc => KeyCode::Esc,
        TtKeyCode::Tab => KeyCode::Tab,
        TtKeyCode::BackTab => {
            // BackTab is <S-Tab> — represented as Tab + SHIFT modifier.
            // `from_ttyr` injects SHIFT for BackTab explicitly.
            KeyCode::Tab
        }
        TtKeyCode::Backspace => KeyCode::Backspace,
        TtKeyCode::Delete => KeyCode::Delete,
        TtKeyCode::Insert => KeyCode::Insert,
        TtKeyCode::Up => KeyCode::Up,
        TtKeyCode::Down => KeyCode::Down,
        TtKeyCode::Left => KeyCode::Left,
        TtKeyCode::Right => KeyCode::Right,
        TtKeyCode::Home => KeyCode::Home,
        TtKeyCode::End => KeyCode::End,
        TtKeyCode::PageUp => KeyCode::PageUp,
        TtKeyCode::PageDown => KeyCode::PageDown,
        TtKeyCode::F(n) => KeyCode::F(n),
        // Unsupported / no-op codes.
        TtKeyCode::Null
        | TtKeyCode::CapsLock
        | TtKeyCode::ScrollLock
        | TtKeyCode::NumLock
        | TtKeyCode::PrintScreen
        | TtKeyCode::Pause
        | TtKeyCode::Menu
        | TtKeyCode::KeypadBegin
        | TtKeyCode::Media(_)
        | TtKeyCode::Modifier(_) => return None,
    })
}

fn tt_mods_to_keymap(mods: TtKeyMods) -> KeyModifiers {
    let mut out = KeyModifiers::NONE;
    if mods.contains(TtKeyMods::SHIFT) {
        out |= KeyModifiers::SHIFT;
    }
    if mods.contains(TtKeyMods::CONTROL) {
        out |= KeyModifiers::CTRL;
    }
    if mods.contains(TtKeyMods::ALT) {
        out |= KeyModifiers::ALT;
    }
    out
}

/// Convert a `hjkl_keymap::KeyEvent` back to a `ttyr::event::KeyEvent`
/// for replaying unbound sequences or user maps to the engine.
pub fn to_ttyr(ev: &KeyEvent) -> TtKeyEvent {
    // `<S-Tab>` is represented internally as `Tab` + SHIFT (see
    // `from_ttyr`), but the terminal's form is `BackTab`. Emit that so a
    // replayed unbound Shift-Tab matches what the terminal actually delivers
    // (and round-trips through `from_ttyr`) instead of the never-emitted
    // `Tab` + SHIFT.
    if ev.code == KeyCode::Tab && ev.modifiers.contains(KeyModifiers::SHIFT) {
        let mut mods = TtKeyMods::NONE;
        if ev.modifiers.contains(KeyModifiers::CTRL) {
            mods |= TtKeyMods::CONTROL;
        }
        mods |= TtKeyMods::SHIFT;
        if ev.modifiers.contains(KeyModifiers::ALT) {
            mods |= TtKeyMods::ALT;
        }
        return TtKeyEvent::new(TtKeyCode::BackTab, mods);
    }
    let code = match ev.code {
        KeyCode::Char(c) => TtKeyCode::Char(c),
        KeyCode::Enter => TtKeyCode::Enter,
        KeyCode::Esc => TtKeyCode::Esc,
        KeyCode::Tab => TtKeyCode::Tab,
        KeyCode::Backspace => TtKeyCode::Backspace,
        KeyCode::Delete => TtKeyCode::Delete,
        KeyCode::Insert => TtKeyCode::Insert,
        KeyCode::Up => TtKeyCode::Up,
        KeyCode::Down => TtKeyCode::Down,
        KeyCode::Left => TtKeyCode::Left,
        KeyCode::Right => TtKeyCode::Right,
        KeyCode::Home => TtKeyCode::Home,
        KeyCode::End => TtKeyCode::End,
        KeyCode::PageUp => TtKeyCode::PageUp,
        KeyCode::PageDown => TtKeyCode::PageDown,
        KeyCode::F(n) => TtKeyCode::F(n),
    };
    let mut mods = TtKeyMods::NONE;
    if ev.modifiers.contains(KeyModifiers::CTRL) {
        mods |= TtKeyMods::CONTROL;
    }
    if ev.modifiers.contains(KeyModifiers::SHIFT) {
        mods |= TtKeyMods::SHIFT;
    }
    if ev.modifiers.contains(KeyModifiers::ALT) {
        mods |= TtKeyMods::ALT;
    }
    TtKeyEvent::new(code, mods)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ttyr::event::{
        KeyCode as CK, KeyEncoding, KeyEvent as CKE, KeyEventKind, KeyModifiers as CM,
    };

    fn ct_key(code: CK, mods: CM) -> CKE {
        CKE::new(code, mods)
    }

    #[test]
    fn shift_stripped_for_uppercase_char() {
        // Kitty-style: 'B' + SHIFT.
        let ev = from_ttyr(&ct_key(CK::Char('B'), CM::SHIFT)).unwrap();
        assert_eq!(ev, KeyEvent::new(KeyCode::Char('B'), KeyModifiers::NONE));
    }

    #[test]
    fn shift_stripped_for_shifted_symbol() {
        // '<' (shift-comma on US layout) sometimes arrives with SHIFT.
        let ev = from_ttyr(&ct_key(CK::Char('<'), CM::SHIFT)).unwrap();
        assert_eq!(ev, KeyEvent::new(KeyCode::Char('<'), KeyModifiers::NONE));
    }

    #[test]
    fn ctrl_preserved_with_char() {
        let ev = from_ttyr(&ct_key(CK::Char('w'), CM::CONTROL)).unwrap();
        assert_eq!(ev, KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CTRL));
    }

    #[test]
    fn ctrl_shift_with_char_keeps_only_ctrl() {
        // Edge case: Ctrl-Shift-A on kitty arrives as Char('A') + CTRL|SHIFT.
        // We strip SHIFT (case encodes it) but keep CTRL.
        let ev = from_ttyr(&ct_key(CK::Char('A'), CM::CONTROL | CM::SHIFT)).unwrap();
        assert_eq!(ev, KeyEvent::new(KeyCode::Char('A'), KeyModifiers::CTRL));
    }

    #[test]
    fn shift_preserved_for_tab() {
        let ev = from_ttyr(&ct_key(CK::Tab, CM::SHIFT)).unwrap();
        assert_eq!(ev, KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT));
    }

    #[test]
    fn backtab_maps_to_shift_tab_even_without_shift_bit() {
        // ttyr pairs BackTab with SHIFT, but the mapping must not collapse
        // to plain Tab if an event omits the modifier.
        let ev = from_ttyr(&ct_key(CK::BackTab, CM::NONE)).unwrap();
        assert_eq!(ev, KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT));

        let ev = from_ttyr(&ct_key(CK::BackTab, CM::SHIFT)).unwrap();
        assert_eq!(ev, KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT));
    }

    #[test]
    fn shift_tab_serializes_back_to_backtab() {
        // Internally <S-Tab> is Tab+SHIFT; converting back to ttyr must
        // yield the canonical BackTab (what the terminal emits), not Tab+SHIFT.
        let ev = KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT);
        let ct = to_ttyr(&ev);
        assert_eq!(ct.key, CK::BackTab, "Shift-Tab must serialize to BackTab");
        assert!(ct.modifiers.contains(CM::SHIFT));
        // Round-trips: BackTab → Tab+SHIFT → BackTab.
        assert_eq!(from_ttyr(&ct).unwrap(), ev);
    }

    #[test]
    fn plain_tab_serializes_to_tab() {
        let ev = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
        let ct = to_ttyr(&ev);
        assert_eq!(ct.key, CK::Tab);
        assert!(!ct.modifiers.contains(CM::SHIFT));
    }

    #[test]
    fn shift_preserved_for_f_key() {
        let ev = from_ttyr(&ct_key(CK::F(5), CM::SHIFT)).unwrap();
        assert_eq!(ev, KeyEvent::new(KeyCode::F(5), KeyModifiers::SHIFT));
    }

    #[test]
    fn release_returns_none() {
        let k = ct_key(CK::Char('a'), CM::NONE).with_kind(KeyEventKind::Release);
        assert!(from_ttyr(&k).is_none());
    }

    #[test]
    fn repeat_counts_as_a_press() {
        let k = ct_key(CK::Char('j'), CM::NONE).with_kind(KeyEventKind::Repeat);
        assert_eq!(
            from_ttyr(&k),
            Some(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE))
        );
    }

    #[test]
    fn legacy_shifted_letter_is_its_capital() {
        // A legacy terminal sends `A`, which ttyr reads as `a` + Shift with
        // the shifted key.
        let k = ct_key(CK::Char('a'), CM::SHIFT)
            .with_shifted(Some('A'))
            .with_encoding(KeyEncoding::Legacy);
        assert_eq!(
            from_ttyr(&k),
            Some(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::NONE))
        );
    }

    #[test]
    fn kitty_alt_shift_letter_is_alt_capital() {
        // Kitty flag 1 sends Alt+Shift+a as `CSI 97;4u`: only the unshifted
        // key, Shift in the modifiers. It is `<M-A>`, not `<M-a>`.
        let k = ct_key(CK::Char('a'), CM::ALT | CM::SHIFT);
        assert_eq!(
            from_ttyr(&k),
            Some(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::ALT))
        );
    }

    #[test]
    fn kitty_ctrl_shift_letter_is_ctrl_capital() {
        // `CSI 104;6u`: Ctrl+Shift+h.
        let k = ct_key(CK::Char('h'), CM::CONTROL | CM::SHIFT);
        assert_eq!(
            from_ttyr(&k),
            Some(KeyEvent::new(KeyCode::Char('H'), KeyModifiers::CTRL))
        );
    }
}
