//! High-level `StartScreen` wrapper for consumers that want a ready-to-use
//! splash animation wired to app palette colours.
//!
//! Gated on the `ratatui` feature so that headless / no-std consumers can
//! depend on `hjkl-splash` without pulling in ratatui.

use crate::Rgb;
#[cfg(feature = "ratatui")]
use crate::{Layout, Splash, presets};

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

/// Render the start screen into a ratatui `Frame`.
///
/// Paints the hjkl art animation centred in `area`, followed by a dim version
/// line and a hint line below it.
#[cfg(feature = "ratatui")]
pub fn render(frame: &mut ratatui::Frame, area: ratatui::layout::Rect, screen: &StartScreen) {
    use crate::{CellKind, default_trail_color};
    use ratatui::{
        layout::Rect,
        style::{Color, Style},
        text::{Line, Span},
        widgets::Widget,
    };

    let splash = Splash::new(presets::hjkl::ART, presets::hjkl::PATH);
    let layout = Layout::centered(
        area.width,
        area.height,
        presets::hjkl::ROWS,
        presets::hjkl::COLS,
    );

    let buf = frame.buffer_mut();

    // Paint art + animation cells.
    for cell in splash.cells(layout) {
        let x = area.x + cell.x;
        let y = area.y + cell.y;
        if x >= area.x + area.width || y >= area.y + area.height {
            continue;
        }
        let buf_cell = buf.cell_mut((x, y)).unwrap_or_else(|| {
            // Safety: we bounds-checked above; if the cell is somehow missing
            // just skip this iteration via a panic-free path.
            panic!("start_screen: cell ({x},{y}) out of buffer bounds");
        });
        buf_cell.set_char(cell.ch);
        let style = match cell.kind {
            CellKind::Art => Style::default().fg(Color::from(screen.palette.text_dim)),
            CellKind::Trail { age } => {
                let trail_rgb = default_trail_color(age);
                Style::default().fg(Color::from(trail_rgb))
            }
            CellKind::Cursor => Style::default()
                .fg(Color::from(screen.palette.text))
                .bg(Color::from(screen.palette.cursor_line_bg)),
        };
        buf_cell.set_style(style);
    }

    // Version line — two rows below the art block.
    let ver_y = area.y + layout.origin_y + presets::hjkl::ROWS + 1;
    if ver_y < area.y + area.height {
        let ver_line = Line::from(vec![Span::styled(
            format!("  hjkl v{}", screen.version),
            Style::default().fg(Color::from(screen.palette.text_dim)),
        )]);
        let ver_area = Rect {
            x: area.x + layout.origin_x,
            y: ver_y,
            width: area.width.saturating_sub(layout.origin_x),
            height: 1,
        };
        ver_line.render(ver_area, buf);
    }

    // Hint line — one row below the version.
    let hint_y = ver_y + 1;
    if hint_y < area.y + area.height {
        let hint_line = Line::from(vec![Span::styled(
            "  :e <file>  to open",
            Style::default().fg(Color::from(screen.palette.text_dim)),
        )]);
        let hint_area = Rect {
            x: area.x + layout.origin_x,
            y: hint_y,
            width: area.width.saturating_sub(layout.origin_x),
            height: 1,
        };
        hint_line.render(hint_area, buf);
    }
}
