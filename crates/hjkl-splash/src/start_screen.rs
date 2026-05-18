//! High-level `StartScreen` wrapper for consumers that want a ready-to-use
//! splash animation wired to app palette colours.

use crate::Rgb;

/// Theme colours consumed by the start-screen renderer.
#[derive(Clone, Debug)]
pub struct StartScreenTheme {
    /// Dimmed text colour — used for static art glyphs.
    pub text_dim: Rgb,
    /// Primary text colour — used for trail cells.
    pub text: Rgb,
    /// Cursor-line background — used for the cursor cell.
    pub cursor_line_bg: Rgb,
}

impl Default for StartScreenTheme {
    fn default() -> Self {
        Self {
            text_dim: Rgb(0x3b, 0x42, 0x52),
            text: Rgb(0xd8, 0xde, 0xe9),
            cursor_line_bg: Rgb(0x2e, 0x34, 0x40),
        }
    }
}

/// Ready-to-render start screen state.
pub struct StartScreen {
    /// Version string shown below the art (e.g. `"0.23.0"`).
    pub version: String,
    /// Palette used by the renderer.
    pub palette: StartScreenTheme,
}

impl StartScreen {
    /// Build a `StartScreen` for the given version string.
    pub fn build(version: &str) -> Self {
        Self {
            version: version.to_string(),
            palette: StartScreenTheme::default(),
        }
    }
}
