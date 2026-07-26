//! # hjkl-editor-tui
//!
//! Adapters between [`hjkl_engine`]'s SPEC types and the
//! [`ratatui`] ecosystem.
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
//!
//! These are one-line aliases for the canonical implementations in
//! [`hjkl_engine_tui`], which owns the single engine↔ratatui color and
//! attribute tables for the workspace. The names are kept so existing
//! callers don't have to switch crates.
//!
//! Key-event bridging lives in `hjkl-engine-tui` (`crossterm_to_input`);
//! this crate's own copy was deleted as dead code.
//!
//! Lossless within the styles each library can represent. Ratatui-only
//! colors that the engine doesn't model (indexed-256, named) flatten to
//! their nearest RGB approximation in the engine direction — indexed
//! colors resolve through the real xterm-256 palette.
#![forbid(unsafe_code)]

pub mod form;
pub mod prompt;
pub mod spinner;

use hjkl_engine::{Attrs, Color, Style};
use ratatui::style::{Color as RColor, Modifier as RMod, Style as RStyle};
use unicode_width::UnicodeWidthChar;

/// Display width, in terminal cells, of the characters on `line` in the
/// char-index range `[from_col, to_col)`. CJK/emoji count as 2 cells,
/// combining marks as 0 — so cursor and scroll math match what the
/// terminal actually paints for multibyte text.
pub(crate) fn col_span_width(line: &str, from_col: usize, to_col: usize) -> usize {
    line.chars()
        .skip(from_col)
        .take(to_col.saturating_sub(from_col))
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
        .sum()
}

// ── Style ──
//
// All six conversions delegate to `hjkl_engine_tui`, which holds the
// single implementation (and the only copy of the ANSI/xterm-256 color
// table) for the workspace.

pub fn engine_to_ratatui_style(s: Style) -> RStyle {
    hjkl_engine_tui::style_to_ratatui(s)
}

pub fn ratatui_to_engine_style(s: RStyle) -> Style {
    hjkl_engine_tui::style_from_ratatui(s)
}

// ── Color ──

pub fn engine_to_ratatui_color(c: Color) -> RColor {
    hjkl_engine_tui::color_to_ratatui(c)
}

pub fn ratatui_to_engine_color(c: RColor) -> Color {
    hjkl_engine_tui::color_from_ratatui(c)
}

// ── Attrs ──

pub fn engine_to_ratatui_attrs(a: Attrs) -> RMod {
    hjkl_engine_tui::attrs_to_ratatui(a)
}

pub fn ratatui_to_engine_attrs(m: RMod) -> Attrs {
    hjkl_engine_tui::attrs_from_ratatui(m)
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

    /// Every ratatui color the engine can be handed must convert
    /// identically through this crate's names and `hjkl-engine-tui`'s —
    /// they are one implementation, and a form widget must not disagree
    /// with a syntax span about the same color.
    #[test]
    fn conversions_agree_with_engine_tui() {
        let named = [
            RColor::Reset,
            RColor::Black,
            RColor::Red,
            RColor::Green,
            RColor::Yellow,
            RColor::Blue,
            RColor::Magenta,
            RColor::Cyan,
            RColor::Gray,
            RColor::DarkGray,
            RColor::LightRed,
            RColor::LightGreen,
            RColor::LightYellow,
            RColor::LightBlue,
            RColor::LightMagenta,
            RColor::LightCyan,
            RColor::White,
        ];
        // 0–15 named block, 16–231 cube, 232–255 gray ramp, plus edges.
        let indexed = [0u8, 1, 7, 8, 15, 16, 17, 100, 196, 231, 232, 244, 255];
        let rgb = [
            RColor::Rgb(0, 0, 0),
            RColor::Rgb(123, 45, 67),
            RColor::Rgb(255, 255, 255),
        ];

        let cases: Vec<RColor> = named
            .into_iter()
            .chain(indexed.into_iter().map(RColor::Indexed))
            .chain(rgb)
            .collect();

        for c in cases {
            // ratatui → engine, color level.
            assert_eq!(
                ratatui_to_engine_color(c),
                hjkl_engine_tui::color_from_ratatui(c),
                "color_from_ratatui disagrees for {c:?}"
            );
            // ratatui → engine, style level (fg + bg + modifiers).
            let rs = RStyle::default()
                .fg(c)
                .bg(c)
                .add_modifier(RMod::BOLD | RMod::DIM | RMod::CROSSED_OUT);
            assert_eq!(
                ratatui_to_engine_style(rs),
                hjkl_engine_tui::style_from_ratatui(rs),
                "style_from_ratatui disagrees for {c:?}"
            );
            // engine → ratatui, both levels, from the converted value.
            let ec = ratatui_to_engine_color(c);
            assert_eq!(
                engine_to_ratatui_color(ec),
                hjkl_engine_tui::color_to_ratatui(ec),
            );
            let es = ratatui_to_engine_style(rs);
            assert_eq!(
                engine_to_ratatui_style(es),
                hjkl_engine_tui::style_to_ratatui(es),
            );
        }

        // Attrs, every modeled bit.
        let all = Attrs::BOLD
            | Attrs::ITALIC
            | Attrs::UNDERLINE
            | Attrs::REVERSE
            | Attrs::DIM
            | Attrs::STRIKE;
        assert_eq!(
            engine_to_ratatui_attrs(all),
            hjkl_engine_tui::attrs_to_ratatui(all)
        );
        let m = engine_to_ratatui_attrs(all);
        assert_eq!(
            ratatui_to_engine_attrs(m),
            hjkl_engine_tui::attrs_from_ratatui(m)
        );
    }

    /// Indexed colors resolve through the real xterm-256 palette, not a
    /// black flatten — the drift `hjkl-engine-tui` used to have.
    #[test]
    fn indexed_uses_real_palette_on_both_entry_points() {
        // 16 = cube origin (0,0,0) is genuinely black; 21 = pure blue,
        // 196 = pure red, 244 = mid gray ramp — all non-black.
        for (i, want) in [
            (21u8, EColor(0, 0, 255)),
            (196, EColor(255, 0, 0)),
            (244, EColor(128, 128, 128)),
        ] {
            assert_eq!(ratatui_to_engine_color(RColor::Indexed(i)), want);
            assert_eq!(
                hjkl_engine_tui::style_from_ratatui(RStyle::default().fg(RColor::Indexed(i))).fg,
                Some(want)
            );
        }
    }

    #[test]
    fn attrs_roundtrip() {
        let a = Attrs::BOLD | Attrs::ITALIC | Attrs::STRIKE;
        let m = engine_to_ratatui_attrs(a);
        let back = ratatui_to_engine_attrs(m);
        assert_eq!(a, back);
    }
}
