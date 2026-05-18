//! Backend-agnostic key event and modifier types.

use bitflags::bitflags;

/// A single key press, backend-agnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl KeyEvent {
    pub fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self { code, modifiers }
    }

    /// Convenience: plain character with no modifiers.
    pub fn char(c: char) -> Self {
        Self::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    /// Convenience: character with CTRL modifier.
    pub fn ctrl(c: char) -> Self {
        Self::new(KeyCode::Char(c), KeyModifiers::CTRL)
    }
}

/// The logical key that was pressed, independent of modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCode {
    Char(char),
    Enter,
    Esc,
    Tab,
    Backspace,
    Delete,
    Insert,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    F(u8),
}

bitflags! {
    /// Modifier keys held during a key press.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct KeyModifiers: u8 {
        const NONE  = 0;
        const SHIFT = 1 << 0;
        const CTRL  = 1 << 1;
        const ALT   = 1 << 2;
    }
}
