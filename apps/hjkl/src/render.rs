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
use hjkl_prompt_tui::{PromptTheme, build_prompt_line};
use hjkl_tabs::TabBar;
use hjkl_tabs_tui::{TabBarTheme, build_line as build_tab_line};

// ── Explorer color constants ──────────────────────────────────────────────────

/// Git-status BACKGROUND colors for explorer filenames. The status is shown as
/// the name's background so the filetype/dir foreground color stays distinct
/// from the git state. The name text itself is repainted [`GIT_TEXT`] for
/// contrast against these mid-tone backgrounds.
const GIT_MODIFIED: Color = Color::Rgb(0xd7, 0x87, 0x5f);
const GIT_STAGED: Color = Color::Rgb(0x87, 0xaf, 0x5f);
const GIT_DELETED: Color = Color::Rgb(0xd7, 0x5f, 0x5f);
const GIT_UNTRACKED: Color = Color::Rgb(0x5f, 0xaf, 0xaf);

/// High-contrast text color painted over a git-status background so the
/// filename stays readable on any of the mid-tone status backgrounds.
const GIT_TEXT: Color = Color::Rgb(0x1c, 0x1c, 0x1c);

/// Convert an `Option<(u8,u8,u8)>` RGB triple to a ratatui `Color::Rgb`.
fn rgb(o: Option<(u8, u8, u8)>) -> Option<Color> {
    o.map(|(r, g, b)| Color::Rgb(r, g, b))
}

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
        let pc = app.active_editor().pending_count();
        let po = app.active_editor().pending_op();
        let po_str = po.map(|s| s.to_string());
        if let Some(seg) = pending_segment(pc.map(|n| n as u64), po_str.as_deref(), &theme) {
            bar.left.push(seg);
        }
    }

    // Filename with tags.
    let ro_tag = if app.active_editor().is_readonly() {
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
    let (row, col) = app.active_editor().cursor();
    let line_count = app.active_editor().buffer().line_count() as usize;

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
        let pc = app.active_editor().pending_count();
        let po = app.active_editor().pending_op();
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
            *content = search_count_content.clone().into();
        }
    }

    if !diag_count_content.is_empty() {
        // Color by highest severity — routed through StatusTheme so the host
        // controls the palette (adapts to terminal-named colors vs RGB).
        let diags = &app.active().lsp_diags;
        let diag_fg = if diags.iter().any(|d| d.severity == DiagSeverity::Error) {
            theme.diag_error_fg
        } else if diags.iter().any(|d| d.severity == DiagSeverity::Warning) {
            theme.diag_warning_fg
        } else if diags.iter().any(|d| d.severity == DiagSeverity::Info) {
            theme.diag_info_fg
        } else {
            theme.diag_hint_fg
        };
        bar.left.push(SlSegment::Text {
            content: diag_count_content.clone().into(),
            style: SlStyle::default_style()
                .bg(ratatui_rgb_to_sl(ui.surface_bg))
                .fg(diag_fg),
        });
    }

    if !loading_label.is_empty() {
        bar.left.push(loading_segment(&loading_label, "", &theme));
        // Fix content: loading_segment adds " frame label " but we want " frame+label "
        if let Some(SlSegment::Text { content, .. }) = bar.left.last_mut() {
            *content = format!(" {loading_label} ").into();
        }
    }

    // ── Right side ───────────────────────────────────────────────────────────
    bar.right.push(SlSegment::Text {
        content: pos_content.into(),
        style: SlStyle::default_style()
            .bg(ratatui_rgb_to_sl(ui.surface_bg))
            .fg(ratatui_rgb_to_sl(ui.text)),
    });
    bar.right.push(SlSegment::Text {
        content: pct_content.into(),
        style: SlStyle::default_style()
            .bg(ratatui_rgb_to_sl(ui.mode_normal_bg))
            .fg(ratatui_rgb_to_sl(ui.on_accent))
            .bold(),
    });

    hjkl_statusline_tui::to_line(&bar, width)
}

/// Build the style for a diagnostic severity used in overlays and the status line.
/// Style for the diagnostic span overlay on the offending code. Only the
/// *underline* is colored (by severity) — the text keeps its syntax-highlight
/// foreground, so the line is not recolored, just underlined. Applied via
/// `Style::patch`, so leaving `fg` unset preserves the cell's existing color.
fn diag_severity_style(sev: DiagSeverity) -> Style {
    Style::default()
        .underline_color(diag_severity_fg(sev))
        .add_modifier(Modifier::UNDERLINED)
}

/// Severity color: red = error, orange = warning, green = information,
/// cyan = hint. Used for the colored underline on the offending span and for
/// the inline end-of-line ghost-text hint, so both read the same palette.
fn diag_severity_fg(sev: DiagSeverity) -> Color {
    match sev {
        DiagSeverity::Error => Color::Red,
        DiagSeverity::Warning => Color::Rgb(255, 165, 0), // orange
        DiagSeverity::Info => Color::Green,
        DiagSeverity::Hint => Color::Cyan,
    }
}

/// Sort rank for severity — lower is more severe. Used to pick the single
/// diagnostic to surface as the cursor-line inline hint.
fn diag_severity_rank(sev: DiagSeverity) -> u8 {
    match sev {
        DiagSeverity::Error => 0,
        DiagSeverity::Warning => 1,
        DiagSeverity::Info => 2,
        DiagSeverity::Hint => 3,
    }
}

/// Char-safe truncation with an ellipsis. Byte-indexing `&str` panics when the
/// cut lands inside a multi-byte char (CJK / emoji); counting chars avoids it.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        format!("{}\u{2026}", s.chars().take(max).collect::<String>())
    } else {
        s.to_string()
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

/// Widest line-number gutter across all open (non-explorer) buffers, so the
/// number column can be sized to the biggest file and the text column stays put
/// when switching buffers. Buffers with line numbers off contribute 0.
pub(crate) fn max_lnum_width(app: &App) -> u16 {
    app.slots()
        .iter()
        .filter(|s| !s.is_explorer)
        .map(|s| s.editor.lnum_width())
        .max()
        .unwrap_or(0)
}

/// Stable `(sign_column_width, fold_column_width)` reserved for ALL non-explorer
/// buffers — sized to the max each would need across every open buffer. This
/// keeps the text column from shifting when a diagnostic/git sign (or a fold)
/// appears in one buffer but not another (or scrolls in/out of view): once any
/// buffer needs a sign/fold column, it's reserved everywhere.
///
/// The sign decision uses whether a buffer has ANY signs (not just ones in the
/// current viewport), so scrolling within a buffer doesn't jiggle either.
pub(crate) fn stable_gutter_extra(app: &App) -> (u16, u16) {
    use hjkl_engine::types::SignColumnMode;
    let mut sign_w = 0u16;
    let mut fold_w = 0u16;
    for slot in app.slots().iter().filter(|s| !s.is_explorer) {
        let st = slot.editor.settings();
        let has_any_signs = !slot.diag_signs.is_empty()
            || !slot.diag_signs_lsp.is_empty()
            || !slot.git_signs.is_empty();
        let sw = match st.signcolumn {
            SignColumnMode::Yes => 1,
            SignColumnMode::No => 0,
            SignColumnMode::Auto => {
                if has_any_signs {
                    1
                } else {
                    0
                }
            }
        };
        sign_w = sign_w.max(sw);
        let has_folds = !slot.editor.buffer().folds().is_empty();
        let fw = (st.foldcolumn.min(12) as u16).max(if has_folds { 1 } else { 0 });
        fold_w = fold_w.max(fw);
    }
    // Window-level folds: the shared buffer only holds the focused window's
    // set, so an unfocused window with folds wouldn't be seen by the loop
    // above. Reserve the fold column if ANY window's snapshot has folds.
    if fold_w == 0 && app.window_folds.values().any(|f| !f.is_empty()) {
        fold_w = 1;
    }
    (sign_w, fold_w)
}

/// Total rendered gutter width (sign + number + fold cells) for `slot_idx`,
/// matching exactly what `render_window` draws: the number column is sized to
/// the cross-buffer max, and the sign/fold columns are the stable reserved
/// widths. The explorer pane is gutterless (0). This is the single source of
/// truth shared with the mouse hit-test so clicks map to the right column even
/// when the gutter is widened beyond the buffer's own line-count width.
pub(crate) fn rendered_gutter_width(app: &App, win_id: window::WindowId) -> u16 {
    let Some(Some(win)) = app.windows.get(win_id) else {
        return 0;
    };
    let slot_idx = win.slot;
    let Some(slot) = app.slots().get(slot_idx) else {
        return 0;
    };
    if slot.is_explorer {
        return 0;
    }
    let (sign_w, fold_w) = stable_gutter_extra(app);
    // lnum_width depends on per-window `number`/`relativenumber` (#151 Phase D) —
    // read this window's editor so the mouse gutter matches render_window's.
    let own_lnum = app
        .window_editors
        .get(&win_id)
        .map(|e| e.lnum_width())
        .unwrap_or_else(|| slot.editor.lnum_width());
    let num_gw = if own_lnum > 0 { max_lnum_width(app) } else { 0 };
    sign_w + num_gw + fold_w
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

/// Re-pin the explorer window to a fixed column width by overriding the ratio of
/// its enclosing vertical split. Called each frame before `render_layout`, so the
/// sidebar holds a constant width as the terminal or sibling windows resize
/// (instead of scaling like a normal ratio split) and naturally resists
/// `<C-w>=` equalize. `width` is the column width currently available to `node`,
/// threaded down from the live frame area (NOT the stale `last_rect`, which would
/// lag one frame on resize and visibly jiggle).
fn pin_explorer_width(
    node: &mut window::LayoutTree,
    explorer_win: window::WindowId,
    width: u16,
    fixed: u16,
) {
    use window::{Axis, LayoutTree};
    let LayoutTree::Split {
        dir, ratio, a, b, ..
    } = node
    else {
        return;
    };
    let w = width.max(1);
    if dir.axis() == Axis::Col {
        let frac = (fixed as f32 / w as f32).clamp(0.05, 0.9);
        if matches!(a.as_ref(), LayoutTree::Leaf(id) if *id == explorer_win) {
            *ratio = frac;
            return;
        }
        if matches!(b.as_ref(), LayoutTree::Leaf(id) if *id == explorer_win) {
            *ratio = 1.0 - frac;
            return;
        }
    }
    // Recurse with each child's CURRENT width (mirrors `split_rect`): a vertical
    // split divides the width by ratio; a horizontal split gives both children
    // the full width.
    let (aw, bw) = match dir.axis() {
        Axis::Col => {
            let aw = ((w as f32) * *ratio).round() as u16;
            let aw = aw.clamp(1, w.saturating_sub(1).max(1));
            (aw, w.saturating_sub(aw))
        }
        Axis::Row => (w, w),
    };
    pin_explorer_width(a, explorer_win, aw, fixed);
    pin_explorer_width(b, explorer_win, bw, fixed);
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
    let (slot_idx, is_focused) = {
        let win = match app.windows[win_id].as_ref() {
            Some(w) => w,
            None => return, // closed window — skip
        };
        (win.slot, win_id == app.focused_window())
    };

    // 1-col left/right padding for the file list so it isn't flush against
    // the pane edges.
    let mut area = area;
    if app.slots()[slot_idx].is_explorer && area.width >= 2 {
        area.x += 1;
        area.width -= 2;
    }

    // Per-window state (#151 Phase D): settings, cursor, viewport, blame-view
    // come from THIS window's editor. Content + syntax spans + per-buffer
    // metadata (blame data, diag/git signs) stay on the slot editor (shared).
    let win_settings = app
        .window_editors
        .get(&win_id)
        .map(|e| e.settings().clone())
        .unwrap_or_else(|| app.slots()[slot_idx].editor.settings().clone());
    let s = &win_settings;
    let (w_cursor_row, w_is_blame) = app
        .window_editors
        .get(&win_id)
        .map(|e| (e.buffer().cursor().row, e.is_blame()))
        .unwrap_or_else(|| {
            let e = &app.slots()[slot_idx].editor;
            (e.buffer().cursor().row, e.is_blame())
        });
    let (nu, rnu) = (s.number, s.relativenumber);
    let (cul, cuc) = (s.cursorline, s.cursorcolumn);
    let colorcolumn = s.colorcolumn.clone();
    let list_active = s.list;
    let listchars_owned = s.listchars.clone();
    let indent_guides_enabled = s.indent_guides;
    let indent_guide_char = s.indent_guide_char;
    let indent_guide_shiftwidth = s.shiftwidth;
    let indent_guide_tabstop = s.tabstop;

    // Stable sign + fold columns: reserve the max width each would need across
    // ALL open buffers, so a diagnostic/git sign (or fold) appearing in one
    // buffer doesn't shift the text column when switching buffers or scrolling.
    // The explorer pane is gutterless.
    let is_explorer_slot = app.slots()[slot_idx].is_explorer;
    let (sign_w, fold_w) = if is_explorer_slot {
        (0, 0)
    } else {
        stable_gutter_extra(app)
    };
    // Stable line-number gutter: when this buffer shows numbers, size the column
    // to the widest needed across ALL open buffers (the biggest file's line
    // count) so switching buffers doesn't shift the text column horizontally.
    let own_lnum = app
        .window_editors
        .get(&win_id)
        .map(|e| e.lnum_width())
        .unwrap_or_else(|| app.slots()[slot_idx].editor.lnum_width());
    let num_gw_for_text = if own_lnum > 0 { max_lnum_width(app) } else { 0 };
    // Extra padding added to the number column beyond the buffer's own width —
    // folded into the cursor's gutter offset below so the caret stays aligned.
    let lnum_pad = num_gw_for_text.saturating_sub(own_lnum);
    let gw = sign_w + num_gw_for_text + fold_w;
    let text_width = area.width.saturating_sub(gw);

    // For the focused window: publish viewport dims into the engine so
    // scrolloff math and cursor-screen-pos work correctly.
    if is_focused {
        let tabstop = s.tabstop as u16;
        if let Some(e) = app.window_editors.get_mut(&win_id) {
            let vp = e.host_mut().viewport_mut();
            vp.width = text_width;
            vp.height = area.height;
            vp.text_width = text_width;
            vp.tab_width = tabstop;
            e.set_viewport_height(area.height);
        }
    }

    // Relative/hybrid line numbers count from THIS window's cursor row. The
    // focused window's editor cursor is authoritative and matches its saved
    // `cursor_row`; an unfocused window must use its own saved row so its
    // relative numbers don't count from the active window's cursor.
    let cursor_row = w_cursor_row;
    let numbers = match (nu, rnu) {
        (false, false) => GutterNumbers::None,
        (true, false) => GutterNumbers::Absolute,
        (false, true) => GutterNumbers::Relative { cursor_row },
        (true, true) => GutterNumbers::Hybrid { cursor_row },
    };
    // Number-column width only (sign/fold widths are tracked separately).
    // Uses the stable cross-buffer max computed above.
    let num_gw = num_gw_for_text;
    let gutter = if num_gw > 0 || sign_w > 0 || fold_w > 0 {
        Some(Gutter {
            width: num_gw,
            style: Style::default().fg(app.theme.ui.gutter),
            line_offset: 0,
            numbers,
            sign_column_width: sign_w,
            fold_column_width: fold_w,
        })
    } else {
        None
    };

    // Viewport for this window: focused uses editor's live viewport (with
    // auto-scroll applied); non-focused builds one from the window's own
    // stored scroll position so it doesn't chase the focused editor.
    // This window's viewport comes from its own editor (#151 Phase D): focused
    // windows get live auto-scrolled scroll; non-focused windows keep the scroll
    // they were left at (their editor isn't dispatched while unfocused).
    let mut viewport_owned = app
        .window_editors
        .get(&win_id)
        .map(|e| *e.host().viewport())
        .unwrap_or_default();
    viewport_owned.width = text_width;
    viewport_owned.height = area.height;
    viewport_owned.text_width = text_width;
    let viewport_ref: &Viewport = &viewport_owned;

    let in_prompt = app.command_field.is_some()
        || app.filter_field.is_some()
        || app.search_field.is_some()
        || app.picker.is_some()
        || app.explorer_git_discard_confirm.is_some();

    // Merge diagnostic + LSP diag + git signs, filtered to the visible viewport.
    let vp_top = app
        .scroll_anim_render_top(win_id)
        .unwrap_or(viewport_ref.top_row);
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

    // Visual selection belongs to the focused window only. The selection is
    // editor state shared by every window on the same slot, so an unfocused
    // split would otherwise paint the active window's selection too.
    let selection = if is_focused {
        app.window_editors
            .get(&win_id)
            .and_then(|e| e.buffer_selection())
    } else {
        None
    };
    let buffer_spans = app.slots()[slot_idx].editor.buffer_spans();
    let search_pattern_owned = app
        .window_editors
        .get(&win_id)
        .and_then(|e| e.search_state().pattern.clone());
    let search_pattern = search_pattern_owned.as_ref();

    let search_bg = if search_pattern.is_some() {
        Style::default()
            .bg(app.theme.ui.search_bg)
            .fg(app.theme.ui.search_fg)
    } else {
        Style::default()
    };

    let style_table = app.slots()[slot_idx].editor.style_table().to_owned();
    let resolver = move |id: u32| {
        hjkl_engine_tui::style_to_ratatui(style_table.get(id as usize).copied().unwrap_or_default())
    };

    // For non-focused windows, don't show cursor highlight or cursor position.
    let show_cursor = is_focused && !in_prompt;

    // Resolve cursorline / cursorcolumn styles for this window. The cursor line
    // stays visible on UNFOCUSED windows too (e.g. the explorer's selection when
    // focus is in the editor), painted with a fainter bg and without the cursor.
    let cursor_line_style = if cul {
        if show_cursor {
            cursor_line_bg(&app.theme.ui)
        } else {
            Style::default().bg(app.theme.ui.cursor_line_inactive_bg)
        }
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

    // Compute indent guide active column from the cursor row's leading whitespace.
    // The active column is the deepest guide column at the cursor's indent level:
    //   active_col = floor((leading_vcols - 1) / sw) * sw
    // Returns None when sw == 0 or the cursor row has no leading whitespace.
    // Only the focused window highlights its active indent column — it keys off
    // the shared editor cursor, so an unfocused window would otherwise track the
    // active window's cursor instead of staying put.
    let indent_guide_active_col: Option<usize> =
        if is_focused && indent_guides_enabled && indent_guide_shiftwidth > 0 {
            let cursor_row = w_cursor_row;
            let rope = app.slots()[slot_idx].editor.buffer().rope();
            let cursor_line = hjkl_buffer::rope_line_str(&rope, cursor_row);
            let tab_width = indent_guide_tabstop.max(1);
            let mut leading_vcols: usize = 0;
            for ch in cursor_line.chars() {
                match ch {
                    ' ' => leading_vcols += 1,
                    '\t' => {
                        leading_vcols += tab_width - (leading_vcols % tab_width);
                    }
                    _ => break,
                }
            }
            if leading_vcols >= indent_guide_shiftwidth {
                // paint_row paints guides at sw, 2*sw, ... while
                // `guide_col < leading_vcols`. Deepest painted is
                // ((L - 1) / sw) * sw.
                let level = (leading_vcols - 1) / indent_guide_shiftwidth;
                Some(level * indent_guide_shiftwidth)
            } else {
                None
            }
        } else {
            None
        };

    let diag_overlays = build_diag_overlays(&app.slots()[slot_idx], &app.theme.ui);

    // Inline end-of-line ghost text, rendered comment-style (`// …` in
    // Rust/JS, `# …` in Python, …) so it reads like a trailing comment, with
    // per-hint colors. Two sources:
    //   1. LSP diagnostics — `// message` in the severity color. Mode is
    //      `:set diagnostics_inline=off|current|all` (default `all`): `all`
    //      annotates every diagnostic line, `current` only the cursor line.
    //      One hint per line (most-severe diagnostic wins).
    //   2. Inline git blame — `// author · summary` dimmed on the cursor line,
    //      idle-gated so it doesn't flicker while moving (#202 P5). Shown only
    //      when the cursor line has no diagnostic hint.
    use hjkl_engine::types::DiagInlineMode;
    const BLAME_IDLE_DELAY: std::time::Duration = std::time::Duration::from_millis(400);
    use hjkl_buffer_tui::render::EolHint;
    let comment_lead = app.active_comment_lead();
    let eol_hints: Vec<EolHint> = {
        let slot = &app.slots()[slot_idx];
        let cursor_row = w_cursor_row;
        let diag_mode = s.diagnostics_inline;

        let mut hints: Vec<EolHint> = Vec::new();

        // 1. Diagnostics — pick the most-severe diagnostic per (start) line.
        if is_focused && diag_mode != DiagInlineMode::Off {
            let mut by_row: std::collections::HashMap<usize, (DiagSeverity, &str)> =
                std::collections::HashMap::new();
            for d in &slot.lsp_diags {
                if diag_mode == DiagInlineMode::Current && d.start_row != cursor_row {
                    continue;
                }
                let msg = d.message.lines().next().unwrap_or("");
                by_row
                    .entry(d.start_row)
                    .and_modify(|cur| {
                        if diag_severity_rank(d.severity) < diag_severity_rank(cur.0) {
                            *cur = (d.severity, msg);
                        }
                    })
                    .or_insert((d.severity, msg));
            }
            for (row, (severity, msg)) in by_row {
                hints.push(EolHint {
                    row,
                    text: format!("{comment_lead} {}", truncate_chars(msg, 80)),
                    style: Style::default()
                        .fg(diag_severity_fg(severity))
                        .add_modifier(Modifier::ITALIC),
                });
            }
        }

        // 2. Inline blame on the cursor line, unless a diagnostic already
        //    annotates it (errors take precedence over blame).
        let blame_show = s.blame_inline
            && !w_is_blame
            && is_focused
            && app.blame_cursor_moved_at.elapsed() >= BLAME_IDLE_DELAY
            && !hints.iter().any(|h| h.row == cursor_row);
        if blame_show && let Some(Some(info)) = slot.blame.get(cursor_row) {
            let body = if info.is_uncommitted {
                "You \u{00b7} Not Committed Yet".to_string()
            } else {
                format!(
                    "{} \u{00b7} {}",
                    info.author,
                    truncate_chars(&info.summary, 50)
                )
            };
            hints.push(EolHint {
                row: cursor_row,
                text: format!("{comment_lead} {body}"),
                style: Style::default().fg(app.theme.ui.non_text),
            });
        }

        hints
    };

    // Boxed-blame layout: when the blame view is on (Wrap::None), build a
    // render plan that frames each commit run in a box (titled top border,
    // `│` sides, bottom border). The engine's cursor/scroll stays authoritative
    // — this only changes how the viewport is painted.
    let box_mode = w_is_blame && matches!(viewport_owned.wrap, hjkl_buffer::Wrap::None);
    let blame_box_plan: Vec<hjkl_buffer_tui::render::BlameRow> = if box_mode {
        let s = &app.slots()[slot_idx];
        let vp_top = viewport_owned.top_row;
        let line_count = s.editor.buffer().line_count() as usize;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let buf = s.editor.buffer();
        crate::app::git_hunks::build_blame_box_plan(
            &s.blame,
            line_count,
            |r| buf.is_row_hidden(r),
            vp_top,
            area.height as usize,
            now,
        )
    } else {
        Vec::new()
    };

    // ── Explorer conceals: hide the <US><id> tail on each non-root line ─────
    //
    // When this slot is the explorer and debug_mode is off, build one Conceal
    // per buffer line that contains a Unit Separator (U+001F). The conceal
    // covers [byte of US .. end of line] with an empty replacement so the tail
    // is invisible. The buffer text itself is unchanged; only the rendered cells
    // differ. In debug_mode the raw ids are shown for diagnostics.
    let explorer_conceals: Vec<hjkl_buffer_tui::Conceal> = if is_explorer_slot && !app.debug_mode {
        use crate::app::explorer_reconcile::ID_SEP;
        let buf_text = app.slots()[slot_idx].editor.buffer().as_string();
        buf_text
            .lines()
            .enumerate()
            .filter_map(|(row, line)| {
                let us_byte = line.find(ID_SEP)?;
                Some(hjkl_buffer_tui::Conceal {
                    row,
                    start_byte: us_byte,
                    end_byte: line.len(),
                    replacement: String::new(),
                })
            })
            .collect()
    } else {
        Vec::new()
    };

    // Diff-mode filler plan (#250): blank rows that keep the two diff windows
    // aligned. Built once and reused by the renderer, the cursor placement, and
    // the diff-band overlay so all three agree on screen rows.
    let diff_filler_plan = app.diff_filler_plan(win_id);

    let view = BufferView {
        buffer: app.slots()[slot_idx].editor.buffer(),
        viewport: viewport_ref,
        selection,
        resolver: &resolver,
        cursor_line_bg: cursor_line_style,
        // Unfocused windows paint the cursorline on their OWN saved cursor row
        // (the per-window `cursor_row`), not the shared editor cursor — so the
        // ghost line stays put when another window on the same buffer moves.
        // Focused window: `None` defers to the live editor cursor.
        cursor_line_row: if is_focused {
            None
        } else {
            Some(app.window_cursor(win_id).0)
        },
        // The explorer is a tree, not code — don't tint folded directory rows.
        fold_line_bg: if is_explorer_slot {
            Style::default()
        } else {
            Style::default().bg(app.theme.ui.fold_line_bg)
        },
        // Unfocused windows render their OWN fold snapshot (window-level folds);
        // the shared buffer holds only the focused window's set. Focused window:
        // `None` reads the live `buffer.folds()`.
        folds_override: if is_focused {
            None
        } else {
            app.window_folds.get(&win_id).map(Vec::as_slice)
        },
        cursor_column_bg: cursor_column_style,
        selection_bg: Style::default().bg(Color::Blue),
        cursor_style: Style::default(),
        gutter,
        search_bg,
        signs: &visible_signs,
        conceals: &explorer_conceals,
        spans: buffer_spans,
        search_pattern,
        non_text_style: Style::default().fg(app.theme.ui.non_text),
        show_eob: app.slots()[slot_idx].features.end_of_buffer,
        diag_overlays: &diag_overlays,
        colorcolumn_cols: &cc_cols,
        colorcolumn_style: cc_style,
        listchars: if list_active {
            Some(&listchars_owned)
        } else {
            None
        },
        indent_guides_enabled,
        indent_guide_char,
        indent_guide_shiftwidth,
        indent_guide_fg: app.theme.ui.indent_guide_fg,
        indent_guide_active_fg: app.theme.ui.indent_guide_active_fg,
        indent_guide_active_col,
        // Inline EOL ghost text (cursor line): diagnostics + git blame.
        eol_hints: &eol_hints,
        blame_plan: if box_mode {
            Some(&blame_box_plan)
        } else {
            None
        },
        diff_filler: diff_filler_plan.as_ref(),
    };
    frame.render_widget(view, area);

    // ── Explorer devicon + git-status color overlay ───────────────────────
    //
    // Post-render cell walk applied only to the window matching the explorer
    // pane's win_id so multiple windows on the same buffer are not
    // double-painted.
    //
    // The buffer now contains only indentation spaces + bare names (no glyphs).
    // This pass does two things in one loop:
    //   1. PAINT GLYPHS — write the tree-guide, connector, and icon characters
    //      into the leading blank cells that `render_text` left empty.
    //   2. PAINT COLORS — repaint the icon cell with a devicon/dir RGB fg and
    //      every name cell with the git-status color (or devicon fg for clean).
    //
    // Column layout (depth-0 root / depth ≥ 1):
    //   depth 0 : col 0 = icon, col 1 = space, col 2.. = name
    //   depth ≥ 1:
    //     col i*2        (i in 0..branches.len()) = '│' or space  (guide)
    //     col i*2+1                                = space
    //     col (depth-1)*2   = '└' or '├'          (connector)
    //     col (depth-1)*2+1 = '╴'
    //     col depth*2       = icon
    //     col depth*2+1     = space
    //     col depth*2+2..   = name
    // `:debug` mode renders the explorer as its RAW buffer text (no glyph /
    // guide / git-color overlay) so the actual on-disk buffer contents are
    // visible for debugging.
    if is_explorer_slot
        && !app.debug_mode
        && let Some(ref pane) = app.explorer
        && pane.win_id == win_id
    {
        let buf = frame.buffer_mut();
        let screen_top = area.y;
        let screen_height = area.height as usize;
        // Explorer is gutterless: text starts at area.x (sign_w=fold_w=num_gw=0).
        let text_x = area.x;

        // The viewport top row for this window (same variable as the BufferView uses).
        let vp_top_ex = vp_top;

        // Dim color used for tree guides and connectors.
        let guide_fg = app.theme.ui.indent_guide_fg;

        // Build the fold list + live buffer text once. The fold list MUST match
        // what `BufferView` rendered with: a FOCUSED window draws from the live
        // `buffer.folds()`, an UNFOCUSED one from its `window_folds` snapshot
        // (window-level folds). Reading the wrong source here desyncs the glyph
        // overlay from the drawn rows and garbles the tree.
        let (explorer_folds, buf_text): (Vec<hjkl_buffer::Fold>, String) =
            if let Some(slot_idx) = app.slots().iter().position(|s| s.is_explorer) {
                let b = app.slots()[slot_idx].editor.buffer();
                let folds = if is_focused {
                    b.folds()
                } else {
                    app.window_folds
                        .get(&win_id)
                        .cloned()
                        .unwrap_or_else(|| b.folds())
                };
                (folds, b.as_string())
            } else {
                (Vec::new(), String::new())
            };

        // Layout (icons, guides, depth) is derived from the LIVE buffer text —
        // NOT the last-reconciled `pane.tree.nodes` — so glyphs stay aligned
        // while the buffer is being edited (a mid-edit `o`/`O` shifts rows
        // before the Normal-mode reconcile rebuilds the tree). `None` slots are
        // blank lines (e.g. a fresh open-line awaiting a name).
        let overlay_nodes =
            crate::app::explorer::overlay_nodes_from_buffer(&buf_text, &pane.tree.root);

        // Git status follows the PATH, not the row: resolve each row's color
        // from the reconciled tree's status map (which carries rollup + the
        // dirty-buffer overlay). New / renamed paths absent from the map render
        // clean until the next reconcile.
        let git_map: std::collections::HashMap<&std::path::Path, hjkl_app::git::ExplorerGit> = pane
            .tree
            .nodes
            .iter()
            .filter_map(|n| n.git.map(|g| (n.path.as_path(), g)))
            .collect();

        // Walk doc rows starting at vp_top, skipping hidden rows, collecting
        // up to `screen_height` visible rows — mirroring BufferView's render
        // loop (crates/hjkl-buffer-tui/src/render.rs) which skips
        // rows where any fold f.hides(row).
        let total_nodes = overlay_nodes.len();
        let mut doc_row = vp_top_ex;
        let mut screen_row_idx: usize = 0;

        while doc_row < total_nodes && screen_row_idx < screen_height {
            // Skip rows hidden by a closed fold.
            if explorer_folds.iter().any(|f| f.hides(doc_row)) {
                doc_row += 1;
                continue;
            }

            let buf_row = doc_row;
            let screen_row = screen_top + screen_row_idx as u16;
            if screen_row >= screen_top + area.height {
                break;
            }
            screen_row_idx += 1;
            doc_row += 1;

            let Some(node) = overlay_nodes.get(buf_row).and_then(|o| o.as_ref()) else {
                continue;
            };
            // Git status by path (overlay parse leaves `node.git` = None).
            let node_git = git_map.get(node.path.as_path()).copied();

            let right = area.x + area.width;

            // ── Glyph painting ────────────────────────────────────────────

            // Drive the dir open/closed icon from the tree's `expanded` set. The
            // lazy explorer has no buffer folds — a collapsed dir's children are
            // simply absent — so the icon reads expansion state directly.
            let is_expanded = app
                .explorer
                .as_ref()
                .map(|ep| ep.tree.is_expanded(node.path.as_path()))
                .unwrap_or(false);

            // Icon character for this node.
            let icon_ch = if node.is_dir {
                hjkl_icons::dir_icon_for_path(&node.path, is_expanded, app.icon_mode)
            } else {
                hjkl_icons::file_icon_for_path(&node.path, app.icon_mode)
            };

            if node.depth == 0 {
                // Root: icon at col 0, space at col 1.
                let icon_abs = text_x;
                if icon_abs < right
                    && let Some(cell) = buf.cell_mut((icon_abs, screen_row))
                {
                    cell.set_symbol(&icon_ch.to_string());
                }
                // col 1 is already space from the buffer — nothing to write.
            } else {
                // Guide columns: for each ancestor level.
                for (i, &has_sibling) in node.branches.iter().enumerate() {
                    let col = text_x + (i as u16) * 2;
                    if col < right
                        && let Some(cell) = buf.cell_mut((col, screen_row))
                    {
                        let sym = if has_sibling { "│" } else { " " };
                        cell.set_symbol(sym);
                        cell.set_fg(guide_fg);
                    }
                    // col+1 stays space — leave it.
                }

                // Connector: at col (depth-1)*2 and (depth-1)*2+1.
                let connector_col = text_x + (node.branches.len() as u16) * 2;
                if connector_col < right
                    && let Some(cell) = buf.cell_mut((connector_col, screen_row))
                {
                    let sym = if node.is_last { "└" } else { "├" };
                    cell.set_symbol(sym);
                    cell.set_fg(guide_fg);
                }
                let connector_col2 = connector_col + 1;
                if connector_col2 < right
                    && let Some(cell) = buf.cell_mut((connector_col2, screen_row))
                {
                    cell.set_symbol("╴");
                    cell.set_fg(guide_fg);
                }

                // Icon: at col depth*2 (= name_col - 2).
                let icon_abs = text_x + node.depth as u16 * 2;
                if icon_abs < right
                    && let Some(cell) = buf.cell_mut((icon_abs, screen_row))
                {
                    cell.set_symbol(&icon_ch.to_string());
                }
                // col depth*2+1 is the space between icon and name — leave it.
            }

            // ── Color painting ────────────────────────────────────────────

            // Compute icon and name screen-column offsets.
            // Layout (same as what render_text used to emit):
            //   depth-0 (root): icon(1) + space(1) + name(...)
            //   depth>0: branches*(2) + connector(2) + icon(1) + space(1) + name(...)
            let (icon_col, name_col) = if node.depth == 0 {
                (text_x, text_x + 2)
            } else {
                let guide_cols = node.branches.len() as u16 * 2;
                let icon_off = guide_cols + 2; // +2 for the connector (e.g. `├╴`)
                (text_x + icon_off, text_x + icon_off + 2)
            };

            // Icon color: devicon per-filetype or folder blue. Falls back to the
            // theme text color so the icon ALWAYS gets a definite fg — otherwise
            // a colorless devicon (e.g. a generic file) would inherit whatever
            // the cursorline painted underneath, making the icon change color
            // depending on which row the cursor sits on.
            let icon_color = if node.is_dir {
                let name = node.path.file_name().and_then(|n| n.to_str());
                rgb(hjkl_icons::dir_color(name))
            } else {
                rgb(hjkl_icons::file_color_for_path(&node.path))
            }
            .unwrap_or(app.theme.ui.text);

            // Git status → name BACKGROUND (so the filetype/dir foreground stays
            // distinct). Clean nodes keep their devicon/dir foreground and the
            // normal background.
            let git_bg = match node_git {
                Some(hjkl_app::git::ExplorerGit::Modified) => Some(GIT_MODIFIED),
                Some(hjkl_app::git::ExplorerGit::Staged) => Some(GIT_STAGED),
                Some(hjkl_app::git::ExplorerGit::Deleted) => Some(GIT_DELETED),
                Some(hjkl_app::git::ExplorerGit::Untracked) => Some(GIT_UNTRACKED),
                None => None,
            };
            // Foreground for the name when clean: devicon (files) / folder (dirs),
            // falling back to the theme text color.
            let clean_name_fg = if node.is_dir {
                rgb(hjkl_icons::dir_color(
                    node.path.file_name().and_then(|n| n.to_str()),
                ))
            } else {
                rgb(hjkl_icons::file_color_for_path(&node.path))
            }
            .unwrap_or(app.theme.ui.text);

            // Repaint the icon cell foreground (icon keeps its filetype/dir color
            // regardless of git state — the icon column never gets a git bg).
            // Always set a definite fg so the icon never inherits the
            // cursorline's foreground on the cursor row.
            if icon_col < right
                && let Some(cell) = buf.cell_mut((icon_col, screen_row))
            {
                cell.set_fg(icon_color);
            }

            // Name span width = the ACTUAL displayed name on this row: the live
            // line text after the indent, up to the concealed `<US><id>` tail.
            // Deriving it from the buffer (not `path.file_name()`) keeps the
            // git-bg / clean-color highlight tight AND correct mid-edit when the
            // typed name contains a `/` (e.g. `somedir/test.txt`) — there
            // `file_name()` would be `test.txt` and only cover part of the name,
            // leaving the rest mis-colored. The trailing `/` on a dir name is
            // part of the line text, so it's counted naturally.
            let indent_chars = name_col.saturating_sub(text_x) as usize;
            let name_len = buf_text
                .lines()
                .nth(buf_row)
                .map(|line| {
                    line.chars()
                        .skip(indent_chars)
                        .take_while(|&c| c != '\u{1F}')
                        .count() as u16
                })
                .unwrap_or(0);
            let name_end = name_col.saturating_add(name_len).min(right);

            // Repaint name cells.
            for col in name_col..name_end {
                if let Some(cell) = buf.cell_mut((col, screen_row)) {
                    match git_bg {
                        Some(bg) => {
                            cell.set_bg(bg);
                            cell.set_fg(GIT_TEXT);
                        }
                        None => {
                            cell.set_fg(clean_name_fg);
                        }
                    }
                }
            }
        }
    }

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
        let text_x = area.x + sign_w + num_gw + fold_w;
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

    // ── Diff-mode overlay (#208 Phase 2) ──────────────────────────────────
    // Post-render pass: tint changed/added lines (DiffChange/DiffAdd) and the
    // changed character ranges within them (DiffText), for both windows of an
    // active diff pair. Colors are hardcoded (vim-like dark palette) pending a
    // theme promotion. Filler-line alignment is a separate concern.
    if app.is_diff_window(win_id) {
        let classes = app.diff_line_classes(win_id);
        if !classes.is_empty() {
            // Vim-like dark diff palette (bg only; syntax fg is preserved).
            let add_bg = Color::Rgb(32, 51, 32); // DiffAdd
            let change_bg = Color::Rgb(32, 42, 60); // DiffChange
            let text_bg = Color::Rgb(48, 78, 110); // DiffText (stronger)
            let text_x = area.x + sign_w + num_gw + fold_w;
            let text_right = area.x + area.width;
            let screen_top = area.y;
            let screen_height = area.height as usize;
            let buf = frame.buffer_mut();
            for (&line, class) in &classes {
                if line < vp_top {
                    continue;
                }
                // Account for filler rows inserted above this line so the band
                // lands on the same screen row the renderer painted it.
                let off = match diff_filler_plan.as_ref() {
                    Some(p) => p.screen_offset(vp_top, line),
                    None => line - vp_top,
                };
                if off >= screen_height {
                    continue;
                }
                let screen_row = screen_top + off as u16;
                let band_bg = match class.band {
                    crate::app::diff_mode::DiffBand::Add => add_bg,
                    crate::app::diff_mode::DiffBand::Change => change_bg,
                };
                for col in text_x..text_right {
                    if let Some(c) = buf.cell_mut((col, screen_row)) {
                        c.set_style(c.style().patch(Style::default().bg(band_bg)));
                    }
                }
                // Char-level DiffText over the changed byte ranges.
                if !class.text_ranges.is_empty() {
                    let rope = hjkl_engine::Query::rope(app.slots()[slot_idx].editor.buffer());
                    let lt = hjkl_buffer::rope_line_str(&rope, line);
                    let lt = lt.trim_end_matches('\n');
                    let len = lt.len();
                    for r in &class.text_ranges {
                        let start = r.start.min(len);
                        let end = r.end.min(len);
                        let cs = lt[..start].chars().count();
                        let ce = lt[..end].chars().count();
                        for ci in cs..ce {
                            let scol = text_x + ci as u16;
                            if scol >= text_right {
                                break;
                            }
                            if let Some(c) = buf.cell_mut((scol, screen_row)) {
                                c.set_style(c.style().patch(Style::default().bg(text_bg)));
                            }
                        }
                    }
                }
            }
        }
    }

    // ── Confirm-substitute active-match highlight ─────────────────────────
    // Paint inverse-video (search highlight) over the cell range of the
    // match currently being prompted. Only for the focused window so
    // inactive splits don't flash confusingly.
    if is_focused
        && let Some(cs) = app.confirming_substitute.as_ref()
        && cs.idx < cs.matches.len()
    {
        let m = &cs.matches[cs.idx];
        let match_row = m.row as usize;
        // Only paint when the row is visible in this window.
        let vp_bot = vp_top + area.height as usize;
        if match_row >= vp_top && match_row < vp_bot {
            let screen_row = area.y + (match_row - vp_top) as u16;
            // Convert byte offsets to char columns for rendering.
            let rope = hjkl_engine::Query::rope(app.slots()[slot_idx].editor.buffer());
            let line = hjkl_buffer::rope_line_str(&rope, match_row);
            let line_no_nl = line.trim_end_matches('\n');
            let char_start = line_no_nl[..m.byte_start as usize].chars().count();
            let char_end = line_no_nl[..m.byte_end as usize].chars().count();
            let text_x = area.x + sign_w + num_gw + fold_w;
            let highlight_style = Style::default()
                .bg(app.theme.ui.search_bg)
                .fg(app.theme.ui.search_fg)
                .add_modifier(Modifier::REVERSED);
            let buf = frame.buffer_mut();
            for ch_idx in char_start..char_end {
                let screen_col = text_x + ch_idx as u16;
                if screen_col >= area.x + area.width {
                    break;
                }
                if let Some(cell) = buf.cell_mut((screen_col, screen_row)) {
                    cell.set_style(cell.style().patch(highlight_style));
                }
            }
        }
    }

    // Map a doc `(row, col)` to a terminal cell for post-render overlays,
    // box-aware: the boxed-blame layout shifts the screen row (virtual border
    // rows) and the column (+1 box frame). `None` when the row isn't visible.
    let base_text_x = area.x + sign_w + num_gw + fold_w;
    let vp_bot_overlay = vp_top + area.height as usize;
    let map_doc_to_screen = |pair_row: usize, pair_col: usize| -> Option<(u16, u16)> {
        if box_mode {
            let idx = blame_box_plan.iter().position(
                |r| matches!(r, hjkl_buffer_tui::render::BlameRow::Content(d) if *d == pair_row),
            )?;
            Some((
                base_text_x + hjkl_buffer_tui::render::BLAME_BOX_FRAME_LEFT + pair_col as u16,
                area.y + idx as u16,
            ))
        } else if pair_row >= vp_top && pair_row < vp_bot_overlay {
            Some((
                base_text_x + pair_col as u16,
                area.y + (pair_row - vp_top) as u16,
            ))
        } else {
            None
        }
    };

    // ── matchparen bracket highlight (focused window only) ─────────────────
    if is_focused && let Some(pairs) = app.matchparen_cells() {
        let match_paren_style = Style::default()
            .bg(app.theme.ui.match_paren_bg)
            .add_modifier(Modifier::BOLD | Modifier::REVERSED);
        let right = area.x + area.width;
        let buf = frame.buffer_mut();
        for (pair_row, pair_col) in pairs {
            if let Some((screen_col, screen_row)) = map_doc_to_screen(pair_row, pair_col)
                && screen_col < right
                && let Some(cell) = buf.cell_mut((screen_col, screen_row))
            {
                cell.set_style(cell.style().patch(match_paren_style));
            }
        }
    }

    // ── matchparen tag highlight ──────────────────────────────────────────
    if is_focused && let Some(tag_cells) = app.matchparen_tag_cells() {
        let match_paren_style = Style::default()
            .bg(app.theme.ui.match_paren_bg)
            .add_modifier(Modifier::BOLD | Modifier::REVERSED);
        let right = area.x + area.width;
        let buf = frame.buffer_mut();
        for (pair_row, pair_col) in tag_cells {
            if let Some((screen_col, screen_row)) = map_doc_to_screen(pair_row, pair_col)
                && screen_col < right
                && let Some(cell) = buf.cell_mut((screen_col, screen_row))
            {
                cell.set_style(cell.style().patch(match_paren_style));
            }
        }
    }

    // ── Hop / easymotion label overlay (#197) ────────────────────────────
    // Paint each hop target's label string starting at the target's screen cell.
    // Only rendered for the window that owns the active hop state.
    if let Some(hop) = app.hop.as_ref()
        && hop.win_id == win_id
    {
        let label_style = Style::default()
            .fg(app.theme.ui.hop_label_fg)
            .bg(app.theme.ui.hop_label_bg)
            .add_modifier(Modifier::BOLD);
        let right = area.x + area.width;
        let bottom = area.y + area.height;
        // Clone the snapshot so we can call frame.buffer_mut() below.
        let targets: Vec<(usize, usize, String)> = hop
            .targets
            .iter()
            .map(|t| (t.row, t.col, t.label.clone()))
            .collect();
        let buf = frame.buffer_mut();
        for (doc_row, doc_col, label) in &targets {
            if let Some((screen_col, screen_row)) = map_doc_to_screen(*doc_row, *doc_col)
                && screen_col < right
                && screen_row < bottom
            {
                // Paint each char of the label into consecutive cells.
                for (i, ch) in label.chars().enumerate() {
                    let x = screen_col + i as u16;
                    if x >= right {
                        break;
                    }
                    if let Some(cell) = buf.cell_mut((x, screen_row)) {
                        cell.set_char(ch);
                        cell.set_style(label_style);
                    }
                }
            }
        }
    }

    // Emit the terminal cursor only for the focused window.
    // extra_gutter_width = sign_w + fold_w + lnum_pad. The engine computes the
    // cursor x from the buffer's OWN line-number width; `lnum_pad` accounts for
    // the extra cells when the number column is widened to the cross-buffer max.
    // Cursor rendering. The BLOCK (normal/visual) and UNDERLINE shapes are drawn
    // as a SOFTWARE cursor — a styled cell — because the terminal's hardware
    // cursor trails/flickers during scroll redraws (ratatui flushes the cell
    // diff BEFORE repositioning it), and held-`j`/`k` scrolling happens in those
    // modes. Both keep the underlying character visible (reversed / underlined).
    //
    // The BAR (insert) shape can't be a software cursor: a thin bar AND the
    // character can't share one cell, so painting `▏` would erase the char under
    // the cursor. Insert mode uses the real HARDWARE bar instead (positioned via
    // `set_cursor_position`), which renders between cells and leaves the char
    // intact. Insert mode has no held-scroll, so the hardware cursor doesn't
    // trail there. The command/search prompt likewise keeps the hardware cursor.
    if show_cursor
        && let Some((cx, cy)) = app.active_editor_mut().cursor_screen_pos(
            area.x,
            area.y,
            area.width,
            area.height,
            sign_w + fold_w + lnum_pad,
        )
    {
        // Boxed-blame: the cursor's screen row comes from the plan (borders
        // shift content down); the column shifts right by the box frame.
        // `None` when the cursor row scrolled past the plan's last row.
        let pos = if box_mode {
            let cur = w_cursor_row;
            blame_box_plan
                .iter()
                .position(
                    |r| matches!(r, hjkl_buffer_tui::render::BlameRow::Content(d) if *d == cur),
                )
                .map(|idx| {
                    (
                        cx + hjkl_buffer_tui::render::BLAME_BOX_FRAME_LEFT,
                        area.y + idx as u16,
                    )
                })
        } else if let Some(p) = diff_filler_plan.as_ref() {
            // Diff filler rows above the cursor shift it down by that many rows.
            let cur = w_cursor_row;
            if cur >= vp_top {
                let off = p.screen_offset(vp_top, cur);
                if (off as u16) < area.height {
                    Some((cx, area.y + off as u16))
                } else {
                    None
                }
            } else {
                Some((cx, cy))
            }
        } else {
            Some((cx, cy))
        };
        if let Some((cx, cy)) = pos {
            let shape = app.active_editor().host().cursor_shape();
            match shape {
                hjkl_engine::CursorShape::Bar => {
                    // Hardware bar — keeps the char under the cursor visible.
                    frame.set_cursor_position((cx, cy));
                }
                hjkl_engine::CursorShape::Block => {
                    if let Some(cell) = frame.buffer_mut().cell_mut((cx, cy)) {
                        cell.set_style(Style::default().add_modifier(Modifier::REVERSED));
                    }
                }
                hjkl_engine::CursorShape::Underline => {
                    if let Some(cell) = frame.buffer_mut().cell_mut((cx, cy)) {
                        cell.set_style(Style::default().add_modifier(Modifier::UNDERLINED));
                    }
                }
            }
        }
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

    // Convert buffer coordinates to screen coordinates. Anchor relative to the
    // FOCUSED window's rect (not `buf_area`) so the popup lands correctly when
    // the window is offset — e.g. the editor sits right of the explorer sidebar.
    let fw = app.focused_window();
    let slot_idx = app.windows[fw].as_ref().map(|w| w.slot).unwrap_or(0);
    let win_rect = app.windows[fw].as_ref().and_then(|w| w.last_rect);
    let (base_x, base_y) = win_rect
        .map(|r| (r.x, r.y))
        .unwrap_or((buf_area.x, buf_area.y));
    let vp = app.slots()[slot_idx].editor.host().viewport();
    let vp_top = vp.top_row;
    // Stable cross-buffer gutter width (matches render_window) so the popup
    // anchors under the cursor even when the number/sign/fold columns are
    // widened to the open-buffer max.
    let own_lnum = app.slots()[slot_idx].editor.lnum_width();
    let eff_lnum = if own_lnum > 0 { max_lnum_width(app) } else { 0 };
    let (sign_w, fold_w) = stable_gutter_extra(app);
    let gw = eff_lnum + sign_w + fold_w;

    // Cursor cell in absolute screen coordinates (0-based row relative to viewport top).
    let cursor_row = completion.anchor_row.saturating_sub(vp_top) as u16;
    let cursor_col = completion.anchor_col as u16 + gw;
    let anchor = Rect {
        x: base_x + cursor_col,
        y: base_y + cursor_row,
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

pub fn frame(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    // Explorer slots don't count as additional user buffers for the top-bar
    // visibility decision — otherwise opening the explorer alone would show the bar.
    let real_slots = app.slots().iter().filter(|s| !s.is_explorer).count();
    let show_top_bar = app.tabs.len() > 1 || real_slots > 1;
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

    // Spans are kept current by the event loop's end-of-drain flush
    // (`pending_recompute` → `recompute_and_install`). App::new seeds
    // `pending_recompute = true` so the first frame's flush handles the
    // initial parse. Calling recompute here every frame meant TWO sync
    // tree-sitter parses per draw — visible as ~half of per-keystroke
    // CPU on huge files.

    if let Some(tb) = top_bar_area {
        top_bar(frame, app, tb);
    }

    // Walk the window tree and render each pane. Use take_layout /
    // restore_layout so we can pass `&mut LayoutTree` to render_layout
    // (which writes last_rect on Split nodes) while also holding
    // `&mut App` for render_window.
    let mut layout = app.take_layout();
    // Keep the explorer sidebar a fixed column width across resizes.
    if let Some(explorer_win) = app.explorer.as_ref().map(|e| e.win_id) {
        pin_explorer_width(
            &mut layout,
            explorer_win,
            buf_area.width,
            crate::app::explorer::EXPLORER_WINDOW_WIDTH,
        );
    }
    render_layout(frame, app, buf_area, &mut layout);
    app.restore_layout(layout);

    status_line(frame, app, status_area);

    // Picker overlay sits on top of the buffer pane. Renders last so
    // its `Clear` widget masks the editor content beneath it.
    if app.picker.is_some() {
        picker_overlay(frame, app, buf_area);
    }

    // Completion popup: command-bar popup anchored above the status line;
    // LSP/buffer popup anchored at the buffer cursor.
    if app.completion.is_some() {
        if app.command_field.is_some() {
            command_completion_popup(frame, app, status_area, buf_area);
        } else {
            completion_popup(frame, app, buf_area);
        }
    }

    // Which-key popup: shown after a prefix key idles past the configured delay.
    if app.which_key_active {
        which_key_popup(frame, app, buf_area);
    }

    // Quickfix / location-list popup (`:copen` / `:lopen`, #184) — bottom pane.
    if app.quickfix_open || app.loclist_open {
        quickfix_popup_overlay(frame, app, buf_area);
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
    let inactive_style = Style::default().fg(ui.text_dim).bg(ui.surface_bg);
    let sep_style = Style::default().fg(ui.border);

    let show_tabs = app.tabs.len() > 1;
    // Count only real (non-explorer) slots for the buffer-line visibility check.
    let real_slot_count = app.slots().iter().filter(|s| !s.is_explorer).count();
    let show_buffers = real_slot_count > 1;
    let total_width = area.width as usize;

    // ── Right side: build a hjkl_tabs::TabBar from the layout tabs ──────────
    //
    // Each `hjkl_layout::Tab` (a window-split tree tab) maps to one
    // `hjkl_tabs::Tab<usize>` where the id is the positional index.
    // The title is `"{n}: {filename}"` matching the pre-migration format.
    // Dirty = any leaf window in the layout has a dirty slot.
    let mut tab_bar: TabBar<usize> = TabBar::new();
    if show_tabs {
        // `:set tabline_icons` (default on) — Nerd-Font filetype icon per tab.
        let tabline_icons = app.active_editor().settings().tabline_icons;
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
            // Prepend a filetype icon (generic glyph when the buffer is unnamed
            // or the extension is unknown). Gated on the `tabline_icons` toggle.
            let icon_part = if tabline_icons {
                let ic = match slot.filename.as_deref() {
                    Some(p) => hjkl_icons::file_icon_for_path(p, app.icon_mode),
                    None => hjkl_icons::file_icon(None, app.icon_mode),
                };
                format!("{ic} ")
            } else {
                String::new()
            };
            let title = format!(
                "{}: {}{} {}",
                i + 1,
                icon_part,
                base_name,
                crate::app::TAB_CLOSE_GLYPH
            );
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
        ui.surface_bg,
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
        let mut first = true;
        for (i, slot) in app.slots().iter().enumerate() {
            // Skip the explorer scratch buffer — it's a real window but not a
            // user-visible named buffer, so it must not appear in the buffer line.
            if slot.is_explorer {
                continue;
            }
            let base_name = slot
                .filename
                .as_ref()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("[No Name]");
            let label = if slot.dirty {
                format!(" {}+ {} ", base_name, crate::app::TAB_CLOSE_GLYPH)
            } else {
                format!(" {} {} ", base_name, crate::app::TAB_CLOSE_GLYPH)
            };
            let sep_len = if first { 0 } else { 1 };
            let entry_width = sep_len + label.chars().count();

            if buf_used + entry_width > buf_budget {
                // Truncate with ellipsis if space remains.
                if buf_used < buf_budget {
                    buf_spans.push(Span::styled("…".to_string(), sep_style));
                }
                break;
            }

            if !first {
                buf_spans.push(Span::styled("│".to_string(), sep_style));
            }
            let style = if i == app.active_index() {
                active_style
            } else {
                inactive_style
            };
            buf_spans.push(Span::styled(label, style));
            buf_used += entry_width;
            first = false;
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

/// Render the completion popup anchored above the command-line row.
/// Called when both `app.command_field` and `app.completion` are `Some`.
/// The anchor rect is placed at `status_area.y` so the popup flips above it.
fn command_completion_popup(frame: &mut Frame, app: &App, status_area: Rect, viewport: Rect) {
    let completion = match app.completion.as_ref() {
        Some(p) => p,
        None => return,
    };
    // Anchor at the cursor column in the status line. The cursor column is
    // 1 (for the `:` prefix) + field cursor col.
    let cursor_col: u16 = if let Some(ref field) = app.command_field {
        let (_, col) = field.cursor();
        1u16 + col as u16
    } else {
        1
    };
    let anchor = Rect {
        x: status_area.x + cursor_col.min(status_area.width.saturating_sub(1)),
        y: status_area.y,
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
    // Use full frame area as viewport so popup can extend into buf_area above.
    hjkl_completion_tui::popup(frame, completion, &theme, anchor, viewport);
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
///
/// Result is memoised on `App::search_count_cache` keyed by
/// `(buffer_id, dirty_gen, cursor, pattern_text)` — the status line
/// re-runs this every render, but the scan only re-runs when one of
/// those changes. On a cache miss the scan walks
/// `Buffer::content_joined` (cached `Arc<String>`) once with regex's
/// SIMD-fast `find_iter`, translating each match position back to its
/// row via the cumulative newline count instead of cloning per-line
/// `Vec<String>`s.
pub(crate) fn search_count(app: &App) -> Option<(usize, usize)> {
    const MATCH_CAP: usize = 10_000;
    let st = app.active_editor().search_state();
    let pat = st.pattern.as_ref()?;
    let pattern_str = pat.as_str();

    let buf = app.active_editor().buffer();
    let buffer_id = app.active().buffer_id;
    let dirty_gen = buf.dirty_gen();
    let cursor = app.active_editor().cursor();

    // Cache hit — return immediately.
    if let Some(cached) = app.search_count_cache.borrow().as_ref()
        && cached.buffer_id == buffer_id
        && cached.dirty_gen == dirty_gen
        && cached.cursor == cursor
        && cached.pattern == pattern_str
    {
        return cached.result;
    }

    let (cursor_row, cursor_col) = cursor;

    // `cursor_col` is a char index; regex match offsets are bytes.
    // Convert cursor's column to a byte offset within its own line.
    let cursor_byte_in_row = {
        let rope = buf.rope();
        if cursor_row < rope.len_lines() {
            let line = hjkl_buffer::rope_line_str(&rope, cursor_row);
            line.char_indices()
                .nth(cursor_col)
                .map(|(b, _)| b)
                .unwrap_or(line.len())
        } else {
            0
        }
    };

    // Single shared `Arc<String>` of the whole document — cached against
    // `dirty_gen`, so calling content_joined here costs an `Arc::clone`.
    let content = buf.content_joined();

    // O(log N) rope lookup beats the prior linear newline scan that ran
    // up to ~3 MB per keystroke on huge files when search was active.
    let cursor_global_byte = {
        let rope = buf.rope();
        let row = cursor_row.min(rope.len_lines());
        rope.line_to_byte(row) + cursor_byte_in_row
    };

    let mut total = 0usize;
    let mut current_idx = 0usize;
    for m in pat.find_iter(&content) {
        total += 1;
        if m.start() <= cursor_global_byte {
            current_idx = total;
        }
        if total >= MATCH_CAP {
            break;
        }
    }
    let result = if total == 0 {
        None
    } else {
        Some((current_idx, total))
    };

    *app.search_count_cache.borrow_mut() = Some(crate::app::SearchCountCache {
        buffer_id,
        dirty_gen,
        cursor,
        pattern: pattern_str.to_string(),
        result,
    });

    result
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

    // ── Filter prompt (`!`) ───────────────────────────────────────────────────
    if let Some(ref field) = app.filter_field {
        let text = field.text();
        let display: String = text.lines().next().unwrap_or("").to_string();
        let content = format!("!{display}");
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

    // ── Crash-recovery prompt (issue #185) ─────────────────────────────────
    // While pending_recovery is Some, show the recovery prompt in the status
    // line (mirrors the confirm-substitute pattern).
    if let Some(pr) = app.pending_recovery.as_ref() {
        let content = format!(
            "E325: swap file found (written {} ago). Recover? [y/N/q]",
            pr.written_ago
        );
        let padded = format!("{content:<width$}", width = width as usize);
        return (
            Line::from(vec![Span::styled(
                padded,
                Style::default()
                    .bg(app.theme.ui.search_bg)
                    .fg(app.theme.ui.search_fg)
                    .add_modifier(Modifier::BOLD),
            )]),
            None,
        );
    }

    // ── Dirty-buffer disk-change prompt (issue #241) ────────────────────────
    // While pending_disk_change is Some, offer keep / reload / diff in place of
    // the normal status bar (mirrors the recovery-prompt pattern).
    if let Some(pdc) = app.pending_disk_change.as_ref() {
        let name = pdc
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| pdc.path.to_string_lossy().into_owned());
        let content =
            format!("W12: \"{name}\" changed on disk since editing — [k]eep / [r]eload / [d]iff");
        let padded = format!("{content:<width$}", width = width as usize);
        return (
            Line::from(vec![Span::styled(
                padded,
                Style::default()
                    .bg(app.theme.ui.search_bg)
                    .fg(app.theme.ui.search_fg)
                    .add_modifier(Modifier::BOLD),
            )]),
            None,
        );
    }

    // ── Explorer git-discard confirm ────────────────────────────────────────
    if let Some(ref path) = app.explorer_git_discard_confirm {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        let content = format!("Discard changes to {name}? (y/n)");
        let padded = format!("{content:<width$}", width = width as usize);
        return (
            Line::from(vec![Span::styled(
                padded,
                Style::default()
                    .bg(app.theme.ui.search_bg)
                    .fg(app.theme.ui.search_fg)
                    .add_modifier(Modifier::BOLD),
            )]),
            None,
        );
    }

    // ── Interactive substitute confirm (:s/pat/rep/c) ──────────────────────
    // While confirming_substitute is Some, show the per-match prompt instead
    // of the normal status bar.
    if let Some(cs) = app.confirming_substitute.as_ref() {
        let rep = if cs.idx < cs.matches.len() {
            cs.matches[cs.idx].replacement.as_str()
        } else {
            ""
        };
        let content = format!("replace with \"{rep}\"? (y/n/a/q/l)");
        let padded = format!("{content:<width$}", width = width as usize);
        return (
            Line::from(vec![Span::styled(
                padded,
                Style::default()
                    .bg(app.theme.ui.search_bg)
                    .fg(app.theme.ui.search_fg)
                    .add_modifier(Modifier::BOLD),
            )]),
            None,
        );
    }

    // ── Macro recording indicator ───────────────────────────────────────────
    // Vim shows "recording @r" while `q{reg}` is active. Render it as a
    // status-message-equivalent so it visually pre-empts the lualine row,
    // matching vim's bottom-line takeover.
    if let Some(reg) = app.active_editor().recording_register() {
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
                    let base = hjkl_engine_tui::style_to_ratatui(base_engine);
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
/// `:copen` / `:lopen` popup (#184) — a read-only bottom pane listing
/// `path:row:col: message`, with the current entry highlighted. Not the fuzzy
/// picker; navigation is `j`/`k`/`<CR>` (see `event_loop`). The `ListState`
/// auto-scrolls to keep the selected entry visible. The quickfix list takes
/// precedence over the location list when both are open.
fn quickfix_popup_overlay(frame: &mut Frame, app: &App, buf_area: Rect) {
    let (list_data, title) = if app.quickfix_open {
        (&app.quickfix, " quickfix ")
    } else {
        (&app.loclist, " location list ")
    };
    let ui = &app.theme.ui;
    let entries = list_data.entries();
    let body_rows = entries.len().clamp(1, 10) as u16;
    let h = body_rows + 2; // +2 for the border.
    if buf_area.height <= h {
        return;
    }
    let area = Rect {
        x: buf_area.x,
        y: buf_area.y + buf_area.height - h,
        width: buf_area.width,
        height: h,
    };
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ui.border));
    if entries.is_empty() {
        frame.render_widget(Paragraph::new("list is empty").block(block), area);
        return;
    }
    let items: Vec<ListItem> = entries
        .iter()
        .map(|e| {
            ListItem::new(format!(
                "{}:{}:{}: {}",
                e.path.display(),
                e.row + 1,
                e.col + 1,
                e.message
            ))
        })
        .collect();
    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .bg(ui.picker_selection_bg)
            .add_modifier(Modifier::BOLD),
    );
    let mut state = ListState::default();
    state.select(Some(list_data.cursor()));
    frame.render_stateful_widget(list, area, &mut state);
}

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
    // The popup is Normal-mode only by design: `entries_for` is called with a
    // hardcoded `HjklMode::Normal` regardless of the current vim mode.
    // Non-Normal modes (Visual, Insert, etc.) suppress the popup implicitly
    // because `active_which_key_prefix` reads the Normal pending buffer of the
    // context keymap, which is always empty when the mode is not Normal.
    // When the explorer sidebar is focused, `ctx_keymap()` returns
    // `explorer_keymap` so the popup lists explorer-specific bindings.
    let entries = crate::which_key::entries_for(
        app.ctx_keymap(),
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

    /// A file's name in the explorer must be colored uniformly — every cell of
    /// the name (including the LAST one) gets the filetype color, not a stray
    /// default-white cell at the end.
    #[test]
    fn explorer_file_name_coloring_is_uniform() {
        use crate::app::App;
        use crate::keymap_actions::AppAction;
        use ratatui::{Terminal, backend::TestBackend};

        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("a").join("b").join("c")).unwrap();
        std::fs::write(
            tmp.path().join("a").join("b").join("c").join("widget.rs"),
            b"x",
        )
        .unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        // Open the DEEP file so there's no splash and its ancestors are expanded
        // (the white-last-char artifact only shows on nested rows).
        let mut app = App::new(Some("a/b/c/widget.rs".into()), false, None, None).unwrap();
        app.dispatch_action(AppAction::ToggleExplorer, 1);

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        // Draw twice: the first frame settles window rects/viewports.
        terminal.draw(|f| frame(f, &mut app)).unwrap();
        terminal.draw(|f| frame(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer().clone();
        let text_color = app.theme.ui.text;
        std::env::set_current_dir(prev).unwrap();

        // Locate the contiguous run of cells spelling "widget.rs".
        let name: Vec<char> = "widget.rs".chars().collect();
        let n = name.len() as u16;
        let cell_sym = |x: u16, y: u16| -> String {
            buf.cell((x, y))
                .map(|c| c.symbol().to_string())
                .unwrap_or_default()
        };
        let mut found: Option<(u16, u16)> = None;
        'scan: for y in 0..24u16 {
            for x in 0..(80 - n) {
                if (0..n).all(|i| cell_sym(x + i, y) == name[i as usize].to_string()) {
                    found = Some((x, y));
                    break 'scan;
                }
            }
        }
        let (sx, sy) = found.expect("widget.rs must be rendered in the explorer");
        let _ = text_color;

        // The icon (two cells before the name) carries the filetype color; the
        // whole name must share that exact color — no `Reset`/white cells.
        let icon_fg = buf.cell((sx - 2, sy)).unwrap().fg;
        assert_ne!(icon_fg, Color::Reset, "icon should be filetype-colored");
        let fgs: Vec<Color> = (0..n).map(|i| buf.cell((sx + i, sy)).unwrap().fg).collect();
        assert!(
            fgs.iter().all(|&f| f == icon_fg),
            "every cell of the name must share the filetype color (no uncolored/white \
             cells); icon_fg={icon_fg:?} name fgs={fgs:?}"
        );
    }

    /// `:diffsplit` paints DiffChange line bands and DiffText char highlights
    /// (#208 Phase 2). One line differs; its changed characters get the stronger
    /// DiffText bg and the rest of the line gets the DiffChange band bg.
    #[test]
    fn diffsplit_paints_change_band_and_difftext() {
        use crate::app::App;
        use ratatui::{Terminal, backend::TestBackend};

        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.txt");
        let b = tmp.path().join("b.txt");
        std::fs::write(&a, "alpha\nbeta\ngamma\n").unwrap();
        std::fs::write(&b, "alpha\nBETA\ngamma\n").unwrap();

        let mut app = App::new(Some(a.clone()), false, None, None).unwrap();
        app.dispatch_ex(&format!("diffsplit {}", b.display()));

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        // Draw twice so window rects/viewports settle before the overlay pass.
        terminal.draw(|f| frame(f, &mut app)).unwrap();
        terminal.draw(|f| frame(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer().clone();

        let change_bg = Color::Rgb(32, 42, 60);
        let text_bg = Color::Rgb(48, 78, 110);

        let mut has_change = false;
        let mut has_text = false;
        for y in 0..24u16 {
            for x in 0..80u16 {
                if let Some(c) = buf.cell((x, y)) {
                    if c.bg == change_bg {
                        has_change = true;
                    }
                    if c.bg == text_bg {
                        has_text = true;
                    }
                }
            }
        }
        assert!(has_change, "a DiffChange band must be painted");
        assert!(has_text, "a DiffText char highlight must be painted");
    }

    /// Filler rows (#250) keep both diff windows aligned: a line only present in
    /// one buffer pushes a blank `DiffDelete`-tinted filler into the other, so a
    /// shared line below the insertion lands on the SAME screen row in both
    /// windows.
    #[test]
    fn diffsplit_filler_aligns_shared_line() {
        use crate::app::App;
        use ratatui::{Terminal, backend::TestBackend};

        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.txt");
        let b = tmp.path().join("b.txt");
        // `ins` exists only in b → a gets a filler row before the later lines.
        // The shared marker `zebra` sits well below the transient top-right
        // toast so the alignment check isn't occluded by it.
        std::fs::write(&a, "l0\nl1\nl2\nl3\nl4\nl5\nzebra\n").unwrap();
        std::fs::write(&b, "l0\nl1\nins\nl2\nl3\nl4\nl5\nzebra\n").unwrap();

        let mut app = App::new(Some(a.clone()), false, None, None).unwrap();
        app.dispatch_ex(&format!("diffsplit {}", b.display()));

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| frame(f, &mut app)).unwrap();
        terminal.draw(|f| frame(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer().clone();

        let cell_sym = |x: u16, y: u16| -> String {
            buf.cell((x, y))
                .map(|c| c.symbol().to_string())
                .unwrap_or_default()
        };
        // Find both occurrences of the shared marker (one per window).
        let needle: Vec<char> = "zebra".chars().collect();
        let n = needle.len() as u16;
        let mut gamma_rows = Vec::new();
        for y in 0..24u16 {
            for x in 0..(80 - n) {
                if (0..n).all(|i| cell_sym(x + i, y) == needle[i as usize].to_string()) {
                    gamma_rows.push(y);
                }
            }
        }
        assert_eq!(
            gamma_rows.len(),
            2,
            "gamma must render once per diff window; got {gamma_rows:?}"
        );
        assert_eq!(
            gamma_rows[0], gamma_rows[1],
            "filler must align `gamma` on the same screen row in both windows"
        );

        // A DiffDelete filler row must be painted (muted dark red).
        let filler_bg = Color::Rgb(60, 32, 32);
        let has_filler = (0..24u16)
            .any(|y| (0..80u16).any(|x| buf.cell((x, y)).map(|c| c.bg) == Some(filler_bg)));
        assert!(has_filler, "a DiffDelete filler row must be painted");
    }
}
