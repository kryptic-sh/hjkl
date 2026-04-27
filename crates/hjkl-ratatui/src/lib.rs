//! # hjkl-ratatui
//!
//! Adapters between [`hjkl_engine`]'s SPEC types and the
//! [`ratatui`] / [`crossterm`] ecosystems.
//!
//! Engine types are deliberately UI-agnostic so non-terminal hosts
//! (buffr's wasm-flavored renderer, future GUI shells) can consume
//! them without dragging ratatui in. This crate is the opt-in bridge
//! ratatui-based hosts pull in.
//!
//! ## Conversions
//!
//! Free functions, not `From`/`Into` impls — orphan rules block
//! `impl From<engine::Style> for ratatui::Style` since both types are
//! foreign to this crate. Function syntax keeps the bridge explicit at
//! callsites, which is fine for low-frequency style mapping.
//!
//! - [`engine_to_ratatui_style`] / [`ratatui_to_engine_style`]
//! - [`engine_to_ratatui_color`] / [`ratatui_to_engine_color`]
//! - [`engine_to_ratatui_attrs`] / [`ratatui_to_engine_attrs`]
//! - [`crossterm_key_event_to_input`] (behind `crossterm` feature, on
//!   by default)
//!
//! Lossless within the styles each library can represent. Ratatui-only
//! colors that the engine doesn't model (indexed-256, named) flatten to
//! their nearest RGB approximation in the engine direction.
#![forbid(unsafe_code)]

pub mod form;
pub mod prompt;

use hjkl_engine::{Attrs, Color, Style};
use ratatui::style::{Color as RColor, Modifier as RMod, Style as RStyle};

// ── Style ──

pub fn engine_to_ratatui_style(s: Style) -> RStyle {
    let mut out = RStyle::default();
    if let Some(fg) = s.fg {
        out = out.fg(engine_to_ratatui_color(fg));
    }
    if let Some(bg) = s.bg {
        out = out.bg(engine_to_ratatui_color(bg));
    }
    out = out.add_modifier(engine_to_ratatui_attrs(s.attrs));
    out
}

pub fn ratatui_to_engine_style(s: RStyle) -> Style {
    Style {
        fg: s.fg.map(ratatui_to_engine_color),
        bg: s.bg.map(ratatui_to_engine_color),
        attrs: ratatui_to_engine_attrs(s.add_modifier),
    }
}

// ── Color ──

pub fn engine_to_ratatui_color(c: Color) -> RColor {
    RColor::Rgb(c.0, c.1, c.2)
}

pub fn ratatui_to_engine_color(c: RColor) -> Color {
    match c {
        RColor::Rgb(r, g, b) => Color(r, g, b),
        RColor::Indexed(i) => indexed_to_rgb(i),
        // Named ANSI colors flatten to a sensible RGB; precise mapping
        // is theme-dependent and lives in the host.
        RColor::Black => Color(0, 0, 0),
        RColor::Red => Color(205, 49, 49),
        RColor::Green => Color(13, 188, 121),
        RColor::Yellow => Color(229, 229, 16),
        RColor::Blue => Color(36, 114, 200),
        RColor::Magenta => Color(188, 63, 188),
        RColor::Cyan => Color(17, 168, 205),
        RColor::Gray => Color(229, 229, 229),
        RColor::DarkGray => Color(102, 102, 102),
        RColor::LightRed => Color(241, 76, 76),
        RColor::LightGreen => Color(35, 209, 139),
        RColor::LightYellow => Color(245, 245, 67),
        RColor::LightBlue => Color(59, 142, 234),
        RColor::LightMagenta => Color(214, 112, 214),
        RColor::LightCyan => Color(41, 184, 219),
        RColor::White => Color(255, 255, 255),
        RColor::Reset => Color(0, 0, 0),
    }
}

/// xterm-256 palette indexes 0–15 use the named colors; 16–231 cover
/// the 6×6×6 RGB cube; 232–255 are the grayscale ramp.
fn indexed_to_rgb(i: u8) -> Color {
    if i < 16 {
        let r: RColor = match i {
            0 => RColor::Black,
            1 => RColor::Red,
            2 => RColor::Green,
            3 => RColor::Yellow,
            4 => RColor::Blue,
            5 => RColor::Magenta,
            6 => RColor::Cyan,
            7 => RColor::Gray,
            8 => RColor::DarkGray,
            9 => RColor::LightRed,
            10 => RColor::LightGreen,
            11 => RColor::LightYellow,
            12 => RColor::LightBlue,
            13 => RColor::LightMagenta,
            14 => RColor::LightCyan,
            _ => RColor::White,
        };
        return ratatui_to_engine_color(r);
    }
    if (16..=231).contains(&i) {
        let idx = i - 16;
        let r = idx / 36;
        let g = (idx / 6) % 6;
        let b = idx % 6;
        let comp = |v| if v == 0 { 0u8 } else { 55 + v * 40 };
        return Color(comp(r), comp(g), comp(b));
    }
    let v = 8 + (i - 232) * 10;
    Color(v, v, v)
}

// ── Attrs ──

pub fn engine_to_ratatui_attrs(a: Attrs) -> RMod {
    let mut m = RMod::empty();
    if a.contains(Attrs::BOLD) {
        m |= RMod::BOLD;
    }
    if a.contains(Attrs::ITALIC) {
        m |= RMod::ITALIC;
    }
    if a.contains(Attrs::UNDERLINE) {
        m |= RMod::UNDERLINED;
    }
    if a.contains(Attrs::REVERSE) {
        m |= RMod::REVERSED;
    }
    if a.contains(Attrs::DIM) {
        m |= RMod::DIM;
    }
    if a.contains(Attrs::STRIKE) {
        m |= RMod::CROSSED_OUT;
    }
    m
}

pub fn ratatui_to_engine_attrs(m: RMod) -> Attrs {
    let mut a = Attrs::empty();
    if m.contains(RMod::BOLD) {
        a |= Attrs::BOLD;
    }
    if m.contains(RMod::ITALIC) {
        a |= Attrs::ITALIC;
    }
    if m.contains(RMod::UNDERLINED) {
        a |= Attrs::UNDERLINE;
    }
    if m.contains(RMod::REVERSED) {
        a |= Attrs::REVERSE;
    }
    if m.contains(RMod::DIM) {
        a |= Attrs::DIM;
    }
    if m.contains(RMod::CROSSED_OUT) {
        a |= Attrs::STRIKE;
    }
    a
}

// ── Crossterm KeyEvent → engine SPEC Input ──

#[cfg(feature = "crossterm")]
pub use crossterm_bridge::crossterm_key_event_to_input;

#[cfg(feature = "crossterm")]
mod crossterm_bridge {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use hjkl_engine::{Modifiers, PlannedInput, SpecialKey};

    /// Bridge a [`crossterm::event::KeyEvent`] into the SPEC
    /// [`hjkl_engine::PlannedInput`]. Lossy for keycodes the engine
    /// doesn't model (KeyPad, Media, Caps, etc.) — falls back to Esc.
    pub fn crossterm_key_event_to_input(ev: KeyEvent) -> PlannedInput {
        let mods = Modifiers {
            ctrl: ev.modifiers.contains(KeyModifiers::CONTROL),
            shift: ev.modifiers.contains(KeyModifiers::SHIFT),
            alt: ev.modifiers.contains(KeyModifiers::ALT),
            super_: ev.modifiers.contains(KeyModifiers::SUPER),
        };
        match ev.code {
            KeyCode::Char(c) => PlannedInput::Char(c, mods),
            KeyCode::Esc => PlannedInput::Key(SpecialKey::Esc, mods),
            KeyCode::Enter => PlannedInput::Key(SpecialKey::Enter, mods),
            KeyCode::Backspace => PlannedInput::Key(SpecialKey::Backspace, mods),
            KeyCode::Tab => PlannedInput::Key(SpecialKey::Tab, mods),
            KeyCode::BackTab => PlannedInput::Key(SpecialKey::BackTab, mods),
            KeyCode::Up => PlannedInput::Key(SpecialKey::Up, mods),
            KeyCode::Down => PlannedInput::Key(SpecialKey::Down, mods),
            KeyCode::Left => PlannedInput::Key(SpecialKey::Left, mods),
            KeyCode::Right => PlannedInput::Key(SpecialKey::Right, mods),
            KeyCode::Home => PlannedInput::Key(SpecialKey::Home, mods),
            KeyCode::End => PlannedInput::Key(SpecialKey::End, mods),
            KeyCode::PageUp => PlannedInput::Key(SpecialKey::PageUp, mods),
            KeyCode::PageDown => PlannedInput::Key(SpecialKey::PageDown, mods),
            KeyCode::Insert => PlannedInput::Key(SpecialKey::Insert, mods),
            KeyCode::Delete => PlannedInput::Key(SpecialKey::Delete, mods),
            KeyCode::F(n) => PlannedInput::Key(SpecialKey::F(n), mods),
            _ => PlannedInput::Key(SpecialKey::Esc, mods),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hjkl_engine::Color as EColor;

    #[test]
    fn style_roundtrip() {
        let s = Style {
            fg: Some(EColor(255, 100, 50)),
            bg: Some(EColor(20, 20, 20)),
            attrs: Attrs::BOLD | Attrs::UNDERLINE,
        };
        let r = engine_to_ratatui_style(s);
        let back = ratatui_to_engine_style(r);
        assert_eq!(s, back);
    }

    #[test]
    fn color_rgb_roundtrip() {
        let c = EColor(123, 45, 67);
        let r = engine_to_ratatui_color(c);
        let back = ratatui_to_engine_color(r);
        assert_eq!(c, back);
    }

    #[test]
    fn indexed_into_engine_color() {
        let c = ratatui_to_engine_color(RColor::Indexed(1));
        assert_eq!(c, EColor(205, 49, 49));
    }

    #[test]
    fn attrs_roundtrip() {
        let a = Attrs::BOLD | Attrs::ITALIC | Attrs::STRIKE;
        let m = engine_to_ratatui_attrs(a);
        let back = ratatui_to_engine_attrs(m);
        assert_eq!(a, back);
    }

    #[cfg(feature = "crossterm")]
    #[test]
    fn crossterm_keyevent_into_input() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use hjkl_engine::{PlannedInput, SpecialKey};

        let ev = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL);
        match crossterm_key_event_to_input(ev) {
            PlannedInput::Char('a', mods) => assert!(mods.ctrl),
            other => panic!("expected Char('a', ctrl), got {other:?}"),
        }

        let ev = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        match crossterm_key_event_to_input(ev) {
            PlannedInput::Key(SpecialKey::Esc, _) => {}
            other => panic!("expected Esc, got {other:?}"),
        }
    }
}
