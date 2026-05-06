//! Per-frame render functions.
//!
//! [`frame`] is the top-level entry point called from the event loop.
//! It splits the terminal area into a buffer pane + status line row and
//! delegates to [`buffer_pane`] and [`status_line`].

use hjkl_buffer::{BufferView, DiagOverlay, Gutter, GutterNumbers, Viewport};
use hjkl_engine::{Host, Query};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
};

use crate::app::{
    App, BUFFER_LINE_HEIGHT, DiagSeverity, DiskState, STATUS_LINE_HEIGHT, TAB_BAR_HEIGHT, window,
};

/// Build the style for a diagnostic severity used in overlays and the status line.
fn diag_severity_style(sev: DiagSeverity) -> Style {
    match sev {
        DiagSeverity::Error => Style::default()
            .fg(Color::Red)
            .add_modifier(Modifier::UNDERLINED),
        DiagSeverity::Warning => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::UNDERLINED),
        DiagSeverity::Info => Style::default()
            .fg(Color::Blue)
            .add_modifier(Modifier::UNDERLINED),
        DiagSeverity::Hint => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::UNDERLINED),
    }
}

/// Build `DiagOverlay` items for the active buffer slot, filtered to the
/// visible viewport. Called once per frame in `render_window`.
fn build_diag_overlays(
    slot: &crate::app::BufferSlot,
    _ui: &crate::theme::UiTheme,
) -> Vec<DiagOverlay> {
    slot.lsp_diags
        .iter()
        .map(|d| DiagOverlay {
            row: d.start_row,
            col_start: d.start_col,
            col_end: if d.end_row == d.start_row && d.end_col > d.start_col {
                d.end_col
            } else {
                d.start_col + 1
            },
            style: diag_severity_style(d.severity),
        })
        .collect()
}

/// Gutter width formula — matches `Editor::cursor_screen_pos`'s
/// `lnum_width = max(numberwidth, line_count.to_string().len() + 1)`.
/// The renderer must agree with the engine or terminal cursor lands off by
/// one column. Returns 0 when both `number` and `relativenumber` are false.
fn gutter_width(line_count: usize, number: bool, relativenumber: bool, numberwidth: usize) -> u16 {
    if !number && !relativenumber {
        return 0;
    }
    let needed = line_count.to_string().len() + 1; // digits + 1 trailing spacer
    needed.max(numberwidth) as u16
}

/// Bg painted across the cursor row in both the editor pane and the
/// picker preview pane. Subtle blue-grey — visible enough to track the
/// cursor at a glance without competing with the syntax foreground.
fn cursor_line_bg(theme: &crate::theme::UiTheme) -> Style {
    Style::default().bg(theme.cursor_line_bg)
}

/// Split a `Rect` into two parts according to `dir` and `ratio`.
fn split_rect(area: Rect, dir: window::SplitDir, ratio: f32) -> (Rect, Rect) {
    match dir {
        window::SplitDir::Horizontal => {
            let a_h = ((area.height as f32) * ratio).round() as u16;
            let a_h = a_h.clamp(1, area.height.saturating_sub(1).max(1));
            let b_h = area.height.saturating_sub(a_h);
            let rect_a = Rect {
                x: area.x,
                y: area.y,
                width: area.width,
                height: a_h,
            };
            let rect_b = Rect {
                x: area.x,
                y: area.y + a_h,
                width: area.width,
                height: b_h,
            };
            (rect_a, rect_b)
        }
        window::SplitDir::Vertical => {
            let a_w = ((area.width as f32) * ratio).round() as u16;
            let a_w = a_w.clamp(1, area.width.saturating_sub(1).max(1));
            let b_w = area.width.saturating_sub(a_w);
            let rect_a = Rect {
                x: area.x,
                y: area.y,
                width: a_w,
                height: area.height,
            };
            let rect_b = Rect {
                x: area.x + a_w,
                y: area.y,
                width: b_w,
                height: area.height,
            };
            (rect_a, rect_b)
        }
    }
}

/// Walk the layout tree and render each leaf window into its allocated rect.
/// Takes `&mut LayoutTree` so that Split nodes can record their `last_rect`
/// for use by resize commands in later phases.
fn render_layout(frame: &mut Frame, app: &mut App, area: Rect, layout: &mut window::LayoutTree) {
    match layout {
        window::LayoutTree::Leaf(id) => render_window(frame, app, area, *id),
        window::LayoutTree::Split {
            dir,
            ratio,
            a,
            b,
            last_rect,
        } => {
            // Record the rect this split occupied so resize commands can
            // convert line/column deltas to ratio updates.
            *last_rect = Some(area);
            let (rect_a, rect_b) = split_rect(area, *dir, *ratio);
            render_layout(frame, app, rect_a, a);
            render_layout(frame, app, rect_b, b);
        }
    }
}

/// Render a single window occupying `area`.
fn render_window(frame: &mut Frame, app: &mut App, area: Rect, win_id: window::WindowId) {
    // Record the rendered rect for Phase 2+ direction navigation.
    if let Some(win) = app.windows[win_id].as_mut() {
        win.last_rect = Some(area);
    }

    // Extract window metadata (then drop the borrow so we can access slots).
    let (slot_idx, top_row, top_col, is_focused) = {
        let win = match app.windows[win_id].as_ref() {
            Some(w) => w,
            None => return, // closed window — skip
        };
        (
            win.slot,
            win.top_row,
            win.top_col,
            win_id == app.focused_window(),
        )
    };

    let s = app.slots()[slot_idx].editor.settings();
    let (nu, rnu, nuw) = (s.number, s.relativenumber, s.numberwidth);
    let gw = gutter_width(
        app.slots()[slot_idx].editor.buffer().line_count() as usize,
        nu,
        rnu,
        nuw,
    );
    let text_width = area.width.saturating_sub(gw);

    // For the focused window: publish viewport dims into the engine so
    // scrolloff math and cursor-screen-pos work correctly.
    if is_focused {
        let tabstop = app.slots()[slot_idx].editor.settings().tabstop as u16;
        let vp = app.slots_mut()[slot_idx].editor.host_mut().viewport_mut();
        vp.width = text_width;
        vp.height = area.height;
        vp.text_width = text_width;
        vp.tab_width = tabstop;
        app.slots_mut()[slot_idx]
            .editor
            .set_viewport_height(area.height);
    }

    let cursor_row = app.slots()[slot_idx].editor.buffer().cursor().row;
    let numbers = match (nu, rnu) {
        (false, false) => GutterNumbers::None,
        (true, false) => GutterNumbers::Absolute,
        (false, true) => GutterNumbers::Relative { cursor_row },
        (true, true) => GutterNumbers::Hybrid { cursor_row },
    };
    let gutter = if gw > 0 {
        Some(Gutter {
            width: gw,
            style: Style::default().fg(app.theme.ui.gutter),
            line_offset: 0,
            numbers,
        })
    } else {
        None
    };

    // Viewport for this window: focused uses editor's live viewport (with
    // auto-scroll applied); non-focused builds one from the window's own
    // stored scroll position so it doesn't chase the focused editor.
    let viewport_owned: Viewport;
    let viewport_ref: &Viewport = if is_focused {
        app.slots()[slot_idx].editor.host().viewport()
    } else {
        viewport_owned = Viewport {
            top_row,
            top_col,
            width: text_width,
            height: area.height,
            text_width,
            ..Viewport::default()
        };
        &viewport_owned
    };

    let in_prompt =
        app.command_field.is_some() || app.search_field.is_some() || app.picker.is_some();

    // Merge diagnostic + LSP diag + git signs, filtered to the visible viewport.
    let vp_top = viewport_ref.top_row;
    let vp_bot = vp_top + area.height as usize;
    let mut visible_signs: Vec<hjkl_buffer::Sign> = app.slots()[slot_idx]
        .diag_signs
        .iter()
        .copied()
        .filter(|s| s.row >= vp_top && s.row < vp_bot)
        .chain(
            app.slots()[slot_idx]
                .diag_signs_lsp
                .iter()
                .copied()
                .filter(|s| s.row >= vp_top && s.row < vp_bot),
        )
        .chain(
            app.slots()[slot_idx]
                .git_signs
                .iter()
                .copied()
                .filter(|s| s.row >= vp_top && s.row < vp_bot),
        )
        .collect();
    visible_signs.sort_by_key(|s| s.row);

    let selection = app.slots()[slot_idx].editor.buffer_selection();
    let buffer_spans = app.slots()[slot_idx].editor.buffer_spans();
    let search_pattern = app.slots()[slot_idx].editor.search_state().pattern.as_ref();

    let search_bg = if search_pattern.is_some() {
        Style::default()
            .bg(app.theme.ui.search_bg)
            .fg(app.theme.ui.search_fg)
    } else {
        Style::default()
    };

    let style_table = app.slots()[slot_idx].editor.style_table().to_owned();
    let resolver = move |id: u32| style_table.get(id as usize).copied().unwrap_or_default();

    // For non-focused windows, don't show cursor highlight or cursor position.
    let show_cursor = is_focused && !in_prompt;

    let diag_overlays = build_diag_overlays(&app.slots()[slot_idx], &app.theme.ui);
    let view = BufferView {
        buffer: app.slots()[slot_idx].editor.buffer(),
        viewport: viewport_ref,
        selection,
        resolver: &resolver,
        cursor_line_bg: if show_cursor {
            cursor_line_bg(&app.theme.ui)
        } else {
            Style::default()
        },
        cursor_column_bg: Style::default(),
        selection_bg: Style::default().bg(Color::Blue),
        cursor_style: Style::default(),
        gutter,
        search_bg,
        signs: &visible_signs,
        conceals: &[],
        spans: buffer_spans,
        search_pattern,
        non_text_style: Style::default().fg(app.theme.ui.non_text),
        diag_overlays: &diag_overlays,
    };
    frame.render_widget(view, area);

    // Emit the terminal cursor only for the focused window.
    if show_cursor
        && let Some((cx, cy)) = app.slots_mut()[slot_idx]
            .editor
            .cursor_screen_pos_in_rect(area)
    {
        frame.set_cursor_position((cx, cy));
    }
}

/// Render the completion popup, floating below the cursor position.
///
/// Only called when `app.completion.is_some()`.
fn completion_popup(frame: &mut Frame, app: &App, buf_area: Rect) {
    let popup = match app.completion.as_ref() {
        Some(p) => p,
        None => return,
    };
    if popup.is_empty() {
        return;
    }

    // The anchor position in buffer coordinates (0-based row/col).
    // We need to convert to screen coordinates inside buf_area.
    // The focused window may have a gutter — compute it.
    let slot_idx = {
        let fw = app.focused_window();
        app.windows[fw].as_ref().map(|w| w.slot).unwrap_or(0)
    };
    let s = app.slots()[slot_idx].editor.settings();
    let (nu, rnu, nuw) = (s.number, s.relativenumber, s.numberwidth);
    let gw = gutter_width(
        app.slots()[slot_idx].editor.buffer().line_count() as usize,
        nu,
        rnu,
        nuw,
    );

    let vp = app.slots()[slot_idx].editor.host().viewport();
    let vp_top = vp.top_row;

    // Screen row: anchor_row relative to viewport top, +1 so popup appears below.
    let screen_row = popup.anchor_row.saturating_sub(vp_top) as u16 + 1;
    // Screen col: anchor_col + gutter width.
    let screen_col = popup.anchor_col as u16 + gw;

    // Compute popup dimensions.
    const MIN_WIDTH: u16 = 20;
    const MAX_HEIGHT: u16 = 10;

    let visible_count = popup.visible.len().min(MAX_HEIGHT as usize) as u16;
    if visible_count == 0 {
        return;
    }

    // Determine width from longest label + detail.
    let content_width = popup
        .visible
        .iter()
        .filter_map(|&idx| popup.all_items.get(idx))
        .map(|item| {
            let detail_len = item.detail.as_deref().map(|d| d.len() + 2).unwrap_or(0);
            // icon(1) + space(1) + label + space(2) + detail
            1 + 1 + item.label.len() + 2 + detail_len
        })
        .max()
        .unwrap_or(MIN_WIDTH as usize) as u16;
    let popup_w = content_width.max(MIN_WIDTH).min(buf_area.width);

    // Anchor position in screen coordinates (relative to buf_area).
    let abs_row = buf_area.y + screen_row;
    let abs_col = buf_area.x + screen_col;

    // Clamp to screen bounds.
    let popup_h = visible_count + 2; // +2 for border
    let popup_y = if abs_row + popup_h > buf_area.y + buf_area.height {
        // Would extend past bottom — shift above cursor.
        abs_row.saturating_sub(popup_h + 1).max(buf_area.y)
    } else {
        abs_row
    };
    let popup_x = abs_col.min(buf_area.x + buf_area.width.saturating_sub(popup_w));

    let area = Rect {
        x: popup_x,
        y: popup_y,
        width: popup_w,
        height: popup_h,
    };

    frame.render_widget(Clear, area);

    let ui = &app.theme.ui;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ui.border_active));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let selected_style = Style::default()
        .bg(ui.picker_selection_bg)
        .add_modifier(Modifier::BOLD);
    let normal_style = Style::default().fg(ui.text);
    let detail_style = Style::default().fg(ui.text_dim);

    let items: Vec<ListItem> = popup
        .visible
        .iter()
        .enumerate()
        .filter_map(|(vis_idx, &item_idx)| {
            let item = popup.all_items.get(item_idx)?;
            let icon = item.kind.icon();
            let label = &item.label;
            let mut spans = vec![
                Span::styled(
                    format!("{icon} "),
                    if vis_idx == popup.selected {
                        selected_style
                    } else {
                        normal_style
                    },
                ),
                Span::styled(
                    label.clone(),
                    if vis_idx == popup.selected {
                        selected_style
                    } else {
                        normal_style
                    },
                ),
            ];
            if let Some(ref detail) = item.detail {
                // Truncate detail to avoid overflow.
                let avail = inner.width.saturating_sub(2 + label.len() as u16 + 2) as usize;
                let truncated: String = detail.chars().take(avail).collect();
                if !truncated.is_empty() {
                    spans.push(Span::styled(
                        format!("  {truncated}"),
                        if vis_idx == popup.selected {
                            selected_style
                        } else {
                            detail_style
                        },
                    ));
                }
            }
            Some(ListItem::new(Line::from(spans)))
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(popup.selected.min(items.len().saturating_sub(1))));
    let list = List::new(items).highlight_style(selected_style);
    frame.render_stateful_widget(list, inner, &mut state);
}

/// Render one complete frame into `frame`.
pub fn frame(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    let multi = app.slots().len() > 1;
    let show_tab_bar = app.tabs.len() > 1;
    let (buf_area, status_area, bufline_area, tabbar_area) = {
        // Build constraint list dynamically based on what rows are visible.
        let mut constraints = Vec::new();
        if show_tab_bar {
            constraints.push(Constraint::Length(TAB_BAR_HEIGHT));
        }
        if multi {
            constraints.push(Constraint::Length(BUFFER_LINE_HEIGHT));
        }
        constraints.push(Constraint::Min(1));
        constraints.push(Constraint::Length(STATUS_LINE_HEIGHT));

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area);

        let mut idx = 0usize;
        let tab_area = if show_tab_bar {
            let a = Some(chunks[idx]);
            idx += 1;
            a
        } else {
            None
        };
        let bl_area = if multi {
            let a = Some(chunks[idx]);
            idx += 1;
            a
        } else {
            None
        };
        let buf = chunks[idx];
        idx += 1;
        let stat = chunks[idx];
        (buf, stat, bl_area, tab_area)
    };

    // Splash screen path — skip all buffer rendering while active.
    if let Some(ref screen) = app.start_screen {
        if let Some(ta) = tabbar_area {
            tab_bar(frame, app, ta);
        }
        if let Some(bl_area) = bufline_area {
            buffer_line(frame, app, bl_area);
        }
        crate::start_screen::render(frame, buf_area, screen, &app.theme);
        status_line(frame, app, status_area);
        return;
    }

    // Refresh syntax spans against the now-current viewport. On the first
    // frame, App::new ran the initial parse with `viewport.height = 0`
    // (the atomic's init value) so only row 0 had spans installed. With
    // the source/tree cache + parse-skip on unchanged buffers, this call
    // is ~140µs even on 100k-line files.
    app.recompute_and_install();

    if let Some(ta) = tabbar_area {
        tab_bar(frame, app, ta);
    }

    if let Some(bl_area) = bufline_area {
        buffer_line(frame, app, bl_area);
    }

    // Walk the window tree and render each pane. Use take_layout /
    // restore_layout so we can pass `&mut LayoutTree` to render_layout
    // (which writes last_rect on Split nodes) while also holding
    // `&mut App` for render_window.
    let mut layout = app.take_layout();
    render_layout(frame, app, buf_area, &mut layout);
    app.restore_layout(layout);

    status_line(frame, app, status_area);

    // Picker overlay sits on top of the buffer pane. Renders last so
    // its `Clear` widget masks the editor content beneath it.
    if app.picker.is_some() {
        picker_overlay(frame, app, buf_area);
    }

    // Completion popup: floating over the buffer, below picker/info.
    if app.completion.is_some() {
        completion_popup(frame, app, buf_area);
    }

    // Info popup (`:reg`, `:marks`, `:jumps`, `:changes`) renders on top of
    // the picker overlay so it always shows.
    if app.info_popup.is_some() {
        info_popup_overlay(frame, app, buf_area);
    }
}

/// Render the vim-style tab bar. Only called when `app.tabs.len() > 1`.
///
/// Format: `[1: foo.rs] [2: +bar.rs]` where the number is the 1-based tab
/// index, the label is the focused window's slot basename (or `[No Name]`),
/// and a leading `+` marks any dirty slot in the tab.  Active tab is
/// highlighted; inactive tabs are dimmed.
fn tab_bar(frame: &mut Frame, app: &App, area: Rect) {
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

    for (i, tab) in app.tabs.iter().enumerate() {
        // Find the focused window's slot for this tab.
        let slot_idx = app.windows[tab.focused_window]
            .as_ref()
            .map(|w| w.slot)
            .unwrap_or(0);
        let slot = &app.slots()[slot_idx];
        let base_name = slot
            .filename
            .as_ref()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("[No Name]");
        // Mark tab dirty if any window in the tab has a dirty slot.
        let tab_dirty = tab.layout.leaves().iter().any(|&wid| {
            app.windows[wid]
                .as_ref()
                .map(|w| app.slots()[w.slot].dirty)
                .unwrap_or(false)
        });
        let label = if tab_dirty {
            format!("[{}: +{}]", i + 1, base_name)
        } else {
            format!("[{}: {}]", i + 1, base_name)
        };

        let sep = if i == 0 { "" } else { " " };
        let entry_width = sep.len() + label.len();

        if used + entry_width > max_width {
            if used < max_width {
                spans.push(Span::styled("…".to_string(), sep_style));
            }
            break;
        }

        if i > 0 {
            spans.push(Span::styled(" ".to_string(), sep_style));
        }
        let style = if i == app.active_tab {
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
pub(crate) fn search_count(app: &App) -> Option<(usize, usize)> {
    const MATCH_CAP: usize = 10_000;
    let st = app.active().editor.search_state();
    let pat = st.pattern.as_ref()?;
    let buf = app.active().editor.buffer();
    let (cursor_row, cursor_col) = app.active().editor.cursor();
    // The engine reports `cursor_col` as a char index, but `regex::Match::start`
    // returns a byte offset. On lines with multi-byte chars before the match
    // (e.g. an em-dash in a doc comment) byte > char and the comparison drops
    // the match the cursor is sitting on. Convert the cursor to a byte offset
    // on its own line so both sides of the inequality are byte-counted.
    let cursor_byte = buf
        .lines()
        .get(cursor_row)
        .map(|line| {
            line.char_indices()
                .nth(cursor_col)
                .map(|(b, _)| b)
                .unwrap_or(line.len())
        })
        .unwrap_or(0);
    let mut total = 0usize;
    let mut current_idx = 0usize;
    'outer: for (row_idx, line) in buf.lines().iter().enumerate() {
        for m in pat.find_iter(line) {
            total += 1;
            if (row_idx, m.start()) <= (cursor_row, cursor_byte) {
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

    // ── Macro recording indicator ───────────────────────────────────────────
    // Vim shows "recording @r" while `q{reg}` is active. Render it as a
    // status-message-equivalent so it visually pre-empts the lualine row,
    // matching vim's bottom-line takeover.
    if let Some(reg) = app.active().editor.recording_register() {
        let content = format!(" recording @{reg}");
        let padded = format!("{content:<width$}", width = width as usize);
        return (
            Line::from(vec![Span::styled(
                padded,
                Style::default()
                    .bg(app.theme.ui.surface_bg)
                    .fg(app.theme.ui.text)
                    .add_modifier(Modifier::BOLD),
            )]),
            None,
        );
    }

    // ── Grammar load error (transient, 5 s TTL) ────────────────────────────
    if let Some(err) = &app.grammar_load_error
        && !err.is_expired()
    {
        let content = format!(" grammar load failed: {} — {}", err.name, err.message);
        let truncated = if content.len() > width as usize {
            let max = (width as usize).saturating_sub(1);
            format!("{}…", &content[..max.min(content.len())])
        } else {
            content
        };
        let padded = format!("{truncated:<width$}", width = width as usize);
        return (
            Line::from(vec![Span::styled(
                padded,
                Style::default()
                    .bg(app.theme.ui.surface_bg)
                    .fg(app.theme.ui.status_dirty_marker)
                    .add_modifier(Modifier::BOLD),
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
    // LSP diag count block: E:N W:N ... skip zero-count categories.
    let diag_count_block: String = {
        let diags = &app.active().lsp_diags;
        if diags.is_empty() {
            String::new()
        } else {
            let e = diags
                .iter()
                .filter(|d| d.severity == DiagSeverity::Error)
                .count();
            let w2 = diags
                .iter()
                .filter(|d| d.severity == DiagSeverity::Warning)
                .count();
            let i = diags
                .iter()
                .filter(|d| d.severity == DiagSeverity::Info)
                .count();
            let h = diags
                .iter()
                .filter(|d| d.severity == DiagSeverity::Hint)
                .count();
            let mut parts = Vec::new();
            if e > 0 {
                parts.push(format!("E:{e}"));
            }
            if w2 > 0 {
                parts.push(format!("W:{w2}"));
            }
            if i > 0 {
                parts.push(format!("I:{i}"));
            }
            if h > 0 {
                parts.push(format!("H:{h}"));
            }
            if parts.is_empty() {
                String::new()
            } else {
                format!(" {} ", parts.join(" "))
            }
        }
    };
    let suffix = format!("{ro_tag}{new_tag}{disk_tag}{untracked_tag}");

    // Loading block — inline spinner for in-flight LSP requests OR
    // pending grammar compile. LSP wins when both are happening since
    // the user typically just pressed gd/gr/K and the grammar is older
    // background work. Empty otherwise.
    let loading_block: String = if !app.lsp_pending.is_empty() {
        let label = app
            .lsp_pending
            .values()
            .next()
            .map(|p| match p {
                crate::app::LspPendingRequest::GotoDefinition { .. } => "definition",
                crate::app::LspPendingRequest::GotoDeclaration { .. } => "declaration",
                crate::app::LspPendingRequest::GotoTypeDefinition { .. } => "type definition",
                crate::app::LspPendingRequest::GotoImplementation { .. } => "implementation",
                crate::app::LspPendingRequest::GotoReferences { .. } => "references",
                crate::app::LspPendingRequest::Hover { .. } => "hover",
                crate::app::LspPendingRequest::Completion { .. } => "completion",
                crate::app::LspPendingRequest::CodeAction { .. } => "code action",
                crate::app::LspPendingRequest::Rename { .. } => "rename",
                _ => "request",
            })
            .unwrap_or("request");
        format!(" {} LSP:{label} ", hjkl_ratatui::spinner::frame())
    } else if let Some(name) = app.pending_grammar_name_for_active() {
        format!(" {} grammar:{name} ", hjkl_ratatui::spinner::frame())
    } else {
        String::new()
    };

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
        + diag_count_block.len()
        + loading_block.len()
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
        + diag_count_block.len()
        + loading_block.len()
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
    if !diag_count_block.is_empty() {
        // Color the diag count by the highest-severity present.
        let diags = &app.active().lsp_diags;
        let diag_style = if diags.iter().any(|d| d.severity == DiagSeverity::Error) {
            Style::default()
                .bg(ui.surface_bg)
                .fg(Color::Red)
                .add_modifier(Modifier::BOLD)
        } else if diags.iter().any(|d| d.severity == DiagSeverity::Warning) {
            Style::default().bg(ui.surface_bg).fg(Color::Yellow)
        } else if diags.iter().any(|d| d.severity == DiagSeverity::Info) {
            Style::default().bg(ui.surface_bg).fg(Color::Blue)
        } else {
            Style::default().bg(ui.surface_bg).fg(Color::Cyan)
        };
        spans.push(Span::styled(diag_count_block, diag_style));
    }
    if !loading_block.is_empty() {
        let loading_style = Style::default()
            .bg(ui.surface_bg)
            .fg(ui.text)
            .add_modifier(Modifier::ITALIC);
        spans.push(Span::styled(loading_block, loading_style));
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
    // Picker preview always shows absolute numbers with neovim default numberwidth.
    let gw = gutter_width(line_count.max(1), true, false, 4);
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
            ..Default::default()
        }),
        search_bg: Style::default(),
        signs: &[],
        conceals: &[],
        spans: &preview_spans.by_row,
        search_pattern: None,
        non_text_style: Style::default().fg(theme.non_text),
        diag_overlays: &[],
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
