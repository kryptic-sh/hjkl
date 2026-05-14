//! Ratatui adapters for hjkl-theme.

use hjkl_theme::{Color, Modifiers, StyleSpec};
use ratatui::style::{Color as RColor, Modifier as RMod, Style as RStyle};

/// Conversion from hjkl-theme types to ratatui style types.
///
/// `From` impls are not possible here because both the trait (`From`, in `core`)
/// and the target types (`ratatui::style::*`) are foreign to this crate. The
/// extension-trait pattern is the standard escape hatch.
pub trait ToRatatui {
    /// The corresponding ratatui type.
    type Output;
    /// Convert to the ratatui type.
    fn to_ratatui(&self) -> Self::Output;
}

impl ToRatatui for Color {
    type Output = RColor;
    /// Alpha channel is dropped; ratatui has no alpha support.
    fn to_ratatui(&self) -> RColor {
        RColor::Rgb(self.r, self.g, self.b)
    }
}

impl ToRatatui for Modifiers {
    type Output = RMod;
    /// Maps each modifier flag to the corresponding ratatui constant.
    fn to_ratatui(&self) -> RMod {
        let mut m = RMod::empty();
        if self.bold {
            m |= RMod::BOLD;
        }
        if self.italic {
            m |= RMod::ITALIC;
        }
        if self.underline {
            m |= RMod::UNDERLINED;
        }
        if self.reverse {
            m |= RMod::REVERSED;
        }
        if self.strikethrough {
            m |= RMod::CROSSED_OUT;
        }
        m
    }
}

impl ToRatatui for StyleSpec {
    type Output = RStyle;
    /// Sets fg/bg when present and applies modifiers.
    fn to_ratatui(&self) -> RStyle {
        let mut style = RStyle::default();
        if let Some(fg) = &self.fg {
            style = style.fg(fg.to_ratatui());
        }
        if let Some(bg) = &self.bg {
            style = style.bg(bg.to_ratatui());
        }
        style.add_modifier(self.modifiers.to_ratatui())
    }
}
