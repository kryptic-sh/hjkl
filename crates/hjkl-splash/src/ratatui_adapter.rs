//! Optional ratatui adapter — gated on the `ratatui` feature.

use crate::Rgb;

impl From<Rgb> for ratatui::style::Color {
    fn from(Rgb(r, g, b): Rgb) -> Self {
        ratatui::style::Color::Rgb(r, g, b)
    }
}
