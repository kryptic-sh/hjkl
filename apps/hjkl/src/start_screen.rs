use hjkl_splash::{CellKind, Layout, Splash, default_trail_color, presets};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

pub struct StartScreen {
    splash: Splash<'static>,
}

impl StartScreen {
    pub fn new() -> Self {
        Self {
            splash: Splash::new(presets::hjkl::ART, presets::hjkl::PATH),
        }
    }
}

pub fn render(frame: &mut Frame, area: Rect, screen: &StartScreen, theme: &crate::theme::AppTheme) {
    let layout = Layout::centered(
        area.width,
        area.height,
        presets::hjkl::ROWS,
        presets::hjkl::COLS,
    );

    let art_top = area.y + layout.origin_y;
    let art_left = area.x + layout.origin_x;

    let abs_layout = hjkl_splash::Layout {
        origin_x: art_left,
        origin_y: art_top,
        ..layout
    };

    let buf = frame.buffer_mut();
    for cell in screen.splash.cells(abs_layout) {
        if cell.x >= area.x + area.width || cell.y >= area.y + area.height {
            continue;
        }
        match cell.kind {
            CellKind::Art => {
                if let Some(buf_cell) = buf.cell_mut((cell.x, cell.y)) {
                    buf_cell.set_char(cell.ch);
                    buf_cell.set_style(Style::default().fg(theme.ui.text_dim));
                }
            }
            CellKind::Trail { age } => {
                let color: Color = default_trail_color(age).into();
                if let Some(buf_cell) = buf.cell_mut((cell.x, cell.y)) {
                    buf_cell.set_char(cell.ch);
                    buf_cell.set_style(Style::default().fg(color));
                }
            }
            CellKind::Cursor => {
                if let Some(buf_cell) = buf.cell_mut((cell.x, cell.y)) {
                    buf_cell.set_char(cell.ch);
                    buf_cell.set_style(
                        Style::default()
                            .fg(theme.ui.text)
                            .bg(theme.ui.cursor_line_bg),
                    );
                }
            }
        }
    }

    // ── hjkl-specific hint text ────────────────────────────────────────────
    let hint_style = Style::default().fg(theme.ui.text_dim);
    let cta = "press any key to start editing";
    let ex_hints = [(":e <file>", "open a file"), (":q", "quit")];

    let cmd_col_width = ex_hints.iter().map(|(cmd, _)| cmd.len()).max().unwrap_or(0);
    let gap = 3;
    let block_width = ex_hints
        .iter()
        .map(|(_, desc)| cmd_col_width + gap + desc.len())
        .max()
        .unwrap_or(0) as u16;

    let cta_y = art_top + presets::hjkl::ROWS + 1;
    if cta_y < area.y + area.height {
        let cta_len = cta.len() as u16;
        let x = area.x + area.width.saturating_sub(cta_len) / 2;
        let rect = Rect {
            x,
            y: cta_y,
            width: cta_len.min(area.width),
            height: 1,
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(cta, hint_style)])),
            rect,
        );
    }

    let block_x = area.x + area.width.saturating_sub(block_width) / 2;
    for (i, (cmd, desc)) in ex_hints.iter().enumerate() {
        let y = art_top + presets::hjkl::ROWS + 3 + i as u16;
        if y >= area.y + area.height {
            break;
        }
        let line = format!("{cmd:<cmd_col_width$}{:gap$}{desc}", "");
        let rect = Rect {
            x: block_x,
            y,
            width: block_width.min(area.width),
            height: 1,
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(line, hint_style)])),
            rect,
        );
    }
}
