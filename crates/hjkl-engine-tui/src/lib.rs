//! Ratatui adapter surface for [`hjkl_engine`].
//!
//! Provides:
//! - [`style_to_ratatui`] — convert an engine-native [`hjkl_engine::types::Style`]
//!   to a [`ratatui::style::Style`].
//! - [`style_from_ratatui`] — the inverse conversion (lossy for ratatui colors
//!   the engine doesn't model — flattens to nearest RGB).
//! - [`EditorRatatuiExt`] — extension trait on [`hjkl_engine::Editor`] that
//!   exposes `intern_ratatui_style`, `install_ratatui_syntax_spans`, and
//!   `ratatui_style_table`. Extracted from `hjkl-engine`'s `ratatui` feature
//!   gate as part of #162 phase 2.
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
        out = out.fg(RColor::Rgb(c.0, c.1, c.2));
    }
    if let Some(c) = s.bg {
        out = out.bg(RColor::Rgb(c.0, c.1, c.2));
    }
    let mut m = RMod::empty();
    if s.attrs.contains(Attrs::BOLD) {
        m |= RMod::BOLD;
    }
    if s.attrs.contains(Attrs::ITALIC) {
        m |= RMod::ITALIC;
    }
    if s.attrs.contains(Attrs::UNDERLINE) {
        m |= RMod::UNDERLINED;
    }
    if s.attrs.contains(Attrs::REVERSE) {
        m |= RMod::REVERSED;
    }
    if s.attrs.contains(Attrs::DIM) {
        m |= RMod::DIM;
    }
    if s.attrs.contains(Attrs::STRIKE) {
        m |= RMod::CROSSED_OUT;
    }
    out.add_modifier(m)
}

/// Convert a [`ratatui::style::Style`] to an engine-native [`Style`].
///
/// Lossy for ratatui colors the engine doesn't model (Indexed, named ANSI) —
/// flattens to nearest RGB.
pub fn style_from_ratatui(s: RStyle) -> Style {
    fn c(rc: RColor) -> Color {
        match rc {
            RColor::Rgb(r, g, b) => Color(r, g, b),
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
            _ => Color(0, 0, 0),
        }
    }
    let mut attrs = Attrs::empty();
    if s.add_modifier.contains(RMod::BOLD) {
        attrs |= Attrs::BOLD;
    }
    if s.add_modifier.contains(RMod::ITALIC) {
        attrs |= Attrs::ITALIC;
    }
    if s.add_modifier.contains(RMod::UNDERLINED) {
        attrs |= Attrs::UNDERLINE;
    }
    if s.add_modifier.contains(RMod::REVERSED) {
        attrs |= Attrs::REVERSE;
    }
    if s.add_modifier.contains(RMod::DIM) {
        attrs |= Attrs::DIM;
    }
    if s.add_modifier.contains(RMod::CROSSED_OUT) {
        attrs |= Attrs::STRIKE;
    }
    Style {
        fg: s.fg.map(c),
        bg: s.bg.map(c),
        attrs,
    }
}

/// Extension trait that adds ratatui-flavoured style methods to
/// [`hjkl_engine::Editor`].
///
/// Bring into scope with `use hjkl_engine_tui::EditorRatatuiExt;` then call
/// the methods directly on any `Editor<B, H>` value.
pub trait EditorRatatuiExt {
    /// Intern a [`ratatui::style::Style`] and return the opaque id used in
    /// `hjkl_buffer::Span::style`. Converts via [`style_from_ratatui`] then
    /// delegates to `Editor::intern_style`.
    fn intern_ratatui_style(&mut self, style: RStyle) -> u32;

    /// Install styled syntax spans given as ratatui styles. Converts each
    /// `ratatui::style::Style` to engine-native via [`style_from_ratatui`]
    /// then delegates to `Editor::install_syntax_spans`. Drops zero-width
    /// runs and clamps `end` to the line's char length.
    fn install_ratatui_syntax_spans(&mut self, spans: Vec<Vec<(usize, usize, RStyle)>>);

    /// Patch only `rows` of the installed spans (ratatui-typed input).
    /// Mirrors [`hjkl_engine::Editor::patch_syntax_spans_range`] for
    /// callers that have ratatui styles.
    fn patch_ratatui_syntax_spans_range(
        &mut self,
        rows: std::ops::Range<usize>,
        spans: &[Vec<(usize, usize, RStyle)>],
    );

    /// Allocate and return the style table converted to ratatui styles.
    /// Convenience for render paths that need a `Vec<ratatui::style::Style>`.
    /// Allocates on every call — prefer a per-draw local binding.
    fn ratatui_style_table(&self) -> Vec<RStyle>;
}

impl<H: Host> EditorRatatuiExt for Editor<hjkl_buffer::Buffer, H> {
    fn intern_ratatui_style(&mut self, style: RStyle) -> u32 {
        self.intern_style(style_from_ratatui(style))
    }

    fn install_ratatui_syntax_spans(&mut self, spans: Vec<Vec<(usize, usize, RStyle)>>) {
        let engine_spans: Vec<Vec<(usize, usize, Style)>> = spans
            .into_iter()
            .map(|row_spans| {
                row_spans
                    .into_iter()
                    .map(|(start, end, style)| (start, end, style_from_ratatui(style)))
                    .collect()
            })
            .collect();
        self.install_syntax_spans(engine_spans);
    }

    fn patch_ratatui_syntax_spans_range(
        &mut self,
        rows: std::ops::Range<usize>,
        spans: &[Vec<(usize, usize, RStyle)>],
    ) {
        let engine_spans: Vec<Vec<(usize, usize, Style)>> = spans
            .iter()
            .map(|row_spans| {
                row_spans
                    .iter()
                    .map(|(start, end, style)| (*start, *end, style_from_ratatui(*style)))
                    .collect()
            })
            .collect();
        self.patch_syntax_spans_range(rows, &engine_spans);
    }

    fn ratatui_style_table(&self) -> Vec<RStyle> {
        self.style_table()
            .iter()
            .copied()
            .map(style_to_ratatui)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hjkl_engine::{Editor, types::DefaultHost};

    fn fresh_editor(content: &str) -> Editor {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            DefaultHost::new(),
            hjkl_engine::types::Options::default(),
        );
        e.set_content(content);
        e
    }

    #[test]
    fn intern_ratatui_style_dedups_repeated_styles() {
        use ratatui::style::{Color, Style};
        let mut e = fresh_editor("");
        let red = Style::default().fg(Color::Red);
        let blue = Style::default().fg(Color::Blue);
        let id_r1 = e.intern_ratatui_style(red);
        let id_r2 = e.intern_ratatui_style(red);
        let id_b = e.intern_ratatui_style(blue);
        assert_eq!(id_r1, id_r2);
        assert_ne!(id_r1, id_b);
        assert_eq!(e.style_table().len(), 2);
    }

    #[test]
    fn install_ratatui_syntax_spans_translates_styled_spans() {
        use hjkl_engine::types::Color as EColor;
        use ratatui::style::{Color, Style};
        let mut e = fresh_editor("SELECT foo");
        e.install_ratatui_syntax_spans(vec![vec![(0, 6, Style::default().fg(Color::Red))]]);
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
        e.install_ratatui_syntax_spans(vec![vec![(
            0,
            usize::MAX,
            Style::default().fg(Color::Blue),
        )]]);
        let by_row = e.buffer_spans();
        assert_eq!(by_row[0][0].end_byte, 5);
    }

    #[test]
    fn install_ratatui_syntax_spans_drops_zero_width() {
        use ratatui::style::{Color, Style};
        let mut e = fresh_editor("abc");
        e.install_ratatui_syntax_spans(vec![vec![(2, 2, Style::default().fg(Color::Red))]]);
        assert!(e.buffer_spans()[0].is_empty());
    }
}
