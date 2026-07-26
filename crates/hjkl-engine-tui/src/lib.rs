//! Ratatui adapter surface for [`hjkl_engine`].
//!
//! Provides:
//! - [`style_to_ratatui`] — convert an engine-native [`hjkl_engine::types::Style`]
//!   to a [`ratatui::style::Style`].
//! - [`style_from_ratatui`] — the inverse conversion (lossy for ratatui colors
//!   the engine doesn't model — flattens to nearest RGB).
//! - [`color_to_ratatui`] / [`color_from_ratatui`] and [`attrs_to_ratatui`] /
//!   [`attrs_from_ratatui`] — the per-field pieces the style conversions are
//!   built from. This is the workspace's single engine↔ratatui color table;
//!   `hjkl-editor-tui`'s `engine_to_ratatui_*` / `ratatui_to_engine_*` names
//!   are thin delegations to these.
//! - [`EditorRatatuiExt`] — extension trait on [`hjkl_engine::Editor`] that
//!   exposes `install_ratatui_syntax_spans` and
//!   `patch_ratatui_syntax_spans_range`. Extracted from `hjkl-engine`'s
//!   `ratatui` feature gate as part of #162 phase 2.
//! - [`KeyEvent`] — re-export of [`crossterm::event::KeyEvent`] for
//!   downstream convenience.
//! - [`crossterm_to_input`] — convert a crossterm `KeyEvent` to the
//!   engine-agnostic [`hjkl_engine::Input`] type. Moved from `hjkl-engine`'s
//!   `crossterm` feature gate as part of #162 phase 3.

use crossterm::event::{KeyCode, KeyModifiers};
use hjkl_engine::{
    Editor, Input, Key,
    types::{Attrs, Color, Host, Style},
};
use ratatui::style::{Color as RColor, Modifier as RMod, Style as RStyle};

/// Re-export of [`crossterm::event::KeyEvent`] for downstream convenience.
pub use crossterm::event::KeyEvent;

/// Convert a crossterm [`KeyEvent`] to the engine-agnostic [`hjkl_engine::Input`].
///
/// Keys the engine FSM does not model (`KeyCode::F(_)`, `KeyCode::Insert`, and
/// any other unrecognised variant) map to [`hjkl_engine::Key::Null`]; callers
/// should early-return or discard such inputs. Moved from `hjkl-engine`'s
/// `crossterm` feature gate as part of #162 phase 3.
pub fn crossterm_to_input(key: KeyEvent) -> Input {
    let k = match key.code {
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Delete => Key::Delete,
        KeyCode::Enter => Key::Enter,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::Tab => Key::Tab,
        KeyCode::Esc => Key::Esc,
        _ => Key::Null,
    };
    Input {
        key: k,
        ctrl: key.modifiers.contains(KeyModifiers::CONTROL),
        alt: key.modifiers.contains(KeyModifiers::ALT),
        shift: key.modifiers.contains(KeyModifiers::SHIFT),
    }
}

/// Convert an engine-native [`Style`] to a [`ratatui::style::Style`].
///
/// Lossless within the styles each library represents.
pub fn style_to_ratatui(s: Style) -> RStyle {
    let mut out = RStyle::default();
    if let Some(c) = s.fg {
        out = out.fg(color_to_ratatui(c));
    }
    if let Some(c) = s.bg {
        out = out.bg(color_to_ratatui(c));
    }
    out.add_modifier(attrs_to_ratatui(s.attrs))
}

/// Convert a [`ratatui::style::Style`] to an engine-native [`Style`].
///
/// Lossy for ratatui colors the engine doesn't model (Indexed, named ANSI) —
/// flattens to nearest RGB via [`color_from_ratatui`].
pub fn style_from_ratatui(s: RStyle) -> Style {
    Style {
        fg: s.fg.map(color_from_ratatui),
        bg: s.bg.map(color_from_ratatui),
        attrs: attrs_from_ratatui(s.add_modifier),
    }
}

/// Convert an engine-native [`Color`] to a [`ratatui::style::Color`].
///
/// The engine models colors as truecolor RGB only, so this is always
/// [`RColor::Rgb`].
pub fn color_to_ratatui(c: Color) -> RColor {
    RColor::Rgb(c.0, c.1, c.2)
}

/// Convert a [`ratatui::style::Color`] to an engine-native [`Color`].
///
/// This is the single ratatui→engine color table for the whole stack:
/// named ANSI colors flatten to a fixed RGB approximation, indexed-256
/// colors resolve through the real xterm palette ([`indexed_to_rgb`]),
/// and `Reset` degrades to black (the engine has no "unset" color).
pub fn color_from_ratatui(c: RColor) -> Color {
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

/// Resolve an xterm-256 palette index to RGB: indexes 0–15 use the named
/// colors, 16–231 cover the 6×6×6 RGB cube, 232–255 the grayscale ramp.
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
        return color_from_ratatui(r);
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

/// Convert engine-native [`Attrs`] to a ratatui [`Modifier`](RMod) set.
pub fn attrs_to_ratatui(a: Attrs) -> RMod {
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

/// Convert a ratatui [`Modifier`](RMod) set to engine-native [`Attrs`].
///
/// Modifiers the engine doesn't model (`SLOW_BLINK`, `RAPID_BLINK`,
/// `HIDDEN`) are dropped.
pub fn attrs_from_ratatui(m: RMod) -> Attrs {
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

/// Extension trait that adds ratatui-flavoured style methods to
/// [`hjkl_engine::Editor`].
///
/// Bring into scope with `use hjkl_engine_tui::EditorRatatuiExt;` then call
/// the methods directly on any `Editor<B, H>` value.
pub trait EditorRatatuiExt {
    /// Install styled syntax spans given as ratatui styles. Converts each
    /// `ratatui::style::Style` to engine-native via [`style_from_ratatui`]
    /// then delegates to `Editor::install_syntax_spans`. Drops zero-width
    /// runs and clamps `end` to the line's char length.
    ///
    /// Borrows the table so a host can install the same spans into several
    /// editors (one per window showing the buffer) without cloning it per
    /// editor; the conversion streams lazily into the engine.
    fn install_ratatui_syntax_spans(&mut self, spans: &[Vec<(usize, usize, RStyle)>]);

    /// Patch only `rows` of the installed spans (ratatui-typed input).
    /// Mirrors [`hjkl_engine::Editor::patch_syntax_spans_range`] for
    /// callers that have ratatui styles.
    fn patch_ratatui_syntax_spans_range(
        &mut self,
        rows: std::ops::Range<usize>,
        spans: &[Vec<(usize, usize, RStyle)>],
    );
}

impl<H: Host> EditorRatatuiExt for Editor<hjkl_buffer::View, H> {
    fn install_ratatui_syntax_spans(&mut self, spans: &[Vec<(usize, usize, RStyle)>]) {
        // Lazy per-row conversion: the engine consumes the iterator directly,
        // so no intermediate engine-typed table is allocated per install.
        self.install_syntax_spans(spans.iter().map(|row_spans| {
            row_spans
                .iter()
                .map(|(start, end, style)| (*start, *end, style_from_ratatui(*style)))
        }));
    }

    fn patch_ratatui_syntax_spans_range(
        &mut self,
        rows: std::ops::Range<usize>,
        spans: &[Vec<(usize, usize, RStyle)>],
    ) {
        self.patch_syntax_spans_range(
            rows,
            spans.iter().map(|row_spans| {
                row_spans
                    .iter()
                    .map(|(start, end, style)| (*start, *end, style_from_ratatui(*style)))
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hjkl_engine::{Editor, types::DefaultHost};

    fn fresh_editor(content: &str) -> Editor {
        let mut e = hjkl_vim::vim_editor(
            hjkl_buffer::View::new(),
            DefaultHost::new(),
            hjkl_engine::types::Options::default(),
        );
        e.set_content(content);
        e
    }

    #[test]
    fn install_ratatui_syntax_spans_translates_styled_spans() {
        use hjkl_engine::types::Color as EColor;
        use ratatui::style::{Color, Style};
        let mut e = fresh_editor("SELECT foo");
        e.install_ratatui_syntax_spans(&[vec![(0, 6, Style::default().fg(Color::Red))]]);
        let by_row = e.buffer_spans();
        assert_eq!(by_row.len(), 1);
        assert_eq!(by_row[0].len(), 1);
        assert_eq!(by_row[0][0].start_byte, 0);
        assert_eq!(by_row[0][0].end_byte, 6);
        let id = by_row[0][0].style;
        // Named colors are flattened to RGB at the ratatui→engine boundary
        // (see `style_from_ratatui`). Color::Red maps to (205, 49, 49).
        assert_eq!(e.style_table()[id as usize].fg, Some(EColor(205, 49, 49)));
    }

    #[test]
    fn install_ratatui_syntax_spans_clamps_sentinel_end() {
        use ratatui::style::{Color, Style};
        let mut e = fresh_editor("hello");
        e.install_ratatui_syntax_spans(&[vec![(0, usize::MAX, Style::default().fg(Color::Blue))]]);
        let by_row = e.buffer_spans();
        assert_eq!(by_row[0][0].end_byte, 5);
    }

    #[test]
    fn install_ratatui_syntax_spans_drops_zero_width() {
        use ratatui::style::{Color, Style};
        let mut e = fresh_editor("abc");
        e.install_ratatui_syntax_spans(&[vec![(2, 2, Style::default().fg(Color::Red))]]);
        assert!(e.buffer_spans()[0].is_empty());
    }
}
