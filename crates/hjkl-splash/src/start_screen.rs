//! High-level `StartScreen` wrapper for consumers that want a ready-to-use
//! splash animation wired to app palette colours.

use std::time::Instant;

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
    /// Wall-clock anchor for the splash animation, captured once at build time.
    ///
    /// Renderers rebuild a transient [`crate::Splash`] each frame; they must
    /// pass this anchor via [`crate::Splash::with_anchor`] so the animation
    /// advances across frames instead of re-anchoring to "now" every paint
    /// (which freezes the tick at 0).
    pub anchor: Instant,
}

impl StartScreen {
    /// Build a `StartScreen` for the given version string. Captures the
    /// animation clock anchor once, here — not per render.
    pub fn build(version: &str) -> Self {
        Self {
            version: version.to_string(),
            palette: StartScreenTheme::default(),
            anchor: Instant::now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Splash;
    use std::time::Duration;

    /// Regression for #251: a renderer rebuilds a transient `Splash` each frame
    /// but anchors it to the screen's persistent anchor, so the animation
    /// advances. Anchoring to a fresh `now` each frame (the bug) would freeze
    /// the tick at 0.
    #[test]
    fn screen_anchor_drives_advancing_animation() {
        let path: &[(u8, u8, char)] = &[(0, 0, 'a'), (0, 1, 'b')];
        let mut screen = StartScreen::build("0.0.0");
        // Simulate the screen having existed for several animation periods.
        screen.anchor = Instant::now() - Duration::from_millis(600);

        // Consumer pattern (post-fix): transient Splash anchored to the screen.
        let framed = Splash::new("ab", path).with_anchor(screen.anchor).tick();
        assert!(
            framed >= 4,
            "screen-anchored tick should advance with elapsed time, got {framed}"
        );

        // Buggy pattern (pre-fix): fresh anchor each frame → frozen at 0.
        let frozen = Splash::new("ab", path).tick();
        assert_eq!(frozen, 0);
    }
}
