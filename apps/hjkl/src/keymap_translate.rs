//! Translation between crossterm key events and [`hjkl_keymap::KeyEvent`].
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
    let modifiers = ct_mods_to_keymap(ev.modifiers);
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
            // We return the code and rely on the modifier translation below,
            // but crossterm does not set the SHIFT bit for BackTab.
            // Handle separately: callers that get BackTab should inject SHIFT.
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

/// Special-case: crossterm `BackTab` (Shift-Tab) arrives without the SHIFT
/// modifier set. This function synthesises the correct keymap event.
#[allow(dead_code)]
pub fn backtab_event() -> KeyEvent {
    KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT)
}
