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

/// Gutter width formula — matches `Editor::cursor_screen_pos`'s
/// `lnum_width = line_count.to_string().len() + 2`. The renderer must
/// agree with the engine or terminal cursor lands off by one column.
fn gutter_width(line_count: usize) -> u16 {
    line_count.to_string().len() as u16 + 2
}

/// Render one complete frame into `frame`.
pub fn frame(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(STATUS_LINE_HEIGHT)])
        .split(area);

    let buf_area = chunks[0];
    let status_area = chunks[1];

    let gw = gutter_width(app.editor.buffer().line_count() as usize);
    let text_width = buf_area.width.saturating_sub(gw);

    // Publish viewport dims so engine scrolloff math is accurate.
    // `width` is the text-area width (gutter excluded) — `Viewport::ensure_visible`
    // uses it as the horizontal cursor band, and the cursor lives in the text area.
    {
        let vp = app.editor.host_mut().viewport_mut();
        vp.width = text_width;
        vp.height = buf_area.height;
        vp.text_width = text_width;
    }
    // Publish height to the engine's atomic so scrolloff (5-row margin) engages.
    app.editor.set_viewport_height(buf_area.height);

    // Refresh syntax spans against the now-current viewport. On the first
    // frame, App::new ran the initial parse with `viewport.height = 0`
    // (the atomic's init value) so only row 0 had spans installed. With
    // the source/tree cache + parse-skip on unchanged buffers, this call
    // is ~140µs even on 100k-line files.
    app.recompute_and_install();

    buffer_pane(frame, app, buf_area, gw);
    status_line(frame, app, status_area);
}

/// Render the buffer pane with line numbers, text, and the cursor.
///
/// The buffer-pane cursor is suppressed when the user is typing in the
/// command line (`:` prompt or `/`/`?` search prompt), because the
/// terminal cursor belongs to the bottom row in those states.
fn buffer_pane(frame: &mut Frame, app: &mut App, area: ratatui::layout::Rect, gutter_width: u16) {
    let gutter = Gutter {
        width: gutter_width,
        style: Style::default().fg(Color::DarkGray),
    };

    let selection = app.editor.buffer_selection();
    let buffer_spans = app.editor.buffer_spans();
    let search_pattern = app.editor.search_state().pattern.as_ref();
    let in_prompt = app.command_field.is_some() || app.search_field.is_some();

    // Merge diagnostic + git signs, filtered to the visible viewport so
    // BufferView's per-row linear scan stays cheap on large files.
    let vp_top = app.editor.host().viewport().top_row;
    let vp_bot = vp_top + area.height as usize;
    let mut visible_signs: Vec<hjkl_buffer::Sign> = app
        .diag_signs
        .iter()
        .copied()
        .filter(|s| s.row >= vp_top && s.row < vp_bot)
        .chain(
            app.git_signs
                .iter()
                .copied()
                .filter(|s| s.row >= vp_top && s.row < vp_bot),
        )
        .collect();
    // Stable sort by row keeps BufferView's max_by_key dedupe deterministic.
    visible_signs.sort_by_key(|s| s.row);

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
        signs: &visible_signs,
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

/// Render a prompt status row: prompt content on the left, a right-aligned
/// `[I]` (Insert) or `[N]` (Normal) mode tag for users on terminals that
/// don't render cursor-shape changes (or who want a discoverable visual cue).
fn prompt_line(content: &str, mode: hjkl_form::VimMode, width: u16) -> Line<'static> {
    // Insert: warm dark gray (active typing). Normal: cooler blue-tinted
    // dark (navigating). Subtle ambient cue layered on top of cursor shape
    // + the [I]/[N] tag.
    let (bg, tag, tag_fg) = match mode {
        hjkl_form::VimMode::Insert => (Color::DarkGray, " [I]", Color::Yellow),
        _ => (Color::Rgb(35, 40, 60), " [N]", Color::Gray),
    };
    let body_width = (width as usize).saturating_sub(tag.len());
    let visible: String = content.chars().take(body_width).collect();
    let body = format!("{visible:<body_width$}");
    Line::from(vec![
        Span::styled(body, Style::default().bg(bg).fg(Color::White)),
        Span::styled(tag, Style::default().bg(bg).fg(tag_fg)),
    ])
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
    if let Some(ref field) = app.command_field {
        let text = field.text();
        let display: String = text.lines().next().unwrap_or("").to_string();
        let content = format!(":{display}");
        let (_, ccol) = field.cursor();
        let cursor_col = 1u16 + ccol as u16;
        return (
            prompt_line(&content, field.vim_mode(), width),
            Some(cursor_col),
        );
    }

    // ── Host search prompt (`/` or `?`) ──────────────────────────────────────
    if let Some(ref field) = app.search_field {
        let prefix = match app.search_dir {
            crate::app::SearchDir::Forward => '/',
            crate::app::SearchDir::Backward => '?',
        };
        let text = field.text();
        let display: String = text.lines().next().unwrap_or("").to_string();
        let content = format!("{prefix}{display}");
        let (_, ccol) = field.cursor();
        let cursor_col = 1u16 + ccol as u16;
        return (
            prompt_line(&content, field.vim_mode(), width),
            Some(cursor_col),
        );
    }

    // ── Perf overlay (toggled via `:perf`) ──────────────────────────────────
    if app.perf_overlay {
        let p = &app.last_perf;
        let content = format!(
            " perf  total={}µs src={} parse={} hl={} byrow={} diag={} install={} sig={} git={} | runs={} hits={} thr={} ",
            app.last_recompute_us,
            p.source_build_us,
            p.parse_us,
            p.highlight_us,
            p.by_row_us,
            p.diag_us,
            app.last_install_us,
            app.last_signature_us,
            app.last_git_us,
            app.recompute_runs,
            app.recompute_hits,
            app.recompute_throttled,
        );
        let padded = format!("{content:<width$}", width = width as usize);
        return (
            Line::from(vec![Span::styled(
                padded,
                Style::default().bg(Color::DarkGray).fg(Color::White),
            )]),
            None,
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

    // ── Normal status line (lualine-style colored sections) ─────────────────
    // Palette derived from default-dark.toml (hjkl-tree-sitter): mode
    // colours map to keyword (purple), string (green), function.builtin
    // (blue) so the status bar visually matches the syntax theme.
    const HJ_BASE: Color = Color::Rgb(46, 52, 64); // overall bg fill
    const HJ_SURFACE: Color = Color::Rgb(59, 66, 82); // mid sections (filename / position)
    const HJ_TEXT: Color = Color::Rgb(216, 222, 233); // default fg
    const HJ_BLUE: Color = Color::Rgb(94, 129, 172); // NORMAL
    const HJ_GREEN: Color = Color::Rgb(163, 190, 140); // INSERT
    const HJ_PURPLE: Color = Color::Rgb(204, 153, 204); // VISUAL*
    const HJ_RED: Color = Color::Rgb(255, 170, 170); // dirty marker
    const HJ_DARK: Color = Color::Rgb(46, 52, 64); // mode-fg (dark on bright)

    let mode = app.mode_label();
    let mode_color = match mode {
        "INSERT" => HJ_GREEN,
        "VISUAL" | "VISUAL LINE" | "VISUAL BLOCK" => HJ_PURPLE,
        _ => HJ_BLUE,
    };
    let mode_style = Style::default()
        .bg(mode_color)
        .fg(HJ_DARK)
        .add_modifier(Modifier::BOLD);
    let mid_style = Style::default().bg(HJ_SURFACE).fg(HJ_TEXT);
    let fill_style = Style::default().bg(HJ_BASE).fg(HJ_TEXT);
    let dirty_style = Style::default().bg(HJ_SURFACE).fg(HJ_RED);

    // Tags & markers
    let ro_tag = if app.editor.is_readonly() {
        " [RO]"
    } else {
        ""
    };
    let new_tag = if app.is_new_file { " [New File]" } else { "" };
    let untracked_tag = if app.is_untracked && !app.is_new_file {
        " [Untracked]"
    } else {
        ""
    };

    let raw_filename: String = app
        .filename
        .as_ref()
        .and_then(|p| p.to_str())
        .unwrap_or("[No Name]")
        .to_owned();

    let (row, col) = app.editor.cursor();
    let line_count = app.editor.buffer().line_count() as usize;
    let pct = ((row + 1) * 100).checked_div(line_count).unwrap_or(0);

    // Section text (each block has 1-space padding both sides).
    let mode_block = format!(" {mode} ");
    let pos_block = format!(" {}:{} ", row + 1, col + 1);
    let pct_block = format!(" {pct}% ");
    let dirty_block = if app.dirty { " ● " } else { "" };
    let suffix = format!("{ro_tag}{new_tag}{untracked_tag}");

    // Filename block — surface bg, with leading + trailing space.
    // Truncate with leading `…` if the line doesn't fit.
    let w = width as usize;
    let reserved = mode_block.len()
        + 2 /* leading + trailing space around filename */
        + suffix.len()
        + dirty_block.len()
        + pos_block.len()
        + pct_block.len();
    let avail_for_name = w.saturating_sub(reserved);
    let filename: String = if raw_filename.len() <= avail_for_name {
        raw_filename.clone()
    } else if avail_for_name <= 1 {
        String::new()
    } else {
        let keep = avail_for_name.saturating_sub(1);
        let start = raw_filename.len().saturating_sub(keep);
        format!("\u{2026}{}", &raw_filename[start..])
    };
    let mid_block = format!(" {filename}{suffix} ");

    // Spacer fills the gap between mid and the right-side blocks.
    let used =
        mode_block.len() + mid_block.len() + dirty_block.len() + pos_block.len() + pct_block.len();
    let spacer: String = " ".repeat(w.saturating_sub(used));

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(6);
    spans.push(Span::styled(mode_block, mode_style));
    spans.push(Span::styled(mid_block, mid_style));
    if !dirty_block.is_empty() {
        spans.push(Span::styled(dirty_block.to_string(), dirty_style));
    }
    spans.push(Span::styled(spacer, fill_style));
    spans.push(Span::styled(pos_block, mid_style));
    spans.push(Span::styled(pct_block, mode_style));

    (Line::from(spans), None)
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
