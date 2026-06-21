//! Ratatui adapter for `hjkl-splash` — renders a [`hjkl_splash::StartScreen`]
//! into a ratatui [`Frame`].

use hjkl_splash::{
    CellKind, Layout, Rgb, Splash, default_trail_color, presets, start_screen::StartScreen,
};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Widget,
};

fn rgb_to_color(Rgb(r, g, b): Rgb) -> Color {
    Color::Rgb(r, g, b)
}

/// Render the start screen into a ratatui `Frame`.
///
/// Paints the hjkl art animation centred in `area`, followed by a dim version
/// line and a hint line below it.
pub fn render(frame: &mut Frame, area: Rect, screen: &StartScreen) {
    // Anchor the animation clock to the screen's persistent anchor (captured
    // once at build), NOT `Instant::now()` — `render` runs every frame, so a
    // fresh anchor each call would pin the tick at 0 and freeze the animation.
    let splash = Splash::new(presets::hjkl::ART, presets::hjkl::PATH).with_anchor(screen.anchor);
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
            CellKind::Art => Style::default().fg(rgb_to_color(screen.palette.text_dim)),
            CellKind::Trail { age } => {
                let trail_rgb = default_trail_color(age);
                Style::default().fg(rgb_to_color(trail_rgb))
            }
            CellKind::Cursor => Style::default()
                .fg(rgb_to_color(screen.palette.text))
                .bg(rgb_to_color(screen.palette.cursor_line_bg)),
        };
        buf_cell.set_style(style);
    }

    // Version line — two rows below the art block.
    let ver_y = area.y + layout.origin_y + presets::hjkl::ROWS + 1;
    if ver_y < area.y + area.height {
        let ver_line = Line::from(vec![Span::styled(
            format!("  hjkl v{}", screen.version),
            Style::default().fg(rgb_to_color(screen.palette.text_dim)),
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
            Style::default().fg(rgb_to_color(screen.palette.text_dim)),
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
