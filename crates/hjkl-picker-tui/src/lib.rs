//! Ratatui adapter for [`hjkl_picker`].
//!
//! Provides:
//! - [`handle_key`]: translate a `crossterm::event::KeyEvent` into the
//!   appropriate `Picker` method call, returning a `PickerEvent`.
//! - [`preview_pane`]: render the picker's preview surface into a `ratatui::Frame`.
//! - [`PreviewTheme`]: pre-computed styles consumed by `preview_pane`.
//!
//! The agnostic [`hjkl_picker::Picker`] owns the state and scoring logic;
//! this crate owns the ratatui + crossterm surface.

#![forbid(unsafe_code)]

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hjkl_buffer::Viewport;
use hjkl_buffer_tui::{BufferView, Gutter};
use hjkl_engine::Input;
use hjkl_picker::{Picker, PickerAction, PickerEvent, PreviewHighlighter, PreviewSpans};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Block, Borders, Paragraph};

/// Translate a crossterm key event into picker state changes.
///
/// Handles the same shape the old `Picker::handle_key` did:
/// - `Esc` / `C-c` → `Cancel`
/// - `Enter` → `Accept` (returns `Select(action)` or `None`)
/// - `Down` / `C-n` → `select_next`
/// - `Up`   / `C-p` → `select_prev`
/// - Other keys: first offered to the source's `handle_key`; if it returns
///   `Some(action)`, emitted as `Select`. Otherwise forwarded to the query field.
pub fn handle_key(picker: &mut Picker, key: KeyEvent) -> PickerEvent {
    if key.code == KeyCode::Esc {
        return picker.cancel();
    }
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return picker.cancel();
    }
    if key.code == KeyCode::Enter {
        return picker.accept();
    }

    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Down => {
            picker.select_next();
            return PickerEvent::None;
        }
        KeyCode::Up => {
            picker.select_prev();
            return PickerEvent::None;
        }
        KeyCode::Char('n') if ctrl => {
            picker.select_next();
            return PickerEvent::None;
        }
        KeyCode::Char('p') if ctrl => {
            picker.select_prev();
            return PickerEvent::None;
        }
        _ => {}
    }

    let input: Input = hjkl_engine_tui::crossterm_to_input(key);
    if let Some(action) = picker.handle_source_key(input) {
        return PickerEvent::Select(action);
    }
    if matches!(input.key, hjkl_engine::Key::Enter | hjkl_engine::Key::Esc) {
        return PickerEvent::None;
    }
    picker.handle_query_input(input);
    let _ = PickerAction::None; // keep the import used
    PickerEvent::None
}

/// Visual styling for [`preview_pane`]. Pre-computed `Style`s rather than
/// raw `Color`s so consumers retain full control (modifiers, bg/fg layering).
pub struct PreviewTheme {
    /// Border around the preview block.
    pub border: Style,
    /// Gutter (line-number column) foreground style.
    pub gutter: Style,
    /// Style for non-text glyphs (tabs, trailing whitespace markers).
    pub non_text: Style,
    /// Background painted across the cursor row when a match is active.
    pub cursor_line: Style,
}

/// Render the picker preview pane into `area`.
///
/// Pulls the active preview's path + buffer bytes from `picker`, dispatches to
/// `highlighter` for spans, and draws the result via `BufferView`. When the
/// active source has no preview path (e.g. ephemeral diff text), the pane
/// renders monochrome — the highlighter is not consulted.
pub fn preview_pane(
    frame: &mut Frame,
    picker: &Picker,
    highlighter: &dyn PreviewHighlighter,
    theme: &PreviewTheme,
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
        .border_style(theme.border)
        .title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if !status.is_empty() {
        let para = Paragraph::new(format!("  ({status})"));
        frame.render_widget(para, inner);
        return;
    }

    let buf = picker.preview_buffer();
    let line_count = buf.lines().len();

    let preview_spans: PreviewSpans = match picker.preview_path() {
        Some(path) => {
            let mut bytes = buf.lines().join("\n").into_bytes();
            if !bytes.is_empty() {
                bytes.push(b'\n');
            }
            highlighter.spans_for_viewport(
                path,
                &bytes,
                picker.preview_top_row(),
                inner.height as usize,
            )
        }
        None => PreviewSpans::default(),
    };

    let gw = gutter_width(line_count.max(1));
    let viewport = Viewport {
        top_row: picker.preview_top_row(),
        top_col: 0,
        width: inner.width.saturating_sub(gw),
        height: inner.height,
        text_width: inner.width.saturating_sub(gw),
        ..Viewport::default()
    };
    // Resolve engine-native style ids to ratatui styles via the engine's
    // ratatui-feature conversion. PreviewSpans stores `hjkl_engine::Style`
    // post-headless-refactor; the buffer's `StyleResolver` expects ratatui
    // Style at the boundary.
    let resolver = |id: u32| -> Style {
        preview_spans
            .styles
            .get(id as usize)
            .map(|s| hjkl_engine_tui::style_to_ratatui(*s))
            .unwrap_or_default()
    };
    let cursor_line_bg = if picker.preview_match_row().is_some() {
        theme.cursor_line
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
            style: theme.gutter,
            line_offset: picker.preview_line_offset(),
            ..Default::default()
        }),
        search_bg: Style::default(),
        signs: &[],
        conceals: &[],
        spans: &preview_spans.by_row,
        search_pattern: None,
        non_text_style: theme.non_text,
        diag_overlays: &[],
        colorcolumn_cols: &[],
        colorcolumn_style: Style::default(),
    };
    frame.render_widget(view, inner);
}

/// Gutter width for preview pane: digit count + 1-column trailing spacer,
/// floored to neovim's default `numberwidth` of 4.
fn gutter_width(line_count: usize) -> u16 {
    const NUMBERWIDTH: usize = 4;
    let needed = line_count.to_string().len() + 1;
    needed.max(NUMBERWIDTH) as u16
}
