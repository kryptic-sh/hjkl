//! Direct cell-write `ratatui::widgets::Widget` for [`hjkl_buffer::Buffer`].
//!
//! ## Render path
//!
//! [`BufferView`] implements
//! `ratatui::widgets::Widget`. The widget is **single-pass** — text,
//! selection, gutter signs, and styled spans all paint together. There is
//! no separate `Paragraph` or layout step. Writes one cell at a time so
//! syntax span fg, cursor-line bg, cursor cell REVERSED, and selection bg
//! layer in a single pass without the grapheme / wrap machinery `Paragraph`
//! does.
//!
//! Caller wraps a `&Buffer` in [`BufferView`], hands it the style table
//! that resolves opaque [`hjkl_buffer::Span`] style ids to real ratatui styles
//! via a [`StyleResolver`], and renders into a `ratatui::Frame`.
//!
//! ## StyleResolver hooks
//!
//! The [`StyleResolver`] trait is the host's bridge from opaque `u32` style
//! ids (stored in [`hjkl_buffer::Span::style`]) to real `ratatui::style::Style`
//! values. Implement it against your own theme. A convenience blanket impl
//! exists for closures `Fn(u32) -> Style`.

use ratatui::buffer::Buffer as TermBuffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Widget;
use unicode_width::UnicodeWidthChar;

use hjkl_buffer::wrap::wrap_segments;
use hjkl_buffer::{Buffer, Selection, Span, Viewport, Wrap};

/// Resolves an opaque [`hjkl_buffer::Span::style`] id to a real ratatui
/// style. The buffer doesn't know about colours; the host (sqeel-vim
/// or any future user) keeps a lookup table.
pub trait StyleResolver {
    fn resolve(&self, style_id: u32) -> Style;
}

/// Convenience impl so simple closures can drive the renderer.
impl<F: Fn(u32) -> Style> StyleResolver for F {
    fn resolve(&self, style_id: u32) -> Style {
        self(style_id)
    }
}

/// Render-time wrapper around `&Buffer` that carries the optional
/// [`Selection`] + a [`StyleResolver`]. Created per draw, dropped
/// when the frame is done — cheap, holds only refs.
///
/// 0.0.34 (Patch C-δ.1): added the `viewport` field. The viewport
/// previously lived on the buffer itself; with the relocation to the
/// engine `Host`, the renderer takes a borrow per draw.
///
/// 0.0.37: added the `spans` and `search_pattern` fields. Per-row
/// syntax spans + the active `/` regex used to live on the buffer
/// (`Buffer::spans` / `Buffer::search_pattern`); both moved out per
/// step 3 of `DESIGN_33_METHOD_CLASSIFICATION.md`. The host now feeds
/// each into the view per draw — populated from
/// `Editor::buffer_spans()` and `Editor::search_state().pattern`.
pub struct BufferView<'a, R: StyleResolver> {
    pub buffer: &'a Buffer,
    /// Viewport snapshot the host published this frame. Owned by the
    /// engine `Host`; the renderer borrows for the duration of the
    /// draw.
    pub viewport: &'a Viewport,
    pub selection: Option<Selection>,
    pub resolver: &'a R,
    /// Bg painted across the cursor row (vim's `cursorline`). Pass
    /// `Style::default()` to disable.
    pub cursor_line_bg: Style,
    /// Row to paint [`Self::cursor_line_bg`] across, overriding the buffer's
    /// live cursor row. Used for **unfocused** windows sharing a buffer with
    /// the focused one: each window keeps its own saved cursor row, so the
    /// inactive "ghost" cursorline must sit on that saved row, not chase the
    /// active window's cursor. `None` = use `buffer.cursor().row` (the
    /// focused-window default).
    pub cursor_line_row: Option<usize>,
    /// Bg painted across a closed fold's header row — brighter than
    /// `cursor_line_bg` so a collapsed fold is visually distinct.
    /// `Style::default()` to disable.
    pub fold_line_bg: Style,
    /// Fold set to render, overriding `buffer.folds()`. Used for **unfocused**
    /// windows under window-level folds: each window keeps its own open/closed
    /// state, and the shared buffer only holds the focused window's set, so an
    /// unfocused split must render against its own snapshot. `None` = read the
    /// live `buffer.folds()` (the focused-window default).
    pub folds_override: Option<&'a [hjkl_buffer::Fold]>,
    /// Bg painted down the cursor column (vim's `cursorcolumn`). Pass
    /// `Style::default()` to disable.
    pub cursor_column_bg: Style,
    /// Bg painted under selected cells. Composed over syntax fg.
    pub selection_bg: Style,
    /// Style for the cursor cell. `REVERSED` is the conventional
    /// choice; works against any theme.
    pub cursor_style: Style,
    /// Optional left-side line-number gutter. `width` includes the
    /// trailing space separating the number from text. Pass `None`
    /// to disable. Numbers are 1-based, right-aligned.
    pub gutter: Option<Gutter>,
    /// Bg painted under cells covered by an active `/` search match.
    /// `Style::default()` to disable.
    pub search_bg: Style,
    /// Per-row gutter signs (LSP diagnostic dots, git diff markers,
    /// …). Painted into the leftmost gutter column after the line
    /// number, so they overwrite the leading space tui-style gutters
    /// reserve. Highest-priority sign per row wins.
    pub signs: &'a [Sign],
    /// Per-row substitutions applied at render time. Each conceal
    /// hides the byte range `[start_byte, end_byte)` and paints
    /// `replacement` in its place. Empty slice = no conceals.
    pub conceals: &'a [Conceal],
    /// Per-row syntax spans the host has computed for this frame.
    /// `spans[row]` carries the styled byte ranges for that row;
    /// rows beyond `spans.len()` get no syntax styling. Pass `&[]`
    /// for hosts without syntax integration.
    ///
    /// 0.0.37: lifted out of `Buffer` per step 3 of
    /// `DESIGN_33_METHOD_CLASSIFICATION.md`. The engine populates
    /// this via `Editor::buffer_spans()`.
    pub spans: &'a [Vec<Span>],
    /// Active `/` search regex, if any. The renderer paints
    /// [`Self::search_bg`] under cells that match. Pass `None` to
    /// disable hlsearch.
    ///
    /// 0.0.37: lifted out of `Buffer` (was `Buffer::search_pattern`)
    /// per step 3 of `DESIGN_33_METHOD_CLASSIFICATION.md`. The engine
    /// publishes the pattern via `Editor::search_state().pattern`.
    pub search_pattern: Option<&'a regex::Regex>,
    /// Style for the `~` tilde marker painted on screen rows that are
    /// past the last buffer line (vim's `NonText` highlight group).
    /// Pass `Style::default()` to use terminal defaults.
    ///
    /// The gutter on those rows is painted blank; the `~` appears at the
    /// leftmost text column. Rows within the buffer are unaffected.
    pub non_text_style: Style,
    /// Whether to paint the end-of-buffer `~` markers (vim's `fillchars=eob:`).
    /// `true` is the default vim behaviour; `false` leaves rows past the last
    /// line blank (e.g. the file explorer).
    pub show_eob: bool,
    /// Diagnostic overlays (LSP inline highlights). Applied in a
    /// post-paint pass after every row is drawn so they layer on top of
    /// syntax and selection colours without a second layout traversal.
    /// Pass `&[]` to disable. Added in 0.5.0.
    pub diag_overlays: &'a [DiagOverlay],
    /// 1-based column indices for vertical rulers (vim's `colorcolumn`).
    /// The renderer paints `colorcolumn_style` on those text-area cells
    /// beneath syntax highlights. Pass `&[]` to disable.
    pub colorcolumn_cols: &'a [u16],
    /// Background style applied to cells at a `colorcolumn` position.
    /// Ignored when `colorcolumn_cols` is empty.
    pub colorcolumn_style: Style,
    /// When `Some`, invisibles rendering is active. The [`hjkl_buffer::ListChars`]
    /// value controls which glyphs substitute whitespace characters.
    /// Matches vim's `:set list` + `:set listchars`. Pass `None` to disable.
    pub listchars: Option<&'a hjkl_buffer::ListChars>,
    /// When `true`, paint thin vertical indent guide characters at every
    /// `shiftwidth`-aligned leading-whitespace column on each row. Only active
    /// in `Wrap::None` mode. Pass `false` to disable.
    pub indent_guides_enabled: bool,
    /// Character to paint as the indent guide. Typically `'│'`.
    pub indent_guide_char: char,
    /// Number of leading visual columns that qualify as "shiftwidth" per level.
    /// Derived from `Settings::shiftwidth` / `Settings::tabstop`. The renderer
    /// uses this to locate guide columns on each row.
    pub indent_guide_shiftwidth: usize,
    /// Fg color for inactive indent guide cells.
    pub indent_guide_fg: ratatui::style::Color,
    /// Fg color for the active (cursor's indent level) guide cell.
    pub indent_guide_active_fg: ratatui::style::Color,
    /// Visual column of the active indent guide, if any. Computed from the
    /// cursor row's leading whitespace and `indent_guide_shiftwidth`.
    /// `None` when the cursor sits on a non-indented or empty row.
    pub indent_guide_active_col: Option<usize>,
    /// End-of-line virtual-text hints (e.g. inline git blame). Painted after
    /// each row's real text in a post-pass. Empty slice = none. Added for #202.
    pub eol_hints: &'a [EolHint],
    /// Boxed-blame layout. When `Some`, the renderer draws exactly this
    /// sequence of screen rows (top-to-bottom) instead of deriving rows from
    /// `top_row`: `Content(doc_row)` paints that buffer row, `BorderTop`/
    /// `BorderBottom` paint a box rule. The host (render level) builds the plan
    /// — the engine's cursor/scroll stays the source of truth. `Wrap::None`
    /// only. `None` = normal rendering.
    pub blame_plan: Option<&'a [BlameRow]>,
}

/// One screen row in a boxed-blame layout ([`BufferView::blame_plan`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlameRow {
    /// A real buffer row painted at this screen line.
    Content(usize),
    /// Box top border with a commit title rendered into it.
    BorderTop(String),
    /// Box bottom border (plain rule).
    BorderBottom,
}

/// Controls what numbers are rendered in the gutter.
///
/// Matches vim's `:set number` / `:set relativenumber` combinations.
#[derive(Debug, Clone, Copy, Default)]
pub enum GutterNumbers {
    /// No line numbers — gutter cells painted blank (still occupies width).
    None,
    /// 1-based absolute row numbers (current default).
    #[default]
    Absolute,
    /// Offset from `cursor_row` for non-cursor rows; cursor row shows `0`.
    Relative { cursor_row: usize },
    /// Vim's `nu+rnu`: cursor row shows its absolute number, others show
    /// offset from `cursor_row`.
    Hybrid { cursor_row: usize },
}

/// Configuration for the line-number gutter rendered to the left of
/// the text area. `width` is the number-column cell count reserved
/// (including any trailing spacer); the renderer right-aligns the
/// 1-based row number into the leftmost `width - 1` cells.
///
/// `sign_column_width` reserves cells to the LEFT of the number column
/// for sign chars (LSP diagnostics, git diff markers). The sign column
/// is a dedicated strip separate from the number column: vim/neovim
/// convention is `[ sign | number_padded | spacer | text ]`. When
/// `sign_column_width == 0` the layout collapses to
/// `[ number_padded | spacer | text ]`.
///
/// `line_offset` is added to the displayed line number, so a host
/// rendering a windowed view of a larger document (e.g. picker preview
/// of a 7000-line buffer) can show the original line numbers instead
/// of starting at 1. Only applied in `Absolute` mode.
#[derive(Debug, Clone, Copy, Default)]
pub struct Gutter {
    /// Width of the number column (digits + 1 trailing spacer). Does NOT
    /// include `sign_column_width`.
    pub width: u16,
    pub style: Style,
    pub line_offset: usize,
    /// What kind of numbers to render. Defaults to `Absolute`.
    pub numbers: GutterNumbers,
    /// Width of the dedicated sign column to the left of the number column.
    /// Typically 0 (no signs) or 1 (one sign char per row). Signs are
    /// painted in `area.x .. area.x + sign_column_width`; numbers are
    /// painted in `area.x + sign_column_width .. area.x + sign_column_width + width`.
    pub sign_column_width: u16,
    /// Width of the fold-indicator column, painted to the RIGHT of the
    /// number column and LEFT of the text (`[sign][number][fold][text]`,
    /// matching the cursor/overlay geometry which reserves
    /// `sign_column_width + fold_column_width` of extra gutter). Typically
    /// 0 (no fold cue) or 1. The renderer paints an open/closed fold glyph
    /// per row into this column; `0` collapses it entirely.
    pub fold_column_width: u16,
}

/// Single-cell marker painted into the leftmost gutter column for a
/// document row. Used by hosts to surface LSP diagnostics, git diff
/// signs, etc. Higher `priority` wins when multiple signs land on
/// the same row.
#[derive(Debug, Clone, Copy)]
pub struct Sign {
    pub row: usize,
    pub ch: char,
    pub style: Style,
    pub priority: u8,
}

/// Render-time substitution that hides a byte range and paints
/// `replacement` in its place. The buffer's content stays unchanged;
/// only the rendered cells differ. Used by hosts to pretty-print
/// URLs, conceal markdown markers, etc.
#[derive(Debug, Clone)]
pub struct Conceal {
    pub row: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub replacement: String,
}

/// A char-column range on a document row that should be styled with an
/// overlay (e.g. an underline for LSP diagnostics). Applied in a
/// post-paint pass so it composes on top of syntax and selection colours.
///
/// Added in 0.5.0 for LSP diagnostic inline rendering.
#[derive(Debug, Clone, Copy)]
pub struct DiagOverlay {
    /// 0-based document row.
    pub row: usize,
    /// 0-based start char-column (inclusive).
    pub col_start: usize,
    /// 0-based end char-column (exclusive).
    pub col_end: usize,
    /// Style applied to cells in `[col_start, col_end)`.
    pub style: Style,
}

/// End-of-line virtual text (e.g. inline git blame). Painted in the trailing
/// cells of `row` after its real text, in `style`, with a 2-column gap. Purely
/// decorative — occupies no buffer bytes and never affects cursor motion or
/// layout. Truncated at the right edge of the text area; skipped when the row's
/// text already fills the width.
#[derive(Debug, Clone)]
pub struct EolHint {
    /// 0-based document row the hint trails.
    pub row: usize,
    /// The virtual text to paint (already formatted by the host).
    pub text: String,
    /// Style for the hint cells (typically a dimmed fg).
    pub style: Style,
}

/// Average two terminal colors channel-wise. Used to blend the fold-header bg
/// with the cursorline (or ghost-cursorline) bg when both land on the same
/// row, so the line reads as "fold AND cursor" instead of one overriding the
/// other. Falls back to the first color when either side isn't a concrete RGB
/// value (named/indexed colors can't be averaged meaningfully).
fn mix_colors(a: ratatui::style::Color, b: ratatui::style::Color) -> ratatui::style::Color {
    use ratatui::style::Color;
    match (a, b) {
        (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => Color::Rgb(
            ((r1 as u16 + r2 as u16) / 2) as u8,
            ((g1 as u16 + g2 as u16) / 2) as u8,
            ((b1 as u16 + b2 as u16) / 2) as u8,
        ),
        _ => a,
    }
}

/// Glyph for the fold-indicator column at `doc_row`:
///   `▾` open fold start · `▸` closed fold start ·
///   `│` row inside an open fold body · ` ` no fold.
/// A row that is both an open-fold start and inside an outer open fold
/// still reads as a start (`▾`), matching vim's foldcolumn precedence.
fn fold_column_glyph(folds: &[hjkl_buffer::Fold], doc_row: usize) -> char {
    let mut inside_open = false;
    for f in folds {
        if f.start_row == doc_row {
            return if f.closed { '▸' } else { '▾' };
        }
        if !f.closed && doc_row > f.start_row && doc_row <= f.end_row {
            inside_open = true;
        }
    }
    if inside_open { '│' } else { ' ' }
}

/// Compute the visual display width of `line` in terminal cells, expanding
/// tabs to the next multiple of `tab_width` (matching `paint_row`'s logic).
/// Uses `UnicodeWidthChar` for non-tab characters, identical to how
/// `paint_row` advances `screen_x`.
fn display_width(line: &str, tab_width: usize) -> usize {
    let mut col: usize = 0;
    for ch in line.chars() {
        if ch == '\t' {
            col += tab_width - (col % tab_width);
        } else {
            col += ch.width().unwrap_or(1);
        }
    }
    col
}

impl<R: StyleResolver> Widget for BufferView<'_, R> {
    fn render(self, area: Rect, term_buf: &mut TermBuffer) {
        // Boxed-blame layout: render the host-supplied plan instead of the
        // normal doc-row walk. Keeps the engine's cursor/scroll authoritative;
        // this is a pure render transform.
        if let Some(plan) = self.blame_plan {
            self.render_blame_plan(area, term_buf, plan);
            return;
        }
        let viewport = *self.viewport;
        let cursor = self.buffer.cursor();
        // Cursorline lands on the overridden row (unfocused window's saved
        // cursor) when set, else the buffer's live cursor row.
        let cursor_line_row = self.cursor_line_row.unwrap_or(cursor.row);
        let spans = self.spans;
        let folds = self
            .folds_override
            .map(<[hjkl_buffer::Fold]>::to_vec)
            .unwrap_or_else(|| self.buffer.folds());
        let top_row = viewport.top_row;
        let top_col = viewport.top_col;
        // Fetch only the viewport-bounded row slice. The render loop walks
        // at most `area.height` screen rows, so on a 100K-row buffer we
        // clone ~50 rows instead of the entire Vec<String>. Closed folds
        // can skip past the precomputed bound — the rare overflow branch
        // falls back to `Buffer::line(row)`.
        let total_rows = self.buffer.row_count();
        let prefetch_end = top_row.saturating_add(area.height as usize).min(total_rows);
        let rope = self.buffer.rope();
        let lines_prefetch: Vec<String> = (top_row..prefetch_end)
            .map(|i| hjkl_buffer::rope_line_str(&rope, i))
            .collect();
        let prefetch_base = top_row;
        let prefetch_end_idx = prefetch_end;
        let line_at = |row: usize| -> String {
            if row >= prefetch_base && row < prefetch_end_idx {
                lines_prefetch[row - prefetch_base].clone()
            } else {
                hjkl_buffer::rope_line_str(&rope, row)
            }
        };

        let gutter_total = self
            .gutter
            .map(|g| g.sign_column_width + g.width + g.fold_column_width)
            .unwrap_or(0);
        let text_area = Rect {
            x: area.x.saturating_add(gutter_total),
            y: area.y,
            width: area.width.saturating_sub(gutter_total),
            height: area.height,
        };

        // total_rows already captured above.
        let mut doc_row = top_row;
        let mut screen_row: u16 = 0;
        let wrap_mode = viewport.wrap;
        let seg_width = if viewport.text_width > 0 {
            viewport.text_width
        } else {
            text_area.width
        };
        // Per-screen-row flag: true when the cell at the cursor's
        // column on that screen row is part of an active `/` search
        // match. The cursorcolumn pass uses this to skip cells that
        // search bg already painted, so search highlight wins over
        // the column bg.
        let mut search_hit_at_cursor_col: Vec<bool> = Vec::new();
        // Map each painted screen row to the doc row whose LAST visual segment
        // landed there, so the EOL-hint pass can paint at the right y for the
        // correct row (handles folds + soft-wrap: only the last segment row of
        // a doc row is recorded). `None` for tilde/blank rows.
        let mut screen_to_doc: Vec<Option<usize>> = vec![None; area.height as usize];
        // Walk the document forward, skipping rows hidden by closed
        // folds. Emit the start row of a closed fold as a marker
        // line instead of its actual content.
        while doc_row < total_rows && screen_row < area.height {
            // Skip rows hidden by a closed fold (any row past start
            // of a closed fold).
            if folds.iter().any(|f| f.hides(doc_row)) {
                doc_row += 1;
                continue;
            }
            let folded_at_start = folds
                .iter()
                .find(|f| f.closed && f.start_row == doc_row)
                .copied();
            let line_owned = line_at(doc_row);
            let line: &str = line_owned.as_str();
            let row_spans = spans.get(doc_row).map(Vec::as_slice).unwrap_or(&[]);
            let sel_range = self.selection.and_then(|s| s.row_span(doc_row));
            let is_cursor_row = doc_row == cursor_line_row;
            if let Some(fold) = folded_at_start {
                if let Some(gutter) = self.gutter {
                    self.paint_gutter(term_buf, area, screen_row, doc_row, gutter);
                    self.paint_signs(term_buf, area, screen_row, doc_row, gutter);
                    self.paint_fold_column(term_buf, area, screen_row, doc_row, gutter, &folds);
                }
                // Render the fold's first line exactly like a normal line
                // (real syntax highlighting, selection, search matches, conceals).
                // Only the first screen segment (Wrap::None: top_col..MAX) is needed
                // since a closed fold collapses to exactly one screen row.
                let fold_search_ranges = self.row_search_ranges(line);
                let fold_conceals: Vec<&Conceal> = {
                    let mut v: Vec<&Conceal> =
                        self.conceals.iter().filter(|c| c.row == doc_row).collect();
                    v.sort_by_key(|c| c.start_byte);
                    v
                };
                self.paint_row(
                    term_buf,
                    text_area,
                    screen_row,
                    line,
                    row_spans,
                    sel_range,
                    &fold_search_ranges,
                    is_cursor_row,
                    cursor.col,
                    top_col,
                    usize::MAX,
                    true, // is_last_segment
                    &fold_conceals,
                );
                // Overlay fold bg across the full row (gutter + text) so the
                // collapsed fold header is visually distinct. Patched so only
                // bg changes — fg, glyphs, and text are preserved. When the
                // cursor row (or an unfocused window's ghost cursor row) lands
                // on the fold header, blend the two backgrounds so the row
                // reads as "fold AND cursorline" instead of one hiding the
                // other.
                if self.fold_line_bg != Style::default() {
                    let overlay =
                        match (is_cursor_row, self.fold_line_bg.bg, self.cursor_line_bg.bg) {
                            (true, Some(fold_bg), Some(cur_bg)) => {
                                Style::default().bg(mix_colors(fold_bg, cur_bg))
                            }
                            _ => self.fold_line_bg,
                        };
                    let y = area.y + screen_row;
                    for x in area.x..(area.x + area.width) {
                        if let Some(cell) = term_buf.cell_mut((x, y)) {
                            cell.set_style(cell.style().patch(overlay));
                        }
                    }
                }
                let fold_has_hit = fold_search_ranges
                    .iter()
                    .any(|&(s, e)| cursor.col >= s && cursor.col < e);
                search_hit_at_cursor_col.push(fold_has_hit);
                if (screen_row as usize) < screen_to_doc.len() {
                    screen_to_doc[screen_row as usize] = Some(doc_row);
                }
                screen_row += 1;
                doc_row = fold.end_row + 1;
                continue;
            }
            let search_ranges = self.row_search_ranges(line);
            let row_has_hit_at_cursor_col = search_ranges
                .iter()
                .any(|&(s, e)| cursor.col >= s && cursor.col < e);
            // Collect conceals for this row, sorted by start_byte.
            let row_conceals: Vec<&Conceal> = {
                let mut v: Vec<&Conceal> =
                    self.conceals.iter().filter(|c| c.row == doc_row).collect();
                v.sort_by_key(|c| c.start_byte);
                v
            };
            // Compute screen segments for this doc row. `Wrap::None`
            // produces a single segment that spans the whole line; the
            // existing `top_col` horizontal scroll is preserved by
            // passing `top_col` as the segment start. Wrap modes split
            // the line into multiple visual rows that fit
            // `viewport.text_width` (falls back to `text_area.width`
            // when the host hasn't published a text width yet).
            let segments = match wrap_mode {
                Wrap::None => vec![(top_col, usize::MAX)],
                _ => wrap_segments(line, seg_width, wrap_mode),
            };
            let last_seg_idx = segments.len().saturating_sub(1);
            for (seg_idx, &(seg_start, seg_end)) in segments.iter().enumerate() {
                if screen_row >= area.height {
                    break;
                }
                if let Some(gutter) = self.gutter {
                    if seg_idx == 0 {
                        self.paint_gutter(term_buf, area, screen_row, doc_row, gutter);
                        self.paint_signs(term_buf, area, screen_row, doc_row, gutter);
                        self.paint_fold_column(term_buf, area, screen_row, doc_row, gutter, &folds);
                    } else {
                        self.paint_blank_gutter(term_buf, area, screen_row, gutter);
                    }
                }
                self.paint_row(
                    term_buf,
                    text_area,
                    screen_row,
                    line,
                    row_spans,
                    sel_range,
                    &search_ranges,
                    is_cursor_row,
                    cursor.col,
                    seg_start,
                    seg_end,
                    seg_idx == last_seg_idx,
                    &row_conceals,
                );
                search_hit_at_cursor_col.push(row_has_hit_at_cursor_col);
                if seg_idx == last_seg_idx && (screen_row as usize) < screen_to_doc.len() {
                    screen_to_doc[screen_row as usize] = Some(doc_row);
                }
                screen_row += 1;
            }
            doc_row += 1;
        }
        // Tilde pass: paint `~` on every screen row past the last buffer
        // line (vim's NonText marker). Gutter on those rows stays blank.
        while screen_row < area.height {
            // Blank gutter if present.
            if let Some(gutter) = self.gutter {
                self.paint_blank_gutter(term_buf, area, screen_row, gutter);
            }
            // Paint `~` at the first text column (unless eob markers are off).
            if self.show_eob {
                let y = text_area.y + screen_row;
                if let Some(cell) = term_buf.cell_mut((text_area.x, y)) {
                    cell.set_char('~');
                    cell.set_style(self.non_text_style);
                }
            }
            screen_row += 1;
        }
        // ── EOL virtual-text pass (#202): paint host hints after row text ──
        if !self.eol_hints.is_empty() {
            let tab_width = self.viewport.effective_tab_width();
            for (sr, maybe_doc) in screen_to_doc.iter().enumerate() {
                let Some(doc) = *maybe_doc else { continue };
                let Some(hint) = self.eol_hints.iter().find(|h| h.row == doc) else {
                    continue;
                };
                // Visual width of the row's text in cells (tab-expanded),
                // minus horizontal scroll, to find where real text ends.
                let line = line_at(doc);
                let text_cols = display_width(&line, tab_width);
                let start_vis = text_cols.saturating_sub(top_col);
                // 2-col gap after the text.
                let x = text_area
                    .x
                    .saturating_add(start_vis as u16)
                    .saturating_add(2);
                let y = text_area.y + sr as u16;
                let right = text_area.x + text_area.width;
                if x >= right {
                    continue; // no room
                }
                for (i, ch) in hint.text.chars().enumerate() {
                    let cx = x + i as u16;
                    if cx >= right {
                        break;
                    }
                    if let Some(cell) = term_buf.cell_mut((cx, y)) {
                        cell.set_char(ch);
                        cell.set_style(hint.style);
                    }
                }
            }
        }

        // Cursorcolumn pass: layer the bg over the cursor's visible
        // column once every row is painted so it composes on top of
        // syntax / cursorline backgrounds without disturbing fg.
        // Skipped when wrapping — the cursor's screen x depends on the
        // segment it lands in, and vim's cursorcolumn semantics with
        // wrap are fuzzy. Revisit if it bites.
        if matches!(wrap_mode, Wrap::None)
            && self.cursor_column_bg != Style::default()
            && cursor.col >= top_col
            && (cursor.col - top_col) < text_area.width as usize
        {
            let x = text_area.x + (cursor.col - top_col) as u16;
            for sy in 0..screen_row {
                // Skip rows where search bg already painted this cell —
                // search highlight wins over cursorcolumn so `/foo`
                // matches stay readable when the cursor sits on them.
                if search_hit_at_cursor_col
                    .get(sy as usize)
                    .copied()
                    .unwrap_or(false)
                {
                    continue;
                }
                let y = text_area.y + sy;
                if let Some(cell) = term_buf.cell_mut((x, y)) {
                    cell.set_style(cell.style().patch(self.cursor_column_bg));
                }
            }
        }

        // Colorcolumn pass: paint vertical ruler(s) under syntax.
        // Applied only in Wrap::None mode; skips indices that are
        // scrolled out of the visible horizontal window.
        if matches!(wrap_mode, Wrap::None) && !self.colorcolumn_cols.is_empty() {
            for &col_1based in self.colorcolumn_cols {
                let col = col_1based as usize; // convert to 0-based
                if col == 0 || col < top_col + 1 {
                    continue; // out of visible range (scrolled past left edge)
                }
                let screen_col = col - 1 - top_col; // 0-based screen offset
                if screen_col >= text_area.width as usize {
                    continue; // out of visible range (past right edge)
                }
                let x = text_area.x + screen_col as u16;
                for sy in 0..screen_row {
                    let y = text_area.y + sy;
                    if let Some(cell) = term_buf.cell_mut((x, y)) {
                        cell.set_style(cell.style().patch(self.colorcolumn_style));
                    }
                }
            }
        }

        // Indent guides pass: paint guide characters at shiftwidth-aligned
        // columns on every visible row that has sufficient leading whitespace.
        // Only active in Wrap::None mode. Computes leading visual width per
        // screen row from the pre-fetched line strings.
        if matches!(wrap_mode, Wrap::None)
            && self.indent_guides_enabled
            && self.indent_guide_shiftwidth > 0
        {
            let sw = self.indent_guide_shiftwidth;
            let tab_width = self.viewport.effective_tab_width();
            // Walk the same doc_row range as the first pass.
            let mut ig_doc_row = top_row;
            let mut ig_screen_row: u16 = 0;
            while ig_doc_row < total_rows && ig_screen_row < area.height {
                if folds.iter().any(|f| f.hides(ig_doc_row)) {
                    ig_doc_row += 1;
                    continue;
                }
                // Skip closed fold markers — they collapse to a single marker row.
                if let Some(fold) = folds
                    .iter()
                    .find(|f| f.closed && f.start_row == ig_doc_row)
                    .copied()
                {
                    ig_screen_row += 1;
                    ig_doc_row = fold.end_row + 1;
                    continue;
                }
                let line_owned = line_at(ig_doc_row);
                let line: &str = line_owned.as_str();
                // Compute leading visual column count: walk until non-whitespace.
                let mut leading_vcols: usize = 0;
                for ch in line.chars() {
                    match ch {
                        ' ' => leading_vcols += 1,
                        '\t' => {
                            leading_vcols += tab_width - (leading_vcols % tab_width);
                        }
                        _ => break,
                    }
                }
                // Paint guides at sw, 2*sw, 3*sw, ... while < leading_vcols.
                let y = text_area.y + ig_screen_row;
                let mut guide_col = sw;
                while guide_col < leading_vcols {
                    // Convert visual column to screen x, accounting for top_col scroll.
                    if guide_col >= top_col {
                        let screen_col = guide_col - top_col;
                        if screen_col < text_area.width as usize {
                            let x = text_area.x + screen_col as u16;
                            let is_active = Some(guide_col) == self.indent_guide_active_col;
                            let fg = if is_active {
                                self.indent_guide_active_fg
                            } else {
                                self.indent_guide_fg
                            };
                            if let Some(cell) = term_buf.cell_mut((x, y)) {
                                // Only paint guide if the cell currently holds a space
                                // (i.e., leading whitespace was painted there).
                                if cell.symbol() == " " {
                                    cell.set_char(self.indent_guide_char);
                                    // Preserve bg; only set fg.
                                    let existing = cell.style();
                                    cell.set_style(existing.fg(fg));
                                }
                            }
                        }
                    }
                    guide_col += sw;
                }
                ig_screen_row += 1;
                ig_doc_row += 1;
            }
        }

        // Diag overlay pass: apply underline / style over visible char
        // columns. Only supported in Wrap::None mode; wrap is a future
        // concern. Overlays beyond the visible horizontal scroll are
        // skipped silently.
        if matches!(wrap_mode, Wrap::None) && !self.diag_overlays.is_empty() {
            // Build a doc_row → screen_row map from the first pass.
            // We re-walk the viewport range instead of storing a map to
            // keep memory allocation proportional to the viewport.
            let vp_top = top_row;
            let vp_bot = vp_top + area.height as usize;
            for overlay in self.diag_overlays {
                if overlay.row < vp_top || overlay.row >= vp_bot {
                    continue;
                }
                // Compute screen row: count non-hidden rows from vp_top
                // to overlay.row.
                let mut sr: u16 = 0;
                let mut dr = vp_top;
                while dr < overlay.row && sr < area.height {
                    if !folds.iter().any(|f| f.hides(dr)) {
                        sr += 1;
                    }
                    dr += 1;
                }
                if sr >= area.height {
                    continue;
                }
                let y = text_area.y + sr;
                // Paint the char columns in the overlay range, clamped
                // to the horizontal scroll window and text area width.
                let col_start = overlay.col_start;
                let col_end = overlay.col_end.max(col_start + 1);
                for col in col_start..col_end {
                    if col < top_col {
                        continue;
                    }
                    let screen_col = col - top_col;
                    if screen_col >= text_area.width as usize {
                        break;
                    }
                    let x = text_area.x + screen_col as u16;
                    if let Some(cell) = term_buf.cell_mut((x, y)) {
                        cell.set_style(cell.style().patch(overlay.style));
                    }
                }
            }
        }
    }
}

/// Left-frame width reserved by the boxed-blame layout (the `│`/`┌`/`└`
/// column). The host adds this when computing the cursor's screen column.
pub const BLAME_BOX_FRAME_LEFT: u16 = 1;

impl<R: StyleResolver> BufferView<'_, R> {
    /// Render the boxed-blame plan: one screen row per [`BlameRow`]. Content
    /// rows paint the buffer line (gutter + syntax/selection/search) framed by
    /// `│` sides; border rows draw the box top (with commit title) / bottom.
    /// `Wrap::None` only — blame mode requires it.
    fn render_blame_plan(&self, area: Rect, term_buf: &mut TermBuffer, plan: &[BlameRow]) {
        let viewport = *self.viewport;
        let top_col = viewport.top_col;
        let cursor = self.buffer.cursor();
        let folds = self
            .folds_override
            .map(<[hjkl_buffer::Fold]>::to_vec)
            .unwrap_or_else(|| self.buffer.folds());
        let rope = self.buffer.rope();

        let frame_x = area.x;
        let right_x = area.x + area.width.saturating_sub(1);
        // Inner region after the left frame column.
        let inner = Rect {
            x: area.x + BLAME_BOX_FRAME_LEFT,
            y: area.y,
            width: area.width.saturating_sub(BLAME_BOX_FRAME_LEFT + 1), // also drop right frame
            height: area.height,
        };
        let gutter_total = self
            .gutter
            .map(|g| g.sign_column_width + g.width + g.fold_column_width)
            .unwrap_or(0);
        let text_area = Rect {
            x: inner.x.saturating_add(gutter_total),
            y: inner.y,
            width: inner.width.saturating_sub(gutter_total),
            height: inner.height,
        };

        let border_style = self.non_text_style;
        let title_style = self
            .non_text_style
            .add_modifier(ratatui::style::Modifier::BOLD);

        for (sr, item) in plan.iter().enumerate().take(area.height as usize) {
            let screen_row = sr as u16;
            let y = area.y + screen_row;
            match item {
                BlameRow::Content(dr) => {
                    let dr = *dr;
                    let line_owned = hjkl_buffer::rope_line_str(&rope, dr);
                    let line: &str = line_owned.as_str();
                    let row_spans = self.spans.get(dr).map(Vec::as_slice).unwrap_or(&[]);
                    let sel_range = self.selection.and_then(|s| s.row_span(dr));
                    let is_cursor_row = dr == cursor.row;
                    if let Some(gutter) = self.gutter {
                        self.paint_gutter(term_buf, inner, screen_row, dr, gutter);
                        self.paint_signs(term_buf, inner, screen_row, dr, gutter);
                        self.paint_fold_column(term_buf, inner, screen_row, dr, gutter, &folds);
                    }
                    let search_ranges = self.row_search_ranges(line);
                    let row_conceals: Vec<&Conceal> = {
                        let mut v: Vec<&Conceal> =
                            self.conceals.iter().filter(|c| c.row == dr).collect();
                        v.sort_by_key(|c| c.start_byte);
                        v
                    };
                    self.paint_row(
                        term_buf,
                        text_area,
                        screen_row,
                        line,
                        row_spans,
                        sel_range,
                        &search_ranges,
                        is_cursor_row,
                        cursor.col,
                        top_col,
                        usize::MAX,
                        true,
                        &row_conceals,
                    );
                    // Side frame borders.
                    if let Some(cell) = term_buf.cell_mut((frame_x, y)) {
                        cell.set_char('\u{2502}'); // │
                        cell.set_style(border_style);
                    }
                    if let Some(cell) = term_buf.cell_mut((right_x, y)) {
                        cell.set_char('\u{2502}');
                        cell.set_style(border_style);
                    }
                }
                BlameRow::BorderTop(_) | BlameRow::BorderBottom => {
                    let (lc, rc) = match item {
                        BlameRow::BorderTop(_) => ('\u{250c}', '\u{2510}'), // ┌ ┐
                        _ => ('\u{2514}', '\u{2518}'),                      // └ ┘
                    };
                    for x in frame_x..=right_x {
                        if let Some(cell) = term_buf.cell_mut((x, y)) {
                            let ch = if x == frame_x {
                                lc
                            } else if x == right_x {
                                rc
                            } else {
                                '\u{2500}' // ─
                            };
                            cell.set_char(ch);
                            cell.set_style(border_style);
                        }
                    }
                    if let BlameRow::BorderTop(title) = item {
                        // Lay " title " over the rule, after the corner + one ─.
                        let label = format!(" {title} ");
                        let start = frame_x + 2;
                        for (i, ch) in label.chars().enumerate() {
                            let x = start + i as u16;
                            if x >= right_x {
                                break;
                            }
                            if let Some(cell) = term_buf.cell_mut((x, y)) {
                                cell.set_char(ch);
                                cell.set_style(title_style);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Run the active search regex against `line` and return the
    /// charwise `(start_col, end_col_exclusive)` ranges that need
    /// the search bg painted. Empty when no pattern is set.
    fn row_search_ranges(&self, line: &str) -> Vec<(usize, usize)> {
        let Some(re) = self.search_pattern else {
            return Vec::new();
        };
        re.find_iter(line)
            .map(|m| {
                let start = line[..m.start()].chars().count();
                let end = line[..m.end()].chars().count();
                (start, end)
            })
            .collect()
    }

    fn paint_signs(
        &self,
        term_buf: &mut TermBuffer,
        area: Rect,
        screen_row: u16,
        doc_row: usize,
        gutter: Gutter,
    ) {
        // Only paint when a sign column is reserved.
        if gutter.sign_column_width == 0 {
            return;
        }
        let y = area.y + screen_row;
        let sign_x = area.x;
        // Fill sign column cells with blank first (gutter style bg).
        for x in sign_x..sign_x + gutter.sign_column_width {
            if let Some(cell) = term_buf.cell_mut((x, y)) {
                cell.set_char(' ');
                cell.set_style(gutter.style);
            }
        }
        // Paint the highest-priority sign for this row in the leftmost cell.
        let Some(sign) = self
            .signs
            .iter()
            .filter(|s| s.row == doc_row)
            .max_by_key(|s| s.priority)
        else {
            return;
        };
        if let Some(cell) = term_buf.cell_mut((sign_x, y)) {
            cell.set_char(sign.ch);
            cell.set_style(sign.style);
        }
    }

    /// Paint a wrap-continuation gutter row: blank cells in the
    /// gutter style so the bg stays continuous, no line number.
    fn paint_blank_gutter(
        &self,
        term_buf: &mut TermBuffer,
        area: Rect,
        screen_row: u16,
        gutter: Gutter,
    ) {
        let y = area.y + screen_row;
        let total = gutter.sign_column_width + gutter.width;
        for x in area.x..(area.x + total) {
            if let Some(cell) = term_buf.cell_mut((x, y)) {
                cell.set_char(' ');
                cell.set_style(gutter.style);
            }
        }
    }

    fn paint_gutter(
        &self,
        term_buf: &mut TermBuffer,
        area: Rect,
        screen_row: u16,
        doc_row: usize,
        gutter: Gutter,
    ) {
        let y = area.y + screen_row;
        // Number column starts after the sign column.
        let num_start = area.x + gutter.sign_column_width;
        // Total gutter cells in the number column, leaving one trailing spacer.
        let number_width = gutter.width.saturating_sub(1) as usize;

        // Compute the label to display based on the numbers mode.
        let label = match gutter.numbers {
            GutterNumbers::None => {
                // Blank — paint all number-column cells (including spacer) as spaces.
                for x in num_start..(num_start + gutter.width) {
                    if let Some(cell) = term_buf.cell_mut((x, y)) {
                        cell.set_char(' ');
                        cell.set_style(gutter.style);
                    }
                }
                return;
            }
            GutterNumbers::Absolute => {
                format!(
                    "{:>width$}",
                    doc_row + 1 + gutter.line_offset,
                    width = number_width
                )
            }
            GutterNumbers::Relative { cursor_row } => {
                let n = if doc_row == cursor_row {
                    0
                } else {
                    doc_row.abs_diff(cursor_row)
                };
                format!("{:>width$}", n, width = number_width)
            }
            GutterNumbers::Hybrid { cursor_row } => {
                let n = if doc_row == cursor_row {
                    doc_row + 1 + gutter.line_offset
                } else {
                    doc_row.abs_diff(cursor_row)
                };
                format!("{:>width$}", n, width = number_width)
            }
        };

        let mut x = num_start;
        for ch in label.chars() {
            if x >= num_start + gutter.width.saturating_sub(1) {
                break;
            }
            if let Some(cell) = term_buf.cell_mut((x, y)) {
                cell.set_char(ch);
                cell.set_style(gutter.style);
            }
            x = x.saturating_add(1);
        }
        // Spacer cell — same gutter style so the background is
        // continuous when a bg colour is set.
        let spacer_x = num_start + gutter.width.saturating_sub(1);
        if let Some(cell) = term_buf.cell_mut((spacer_x, y)) {
            cell.set_char(' ');
            cell.set_style(gutter.style);
        }
    }

    /// Paint the fold-indicator column for `doc_row`. The column sits to
    /// the RIGHT of the number column and LEFT of the text
    /// (`[sign][number][fold][text]`). One glyph per row:
    ///   `▸` closed fold start · `▾` open fold start ·
    ///   `│` open fold body · ` ` otherwise.
    /// No-op when `fold_column_width == 0`.
    fn paint_fold_column(
        &self,
        term_buf: &mut TermBuffer,
        area: Rect,
        screen_row: u16,
        doc_row: usize,
        gutter: Gutter,
        folds: &[hjkl_buffer::Fold],
    ) {
        if gutter.fold_column_width == 0 {
            return;
        }
        let y = area.y + screen_row;
        let fold_x = area.x + gutter.sign_column_width + gutter.width;
        let glyph = fold_column_glyph(folds, doc_row);
        // Only the leftmost fold cell carries the glyph; any remaining
        // fold-column cells are blanked so the gutter bg stays continuous.
        for (i, x) in (fold_x..fold_x + gutter.fold_column_width).enumerate() {
            if let Some(cell) = term_buf.cell_mut((x, y)) {
                cell.set_char(if i == 0 { glyph } else { ' ' });
                cell.set_style(gutter.style);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_row(
        &self,
        term_buf: &mut TermBuffer,
        area: Rect,
        screen_row: u16,
        line: &str,
        row_spans: &[hjkl_buffer::Span],
        sel_range: hjkl_buffer::RowSpan,
        search_ranges: &[(usize, usize)],
        is_cursor_row: bool,
        cursor_col: usize,
        seg_start: usize,
        seg_end: usize,
        is_last_segment: bool,
        conceals: &[&Conceal],
    ) {
        let y = area.y + screen_row;
        let mut screen_x = area.x;
        let row_end_x = area.x + area.width;

        // Paint cursor-line bg across the whole row first so empty
        // trailing cells inherit the highlight (matches vim's
        // cursorline). Selection / cursor cells overwrite below.
        if is_cursor_row && self.cursor_line_bg != Style::default() {
            for x in area.x..row_end_x {
                if let Some(cell) = term_buf.cell_mut((x, y)) {
                    cell.set_style(self.cursor_line_bg);
                }
            }
        }

        // Tab width for `\t` expansion — host publishes via
        // `Viewport::tab_width` (driven by engine's `:set tabstop`).
        // `effective_tab_width` falls back to 4 when unset.
        let tab_width = self.viewport.effective_tab_width();

        // Precompute trailing-whitespace boundary for `:set list` trail rendering.
        // `trail_byte_start` is the byte index of the first trailing space/tab
        // at the end of the line. Bytes at or past this index are "trailing".
        let trail_byte_start: usize = if self.listchars.is_some() {
            line.trim_end_matches([' ', '\t']).len()
        } else {
            line.len()
        };

        let mut byte_offset: usize = 0;
        let mut line_col: usize = 0;
        let mut chars_iter = line.chars().enumerate().peekable();
        while let Some((col_idx, ch)) = chars_iter.next() {
            let ch_byte_len = ch.len_utf8();
            if col_idx >= seg_end {
                break;
            }
            // If a conceal starts at this byte, paint the replacement
            // text (using this cell's style) and skip the rest of the
            // concealed range. Cursor / selection / search highlights
            // still attribute to the original char positions.
            if let Some(conc) = conceals.iter().find(|c| c.start_byte == byte_offset) {
                if col_idx >= seg_start {
                    let mut style = if is_cursor_row {
                        self.cursor_line_bg
                    } else {
                        Style::default()
                    };
                    if let Some(span_style) = self.resolve_span_style(row_spans, byte_offset) {
                        style = style.patch(span_style);
                    }
                    for rch in conc.replacement.chars() {
                        let rwidth = rch.width().unwrap_or(1) as u16;
                        if screen_x + rwidth > row_end_x {
                            break;
                        }
                        if let Some(cell) = term_buf.cell_mut((screen_x, y)) {
                            cell.set_char(rch);
                            cell.set_style(style);
                        }
                        screen_x += rwidth;
                    }
                }
                // Advance byte_offset / chars iter past the concealed
                // range without painting the original cells.
                let mut consumed = ch_byte_len;
                byte_offset += ch_byte_len;
                while byte_offset < conc.end_byte {
                    let Some((_, next_ch)) = chars_iter.next() else {
                        break;
                    };
                    consumed += next_ch.len_utf8();
                    byte_offset = byte_offset.saturating_add(next_ch.len_utf8());
                }
                let _ = consumed;
                continue;
            }
            // Visible cell count: tabs expand to the next tab_width stop
            // based on `line_col` (visible column in the *line*, not the
            // segment), so a tab at line column 0 paints tab_width cells
            // and a tab at line column 3 paints 1 cell.
            let visible_width = if ch == '\t' {
                tab_width - (line_col % tab_width)
            } else {
                ch.width().unwrap_or(1)
            };
            // Skip chars to the left of the segment start (horizontal
            // scroll for `Wrap::None`, segment offset for wrap modes).
            if col_idx < seg_start {
                line_col += visible_width;
                byte_offset += ch_byte_len;
                continue;
            }
            // Stop when we run out of horizontal room.
            let width = visible_width as u16;
            if screen_x + width > row_end_x {
                break;
            }

            // Resolve final style for this cell.
            let mut style = if is_cursor_row {
                self.cursor_line_bg
            } else {
                Style::default()
            };
            if let Some(span_style) = self.resolve_span_style(row_spans, byte_offset) {
                style = style.patch(span_style);
            }
            // Search bg first, then selection bg — so when a visual
            // selection covers a search match, the selection wins
            // (last patch overwrites the bg field).
            if self.search_bg != Style::default()
                && search_ranges
                    .iter()
                    .any(|&(s, e)| col_idx >= s && col_idx < e)
            {
                style = style.patch(self.search_bg);
            }
            if let Some((lo, hi)) = sel_range
                && col_idx >= lo
                && col_idx <= hi
            {
                style = style.patch(self.selection_bg);
            }
            if is_cursor_row && col_idx == cursor_col {
                style = style.patch(self.cursor_style);
            }

            if ch == '\t' {
                // Paint tab expansion. When `:set list` is on, substitute
                // the leading cell with `tab_lead` and fill cells with
                // `tab_fill` (or spaces when tab_fill is None).
                if let Some(lc) = self.listchars {
                    // tab_lead occupies the first cell
                    if let Some(cell) = term_buf.cell_mut((screen_x, y)) {
                        cell.set_char(lc.tab_lead);
                        cell.set_style(style);
                    }
                    let fill_ch = lc.tab_fill.unwrap_or(' ');
                    for k in 1..width {
                        if let Some(cell) = term_buf.cell_mut((screen_x + k, y)) {
                            cell.set_char(fill_ch);
                            cell.set_style(style);
                        }
                    }
                } else {
                    // Default: paint tab as `visible_width` space cells carrying
                    // the resolved style — tab/text bg/cursor-line bg all paint
                    // through the expansion.
                    for k in 0..width {
                        if let Some(cell) = term_buf.cell_mut((screen_x + k, y)) {
                            cell.set_char(' ');
                            cell.set_style(style);
                        }
                    }
                }
            } else if let Some(lc) = self.listchars {
                // Listchars substitutions for non-tab characters.
                let display_ch = if ch == '\u{00a0}' {
                    // Non-breaking space
                    lc.nbsp.unwrap_or(ch)
                } else if ch == ' ' {
                    let is_trailing = byte_offset >= trail_byte_start;
                    if is_trailing {
                        lc.trail.or(lc.space).unwrap_or(ch)
                    } else {
                        lc.space.unwrap_or(ch)
                    }
                } else {
                    ch
                };
                if let Some(cell) = term_buf.cell_mut((screen_x, y)) {
                    cell.set_char(display_ch);
                    cell.set_style(style);
                }
            } else if let Some(cell) = term_buf.cell_mut((screen_x, y)) {
                cell.set_char(ch);
                cell.set_style(style);
            }
            screen_x += width;
            line_col += visible_width;
            byte_offset += ch_byte_len;
        }

        // When `:set list` is on and `eol` is configured, paint the EOL marker
        // immediately after the last character on the last segment.
        if is_last_segment
            && let Some(lc) = self.listchars
            && let Some(eol_ch) = lc.eol
            && screen_x < row_end_x
            && let Some(cell) = term_buf.cell_mut((screen_x, y))
        {
            cell.set_char(eol_ch);
            cell.set_style(Style::default());
            screen_x += 1;
        }
        let _ = screen_x; // consumed by eol branch above; not used further

        // Empty-line selection placeholder. Without this, an empty row
        // covered by a v/V/Ctrl-V selection paints zero cells — the user
        // loses the visible marker that the row is part of the range.
        //
        // For Char/Line selections, `hi == usize::MAX` (the "whole row"
        // sentinel from `Selection::row_span`); paint a single ' ' cell
        // at col 0 — matches Neovim's marker on otherwise-empty rows.
        //
        // For Block selections, `hi` is bounded by the block's right
        // column; paint cols `lo..=hi` so the block stays visually
        // rectangular even where rows have no chars (also matches
        // Neovim).
        //
        // Layered before the cursor-EOL placeholder so the terminal
        // cursor still wins visually.
        if let Some((lo, hi)) = sel_range
            && is_last_segment
            && line.chars().count() <= seg_start
        {
            let (start_col, end_col) = if hi == usize::MAX { (0, 0) } else { (lo, hi) };
            for col in start_col..=end_col {
                let pad_x = area.x + col as u16;
                if pad_x >= row_end_x {
                    break;
                }
                if let Some(cell) = term_buf.cell_mut((pad_x, y)) {
                    let prev = cell.style();
                    cell.set_char(' ');
                    cell.set_style(prev.patch(self.selection_bg));
                }
            }
        }

        // If the cursor sits at end-of-line (insert / past-end mode),
        // paint a single REVERSED placeholder cell so it stays visible.
        // Only on the last segment of a wrapped row — earlier segments
        // can't host the past-end cursor.
        if is_cursor_row
            && is_last_segment
            && cursor_col >= line.chars().count()
            && cursor_col >= seg_start
        {
            let pad_x = area.x + (cursor_col.saturating_sub(seg_start)) as u16;
            if pad_x < row_end_x
                && let Some(cell) = term_buf.cell_mut((pad_x, y))
            {
                cell.set_char(' ');
                cell.set_style(self.cursor_line_bg.patch(self.cursor_style));
            }
        }
    }

    /// Resolve the final style for a byte by layering every span that
    /// contains it, broadest first and narrowest last. `Style::patch` keeps
    /// the broader span's fields when the narrower span doesn't override
    /// them, so a wide `@markup.raw.block` carrying just `bg = codeblock`
    /// shines through under a narrow `@keyword` carrying just `fg = mauve`,
    /// matching vim/Helix's layered hi-group model.
    ///
    /// Pre-0.6.1 behaviour was narrowest-wins-completely: only one span's
    /// style applied per byte, so broader-span backgrounds were dropped
    /// whenever a narrower foreground span overlapped them. That made it
    /// impossible to give markdown code blocks a tinted bg without also
    /// burdening every injected language's captures with the same bg.
    ///
    /// Hosts that want the old behaviour can ensure their narrower spans
    /// set every field explicitly — `Style::patch` only carries broader
    /// fields through `None` slots.
    fn resolve_span_style(
        &self,
        row_spans: &[hjkl_buffer::Span],
        byte_offset: usize,
    ) -> Option<Style> {
        // Collect every span containing this byte, sorted broadest first.
        let mut overlapping: Vec<&hjkl_buffer::Span> = row_spans
            .iter()
            .filter(|s| byte_offset >= s.start_byte && byte_offset < s.end_byte)
            .collect();
        if overlapping.is_empty() {
            return None;
        }
        overlapping.sort_by_key(|s| std::cmp::Reverse(s.end_byte.saturating_sub(s.start_byte)));
        let mut style = self.resolver.resolve(overlapping[0].style);
        for s in &overlapping[1..] {
            style = style.patch(self.resolver.resolve(s.style));
        }
        Some(style)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Modifier};
    use ratatui::widgets::Widget;

    fn run_render<R: StyleResolver>(view: BufferView<'_, R>, w: u16, h: u16) -> TermBuffer {
        let area = Rect::new(0, 0, w, h);
        let mut buf = TermBuffer::empty(area);
        view.render(area, &mut buf);
        buf
    }

    fn no_styles(_id: u32) -> Style {
        Style::default()
    }

    /// Build a default viewport for plain (no-wrap) tests.
    fn vp(width: u16, height: u16) -> Viewport {
        Viewport {
            top_row: 0,
            top_col: 0,
            width,
            height,
            wrap: Wrap::None,
            text_width: width,
            tab_width: 0,
        }
    }

    #[test]
    fn renders_plain_chars_into_terminal_buffer() {
        let b = Buffer::from_str("hello\nworld");
        let v = vp(20, 5);
        let view = BufferView {
            buffer: &b,
            viewport: &v,
            selection: None,
            resolver: &(no_styles as fn(u32) -> Style),
            cursor_line_bg: Style::default(),
            cursor_line_row: None,
            cursor_column_bg: Style::default(),
            selection_bg: Style::default().bg(Color::Blue),
            cursor_style: Style::default().add_modifier(Modifier::REVERSED),
            gutter: None,
            search_bg: Style::default(),
            signs: &[],
            conceals: &[],
            spans: &[],
            search_pattern: None,
            non_text_style: Style::default(),
            show_eob: true,
            diag_overlays: &[],
            colorcolumn_cols: &[],
            colorcolumn_style: Style::default(),
            listchars: None,
            indent_guides_enabled: false,
            indent_guide_char: '│',
            indent_guide_shiftwidth: 4,
            indent_guide_fg: Color::Reset,
            indent_guide_active_fg: Color::Reset,
            indent_guide_active_col: None,
            fold_line_bg: Style::default(),
            folds_override: None,
            eol_hints: &[],
            blame_plan: None,
        };
        let term = run_render(view, 20, 5);
        assert_eq!(term.cell((0, 0)).unwrap().symbol(), "h");
        assert_eq!(term.cell((4, 0)).unwrap().symbol(), "o");
        assert_eq!(term.cell((0, 1)).unwrap().symbol(), "w");
        assert_eq!(term.cell((4, 1)).unwrap().symbol(), "d");
    }

    #[test]
    fn cursor_cell_gets_reversed_style() {
        let mut b = Buffer::from_str("abc");
        let v = vp(10, 1);
        b.set_cursor(hjkl_buffer::Position::new(0, 1));
        let view = BufferView {
            buffer: &b,
            viewport: &v,
            selection: None,
            resolver: &(no_styles as fn(u32) -> Style),
            cursor_line_bg: Style::default(),
            cursor_line_row: None,
            cursor_column_bg: Style::default(),
            selection_bg: Style::default().bg(Color::Blue),
            cursor_style: Style::default().add_modifier(Modifier::REVERSED),
            gutter: None,
            search_bg: Style::default(),
            signs: &[],
            conceals: &[],
            spans: &[],
            search_pattern: None,
            non_text_style: Style::default(),
            show_eob: true,
            diag_overlays: &[],
            colorcolumn_cols: &[],
            colorcolumn_style: Style::default(),
            listchars: None,
            indent_guides_enabled: false,
            indent_guide_char: '│',
            indent_guide_shiftwidth: 4,
            indent_guide_fg: Color::Reset,
            indent_guide_active_fg: Color::Reset,
            indent_guide_active_col: None,
            fold_line_bg: Style::default(),
            folds_override: None,
            eol_hints: &[],
            blame_plan: None,
        };
        let term = run_render(view, 10, 1);
        let cursor_cell = term.cell((1, 0)).unwrap();
        assert!(cursor_cell.modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn selection_bg_applies_only_to_selected_cells() {
        use hjkl_buffer::{Position, Selection};
        let b = Buffer::from_str("abcdef");
        let v = vp(10, 1);
        let view = BufferView {
            buffer: &b,
            viewport: &v,
            selection: Some(Selection::Char {
                anchor: Position::new(0, 1),
                head: Position::new(0, 3),
            }),
            resolver: &(no_styles as fn(u32) -> Style),
            cursor_line_bg: Style::default(),
            cursor_line_row: None,
            cursor_column_bg: Style::default(),
            selection_bg: Style::default().bg(Color::Blue),
            cursor_style: Style::default().add_modifier(Modifier::REVERSED),
            gutter: None,
            search_bg: Style::default(),
            signs: &[],
            conceals: &[],
            spans: &[],
            search_pattern: None,
            non_text_style: Style::default(),
            show_eob: true,
            diag_overlays: &[],
            colorcolumn_cols: &[],
            colorcolumn_style: Style::default(),
            listchars: None,
            indent_guides_enabled: false,
            indent_guide_char: '│',
            indent_guide_shiftwidth: 4,
            indent_guide_fg: Color::Reset,
            indent_guide_active_fg: Color::Reset,
            indent_guide_active_col: None,
            fold_line_bg: Style::default(),
            folds_override: None,
            eol_hints: &[],
            blame_plan: None,
        };
        let term = run_render(view, 10, 1);
        assert!(term.cell((0, 0)).unwrap().bg != Color::Blue);
        for x in 1..=3 {
            assert_eq!(term.cell((x, 0)).unwrap().bg, Color::Blue);
        }
        assert!(term.cell((4, 0)).unwrap().bg != Color::Blue);
    }

    #[test]
    fn selection_paints_placeholder_on_empty_line_charwise() {
        // Char selection spanning two lines, middle empty row must show
        // a selection cell at col 0 so the user can see the row is in range.
        use hjkl_buffer::{Position, Selection};
        let b = Buffer::from_str("abc\n\nxyz");
        let v = vp(10, 3);
        let view = BufferView {
            buffer: &b,
            viewport: &v,
            selection: Some(Selection::Char {
                anchor: Position::new(0, 0),
                head: Position::new(2, 2),
            }),
            resolver: &(no_styles as fn(u32) -> Style),
            cursor_line_bg: Style::default(),
            cursor_line_row: None,
            cursor_column_bg: Style::default(),
            selection_bg: Style::default().bg(Color::Blue),
            cursor_style: Style::default().add_modifier(Modifier::REVERSED),
            gutter: None,
            search_bg: Style::default(),
            signs: &[],
            conceals: &[],
            spans: &[],
            search_pattern: None,
            non_text_style: Style::default(),
            show_eob: true,
            diag_overlays: &[],
            colorcolumn_cols: &[],
            colorcolumn_style: Style::default(),
            listchars: None,
            indent_guides_enabled: false,
            indent_guide_char: '│',
            indent_guide_shiftwidth: 4,
            indent_guide_fg: Color::Reset,
            indent_guide_active_fg: Color::Reset,
            indent_guide_active_col: None,
            fold_line_bg: Style::default(),
            folds_override: None,
            eol_hints: &[],
            blame_plan: None,
        };
        let term = run_render(view, 10, 3);
        // Empty middle row (y=1) — col 0 must carry the selection bg.
        assert_eq!(term.cell((0, 1)).unwrap().bg, Color::Blue);
    }

    #[test]
    fn selection_paints_placeholder_on_empty_line_linewise() {
        use hjkl_buffer::Selection;
        let b = Buffer::from_str("abc\n\nxyz");
        let v = vp(10, 3);
        let view = BufferView {
            buffer: &b,
            viewport: &v,
            selection: Some(Selection::Line {
                anchor_row: 0,
                head_row: 2,
            }),
            resolver: &(no_styles as fn(u32) -> Style),
            cursor_line_bg: Style::default(),
            cursor_line_row: None,
            cursor_column_bg: Style::default(),
            selection_bg: Style::default().bg(Color::Blue),
            cursor_style: Style::default().add_modifier(Modifier::REVERSED),
            gutter: None,
            search_bg: Style::default(),
            signs: &[],
            conceals: &[],
            spans: &[],
            search_pattern: None,
            non_text_style: Style::default(),
            show_eob: true,
            diag_overlays: &[],
            colorcolumn_cols: &[],
            colorcolumn_style: Style::default(),
            listchars: None,
            indent_guides_enabled: false,
            indent_guide_char: '│',
            indent_guide_shiftwidth: 4,
            indent_guide_fg: Color::Reset,
            indent_guide_active_fg: Color::Reset,
            indent_guide_active_col: None,
            fold_line_bg: Style::default(),
            folds_override: None,
            eol_hints: &[],
            blame_plan: None,
        };
        let term = run_render(view, 10, 3);
        assert_eq!(term.cell((0, 1)).unwrap().bg, Color::Blue);
    }

    #[test]
    fn selection_paints_placeholder_on_empty_line_blockwise() {
        // Block selection at cols 2..=5 over rows 0..=2 with empty middle.
        // The empty row must paint cols 2..=5 (the block's full width),
        // NOT just col 0 — otherwise the block looks broken at empty
        // rows. Matches Neovim's rectangular block highlight.
        use hjkl_buffer::{Position, Selection};
        let b = Buffer::from_str("abcdef\n\nuvwxyz");
        let v = vp(10, 3);
        let view = BufferView {
            buffer: &b,
            viewport: &v,
            selection: Some(Selection::Block {
                anchor: Position::new(0, 2),
                head: Position::new(2, 5),
            }),
            resolver: &(no_styles as fn(u32) -> Style),
            cursor_line_bg: Style::default(),
            cursor_line_row: None,
            cursor_column_bg: Style::default(),
            selection_bg: Style::default().bg(Color::Blue),
            cursor_style: Style::default().add_modifier(Modifier::REVERSED),
            gutter: None,
            search_bg: Style::default(),
            signs: &[],
            conceals: &[],
            spans: &[],
            search_pattern: None,
            non_text_style: Style::default(),
            show_eob: true,
            diag_overlays: &[],
            colorcolumn_cols: &[],
            colorcolumn_style: Style::default(),
            listchars: None,
            indent_guides_enabled: false,
            indent_guide_char: '│',
            indent_guide_shiftwidth: 4,
            indent_guide_fg: Color::Reset,
            indent_guide_active_fg: Color::Reset,
            indent_guide_active_col: None,
            fold_line_bg: Style::default(),
            folds_override: None,
            eol_hints: &[],
            blame_plan: None,
        };
        let term = run_render(view, 10, 3);
        // Empty row (y=1): cols 2..=5 carry selection bg (block width).
        for x in 2u16..=5 {
            assert_eq!(
                term.cell((x, 1)).unwrap().bg,
                Color::Blue,
                "empty row col {x} should carry block selection bg"
            );
        }
        // Col 0 and 1 on empty row MUST NOT carry selection bg — block
        // starts at col 2.
        assert!(term.cell((0, 1)).unwrap().bg != Color::Blue);
        assert!(term.cell((1, 1)).unwrap().bg != Color::Blue);
        // Col 6 (just past block right edge) also clear.
        assert!(term.cell((6, 1)).unwrap().bg != Color::Blue);
        // Non-empty rows still highlight cols 2..=5.
        for x in 2u16..=5 {
            assert_eq!(term.cell((x, 0)).unwrap().bg, Color::Blue);
        }
    }

    #[test]
    fn selection_block_placeholder_clips_to_row_width() {
        // Block right edge past row width must stop at row_end_x.
        use hjkl_buffer::{Position, Selection};
        let b = Buffer::from_str("abc\n\nxyz");
        let v = vp(5, 3);
        let view = BufferView {
            buffer: &b,
            viewport: &v,
            selection: Some(Selection::Block {
                anchor: Position::new(0, 1),
                head: Position::new(2, 20),
            }),
            resolver: &(no_styles as fn(u32) -> Style),
            cursor_line_bg: Style::default(),
            cursor_line_row: None,
            cursor_column_bg: Style::default(),
            selection_bg: Style::default().bg(Color::Blue),
            cursor_style: Style::default(),
            gutter: None,
            search_bg: Style::default(),
            signs: &[],
            conceals: &[],
            spans: &[],
            search_pattern: None,
            non_text_style: Style::default(),
            show_eob: true,
            diag_overlays: &[],
            colorcolumn_cols: &[],
            colorcolumn_style: Style::default(),
            listchars: None,
            indent_guides_enabled: false,
            indent_guide_char: '│',
            indent_guide_shiftwidth: 4,
            indent_guide_fg: Color::Reset,
            indent_guide_active_fg: Color::Reset,
            indent_guide_active_col: None,
            fold_line_bg: Style::default(),
            folds_override: None,
            eol_hints: &[],
            blame_plan: None,
        };
        // 5-wide area; block lo=1, hi=20 → paint cols 1..=4 (rest clipped).
        let term = run_render(view, 5, 3);
        for x in 1u16..=4 {
            assert_eq!(
                term.cell((x, 1)).unwrap().bg,
                Color::Blue,
                "col {x} clipped block placeholder"
            );
        }
        // No panic from pad_x past row_end_x is the main thing.
    }

    #[test]
    fn layered_spans_blend_broad_bg_with_narrow_fg() {
        // Regression: a wide `@markup.raw.block`-style span carrying only
        // `bg = ...` must shine through a narrow `@keyword`-style span
        // carrying only `fg = ...`. Pre-0.6.1 the narrow span won outright
        // and dropped the broad bg, which made markdown code-block tinting
        // impossible without bloating every injected language's captures.
        use hjkl_buffer::Span;
        let b = Buffer::from_str("fn main() {}");
        let v = vp(20, 1);
        // id=1 = broad code-block bg, id=2 = narrow keyword fg.
        let spans = vec![vec![
            Span::new(0, 12, 1), // bg-only, whole line
            Span::new(0, 2, 2),  // fg-only, just "fn"
        ]];
        let resolver = |id: u32| -> Style {
            match id {
                1 => Style::default().bg(Color::DarkGray),
                2 => Style::default().fg(Color::Magenta),
                _ => Style::default(),
            }
        };
        let view = BufferView {
            buffer: &b,
            viewport: &v,
            selection: None,
            resolver: &resolver,
            cursor_line_bg: Style::default(),
            cursor_line_row: None,
            cursor_column_bg: Style::default(),
            selection_bg: Style::default().bg(Color::Blue),
            cursor_style: Style::default().add_modifier(Modifier::REVERSED),
            gutter: None,
            search_bg: Style::default(),
            signs: &[],
            conceals: &[],
            spans: &spans,
            search_pattern: None,
            non_text_style: Style::default(),
            show_eob: true,
            diag_overlays: &[],
            colorcolumn_cols: &[],
            colorcolumn_style: Style::default(),
            listchars: None,
            indent_guides_enabled: false,
            indent_guide_char: '│',
            indent_guide_shiftwidth: 4,
            indent_guide_fg: Color::Reset,
            indent_guide_active_fg: Color::Reset,
            indent_guide_active_col: None,
            fold_line_bg: Style::default(),
            folds_override: None,
            eol_hints: &[],
            blame_plan: None,
        };
        let term = run_render(view, 20, 1);
        // Cols 0-1 ("fn"): narrow fg + broad bg.
        for x in 0u16..2 {
            let cell = term.cell((x, 0)).unwrap();
            assert_eq!(cell.fg, Color::Magenta, "col {x}: fg from narrow span");
            assert_eq!(cell.bg, Color::DarkGray, "col {x}: bg from broad span");
        }
        // Cols 2-11 (" main() {}"): broad bg only, no fg set.
        for x in 2u16..12 {
            let cell = term.cell((x, 0)).unwrap();
            assert_eq!(cell.bg, Color::DarkGray, "col {x}: bg from broad span");
            assert_eq!(
                cell.fg,
                Color::Reset,
                "col {x}: no fg set (broad span is bg-only)"
            );
        }
    }

    #[test]
    fn narrow_span_with_explicit_bg_still_overrides_broad_bg() {
        // Regression: a narrow span that DOES set bg must override the
        // broader span's bg. Earlier "narrowest-wins-completely" behaviour
        // had this trivially; the new layered logic relies on
        // `Style::patch` overriding only set fields, so we pin it.
        use hjkl_buffer::Span;
        let b = Buffer::from_str("hello world");
        let v = vp(20, 1);
        let spans = vec![vec![
            Span::new(0, 11, 1), // broad bg = DarkGray
            Span::new(6, 11, 2), // narrow bg = Red (overrides)
        ]];
        let resolver = |id: u32| -> Style {
            match id {
                1 => Style::default().bg(Color::DarkGray),
                2 => Style::default().bg(Color::Red),
                _ => Style::default(),
            }
        };
        let view = BufferView {
            buffer: &b,
            viewport: &v,
            selection: None,
            resolver: &resolver,
            cursor_line_bg: Style::default(),
            cursor_line_row: None,
            cursor_column_bg: Style::default(),
            selection_bg: Style::default().bg(Color::Blue),
            cursor_style: Style::default().add_modifier(Modifier::REVERSED),
            gutter: None,
            search_bg: Style::default(),
            signs: &[],
            conceals: &[],
            spans: &spans,
            search_pattern: None,
            non_text_style: Style::default(),
            show_eob: true,
            diag_overlays: &[],
            colorcolumn_cols: &[],
            colorcolumn_style: Style::default(),
            listchars: None,
            indent_guides_enabled: false,
            indent_guide_char: '│',
            indent_guide_shiftwidth: 4,
            indent_guide_fg: Color::Reset,
            indent_guide_active_fg: Color::Reset,
            indent_guide_active_col: None,
            fold_line_bg: Style::default(),
            folds_override: None,
            eol_hints: &[],
            blame_plan: None,
        };
        let term = run_render(view, 20, 1);
        // Cols 0-5 ("hello "): broad bg only.
        for x in 0u16..6 {
            assert_eq!(term.cell((x, 0)).unwrap().bg, Color::DarkGray);
        }
        // Cols 6-10 ("world"): narrow bg wins.
        for x in 6u16..11 {
            assert_eq!(
                term.cell((x, 0)).unwrap().bg,
                Color::Red,
                "col {x}: narrow span's bg overrides broad bg"
            );
        }
    }

    #[test]
    fn syntax_span_fg_resolves_via_table() {
        use hjkl_buffer::Span;
        let b = Buffer::from_str("SELECT foo");
        let v = vp(20, 1);
        let spans = vec![vec![Span::new(0, 6, 7)]];
        let resolver = |id: u32| -> Style {
            if id == 7 {
                Style::default().fg(Color::Red)
            } else {
                Style::default()
            }
        };
        let view = BufferView {
            buffer: &b,
            viewport: &v,
            selection: None,
            resolver: &resolver,
            cursor_line_bg: Style::default(),
            cursor_line_row: None,
            cursor_column_bg: Style::default(),
            selection_bg: Style::default().bg(Color::Blue),
            cursor_style: Style::default().add_modifier(Modifier::REVERSED),
            gutter: None,
            search_bg: Style::default(),
            signs: &[],
            conceals: &[],
            spans: &spans,
            search_pattern: None,
            non_text_style: Style::default(),
            show_eob: true,
            diag_overlays: &[],
            colorcolumn_cols: &[],
            colorcolumn_style: Style::default(),
            listchars: None,
            indent_guides_enabled: false,
            indent_guide_char: '│',
            indent_guide_shiftwidth: 4,
            indent_guide_fg: Color::Reset,
            indent_guide_active_fg: Color::Reset,
            indent_guide_active_col: None,
            fold_line_bg: Style::default(),
            folds_override: None,
            eol_hints: &[],
            blame_plan: None,
        };
        let term = run_render(view, 20, 1);
        for x in 0..6 {
            assert_eq!(term.cell((x, 0)).unwrap().fg, Color::Red);
        }
    }

    #[test]
    fn gutter_renders_right_aligned_line_numbers() {
        let b = Buffer::from_str("a\nb\nc");
        let v = vp(10, 3);
        let view = BufferView {
            buffer: &b,
            viewport: &v,
            selection: None,
            resolver: &(no_styles as fn(u32) -> Style),
            cursor_line_bg: Style::default(),
            cursor_line_row: None,
            cursor_column_bg: Style::default(),
            selection_bg: Style::default().bg(Color::Blue),
            cursor_style: Style::default().add_modifier(Modifier::REVERSED),
            gutter: Some(Gutter {
                width: 4,
                style: Style::default().fg(Color::Yellow),
                line_offset: 0,
                ..Default::default()
            }),
            search_bg: Style::default(),
            signs: &[],
            conceals: &[],
            spans: &[],
            search_pattern: None,
            non_text_style: Style::default(),
            show_eob: true,
            diag_overlays: &[],
            colorcolumn_cols: &[],
            colorcolumn_style: Style::default(),
            listchars: None,
            indent_guides_enabled: false,
            indent_guide_char: '│',
            indent_guide_shiftwidth: 4,
            indent_guide_fg: Color::Reset,
            indent_guide_active_fg: Color::Reset,
            indent_guide_active_col: None,
            fold_line_bg: Style::default(),
            folds_override: None,
            eol_hints: &[],
            blame_plan: None,
        };
        let term = run_render(view, 10, 3);
        // Width 4 = 3 number cells + 1 spacer; right-aligned "  1".
        assert_eq!(term.cell((2, 0)).unwrap().symbol(), "1");
        assert_eq!(term.cell((2, 0)).unwrap().fg, Color::Yellow);
        assert_eq!(term.cell((2, 1)).unwrap().symbol(), "2");
        assert_eq!(term.cell((2, 2)).unwrap().symbol(), "3");
        // Text shifted right past the gutter.
        assert_eq!(term.cell((4, 0)).unwrap().symbol(), "a");
    }

    #[test]
    fn gutter_renders_relative_with_cursor_at_zero() {
        // 5 rows, cursor on row 2 (0-based). Relative: row 2 → 0, row 0 → 2, row 4 → 2.
        let mut b = Buffer::from_str("a\nb\nc\nd\ne");
        b.set_cursor(hjkl_buffer::Position::new(2, 0));
        let v = vp(10, 5);
        let view = BufferView {
            buffer: &b,
            viewport: &v,
            selection: None,
            resolver: &(no_styles as fn(u32) -> Style),
            cursor_line_bg: Style::default(),
            cursor_line_row: None,
            cursor_column_bg: Style::default(),
            selection_bg: Style::default().bg(Color::Blue),
            cursor_style: Style::default().add_modifier(Modifier::REVERSED),
            gutter: Some(Gutter {
                width: 4,
                style: Style::default().fg(Color::Yellow),
                line_offset: 0,
                numbers: GutterNumbers::Relative { cursor_row: 2 },
                sign_column_width: 0,
                fold_column_width: 0,
            }),
            search_bg: Style::default(),
            signs: &[],
            conceals: &[],
            spans: &[],
            search_pattern: None,
            non_text_style: Style::default(),
            show_eob: true,
            diag_overlays: &[],
            colorcolumn_cols: &[],
            colorcolumn_style: Style::default(),
            listchars: None,
            indent_guides_enabled: false,
            indent_guide_char: '│',
            indent_guide_shiftwidth: 4,
            indent_guide_fg: Color::Reset,
            indent_guide_active_fg: Color::Reset,
            indent_guide_active_col: None,
            fold_line_bg: Style::default(),
            folds_override: None,
            eol_hints: &[],
            blame_plan: None,
        };
        let term = run_render(view, 10, 5);
        // Width 4 = 3 number cells + 1 spacer.
        // Row 0 (doc 0): distance from cursor (2) = 2 → "  2"
        assert_eq!(term.cell((2, 0)).unwrap().symbol(), "2");
        // Row 1 (doc 1): distance = 1 → "  1"
        assert_eq!(term.cell((2, 1)).unwrap().symbol(), "1");
        // Row 2 (doc 2): cursor row → "  0"
        assert_eq!(term.cell((2, 2)).unwrap().symbol(), "0");
        // Row 3 (doc 3): distance = 1 → "  1"
        assert_eq!(term.cell((2, 3)).unwrap().symbol(), "1");
        // Row 4 (doc 4): distance = 2 → "  2"
        assert_eq!(term.cell((2, 4)).unwrap().symbol(), "2");
    }

    #[test]
    fn gutter_renders_hybrid_cursor_row_absolute() {
        // 3 rows, cursor on row 1 (0-based). Hybrid: row 1 → absolute (2),
        // row 0 → offset 1, row 2 → offset 1.
        let mut b = Buffer::from_str("a\nb\nc");
        b.set_cursor(hjkl_buffer::Position::new(1, 0));
        let v = vp(10, 3);
        let view = BufferView {
            buffer: &b,
            viewport: &v,
            selection: None,
            resolver: &(no_styles as fn(u32) -> Style),
            cursor_line_bg: Style::default(),
            cursor_line_row: None,
            cursor_column_bg: Style::default(),
            selection_bg: Style::default().bg(Color::Blue),
            cursor_style: Style::default().add_modifier(Modifier::REVERSED),
            gutter: Some(Gutter {
                width: 4,
                style: Style::default().fg(Color::Yellow),
                line_offset: 0,
                numbers: GutterNumbers::Hybrid { cursor_row: 1 },
                sign_column_width: 0,
                fold_column_width: 0,
            }),
            search_bg: Style::default(),
            signs: &[],
            conceals: &[],
            spans: &[],
            search_pattern: None,
            non_text_style: Style::default(),
            show_eob: true,
            diag_overlays: &[],
            colorcolumn_cols: &[],
            colorcolumn_style: Style::default(),
            listchars: None,
            indent_guides_enabled: false,
            indent_guide_char: '│',
            indent_guide_shiftwidth: 4,
            indent_guide_fg: Color::Reset,
            indent_guide_active_fg: Color::Reset,
            indent_guide_active_col: None,
            fold_line_bg: Style::default(),
            folds_override: None,
            eol_hints: &[],
            blame_plan: None,
        };
        let term = run_render(view, 10, 3);
        // Row 0 (doc 0): offset from cursor row 1 → 1
        assert_eq!(term.cell((2, 0)).unwrap().symbol(), "1");
        // Row 1 (doc 1): cursor row → absolute 2
        assert_eq!(term.cell((2, 1)).unwrap().symbol(), "2");
        // Row 2 (doc 2): offset from cursor row 1 → 1
        assert_eq!(term.cell((2, 2)).unwrap().symbol(), "1");
    }

    #[test]
    fn gutter_none_paints_blank_cells() {
        let b = Buffer::from_str("a\nb\nc");
        let v = vp(10, 3);
        let view = BufferView {
            buffer: &b,
            viewport: &v,
            selection: None,
            resolver: &(no_styles as fn(u32) -> Style),
            cursor_line_bg: Style::default(),
            cursor_line_row: None,
            cursor_column_bg: Style::default(),
            selection_bg: Style::default().bg(Color::Blue),
            cursor_style: Style::default().add_modifier(Modifier::REVERSED),
            gutter: Some(Gutter {
                width: 4,
                style: Style::default().fg(Color::Yellow),
                line_offset: 0,
                numbers: GutterNumbers::None,
                sign_column_width: 0,
                fold_column_width: 0,
            }),
            search_bg: Style::default(),
            signs: &[],
            conceals: &[],
            spans: &[],
            search_pattern: None,
            non_text_style: Style::default(),
            show_eob: true,
            diag_overlays: &[],
            colorcolumn_cols: &[],
            colorcolumn_style: Style::default(),
            listchars: None,
            indent_guides_enabled: false,
            indent_guide_char: '│',
            indent_guide_shiftwidth: 4,
            indent_guide_fg: Color::Reset,
            indent_guide_active_fg: Color::Reset,
            indent_guide_active_col: None,
            fold_line_bg: Style::default(),
            folds_override: None,
            eol_hints: &[],
            blame_plan: None,
        };
        let term = run_render(view, 10, 3);
        // All gutter cells (0..4) on every row should be blank spaces.
        for row in 0..3u16 {
            for x in 0..4u16 {
                assert_eq!(
                    term.cell((x, row)).unwrap().symbol(),
                    " ",
                    "expected blank at ({x}, {row})"
                );
            }
        }
        // Text still appears shifted right past the gutter.
        assert_eq!(term.cell((4, 0)).unwrap().symbol(), "a");
    }

    #[test]
    fn search_bg_paints_match_cells() {
        use regex::Regex;
        let b = Buffer::from_str("foo bar foo");
        let v = vp(20, 1);
        let pat = Regex::new("foo").unwrap();
        let view = BufferView {
            buffer: &b,
            viewport: &v,
            selection: None,
            resolver: &(no_styles as fn(u32) -> Style),
            cursor_line_bg: Style::default(),
            cursor_line_row: None,
            cursor_column_bg: Style::default(),
            selection_bg: Style::default().bg(Color::Blue),
            cursor_style: Style::default().add_modifier(Modifier::REVERSED),
            gutter: None,
            search_bg: Style::default().bg(Color::Magenta),
            signs: &[],
            conceals: &[],
            spans: &[],
            search_pattern: Some(&pat),
            non_text_style: Style::default(),
            show_eob: true,
            diag_overlays: &[],
            colorcolumn_cols: &[],
            colorcolumn_style: Style::default(),
            listchars: None,
            indent_guides_enabled: false,
            indent_guide_char: '│',
            indent_guide_shiftwidth: 4,
            indent_guide_fg: Color::Reset,
            indent_guide_active_fg: Color::Reset,
            indent_guide_active_col: None,
            fold_line_bg: Style::default(),
            folds_override: None,
            eol_hints: &[],
            blame_plan: None,
        };
        let term = run_render(view, 20, 1);
        for x in 0..3 {
            assert_eq!(term.cell((x, 0)).unwrap().bg, Color::Magenta);
        }
        // " bar " between matches stays default bg.
        assert_ne!(term.cell((3, 0)).unwrap().bg, Color::Magenta);
        for x in 8..11 {
            assert_eq!(term.cell((x, 0)).unwrap().bg, Color::Magenta);
        }
    }

    #[test]
    fn search_bg_survives_cursorcolumn_overlay() {
        use regex::Regex;
        // Cursor sits on a `/foo` match. The cursorcolumn pass would
        // otherwise overwrite the search bg with column bg — verify
        // the match cells keep their search colour.
        let mut b = Buffer::from_str("foo bar foo");
        let v = vp(20, 1);
        let pat = Regex::new("foo").unwrap();
        // Cursor on column 1 (inside first `foo` match).
        b.set_cursor(hjkl_buffer::Position::new(0, 1));
        let view = BufferView {
            buffer: &b,
            viewport: &v,
            selection: None,
            resolver: &(no_styles as fn(u32) -> Style),
            cursor_line_bg: Style::default(),
            cursor_line_row: None,
            cursor_column_bg: Style::default().bg(Color::DarkGray),
            selection_bg: Style::default().bg(Color::Blue),
            cursor_style: Style::default().add_modifier(Modifier::REVERSED),
            gutter: None,
            search_bg: Style::default().bg(Color::Magenta),
            signs: &[],
            conceals: &[],
            spans: &[],
            search_pattern: Some(&pat),
            non_text_style: Style::default(),
            show_eob: true,
            diag_overlays: &[],
            colorcolumn_cols: &[],
            colorcolumn_style: Style::default(),
            listchars: None,
            indent_guides_enabled: false,
            indent_guide_char: '│',
            indent_guide_shiftwidth: 4,
            indent_guide_fg: Color::Reset,
            indent_guide_active_fg: Color::Reset,
            indent_guide_active_col: None,
            fold_line_bg: Style::default(),
            folds_override: None,
            eol_hints: &[],
            blame_plan: None,
        };
        let term = run_render(view, 20, 1);
        // Cursor cell at (1, 0) is in the search match. Search wins.
        assert_eq!(term.cell((1, 0)).unwrap().bg, Color::Magenta);
    }

    #[test]
    fn highest_priority_sign_wins_per_row_in_dedicated_sign_column() {
        // Layout: sign_column_width=1, width=3 → total gutter = 4 cells.
        // Sign column at x=0; number column at x=1..4; text at x=4.
        let b = Buffer::from_str("a\nb\nc");
        let v = vp(10, 3);
        let signs = [
            Sign {
                row: 0,
                ch: 'W',
                style: Style::default().fg(Color::Yellow),
                priority: 1,
            },
            Sign {
                row: 0,
                ch: 'E',
                style: Style::default().fg(Color::Red),
                priority: 2,
            },
        ];
        let view = BufferView {
            buffer: &b,
            viewport: &v,
            selection: None,
            resolver: &(no_styles as fn(u32) -> Style),
            cursor_line_bg: Style::default(),
            cursor_line_row: None,
            cursor_column_bg: Style::default(),
            selection_bg: Style::default().bg(Color::Blue),
            cursor_style: Style::default().add_modifier(Modifier::REVERSED),
            gutter: Some(Gutter {
                width: 3,
                style: Style::default().fg(Color::DarkGray),
                line_offset: 0,
                sign_column_width: 1,
                fold_column_width: 0,
                ..Default::default()
            }),
            search_bg: Style::default(),
            signs: &signs,
            conceals: &[],
            spans: &[],
            search_pattern: None,
            non_text_style: Style::default(),
            show_eob: true,
            diag_overlays: &[],
            colorcolumn_cols: &[],
            colorcolumn_style: Style::default(),
            listchars: None,
            indent_guides_enabled: false,
            indent_guide_char: '│',
            indent_guide_shiftwidth: 4,
            indent_guide_fg: Color::Reset,
            indent_guide_active_fg: Color::Reset,
            indent_guide_active_col: None,
            fold_line_bg: Style::default(),
            folds_override: None,
            eol_hints: &[],
            blame_plan: None,
        };
        let term = run_render(view, 10, 3);
        // Sign 'E' (higher priority) lands in the sign column at x=0.
        assert_eq!(term.cell((0, 0)).unwrap().symbol(), "E");
        assert_eq!(term.cell((0, 0)).unwrap().fg, Color::Red);
        // Number column at x=1 must NOT be the sign char.
        assert_ne!(term.cell((1, 0)).unwrap().symbol(), "E");
        // Row 1 has no sign — sign column cell stays blank.
        assert_eq!(term.cell((0, 1)).unwrap().symbol(), " ");
    }

    #[test]
    fn conceal_replaces_byte_range() {
        let b = Buffer::from_str("see https://example.com end");
        let v = vp(30, 1);
        let conceals = vec![Conceal {
            row: 0,
            start_byte: 4,                             // start of "https"
            end_byte: 4 + "https://example.com".len(), // end of URL
            replacement: "🔗".to_string(),
        }];
        let view = BufferView {
            buffer: &b,
            viewport: &v,
            selection: None,
            resolver: &(no_styles as fn(u32) -> Style),
            cursor_line_bg: Style::default(),
            cursor_line_row: None,
            cursor_column_bg: Style::default(),
            selection_bg: Style::default(),
            cursor_style: Style::default(),
            gutter: None,
            search_bg: Style::default(),
            signs: &[],
            conceals: &conceals,
            spans: &[],
            search_pattern: None,
            non_text_style: Style::default(),
            show_eob: true,
            diag_overlays: &[],
            colorcolumn_cols: &[],
            colorcolumn_style: Style::default(),
            listchars: None,
            indent_guides_enabled: false,
            indent_guide_char: '│',
            indent_guide_shiftwidth: 4,
            indent_guide_fg: Color::Reset,
            indent_guide_active_fg: Color::Reset,
            indent_guide_active_col: None,
            fold_line_bg: Style::default(),
            folds_override: None,
            eol_hints: &[],
            blame_plan: None,
        };
        let term = run_render(view, 30, 1);
        // Cells 0..=3: "see "
        assert_eq!(term.cell((0, 0)).unwrap().symbol(), "s");
        assert_eq!(term.cell((3, 0)).unwrap().symbol(), " ");
        // Cell 4: the link emoji (a wide char takes 2 cells; we just
        // assert the first cell holds the replacement char).
        assert_eq!(term.cell((4, 0)).unwrap().symbol(), "🔗");
    }

    /// Closed fold header renders the first line's real content (no summary
    /// text), and the `fold_line_bg` is overlaid across that row.
    #[test]
    fn closed_fold_renders_first_line_content_with_fold_bg() {
        let mut b = Buffer::from_str("a\nb\nc\nd\ne");
        let v = vp(30, 5);
        // Fold rows 1-3 closed. Visible should be: 'a', fold-header, 'e'.
        b.add_fold(1, 3, true);
        let fold_bg = Color::Rgb(0x3a, 0x4a, 0x5a);
        let view = BufferView {
            buffer: &b,
            viewport: &v,
            selection: None,
            resolver: &(no_styles as fn(u32) -> Style),
            cursor_line_bg: Style::default(),
            cursor_line_row: None,
            fold_line_bg: Style::default().bg(fold_bg),
            folds_override: None,
            cursor_column_bg: Style::default(),
            selection_bg: Style::default().bg(Color::Blue),
            cursor_style: Style::default().add_modifier(Modifier::REVERSED),
            gutter: None,
            search_bg: Style::default(),
            signs: &[],
            conceals: &[],
            spans: &[],
            search_pattern: None,
            non_text_style: Style::default(),
            show_eob: true,
            diag_overlays: &[],
            colorcolumn_cols: &[],
            colorcolumn_style: Style::default(),
            listchars: None,
            indent_guides_enabled: false,
            indent_guide_char: '│',
            indent_guide_shiftwidth: 4,
            indent_guide_fg: Color::Reset,
            indent_guide_active_fg: Color::Reset,
            indent_guide_active_col: None,
            eol_hints: &[],
            blame_plan: None,
        };
        let term = run_render(view, 30, 5);
        // Row 0: "a" — normal line, no fold bg.
        assert_eq!(term.cell((0, 0)).unwrap().symbol(), "a");
        assert_ne!(
            term.cell((0, 0)).unwrap().bg,
            fold_bg,
            "row 0 is not a fold header and must NOT carry fold_bg"
        );
        // Row 1: closed fold header — renders doc row 1's real content ('b'),
        // no summary text. The fold_line_bg must be applied.
        assert_eq!(
            term.cell((0, 1)).unwrap().symbol(),
            "b",
            "fold header row must show the first line's real content, not summary text"
        );
        assert_eq!(
            term.cell((0, 1)).unwrap().bg,
            fold_bg,
            "fold header row must carry fold_line_bg"
        );
        // Row 2: "e" (the 5th doc row, after the collapsed range).
        assert_eq!(term.cell((0, 2)).unwrap().symbol(), "e");
    }

    #[test]
    fn mix_colors_averages_rgb_channels() {
        use ratatui::style::Color;
        assert_eq!(
            super::mix_colors(Color::Rgb(0x10, 0x20, 0x30), Color::Rgb(0x30, 0x40, 0x50)),
            Color::Rgb(0x20, 0x30, 0x40),
        );
        // Non-RGB → falls back to the first color (can't average named/indexed).
        assert_eq!(
            super::mix_colors(Color::Rgb(0x10, 0x20, 0x30), Color::Blue),
            Color::Rgb(0x10, 0x20, 0x30),
        );
    }

    /// When the cursorline lands on a closed fold header, the row bg is the
    /// BLEND of `fold_line_bg` and `cursor_line_bg`, not either one alone.
    #[test]
    fn fold_header_on_cursor_row_blends_bgs() {
        let mut b = Buffer::from_str("a\nb\nc\nd\ne");
        let v = vp(30, 5);
        b.add_fold(1, 3, true);
        let fold_bg = Color::Rgb(0x40, 0x20, 0x60);
        let cursor_bg = Color::Rgb(0x20, 0x30, 0x40);
        let blended = Color::Rgb(0x30, 0x28, 0x50); // channel-wise average
        let view = BufferView {
            buffer: &b,
            viewport: &v,
            selection: None,
            resolver: &(no_styles as fn(u32) -> Style),
            cursor_line_bg: Style::default().bg(cursor_bg),
            // Cursor is on doc row 1 — the fold header row.
            cursor_line_row: Some(1),
            fold_line_bg: Style::default().bg(fold_bg),
            folds_override: None,
            cursor_column_bg: Style::default(),
            selection_bg: Style::default().bg(Color::Blue),
            cursor_style: Style::default().add_modifier(Modifier::REVERSED),
            gutter: None,
            search_bg: Style::default(),
            signs: &[],
            conceals: &[],
            spans: &[],
            search_pattern: None,
            non_text_style: Style::default(),
            show_eob: true,
            diag_overlays: &[],
            colorcolumn_cols: &[],
            colorcolumn_style: Style::default(),
            listchars: None,
            indent_guides_enabled: false,
            indent_guide_char: '│',
            indent_guide_shiftwidth: 4,
            indent_guide_fg: Color::Reset,
            indent_guide_active_fg: Color::Reset,
            indent_guide_active_col: None,
            eol_hints: &[],
            blame_plan: None,
        };
        let term = run_render(view, 30, 5);
        // Row 1 is the fold header AND the cursor row → blended bg.
        assert_eq!(
            term.cell((0, 1)).unwrap().bg,
            blended,
            "fold header on the cursor row must blend fold + cursorline bg"
        );
    }

    #[test]
    fn open_fold_renders_normally() {
        let mut b = Buffer::from_str("a\nb\nc");
        let v = vp(5, 3);
        b.add_fold(0, 2, false); // open
        let view = BufferView {
            buffer: &b,
            viewport: &v,
            selection: None,
            resolver: &(no_styles as fn(u32) -> Style),
            cursor_line_bg: Style::default(),
            cursor_line_row: None,
            cursor_column_bg: Style::default(),
            selection_bg: Style::default().bg(Color::Blue),
            cursor_style: Style::default().add_modifier(Modifier::REVERSED),
            gutter: None,
            search_bg: Style::default(),
            signs: &[],
            conceals: &[],
            spans: &[],
            search_pattern: None,
            non_text_style: Style::default(),
            show_eob: true,
            diag_overlays: &[],
            colorcolumn_cols: &[],
            colorcolumn_style: Style::default(),
            listchars: None,
            indent_guides_enabled: false,
            indent_guide_char: '│',
            indent_guide_shiftwidth: 4,
            indent_guide_fg: Color::Reset,
            indent_guide_active_fg: Color::Reset,
            indent_guide_active_col: None,
            fold_line_bg: Style::default(),
            folds_override: None,
            eol_hints: &[],
            blame_plan: None,
        };
        let term = run_render(view, 5, 3);
        assert_eq!(term.cell((0, 0)).unwrap().symbol(), "a");
        assert_eq!(term.cell((0, 1)).unwrap().symbol(), "b");
        assert_eq!(term.cell((0, 2)).unwrap().symbol(), "c");
    }

    #[test]
    fn horizontal_scroll_clips_left_chars() {
        let b = Buffer::from_str("abcdefgh");
        let mut v = vp(4, 1);
        v.top_col = 3;
        let view = BufferView {
            buffer: &b,
            viewport: &v,
            selection: None,
            resolver: &(no_styles as fn(u32) -> Style),
            cursor_line_bg: Style::default(),
            cursor_line_row: None,
            cursor_column_bg: Style::default(),
            selection_bg: Style::default().bg(Color::Blue),
            cursor_style: Style::default().add_modifier(Modifier::REVERSED),
            gutter: None,
            search_bg: Style::default(),
            signs: &[],
            conceals: &[],
            spans: &[],
            search_pattern: None,
            non_text_style: Style::default(),
            show_eob: true,
            diag_overlays: &[],
            colorcolumn_cols: &[],
            colorcolumn_style: Style::default(),
            listchars: None,
            indent_guides_enabled: false,
            indent_guide_char: '│',
            indent_guide_shiftwidth: 4,
            indent_guide_fg: Color::Reset,
            indent_guide_active_fg: Color::Reset,
            indent_guide_active_col: None,
            fold_line_bg: Style::default(),
            folds_override: None,
            eol_hints: &[],
            blame_plan: None,
        };
        let term = run_render(view, 4, 1);
        assert_eq!(term.cell((0, 0)).unwrap().symbol(), "d");
        assert_eq!(term.cell((3, 0)).unwrap().symbol(), "g");
    }

    fn make_wrap_view<'a>(
        b: &'a Buffer,
        viewport: &'a Viewport,
        resolver: &'a (impl StyleResolver + 'a),
        gutter: Option<Gutter>,
    ) -> BufferView<'a, impl StyleResolver + 'a> {
        BufferView {
            buffer: b,
            viewport,
            selection: None,
            resolver,
            cursor_line_bg: Style::default(),
            cursor_line_row: None,
            fold_line_bg: Style::default(),
            folds_override: None,
            cursor_column_bg: Style::default(),
            selection_bg: Style::default().bg(Color::Blue),
            cursor_style: Style::default().add_modifier(Modifier::REVERSED),
            gutter,
            search_bg: Style::default(),
            signs: &[],
            conceals: &[],
            spans: &[],
            search_pattern: None,
            non_text_style: Style::default(),
            show_eob: true,
            diag_overlays: &[],
            colorcolumn_cols: &[],
            colorcolumn_style: Style::default(),
            listchars: None,
            indent_guides_enabled: false,
            indent_guide_char: '│',
            indent_guide_shiftwidth: 4,
            indent_guide_fg: Color::Reset,
            indent_guide_active_fg: Color::Reset,
            indent_guide_active_col: None,
            eol_hints: &[],
            blame_plan: None,
        }
    }

    #[test]
    fn wrap_segments_char_breaks_at_width() {
        let segs = wrap_segments("abcdefghij", 4, Wrap::Char);
        assert_eq!(segs, vec![(0, 4), (4, 8), (8, 10)]);
    }

    #[test]
    fn wrap_segments_word_backs_up_to_whitespace() {
        let segs = wrap_segments("alpha beta gamma", 8, Wrap::Word);
        // First segment "alpha " ends after the space at idx 5.
        assert_eq!(segs[0], (0, 6));
        // Second segment "beta " ends after the space at idx 10.
        assert_eq!(segs[1], (6, 11));
        assert_eq!(segs[2], (11, 16));
    }

    #[test]
    fn wrap_segments_word_falls_back_to_char_for_long_runs() {
        let segs = wrap_segments("supercalifragilistic", 5, Wrap::Word);
        // No whitespace anywhere — degrades to a hard char break.
        assert_eq!(segs, vec![(0, 5), (5, 10), (10, 15), (15, 20)]);
    }

    #[test]
    fn wrap_char_paints_continuation_rows() {
        let b = Buffer::from_str("abcdefghij");
        let v = Viewport {
            top_row: 0,
            top_col: 0,
            width: 4,
            height: 3,
            wrap: Wrap::Char,
            text_width: 4,
            tab_width: 0,
        };
        let r = no_styles as fn(u32) -> Style;
        let view = make_wrap_view(&b, &v, &r, None);
        let term = run_render(view, 4, 3);
        // Row 0: "abcd"
        assert_eq!(term.cell((0, 0)).unwrap().symbol(), "a");
        assert_eq!(term.cell((3, 0)).unwrap().symbol(), "d");
        // Row 1: "efgh"
        assert_eq!(term.cell((0, 1)).unwrap().symbol(), "e");
        assert_eq!(term.cell((3, 1)).unwrap().symbol(), "h");
        // Row 2: "ij"
        assert_eq!(term.cell((0, 2)).unwrap().symbol(), "i");
        assert_eq!(term.cell((1, 2)).unwrap().symbol(), "j");
    }

    #[test]
    fn wrap_char_gutter_blank_on_continuation() {
        let b = Buffer::from_str("abcdefgh");
        let v = Viewport {
            top_row: 0,
            top_col: 0,
            width: 6,
            height: 3,
            wrap: Wrap::Char,
            // Text area = 6 - 3 (gutter width) = 3.
            text_width: 3,
            tab_width: 0,
        };
        let r = no_styles as fn(u32) -> Style;
        let gutter = Gutter {
            width: 3,
            style: Style::default().fg(Color::Yellow),
            line_offset: 0,
            ..Default::default()
        };
        let view = make_wrap_view(&b, &v, &r, Some(gutter));
        let term = run_render(view, 6, 3);
        // Row 0: "  1" + "abc"
        assert_eq!(term.cell((1, 0)).unwrap().symbol(), "1");
        assert_eq!(term.cell((3, 0)).unwrap().symbol(), "a");
        // Row 1: blank gutter + "def"
        for x in 0..2 {
            assert_eq!(term.cell((x, 1)).unwrap().symbol(), " ");
        }
        assert_eq!(term.cell((3, 1)).unwrap().symbol(), "d");
        assert_eq!(term.cell((5, 1)).unwrap().symbol(), "f");
    }

    #[test]
    fn wrap_char_cursor_lands_on_correct_segment() {
        let mut b = Buffer::from_str("abcdefghij");
        let v = Viewport {
            top_row: 0,
            top_col: 0,
            width: 4,
            height: 3,
            wrap: Wrap::Char,
            text_width: 4,
            tab_width: 0,
        };
        // Cursor on 'g' (col 6) should land on row 1, col 2.
        b.set_cursor(hjkl_buffer::Position::new(0, 6));
        let r = no_styles as fn(u32) -> Style;
        let mut view = make_wrap_view(&b, &v, &r, None);
        view.cursor_style = Style::default().add_modifier(Modifier::REVERSED);
        let term = run_render(view, 4, 3);
        assert!(
            term.cell((2, 1))
                .unwrap()
                .modifier
                .contains(Modifier::REVERSED)
        );
    }

    #[test]
    fn wrap_char_eol_cursor_placeholder_on_last_segment() {
        let mut b = Buffer::from_str("abcdef");
        let v = Viewport {
            top_row: 0,
            top_col: 0,
            width: 4,
            height: 3,
            wrap: Wrap::Char,
            text_width: 4,
            tab_width: 0,
        };
        // Past-end cursor at col 6.
        b.set_cursor(hjkl_buffer::Position::new(0, 6));
        let r = no_styles as fn(u32) -> Style;
        let mut view = make_wrap_view(&b, &v, &r, None);
        view.cursor_style = Style::default().add_modifier(Modifier::REVERSED);
        let term = run_render(view, 4, 3);
        // Last segment is row 1 ("ef"), placeholder at x = 6 - 4 = 2.
        assert!(
            term.cell((2, 1))
                .unwrap()
                .modifier
                .contains(Modifier::REVERSED)
        );
    }

    #[test]
    fn wrap_word_breaks_at_whitespace() {
        let b = Buffer::from_str("alpha beta gamma");
        let v = Viewport {
            top_row: 0,
            top_col: 0,
            width: 8,
            height: 3,
            wrap: Wrap::Word,
            text_width: 8,
            tab_width: 0,
        };
        let r = no_styles as fn(u32) -> Style;
        let view = make_wrap_view(&b, &v, &r, None);
        let term = run_render(view, 8, 3);
        // Row 0: "alpha "
        assert_eq!(term.cell((0, 0)).unwrap().symbol(), "a");
        assert_eq!(term.cell((4, 0)).unwrap().symbol(), "a");
        // Row 1: "beta "
        assert_eq!(term.cell((0, 1)).unwrap().symbol(), "b");
        assert_eq!(term.cell((3, 1)).unwrap().symbol(), "a");
        // Row 2: "gamma"
        assert_eq!(term.cell((0, 2)).unwrap().symbol(), "g");
        assert_eq!(term.cell((4, 2)).unwrap().symbol(), "a");
    }

    // 0.0.37 — `BufferView` lost `Buffer::spans` / `Buffer::search_pattern`
    // and now takes them as parameters. The tests below cover the new
    // shape: empty/missing parameters, multi-row spans, regex hlsearch,
    // and the interaction with cursor / selection / wrap.

    fn view_with<'a>(
        b: &'a Buffer,
        viewport: &'a Viewport,
        resolver: &'a (impl StyleResolver + 'a),
        spans: &'a [Vec<Span>],
        search_pattern: Option<&'a regex::Regex>,
    ) -> BufferView<'a, impl StyleResolver + 'a> {
        BufferView {
            buffer: b,
            viewport,
            selection: None,
            resolver,
            cursor_line_bg: Style::default(),
            cursor_line_row: None,
            fold_line_bg: Style::default(),
            folds_override: None,
            cursor_column_bg: Style::default(),
            selection_bg: Style::default().bg(Color::Blue),
            cursor_style: Style::default().add_modifier(Modifier::REVERSED),
            gutter: None,
            search_bg: Style::default().bg(Color::Magenta),
            signs: &[],
            conceals: &[],
            spans,
            search_pattern,
            non_text_style: Style::default(),
            show_eob: true,
            diag_overlays: &[],
            colorcolumn_cols: &[],
            colorcolumn_style: Style::default(),
            listchars: None,
            indent_guides_enabled: false,
            indent_guide_char: '│',
            indent_guide_shiftwidth: 4,
            indent_guide_fg: Color::Reset,
            indent_guide_active_fg: Color::Reset,
            indent_guide_active_col: None,
            eol_hints: &[],
            blame_plan: None,
        }
    }

    #[test]
    fn empty_spans_param_renders_default_style() {
        let b = Buffer::from_str("hello");
        let v = vp(10, 1);
        let r = no_styles as fn(u32) -> Style;
        let view = view_with(&b, &v, &r, &[], None);
        let term = run_render(view, 10, 1);
        assert_eq!(term.cell((0, 0)).unwrap().symbol(), "h");
        assert_eq!(term.cell((0, 0)).unwrap().fg, Color::Reset);
    }

    #[test]
    fn spans_param_paints_styled_byte_range() {
        let b = Buffer::from_str("abcdef");
        let v = vp(10, 1);
        let resolver = |id: u32| -> Style {
            if id == 3 {
                Style::default().fg(Color::Green)
            } else {
                Style::default()
            }
        };
        let spans = vec![vec![Span::new(0, 3, 3)]];
        let view = view_with(&b, &v, &resolver, &spans, None);
        let term = run_render(view, 10, 1);
        for x in 0..3 {
            assert_eq!(term.cell((x, 0)).unwrap().fg, Color::Green);
        }
        assert_ne!(term.cell((3, 0)).unwrap().fg, Color::Green);
    }

    #[test]
    fn spans_param_handles_per_row_overlay() {
        let b = Buffer::from_str("abc\ndef");
        let v = vp(10, 2);
        let resolver = |id: u32| -> Style {
            if id == 1 {
                Style::default().fg(Color::Red)
            } else {
                Style::default().fg(Color::Green)
            }
        };
        let spans = vec![vec![Span::new(0, 3, 1)], vec![Span::new(0, 3, 2)]];
        let view = view_with(&b, &v, &resolver, &spans, None);
        let term = run_render(view, 10, 2);
        assert_eq!(term.cell((0, 0)).unwrap().fg, Color::Red);
        assert_eq!(term.cell((0, 1)).unwrap().fg, Color::Green);
    }

    #[test]
    fn spans_param_rows_beyond_get_no_styling() {
        let b = Buffer::from_str("abc\ndef\nghi");
        let v = vp(10, 3);
        let resolver = |_: u32| -> Style { Style::default().fg(Color::Red) };
        // Only row 0 carries spans; rows 1 and 2 inherit default.
        let spans = vec![vec![Span::new(0, 3, 0)]];
        let view = view_with(&b, &v, &resolver, &spans, None);
        let term = run_render(view, 10, 3);
        assert_eq!(term.cell((0, 0)).unwrap().fg, Color::Red);
        assert_ne!(term.cell((0, 1)).unwrap().fg, Color::Red);
        assert_ne!(term.cell((0, 2)).unwrap().fg, Color::Red);
    }

    #[test]
    fn search_pattern_none_disables_hlsearch() {
        let b = Buffer::from_str("foo bar foo");
        let v = vp(20, 1);
        let r = no_styles as fn(u32) -> Style;
        // No regex → no Magenta bg anywhere even though `search_bg` is set.
        let view = view_with(&b, &v, &r, &[], None);
        let term = run_render(view, 20, 1);
        for x in 0..11 {
            assert_ne!(term.cell((x, 0)).unwrap().bg, Color::Magenta);
        }
    }

    #[test]
    fn search_pattern_regex_paints_match_bg() {
        use regex::Regex;
        let b = Buffer::from_str("xyz foo xyz");
        let v = vp(20, 1);
        let r = no_styles as fn(u32) -> Style;
        let pat = Regex::new("foo").unwrap();
        let view = view_with(&b, &v, &r, &[], Some(&pat));
        let term = run_render(view, 20, 1);
        // "foo" is at chars 4..7; bg is Magenta there only.
        assert_ne!(term.cell((3, 0)).unwrap().bg, Color::Magenta);
        for x in 4..7 {
            assert_eq!(term.cell((x, 0)).unwrap().bg, Color::Magenta);
        }
        assert_ne!(term.cell((7, 0)).unwrap().bg, Color::Magenta);
    }

    #[test]
    fn search_pattern_unicode_columns_are_charwise() {
        use regex::Regex;
        // "tablé foo" — match "foo" must land on char column 6, not byte.
        let b = Buffer::from_str("tablé foo");
        let v = vp(20, 1);
        let r = no_styles as fn(u32) -> Style;
        let pat = Regex::new("foo").unwrap();
        let view = view_with(&b, &v, &r, &[], Some(&pat));
        let term = run_render(view, 20, 1);
        // "tablé" is 5 chars + space = 6, then "foo" at 6..9.
        assert_eq!(term.cell((6, 0)).unwrap().bg, Color::Magenta);
        assert_eq!(term.cell((8, 0)).unwrap().bg, Color::Magenta);
        assert_ne!(term.cell((5, 0)).unwrap().bg, Color::Magenta);
    }

    #[test]
    fn spans_param_clamps_short_row_overlay() {
        // Row 0 has 3 chars; span past end shouldn't crash or smear.
        let b = Buffer::from_str("abc");
        let v = vp(10, 1);
        let resolver = |_: u32| -> Style { Style::default().fg(Color::Red) };
        let spans = vec![vec![Span::new(0, 100, 0)]];
        let view = view_with(&b, &v, &resolver, &spans, None);
        let term = run_render(view, 10, 1);
        for x in 0..3 {
            assert_eq!(term.cell((x, 0)).unwrap().fg, Color::Red);
        }
    }

    #[test]
    fn spans_and_search_pattern_compose() {
        // hlsearch bg layers on top of the syntax span fg.
        use regex::Regex;
        let b = Buffer::from_str("foo");
        let v = vp(10, 1);
        let resolver = |_: u32| -> Style { Style::default().fg(Color::Green) };
        let spans = vec![vec![Span::new(0, 3, 0)]];
        let pat = Regex::new("foo").unwrap();
        let view = view_with(&b, &v, &resolver, &spans, Some(&pat));
        let term = run_render(view, 10, 1);
        let cell = term.cell((1, 0)).unwrap();
        assert_eq!(cell.fg, Color::Green);
        assert_eq!(cell.bg, Color::Magenta);
    }

    /// Rows past the last buffer line paint `~` at the first text column
    /// (vim's NonText marker). The `non_text_style` fg is applied to those
    /// cells; all other cells on those rows stay default.
    #[test]
    fn tilde_marker_painted_past_eof() {
        // 5-line buffer rendered in a 10-row viewport.
        let b = Buffer::from_str("a\nb\nc\nd\ne");
        let v = vp(10, 10);
        let r = no_styles as fn(u32) -> Style;
        let non_text_fg = Color::DarkGray;
        let view = BufferView {
            buffer: &b,
            viewport: &v,
            selection: None,
            resolver: &r,
            cursor_line_bg: Style::default(),
            cursor_line_row: None,
            cursor_column_bg: Style::default(),
            selection_bg: Style::default().bg(Color::Blue),
            cursor_style: Style::default().add_modifier(Modifier::REVERSED),
            gutter: None,
            search_bg: Style::default(),
            signs: &[],
            conceals: &[],
            spans: &[],
            search_pattern: None,
            non_text_style: Style::default().fg(non_text_fg),
            show_eob: true,
            diag_overlays: &[],
            colorcolumn_cols: &[],
            colorcolumn_style: Style::default(),
            listchars: None,
            indent_guides_enabled: false,
            indent_guide_char: '│',
            indent_guide_shiftwidth: 4,
            indent_guide_fg: Color::Reset,
            indent_guide_active_fg: Color::Reset,
            indent_guide_active_col: None,
            fold_line_bg: Style::default(),
            folds_override: None,
            eol_hints: &[],
            blame_plan: None,
        };
        let term = run_render(view, 10, 10);
        // Rows 0-4 have content — first cell should NOT be `~`.
        for row in 0..5u16 {
            assert_ne!(
                term.cell((0, row)).unwrap().symbol(),
                "~",
                "row {row} is a content row, expected no tilde"
            );
        }
        // Rows 5-9 are past EOF — should have `~` at column 0 with non_text fg.
        for row in 5..10u16 {
            let cell = term.cell((0, row)).unwrap();
            assert_eq!(cell.symbol(), "~", "row {row} is past EOF, expected tilde");
            assert_eq!(
                cell.fg, non_text_fg,
                "row {row} tilde should use non_text_style fg"
            );
            // Rest of the row should be blank.
            for x in 1..10u16 {
                assert_eq!(
                    term.cell((x, row)).unwrap().symbol(),
                    " ",
                    "row {row} col {x} after tilde should be blank"
                );
            }
        }
    }

    /// With `show_eob: false`, rows past the last buffer line stay blank — no
    /// `~` marker (vim's `fillchars=eob:`).
    #[test]
    fn show_eob_false_suppresses_tilde() {
        let b = Buffer::from_str("a\nb\nc");
        let v = vp(10, 8);
        let r = no_styles as fn(u32) -> Style;
        let view = BufferView {
            buffer: &b,
            viewport: &v,
            selection: None,
            resolver: &r,
            cursor_line_bg: Style::default(),
            cursor_line_row: None,
            cursor_column_bg: Style::default(),
            selection_bg: Style::default(),
            cursor_style: Style::default(),
            gutter: None,
            search_bg: Style::default(),
            signs: &[],
            conceals: &[],
            spans: &[],
            search_pattern: None,
            non_text_style: Style::default().fg(Color::DarkGray),
            show_eob: false,
            diag_overlays: &[],
            colorcolumn_cols: &[],
            colorcolumn_style: Style::default(),
            listchars: None,
            indent_guides_enabled: false,
            indent_guide_char: '│',
            indent_guide_shiftwidth: 4,
            indent_guide_fg: Color::Reset,
            indent_guide_active_fg: Color::Reset,
            indent_guide_active_col: None,
            fold_line_bg: Style::default(),
            folds_override: None,
            eol_hints: &[],
            blame_plan: None,
        };
        let term = run_render(view, 10, 8);
        // Rows 3-7 are past EOF — with show_eob off they must NOT show `~`.
        for row in 3..8u16 {
            assert_ne!(
                term.cell((0, row)).unwrap().symbol(),
                "~",
                "row {row} past EOF must be blank when show_eob is false"
            );
        }
    }

    /// When a gutter is present, rows past EOF paint a blank gutter and
    /// `~` at the first text column (after the gutter).
    #[test]
    fn tilde_marker_with_gutter_past_eof() {
        let b = Buffer::from_str("a\nb");
        let v = vp(10, 5);
        let r = no_styles as fn(u32) -> Style;
        let non_text_fg = Color::DarkGray;
        let view = BufferView {
            buffer: &b,
            viewport: &v,
            selection: None,
            resolver: &r,
            cursor_line_bg: Style::default(),
            cursor_line_row: None,
            cursor_column_bg: Style::default(),
            selection_bg: Style::default().bg(Color::Blue),
            cursor_style: Style::default().add_modifier(Modifier::REVERSED),
            gutter: Some(Gutter {
                width: 4,
                style: Style::default().fg(Color::Yellow),
                line_offset: 0,
                numbers: GutterNumbers::Absolute,
                sign_column_width: 0,
                fold_column_width: 0,
            }),
            search_bg: Style::default(),
            signs: &[],
            conceals: &[],
            spans: &[],
            search_pattern: None,
            non_text_style: Style::default().fg(non_text_fg),
            show_eob: true,
            diag_overlays: &[],
            colorcolumn_cols: &[],
            colorcolumn_style: Style::default(),
            listchars: None,
            indent_guides_enabled: false,
            indent_guide_char: '│',
            indent_guide_shiftwidth: 4,
            indent_guide_fg: Color::Reset,
            indent_guide_active_fg: Color::Reset,
            indent_guide_active_col: None,
            fold_line_bg: Style::default(),
            folds_override: None,
            eol_hints: &[],
            blame_plan: None,
        };
        let term = run_render(view, 10, 5);
        // Rows 2-4 are past EOF.
        for row in 2..5u16 {
            // Gutter (cols 0-3) should be blank.
            for x in 0..4u16 {
                assert_eq!(
                    term.cell((x, row)).unwrap().symbol(),
                    " ",
                    "gutter col {x} on past-EOF row {row} should be blank"
                );
            }
            // Text area starts at col 4: should have `~`.
            let cell = term.cell((4, row)).unwrap();
            assert_eq!(
                cell.symbol(),
                "~",
                "past-EOF row {row}: expected tilde at text column"
            );
            assert_eq!(cell.fg, non_text_fg);
        }
    }

    #[test]
    fn diag_overlay_paints_underline_on_range() {
        // Render "hello world" and apply a DiagOverlay from col 6 to 11.
        // The cells in that range must carry the UNDERLINED modifier; cells
        // outside must not.
        let b = Buffer::from_str("hello world");
        let v = vp(20, 2);
        let overlay = DiagOverlay {
            row: 0,
            col_start: 6,
            col_end: 11,
            style: Style::default().add_modifier(Modifier::UNDERLINED),
        };
        let view = BufferView {
            buffer: &b,
            viewport: &v,
            selection: None,
            resolver: &(no_styles as fn(u32) -> Style),
            cursor_line_bg: Style::default(),
            cursor_line_row: None,
            cursor_column_bg: Style::default(),
            selection_bg: Style::default().bg(Color::Blue),
            cursor_style: Style::default().add_modifier(Modifier::REVERSED),
            gutter: None,
            search_bg: Style::default(),
            signs: &[],
            conceals: &[],
            spans: &[],
            search_pattern: None,
            non_text_style: Style::default(),
            show_eob: true,
            diag_overlays: &[overlay],
            colorcolumn_cols: &[],
            colorcolumn_style: Style::default(),
            listchars: None,
            indent_guides_enabled: false,
            indent_guide_char: '│',
            indent_guide_shiftwidth: 4,
            indent_guide_fg: Color::Reset,
            indent_guide_active_fg: Color::Reset,
            indent_guide_active_col: None,
            fold_line_bg: Style::default(),
            folds_override: None,
            eol_hints: &[],
            blame_plan: None,
        };
        let term = run_render(view, 20, 2);

        // Cols 0-5 ("hello ") must NOT be underlined.
        for x in 0u16..6 {
            let cell = term.cell((x, 0)).unwrap();
            assert!(
                !cell.modifier.contains(Modifier::UNDERLINED),
                "col {x} must not be underlined (outside overlay)"
            );
        }
        // Cols 6-10 ("world") must be underlined.
        for x in 6u16..11 {
            let cell = term.cell((x, 0)).unwrap();
            assert!(
                cell.modifier.contains(Modifier::UNDERLINED),
                "col {x} must be underlined (inside overlay)"
            );
        }
        // Col 11 (past end, space) must NOT be underlined.
        let cell = term.cell((11, 0)).unwrap();
        assert!(
            !cell.modifier.contains(Modifier::UNDERLINED),
            "col 11 must not be underlined (past overlay end)"
        );
    }

    #[test]
    fn diag_overlay_out_of_viewport_is_ignored() {
        // Overlay on row 5, viewport height = 3 → must not panic or paint.
        let b = Buffer::from_str("a\nb\nc");
        let v = vp(10, 3);
        let overlay = DiagOverlay {
            row: 5,
            col_start: 0,
            col_end: 1,
            style: Style::default().add_modifier(Modifier::UNDERLINED),
        };
        let view = BufferView {
            buffer: &b,
            viewport: &v,
            selection: None,
            resolver: &(no_styles as fn(u32) -> Style),
            cursor_line_bg: Style::default(),
            cursor_line_row: None,
            cursor_column_bg: Style::default(),
            selection_bg: Style::default().bg(Color::Blue),
            cursor_style: Style::default().add_modifier(Modifier::REVERSED),
            gutter: None,
            search_bg: Style::default(),
            signs: &[],
            conceals: &[],
            spans: &[],
            search_pattern: None,
            non_text_style: Style::default(),
            show_eob: true,
            diag_overlays: &[overlay],
            colorcolumn_cols: &[],
            colorcolumn_style: Style::default(),
            listchars: None,
            indent_guides_enabled: false,
            indent_guide_char: '│',
            indent_guide_shiftwidth: 4,
            indent_guide_fg: Color::Reset,
            indent_guide_active_fg: Color::Reset,
            indent_guide_active_col: None,
            fold_line_bg: Style::default(),
            folds_override: None,
            eol_hints: &[],
            blame_plan: None,
        };
        // Must not panic.
        let _term = run_render(view, 10, 3);
    }

    // ── T5: dedicated sign-column tests ──────────────────────────────────────

    /// A sign on row 0 must render in the sign column (x=0) and NOT overwrite
    /// any digit of the line-number column. With 5-digit line count (max 13109
    /// lines), gutter.width=6 (5 digits + 1 spacer), sign_column_width=1:
    ///   x=0          → sign char (e.g. '~')
    ///   x=1..5       → digits "13109" right-aligned in 5 cells
    ///   x=6          → spacer ' '
    ///   x=7..        → text
    #[test]
    fn paint_signs_in_dedicated_column_does_not_overwrite_line_number() {
        // Build a buffer with enough lines that the max line number is 5 digits.
        // We don't need all 13109 lines — just enough rows to get a 5-digit
        // line_offset. We'll use line_offset to fake the large document.
        let b = Buffer::from_str("a\nb");
        // num_w = 6 (5 digits + 1 spacer), sign_w = 1, total = 7
        let v = vp(20, 2);
        let sign = Sign {
            row: 0,
            ch: '~',
            style: Style::default().fg(Color::Red),
            priority: 10,
        };
        let view = BufferView {
            buffer: &b,
            viewport: &v,
            selection: None,
            resolver: &(no_styles as fn(u32) -> Style),
            cursor_line_bg: Style::default(),
            cursor_line_row: None,
            cursor_column_bg: Style::default(),
            selection_bg: Style::default().bg(Color::Blue),
            cursor_style: Style::default().add_modifier(Modifier::REVERSED),
            gutter: Some(Gutter {
                width: 6, // 5 digit cells + 1 spacer
                style: Style::default(),
                line_offset: 13108, // row 0 displays as 13109
                sign_column_width: 1,
                fold_column_width: 0,
                ..Default::default()
            }),
            search_bg: Style::default(),
            signs: &[sign],
            conceals: &[],
            spans: &[],
            search_pattern: None,
            non_text_style: Style::default(),
            show_eob: true,
            diag_overlays: &[],
            colorcolumn_cols: &[],
            colorcolumn_style: Style::default(),
            listchars: None,
            indent_guides_enabled: false,
            indent_guide_char: '│',
            indent_guide_shiftwidth: 4,
            indent_guide_fg: Color::Reset,
            indent_guide_active_fg: Color::Reset,
            indent_guide_active_col: None,
            fold_line_bg: Style::default(),
            folds_override: None,
            eol_hints: &[],
            blame_plan: None,
        };
        let term = run_render(view, 20, 2);
        // Sign column (x=0) must contain the sign char '~'.
        assert_eq!(
            term.cell((0, 0)).unwrap().symbol(),
            "~",
            "sign column (x=0) must hold the sign char"
        );
        // Number column digits: right-aligned "13109" in 5 cells at x=1..5.
        assert_eq!(term.cell((1, 0)).unwrap().symbol(), "1", "x=1 must be '1'");
        assert_eq!(term.cell((2, 0)).unwrap().symbol(), "3", "x=2 must be '3'");
        assert_eq!(term.cell((3, 0)).unwrap().symbol(), "1", "x=3 must be '1'");
        assert_eq!(term.cell((4, 0)).unwrap().symbol(), "0", "x=4 must be '0'");
        assert_eq!(term.cell((5, 0)).unwrap().symbol(), "9", "x=5 must be '9'");
        // Spacer at x=6.
        assert_eq!(
            term.cell((6, 0)).unwrap().symbol(),
            " ",
            "x=6 must be spacer"
        );
        // Text 'a' at x=7.
        assert_eq!(
            term.cell((7, 0)).unwrap().symbol(),
            "a",
            "text must start at x=sign_w+num_w=7"
        );
    }

    /// When sign_column_width=0 (no sign column), signs Vec is ignored and the
    /// layout collapses to [ number_padded | spacer | text ] as before.
    #[test]
    fn paint_signs_zero_sign_column_width_layout_collapses() {
        let b = Buffer::from_str("abc");
        let v = vp(10, 1);
        let sign = Sign {
            row: 0,
            ch: 'E',
            style: Style::default().fg(Color::Red),
            priority: 10,
        };
        let view = BufferView {
            buffer: &b,
            viewport: &v,
            selection: None,
            resolver: &(no_styles as fn(u32) -> Style),
            cursor_line_bg: Style::default(),
            cursor_line_row: None,
            cursor_column_bg: Style::default(),
            selection_bg: Style::default().bg(Color::Blue),
            cursor_style: Style::default().add_modifier(Modifier::REVERSED),
            // gutter width=3, sign_column_width=0 → text at x=3
            gutter: Some(Gutter {
                width: 3,
                style: Style::default(),
                line_offset: 0,
                sign_column_width: 0,
                fold_column_width: 0,
                ..Default::default()
            }),
            search_bg: Style::default(),
            signs: &[sign],
            conceals: &[],
            spans: &[],
            search_pattern: None,
            non_text_style: Style::default(),
            show_eob: true,
            diag_overlays: &[],
            colorcolumn_cols: &[],
            colorcolumn_style: Style::default(),
            listchars: None,
            indent_guides_enabled: false,
            indent_guide_char: '│',
            indent_guide_shiftwidth: 4,
            indent_guide_fg: Color::Reset,
            indent_guide_active_fg: Color::Reset,
            indent_guide_active_col: None,
            fold_line_bg: Style::default(),
            folds_override: None,
            eol_hints: &[],
            blame_plan: None,
        };
        let term = run_render(view, 10, 1);
        // No sign column: x=0 must be a number digit or space, NOT 'E'.
        assert_ne!(
            term.cell((0, 0)).unwrap().symbol(),
            "E",
            "with sign_column_width=0, sign char must not appear in the gutter"
        );
        // Text starts at x=3 (gutter.width).
        assert_eq!(
            term.cell((3, 0)).unwrap().symbol(),
            "a",
            "text must start at x=gutter.width when sign_column_width=0"
        );
    }

    // ── Indent guide tests ──────────────────────────────────────────────────

    /// Helper to build a BufferView with indent guides configured.
    fn indent_guide_view<'a>(
        b: &'a Buffer,
        viewport: &'a Viewport,
        shiftwidth: usize,
        guide_char: char,
        guide_fg: Color,
        active_fg: Color,
        active_col: Option<usize>,
    ) -> BufferView<'a, impl StyleResolver + 'a> {
        BufferView {
            buffer: b,
            viewport,
            selection: None,
            resolver: &(no_styles as fn(u32) -> Style),
            cursor_line_bg: Style::default(),
            cursor_line_row: None,
            fold_line_bg: Style::default(),
            folds_override: None,
            cursor_column_bg: Style::default(),
            selection_bg: Style::default(),
            cursor_style: Style::default(),
            gutter: None,
            search_bg: Style::default(),
            signs: &[],
            conceals: &[],
            spans: &[],
            search_pattern: None,
            non_text_style: Style::default(),
            show_eob: true,
            diag_overlays: &[],
            colorcolumn_cols: &[],
            colorcolumn_style: Style::default(),
            listchars: None,
            indent_guides_enabled: true,
            indent_guide_char: guide_char,
            indent_guide_shiftwidth: shiftwidth,
            indent_guide_fg: guide_fg,
            indent_guide_active_fg: active_fg,
            indent_guide_active_col: active_col,
            eol_hints: &[],
            blame_plan: None,
        }
    }

    #[test]
    fn indent_guides_disabled_paints_nothing() {
        // Even with indented content, flag=false → no guide chars.
        let b = Buffer::from_str("    foo\n        bar");
        let v = vp(20, 2);
        let view = BufferView {
            buffer: &b,
            viewport: &v,
            selection: None,
            resolver: &(no_styles as fn(u32) -> Style),
            cursor_line_bg: Style::default(),
            cursor_line_row: None,
            cursor_column_bg: Style::default(),
            selection_bg: Style::default(),
            cursor_style: Style::default(),
            gutter: None,
            search_bg: Style::default(),
            signs: &[],
            conceals: &[],
            spans: &[],
            search_pattern: None,
            non_text_style: Style::default(),
            show_eob: true,
            diag_overlays: &[],
            colorcolumn_cols: &[],
            colorcolumn_style: Style::default(),
            listchars: None,
            indent_guides_enabled: false,
            indent_guide_char: '│',
            indent_guide_shiftwidth: 4,
            indent_guide_fg: Color::DarkGray,
            indent_guide_active_fg: Color::Gray,
            indent_guide_active_col: None,
            fold_line_bg: Style::default(),
            folds_override: None,
            eol_hints: &[],
            blame_plan: None,
        };
        let term = run_render(view, 20, 2);
        // No cell should contain '│' anywhere.
        for y in 0..2u16 {
            for x in 0..20u16 {
                assert_ne!(
                    term.cell((x, y)).unwrap().symbol(),
                    "│",
                    "no guide expected at ({x}, {y}) when disabled"
                );
            }
        }
    }

    #[test]
    fn indent_guides_basic_two_levels() {
        // fn() {\n    if foo {\n        bar();\n    }
        // shiftwidth=4: guides at col 4 on rows 1+2+3, col 8 on row 2.
        let b = Buffer::from_str("fn() {\n    if foo {\n        bar();\n    }");
        let v = vp(20, 4);
        // Active col: None (just test inactive guides).
        let view = indent_guide_view(&b, &v, 4, '│', Color::DarkGray, Color::Gray, None);
        let term = run_render(view, 20, 4);
        // Row 0 "fn() {" — no indent, no guides.
        assert_ne!(term.cell((4, 0)).unwrap().symbol(), "│");
        // Row 1 "    if foo {" — leading 4 spaces → guide at col 4 NOT painted
        // (col 4 is first non-space). Actually guide is only at col 4 when
        // leading_vcols > 4. Here leading_vcols = 4, and guide_col = 4 which is
        // NOT < 4, so no guide on row 1... wait.
        // Per spec: paint at sw, 2*sw, 3*sw, ... while col < leading_vcols.
        // leading_vcols = 4 → guide_col = 4 → 4 < 4 is false → no guide on row 1.
        // Actually row 1 has 4 leading spaces. sw=4. First guide col = 4.
        // 4 < 4 = false → no guide at col 4 on row 1.
        // Row 2 "        bar();" — 8 leading spaces → guides at 4 and 8? 4 < 8 yes, 8 < 8 no.
        // So guide at col 4 only on row 2.
        // Row 3 "    }" — 4 spaces → same as row 1: no guide (4 < 4 = false).
        //
        // This test verifies the actual behavior: guide at col 4 on row 2 only.
        assert_ne!(
            term.cell((4, 1)).unwrap().symbol(),
            "│",
            "row1: no guide (leading_vcols=4, sw=4, 4<4=false)"
        );
        assert_eq!(
            term.cell((4, 2)).unwrap().symbol(),
            "│",
            "row2: guide at col 4 (leading_vcols=8)"
        );
        assert_ne!(
            term.cell((8, 2)).unwrap().symbol(),
            "│",
            "row2: no guide at col 8 (8<8=false)"
        );
        assert_ne!(
            term.cell((4, 3)).unwrap().symbol(),
            "│",
            "row3: no guide (leading_vcols=4, 4<4=false)"
        );
    }

    #[test]
    fn indent_guides_skip_when_no_indent() {
        let b = Buffer::from_str("no_indent\nstill_none");
        let v = vp(20, 2);
        let view = indent_guide_view(&b, &v, 4, '│', Color::DarkGray, Color::Gray, None);
        let term = run_render(view, 20, 2);
        for y in 0..2u16 {
            for x in 0..20u16 {
                assert_ne!(
                    term.cell((x, y)).unwrap().symbol(),
                    "│",
                    "no guide expected on non-indented rows"
                );
            }
        }
    }

    #[test]
    fn indent_guides_respects_tabs() {
        // shiftwidth=4 tabstop=4 line "\t\tfoo": visual cols 0..3=tab, 4..7=tab, 8='f'.
        // leading_vcols = 8 (two tabs each expanding to 4 cells).
        // Guide at sw=4 only (4 < 8 = true; 8 < 8 = false).
        let b = Buffer::from_str("\t\tfoo");
        let mut v = vp(20, 1);
        v.tab_width = 4;
        let view = indent_guide_view(&b, &v, 4, '│', Color::DarkGray, Color::Gray, None);
        let term = run_render(view, 20, 1);
        // Col 4 should have '│' (tab expansion: first tab = cells 0-3, second tab = cells 4-7).
        // Cell at screen x=4 is inside the second tab expansion → space → guide painted.
        assert_eq!(
            term.cell((4, 0)).unwrap().symbol(),
            "│",
            "guide at visual col 4 inside second tab"
        );
        // Col 8 is 'f' — not a space, no guide (and 8 < 8 is false anyway).
        assert_eq!(term.cell((8, 0)).unwrap().symbol(), "f");
    }

    #[test]
    fn indent_guides_active_col_uses_active_fg() {
        // 8 leading spaces, shiftwidth=4 → guide at col 4.
        // active_col = 4 → that guide gets active_fg (Gray), not inactive (DarkGray).
        let b = Buffer::from_str("        code");
        let v = vp(20, 1);
        let active_col = Some(4usize);
        let view = indent_guide_view(&b, &v, 4, '│', Color::DarkGray, Color::Gray, active_col);
        let term = run_render(view, 20, 1);
        let cell = term.cell((4, 0)).unwrap();
        assert_eq!(cell.symbol(), "│", "guide painted at col 4");
        assert_eq!(cell.fg, Color::Gray, "active col uses active_fg (Gray)");
    }

    #[test]
    fn indent_guides_inactive_col_uses_inactive_fg() {
        // 12 leading spaces, shiftwidth=4 → guides at col 4 and col 8.
        // active_col = 8 → col 4 is inactive (DarkGray), col 8 is active (Gray).
        let b = Buffer::from_str("            code");
        let v = vp(20, 1);
        let view = indent_guide_view(&b, &v, 4, '│', Color::DarkGray, Color::Gray, Some(8));
        let term = run_render(view, 20, 1);
        // Col 4 inactive.
        let cell4 = term.cell((4, 0)).unwrap();
        assert_eq!(cell4.symbol(), "│", "guide at col 4");
        assert_eq!(cell4.fg, Color::DarkGray, "col 4 uses inactive fg");
        // Col 8 active.
        let cell8 = term.cell((8, 0)).unwrap();
        assert_eq!(cell8.symbol(), "│", "guide at col 8");
        assert_eq!(cell8.fg, Color::Gray, "col 8 uses active fg");
    }

    #[test]
    fn indent_guides_custom_char_paints_that_char() {
        // Use ':' as guide character.
        let b = Buffer::from_str("        code");
        let v = vp(20, 1);
        let view = indent_guide_view(&b, &v, 4, ':', Color::DarkGray, Color::Gray, None);
        let term = run_render(view, 20, 1);
        assert_eq!(
            term.cell((4, 0)).unwrap().symbol(),
            ":",
            "custom guide char ':' at col 4"
        );
    }

    #[test]
    fn eol_hint_paints_after_text() {
        // Buffer with line "ab". EolHint{row:0, text:"BLAME"} must paint the
        // 5 chars "BLAME" somewhere to the right of "ab" on screen row 0.
        let b = Buffer::from_str("ab");
        let v = vp(30, 1);
        let hint = EolHint {
            row: 0,
            text: "BLAME".into(),
            style: Style::default(),
        };
        let view = BufferView {
            buffer: &b,
            viewport: &v,
            selection: None,
            resolver: &(no_styles as fn(u32) -> Style),
            cursor_line_bg: Style::default(),
            cursor_line_row: None,
            fold_line_bg: Style::default(),
            folds_override: None,
            cursor_column_bg: Style::default(),
            selection_bg: Style::default(),
            cursor_style: Style::default(),
            gutter: None,
            search_bg: Style::default(),
            signs: &[],
            conceals: &[],
            spans: &[],
            search_pattern: None,
            non_text_style: Style::default(),
            show_eob: true,
            diag_overlays: &[],
            colorcolumn_cols: &[],
            colorcolumn_style: Style::default(),
            listchars: None,
            indent_guides_enabled: false,
            indent_guide_char: '│',
            indent_guide_shiftwidth: 4,
            indent_guide_fg: Color::Reset,
            indent_guide_active_fg: Color::Reset,
            indent_guide_active_col: None,
            eol_hints: &[hint],
            blame_plan: None,
        };
        let term = run_render(view, 30, 1);
        // "ab" is at x=0 and x=1; hint starts at x = 2 + 2 (gap) = 4.
        // Scan row 0 for the substring "BLAME".
        let row_text: String = (0..30u16)
            .map(|x| {
                term.cell((x, 0))
                    .map(|c| c.symbol().chars().next().unwrap_or(' '))
                    .unwrap_or(' ')
            })
            .collect();
        assert!(
            row_text.contains("BLAME"),
            "expected 'BLAME' to appear in row 0, got: {row_text:?}"
        );
    }

    #[test]
    fn fold_column_glyph_open_closed_body() {
        // Outer fold rows 1..=4 OPEN, with a CLOSED fold rows 2..=3 inside.
        let mut b = hjkl_buffer::Buffer::from_str("a\nb\nc\nd\ne\nf");
        b.add_fold(1, 4, false); // open outer
        b.add_fold(2, 3, true); // closed inner
        let folds = b.folds();
        assert_eq!(super::fold_column_glyph(&folds, 0), ' ', "row 0: no fold");
        assert_eq!(
            super::fold_column_glyph(&folds, 1),
            '\u{25be}',
            "row 1: open fold start = \u{25be}"
        );
        assert_eq!(
            super::fold_column_glyph(&folds, 2),
            '\u{25b8}',
            "row 2: closed fold start = \u{25b8}"
        );
        // row 4 is inside the open outer fold body (and not a start) → bar.
        assert_eq!(
            super::fold_column_glyph(&folds, 4),
            '\u{2502}',
            "row 4: open fold body = \u{2502}"
        );
        assert_eq!(
            super::fold_column_glyph(&folds, 5),
            ' ',
            "row 5: outside all folds"
        );
    }
}
