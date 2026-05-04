//! Per-frame render functions.
//!
//! [`frame`] is the top-level entry point called from the event loop.
//! It splits the terminal area into a buffer pane + status line row and
//! delegates to [`buffer_pane`] and [`status_line`].

use hjkl_buffer::{BufferView, Gutter};
use hjkl_engine::{Host, Query};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
};

use crate::app::{App, BUFFER_LINE_HEIGHT, DiskState, STATUS_LINE_HEIGHT};

/// Gutter width formula — matches `Editor::cursor_screen_pos`'s
/// `lnum_width = line_count.to_string().len() + 2`. The renderer must
/// agree with the engine or terminal cursor lands off by one column.
fn gutter_width(line_count: usize) -> u16 {
    line_count.to_string().len() as u16 + 2
}

/// Bg painted across the cursor row in both the editor pane and the
/// picker preview pane. Subtle blue-grey — visible enough to track the
/// cursor at a glance without competing with the syntax foreground.
fn cursor_line_bg(theme: &crate::theme::UiTheme) -> Style {
    Style::default().bg(theme.cursor_line_bg)
}

/// Render one complete frame into `frame`.
pub fn frame(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    let multi = app.slots().len() > 1;
    let (buf_area, status_area, bufline_area) = if multi {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(BUFFER_LINE_HEIGHT),
                Constraint::Min(1),
                Constraint::Length(STATUS_LINE_HEIGHT),
            ])
            .split(area);
        (chunks[1], chunks[2], Some(chunks[0]))
    } else {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(STATUS_LINE_HEIGHT)])
            .split(area);
        (chunks[0], chunks[1], None)
    };

    // Splash screen path — skip all buffer rendering while active.
    if let Some(ref screen) = app.start_screen {
        if let Some(bl_area) = bufline_area {
            buffer_line(frame, app, bl_area);
        }
        crate::start_screen::render(frame, buf_area, screen, &app.theme);
        status_line(frame, app, status_area);
        return;
    }

    let gw = gutter_width(app.active().editor.buffer().line_count() as usize);
    let text_width = buf_area.width.saturating_sub(gw);

    // Publish viewport dims so engine scrolloff math is accurate.
    // `width` is the text-area width (gutter excluded) — `Viewport::ensure_visible`
    // uses it as the horizontal cursor band, and the cursor lives in the text area.
    {
        let tabstop = app.active().editor.settings().tabstop as u16;
        let vp = app.active_mut().editor.host_mut().viewport_mut();
        vp.width = text_width;
        vp.height = buf_area.height;
        vp.text_width = text_width;
        vp.tab_width = tabstop;
    }
    // Publish height to the engine's atomic so scrolloff (5-row margin) engages.
    app.active_mut().editor.set_viewport_height(buf_area.height);

    // Refresh syntax spans against the now-current viewport. On the first
    // frame, App::new ran the initial parse with `viewport.height = 0`
    // (the atomic's init value) so only row 0 had spans installed. With
    // the source/tree cache + parse-skip on unchanged buffers, this call
    // is ~140µs even on 100k-line files.
    app.recompute_and_install();

    if let Some(bl_area) = bufline_area {
        buffer_line(frame, app, bl_area);
    }
    buffer_pane(frame, app, buf_area, gw);
    status_line(frame, app, status_area);

    // Picker overlay sits on top of the buffer pane. Renders last so
    // its `Clear` widget masks the editor content beneath it.
    if app.picker.is_some() {
        picker_overlay(frame, app, buf_area);
    }

    // Info popup (`:reg`, `:marks`, `:jumps`, `:changes`) renders on top of
    // the picker overlay so it always shows.
    if app.info_popup.is_some() {
        info_popup_overlay(frame, app, buf_area);
    }
}

/// Render the one-row buffer/tab line at the top of the screen.
///
/// Shows all open slots with the active one highlighted and a `+` marker
/// on dirty slots. Only called when `app.slots.len() > 1`.
fn buffer_line(frame: &mut Frame, app: &App, area: Rect) {
    let ui = &app.theme.ui;
    let active_style = Style::default()
        .fg(ui.on_accent)
        .bg(ui.mode_normal_bg)
        .add_modifier(Modifier::BOLD);
    let inactive_style = Style::default().fg(ui.text_dim);
    let sep_style = Style::default().fg(ui.border);

    let mut spans: Vec<Span<'static>> = Vec::new();
    let max_width = area.width as usize;
    let mut used = 0usize;

    for (i, slot) in app.slots().iter().enumerate() {
        let base_name = slot
            .filename
            .as_ref()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("[No Name]");
        let label = if slot.dirty {
            format!(" {}+ ", base_name)
        } else {
            format!(" {} ", base_name)
        };

        // Separator between entries (not before the first).
        let sep = if i == 0 { "" } else { "│" };
        let entry_width = sep.len() + label.len();

        // If adding this entry would overflow, truncate remaining with `…`.
        if used + entry_width > max_width {
            // Always include the active slot even if it means skipping earlier
            // entries — but for simplicity, just hard-truncate the end.
            if used < max_width {
                spans.push(Span::styled("…".to_string(), sep_style));
            }
            break;
        }

        if i > 0 {
            spans.push(Span::styled("│".to_string(), sep_style));
        }
        let style = if i == app.active_index() {
            active_style
        } else {
            inactive_style
        };
        spans.push(Span::styled(label, style));
        used += entry_width;
    }

    let paragraph = Paragraph::new(Line::from(spans));
    frame.render_widget(paragraph, area);
}

/// Render the buffer pane with line numbers, text, and the cursor.
///
/// The buffer-pane cursor is suppressed when the user is typing in the
/// command line (`:` prompt or `/`/`?` search prompt), because the
/// terminal cursor belongs to the bottom row in those states.
fn buffer_pane(frame: &mut Frame, app: &mut App, area: ratatui::layout::Rect, gutter_width: u16) {
    let gutter = Gutter {
        width: gutter_width,
        style: Style::default().fg(app.theme.ui.gutter),
        line_offset: 0,
    };

    let selection = app.active().editor.buffer_selection();
    let buffer_spans = app.active().editor.buffer_spans();
    let search_pattern = app.active().editor.search_state().pattern.as_ref();
    let in_prompt =
        app.command_field.is_some() || app.search_field.is_some() || app.picker.is_some();

    // Merge diagnostic + git signs, filtered to the visible viewport so
    // BufferView's per-row linear scan stays cheap on large files.
    let vp_top = app.active().editor.host().viewport().top_row;
    let vp_bot = vp_top + area.height as usize;
    let mut visible_signs: Vec<hjkl_buffer::Sign> = app
        .active()
        .diag_signs
        .iter()
        .copied()
        .filter(|s| s.row >= vp_top && s.row < vp_bot)
        .chain(
            app.active()
                .git_signs
                .iter()
                .copied()
                .filter(|s| s.row >= vp_top && s.row < vp_bot),
        )
        .collect();
    // Stable sort by row keeps BufferView's max_by_key dedupe deterministic.
    visible_signs.sort_by_key(|s| s.row);

    // Search match highlight — uses theme's --orange + on-bright fg.
    let search_bg = if search_pattern.is_some() {
        Style::default()
            .bg(app.theme.ui.search_bg)
            .fg(app.theme.ui.search_fg)
    } else {
        Style::default()
    };

    // Bind the style table after the viewport mutation above to avoid a
    // double-borrow on `app.active().editor` (host_mut() and style_table() both
    // require access to the editor).
    let style_table = app.active().editor.style_table().to_owned();
    let resolver = move |id: u32| style_table.get(id as usize).copied().unwrap_or_default();

    let view = BufferView {
        buffer: app.active().editor.buffer(),
        viewport: app.active().editor.host().viewport(),
        selection,
        resolver: &resolver,
        cursor_line_bg: if in_prompt {
            Style::default()
        } else {
            cursor_line_bg(&app.theme.ui)
        },
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
    if !in_prompt && let Some((cx, cy)) = app.active_mut().editor.cursor_screen_pos_in_rect(area) {
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
fn prompt_line(
    content: &str,
    mode: hjkl_form::VimMode,
    theme: &crate::theme::UiTheme,
    width: u16,
) -> Line<'static> {
    // Insert vs Normal use different bgs sourced from the app theme so the
    // mode is visible without relying on cursor shape.
    let (bg, tag, tag_fg) = match mode {
        hjkl_form::VimMode::Insert => (theme.form_insert_bg, " [I]", theme.form_tag_insert_fg),
        _ => (theme.form_normal_bg, " [N]", theme.form_tag_normal_fg),
    };
    let body_width = (width as usize).saturating_sub(tag.len());
    let visible: String = content.chars().take(body_width).collect();
    let body = format!("{visible:<body_width$}");
    Line::from(vec![
        Span::styled(body, Style::default().bg(bg).fg(theme.text)),
        Span::styled(tag, Style::default().bg(bg).fg(tag_fg)),
    ])
}

/// Count search matches in the buffer and return `(current_idx, total)`.
/// `current_idx` is 1-based (the match the cursor is on or just passed).
/// Returns `None` when no pattern is active or there are no matches.
/// Caps at 10 000 matches to avoid stalling on huge files.
fn search_count(app: &App) -> Option<(usize, usize)> {
    const MATCH_CAP: usize = 10_000;
    let st = app.active().editor.search_state();
    let pat = st.pattern.as_ref()?;
    let buf = app.active().editor.buffer();
    let (cursor_row, cursor_col) = app.active().editor.cursor();
    let mut total = 0usize;
    let mut current_idx = 0usize;
    'outer: for (row_idx, line) in buf.lines().iter().enumerate() {
        for m in pat.find_iter(line) {
            total += 1;
            if (row_idx, m.start()) <= (cursor_row, cursor_col) {
                current_idx = total;
            }
            if total >= MATCH_CAP {
                break 'outer;
            }
        }
    }
    if total == 0 {
        None
    } else {
        Some((current_idx, total))
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
    if let Some(ref field) = app.command_field {
        let text = field.text();
        let display: String = text.lines().next().unwrap_or("").to_string();
        let content = format!(":{display}");
        let (_, ccol) = field.cursor();
        let cursor_col = 1u16 + ccol as u16;
        return (
            prompt_line(&content, field.vim_mode(), &app.theme.ui, width),
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
            prompt_line(&content, field.vim_mode(), &app.theme.ui, width),
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
                Style::default()
                    .bg(app.theme.ui.surface_bg)
                    .fg(app.theme.ui.text),
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
                Style::default()
                    .bg(app.theme.ui.surface_bg)
                    .fg(app.theme.ui.text),
            )]),
            None,
        );
    }

    // ── Normal status line (lualine-style colored sections) ─────────────────
    // Palette pulled from app theme (themes/ui-dark.toml). Mode colors map
    // to website --blue / --green / --accent so the status bar visually
    // matches both the syntax theme and the project's web identity.
    let ui = &app.theme.ui;
    let mode = app.mode_label();
    let mode_color = match mode {
        "INSERT" => ui.mode_insert_bg,
        "VISUAL" | "VISUAL LINE" | "VISUAL BLOCK" => ui.mode_visual_bg,
        _ => ui.mode_normal_bg,
    };
    let mode_style = Style::default()
        .bg(mode_color)
        .fg(ui.on_accent)
        .add_modifier(Modifier::BOLD);
    let mid_style = Style::default().bg(ui.surface_bg).fg(ui.text);
    let fill_style = Style::default().bg(ui.panel_bg).fg(ui.text);
    let dirty_style = Style::default()
        .bg(ui.surface_bg)
        .fg(ui.status_dirty_marker);

    // Tags & markers
    let ro_tag = if app.active().editor.is_readonly() {
        " [RO]"
    } else {
        ""
    };
    let new_tag = if app.active().is_new_file {
        " [New File]"
    } else {
        ""
    };
    let disk_tag = match app.active().disk_state {
        DiskState::DeletedOnDisk => " [deleted]",
        DiskState::ChangedOnDisk => " [changed on disk]",
        DiskState::Synced => "",
    };
    let untracked_tag = if app.active().is_untracked && !app.active().is_new_file {
        " [Untracked]"
    } else {
        ""
    };

    let raw_filename: String = app
        .active()
        .filename
        .as_ref()
        .and_then(|p| p.to_str())
        .unwrap_or("[No Name]")
        .to_owned();

    let (row, col) = app.active().editor.cursor();
    let line_count = app.active().editor.buffer().line_count() as usize;
    let pct = ((row + 1) * 100).checked_div(line_count).unwrap_or(0);

    // Section text (each block has 1-space padding both sides).
    let mode_block = format!(" {mode} ");
    let pos_block = format!(" {}:{} ", row + 1, col + 1);
    let pct_block = format!(" {pct}% ");
    let dirty_block = if app.active().dirty { " ● " } else { "" };
    let rec_block = match app.active().editor.recording_register() {
        Some(reg) => format!(" REC @{reg} "),
        None => String::new(),
    };
    // Pending count + operator block (vim "showcmd").
    let pending_block: String = {
        let pc = app.active().editor.pending_count();
        let po = app.active().editor.pending_op();
        match (pc, po) {
            (Some(n), Some(op)) => format!(" {n}{op} "),
            (Some(n), None) => format!(" {n} "),
            (None, Some(op)) => format!(" {op} "),
            (None, None) => String::new(),
        }
    };
    // Search count block `[idx/total]`.
    let search_count_block: String = search_count(app)
        .map(|(idx, total)| format!(" [{idx}/{total}] "))
        .unwrap_or_default();
    let suffix = format!("{ro_tag}{new_tag}{disk_tag}{untracked_tag}");

    // Filename block — surface bg, with leading + trailing space.
    // Truncate with leading `…` if the line doesn't fit.
    let w = width as usize;
    let reserved = mode_block.len()
        + rec_block.len()
        + pending_block.len()
        + 2 /* leading + trailing space around filename */
        + suffix.len()
        + dirty_block.len()
        + search_count_block.len()
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
    let used = mode_block.len()
        + rec_block.len()
        + pending_block.len()
        + mid_block.len()
        + dirty_block.len()
        + search_count_block.len()
        + pos_block.len()
        + pct_block.len();
    let spacer: String = " ".repeat(w.saturating_sub(used));

    let rec_style = Style::default()
        .bg(ui.recording_bg)
        .fg(ui.recording_fg)
        .add_modifier(Modifier::BOLD);
    let pending_style = Style::default()
        .bg(ui.surface_bg)
        .fg(ui.text)
        .add_modifier(Modifier::ITALIC);

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(9);
    spans.push(Span::styled(mode_block, mode_style));
    if !rec_block.is_empty() {
        spans.push(Span::styled(rec_block, rec_style));
    }
    if !pending_block.is_empty() {
        spans.push(Span::styled(pending_block, pending_style));
    }
    spans.push(Span::styled(mid_block, mid_style));
    if !dirty_block.is_empty() {
        spans.push(Span::styled(dirty_block.to_string(), dirty_style));
    }
    if !search_count_block.is_empty() {
        spans.push(Span::styled(search_count_block, mid_style));
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

/// Centered popup containing the picker's query input + scrollable
/// result list. Drawn on top of the buffer pane via `Clear` so the
/// editor content beneath is masked.
fn picker_overlay(frame: &mut Frame, app: &mut App, buf_area: Rect) {
    let area = centered_rect(80, 70, buf_area);
    frame.render_widget(Clear, area);

    let p = match app.picker.as_mut() {
        Some(p) => p,
        None => return,
    };

    // Tick debounce for Spawn sources.
    p.tick(std::time::Instant::now());

    p.refresh();
    p.refresh_preview();

    const PREVIEW_MIN_WIDTH: u16 = 80;
    let with_preview = p.has_preview() && area.width >= PREVIEW_MIN_WIDTH;

    let (left_area, preview_area) = if with_preview {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(area);
        (cols[0], Some(cols[1]))
    } else {
        (area, None)
    };

    render_picker_input_and_list(frame, p, &app.theme.ui, left_area);

    if let Some(right) = preview_area {
        picker_preview_pane(frame, p, &app.theme.ui, right);
    }
}

/// Shared input + list rendering for the non-generic `Picker`.
fn render_picker_input_and_list(
    frame: &mut Frame,
    picker: &mut crate::picker::Picker,
    theme: &crate::theme::UiTheme,
    left_area: Rect,
) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(left_area);
    let input_area = layout[0];
    let list_area = layout[1];

    let total = picker.total();
    let matched = picker.matched();
    let scan_tag = if picker.scan_done() {
        "".to_string()
    } else {
        format!(" {} scanning", hjkl_ratatui::spinner::frame())
    };
    let kind = picker.title();
    let title = format!(" picker — {kind} — {matched}/{total}{scan_tag} ");

    let query_text = picker.query.text();
    let display: String = query_text.lines().next().unwrap_or("").to_string();
    let input_block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(theme.border_active));
    let input_inner = input_block.inner(input_area);
    frame.render_widget(input_block, input_area);
    let input_para = Paragraph::new(format!("/ {display}"));
    frame.render_widget(input_para, input_inner);

    let (_, ccol) = picker.query.cursor();
    let cx = input_inner.x + 2 + ccol as u16;
    let cy = input_inner.y;
    if cx < input_inner.x + input_inner.width && cy < input_inner.y + input_inner.height {
        frame.set_cursor_position((cx, cy));
    }

    let entries = picker.visible_entries();
    let row_styles = picker.visible_entry_styles();
    // Match-position highlight inside picker rows — uses the same orange
    // as search match highlighting.
    let match_style = Style::default()
        .fg(theme.search_bg)
        .add_modifier(Modifier::BOLD);
    let items: Vec<ListItem> = entries
        .iter()
        .enumerate()
        .map(|(row_idx, (label, matches))| {
            let styles = row_styles.get(row_idx).map(Vec::as_slice).unwrap_or(&[]);
            if matches.is_empty() && styles.is_empty() {
                return ListItem::new(label.clone());
            }
            let spans: Vec<Span> = label
                .chars()
                .enumerate()
                .map(|(ci, ch)| {
                    let s = ch.to_string();
                    let base = styles
                        .iter()
                        .find(|(r, _)| r.contains(&ci))
                        .map(|(_, st)| *st)
                        .unwrap_or_default();
                    if matches.contains(&ci) {
                        Span::styled(s, base.patch(match_style))
                    } else if base != Style::default() {
                        Span::styled(s, base)
                    } else {
                        Span::raw(s)
                    }
                })
                .collect();
            ListItem::new(Line::from(spans))
        })
        .collect();
    // Keep labels for length check below.
    let label_count = entries.len();
    let list_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border));
    let list = List::new(items).block(list_block).highlight_style(
        Style::default()
            .bg(theme.picker_selection_bg)
            .add_modifier(Modifier::BOLD),
    );
    let mut state = ListState::default();
    if label_count > 0 {
        state.select(Some(picker.selected.min(label_count.saturating_sub(1))));
    }
    frame.render_stateful_widget(list, list_area, &mut state);
}

/// Render the preview pane via `BufferView` so the gutter, line
/// numbers, and per-row layout match the editor proper.
fn picker_preview_pane(
    frame: &mut Frame,
    picker: &crate::picker::Picker,
    theme: &crate::theme::UiTheme,
    area: Rect,
) {
    let label = picker.preview_label().unwrap_or("(none)").to_string();
    let status = picker.preview_status();
    let title = if status.is_empty() {
        format!(" preview — {label} ")
    } else {
        format!(" preview — {label} [{status}] ")
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if !status.is_empty() {
        // Skipped (binary / oversized / I/O error) — show the status
        // tag in the body too for visibility on narrow terminals where
        // the title might be clipped.
        let para = Paragraph::new(format!("  ({status})"));
        frame.render_widget(para, inner);
        return;
    }

    let buf = picker.preview_buffer();
    let line_count = buf.line_count() as usize;
    let gw = gutter_width(line_count.max(1));
    let viewport = hjkl_buffer::Viewport {
        top_row: picker.preview_top_row(),
        top_col: 0,
        width: inner.width.saturating_sub(gw),
        height: inner.height,
        text_width: inner.width.saturating_sub(gw),
        ..hjkl_buffer::Viewport::default()
    };
    let preview_spans = picker.preview_spans();
    let resolver = |id: u32| {
        preview_spans
            .styles
            .get(id as usize)
            .copied()
            .unwrap_or_default()
    };
    let cursor_line_bg = if picker.preview_match_row().is_some() {
        cursor_line_bg(theme)
    } else {
        Style::default()
    };
    let view = BufferView {
        buffer: buf,
        viewport: &viewport,
        selection: None,
        resolver: &resolver,
        cursor_line_bg,
        cursor_column_bg: Style::default(),
        selection_bg: Style::default(),
        cursor_style: Style::default(),
        gutter: Some(Gutter {
            width: gw,
            style: Style::default().fg(theme.gutter),
            line_offset: picker.preview_line_offset(),
        }),
        search_bg: Style::default(),
        signs: &[],
        conceals: &[],
        spans: &preview_spans.by_row,
        search_pattern: None,
    };
    frame.render_widget(view, inner);
}

/// Centered popup for multi-line `:reg` / `:marks` / `:jumps` / `:changes`
/// output. Rendered on top of the buffer pane; any key dismisses it.
fn info_popup_overlay(frame: &mut Frame, app: &App, buf_area: Rect) {
    let text = match app.info_popup.as_ref() {
        Some(t) => t,
        None => return,
    };
    let area = centered_rect(80, 60, buf_area);
    frame.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.ui.border_active))
        .title(" info ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let para = Paragraph::new(text.clone());
    frame.render_widget(para, inner);
}

/// Compute a centered Rect of `pct_x`% × `pct_y`% of `area`.
fn centered_rect(pct_x: u16, pct_y: u16, area: Rect) -> Rect {
    let width = area.width.saturating_mul(pct_x) / 100;
    let height = area.height.saturating_mul(pct_y) / 100;
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect {
        x,
        y,
        width,
        height,
    }
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

    #[test]
    fn buffer_line_height_is_one() {
        assert_eq!(crate::app::BUFFER_LINE_HEIGHT, 1);
    }
}
