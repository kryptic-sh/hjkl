//! Per-frame render functions.
//!
//! [`frame`] is the top-level entry point called from the event loop.
//! It splits the terminal area into a buffer pane + status line row and
//! delegates to [`buffer_pane`] and [`status_line`].

use hjkl_buffer::{BufferView, Gutter};
use hjkl_engine::{Host, Query};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::app::{App, STATUS_LINE_HEIGHT};

/// Gutter width: 4 cells (3 digits + 1 spacer). Matches the Phase 2
/// spec layout. Grows to 5 at 10 000 lines; fine for Phase 2.
const GUTTER_WIDTH: u16 = 4;

/// Render one complete frame into `frame`.
pub fn frame(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(STATUS_LINE_HEIGHT)])
        .split(area);

    let buf_area = chunks[0];
    let status_area = chunks[1];

    // Tell the host the text area dimensions so scrolloff math is accurate.
    // text_width excludes the gutter.
    {
        let vp = app.editor.host_mut().viewport_mut();
        vp.width = buf_area.width;
        vp.height = buf_area.height;
        vp.text_width = buf_area.width.saturating_sub(GUTTER_WIDTH);
    }

    buffer_pane(frame, app, buf_area);
    status_line(frame, app, status_area);
}

/// Render the buffer pane with line numbers, text, and the cursor.
fn buffer_pane(frame: &mut Frame, app: &mut App, area: ratatui::layout::Rect) {
    let gutter = Gutter {
        width: GUTTER_WIDTH,
        style: Style::default().fg(Color::DarkGray),
    };

    let selection = app.editor.buffer_selection();
    let buffer_spans = app.editor.buffer_spans();
    let search_pattern = app.editor.search_state().pattern.as_ref();

    let view = BufferView {
        buffer: app.editor.buffer(),
        viewport: app.editor.host().viewport(),
        selection,
        resolver: &(|_: u32| Style::default()),
        cursor_line_bg: Style::default(),
        cursor_column_bg: Style::default(),
        selection_bg: Style::default().bg(Color::Blue),
        cursor_style: Style::default().add_modifier(Modifier::REVERSED),
        gutter: Some(gutter),
        search_bg: Style::default(),
        signs: &[],
        conceals: &[],
        spans: buffer_spans,
        search_pattern,
    };

    frame.render_widget(view, area);

    // Position the terminal cursor so the OS/terminal sees it correctly.
    if let Some((cx, cy)) = app.editor.cursor_screen_pos_in_rect(area) {
        frame.set_cursor_position((cx, cy));
    }
}

/// Render the one-row status line.
///
/// When the user is typing a `:` command, the status area shows the command
/// prompt instead of the normal mode/file/cursor info.
/// When a status message (ex-command result) is pending, it takes priority
/// over the normal right-hand cursor position info.
fn status_line(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let status = build_status_line(app, area.width);
    let paragraph = Paragraph::new(status);
    frame.render_widget(paragraph, area);
}

/// Build the status line text as a ratatui [`Line`].
///
/// Priority (highest first):
/// 1. Command input (user typing `:cmd`) — shows `:{typed_text}`.
/// 2. Status message (ex-command result) — shown until next keypress.
/// 3. Normal mode/filename/cursor line.
fn build_status_line(app: &App, width: u16) -> Line<'static> {
    // ── Command prompt ───────────────────────────────────────────────────────
    if let Some(ref cmd) = app.command_input {
        let content = format!(":{}", cmd.text);
        // Pad to width so the background fills the row.
        let padded = format!("{content:<width$}", width = width as usize);
        return Line::from(vec![Span::styled(
            padded,
            Style::default().bg(Color::DarkGray).fg(Color::White),
        )]);
    }

    // ── Status message (ex-command result) ──────────────────────────────────
    if let Some(ref msg) = app.status_message {
        let content = format!(" {msg}");
        let padded = format!("{content:<width$}", width = width as usize);
        return Line::from(vec![Span::styled(
            padded,
            Style::default().bg(Color::DarkGray).fg(Color::White),
        )]);
    }

    // ── Normal status line ───────────────────────────────────────────────────
    let mode = app.mode_label();

    // Dirty marker — `*` when the buffer has unsaved changes.
    let dirty = if app.dirty { "*" } else { " " };

    let filename: String = app
        .filename
        .as_ref()
        .and_then(|p| p.to_str())
        .unwrap_or("[No Name]")
        .to_owned();

    let (row, col) = app.editor.cursor();
    let line_count = app.editor.buffer().line_count() as usize;
    let pct = ((row + 1) * 100).checked_div(line_count).unwrap_or(0);
    let pos = format!("{}:{}", row + 1, col + 1);
    let pct_str = format!("{pct}%");

    // Left side: `MODE dirty filename`
    let left = format!(" {mode}  {dirty} {filename}");
    // Right side: `pos  pct`
    let right = format!("{pos}  {pct_str} ");

    // Pad the centre spacer so left + spacer + right == width.
    let used = left.len() + right.len();
    let pad_count = (width as usize).saturating_sub(used);
    let spacer: String = " ".repeat(pad_count);

    let content = format!("{left}{spacer}{right}");

    Line::from(vec![Span::styled(
        content,
        Style::default().bg(Color::DarkGray).fg(Color::White),
    )])
}

/// Format the status line as a plain string (unit-test helper).
#[allow(dead_code)]
pub fn format_status_line(
    mode: &str,
    filename: &str,
    dirty: bool,
    row: usize,
    col: usize,
    total_lines: usize,
    width: u16,
) -> String {
    let dirty_marker = if dirty { "*" } else { " " };
    let pct = ((row + 1) * 100).checked_div(total_lines).unwrap_or(0);
    let pos = format!("{}:{}", row + 1, col + 1);
    let pct_str = format!("{pct}%");
    let left = format!(" {mode}  {dirty_marker} {filename}");
    let right = format!("{pos}  {pct_str} ");
    let used = left.len() + right.len();
    let pad_count = (width as usize).saturating_sub(used);
    let spacer: String = " ".repeat(pad_count);
    format!("{left}{spacer}{right}")
}

/// Format the write-success status message. Used in tests.
#[cfg(test)]
pub fn format_write_message(path: &str, lines: usize, bytes: usize) -> String {
    format!("\"{}\" {}L, {}B written", path, lines, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_line_normal_mode_no_name() {
        let s = format_status_line("NORMAL", "[No Name]", false, 0, 0, 1, 60);
        assert!(s.contains("NORMAL"));
        assert!(s.contains("[No Name]"));
        assert!(s.contains("1:1"));
        assert!(s.contains("100%"));
    }

    #[test]
    fn status_line_dirty_marker() {
        let clean = format_status_line("NORMAL", "foo.txt", false, 0, 0, 1, 60);
        let dirty = format_status_line("NORMAL", "foo.txt", true, 0, 0, 1, 60);
        assert!(clean.contains(" [No Name]") || clean.contains(" foo.txt"));
        // dirty marker is `*` in dirty, ` ` in clean
        let dirty_idx = dirty.find('*');
        assert!(dirty_idx.is_some(), "dirty status should contain '*'");
        let clean_contains_star = clean.contains('*');
        assert!(!clean_contains_star, "clean status should not contain '*'");
    }

    #[test]
    fn status_line_percentage() {
        let s = format_status_line("NORMAL", "f.txt", false, 4, 0, 10, 60);
        // row 4 of 10 = 50%
        assert!(s.contains("50%"));
    }

    #[test]
    fn status_line_fits_width() {
        let width: u16 = 40;
        let s = format_status_line("INSERT", "myfile.rs", true, 0, 0, 100, width);
        assert_eq!(s.len(), width as usize);
    }

    #[test]
    fn write_message_format() {
        let msg = format_write_message("/tmp/foo.txt", 10, 128);
        assert_eq!(msg, "\"/tmp/foo.txt\" 10L, 128B written");
    }
}
