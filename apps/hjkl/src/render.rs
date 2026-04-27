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
///
/// The buffer-pane cursor is suppressed when the user is typing in the
/// command line (`:` prompt or `/`/`?` search prompt), because the
/// terminal cursor belongs to the bottom row in those states.
fn buffer_pane(frame: &mut Frame, app: &mut App, area: ratatui::layout::Rect) {
    let gutter = Gutter {
        width: GUTTER_WIDTH,
        style: Style::default().fg(Color::DarkGray),
    };

    let selection = app.editor.buffer_selection();
    let buffer_spans = app.editor.buffer_spans();
    let search_pattern = app.editor.search_state().pattern.as_ref();
    let in_prompt = app.command_input.is_some() || app.editor.search_prompt().is_some();

    // Use a subtle yellow background for search match highlighting (vim's `Search` hl).
    let search_bg = if search_pattern.is_some() {
        Style::default()
            .bg(Color::Rgb(147, 103, 0))
            .fg(Color::White)
    } else {
        Style::default()
    };

    // Bind the style table after the viewport mutation above to avoid a
    // double-borrow on `app.editor` (host_mut() and style_table() both
    // require access to the editor).
    let style_table = app.editor.style_table().to_owned();
    let resolver = move |id: u32| style_table.get(id as usize).copied().unwrap_or_default();

    let view = BufferView {
        buffer: app.editor.buffer(),
        viewport: app.editor.host().viewport(),
        selection,
        resolver: &resolver,
        cursor_line_bg: Style::default(),
        cursor_column_bg: Style::default(),
        selection_bg: Style::default().bg(Color::Blue),
        cursor_style: if in_prompt {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        },
        gutter: Some(gutter),
        search_bg,
        signs: &[],
        conceals: &[],
        spans: buffer_spans,
        search_pattern,
    };

    frame.render_widget(view, area);

    // Suppress the buffer-pane cursor while the user is typing in the
    // command line or search prompt — the cursor belongs to the status row.
    if !in_prompt && let Some((cx, cy)) = app.editor.cursor_screen_pos_in_rect(area) {
        frame.set_cursor_position((cx, cy));
    }
}

/// Render the one-row status line.
///
/// When the user is typing a `:` command or a `/`/`?` search, the status
/// area shows the prompt instead of the normal mode/file/cursor info, and
/// the terminal cursor is moved to the insertion point.
fn status_line(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let (status, cursor_col) = build_status_line(app, area.width);
    let paragraph = Paragraph::new(status);
    frame.render_widget(paragraph, area);

    // Move the terminal cursor to the insertion point in the prompt row.
    if let Some(col) = cursor_col {
        frame.set_cursor_position((area.x + col, area.y));
    }
}

/// Build the status line text as a ratatui [`Line`].
///
/// Returns `(line, Some(cursor_col))` when a prompt is active so the
/// caller can position the terminal cursor at the insertion point.
///
/// Priority (highest first):
/// 1. Command input (user typing `:cmd`) — shows `:{typed_text}`.
/// 2. Engine search prompt (`/` or `?`) — shows `/{typed_text}`.
/// 3. Status message (ex-command result) — shown until next keypress.
/// 4. Normal mode/filename/cursor line.
fn build_status_line(app: &App, width: u16) -> (Line<'static>, Option<u16>) {
    // ── Command prompt (`:`) ─────────────────────────────────────────────────
    if let Some(ref cmd) = app.command_input {
        let content = format!(":{}", cmd.text);
        // Pad to width so the background fills the row.
        let padded = format!("{content:<width$}", width = width as usize);
        let cursor_col = cmd.display_cursor_col(1); // 1 = width of `:`
        return (
            Line::from(vec![Span::styled(
                padded,
                Style::default().bg(Color::DarkGray).fg(Color::White),
            )]),
            Some(cursor_col),
        );
    }

    // ── Engine search prompt (`/` or `?`) ────────────────────────────────────
    if let Some(sp) = app.editor.search_prompt() {
        let prefix = if sp.forward { '/' } else { '?' };
        let content = format!("{prefix}{}", sp.text);
        let padded = format!("{content:<width$}", width = width as usize);
        // cursor position inside the prompt text (byte-counted in ASCII context)
        let cursor_col = 1u16 + sp.text[..sp.cursor.min(sp.text.len())].chars().count() as u16;
        return (
            Line::from(vec![Span::styled(
                padded,
                Style::default().bg(Color::DarkGray).fg(Color::White),
            )]),
            Some(cursor_col),
        );
    }

    // ── Status message (ex-command result) ──────────────────────────────────
    if let Some(ref msg) = app.status_message {
        let content = format!(" {msg}");
        let padded = format!("{content:<width$}", width = width as usize);
        return (
            Line::from(vec![Span::styled(
                padded,
                Style::default().bg(Color::DarkGray).fg(Color::White),
            )]),
            None,
        );
    }

    // ── Normal status line ───────────────────────────────────────────────────
    let mode = app.mode_label();

    // Dirty marker — `*` when the buffer has unsaved changes.
    let dirty = if app.dirty { "*" } else { " " };

    // Readonly indicator.
    let ro_tag = if app.editor.is_readonly() {
        " [RO]"
    } else {
        ""
    };

    // New-file annotation — shown until the user edits or saves.
    let new_tag = if app.is_new_file { " [New File]" } else { "" };

    let raw_filename: String = app
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

    // Right side is fixed width — reserve it first.
    // Format: `pos  pct ` (trailing space).
    let right = format!("{pos}  {pct_str} ");
    // Left prefix before filename: ` MODE  d ` + ro_tag + new_tag.
    let left_prefix = format!(" {mode}  {dirty} ");
    let suffix = format!("{ro_tag}{new_tag}");

    // Available columns for the filename.
    let w = width as usize;
    let reserved = left_prefix.len() + suffix.len() + right.len();
    let avail_for_name = w.saturating_sub(reserved);

    // Truncate filename with leading `…` when it doesn't fit (vim style).
    let filename: String = if raw_filename.len() <= avail_for_name {
        raw_filename.clone()
    } else if avail_for_name <= 1 {
        String::new()
    } else {
        let keep = avail_for_name.saturating_sub(1); // 1 char for `…`
        let start = raw_filename.len().saturating_sub(keep);
        format!("\u{2026}{}", &raw_filename[start..])
    };

    // Left side: ` MODE  dirty filename[RO][New File]`
    let left = format!("{left_prefix}{filename}{suffix}");

    // Pad the centre spacer so left + spacer + right == width.
    let used = left.len() + right.len();
    let pad_count = w.saturating_sub(used);
    let spacer: String = " ".repeat(pad_count);

    let content = format!("{left}{spacer}{right}");

    (
        Line::from(vec![Span::styled(
            content,
            Style::default().bg(Color::DarkGray).fg(Color::White),
        )]),
        None,
    )
}

/// Format the status line as a plain string (unit-test helper).
///
/// `readonly` and `is_new_file` mirror the app state flags.
/// Filename is truncated with `…` when necessary.
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
    format_status_line_full(
        mode,
        filename,
        dirty,
        false,
        false,
        row,
        col,
        total_lines,
        width,
    )
}

/// Full status line formatter with readonly + new-file flags.
#[allow(clippy::too_many_arguments)]
pub fn format_status_line_full(
    mode: &str,
    filename: &str,
    dirty: bool,
    readonly: bool,
    is_new_file: bool,
    row: usize,
    col: usize,
    total_lines: usize,
    width: u16,
) -> String {
    let dirty_marker = if dirty { "*" } else { " " };
    let ro_tag = if readonly { " [RO]" } else { "" };
    let new_tag = if is_new_file { " [New File]" } else { "" };
    let pct = ((row + 1) * 100).checked_div(total_lines).unwrap_or(0);
    let pos = format!("{}:{}", row + 1, col + 1);
    let pct_str = format!("{pct}%");
    let right = format!("{pos}  {pct_str} ");
    let left_prefix = format!(" {mode}  {dirty_marker} ");
    let suffix = format!("{ro_tag}{new_tag}");
    let w = width as usize;
    let reserved = left_prefix.len() + suffix.len() + right.len();
    let avail_for_name = w.saturating_sub(reserved);
    let truncated: String = if filename.len() <= avail_for_name {
        filename.to_string()
    } else if avail_for_name <= 1 {
        String::new()
    } else {
        let keep = avail_for_name.saturating_sub(1);
        let start = filename.len().saturating_sub(keep);
        format!("\u{2026}{}", &filename[start..])
    };
    let left = format!("{left_prefix}{truncated}{suffix}");
    let used = left.len() + right.len();
    let pad_count = w.saturating_sub(used);
    let spacer = " ".repeat(pad_count);
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

    #[test]
    fn status_line_readonly_tag() {
        let s = format_status_line_full("NORMAL", "foo.txt", false, true, false, 0, 0, 1, 80);
        assert!(s.contains("[RO]"), "readonly tag must appear");
    }

    #[test]
    fn status_line_new_file_tag() {
        let s = format_status_line_full("NORMAL", "newfile.txt", false, false, true, 0, 0, 1, 80);
        assert!(s.contains("[New File]"), "new-file tag must appear");
    }

    #[test]
    fn status_line_truncates_long_filename() {
        // Very narrow terminal — filename must be truncated.
        let long = "some/very/long/path/to/a/deeply/nested/file.rs";
        let s = format_status_line_full("NORMAL", long, false, false, false, 0, 0, 1, 30);
        // Truncated filename starts with `…`
        assert!(
            s.contains('\u{2026}'),
            "truncated filename must start with …"
        );
    }

    #[test]
    fn status_line_arg_parsing_plus_n() {
        // Smoke test: +5 → goto_line=Some(5). Tested via parse logic in main.
        // Here we verify the status-line can handle being on line 5.
        let s = format_status_line("NORMAL", "file.txt", false, 4, 0, 10, 60);
        // row 4 (0-based) → 5 in display
        assert!(s.contains("5:1"));
    }
}
