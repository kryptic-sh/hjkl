//! Per-frame render functions.
//!
//! [`frame`] is the top-level entry point called from the event loop.
//! It splits the terminal area into a buffer pane + status line row and
//! delegates to [`buffer_pane`] and [`status_line`].

use hjkl_buffer::Viewport;
use hjkl_buffer_tui::{BufferView, DiagOverlay, Gutter, GutterNumbers};
use hjkl_engine::{Host, Query};
use hjkl_statusline::{
    Bar, Color as SlColor, Segment as SlSegment, StatusTheme, Style as SlStyle, StyleExt,
    dirty_segment, filename_segment, loading_segment, mode_segment, pending_segment,
    search_count_segment, truncate_filename,
};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
};

use crate::app::{App, DiagSeverity, DiskState, STATUS_LINE_HEIGHT, TOP_BAR_HEIGHT, window};
use hjkl_holler_tui::{HollerLayout, render_active};
use hjkl_prompt_tui::{PromptTheme, build_prompt_line, render_wildmenu};
use hjkl_tabs::TabBar;
use hjkl_tabs_tui::{TabBarTheme, build_line as build_tab_line};

/// Convert a `ratatui::style::Color::Rgb` to `hjkl_statusline::Color`.
/// Named/indexed colors fall back to white.
fn ratatui_rgb_to_sl(c: Color) -> SlColor {
    match c {
        Color::Rgb(r, g, b) => SlColor::rgb(r, g, b),
        _ => SlColor::rgb(0xff, 0xff, 0xff),
    }
}

/// Convert a `ratatui::style::Color::Rgb` to `hjkl_theme::Color`.
/// Named/indexed colors fall back to white.
fn to_hjkl_color(c: Color) -> hjkl_theme::Color {
    match c {
        Color::Rgb(r, g, b) => hjkl_theme::Color::rgb(r, g, b),
        _ => hjkl_theme::Color::rgb(0xff, 0xff, 0xff),
    }
}

/// Build a `StatusTheme` from the app's `UiTheme`.
///
/// `StatusTheme` is `#[non_exhaustive]`; construct via `Default` + field mutation
/// so new colour slots added upstream don't break this site.
fn app_status_theme(app: &App) -> StatusTheme {
    let ui = &app.theme.ui;
    let mut t = StatusTheme::default();
    t.bg = ratatui_rgb_to_sl(ui.surface_bg);
    t.fg = ratatui_rgb_to_sl(ui.text);
    t.fill_bg = ratatui_rgb_to_sl(ui.panel_bg);
    t.mode_normal_bg = ratatui_rgb_to_sl(ui.mode_normal_bg);
    t.mode_normal_fg = ratatui_rgb_to_sl(ui.on_accent);
    t.mode_insert_bg = ratatui_rgb_to_sl(ui.mode_insert_bg);
    t.mode_insert_fg = ratatui_rgb_to_sl(ui.on_accent);
    t.mode_visual_bg = ratatui_rgb_to_sl(ui.mode_visual_bg);
    t.mode_visual_fg = ratatui_rgb_to_sl(ui.on_accent);
    t.dirty_fg = ratatui_rgb_to_sl(ui.status_dirty_marker);
    t.readonly_fg = ratatui_rgb_to_sl(ui.text);
    t.new_file_fg = ratatui_rgb_to_sl(ui.text);
    t.recording_bg = ratatui_rgb_to_sl(ui.recording_bg);
    t.recording_fg = ratatui_rgb_to_sl(ui.recording_fg);
    t
}

/// Build the normal-mode status bar as an agnostic `Bar`.
///
/// Populates left/right segments from app state. The caller converts
/// to ratatui via `hjkl_statusline_tui::to_line`.
pub(crate) fn build_normal_status_bar(app: &App, width: u16) -> Line<'static> {
    let theme = app_status_theme(app);
    let ui = &app.theme.ui;
    let mode = app.mode_label();

    let mut bar = Bar {
        fill_style: SlStyle::default_style()
            .bg(ratatui_rgb_to_sl(ui.panel_bg))
            .fg(ratatui_rgb_to_sl(ui.text)),
        ..Default::default()
    };

    // ── Left side ────────────────────────────────────────────────────────────
    // Note: recording @r is handled in build_status_line as a full-line
    // takeover (vim bottom-line behaviour). It never reaches this path.
    bar.left.push(mode_segment(mode, &theme));

    {
        let pc = app.active().editor.pending_count();
        let po = app.active().editor.pending_op();
        let po_str = po.map(|s| s.to_string());
        if let Some(seg) = pending_segment(pc.map(|n| n as u64), po_str.as_deref(), &theme) {
            bar.left.push(seg);
        }
    }

    // Filename with tags.
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
    let suffix = format!("{ro_tag}{new_tag}{disk_tag}{untracked_tag}");

    // Reserve space for all right-side blocks + filename padding + suffix
    // to determine how many chars the filename itself can occupy.
    // We use char counts to stay consistent with Bar::layout.
    let (row, col) = app.active().editor.cursor();
    let line_count = app.active().editor.buffer().line_count() as usize;

    let pos_content = format!(" {}:{} ", row + 1, col + 1);
    let pct_content = {
        let pct = ((row + 1) * 100).checked_div(line_count).unwrap_or(0);
        format!(" {pct}% ")
    };

    // Build loading label (used both for reservation + actual segment).
    let loading_label: String = if !app.lsp_pending.is_empty() {
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
        format!("{} LSP:{label}", hjkl_editor_tui::spinner::frame())
    } else {
        let names = app.directory.in_flight_names();
        match names.len() {
            0 => String::new(),
            1 => format!("{} grammar:{}", hjkl_editor_tui::spinner::frame(), names[0]),
            n => format!(
                "{} grammar:{} +{}",
                hjkl_editor_tui::spinner::frame(),
                names[0],
                n - 1
            ),
        }
    };

    // Build diag count content.
    let diag_count_content: String = {
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

    // Search count content.
    let search_count_content: String = search_count(app)
        .map(|(idx, total)| format!(" [{idx}/{total}] "))
        .unwrap_or_default();

    // Dirty marker content.
    let dirty_content = if app.active().dirty { " ● " } else { "" };

    // Compute available chars for filename:
    // total = left_fixed + " " + name + suffix + " " + dirty + search + diag + loading + pos + pct
    // Note: rec_chars omitted — recording is a full-line takeover handled in
    // build_status_line and never reaches this function.
    let w = width as usize;
    let mode_chars = mode.chars().count() + 2; // " MODE "
    let pending_chars = {
        let pc = app.active().editor.pending_count();
        let po = app.active().editor.pending_op();
        match (pc, po) {
            (Some(n), Some(op)) => format!(" {n}{op} ").chars().count(),
            (Some(n), None) => format!(" {n} ").chars().count(),
            (None, Some(op)) => format!(" {op} ").chars().count(),
            (None, None) => 0,
        }
    };
    let right_chars = pos_content.chars().count() + pct_content.chars().count();
    let loading_chars = if loading_label.is_empty() {
        0
    } else {
        loading_label.chars().count() + 2 // " ... "
    };
    let reserved = mode_chars
        + pending_chars
        + 2 // " name "
        + suffix.chars().count()
        + dirty_content.chars().count()
        + search_count_content.chars().count()
        + diag_count_content.chars().count()
        + loading_chars
        + right_chars;
    let avail_for_name = w.saturating_sub(reserved);
    let filename = truncate_filename(&raw_filename, avail_for_name);

    bar.left.push(filename_segment(&filename, &suffix, &theme));

    // ── Left-side extras (dirty, search, diag, loading) go as left segments ─
    if let Some(seg) = dirty_segment(app.active().dirty, &theme) {
        bar.left.push(seg);
    }

    if !search_count_content.is_empty() {
        bar.left.push(search_count_segment(
            0, // dummy — we use raw content via loading_segment
            0, &theme,
        ));
        // Replace last segment with the actual pre-formatted content.
        if let Some(SlSegment::Text { content, .. }) = bar.left.last_mut() {
            *content = search_count_content.clone();
        }
    }

    if !diag_count_content.is_empty() {
        // Color by highest severity.
        let diags = &app.active().lsp_diags;
        let diag_fg = if diags.iter().any(|d| d.severity == DiagSeverity::Error) {
            SlColor::rgb(0xff, 0x00, 0x00) // Red
        } else if diags.iter().any(|d| d.severity == DiagSeverity::Warning) {
            SlColor::rgb(0xff, 0xc0, 0x00) // Yellow-ish
        } else if diags.iter().any(|d| d.severity == DiagSeverity::Info) {
            SlColor::rgb(0x00, 0x7a, 0xff) // Blue
        } else {
            SlColor::rgb(0x00, 0xd7, 0xd7) // Cyan
        };
        bar.left.push(SlSegment::Text {
            content: diag_count_content.clone(),
            style: SlStyle::default_style()
                .bg(ratatui_rgb_to_sl(ui.surface_bg))
                .fg(diag_fg),
        });
    }

    if !loading_label.is_empty() {
        bar.left.push(loading_segment(&loading_label, "", &theme));
        // Fix content: loading_segment adds " frame label " but we want " frame+label "
        if let Some(SlSegment::Text { content, .. }) = bar.left.last_mut() {
            *content = format!(" {loading_label} ");
        }
    }

    // ── Right side ───────────────────────────────────────────────────────────
    bar.right.push(SlSegment::Text {
        content: pos_content,
        style: SlStyle::default_style()
            .bg(ratatui_rgb_to_sl(ui.surface_bg))
            .fg(ratatui_rgb_to_sl(ui.text)),
    });
    bar.right.push(SlSegment::Text {
        content: pct_content,
        style: SlStyle::default_style()
            .bg(ratatui_rgb_to_sl(ui.mode_normal_bg))
            .fg(ratatui_rgb_to_sl(ui.on_accent))
            .bold(),
    });

    hjkl_statusline_tui::to_line(&bar, width)
}

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

/// Full gutter width including number column, sign column, and fold column.
///
/// `lnum_width` must come from `Editor::lnum_width()` — the single source of
/// truth for the number-column width. Pass 0 when both number flags are off.
///
/// - `sign_column` width: 1 cell when `mode=Yes` or (`mode=Auto` and any visible sign).
/// - `fold_column` width: `foldcolumn` cells (0 = none, capped at 12).
fn full_gutter_width(
    lnum_width: u16,
    signcolumn: hjkl_engine::types::SignColumnMode,
    foldcolumn: u32,
    has_visible_signs: bool,
) -> u16 {
    let sign_w: u16 = match signcolumn {
        hjkl_engine::types::SignColumnMode::Yes => 1,
        hjkl_engine::types::SignColumnMode::No => 0,
        hjkl_engine::types::SignColumnMode::Auto => {
            if has_visible_signs {
                1
            } else {
                0
            }
        }
    };
    let fold_w = foldcolumn.min(12) as u16;
    lnum_width + sign_w + fold_w
}

/// Parse a comma-separated colorcolumn string into a sorted `Vec<u16>` of
/// 1-based column indices. Non-numeric or zero entries are silently ignored.
fn parse_colorcolumn(cc: &str) -> Vec<u16> {
    if cc.is_empty() {
        return Vec::new();
    }
    let mut cols: Vec<u16> = cc
        .split(',')
        .filter_map(|s| s.trim().parse::<u16>().ok())
        .filter(|&n| n > 0)
        .collect();
    cols.sort_unstable();
    cols.dedup();
    cols
}

/// Bg painted across the cursor row in both the editor pane and the
/// picker preview pane. Subtle blue-grey — visible enough to track the
/// cursor at a glance without competing with the syntax foreground.
fn cursor_line_bg(theme: &crate::theme::UiTheme) -> Style {
    Style::default().bg(theme.cursor_line_bg)
}

/// Split a `Rect` into two parts according to `dir` and `ratio`.
fn split_rect(area: Rect, dir: window::SplitDir, ratio: f32) -> (Rect, Rect) {
    match dir.axis() {
        window::Axis::Row => {
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
        window::Axis::Col => {
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

/// Draw a 1-cell-wide separator between sibling panes.
///
/// For `SplitDir::Vertical` (side-by-side panes) the separator is a column
/// of `│` characters. For `SplitDir::Horizontal` (stacked panes) it is a
/// row of `─` characters. The separator uses `theme.border` so it matches
/// the popup / picker border color without requiring a new theme field.
fn draw_separator(
    frame: &mut Frame,
    sep_rect: Rect,
    dir: window::SplitDir,
    border_color: ratatui::style::Color,
) {
    use ratatui::buffer::Cell;
    let style = Style::default().fg(border_color);
    let (glyph, glyph_width) = match dir.axis() {
        window::Axis::Col => ("│", 1u16),
        window::Axis::Row => ("─", 1u16),
    };
    let buf = frame.buffer_mut();
    match dir.axis() {
        window::Axis::Col => {
            // sep_rect is a single column; iterate rows.
            for row in sep_rect.y..sep_rect.y + sep_rect.height {
                if let Some(cell) = buf.cell_mut((sep_rect.x, row)) {
                    *cell = Cell::default();
                    cell.set_symbol(glyph);
                    cell.set_style(style);
                }
            }
        }
        window::Axis::Row => {
            // sep_rect is a single row; iterate columns.
            let mut col = sep_rect.x;
            while col < sep_rect.x + sep_rect.width {
                if let Some(cell) = buf.cell_mut((col, sep_rect.y)) {
                    *cell = Cell::default();
                    cell.set_symbol(glyph);
                    cell.set_style(style);
                    col += glyph_width;
                } else {
                    break;
                }
            }
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
            // Record the FULL rect (pre-separator) so that resize commands
            // can convert line/column deltas to ratio updates correctly.
            *last_rect = Some(window::rect_to_layout(area));

            let (rect_a, rect_b) = split_rect(area, *dir, *ratio);

            // Carve a 1-cell separator between the two child rects and
            // shrink the right/bottom child by 1 cell so children never
            // overlap the separator. Skip when the rect is too small.
            let border_color = app.theme.ui.border;
            let (rect_a, sep_rect, rect_b) = match dir.axis() {
                window::Axis::Col => {
                    // Side-by-side: separator is the rightmost column of rect_a.
                    // Shrink rect_a by 1 on the right; sep is that freed column;
                    // rect_b stays (it already starts right after rect_a).
                    if rect_a.width < 2 || rect_b.width == 0 {
                        // Too narrow — no separator, pass through as-is.
                        (rect_a, None, rect_b)
                    } else {
                        let a_shrunk = Rect {
                            width: rect_a.width.saturating_sub(1),
                            ..rect_a
                        };
                        let sep = Rect {
                            x: rect_a.x + rect_a.width.saturating_sub(1),
                            y: rect_a.y,
                            width: 1,
                            height: rect_a.height,
                        };
                        (a_shrunk, Some(sep), rect_b)
                    }
                }
                window::Axis::Row => {
                    // Stacked: separator is the bottom row of rect_a.
                    if rect_a.height < 2 || rect_b.height == 0 {
                        (rect_a, None, rect_b)
                    } else {
                        let a_shrunk = Rect {
                            height: rect_a.height.saturating_sub(1),
                            ..rect_a
                        };
                        let sep = Rect {
                            x: rect_a.x,
                            y: rect_a.y + rect_a.height.saturating_sub(1),
                            width: rect_a.width,
                            height: 1,
                        };
                        (a_shrunk, Some(sep), rect_b)
                    }
                }
            };

            render_layout(frame, app, rect_a, a);
            render_layout(frame, app, rect_b, b);

            // Draw separator on top of both children after they render so
            // that no window content bleeds into the separator cell.
            if let Some(sep) = sep_rect {
                draw_separator(frame, sep, *dir, border_color);
            }
        }
        // `LayoutTree` is `#[non_exhaustive]`; unknown variant → skip rendering.
        _ => {}
    }
}

/// Render a single window occupying `area`.
fn render_window(frame: &mut Frame, app: &mut App, area: Rect, win_id: window::WindowId) {
    // Record the rendered rect for Phase 2+ direction navigation.
    if let Some(win) = app.windows[win_id].as_mut() {
        win.last_rect = Some(window::rect_to_layout(area));
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
    let (nu, rnu) = (s.number, s.relativenumber);
    let (scl, fdc) = (s.signcolumn, s.foldcolumn);
    let (cul, cuc) = (s.cursorline, s.cursorcolumn);
    let colorcolumn = s.colorcolumn.clone();

    // We need visible signs before computing gutter width for signcolumn=auto.
    // Pre-compute a lightweight "has any visible sign" check using the last
    // known viewport top (updated after lnum_width when focused).
    let pre_vp_top = if is_focused {
        app.slots()[slot_idx].editor.host().viewport().top_row
    } else {
        top_row
    };
    let pre_vp_bot = pre_vp_top + area.height as usize;
    let has_visible_signs = app.slots()[slot_idx]
        .diag_signs
        .iter()
        .chain(app.slots()[slot_idx].diag_signs_lsp.iter())
        .chain(app.slots()[slot_idx].git_signs.iter())
        .any(|s| s.row >= pre_vp_top && s.row < pre_vp_bot);

    let sign_w: u16 = match scl {
        hjkl_engine::types::SignColumnMode::Yes => 1,
        hjkl_engine::types::SignColumnMode::No => 0,
        hjkl_engine::types::SignColumnMode::Auto => {
            if has_visible_signs {
                1
            } else {
                0
            }
        }
    };
    let fold_w = fdc.min(12) as u16;
    let num_gw_for_text = app.slots()[slot_idx].editor.lnum_width();
    let gw = sign_w + num_gw_for_text + fold_w;
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
    // Number-column width only (sign/fold widths are tracked separately).
    let num_gw = app.slots()[slot_idx].editor.lnum_width();
    let gutter = if num_gw > 0 || sign_w > 0 {
        Some(Gutter {
            width: num_gw,
            style: Style::default().fg(app.theme.ui.gutter),
            line_offset: 0,
            numbers,
            sign_column_width: sign_w,
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
    let mut visible_signs: Vec<hjkl_buffer_tui::Sign> = app.slots()[slot_idx]
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

    // Resolve cursorline / cursorcolumn styles for this window.
    let cursor_line_style = if show_cursor && cul {
        cursor_line_bg(&app.theme.ui)
    } else {
        Style::default()
    };
    let cursor_column_style = if show_cursor && cuc {
        Style::default().bg(app.theme.ui.cursor_column_bg)
    } else {
        Style::default()
    };

    // Colorcolumn indices (1-based) — rendered under syntax.
    let cc_cols = parse_colorcolumn(&colorcolumn);
    let cc_style = Style::default().bg(app.theme.ui.colorcolumn_bg);

    let diag_overlays = build_diag_overlays(&app.slots()[slot_idx], &app.theme.ui);
    let view = BufferView {
        buffer: app.slots()[slot_idx].editor.buffer(),
        viewport: viewport_ref,
        selection,
        resolver: &resolver,
        cursor_line_bg: cursor_line_style,
        cursor_column_bg: cursor_column_style,
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
        colorcolumn_cols: &cc_cols,
        colorcolumn_style: cc_style,
    };
    frame.render_widget(view, area);

    // ── Auto-indent flash overlay ─────────────────────────────────────────
    //
    // Approach: post-render ratatui buffer walk. After `BufferView` has painted
    // all cells, we overwrite the bg of every cell on flash rows that are
    // currently visible. Instant on, instant off — no fade (per UX spec).
    // The overlay is painted only on the focused window to avoid a visually
    // confusing multi-pane flash.
    //
    // We call `indent_flash_active` on the shared App state here rather than
    // passing a pre-computed value so the expiry check happens at render time
    // (the same instant the user sees the frame).
    if is_focused && let Some((flash_top, flash_bot)) = app.indent_flash_active() {
        let flash_bg = app.theme.ui.indent_flash_bg;
        let flash_style = Style::default().bg(flash_bg);
        let buf = frame.buffer_mut();
        let screen_top = area.y;
        let screen_height = area.height;
        // Constrain to the TEXT area only — the flash overlay must not bleed
        // into the gutter. The buffer renderer paints text starting at
        // `area.x + sign_w + num_gw` (dedicated sign column + number column).
        // Without this, the gutter and text area share the same flash bg
        // and the cursor visually appears to sit "in the gutter".
        let text_x = area.x + sign_w + num_gw;
        let text_right = area.x + area.width;
        // Clamp flash range to visible rows.
        let vis_start = flash_top.max(vp_top);
        let vis_end = flash_bot.min(vp_top + screen_height as usize - 1);
        for buf_row in vis_start..=vis_end {
            let screen_row = screen_top + (buf_row - vp_top) as u16;
            if screen_row >= screen_top + screen_height {
                break;
            }
            for col in text_x..text_right {
                let cell = buf.cell_mut((col, screen_row));
                if let Some(c) = cell {
                    c.set_style(c.style().patch(flash_style));
                }
            }
        }
    }

    // Emit the terminal cursor only for the focused window.
    // extra_gutter_width = sign_w + fold_w (cells left of number column).
    if show_cursor
        && let Some((cx, cy)) = app.slots_mut()[slot_idx]
            .editor
            .cursor_screen_pos_in_rect(area, sign_w + fold_w)
    {
        frame.set_cursor_position((cx, cy));
    }
}

/// Render the completion popup, floating below the cursor position.
///
/// Only called when `app.completion.is_some()`.
fn completion_popup(frame: &mut Frame, app: &App, buf_area: Rect) {
    let completion = match app.completion.as_ref() {
        Some(p) => p,
        None => return,
    };

    // Convert buffer coordinates to screen coordinates inside buf_area.
    // The focused window may have a gutter — compute its width.
    let slot_idx = {
        let fw = app.focused_window();
        app.windows[fw].as_ref().map(|w| w.slot).unwrap_or(0)
    };
    let s = app.slots()[slot_idx].editor.settings();
    let vp = app.slots()[slot_idx].editor.host().viewport();
    let vp_top = vp.top_row;
    let vp_bot = vp_top + 100; // generous upper bound for sign detection
    let has_visible_signs = app.slots()[slot_idx]
        .diag_signs
        .iter()
        .chain(app.slots()[slot_idx].diag_signs_lsp.iter())
        .chain(app.slots()[slot_idx].git_signs.iter())
        .any(|sg| sg.row >= vp_top && sg.row < vp_bot);
    let gw = full_gutter_width(
        app.slots()[slot_idx].editor.lnum_width(),
        s.signcolumn,
        s.foldcolumn,
        has_visible_signs,
    );

    // Cursor cell in absolute screen coordinates (0-based row relative to viewport top).
    let cursor_row = completion.anchor_row.saturating_sub(vp_top) as u16;
    let cursor_col = completion.anchor_col as u16 + gw;
    let anchor = Rect {
        x: buf_area.x + cursor_col,
        y: buf_area.y + cursor_row,
        width: 1,
        height: 1,
    };

    let ui = &app.theme.ui;
    let theme = hjkl_completion_tui::CompletionTheme::new(
        to_hjkl_color(ui.border_active),
        to_hjkl_color(ui.picker_selection_bg),
        to_hjkl_color(ui.text),
        to_hjkl_color(ui.text_dim),
    );
    hjkl_completion_tui::popup(frame, completion, &theme, anchor, buf_area);
}

/// Render one complete frame into `frame`.
pub fn frame(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    let show_top_bar = app.tabs.len() > 1 || app.slots().len() > 1;
    let (buf_area, status_area, top_bar_area) = {
        // Build constraint list dynamically based on what rows are visible.
        let mut constraints = Vec::new();
        if show_top_bar {
            constraints.push(Constraint::Length(TOP_BAR_HEIGHT));
        }
        constraints.push(Constraint::Min(1));
        constraints.push(Constraint::Length(STATUS_LINE_HEIGHT));

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area);

        let mut idx = 0usize;
        let tb_area = if show_top_bar {
            let a = Some(chunks[idx]);
            idx += 1;
            a
        } else {
            None
        };
        let buf = chunks[idx];
        idx += 1;
        let stat = chunks[idx];
        (buf, stat, tb_area)
    };

    // Splash screen path — skip all buffer rendering while active.
    if let Some(ref screen) = app.start_screen {
        if let Some(tb) = top_bar_area {
            top_bar(frame, app, tb);
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

    if let Some(tb) = top_bar_area {
        top_bar(frame, app, tb);
    }

    // Walk the window tree and render each pane. Use take_layout /
    // restore_layout so we can pass `&mut LayoutTree` to render_layout
    // (which writes last_rect on Split nodes) while also holding
    // `&mut App` for render_window.
    let mut layout = app.take_layout();
    render_layout(frame, app, buf_area, &mut layout);
    app.restore_layout(layout);

    // When wildmenu is active, split the status row into [wildmenu | prompt].
    // The wildmenu occupies one additional row ABOVE the prompt; we carve
    // it from the bottom of buf_area so the buffer view shrinks by one row.
    if app.command_completion.is_some() && buf_area.height >= 2 {
        let wm_row = buf_area.y + buf_area.height - 1;
        let wm_area = Rect {
            x: buf_area.x,
            y: wm_row,
            width: buf_area.width,
            height: 1,
        };
        wildmenu(frame, app, wm_area);
    }

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

    // Which-key popup: shown after a prefix key idles past the configured delay.
    if app.which_key_active {
        which_key_popup(frame, app, buf_area);
    }

    // Info popup (`:reg`, `:marks`, `:jumps`, `:changes`) renders on top of
    // the picker overlay so it always shows.
    if app.info_popup.is_some() {
        info_popup_overlay(frame, app, buf_area);
    }

    // Context menu (right-click, Phase 2 Round A) — floats above everything.
    if let Some(ref menu) = app.context_menu {
        crate::menu::render(frame, menu, area, &crate::menu::MenuTheme::default());
    }

    // Hover popup (Phase 5 mouse support) — renders above all other content.
    if let Some(ref popup) = app.hover_popup {
        let hover_theme = hjkl_hover_tui::HoverTheme::new(
            app.theme.ui.border_active,
            app.theme.ui.panel_bg,
            hjkl_markdown_tui::MdTheme::default(),
        );
        hjkl_hover_tui::render(frame, popup, &hover_theme, frame.area());
    }

    // Toast notifications — float top-right, newest on top.
    let holler_layout = HollerLayout::default();
    render_active(
        frame,
        area,
        &app.bus,
        &holler_layout,
        std::time::SystemTime::now(),
    );
}

/// Render the unified top bar into a single row.
///
/// Left side: buffer-line entries (one per slot), only when `app.slots().len() > 1`.
/// Format: ` name ` or ` name+ ` (dirty), separated by `│`.
///
/// Right side: tab entries (one per layout tab), only when `app.tabs.len() > 1`.
/// Rendered via [`hjkl_tabs_tui::build_line`] so tab-bar behaviour (active
/// highlight, dirty marker `●`, overflow `<`/`>`) is owned by `hjkl-tabs-tui`.
///
/// If both sides overflow the row width, the left (buffer) side is truncated
/// from the right with `…`. If buffers still don't fit even after truncation,
/// they are dropped entirely.
///
/// Show the row whenever EITHER side has content. When neither does, this
/// function is not called (see `render::frame`).
fn top_bar(frame: &mut Frame, app: &App, area: Rect) {
    let ui = &app.theme.ui;
    let active_style = Style::default()
        .fg(ui.on_accent)
        .bg(ui.mode_normal_bg)
        .add_modifier(Modifier::BOLD);
    let inactive_style = Style::default().fg(ui.text_dim);
    let sep_style = Style::default().fg(ui.border);

    let show_tabs = app.tabs.len() > 1;
    let show_buffers = app.slots().len() > 1;
    let total_width = area.width as usize;

    // ── Right side: build a hjkl_tabs::TabBar from the layout tabs ──────────
    //
    // Each `hjkl_layout::Tab` (a window-split tree tab) maps to one
    // `hjkl_tabs::Tab<usize>` where the id is the positional index.
    // The title is `"{n}: {filename}"` matching the pre-migration format.
    // Dirty = any leaf window in the layout has a dirty slot.
    let mut tab_bar: TabBar<usize> = TabBar::new();
    if show_tabs {
        for (i, layout_tab) in app.tabs.iter().enumerate() {
            let slot_idx = app.windows[layout_tab.focused_window]
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
            let title = format!("{}: {}", i + 1, base_name);
            tab_bar.open(i, title);
            let tab_dirty = layout_tab.layout.leaves().iter().any(|&wid| {
                app.windows[wid]
                    .as_ref()
                    .map(|w| app.slots()[w.slot].dirty)
                    .unwrap_or(false)
            });
            if let Some(t) = tab_bar.tabs.last_mut() {
                t.dirty = tab_dirty;
            }
        }
        tab_bar.focus(&app.active_tab);
    }

    // Build the tab line via hjkl-tabs-tui so active/dirty/overflow logic
    // is owned by that crate.
    let tab_theme = TabBarTheme::new(
        ui.on_accent,
        ui.mode_normal_bg,
        ui.text_dim,
        ui.border,
        ui.border,
    );
    let tab_line = if show_tabs {
        build_tab_line(area.width, &tab_bar, &tab_theme)
    } else {
        hjkl_tabs_tui::build_line(0, &tab_bar, &tab_theme)
    };
    let tabs_total_len: usize = tab_line
        .spans
        .iter()
        .map(|s| s.content.chars().count())
        .sum();

    // ── Left side: compute how much width buffers can use ────────────────────
    // Tabs are right-aligned and never truncated; buffers get the remainder.
    let buf_budget = if show_buffers {
        total_width.saturating_sub(tabs_total_len)
    } else {
        0
    };

    // Build buffer spans within the budget.
    let mut buf_spans: Vec<Span<'static>> = Vec::new();
    let mut buf_used = 0usize;

    if show_buffers && buf_budget > 0 {
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
            let sep_len = if i == 0 { 0 } else { 1 };
            let entry_width = sep_len + label.len();

            if buf_used + entry_width > buf_budget {
                // Truncate with ellipsis if space remains.
                if buf_used < buf_budget {
                    buf_spans.push(Span::styled("…".to_string(), sep_style));
                }
                break;
            }

            if i > 0 {
                buf_spans.push(Span::styled("│".to_string(), sep_style));
            }
            let style = if i == app.active_index() {
                active_style
            } else {
                inactive_style
            };
            buf_spans.push(Span::styled(label, style));
            buf_used += entry_width;
        }
    }

    // ── Assemble the full line ────────────────────────────────────────────────
    // Layout: [buf_spans ... padding ... tab_spans]
    // Padding fills the gap so tabs land flush-right.

    let mut all_spans: Vec<Span<'static>> = buf_spans;

    if show_tabs {
        // Pad between left and right sides.
        let used_left = buf_used;
        let pad_width = total_width.saturating_sub(used_left + tabs_total_len);
        if pad_width > 0 {
            all_spans.push(Span::raw(" ".repeat(pad_width)));
        }

        // Emit tab spans from hjkl-tabs-tui.
        all_spans.extend(tab_line.spans);
    }

    let paragraph = Paragraph::new(Line::from(all_spans));
    frame.render_widget(paragraph, area);
}

/// Build a [`PromptTheme`] from the app's [`crate::theme::UiTheme`].
///
/// Maps the form color slots to the corresponding `PromptTheme` fields so that
/// wildmenu and prompt-bar rendering respect the user's configured palette.
fn prompt_theme(ui: &crate::theme::UiTheme) -> PromptTheme {
    PromptTheme::new(
        ui.form_insert_bg,
        ui.form_normal_bg,
        ui.text,
        ui.form_tag_insert_fg,
        ui.form_tag_normal_fg,
        ui.panel_bg,
        ui.text,
        ui.picker_selection_bg,
    )
}

/// Render the wildmenu strip — one row of all completion candidates with the
/// selected one highlighted. Called only when `app.command_completion.is_some()`.
///
/// Delegates to [`hjkl_prompt_tui::render_wildmenu`].
fn wildmenu(frame: &mut Frame, app: &App, area: Rect) {
    let comp = match &app.command_completion {
        Some(c) => c.clone(),
        None => return,
    };
    let theme = prompt_theme(&app.theme.ui);
    // Construct a minimal PromptState carrying just the completion state so we
    // can delegate to the crate's renderer without duplicating its logic.
    let mut ps = hjkl_prompt::PromptState::new(hjkl_prompt::PromptKind::Command);
    ps.completion = Some(comp);
    render_wildmenu(frame, &ps, &theme, area);
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
        let theme = prompt_theme(&app.theme.ui);
        return (
            build_prompt_line(&content, field.vim_mode(), &theme, width),
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
        let theme = prompt_theme(&app.theme.ui);
        return (
            build_prompt_line(&content, field.vim_mode(), &theme, width),
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

    // ── Normal status line — delegated to hjkl-statusline ───────────────────
    // Palette + segments built in `build_normal_status_bar`; ratatui
    // conversion via `hjkl_statusline_tui::to_line`.
    (build_normal_status_bar(app, width), None)
}

/// Format the status line as a plain string (unit-test helper).
///
/// `readonly` and `is_new_file` mirror the app state flags.
/// Filename is truncated with `…` when necessary.
#[cfg(test)]
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
#[cfg(test)]
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

/// Render the status-line content for `app` at the given `width` and return
/// the concatenated plaintext string. Used in unit tests to assert which
/// branch (full-line banner vs normal bar) fires.
#[cfg(test)]
pub(crate) fn status_line_text(app: &App, width: u16) -> String {
    let (line, _cursor_col) = build_status_line(app, width);
    line.spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<Vec<_>>()
        .join("")
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
    // p (mut borrow on app.picker) ends here; below re-borrows app immutably
    // both as the picker handle and as the `PreviewHighlighter` impl.

    if let Some(right) = preview_area {
        let ui = &app.theme.ui;
        let theme = hjkl_picker_tui::PreviewTheme {
            border: Style::default().fg(ui.border),
            gutter: Style::default().fg(ui.gutter),
            non_text: Style::default().fg(ui.non_text),
            cursor_line: cursor_line_bg(ui),
        };
        let picker = app.picker.as_ref().expect("picker still set");
        hjkl_picker_tui::preview_pane(frame, picker, app, &theme, right);
    }
}

/// Convert an engine-native style to a ratatui `Style` for TUI rendering.
fn engine_style_to_ratatui(s: hjkl_engine::types::Style) -> Style {
    use hjkl_engine::types::Attrs;
    let mut out = Style::default();
    if let Some(fg) = s.fg {
        out = out.fg(Color::Rgb(fg.0, fg.1, fg.2));
    }
    if let Some(bg) = s.bg {
        out = out.bg(Color::Rgb(bg.0, bg.1, bg.2));
    }
    let mut m = Modifier::empty();
    if s.attrs.contains(Attrs::BOLD) {
        m |= Modifier::BOLD;
    }
    if s.attrs.contains(Attrs::ITALIC) {
        m |= Modifier::ITALIC;
    }
    if s.attrs.contains(Attrs::UNDERLINE) {
        m |= Modifier::UNDERLINED;
    }
    if s.attrs.contains(Attrs::REVERSE) {
        m |= Modifier::REVERSED;
    }
    if s.attrs.contains(Attrs::DIM) {
        m |= Modifier::DIM;
    }
    if s.attrs.contains(Attrs::STRIKE) {
        m |= Modifier::CROSSED_OUT;
    }
    if !m.is_empty() {
        out = out.add_modifier(m);
    }
    out
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
        format!(" {} scanning", hjkl_editor_tui::spinner::frame())
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
                    let base_engine = styles
                        .iter()
                        .find(|(r, _)| r.contains(&ci))
                        .map(|(_, st)| *st)
                        .unwrap_or_default();
                    let base = engine_style_to_ratatui(base_engine);
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

/// Centered popup for multi-line `:reg` / `:marks` / `:jumps` / `:changes`
/// output and the K-key LSP hover info path.
///
/// Delegates to `hjkl_info_popup_tui::render` (thin shim, ≤10 LOC).
fn info_popup_overlay(frame: &mut Frame, app: &App, buf_area: Rect) {
    let popup = match app.info_popup.as_ref() {
        Some(p) => p,
        None => return,
    };
    let theme = hjkl_info_popup_tui::InfoPopupTheme::new(app.theme.ui.border_active);
    hjkl_info_popup_tui::render(frame, popup, &theme, buf_area);
}

/// Render the which-key popup anchored at the bottom of `buf_area`.
///
/// Only called when `app.which_key_active` is `true` and a prefix is pending.
fn which_key_popup(frame: &mut Frame, app: &App, buf_area: Rect) {
    if !app.which_key_active {
        return;
    }
    let pending = app.active_which_key_prefix();
    if pending.is_empty() && !app.which_key_sticky {
        return;
    }

    let leader = app.config.editor.leader;
    let entries = crate::which_key::entries_for(
        &app.app_keymap,
        crate::app::keymap::HjklMode::Normal,
        &pending,
        leader,
    );
    if entries.is_empty() {
        return;
    }

    let ui = &app.theme.ui;
    let popup_layout = hjkl_which_key::layout(&entries, buf_area.width);

    let header_label = if pending.is_empty() {
        "root".to_string()
    } else {
        hjkl_keymap::Chord(pending.clone()).to_notation(leader)
    };

    let theme = hjkl_which_key_tui::PopupTheme::new(
        ratatui_rgb_to_sl(ui.border_active),
        ratatui_rgb_to_sl(ui.text_dim),
    );

    hjkl_which_key_tui::render(frame, &popup_layout, &header_label, &theme, buf_area);
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
    fn top_bar_height_is_one() {
        assert_eq!(crate::app::TOP_BAR_HEIGHT, 1);
    }
}
