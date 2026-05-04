use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

pub struct StartScreen {
    pub tick: u64,
}

impl StartScreen {
    pub fn new() -> Self {
        Self { tick: 0 }
    }

    pub fn advance(&mut self) {
        self.tick = self.tick.wrapping_add(1);
    }
}

// Art from art.txt is 5 rows × 32 cols. Each entry is (row, col, segment_char).
// The cursor traces: H left-vert → crossbar → right-vert, J top→down→hook,
// K right-vert bottom-to-top → upper arm → lower arm, L vert → bottom bar.
#[rustfmt::skip]
const PATH: &[(u8, u8, char)] = &[
    // H: left vertical top→bottom
    (0, 0, 'h'), (1, 0, 'h'), (2, 0, 'h'), (3, 0, 'h'), (4, 0, 'h'),
    // H: crossbar left→right (row 2)
    (2, 1, 'h'), (2, 2, 'h'), (2, 3, 'h'), (2, 4, 'h'), (2, 5, 'h'), (2, 6, 'h'), (2, 7, 'h'),
    // H: right vertical bottom→top
    (4, 5, 'h'), (3, 5, 'h'), (1, 5, 'h'), (0, 5, 'h'),
    // J: main vertical top→bottom
    (0, 13, 'j'), (1, 13, 'j'), (2, 13, 'j'), (3, 13, 'j'), (4, 13, 'j'),
    // J: hook — row 3 leftward then row 4 leftward
    (3, 9, 'j'), (3, 8, 'j'),
    (4, 12, 'j'), (4, 11, 'j'), (4, 10, 'j'), (4, 9, 'j'), (4, 8, 'j'),
    // K: left vertical bottom→top
    (4, 13, 'k'), (3, 13, 'k'), (2, 13, 'k'), (1, 13, 'k'), (0, 13, 'k'),
    // K: upper arm row 0→2 going right (diagonal)
    (0, 21, 'k'), (0, 22, 'k'), (0, 23, 'k'), (0, 24, 'k'), (0, 25, 'k'), (0, 26, 'k'),
    (1, 20, 'k'), (1, 21, 'k'), (1, 22, 'k'), (1, 23, 'k'), (1, 24, 'k'), (1, 25, 'k'), (1, 26, 'k'),
    (2, 19, 'k'), (2, 20, 'k'), (2, 21, 'k'), (2, 22, 'k'),
    // K: lower arm rows 3→4 going right
    (3, 16, 'k'), (3, 17, 'k'), (3, 18, 'k'), (3, 19, 'k'), (3, 20, 'k'), (3, 21, 'k'), (3, 22, 'k'),
    (4, 16, 'k'), (4, 17, 'k'), (4, 18, 'k'), (4, 21, 'k'), (4, 22, 'k'), (4, 23, 'k'), (4, 24, 'k'), (4, 25, 'k'),
    // L: vertical top→bottom
    (0, 24, 'l'), (1, 24, 'l'), (2, 24, 'l'), (3, 24, 'l'), (4, 24, 'l'),
    // L: bottom stroke left→right (row 4)
    (4, 25, 'l'), (4, 26, 'l'), (4, 27, 'l'), (4, 28, 'l'), (4, 29, 'l'), (4, 30, 'l'), (4, 31, 'l'),
];

// Trail length — how many past ticks to keep lit with fading intensity.
const TRAIL: usize = 6;

// Art is 5 rows tall, 32 cols wide.
const ART_ROWS: u16 = 5;
const ART_COLS: u16 = 32;

const ART: &str = include_str!("art.txt");

// Four brightness steps from brightest → invisible, derived from a grey ramp.
// Index 0 = newest (brightest), index TRAIL-1 = oldest (dimmest).
fn trail_color(age: usize) -> Color {
    match age {
        0 => Color::Rgb(0xe5, 0xe9, 0xf0), // near-white
        1 => Color::Rgb(0xa0, 0xa8, 0xb8), // mid-bright
        2 => Color::Rgb(0x60, 0x68, 0x78), // mid
        3 => Color::Rgb(0x38, 0x40, 0x50), // dim
        4 => Color::Rgb(0x20, 0x26, 0x32), // very dim
        _ => Color::Rgb(0x10, 0x14, 0x1c), // barely visible
    }
}

pub fn render(frame: &mut Frame, area: Rect, screen: &StartScreen, theme: &crate::theme::AppTheme) {
    let cursor_idx = screen.tick as usize % PATH.len();

    // Center the art block in `area`.
    let art_top = area.y + area.height.saturating_sub(ART_ROWS + 4) / 2;
    let art_left = area.x + area.width.saturating_sub(ART_COLS) / 2;

    // Render the art rows as plain styled paragraphs first.
    for (row_idx, art_line) in ART.lines().take(ART_ROWS as usize).enumerate() {
        let y = art_top + row_idx as u16;
        if y >= area.y + area.height {
            break;
        }
        let art_rect = Rect {
            x: art_left,
            y,
            width: ART_COLS.min(area.width),
            height: 1,
        };
        let para = Paragraph::new(art_line)
            .style(Style::default().fg(theme.ui.text_dim).bg(theme.ui.panel_bg));
        frame.render_widget(para, art_rect);
    }

    // Paint the animated trail over the art cells.
    let buf = frame.buffer_mut();
    for age in (0..=TRAIL).rev() {
        let idx = if cursor_idx + PATH.len() >= age {
            (cursor_idx + PATH.len() - age) % PATH.len()
        } else {
            0
        };
        let (row, col, seg_char) = PATH[idx];
        let x = art_left + col as u16;
        let y = art_top + row as u16;
        if x < area.x + area.width && y < area.y + area.height {
            if age == 0 {
                // Current cursor cell: highlighted bg + segment char.
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_char(seg_char);
                    cell.set_style(
                        Style::default()
                            .fg(theme.ui.panel_bg)
                            .bg(theme.ui.cursor_line_bg),
                    );
                }
            } else {
                // Trail cell: fading foreground, no bg override.
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_char(seg_char);
                    cell.set_style(Style::default().fg(trail_color(age - 1)));
                }
            }
        }
    }

    // Call-to-action centered on its own row; the ex-command hints render
    // as a left-aligned block so their `:` columns line up vertically.
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

    let cta_y = art_top + ART_ROWS + 1;
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
        let y = art_top + ART_ROWS + 3 + i as u16;
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
