//! Editor — the public sqeel-vim type, layered over `hjkl_buffer::Buffer`.
//!
//! This file owns the public Editor API — construction, content access,
//! mouse and goto helpers, the (buffer-level) undo stack, and insert-mode
//! session bookkeeping. All vim-specific keyboard handling lives in
//! [`vim`] and communicates with Editor through a small internal API
//! exposed via `pub(super)` fields and helper methods.

use crate::input::{Input, Key};
use crate::vim::{self, VimState};
use crate::{KeybindingMode, VimMode};
#[cfg(feature = "crossterm")]
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
#[cfg(feature = "ratatui")]
use ratatui::layout::Rect;
use std::sync::atomic::{AtomicU16, Ordering};

/// Convert a SPEC [`crate::types::Style`] to a [`ratatui::style::Style`].
///
/// Lossless within the styles each library represents. Lives behind the
/// `ratatui` feature so wasm / no_std consumers that opt out don't pay
/// for the dep. Use the engine-native [`crate::types::Style`] +
/// [`Editor::intern_engine_style`] surface from feature-disabled hosts.
#[cfg(feature = "ratatui")]
pub(crate) fn engine_style_to_ratatui(s: crate::types::Style) -> ratatui::style::Style {
    use crate::types::Attrs;
    use ratatui::style::{Color as RColor, Modifier as RMod, Style as RStyle};
    let mut out = RStyle::default();
    if let Some(c) = s.fg {
        out = out.fg(RColor::Rgb(c.0, c.1, c.2));
    }
    if let Some(c) = s.bg {
        out = out.bg(RColor::Rgb(c.0, c.1, c.2));
    }
    let mut m = RMod::empty();
    if s.attrs.contains(Attrs::BOLD) {
        m |= RMod::BOLD;
    }
    if s.attrs.contains(Attrs::ITALIC) {
        m |= RMod::ITALIC;
    }
    if s.attrs.contains(Attrs::UNDERLINE) {
        m |= RMod::UNDERLINED;
    }
    if s.attrs.contains(Attrs::REVERSE) {
        m |= RMod::REVERSED;
    }
    if s.attrs.contains(Attrs::DIM) {
        m |= RMod::DIM;
    }
    if s.attrs.contains(Attrs::STRIKE) {
        m |= RMod::CROSSED_OUT;
    }
    out.add_modifier(m)
}

/// Inverse of [`engine_style_to_ratatui`]. Lossy for ratatui colors
/// the engine doesn't model (Indexed, named ANSI) — flattens to
/// nearest RGB. Behind the `ratatui` feature.
#[cfg(feature = "ratatui")]
pub(crate) fn ratatui_style_to_engine(s: ratatui::style::Style) -> crate::types::Style {
    use crate::types::{Attrs, Color, Style};
    use ratatui::style::{Color as RColor, Modifier as RMod};
    fn c(rc: RColor) -> Color {
        match rc {
            RColor::Rgb(r, g, b) => Color(r, g, b),
            RColor::Black => Color(0, 0, 0),
            RColor::Red => Color(205, 49, 49),
            RColor::Green => Color(13, 188, 121),
            RColor::Yellow => Color(229, 229, 16),
            RColor::Blue => Color(36, 114, 200),
            RColor::Magenta => Color(188, 63, 188),
            RColor::Cyan => Color(17, 168, 205),
            RColor::Gray => Color(229, 229, 229),
            RColor::DarkGray => Color(102, 102, 102),
            RColor::LightRed => Color(241, 76, 76),
            RColor::LightGreen => Color(35, 209, 139),
            RColor::LightYellow => Color(245, 245, 67),
            RColor::LightBlue => Color(59, 142, 234),
            RColor::LightMagenta => Color(214, 112, 214),
            RColor::LightCyan => Color(41, 184, 219),
            RColor::White => Color(255, 255, 255),
            _ => Color(0, 0, 0),
        }
    }
    let mut attrs = Attrs::empty();
    if s.add_modifier.contains(RMod::BOLD) {
        attrs |= Attrs::BOLD;
    }
    if s.add_modifier.contains(RMod::ITALIC) {
        attrs |= Attrs::ITALIC;
    }
    if s.add_modifier.contains(RMod::UNDERLINED) {
        attrs |= Attrs::UNDERLINE;
    }
    if s.add_modifier.contains(RMod::REVERSED) {
        attrs |= Attrs::REVERSE;
    }
    if s.add_modifier.contains(RMod::DIM) {
        attrs |= Attrs::DIM;
    }
    if s.add_modifier.contains(RMod::CROSSED_OUT) {
        attrs |= Attrs::STRIKE;
    }
    Style {
        fg: s.fg.map(c),
        bg: s.bg.map(c),
        attrs,
    }
}

/// Map a [`hjkl_buffer::Edit`] to one or more SPEC
/// [`crate::types::Edit`] (`EditOp`) records.
///
/// Most buffer edits map to a single EditOp. Block ops
/// ([`hjkl_buffer::Edit::InsertBlock`] /
/// [`hjkl_buffer::Edit::DeleteBlockChunks`]) emit one EditOp per row
/// touched — they edit non-contiguous cells and a single
/// `range..range` can't represent the rectangle.
///
/// Returns an empty vec when the edit isn't representable (no buffer
/// variant currently fails this check).
fn edit_to_editops(edit: &hjkl_buffer::Edit) -> Vec<crate::types::Edit> {
    use crate::types::{Edit as Op, Pos};
    use hjkl_buffer::Edit as B;
    let to_pos = |p: hjkl_buffer::Position| Pos {
        line: p.row as u32,
        col: p.col as u32,
    };
    match edit {
        B::InsertChar { at, ch } => vec![Op {
            range: to_pos(*at)..to_pos(*at),
            replacement: ch.to_string(),
        }],
        B::InsertStr { at, text } => vec![Op {
            range: to_pos(*at)..to_pos(*at),
            replacement: text.clone(),
        }],
        B::DeleteRange { start, end, .. } => vec![Op {
            range: to_pos(*start)..to_pos(*end),
            replacement: String::new(),
        }],
        B::Replace { start, end, with } => vec![Op {
            range: to_pos(*start)..to_pos(*end),
            replacement: with.clone(),
        }],
        B::JoinLines {
            row,
            count,
            with_space,
        } => {
            // Joining `count` rows after `row` collapses
            // [(row+1, 0) .. (row+count, EOL)] into the joined
            // sentinel. The replacement is either an empty string
            // (gJ) or " " between segments (J).
            let start = Pos {
                line: *row as u32 + 1,
                col: 0,
            };
            let end = Pos {
                line: (*row + *count) as u32,
                col: u32::MAX, // covers to EOL of the last source row
            };
            vec![Op {
                range: start..end,
                replacement: if *with_space {
                    " ".into()
                } else {
                    String::new()
                },
            }]
        }
        B::SplitLines {
            row,
            cols,
            inserted_space: _,
        } => {
            // SplitLines reverses a JoinLines: insert a `\n`
            // (and optional dropped space) at each col on `row`.
            cols.iter()
                .map(|c| {
                    let p = Pos {
                        line: *row as u32,
                        col: *c as u32,
                    };
                    Op {
                        range: p..p,
                        replacement: "\n".into(),
                    }
                })
                .collect()
        }
        B::InsertBlock { at, chunks } => {
            // One EditOp per row in the block — non-contiguous edits.
            chunks
                .iter()
                .enumerate()
                .map(|(i, chunk)| {
                    let p = Pos {
                        line: at.row as u32 + i as u32,
                        col: at.col as u32,
                    };
                    Op {
                        range: p..p,
                        replacement: chunk.clone(),
                    }
                })
                .collect()
        }
        B::DeleteBlockChunks { at, widths } => {
            // One EditOp per row, deleting `widths[i]` chars at
            // `(at.row + i, at.col)`.
            widths
                .iter()
                .enumerate()
                .map(|(i, w)| {
                    let start = Pos {
                        line: at.row as u32 + i as u32,
                        col: at.col as u32,
                    };
                    let end = Pos {
                        line: at.row as u32 + i as u32,
                        col: at.col as u32 + *w as u32,
                    };
                    Op {
                        range: start..end,
                        replacement: String::new(),
                    }
                })
                .collect()
        }
    }
}

/// Sum of bytes from the start of the buffer to the start of `row`.
/// Walks lines + their separating `\n` bytes — matches the canonical
/// `lines().join("\n")` byte rendering used by syntax tooling.
#[inline]
fn buffer_byte_of_row(buf: &hjkl_buffer::Buffer, row: usize) -> usize {
    let n = buf.row_count();
    let row = row.min(n);
    let mut acc = 0usize;
    for r in 0..row {
        acc += buf.line(r).map(str::len).unwrap_or(0);
        if r + 1 < n {
            acc += 1; // separator '\n'
        }
    }
    acc
}

/// Convert an `hjkl_buffer::Position` (char-indexed col) into byte
/// coordinates `(byte_within_buffer, (row, col_byte))` against the
/// **pre-edit** buffer.
fn position_to_byte_coords(
    buf: &hjkl_buffer::Buffer,
    pos: hjkl_buffer::Position,
) -> (usize, (u32, u32)) {
    let row = pos.row.min(buf.row_count().saturating_sub(1));
    let line = buf.line(row).unwrap_or("");
    let col_byte = pos.byte_offset(line);
    let byte = buffer_byte_of_row(buf, row) + col_byte;
    (byte, (row as u32, col_byte as u32))
}

/// Compute the byte position after inserting `text` starting at
/// `start_byte` / `start_pos`. Returns `(end_byte, end_position)`.
fn advance_by_text(text: &str, start_byte: usize, start_pos: (u32, u32)) -> (usize, (u32, u32)) {
    let new_end_byte = start_byte + text.len();
    let newlines = text.bytes().filter(|&b| b == b'\n').count();
    let end_pos = if newlines == 0 {
        (start_pos.0, start_pos.1 + text.len() as u32)
    } else {
        // Bytes after the last newline determine the trailing column.
        let last_nl = text.rfind('\n').unwrap();
        let tail_bytes = (text.len() - last_nl - 1) as u32;
        (start_pos.0 + newlines as u32, tail_bytes)
    };
    (new_end_byte, end_pos)
}

/// Translate a single `hjkl_buffer::Edit` into one or more
/// [`crate::types::ContentEdit`] records using the **pre-edit** buffer
/// state for byte/position lookups. Block ops fan out to one entry per
/// touched row (matches `edit_to_editops`).
fn content_edits_from_buffer_edit(
    buf: &hjkl_buffer::Buffer,
    edit: &hjkl_buffer::Edit,
) -> Vec<crate::types::ContentEdit> {
    use hjkl_buffer::Edit as B;
    use hjkl_buffer::Position;

    let mut out: Vec<crate::types::ContentEdit> = Vec::new();

    match edit {
        B::InsertChar { at, ch } => {
            let (start_byte, start_pos) = position_to_byte_coords(buf, *at);
            let new_end_byte = start_byte + ch.len_utf8();
            let new_end_pos = (start_pos.0, start_pos.1 + ch.len_utf8() as u32);
            out.push(crate::types::ContentEdit {
                start_byte,
                old_end_byte: start_byte,
                new_end_byte,
                start_position: start_pos,
                old_end_position: start_pos,
                new_end_position: new_end_pos,
            });
        }
        B::InsertStr { at, text } => {
            let (start_byte, start_pos) = position_to_byte_coords(buf, *at);
            let (new_end_byte, new_end_pos) = advance_by_text(text, start_byte, start_pos);
            out.push(crate::types::ContentEdit {
                start_byte,
                old_end_byte: start_byte,
                new_end_byte,
                start_position: start_pos,
                old_end_position: start_pos,
                new_end_position: new_end_pos,
            });
        }
        B::DeleteRange { start, end, kind } => {
            let (start, end) = if start <= end {
                (*start, *end)
            } else {
                (*end, *start)
            };
            match kind {
                hjkl_buffer::MotionKind::Char => {
                    let (start_byte, start_pos) = position_to_byte_coords(buf, start);
                    let (old_end_byte, old_end_pos) = position_to_byte_coords(buf, end);
                    out.push(crate::types::ContentEdit {
                        start_byte,
                        old_end_byte,
                        new_end_byte: start_byte,
                        start_position: start_pos,
                        old_end_position: old_end_pos,
                        new_end_position: start_pos,
                    });
                }
                hjkl_buffer::MotionKind::Line => {
                    // Linewise delete drops rows [start.row..=end.row]. Map
                    // to a span from start of `start.row` through start of
                    // (end.row + 1). The buffer's own `do_delete_range`
                    // collapses to row `start.row` after dropping.
                    let lo = start.row;
                    let hi = end.row.min(buf.row_count().saturating_sub(1));
                    let start_byte = buffer_byte_of_row(buf, lo);
                    let next_row_byte = if hi + 1 < buf.row_count() {
                        buffer_byte_of_row(buf, hi + 1)
                    } else {
                        // No row after; clamp to end-of-buffer byte.
                        buffer_byte_of_row(buf, buf.row_count())
                            + buf
                                .line(buf.row_count().saturating_sub(1))
                                .map(str::len)
                                .unwrap_or(0)
                    };
                    out.push(crate::types::ContentEdit {
                        start_byte,
                        old_end_byte: next_row_byte,
                        new_end_byte: start_byte,
                        start_position: (lo as u32, 0),
                        old_end_position: ((hi + 1) as u32, 0),
                        new_end_position: (lo as u32, 0),
                    });
                }
                hjkl_buffer::MotionKind::Block => {
                    // Block delete removes a rectangle of chars per row.
                    // Fan out to one ContentEdit per row.
                    let (left_col, right_col) = (start.col.min(end.col), start.col.max(end.col));
                    for row in start.row..=end.row {
                        let row_start_pos = Position::new(row, left_col);
                        let row_end_pos = Position::new(row, right_col + 1);
                        let (sb, sp) = position_to_byte_coords(buf, row_start_pos);
                        let (eb, ep) = position_to_byte_coords(buf, row_end_pos);
                        if eb <= sb {
                            continue;
                        }
                        out.push(crate::types::ContentEdit {
                            start_byte: sb,
                            old_end_byte: eb,
                            new_end_byte: sb,
                            start_position: sp,
                            old_end_position: ep,
                            new_end_position: sp,
                        });
                    }
                }
            }
        }
        B::Replace { start, end, with } => {
            let (start, end) = if start <= end {
                (*start, *end)
            } else {
                (*end, *start)
            };
            let (start_byte, start_pos) = position_to_byte_coords(buf, start);
            let (old_end_byte, old_end_pos) = position_to_byte_coords(buf, end);
            let (new_end_byte, new_end_pos) = advance_by_text(with, start_byte, start_pos);
            out.push(crate::types::ContentEdit {
                start_byte,
                old_end_byte,
                new_end_byte,
                start_position: start_pos,
                old_end_position: old_end_pos,
                new_end_position: new_end_pos,
            });
        }
        B::JoinLines {
            row,
            count,
            with_space,
        } => {
            // Joining `count` rows after `row` collapses the bytes
            // between EOL of `row` and EOL of `row + count` into either
            // an empty string (gJ) or a single space per join (J — but
            // only when both sides are non-empty; we approximate with
            // a single space for simplicity).
            let row = (*row).min(buf.row_count().saturating_sub(1));
            let last_join_row = (row + count).min(buf.row_count().saturating_sub(1));
            let line = buf.line(row).unwrap_or("");
            let row_eol_byte = buffer_byte_of_row(buf, row) + line.len();
            let row_eol_col = line.len() as u32;
            let next_row_after = last_join_row + 1;
            let old_end_byte = if next_row_after < buf.row_count() {
                buffer_byte_of_row(buf, next_row_after).saturating_sub(1)
            } else {
                buffer_byte_of_row(buf, buf.row_count())
                    + buf
                        .line(buf.row_count().saturating_sub(1))
                        .map(str::len)
                        .unwrap_or(0)
            };
            let last_line = buf.line(last_join_row).unwrap_or("");
            let old_end_pos = (last_join_row as u32, last_line.len() as u32);
            let replacement_len = if *with_space { 1 } else { 0 };
            let new_end_byte = row_eol_byte + replacement_len;
            let new_end_pos = (row as u32, row_eol_col + replacement_len as u32);
            out.push(crate::types::ContentEdit {
                start_byte: row_eol_byte,
                old_end_byte,
                new_end_byte,
                start_position: (row as u32, row_eol_col),
                old_end_position: old_end_pos,
                new_end_position: new_end_pos,
            });
        }
        B::SplitLines {
            row,
            cols,
            inserted_space,
        } => {
            // Splits insert "\n" (or "\n " inverse) at each col on `row`.
            // The buffer applies all splits left-to-right via the
            // do_split_lines path; we emit one ContentEdit per col,
            // each treated as an insert at that col on `row`. Note: the
            // buffer state during emission is *pre-edit*, so all cols
            // index into the same pre-edit row.
            let row = (*row).min(buf.row_count().saturating_sub(1));
            let line = buf.line(row).unwrap_or("");
            let row_byte = buffer_byte_of_row(buf, row);
            let insert = if *inserted_space { "\n " } else { "\n" };
            for &c in cols {
                let pos = Position::new(row, c);
                let col_byte = pos.byte_offset(line);
                let start_byte = row_byte + col_byte;
                let start_pos = (row as u32, col_byte as u32);
                let (new_end_byte, new_end_pos) = advance_by_text(insert, start_byte, start_pos);
                out.push(crate::types::ContentEdit {
                    start_byte,
                    old_end_byte: start_byte,
                    new_end_byte,
                    start_position: start_pos,
                    old_end_position: start_pos,
                    new_end_position: new_end_pos,
                });
            }
        }
        B::InsertBlock { at, chunks } => {
            // One ContentEdit per chunk; each lands at `(at.row + i,
            // at.col)` in the pre-edit buffer.
            for (i, chunk) in chunks.iter().enumerate() {
                let pos = Position::new(at.row + i, at.col);
                let (start_byte, start_pos) = position_to_byte_coords(buf, pos);
                let (new_end_byte, new_end_pos) = advance_by_text(chunk, start_byte, start_pos);
                out.push(crate::types::ContentEdit {
                    start_byte,
                    old_end_byte: start_byte,
                    new_end_byte,
                    start_position: start_pos,
                    old_end_position: start_pos,
                    new_end_position: new_end_pos,
                });
            }
        }
        B::DeleteBlockChunks { at, widths } => {
            for (i, w) in widths.iter().enumerate() {
                let row = at.row + i;
                let start_pos = Position::new(row, at.col);
                let end_pos = Position::new(row, at.col + *w);
                let (sb, sp) = position_to_byte_coords(buf, start_pos);
                let (eb, ep) = position_to_byte_coords(buf, end_pos);
                if eb <= sb {
                    continue;
                }
                out.push(crate::types::ContentEdit {
                    start_byte: sb,
                    old_end_byte: eb,
                    new_end_byte: sb,
                    start_position: sp,
                    old_end_position: ep,
                    new_end_position: sp,
                });
            }
        }
    }

    out
}

/// Where the cursor should land in the viewport after a `z`-family
/// scroll (`zz` / `zt` / `zb`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CursorScrollTarget {
    Center,
    Top,
    Bottom,
}

// ── Trait-surface cast helpers ────────────────────────────────────
//
// 0.0.42 (Patch C-δ.7): the helpers introduced in 0.0.41 were
// promoted to [`crate::buf_helpers`] so `vim.rs` free fns can route
// their reaches through the same primitives. Re-import via
// `use` so the editor body keeps its terse call shape.

use crate::buf_helpers::{
    apply_buffer_edit, buf_cursor_pos, buf_cursor_rc, buf_cursor_row, buf_line, buf_lines_to_vec,
    buf_row_count, buf_set_cursor_rc,
};

pub struct Editor<
    B: crate::types::Buffer = hjkl_buffer::Buffer,
    H: crate::types::Host = crate::types::DefaultHost,
> {
    pub keybinding_mode: KeybindingMode,
    /// Set when the user yanks/cuts; caller drains this to write to OS clipboard.
    pub last_yank: Option<String>,
    /// All vim-specific state (mode, pending operator, count, dot-repeat, ...).
    /// Internal — exposed via Editor accessor methods
    /// ([`Editor::buffer_mark`], [`Editor::last_jump_back`],
    /// [`Editor::last_edit_pos`], [`Editor::take_lsp_intent`], …).
    pub(crate) vim: VimState,
    /// Undo history: each entry is (lines, cursor) before the edit.
    /// Internal — managed by [`Editor::push_undo`] / [`Editor::restore`]
    /// / [`Editor::pop_last_undo`].
    pub(crate) undo_stack: Vec<(Vec<String>, (usize, usize))>,
    /// Redo history: entries pushed when undoing.
    pub(super) redo_stack: Vec<(Vec<String>, (usize, usize))>,
    /// Set whenever the buffer content changes; cleared by `take_dirty`.
    pub(super) content_dirty: bool,
    /// Cached snapshot of `lines().join("\n") + "\n"` wrapped in an Arc
    /// so repeated `content_arc()` calls within the same un-mutated
    /// window are free (ref-count bump instead of a full-buffer join).
    /// Invalidated by every [`mark_content_dirty`] call.
    pub(super) cached_content: Option<std::sync::Arc<String>>,
    /// Last rendered viewport height (text rows only, no chrome). Written
    /// by the draw path via [`set_viewport_height`] so the scroll helpers
    /// can clamp the cursor to stay visible without plumbing the height
    /// through every call.
    pub(super) viewport_height: AtomicU16,
    /// Pending LSP intent set by a normal-mode chord (e.g. `gd` for
    /// goto-definition). The host app drains this each step and fires
    /// the matching request against its own LSP client.
    pub(super) pending_lsp: Option<LspIntent>,
    /// Pending [`crate::types::FoldOp`]s raised by `z…` keystrokes,
    /// the `:fold*` Ex commands, or the edit pipeline's
    /// "edits-inside-a-fold open it" invalidation. Drained by hosts
    /// via [`Editor::take_fold_ops`]; the engine also applies each op
    /// locally through [`crate::buffer_impl::BufferFoldProviderMut`]
    /// so the in-tree buffer fold storage stays in sync without host
    /// cooperation. Introduced in 0.0.38 (Patch C-δ.4).
    pub(super) pending_fold_ops: Vec<crate::types::FoldOp>,
    /// Buffer storage.
    ///
    /// 0.1.0 (Patch C-δ): generic over `B: Buffer` per SPEC §"Editor
    /// surface". Default `B = hjkl_buffer::Buffer`. The vim FSM body
    /// and `Editor::mutate_edit` are concrete on `hjkl_buffer::Buffer`
    /// for 0.1.0 — see SPEC.md §"Out of scope" and `crate::buf_helpers::apply_buffer_edit`.
    pub(super) buffer: B,
    /// Style intern table for the migration buffer's opaque
    /// `Span::style` ids. Phase 7d-ii-a wiring — `apply_window_spans`
    /// produces `(start, end, Style)` tuples for the textarea; we
    /// translate those to `hjkl_buffer::Span` by interning the
    /// `Style` here and storing the table index. The render path's
    /// `StyleResolver` looks the style back up by id.
    ///
    /// Behind the `ratatui` feature; non-ratatui hosts use the
    /// engine-native [`crate::types::Style`] surface via
    /// [`Editor::intern_engine_style`] (which lives on a parallel
    /// engine-side table when ratatui is off).
    #[cfg(feature = "ratatui")]
    pub(super) style_table: Vec<ratatui::style::Style>,
    /// Engine-native style intern table. Used directly by
    /// [`Editor::intern_engine_style`] when the `ratatui` feature is
    /// off; when it's on, the table is derived from `style_table` via
    /// [`ratatui_style_to_engine`] / [`engine_style_to_ratatui`].
    #[cfg(not(feature = "ratatui"))]
    pub(super) engine_style_table: Vec<crate::types::Style>,
    /// Vim-style register bank — `"`, `"0`–`"9`, `"a`–`"z`. Sources
    /// every `p` / `P` via the active selector (default unnamed).
    /// Internal — read via [`Editor::registers`]; mutated by yank /
    /// delete / paste FSM paths and by [`Editor::seed_yank`].
    pub(crate) registers: crate::registers::Registers,
    /// Per-row syntax styling, kept here so the host can do
    /// incremental window updates (see `apply_window_spans` in
    /// the host). Same `(start_byte, end_byte, Style)` tuple shape
    /// the textarea used to host. The Buffer-side opaque-id spans are
    /// derived from this on every install. Behind the `ratatui`
    /// feature.
    #[cfg(feature = "ratatui")]
    pub styled_spans: Vec<Vec<(usize, usize, ratatui::style::Style)>>,
    /// Per-editor settings tweakable via `:set`. Exposed by reference
    /// so handlers (indent, search) read the live value rather than a
    /// snapshot taken at startup. Read via [`Editor::settings`];
    /// mutate via [`Editor::settings_mut`].
    pub(crate) settings: Settings,
    /// Unified named-marks map. Lowercase letters (`'a`–`'z`) are
    /// per-Editor / "buffer-scope-equivalent" — set by `m{a-z}`, read
    /// by `'{a-z}` / `` `{a-z} ``. Uppercase letters (`'A`–`'Z`) are
    /// "file marks" that survive [`Editor::set_content`] calls so
    /// they persist across tab swaps within the same Editor.
    ///
    /// 0.0.36: consolidated from three former storages:
    /// - `hjkl_buffer::Buffer::marks` (deleted; was unused dead code).
    /// - `vim::VimState::marks` (lowercase) (deleted).
    /// - `Editor::file_marks` (uppercase) (replaced by this map).
    ///
    /// `BTreeMap` so iteration is deterministic for snapshot tests
    /// and the `:marks` ex command. Mark-shift on edits is handled
    /// by [`Editor::shift_marks_after_edit`].
    pub(crate) marks: std::collections::BTreeMap<char, (usize, usize)>,
    /// Block ranges (`(start_row, end_row)` inclusive) the host has
    /// extracted from a syntax tree. `:foldsyntax` reads these to
    /// populate folds. The host refreshes them on every re-parse via
    /// [`Editor::set_syntax_fold_ranges`]; ex commands read them via
    /// [`Editor::syntax_fold_ranges`].
    pub(crate) syntax_fold_ranges: Vec<(usize, usize)>,
    /// Pending edit log drained by [`Editor::take_changes`]. Each entry
    /// is a SPEC [`crate::types::Edit`] mapped from the underlying
    /// `hjkl_buffer::Edit` operation. Compound ops (JoinLines,
    /// SplitLines, InsertBlock, DeleteBlockChunks) emit a single
    /// best-effort EditOp covering the touched range; hosts wanting
    /// per-cell deltas should diff their own snapshot of `lines()`.
    /// Sealed at 0.1.0 trait extraction.
    /// Drained by [`Editor::take_changes`].
    pub(crate) change_log: Vec<crate::types::Edit>,
    /// Vim's "sticky column" (curswant). `None` before the first
    /// motion — the next vertical motion bootstraps from the live
    /// cursor column. Horizontal motions refresh this to the new
    /// column; vertical motions read it back so bouncing through a
    /// shorter row doesn't drag the cursor to col 0. Hoisted out of
    /// `hjkl_buffer::Buffer` (and `VimState`) in 0.0.28 — Editor is
    /// the single owner now. Buffer motion methods that need it
    /// take a `&mut Option<usize>` parameter.
    pub(crate) sticky_col: Option<usize>,
    /// Host adapter for clipboard, cursor-shape, time, viewport, and
    /// search-prompt / cancellation side-channels.
    ///
    /// 0.1.0 (Patch C-δ): generic over `H: Host` per SPEC §"Editor
    /// surface". Default `H = DefaultHost`. The pre-0.1.0 `EngineHost`
    /// dyn-shim is gone — every method now dispatches through `H`'s
    /// `Host` trait surface directly.
    pub(crate) host: H,
    /// Last public mode the cursor-shape emitter saw. Drives
    /// [`Editor::emit_cursor_shape_if_changed`] so `Host::emit_cursor_shape`
    /// fires exactly once per mode transition without sprinkling the
    /// call across every `vim.mode = ...` site.
    pub(crate) last_emitted_mode: crate::VimMode,
    /// Search FSM state (pattern + per-row match cache + wrapscan).
    /// 0.0.35: relocated out of `hjkl_buffer::Buffer` per
    /// `DESIGN_33_METHOD_CLASSIFICATION.md` step 1.
    /// 0.0.37: the buffer-side bridge (`Buffer::search_pattern`) is
    /// gone; `BufferView` now takes the active regex as a `&Regex`
    /// parameter, sourced from `Editor::search_state().pattern`.
    pub(crate) search_state: crate::search::SearchState,
    /// Per-row syntax span overlay. Source of truth for the host's
    /// renderer ([`hjkl_buffer::BufferView::spans`]). Populated by
    /// [`Editor::install_syntax_spans`] /
    /// [`Editor::install_ratatui_syntax_spans`] (and, in due course,
    /// by `Host::syntax_highlights` once the engine drives that path
    /// directly).
    ///
    /// 0.0.37: lifted out of `hjkl_buffer::Buffer` per step 3 of
    /// `DESIGN_33_METHOD_CLASSIFICATION.md`. The buffer-side cache +
    /// `Buffer::set_spans` / `Buffer::spans` accessors are gone.
    pub(crate) buffer_spans: Vec<Vec<hjkl_buffer::Span>>,
    /// Pending `ContentEdit` records emitted by `mutate_edit`. Drained by
    /// hosts via [`Editor::take_content_edits`] for fan-in to a syntax
    /// tree (or any other content-change observer that needs byte-level
    /// position deltas). Edges are byte-indexed and `(row, col_byte)`.
    pub(crate) pending_content_edits: Vec<crate::types::ContentEdit>,
    /// Pending "reset" flag set when the entire buffer is replaced
    /// (e.g. `set_content` / `restore`). Supersedes any queued
    /// `pending_content_edits` on the same frame: hosts call
    /// [`Editor::take_content_reset`] before draining edits.
    pub(crate) pending_content_reset: bool,
}

/// Vim-style options surfaced by `:set`. New fields land here as
/// individual ex commands gain `:set` plumbing.
#[derive(Debug, Clone)]
pub struct Settings {
    /// Spaces per shift step for `>>` / `<<` / `Ctrl-T` / `Ctrl-D`.
    pub shiftwidth: usize,
    /// Visual width of a `\t` character. Stored for future render
    /// hookup; not yet consumed by the buffer renderer.
    pub tabstop: usize,
    /// When true, `/` / `?` patterns and `:s/.../.../` ignore case
    /// without an explicit `i` flag.
    pub ignore_case: bool,
    /// When true *and* `ignore_case` is true, an uppercase letter in
    /// the pattern flips that search back to case-sensitive. Matches
    /// vim's `:set smartcase`. Default `false`.
    pub smartcase: bool,
    /// Wrap searches past buffer ends. Matches vim's `:set wrapscan`.
    /// Default `true`.
    pub wrapscan: bool,
    /// Wrap column for `gq{motion}` text reflow. Vim's default is 79.
    pub textwidth: usize,
    /// When `true`, the Tab key in insert mode inserts `tabstop` spaces
    /// instead of a literal `\t`. Matches vim's `:set expandtab`.
    /// Default `false`.
    pub expandtab: bool,
    /// Soft-wrap mode the renderer + scroll math + `gj` / `gk` use.
    /// Default is [`hjkl_buffer::Wrap::None`] — long lines extend
    /// past the right edge and `top_col` clips the left side.
    /// `:set wrap` flips to char-break wrap; `:set linebreak` flips
    /// to word-break wrap; `:set nowrap` resets.
    pub wrap: hjkl_buffer::Wrap,
    /// When true, the engine drops every edit before it touches the
    /// buffer — undo, dirty flag, and change log all stay clean.
    /// Matches vim's `:set readonly` / `:set ro`. Default `false`.
    pub readonly: bool,
    /// When `true`, pressing Enter in insert mode copies the leading
    /// whitespace of the current line onto the new line. Matches vim's
    /// `:set autoindent`. Default `true` (vim parity).
    pub autoindent: bool,
    /// Cap on undo-stack length. Older entries are pruned past this
    /// bound. `0` means unlimited. Matches vim's `:set undolevels`.
    /// Default `1000`.
    pub undo_levels: u32,
    /// When `true`, cursor motions inside insert mode break the
    /// current undo group (so a single `u` only reverses the run of
    /// keystrokes that preceded the motion). Default `true`.
    /// Currently a no-op — engine doesn't yet break the undo group
    /// on insert-mode motions; field is wired through `:set
    /// undobreak` for forward compatibility.
    pub undo_break_on_motion: bool,
    /// Vim-flavoured "what counts as a word" character class.
    /// Comma-separated tokens: `@` = `is_alphabetic()`, `_` = literal
    /// `_`, `48-57` = decimal char range, bare integer = single char
    /// code, single ASCII punctuation = literal. Default
    /// `"@,48-57,_,192-255"` matches vim.
    pub iskeyword: String,
    /// Multi-key sequence timeout (e.g. `gg`, `dd`). When the user
    /// pauses longer than this between keys, any pending prefix is
    /// abandoned and the next key starts a fresh sequence. Matches
    /// vim's `:set timeoutlen` / `:set tm` (millis). Default 1000ms.
    pub timeout_len: core::time::Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            shiftwidth: 2,
            tabstop: 8,
            ignore_case: false,
            smartcase: false,
            wrapscan: true,
            textwidth: 79,
            expandtab: false,
            wrap: hjkl_buffer::Wrap::None,
            readonly: false,
            autoindent: true,
            undo_levels: 1000,
            undo_break_on_motion: true,
            iskeyword: "@,48-57,_,192-255".to_string(),
            timeout_len: core::time::Duration::from_millis(1000),
        }
    }
}

/// Translate a SPEC [`crate::types::Options`] into the engine's
/// internal [`Settings`] representation. Field-by-field map; the
/// shapes are isomorphic except for type widths
/// (`u32` vs `usize`, [`crate::types::WrapMode`] vs
/// [`hjkl_buffer::Wrap`]). 0.1.0 (Patch C-δ) collapses both into one
/// type once the `Editor<B, H>::new(buffer, host, options)` constructor
/// is the canonical entry point.
fn settings_from_options(o: &crate::types::Options) -> Settings {
    Settings {
        shiftwidth: o.shiftwidth as usize,
        tabstop: o.tabstop as usize,
        ignore_case: o.ignorecase,
        smartcase: o.smartcase,
        wrapscan: o.wrapscan,
        textwidth: o.textwidth as usize,
        expandtab: o.expandtab,
        wrap: match o.wrap {
            crate::types::WrapMode::None => hjkl_buffer::Wrap::None,
            crate::types::WrapMode::Char => hjkl_buffer::Wrap::Char,
            crate::types::WrapMode::Word => hjkl_buffer::Wrap::Word,
        },
        readonly: o.readonly,
        autoindent: o.autoindent,
        undo_levels: o.undo_levels,
        undo_break_on_motion: o.undo_break_on_motion,
        iskeyword: o.iskeyword.clone(),
        timeout_len: o.timeout_len,
    }
}

/// Host-observable LSP requests triggered by editor bindings. The
/// hjkl-engine crate doesn't talk to an LSP itself — it just raises an
/// intent that the TUI layer picks up and routes to `sqls`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LspIntent {
    /// `gd` — textDocument/definition at the cursor.
    GotoDefinition,
}

impl<H: crate::types::Host> Editor<hjkl_buffer::Buffer, H> {
    /// Build an [`Editor`] from a buffer, host adapter, and SPEC options.
    ///
    /// 0.1.0 (Patch C-δ): canonical, frozen constructor per SPEC §"Editor
    /// surface". Replaces the pre-0.1.0 `Editor::new(KeybindingMode)` /
    /// `with_host` / `with_options` triad — there is no shim.
    ///
    /// Consumers that don't need a custom host pass
    /// [`crate::types::DefaultHost::new()`]; consumers that don't need
    /// custom options pass [`crate::types::Options::default()`].
    pub fn new(buffer: hjkl_buffer::Buffer, host: H, options: crate::types::Options) -> Self {
        let settings = settings_from_options(&options);
        Self {
            keybinding_mode: KeybindingMode::Vim,
            last_yank: None,
            vim: VimState::default(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            content_dirty: false,
            cached_content: None,
            viewport_height: AtomicU16::new(0),
            pending_lsp: None,
            pending_fold_ops: Vec::new(),
            buffer,
            #[cfg(feature = "ratatui")]
            style_table: Vec::new(),
            #[cfg(not(feature = "ratatui"))]
            engine_style_table: Vec::new(),
            registers: crate::registers::Registers::default(),
            #[cfg(feature = "ratatui")]
            styled_spans: Vec::new(),
            settings,
            marks: std::collections::BTreeMap::new(),
            syntax_fold_ranges: Vec::new(),
            change_log: Vec::new(),
            sticky_col: None,
            host,
            last_emitted_mode: crate::VimMode::Normal,
            search_state: crate::search::SearchState::new(),
            buffer_spans: Vec::new(),
            pending_content_edits: Vec::new(),
            pending_content_reset: false,
        }
    }
}

impl<B: crate::types::Buffer, H: crate::types::Host> Editor<B, H> {
    /// Borrow the buffer (typed `&B`). Host renders through this via
    /// `hjkl_buffer::BufferView` when `B = hjkl_buffer::Buffer`.
    pub fn buffer(&self) -> &B {
        &self.buffer
    }

    /// Mutably borrow the buffer (typed `&mut B`).
    pub fn buffer_mut(&mut self) -> &mut B {
        &mut self.buffer
    }

    /// Borrow the host adapter directly (typed `&H`).
    pub fn host(&self) -> &H {
        &self.host
    }

    /// Mutably borrow the host adapter (typed `&mut H`).
    pub fn host_mut(&mut self) -> &mut H {
        &mut self.host
    }
}

impl<H: crate::types::Host> Editor<hjkl_buffer::Buffer, H> {
    /// Update the active `iskeyword` spec for word motions
    /// (`w`/`b`/`e`/`ge` and engine-side `*`/`#` pickup). 0.0.28
    /// hoisted iskeyword storage out of `Buffer` — `Editor` is the
    /// single owner now. Equivalent to assigning
    /// `settings_mut().iskeyword` directly; the dedicated setter is
    /// retained for source-compatibility with 0.0.27 callers.
    pub fn set_iskeyword(&mut self, spec: impl Into<String>) {
        self.settings.iskeyword = spec.into();
    }

    /// Emit `Host::emit_cursor_shape` if the public mode has changed
    /// since the last emit. Engine calls this at the end of every input
    /// step so mode transitions surface to the host without sprinkling
    /// the call across every `vim.mode = ...` site.
    pub(crate) fn emit_cursor_shape_if_changed(&mut self) {
        let mode = self.vim_mode();
        if mode == self.last_emitted_mode {
            return;
        }
        let shape = match mode {
            crate::VimMode::Insert => crate::types::CursorShape::Bar,
            _ => crate::types::CursorShape::Block,
        };
        self.host.emit_cursor_shape(shape);
        self.last_emitted_mode = mode;
    }

    /// Record a yank/cut payload. Writes both the legacy
    /// [`Editor::last_yank`] field (drained directly by 0.0.28-era
    /// hosts) and the new [`crate::types::Host::write_clipboard`]
    /// side-channel (Patch B). Consumers should migrate to a `Host`
    /// impl whose `write_clipboard` queues the platform-clipboard
    /// write; the `last_yank` mirror will be removed at 0.1.0.
    pub(crate) fn record_yank_to_host(&mut self, text: String) {
        self.host.write_clipboard(text.clone());
        self.last_yank = Some(text);
    }

    /// Vim's sticky column (curswant). `None` before the first motion;
    /// hosts shouldn't normally need to read this directly — it's
    /// surfaced for migration off `Buffer::sticky_col` and for
    /// snapshot tests.
    pub fn sticky_col(&self) -> Option<usize> {
        self.sticky_col
    }

    /// Replace the sticky column. Hosts should rarely touch this —
    /// motion code maintains it through the standard horizontal /
    /// vertical motion paths.
    pub fn set_sticky_col(&mut self, col: Option<usize>) {
        self.sticky_col = col;
    }

    /// Host hook: replace the cached syntax-derived block ranges that
    /// `:foldsyntax` consumes. the host calls this on every re-parse;
    /// the cost is just a `Vec` swap.
    /// Look up a named mark by character. Returns `(row, col)` if
    /// set; `None` otherwise. Both lowercase (`'a`–`'z`) and
    /// uppercase (`'A`–`'Z`) marks live in the same unified
    /// [`Editor::marks`] map as of 0.0.36.
    pub fn mark(&self, c: char) -> Option<(usize, usize)> {
        self.marks.get(&c).copied()
    }

    /// Set the named mark `c` to `(row, col)`. Used by the FSM's
    /// `m{a-zA-Z}` keystroke and by [`Editor::restore_snapshot`].
    pub fn set_mark(&mut self, c: char, pos: (usize, usize)) {
        self.marks.insert(c, pos);
    }

    /// Remove the named mark `c` (no-op if unset).
    pub fn clear_mark(&mut self, c: char) {
        self.marks.remove(&c);
    }

    /// Look up a buffer-local lowercase mark (`'a`–`'z`). Kept as a
    /// thin wrapper over [`Editor::mark`] for source compatibility
    /// with pre-0.0.36 callers; new code should call
    /// [`Editor::mark`] directly.
    #[deprecated(
        since = "0.0.36",
        note = "use Editor::mark — lowercase + uppercase marks now live in a single map"
    )]
    pub fn buffer_mark(&self, c: char) -> Option<(usize, usize)> {
        self.mark(c)
    }

    /// Discard the most recent undo entry. Used by ex commands that
    /// pre-emptively pushed an undo state (`:s`, `:r`) but ended up
    /// matching nothing — popping prevents a no-op undo step from
    /// polluting the user's history.
    ///
    /// Returns `true` if an entry was discarded.
    pub fn pop_last_undo(&mut self) -> bool {
        self.undo_stack.pop().is_some()
    }

    /// Read all named marks set this session — both lowercase
    /// (`'a`–`'z`) and uppercase (`'A`–`'Z`). Iteration is
    /// deterministic (BTreeMap-ordered) so snapshot / `:marks`
    /// output is stable.
    pub fn marks(&self) -> impl Iterator<Item = (char, (usize, usize))> + '_ {
        self.marks.iter().map(|(c, p)| (*c, *p))
    }

    /// Read all buffer-local lowercase marks. Kept for source
    /// compatibility with pre-0.0.36 callers (e.g. `:marks` ex
    /// command); new code should use [`Editor::marks`] which
    /// iterates the unified map.
    #[deprecated(
        since = "0.0.36",
        note = "use Editor::marks — lowercase + uppercase marks now live in a single map"
    )]
    pub fn buffer_marks(&self) -> impl Iterator<Item = (char, (usize, usize))> + '_ {
        self.marks
            .iter()
            .filter(|(c, _)| c.is_ascii_lowercase())
            .map(|(c, p)| (*c, *p))
    }

    /// Position the cursor was at when the user last jumped via
    /// `<C-o>` / `g;` / similar. `None` before any jump.
    pub fn last_jump_back(&self) -> Option<(usize, usize)> {
        self.vim.jump_back.last().copied()
    }

    /// Position of the last edit (where `.` would replay). `None` if
    /// no edit has happened yet in this session.
    pub fn last_edit_pos(&self) -> Option<(usize, usize)> {
        self.vim.last_edit_pos
    }

    /// Read-only view of the file-marks table — uppercase / "file"
    /// marks (`'A`–`'Z`) the host has set this session. Returns an
    /// iterator of `(mark_char, (row, col))` pairs.
    ///
    /// Mutate via the FSM (`m{A-Z}` keystroke) or via
    /// [`Editor::restore_snapshot`].
    ///
    /// 0.0.36: file marks now live in the unified [`Editor::marks`]
    /// map; this accessor is kept for source compatibility and
    /// filters the unified map to uppercase entries.
    pub fn file_marks(&self) -> impl Iterator<Item = (char, (usize, usize))> + '_ {
        self.marks
            .iter()
            .filter(|(c, _)| c.is_ascii_uppercase())
            .map(|(c, p)| (*c, *p))
    }

    /// Read-only view of the cached syntax-derived block ranges that
    /// `:foldsyntax` consumes. Returns the slice the host last
    /// installed via [`Editor::set_syntax_fold_ranges`]; empty when
    /// no syntax integration is active.
    pub fn syntax_fold_ranges(&self) -> &[(usize, usize)] {
        &self.syntax_fold_ranges
    }

    pub fn set_syntax_fold_ranges(&mut self, ranges: Vec<(usize, usize)>) {
        self.syntax_fold_ranges = ranges;
    }

    /// Live settings (read-only). `:set` mutates these via
    /// [`Editor::settings_mut`].
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Live settings (mutable). `:set` flows through here to mutate
    /// shiftwidth / tabstop / textwidth / ignore_case / wrap. Hosts
    /// configuring at startup typically construct a [`Settings`]
    /// snapshot and overwrite via `*editor.settings_mut() = …`.
    pub fn settings_mut(&mut self) -> &mut Settings {
        &mut self.settings
    }

    /// Returns `true` when `:set readonly` is active. Convenience
    /// accessor for hosts that cannot import the internal [`Settings`]
    /// type. Phase 5 binary uses this to gate `:w` writes.
    pub fn is_readonly(&self) -> bool {
        self.settings.readonly
    }

    /// Borrow the engine search state. Hosts inspecting the
    /// committed `/` / `?` pattern (e.g. for status-line display) or
    /// feeding the active regex into `BufferView::search_pattern`
    /// read it from here.
    pub fn search_state(&self) -> &crate::search::SearchState {
        &self.search_state
    }

    /// Mutable engine search state. Hosts driving search
    /// programmatically (test fixtures, scripted demos) write the
    /// pattern through here.
    pub fn search_state_mut(&mut self) -> &mut crate::search::SearchState {
        &mut self.search_state
    }

    /// Install `pattern` as the active search regex on the engine
    /// state and clear the cached row matches. Pass `None` to clear.
    /// 0.0.37: dropped the buffer-side mirror that 0.0.35 introduced
    /// — `BufferView` now takes the regex through its `search_pattern`
    /// field per step 3 of `DESIGN_33_METHOD_CLASSIFICATION.md`.
    pub fn set_search_pattern(&mut self, pattern: Option<regex::Regex>) {
        self.search_state.set_pattern(pattern);
    }

    /// Drive `n` (or the `/` commit equivalent) — advance the cursor
    /// to the next match of `search_state.pattern` from the cursor's
    /// current position. Returns `true` when a match was found.
    /// `skip_current = true` excludes a match the cursor sits on.
    pub fn search_advance_forward(&mut self, skip_current: bool) -> bool {
        crate::search::search_forward(&mut self.buffer, &mut self.search_state, skip_current)
    }

    /// Drive `N` — symmetric counterpart of [`Editor::search_advance_forward`].
    pub fn search_advance_backward(&mut self, skip_current: bool) -> bool {
        crate::search::search_backward(&mut self.buffer, &mut self.search_state, skip_current)
    }

    /// Install styled syntax spans using `ratatui::style::Style`. The
    /// ratatui-flavoured variant of [`Editor::install_syntax_spans`].
    /// Drops zero-width runs and clamps `end` to the line's char length
    /// so the buffer cache doesn't see runaway ranges. Behind the
    /// `ratatui` feature; non-ratatui hosts use the unprefixed
    /// [`Editor::install_syntax_spans`] (engine-native `Style`).
    ///
    /// Renamed from `install_syntax_spans` in 0.0.32 — the unprefixed
    /// name now belongs to the engine-native variant per SPEC 0.1.0
    /// freeze ("engine never imports ratatui").
    #[cfg(feature = "ratatui")]
    pub fn install_ratatui_syntax_spans(
        &mut self,
        spans: Vec<Vec<(usize, usize, ratatui::style::Style)>>,
    ) {
        let line_byte_lens: Vec<usize> = (0..buf_row_count(&self.buffer))
            .map(|r| buf_line(&self.buffer, r).map(str::len).unwrap_or(0))
            .collect();
        let mut by_row: Vec<Vec<hjkl_buffer::Span>> = Vec::with_capacity(spans.len());
        for (row, row_spans) in spans.iter().enumerate() {
            let line_len = line_byte_lens.get(row).copied().unwrap_or(0);
            let mut translated = Vec::with_capacity(row_spans.len());
            for (start, end, style) in row_spans {
                let end_clamped = (*end).min(line_len);
                if end_clamped <= *start {
                    continue;
                }
                let id = self.intern_ratatui_style(*style);
                translated.push(hjkl_buffer::Span::new(*start, end_clamped, id));
            }
            by_row.push(translated);
        }
        self.buffer_spans = by_row;
        self.styled_spans = spans;
    }

    /// Snapshot of the unnamed register (the default `p` / `P` source).
    pub fn yank(&self) -> &str {
        &self.registers.unnamed.text
    }

    /// Borrow the full register bank — `"`, `"0`–`"9`, `"a`–`"z`.
    pub fn registers(&self) -> &crate::registers::Registers {
        &self.registers
    }

    /// Host hook: load the OS clipboard's contents into the `"+` / `"*`
    /// register slot. the host calls this before letting vim consume a
    /// paste so `"*p` / `"+p` reflect the live clipboard rather than a
    /// stale snapshot from the last yank.
    pub fn sync_clipboard_register(&mut self, text: String, linewise: bool) {
        self.registers.set_clipboard(text, linewise);
    }

    /// True when the user's pending register selector is `+` or `*`.
    /// the host peeks this so it can refresh `sync_clipboard_register`
    /// only when a clipboard read is actually about to happen.
    pub fn pending_register_is_clipboard(&self) -> bool {
        matches!(self.vim.pending_register, Some('+') | Some('*'))
    }

    /// Replace the unnamed register without touching any other slot.
    /// For host-driven imports (e.g. system clipboard); operator
    /// code uses [`record_yank`] / [`record_delete`].
    pub fn set_yank(&mut self, text: impl Into<String>) {
        let text = text.into();
        let linewise = self.vim.yank_linewise;
        self.registers.unnamed = crate::registers::Slot { text, linewise };
    }

    /// Record a yank into `"` and `"0`, plus the named target if the
    /// user prefixed `"reg`. Updates `vim.yank_linewise` for the
    /// paste path.
    pub(crate) fn record_yank(&mut self, text: String, linewise: bool) {
        self.vim.yank_linewise = linewise;
        let target = self.vim.pending_register.take();
        self.registers.record_yank(text, linewise, target);
    }

    /// Direct write to a named register slot — bypasses the unnamed
    /// `"` and `"0` updates that `record_yank` does. Used by the
    /// macro recorder so finishing a `q{reg}` recording doesn't
    /// pollute the user's last yank.
    pub(crate) fn set_named_register_text(&mut self, reg: char, text: String) {
        if let Some(slot) = match reg {
            'a'..='z' => Some(&mut self.registers.named[(reg as u8 - b'a') as usize]),
            'A'..='Z' => {
                Some(&mut self.registers.named[(reg.to_ascii_lowercase() as u8 - b'a') as usize])
            }
            _ => None,
        } {
            slot.text = text;
            slot.linewise = false;
        }
    }

    /// Record a delete / change into `"` and the `"1`–`"9` ring.
    /// Honours the active named-register prefix.
    pub(crate) fn record_delete(&mut self, text: String, linewise: bool) {
        self.vim.yank_linewise = linewise;
        let target = self.vim.pending_register.take();
        self.registers.record_delete(text, linewise, target);
    }

    /// Install styled syntax spans using the engine-native
    /// [`crate::types::Style`]. Always available, regardless of the
    /// `ratatui` feature. Hosts depending on ratatui can use the
    /// ratatui-flavoured [`Editor::install_ratatui_syntax_spans`].
    ///
    /// Renamed from `install_engine_syntax_spans` in 0.0.32 — at the
    /// 0.1.0 freeze the unprefixed name is the universally-available
    /// engine-native variant ("engine never imports ratatui").
    pub fn install_syntax_spans(&mut self, spans: Vec<Vec<(usize, usize, crate::types::Style)>>) {
        let line_byte_lens: Vec<usize> = (0..buf_row_count(&self.buffer))
            .map(|r| buf_line(&self.buffer, r).map(str::len).unwrap_or(0))
            .collect();
        let mut by_row: Vec<Vec<hjkl_buffer::Span>> = Vec::with_capacity(spans.len());
        #[cfg(feature = "ratatui")]
        let mut ratatui_spans: Vec<Vec<(usize, usize, ratatui::style::Style)>> =
            Vec::with_capacity(spans.len());
        for (row, row_spans) in spans.iter().enumerate() {
            let line_len = line_byte_lens.get(row).copied().unwrap_or(0);
            let mut translated = Vec::with_capacity(row_spans.len());
            #[cfg(feature = "ratatui")]
            let mut translated_r = Vec::with_capacity(row_spans.len());
            for (start, end, style) in row_spans {
                let end_clamped = (*end).min(line_len);
                if end_clamped <= *start {
                    continue;
                }
                let id = self.intern_style(*style);
                translated.push(hjkl_buffer::Span::new(*start, end_clamped, id));
                #[cfg(feature = "ratatui")]
                translated_r.push((*start, end_clamped, engine_style_to_ratatui(*style)));
            }
            by_row.push(translated);
            #[cfg(feature = "ratatui")]
            ratatui_spans.push(translated_r);
        }
        self.buffer_spans = by_row;
        #[cfg(feature = "ratatui")]
        {
            self.styled_spans = ratatui_spans;
        }
    }

    /// Intern a `ratatui::style::Style` and return the opaque id used
    /// in `hjkl_buffer::Span::style`. The ratatui-flavoured variant of
    /// [`Editor::intern_style`]. Linear-scan dedup — the table grows
    /// only as new tree-sitter token kinds appear, so it stays tiny.
    /// Behind the `ratatui` feature.
    ///
    /// Renamed from `intern_style` in 0.0.32 — at 0.1.0 freeze the
    /// unprefixed name belongs to the engine-native variant.
    #[cfg(feature = "ratatui")]
    pub fn intern_ratatui_style(&mut self, style: ratatui::style::Style) -> u32 {
        if let Some(idx) = self.style_table.iter().position(|s| *s == style) {
            return idx as u32;
        }
        self.style_table.push(style);
        (self.style_table.len() - 1) as u32
    }

    /// Read-only view of the style table — id `i` → `style_table[i]`.
    /// The render path passes a closure backed by this slice as the
    /// `StyleResolver` for `BufferView`. Behind the `ratatui` feature.
    #[cfg(feature = "ratatui")]
    pub fn style_table(&self) -> &[ratatui::style::Style] {
        &self.style_table
    }

    /// Per-row syntax span overlay, one `Vec<Span>` per buffer row.
    /// Hosts feed this slice into [`hjkl_buffer::BufferView::spans`]
    /// per draw frame.
    ///
    /// 0.0.37: replaces `editor.buffer().spans()` per step 3 of
    /// `DESIGN_33_METHOD_CLASSIFICATION.md`. The buffer no longer
    /// caches spans; they live on the engine and route through the
    /// `Host::syntax_highlights` pipeline.
    pub fn buffer_spans(&self) -> &[Vec<hjkl_buffer::Span>] {
        &self.buffer_spans
    }

    /// Intern a SPEC [`crate::types::Style`] and return its opaque id.
    /// With the `ratatui` feature on, the id matches the one
    /// [`Editor::intern_ratatui_style`] would return for the equivalent
    /// `ratatui::Style` (both share the underlying table). With it off,
    /// the engine keeps a parallel `crate::types::Style`-keyed table
    /// — ids are still stable per-editor.
    ///
    /// Hosts that don't depend on ratatui (buffr, future GUI shells)
    /// reach this method to populate the table during syntax span
    /// installation.
    ///
    /// Renamed from `intern_engine_style` in 0.0.32 — at 0.1.0 freeze
    /// the unprefixed name is the universally-available engine-native
    /// variant.
    pub fn intern_style(&mut self, style: crate::types::Style) -> u32 {
        #[cfg(feature = "ratatui")]
        {
            let r = engine_style_to_ratatui(style);
            self.intern_ratatui_style(r)
        }
        #[cfg(not(feature = "ratatui"))]
        {
            if let Some(idx) = self.engine_style_table.iter().position(|s| *s == style) {
                return idx as u32;
            }
            self.engine_style_table.push(style);
            (self.engine_style_table.len() - 1) as u32
        }
    }

    /// Look up an interned style by id and return it as a SPEC
    /// [`crate::types::Style`]. Returns `None` for ids past the end
    /// of the table.
    pub fn engine_style_at(&self, id: u32) -> Option<crate::types::Style> {
        #[cfg(feature = "ratatui")]
        {
            let r = self.style_table.get(id as usize).copied()?;
            Some(ratatui_style_to_engine(r))
        }
        #[cfg(not(feature = "ratatui"))]
        {
            self.engine_style_table.get(id as usize).copied()
        }
    }

    /// Historical reverse-sync hook from when the textarea mirrored
    /// the buffer. Now that Buffer is the cursor authority this is a
    /// no-op; call sites can remain in place during the migration.
    pub(crate) fn push_buffer_cursor_to_textarea(&mut self) {}

    /// Force the host viewport's top row without touching the
    /// cursor. Used by tests that simulate a scroll without the
    /// SCROLLOFF cursor adjustment that `scroll_down` / `scroll_up`
    /// apply.
    ///
    /// 0.0.34 (Patch C-δ.1): writes through `Host::viewport_mut`
    /// instead of the (now-deleted) `Buffer::viewport_mut`.
    pub fn set_viewport_top(&mut self, row: usize) {
        let last = buf_row_count(&self.buffer).saturating_sub(1);
        let target = row.min(last);
        self.host.viewport_mut().top_row = target;
    }

    /// Set the cursor to `(row, col)`, clamped to the buffer's
    /// content. Hosts use this for goto-line, jump-to-mark, and
    /// programmatic cursor placement.
    pub fn jump_cursor(&mut self, row: usize, col: usize) {
        buf_set_cursor_rc(&mut self.buffer, row, col);
    }

    /// `(row, col)` cursor read sourced from the migration buffer.
    /// Equivalent to `self.textarea.cursor()` when the two are in
    /// sync — which is the steady state during Phase 7f because
    /// every step opens with `sync_buffer_content_from_textarea` and
    /// every ported motion pushes the result back. Prefer this over
    /// `self.textarea.cursor()` so call sites keep working unchanged
    /// once the textarea field is ripped.
    pub fn cursor(&self) -> (usize, usize) {
        buf_cursor_rc(&self.buffer)
    }

    /// Drain any pending LSP intent raised by the last key. Returns
    /// `None` when no intent is armed.
    pub fn take_lsp_intent(&mut self) -> Option<LspIntent> {
        self.pending_lsp.take()
    }

    /// Drain every [`crate::types::FoldOp`] raised since the last
    /// call. Hosts that mirror the engine's fold storage (or that
    /// project folds onto a separate fold tree, LSP folding ranges,
    /// …) drain this each step and dispatch as their own
    /// [`crate::types::Host::Intent`] requires.
    ///
    /// The engine has already applied every op locally against the
    /// in-tree [`hjkl_buffer::Buffer`] fold storage via
    /// [`crate::buffer_impl::BufferFoldProviderMut`], so hosts that
    /// don't track folds independently can ignore the queue
    /// (or simply never call this drain).
    ///
    /// Introduced in 0.0.38 (Patch C-δ.4).
    pub fn take_fold_ops(&mut self) -> Vec<crate::types::FoldOp> {
        std::mem::take(&mut self.pending_fold_ops)
    }

    /// Dispatch a [`crate::types::FoldOp`] through the canonical fold
    /// surface: queue it for host observation (drained by
    /// [`Editor::take_fold_ops`]) and apply it locally against the
    /// in-tree buffer fold storage via
    /// [`crate::buffer_impl::BufferFoldProviderMut`]. Engine call sites
    /// (vim FSM `z…` chords, `:fold*` Ex commands, edit-pipeline
    /// invalidation) route every fold mutation through this method.
    ///
    /// Introduced in 0.0.38 (Patch C-δ.4).
    pub fn apply_fold_op(&mut self, op: crate::types::FoldOp) {
        use crate::types::FoldProvider;
        self.pending_fold_ops.push(op);
        let mut provider = crate::buffer_impl::BufferFoldProviderMut::new(&mut self.buffer);
        provider.apply(op);
    }

    /// Refresh the host viewport's height from the cached
    /// `viewport_height_value()`. Called from the per-step
    /// boilerplate; was the textarea → buffer mirror before Phase 7f
    /// put Buffer in charge. 0.0.28 hoisted sticky_col out of
    /// `Buffer`. 0.0.34 (Patch C-δ.1) routes the height write through
    /// `Host::viewport_mut`.
    pub(crate) fn sync_buffer_from_textarea(&mut self) {
        let height = self.viewport_height_value();
        self.host.viewport_mut().height = height;
    }

    /// Was the full textarea → buffer content sync. Buffer is the
    /// content authority now; this remains as a no-op so the per-step
    /// call sites don't have to be ripped in the same patch.
    pub(crate) fn sync_buffer_content_from_textarea(&mut self) {
        self.sync_buffer_from_textarea();
    }

    /// Push a `(row, col)` onto the back-jumplist so `Ctrl-o` returns
    /// to it later. Used by host-driven jumps (e.g. `gd`) that move
    /// the cursor without going through the vim engine's motion
    /// machinery, where push_jump fires automatically.
    pub fn record_jump(&mut self, pos: (usize, usize)) {
        const JUMPLIST_MAX: usize = 100;
        self.vim.jump_back.push(pos);
        if self.vim.jump_back.len() > JUMPLIST_MAX {
            self.vim.jump_back.remove(0);
        }
        self.vim.jump_fwd.clear();
    }

    /// Host apps call this each draw with the current text area height so
    /// scroll helpers can clamp the cursor without recomputing layout.
    pub fn set_viewport_height(&self, height: u16) {
        self.viewport_height.store(height, Ordering::Relaxed);
    }

    /// Last height published by `set_viewport_height` (in rows).
    pub fn viewport_height_value(&self) -> u16 {
        self.viewport_height.load(Ordering::Relaxed)
    }

    /// Apply `edit` against the buffer and return the inverse so the
    /// host can push it onto an undo stack. Side effects: dirty
    /// flag, change-list ring, mark / jump-list shifts, change_log
    /// append, fold invalidation around the touched rows.
    ///
    /// The primary edit funnel — both FSM operators and ex commands
    /// route mutations through here so the side effects fire
    /// uniformly.
    pub fn mutate_edit(&mut self, edit: hjkl_buffer::Edit) -> hjkl_buffer::Edit {
        // `:set readonly` short-circuits every mutation funnel: no
        // buffer change, no dirty flag, no undo entry, no change-log
        // emission. We swallow the requested `edit` and hand back a
        // self-inverse no-op (`InsertStr` of an empty string at the
        // current cursor) so callers that push the return value onto
        // an undo stack still get a structurally valid round trip.
        if self.settings.readonly {
            let _ = edit;
            return hjkl_buffer::Edit::InsertStr {
                at: buf_cursor_pos(&self.buffer),
                text: String::new(),
            };
        }
        let pre_row = buf_cursor_row(&self.buffer);
        let pre_rows = buf_row_count(&self.buffer);
        // Map the underlying buffer edit to a SPEC EditOp for
        // change-log emission before consuming it. Coarse — see
        // change_log field doc on the struct.
        self.change_log.extend(edit_to_editops(&edit));
        // Compute ContentEdit fan-out from the pre-edit buffer state.
        // Done before `apply_buffer_edit` consumes `edit` so we can
        // inspect the operation's fields and the buffer's pre-edit row
        // bytes (needed for byte_of_row / col_byte conversion). Edits
        // are pushed onto `pending_content_edits` for host drain.
        let content_edits = content_edits_from_buffer_edit(&self.buffer, &edit);
        self.pending_content_edits.extend(content_edits);
        // 0.0.42 (Patch C-δ.7): the `apply_edit` reach is centralized
        // in [`crate::buf_helpers::apply_buffer_edit`] (option (c) of
        // the 0.0.42 plan — see that fn's doc comment). The free fn
        // takes `&mut hjkl_buffer::Buffer` so the editor body itself
        // no longer carries a `self.buffer.<inherent>` hop.
        let inverse = apply_buffer_edit(&mut self.buffer, edit);
        let (pos_row, pos_col) = buf_cursor_rc(&self.buffer);
        // Drop any folds the edit's range overlapped — vim opens the
        // surrounding fold automatically when you edit inside it. The
        // approximation here invalidates folds covering either the
        // pre-edit cursor row or the post-edit cursor row, which
        // catches the common single-line / multi-line edit shapes.
        let lo = pre_row.min(pos_row);
        let hi = pre_row.max(pos_row);
        self.apply_fold_op(crate::types::FoldOp::Invalidate {
            start_row: lo,
            end_row: hi,
        });
        self.vim.last_edit_pos = Some((pos_row, pos_col));
        // Append to the change-list ring (skip when the cursor sits on
        // the same cell as the last entry — back-to-back keystrokes on
        // one column shouldn't pollute the ring). A new edit while
        // walking the ring trims the forward half, vim style.
        let entry = (pos_row, pos_col);
        if self.vim.change_list.last() != Some(&entry) {
            if let Some(idx) = self.vim.change_list_cursor.take() {
                self.vim.change_list.truncate(idx + 1);
            }
            self.vim.change_list.push(entry);
            let len = self.vim.change_list.len();
            if len > crate::vim::CHANGE_LIST_MAX {
                self.vim
                    .change_list
                    .drain(0..len - crate::vim::CHANGE_LIST_MAX);
            }
        }
        self.vim.change_list_cursor = None;
        // Shift / drop marks + jump-list entries to track the row
        // delta the edit produced. Without this, every line-changing
        // edit silently invalidates `'a`-style positions.
        let post_rows = buf_row_count(&self.buffer);
        let delta = post_rows as isize - pre_rows as isize;
        if delta != 0 {
            self.shift_marks_after_edit(pre_row, delta);
        }
        self.push_buffer_content_to_textarea();
        self.mark_content_dirty();
        inverse
    }

    /// Migrate user marks + jumplist entries when an edit at row
    /// `edit_start` changes the buffer's row count by `delta` (positive
    /// for inserts, negative for deletes). Marks tied to a deleted row
    /// are dropped; marks past the affected band shift by `delta`.
    fn shift_marks_after_edit(&mut self, edit_start: usize, delta: isize) {
        if delta == 0 {
            return;
        }
        // Deleted-row band (only meaningful for delta < 0). Inclusive
        // start, exclusive end.
        let drop_end = if delta < 0 {
            edit_start.saturating_add((-delta) as usize)
        } else {
            edit_start
        };
        let shift_threshold = drop_end.max(edit_start.saturating_add(1));

        // 0.0.36: lowercase + uppercase marks share the unified
        // `marks` map; one pass migrates both.
        let mut to_drop: Vec<char> = Vec::new();
        for (c, (row, _col)) in self.marks.iter_mut() {
            if (edit_start..drop_end).contains(row) {
                to_drop.push(*c);
            } else if *row >= shift_threshold {
                *row = ((*row as isize) + delta).max(0) as usize;
            }
        }
        for c in to_drop {
            self.marks.remove(&c);
        }

        let shift_jumps = |entries: &mut Vec<(usize, usize)>| {
            entries.retain(|(row, _)| !(edit_start..drop_end).contains(row));
            for (row, _) in entries.iter_mut() {
                if *row >= shift_threshold {
                    *row = ((*row as isize) + delta).max(0) as usize;
                }
            }
        };
        shift_jumps(&mut self.vim.jump_back);
        shift_jumps(&mut self.vim.jump_fwd);
    }

    /// Reverse-sync helper paired with [`Editor::mutate_edit`]: rebuild
    /// the textarea from the buffer's lines + cursor, preserving yank
    /// text. Heavy (allocates a fresh `TextArea`) but correct; the
    /// textarea field disappears at the end of Phase 7f anyway.
    /// No-op since Buffer is the content authority. Retained as a
    /// shim so call sites in `mutate_edit` and friends don't have to
    /// be ripped in lockstep with the field removal.
    pub(crate) fn push_buffer_content_to_textarea(&mut self) {}

    /// Single choke-point for "the buffer just changed". Sets the
    /// dirty flag and drops the cached `content_arc` snapshot so
    /// subsequent reads rebuild from the live textarea. Callers
    /// mutating `textarea` directly (e.g. the TUI's bracketed-paste
    /// path) must invoke this to keep the cache honest.
    pub fn mark_content_dirty(&mut self) {
        self.content_dirty = true;
        self.cached_content = None;
    }

    /// Returns true if content changed since the last call, then clears the flag.
    pub fn take_dirty(&mut self) -> bool {
        let dirty = self.content_dirty;
        self.content_dirty = false;
        dirty
    }

    /// Drain the queue of [`crate::types::ContentEdit`]s emitted since
    /// the last call. Each entry corresponds to a single buffer
    /// mutation funnelled through [`Editor::mutate_edit`]; block edits
    /// fan out to one entry per row touched.
    ///
    /// Hosts call this each frame (after [`Editor::take_content_reset`])
    /// to fan edits into a tree-sitter parser via `Tree::edit`.
    pub fn take_content_edits(&mut self) -> Vec<crate::types::ContentEdit> {
        std::mem::take(&mut self.pending_content_edits)
    }

    /// Returns `true` if a bulk buffer replacement happened since the
    /// last call (e.g. `set_content` / `restore` / undo restore), then
    /// clears the flag. When this returns `true`, hosts should drop
    /// any retained syntax tree before consuming
    /// [`Editor::take_content_edits`].
    pub fn take_content_reset(&mut self) -> bool {
        let r = self.pending_content_reset;
        self.pending_content_reset = false;
        r
    }

    /// Pull-model coarse change observation. If content changed since
    /// the last call, returns `Some(Arc<String>)` with the new content
    /// and clears the dirty flag; otherwise returns `None`.
    ///
    /// Hosts that need fine-grained edit deltas (e.g., DOM patching at
    /// the character level) should diff against their own previous
    /// snapshot. The SPEC `take_changes() -> Vec<EditOp>` API lands
    /// once every edit path inside the engine is instrumented; this
    /// coarse form covers the pull-model use case in the meantime.
    pub fn take_content_change(&mut self) -> Option<std::sync::Arc<String>> {
        if !self.content_dirty {
            return None;
        }
        let arc = self.content_arc();
        self.content_dirty = false;
        Some(arc)
    }

    /// Returns the cursor's row within the visible textarea (0-based), updating
    /// the stored viewport top so subsequent calls remain accurate.
    pub fn cursor_screen_row(&mut self, height: u16) -> u16 {
        let cursor = buf_cursor_row(&self.buffer);
        let top = self.host.viewport().top_row;
        cursor.saturating_sub(top).min(height as usize - 1) as u16
    }

    /// Returns the cursor's screen position `(x, y)` for the textarea
    /// described by `(area_x, area_y, area_width, area_height)`.
    /// Accounts for line-number gutter and viewport scroll. Returns
    /// `None` if the cursor is outside the visible viewport. Always
    /// available (engine-native; no ratatui dependency).
    ///
    /// Renamed from `cursor_screen_pos_xywh` in 0.0.32 — the
    /// ratatui-flavoured `Rect` variant is now
    /// [`Editor::cursor_screen_pos_in_rect`] (cfg `ratatui`).
    pub fn cursor_screen_pos(
        &self,
        area_x: u16,
        area_y: u16,
        area_width: u16,
        area_height: u16,
    ) -> Option<(u16, u16)> {
        let (pos_row, pos_col) = buf_cursor_rc(&self.buffer);
        let v = self.host.viewport();
        if pos_row < v.top_row || pos_col < v.top_col {
            return None;
        }
        let lnum_width = buf_row_count(&self.buffer).to_string().len() as u16 + 2;
        let dy = (pos_row - v.top_row) as u16;
        let dx = (pos_col - v.top_col) as u16;
        if dy >= area_height || dx + lnum_width >= area_width {
            return None;
        }
        Some((area_x + lnum_width + dx, area_y + dy))
    }

    /// Ratatui [`Rect`]-flavoured wrapper around
    /// [`Editor::cursor_screen_pos`]. Behind the `ratatui` feature.
    ///
    /// Renamed from `cursor_screen_pos` in 0.0.32 — the unprefixed
    /// name now belongs to the engine-native variant.
    #[cfg(feature = "ratatui")]
    pub fn cursor_screen_pos_in_rect(&self, area: Rect) -> Option<(u16, u16)> {
        self.cursor_screen_pos(area.x, area.y, area.width, area.height)
    }

    pub fn vim_mode(&self) -> VimMode {
        self.vim.public_mode()
    }

    /// Bounds of the active visual-block rectangle as
    /// `(top_row, bot_row, left_col, right_col)` — all inclusive.
    /// `None` when we're not in VisualBlock mode.
    /// Read-only view of the live `/` or `?` prompt. `None` outside
    /// search-prompt mode.
    pub fn search_prompt(&self) -> Option<&crate::vim::SearchPrompt> {
        self.vim.search_prompt.as_ref()
    }

    /// Most recent committed search pattern (persists across `n` / `N`
    /// and across prompt exits). `None` before the first search.
    pub fn last_search(&self) -> Option<&str> {
        self.vim.last_search.as_deref()
    }

    /// Whether the last committed search was a forward `/` (`true`) or
    /// a backward `?` (`false`). `n` and `N` consult this to honour the
    /// direction the user committed.
    pub fn last_search_forward(&self) -> bool {
        self.vim.last_search_forward
    }

    /// Set the most recent committed search text + direction. Used by
    /// host-driven prompts (e.g. apps/hjkl's `/` `?` prompt that lives
    /// outside the engine's vim FSM) so `n` / `N` repeat the host's
    /// most recent commit with the right direction. Pass `None` /
    /// `true` to clear.
    pub fn set_last_search(&mut self, text: Option<String>, forward: bool) {
        self.vim.last_search = text;
        self.vim.last_search_forward = forward;
    }

    /// Start/end `(row, col)` of the active char-wise Visual selection
    /// (inclusive on both ends, positionally ordered). `None` when not
    /// in Visual mode.
    pub fn char_highlight(&self) -> Option<((usize, usize), (usize, usize))> {
        if self.vim_mode() != VimMode::Visual {
            return None;
        }
        let anchor = self.vim.visual_anchor;
        let cursor = self.cursor();
        let (start, end) = if anchor <= cursor {
            (anchor, cursor)
        } else {
            (cursor, anchor)
        };
        Some((start, end))
    }

    /// Top/bottom rows of the active VisualLine selection (inclusive).
    /// `None` when we're not in VisualLine mode.
    pub fn line_highlight(&self) -> Option<(usize, usize)> {
        if self.vim_mode() != VimMode::VisualLine {
            return None;
        }
        let anchor = self.vim.visual_line_anchor;
        let cursor = buf_cursor_row(&self.buffer);
        Some((anchor.min(cursor), anchor.max(cursor)))
    }

    pub fn block_highlight(&self) -> Option<(usize, usize, usize, usize)> {
        if self.vim_mode() != VimMode::VisualBlock {
            return None;
        }
        let (ar, ac) = self.vim.block_anchor;
        let cr = buf_cursor_row(&self.buffer);
        let cc = self.vim.block_vcol;
        let top = ar.min(cr);
        let bot = ar.max(cr);
        let left = ac.min(cc);
        let right = ac.max(cc);
        Some((top, bot, left, right))
    }

    /// Active selection in `hjkl_buffer::Selection` shape. `None` when
    /// not in a Visual mode. Phase 7d-i wiring — the host hands this
    /// straight to `BufferView` once render flips off textarea
    /// (Phase 7d-ii drops the `paint_*_overlay` calls on the same
    /// switch).
    pub fn buffer_selection(&self) -> Option<hjkl_buffer::Selection> {
        use hjkl_buffer::{Position, Selection};
        match self.vim_mode() {
            VimMode::Visual => {
                let (ar, ac) = self.vim.visual_anchor;
                let head = buf_cursor_pos(&self.buffer);
                Some(Selection::Char {
                    anchor: Position::new(ar, ac),
                    head,
                })
            }
            VimMode::VisualLine => {
                let anchor_row = self.vim.visual_line_anchor;
                let head_row = buf_cursor_row(&self.buffer);
                Some(Selection::Line {
                    anchor_row,
                    head_row,
                })
            }
            VimMode::VisualBlock => {
                let (ar, ac) = self.vim.block_anchor;
                let cr = buf_cursor_row(&self.buffer);
                let cc = self.vim.block_vcol;
                Some(Selection::Block {
                    anchor: Position::new(ar, ac),
                    head: Position::new(cr, cc),
                })
            }
            _ => None,
        }
    }

    /// Force back to normal mode (used when dismissing completions etc.)
    pub fn force_normal(&mut self) {
        self.vim.force_normal();
    }

    pub fn content(&self) -> String {
        let n = buf_row_count(&self.buffer);
        let mut s = String::new();
        for r in 0..n {
            if r > 0 {
                s.push('\n');
            }
            s.push_str(crate::types::Query::line(&self.buffer, r as u32));
        }
        s.push('\n');
        s
    }

    /// Same logical output as [`content`], but returns a cached
    /// `Arc<String>` so back-to-back reads within an un-mutated window
    /// are ref-count bumps instead of multi-MB joins. The cache is
    /// invalidated by every [`mark_content_dirty`] call.
    pub fn content_arc(&mut self) -> std::sync::Arc<String> {
        if let Some(arc) = &self.cached_content {
            return std::sync::Arc::clone(arc);
        }
        let arc = std::sync::Arc::new(self.content());
        self.cached_content = Some(std::sync::Arc::clone(&arc));
        arc
    }

    pub fn set_content(&mut self, text: &str) {
        let mut lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
        while lines.last().map(|l| l.is_empty()).unwrap_or(false) {
            lines.pop();
        }
        if lines.is_empty() {
            lines.push(String::new());
        }
        let _ = lines;
        crate::types::BufferEdit::replace_all(&mut self.buffer, text);
        self.undo_stack.clear();
        self.redo_stack.clear();
        // Whole-buffer replace supersedes any queued ContentEdits.
        self.pending_content_edits.clear();
        self.pending_content_reset = true;
        self.mark_content_dirty();
    }

    /// Feed an SPEC [`crate::PlannedInput`] into the engine.
    ///
    /// Bridge for hosts that don't carry crossterm — buffr's CEF
    /// shell, future GUI frontends. Converts directly to the engine's
    /// internal [`Input`] type and dispatches through the vim FSM,
    /// bypassing crossterm entirely so this entry point is always
    /// available regardless of the `crossterm` feature.
    ///
    /// `Input::Mouse`, `Input::Paste`, `Input::FocusGained`,
    /// `Input::FocusLost`, and `Input::Resize` currently fall through
    /// without effect — the legacy FSM doesn't dispatch them. They're
    /// accepted so the host can pump them into the engine without
    /// special-casing.
    ///
    /// Returns `true` when the keystroke was consumed.
    pub fn feed_input(&mut self, input: crate::PlannedInput) -> bool {
        use crate::{PlannedInput, SpecialKey};
        let (key, mods) = match input {
            PlannedInput::Char(c, m) => (Key::Char(c), m),
            PlannedInput::Key(k, m) => {
                let key = match k {
                    SpecialKey::Esc => Key::Esc,
                    SpecialKey::Enter => Key::Enter,
                    SpecialKey::Backspace => Key::Backspace,
                    SpecialKey::Tab => Key::Tab,
                    // Engine's internal `Key` doesn't model BackTab as a
                    // distinct variant — fall through to the FSM as
                    // shift+Tab, matching crossterm semantics.
                    SpecialKey::BackTab => Key::Tab,
                    SpecialKey::Up => Key::Up,
                    SpecialKey::Down => Key::Down,
                    SpecialKey::Left => Key::Left,
                    SpecialKey::Right => Key::Right,
                    SpecialKey::Home => Key::Home,
                    SpecialKey::End => Key::End,
                    SpecialKey::PageUp => Key::PageUp,
                    SpecialKey::PageDown => Key::PageDown,
                    // Engine's `Key` has no Insert / F(n) — drop to Null
                    // (FSM ignores it) which matches the crossterm path
                    // (`crossterm_to_input` mapped these to Null too).
                    SpecialKey::Insert => Key::Null,
                    SpecialKey::Delete => Key::Delete,
                    SpecialKey::F(_) => Key::Null,
                };
                let m = if matches!(k, SpecialKey::BackTab) {
                    crate::Modifiers { shift: true, ..m }
                } else {
                    m
                };
                (key, m)
            }
            // Variants the legacy FSM doesn't consume yet.
            PlannedInput::Mouse(_)
            | PlannedInput::Paste(_)
            | PlannedInput::FocusGained
            | PlannedInput::FocusLost
            | PlannedInput::Resize(_, _) => return false,
        };
        if key == Key::Null {
            return false;
        }
        let event = Input {
            key,
            ctrl: mods.ctrl,
            alt: mods.alt,
            shift: mods.shift,
        };
        let consumed = vim::step(self, event);
        self.emit_cursor_shape_if_changed();
        consumed
    }

    /// Drain the pending change log produced by buffer mutations.
    ///
    /// Returns a `Vec<EditOp>` covering edits applied since the last
    /// call. Empty when no edits ran. Pull-model, complementary to
    /// [`Editor::take_content_change`] which gives back the new full
    /// content.
    ///
    /// Mapping coverage:
    /// - InsertChar / InsertStr → exact `EditOp` with empty range +
    ///   replacement.
    /// - DeleteRange (`Char` kind) → exact range + empty replacement.
    /// - Replace → exact range + new replacement.
    /// - DeleteRange (`Line`/`Block`), JoinLines, SplitLines,
    ///   InsertBlock, DeleteBlockChunks → best-effort placeholder
    ///   covering the touched range. Hosts wanting per-cell deltas
    ///   should diff their own `lines()` snapshot.
    pub fn take_changes(&mut self) -> Vec<crate::types::Edit> {
        std::mem::take(&mut self.change_log)
    }

    /// Read the engine's current settings as a SPEC
    /// [`crate::types::Options`].
    ///
    /// Bridges between the legacy [`Settings`] (which carries fewer
    /// fields than SPEC) and the planned 0.1.0 trait surface. Fields
    /// not present in `Settings` fall back to vim defaults (e.g.,
    /// `expandtab=false`, `wrapscan=true`, `timeout_len=1000ms`).
    /// Once trait extraction lands, this becomes the canonical config
    /// reader and `Settings` retires.
    pub fn current_options(&self) -> crate::types::Options {
        crate::types::Options {
            shiftwidth: self.settings.shiftwidth as u32,
            tabstop: self.settings.tabstop as u32,
            textwidth: self.settings.textwidth as u32,
            expandtab: self.settings.expandtab,
            ignorecase: self.settings.ignore_case,
            smartcase: self.settings.smartcase,
            wrapscan: self.settings.wrapscan,
            wrap: match self.settings.wrap {
                hjkl_buffer::Wrap::None => crate::types::WrapMode::None,
                hjkl_buffer::Wrap::Char => crate::types::WrapMode::Char,
                hjkl_buffer::Wrap::Word => crate::types::WrapMode::Word,
            },
            readonly: self.settings.readonly,
            autoindent: self.settings.autoindent,
            undo_levels: self.settings.undo_levels,
            undo_break_on_motion: self.settings.undo_break_on_motion,
            iskeyword: self.settings.iskeyword.clone(),
            timeout_len: self.settings.timeout_len,
            ..crate::types::Options::default()
        }
    }

    /// Apply a SPEC [`crate::types::Options`] to the engine's settings.
    /// Only the fields backed by today's [`Settings`] take effect;
    /// remaining options become live once trait extraction wires them
    /// through.
    pub fn apply_options(&mut self, opts: &crate::types::Options) {
        self.settings.shiftwidth = opts.shiftwidth as usize;
        self.settings.tabstop = opts.tabstop as usize;
        self.settings.textwidth = opts.textwidth as usize;
        self.settings.expandtab = opts.expandtab;
        self.settings.ignore_case = opts.ignorecase;
        self.settings.smartcase = opts.smartcase;
        self.settings.wrapscan = opts.wrapscan;
        self.settings.wrap = match opts.wrap {
            crate::types::WrapMode::None => hjkl_buffer::Wrap::None,
            crate::types::WrapMode::Char => hjkl_buffer::Wrap::Char,
            crate::types::WrapMode::Word => hjkl_buffer::Wrap::Word,
        };
        self.settings.readonly = opts.readonly;
        self.settings.autoindent = opts.autoindent;
        self.settings.undo_levels = opts.undo_levels;
        self.settings.undo_break_on_motion = opts.undo_break_on_motion;
        self.set_iskeyword(opts.iskeyword.clone());
        self.settings.timeout_len = opts.timeout_len;
    }

    /// Active visual selection as a SPEC [`crate::types::Highlight`]
    /// with [`crate::types::HighlightKind::Selection`].
    ///
    /// Returns `None` when the editor isn't in a Visual mode.
    /// Visual-line and visual-block selections collapse to the
    /// bounding char range of the selection — the SPEC `Selection`
    /// kind doesn't carry sub-line info today; hosts that need full
    /// line / block geometry continue to read [`buffer_selection`]
    /// (the legacy [`hjkl_buffer::Selection`] shape).
    pub fn selection_highlight(&self) -> Option<crate::types::Highlight> {
        use crate::types::{Highlight, HighlightKind, Pos};
        let sel = self.buffer_selection()?;
        let (start, end) = match sel {
            hjkl_buffer::Selection::Char { anchor, head } => {
                let a = (anchor.row, anchor.col);
                let h = (head.row, head.col);
                if a <= h { (a, h) } else { (h, a) }
            }
            hjkl_buffer::Selection::Line {
                anchor_row,
                head_row,
            } => {
                let (top, bot) = if anchor_row <= head_row {
                    (anchor_row, head_row)
                } else {
                    (head_row, anchor_row)
                };
                let last_col = buf_line(&self.buffer, bot).map(|l| l.len()).unwrap_or(0);
                ((top, 0), (bot, last_col))
            }
            hjkl_buffer::Selection::Block { anchor, head } => {
                let (top, bot) = if anchor.row <= head.row {
                    (anchor.row, head.row)
                } else {
                    (head.row, anchor.row)
                };
                let (left, right) = if anchor.col <= head.col {
                    (anchor.col, head.col)
                } else {
                    (head.col, anchor.col)
                };
                ((top, left), (bot, right))
            }
        };
        Some(Highlight {
            range: Pos {
                line: start.0 as u32,
                col: start.1 as u32,
            }..Pos {
                line: end.0 as u32,
                col: end.1 as u32,
            },
            kind: HighlightKind::Selection,
        })
    }

    /// SPEC-typed highlights for `line`.
    ///
    /// Two emission modes:
    ///
    /// - **IncSearch**: the user is typing a `/` or `?` prompt and
    ///   `Editor::search_prompt` is `Some`. Live-preview matches of
    ///   the in-flight pattern surface as
    ///   [`crate::types::HighlightKind::IncSearch`].
    /// - **SearchMatch**: the prompt has been committed (or absent)
    ///   and the buffer's armed pattern is non-empty. Matches surface
    ///   as [`crate::types::HighlightKind::SearchMatch`].
    ///
    /// Selection / MatchParen / Syntax(id) variants land once the
    /// trait extraction routes the FSM's selection set + the host's
    /// syntax pipeline through the [`crate::types::Host`] trait.
    ///
    /// Returns an empty vec when there is nothing to highlight or
    /// `line` is out of bounds.
    pub fn highlights_for_line(&mut self, line: u32) -> Vec<crate::types::Highlight> {
        use crate::types::{Highlight, HighlightKind, Pos};
        let row = line as usize;
        if row >= buf_row_count(&self.buffer) {
            return Vec::new();
        }

        // Live preview while the prompt is open beats the committed
        // pattern.
        if let Some(prompt) = self.search_prompt() {
            if prompt.text.is_empty() {
                return Vec::new();
            }
            let Ok(re) = regex::Regex::new(&prompt.text) else {
                return Vec::new();
            };
            let Some(haystack) = buf_line(&self.buffer, row) else {
                return Vec::new();
            };
            return re
                .find_iter(haystack)
                .map(|m| Highlight {
                    range: Pos {
                        line,
                        col: m.start() as u32,
                    }..Pos {
                        line,
                        col: m.end() as u32,
                    },
                    kind: HighlightKind::IncSearch,
                })
                .collect();
        }

        if self.search_state.pattern.is_none() {
            return Vec::new();
        }
        let dgen = crate::types::Query::dirty_gen(&self.buffer);
        crate::search::search_matches(&self.buffer, &mut self.search_state, dgen, row)
            .into_iter()
            .map(|(start, end)| Highlight {
                range: Pos {
                    line,
                    col: start as u32,
                }..Pos {
                    line,
                    col: end as u32,
                },
                kind: HighlightKind::SearchMatch,
            })
            .collect()
    }

    /// Build the engine's [`crate::types::RenderFrame`] for the
    /// current state. Hosts call this once per redraw and diff
    /// across frames.
    ///
    /// Coarse today — covers mode + cursor + cursor shape + viewport
    /// top + line count. SPEC-target fields (selections, highlights,
    /// command line, search prompt, status line) land once trait
    /// extraction routes them through `SelectionSet` and the
    /// `Highlight` pipeline.
    pub fn render_frame(&self) -> crate::types::RenderFrame {
        use crate::types::{CursorShape, RenderFrame, SnapshotMode};
        let (cursor_row, cursor_col) = self.cursor();
        let (mode, shape) = match self.vim_mode() {
            crate::VimMode::Normal => (SnapshotMode::Normal, CursorShape::Block),
            crate::VimMode::Insert => (SnapshotMode::Insert, CursorShape::Bar),
            crate::VimMode::Visual => (SnapshotMode::Visual, CursorShape::Block),
            crate::VimMode::VisualLine => (SnapshotMode::VisualLine, CursorShape::Block),
            crate::VimMode::VisualBlock => (SnapshotMode::VisualBlock, CursorShape::Block),
        };
        RenderFrame {
            mode,
            cursor_row: cursor_row as u32,
            cursor_col: cursor_col as u32,
            cursor_shape: shape,
            viewport_top: self.host.viewport().top_row as u32,
            line_count: crate::types::Query::line_count(&self.buffer),
        }
    }

    /// Capture the editor's coarse state into a serde-friendly
    /// [`crate::types::EditorSnapshot`].
    ///
    /// Today's snapshot covers mode, cursor, lines, viewport top.
    /// Registers, marks, jump list, undo tree, and full options arrive
    /// once phase 5 trait extraction lands the generic
    /// `Editor<B: Buffer, H: Host>` constructor — this method's surface
    /// stays stable; only the snapshot's internal fields grow.
    ///
    /// Distinct from the internal `snapshot` used by undo (which
    /// returns `(Vec<String>, (usize, usize))`); host-facing
    /// persistence goes through this one.
    pub fn take_snapshot(&self) -> crate::types::EditorSnapshot {
        use crate::types::{EditorSnapshot, SnapshotMode};
        let mode = match self.vim_mode() {
            crate::VimMode::Normal => SnapshotMode::Normal,
            crate::VimMode::Insert => SnapshotMode::Insert,
            crate::VimMode::Visual => SnapshotMode::Visual,
            crate::VimMode::VisualLine => SnapshotMode::VisualLine,
            crate::VimMode::VisualBlock => SnapshotMode::VisualBlock,
        };
        let cursor = self.cursor();
        let cursor = (cursor.0 as u32, cursor.1 as u32);
        let lines: Vec<String> = buf_lines_to_vec(&self.buffer);
        let viewport_top = self.host.viewport().top_row as u32;
        let marks = self
            .marks
            .iter()
            .map(|(c, (r, col))| (*c, (*r as u32, *col as u32)))
            .collect();
        EditorSnapshot {
            version: EditorSnapshot::VERSION,
            mode,
            cursor,
            lines,
            viewport_top,
            registers: self.registers.clone(),
            marks,
        }
    }

    /// Restore editor state from an [`EditorSnapshot`]. Returns
    /// [`crate::EngineError::SnapshotVersion`] if the snapshot's
    /// `version` doesn't match [`EditorSnapshot::VERSION`].
    ///
    /// Mode is best-effort: `SnapshotMode` only round-trips the
    /// status-line summary, not the full FSM state. Visual / Insert
    /// mode entry happens through synthetic key dispatch when needed.
    pub fn restore_snapshot(
        &mut self,
        snap: crate::types::EditorSnapshot,
    ) -> Result<(), crate::EngineError> {
        use crate::types::EditorSnapshot;
        if snap.version != EditorSnapshot::VERSION {
            return Err(crate::EngineError::SnapshotVersion(
                snap.version,
                EditorSnapshot::VERSION,
            ));
        }
        let text = snap.lines.join("\n");
        self.set_content(&text);
        self.jump_cursor(snap.cursor.0 as usize, snap.cursor.1 as usize);
        self.host.viewport_mut().top_row = snap.viewport_top as usize;
        self.registers = snap.registers;
        self.marks = snap
            .marks
            .into_iter()
            .map(|(c, (r, col))| (c, (r as usize, col as usize)))
            .collect();
        Ok(())
    }

    /// Install `text` as the pending yank buffer so the next `p`/`P` pastes
    /// it. Linewise is inferred from a trailing newline, matching how `yy`/`dd`
    /// shape their payload.
    pub fn seed_yank(&mut self, text: String) {
        let linewise = text.ends_with('\n');
        self.vim.yank_linewise = linewise;
        self.registers.unnamed = crate::registers::Slot { text, linewise };
    }

    /// Scroll the viewport down by `rows`. The cursor stays on its
    /// absolute line (vim convention) unless the scroll would take it
    /// off-screen — in that case it's clamped to the first row still
    /// visible.
    pub fn scroll_down(&mut self, rows: i16) {
        self.scroll_viewport(rows);
    }

    /// Scroll the viewport up by `rows`. Cursor stays unless it would
    /// fall off the bottom of the new viewport, then clamp to the
    /// bottom-most visible row.
    pub fn scroll_up(&mut self, rows: i16) {
        self.scroll_viewport(-rows);
    }

    /// Vim's `scrolloff` default — keep the cursor at least this many
    /// rows away from the top / bottom edge of the viewport while
    /// scrolling. Collapses to `height / 2` for tiny viewports.
    const SCROLLOFF: usize = 5;

    /// Scroll the viewport so the cursor stays at least `SCROLLOFF`
    /// rows from each edge. Replaces the bare
    /// `Buffer::ensure_cursor_visible` call at end-of-step so motions
    /// don't park the cursor on the very last visible row.
    pub(crate) fn ensure_cursor_in_scrolloff(&mut self) {
        let height = self.viewport_height.load(Ordering::Relaxed) as usize;
        if height == 0 {
            // 0.0.42 (Patch C-δ.7): viewport math lifted onto engine
            // free fns over `B: Query [+ Cursor]` + `&dyn FoldProvider`.
            // Disjoint-field borrow split: `self.buffer` (immutable via
            // `folds` snapshot + cursor) and `self.host` (mutable
            // viewport ref) live on distinct struct fields, so one
            // statement satisfies the borrow checker.
            let folds = crate::buffer_impl::BufferFoldProvider::new(&self.buffer);
            crate::viewport_math::ensure_cursor_visible(
                &self.buffer,
                &folds,
                self.host.viewport_mut(),
            );
            return;
        }
        // Cap margin at (height - 1) / 2 so the upper + lower bands
        // can't overlap on tiny windows (margin=5 + height=10 would
        // otherwise produce contradictory clamp ranges).
        let margin = Self::SCROLLOFF.min(height.saturating_sub(1) / 2);
        // Soft-wrap path: scrolloff math runs in *screen rows*, not
        // doc rows, since a wrapped doc row spans many visual lines.
        if !matches!(self.host.viewport().wrap, hjkl_buffer::Wrap::None) {
            self.ensure_scrolloff_wrap(height, margin);
            return;
        }
        let cursor_row = buf_cursor_row(&self.buffer);
        let last_row = buf_row_count(&self.buffer).saturating_sub(1);
        let v = self.host.viewport_mut();
        // Top edge: cursor_row should sit at >= top_row + margin.
        if cursor_row < v.top_row + margin {
            v.top_row = cursor_row.saturating_sub(margin);
        }
        // Bottom edge: cursor_row should sit at <= top_row + height - 1 - margin.
        let max_bottom = height.saturating_sub(1).saturating_sub(margin);
        if cursor_row > v.top_row + max_bottom {
            v.top_row = cursor_row.saturating_sub(max_bottom);
        }
        // Clamp top_row so we never scroll past the buffer's bottom.
        let max_top = last_row.saturating_sub(height.saturating_sub(1));
        if v.top_row > max_top {
            v.top_row = max_top;
        }
        // Defer to Buffer for column-side scroll (no scrolloff for
        // horizontal scrolling — vim default `sidescrolloff = 0`).
        let cursor = buf_cursor_pos(&self.buffer);
        self.host.viewport_mut().ensure_visible(cursor);
    }

    /// Soft-wrap-aware scrolloff. Walks `top_row` one visible doc row
    /// at a time so the cursor's *screen* row stays inside
    /// `[margin, height - 1 - margin]`, then clamps `top_row` so the
    /// buffer's bottom never leaves blank rows below it.
    fn ensure_scrolloff_wrap(&mut self, height: usize, margin: usize) {
        let cursor_row = buf_cursor_row(&self.buffer);
        // Step 1 — cursor above viewport: snap top to cursor row,
        // then we'll fix up the margin below.
        if cursor_row < self.host.viewport().top_row {
            let v = self.host.viewport_mut();
            v.top_row = cursor_row;
            v.top_col = 0;
        }
        // Step 2 — push top forward until cursor's screen row is
        // within the bottom margin (`csr <= height - 1 - margin`).
        // 0.0.33 (Patch C-γ): fold-iteration goes through the
        // [`crate::types::FoldProvider`] surface via
        // [`crate::buffer_impl::BufferFoldProvider`]. 0.0.34 (Patch
        // C-δ.1): `cursor_screen_row` / `max_top_for_height` now take
        // a `&Viewport` parameter; the host owns the viewport, so the
        // disjoint `(self.host, self.buffer)` borrows split cleanly.
        let max_csr = height.saturating_sub(1).saturating_sub(margin);
        loop {
            let folds = crate::buffer_impl::BufferFoldProvider::new(&self.buffer);
            let csr =
                crate::viewport_math::cursor_screen_row(&self.buffer, &folds, self.host.viewport())
                    .unwrap_or(0);
            if csr <= max_csr {
                break;
            }
            let top = self.host.viewport().top_row;
            let row_count = buf_row_count(&self.buffer);
            let next = {
                let folds = crate::buffer_impl::BufferFoldProvider::new(&self.buffer);
                <crate::buffer_impl::BufferFoldProvider<'_> as crate::types::FoldProvider>::next_visible_row(&folds, top, row_count)
            };
            let Some(next) = next else {
                break;
            };
            // Don't walk past the cursor's row.
            if next > cursor_row {
                self.host.viewport_mut().top_row = cursor_row;
                break;
            }
            self.host.viewport_mut().top_row = next;
        }
        // Step 3 — pull top backward until cursor's screen row is
        // past the top margin (`csr >= margin`).
        loop {
            let folds = crate::buffer_impl::BufferFoldProvider::new(&self.buffer);
            let csr =
                crate::viewport_math::cursor_screen_row(&self.buffer, &folds, self.host.viewport())
                    .unwrap_or(0);
            if csr >= margin {
                break;
            }
            let top = self.host.viewport().top_row;
            let prev = {
                let folds = crate::buffer_impl::BufferFoldProvider::new(&self.buffer);
                <crate::buffer_impl::BufferFoldProvider<'_> as crate::types::FoldProvider>::prev_visible_row(&folds, top)
            };
            let Some(prev) = prev else {
                break;
            };
            self.host.viewport_mut().top_row = prev;
        }
        // Step 4 — clamp top so the buffer's bottom doesn't leave
        // blank rows below it. `max_top_for_height` walks segments
        // backward from the last row until it accumulates `height`
        // screen rows.
        let max_top = {
            let folds = crate::buffer_impl::BufferFoldProvider::new(&self.buffer);
            crate::viewport_math::max_top_for_height(
                &self.buffer,
                &folds,
                self.host.viewport(),
                height,
            )
        };
        if self.host.viewport().top_row > max_top {
            self.host.viewport_mut().top_row = max_top;
        }
        self.host.viewport_mut().top_col = 0;
    }

    fn scroll_viewport(&mut self, delta: i16) {
        if delta == 0 {
            return;
        }
        // Bump the host viewport's top within bounds.
        let total_rows = buf_row_count(&self.buffer) as isize;
        let height = self.viewport_height.load(Ordering::Relaxed) as usize;
        let cur_top = self.host.viewport().top_row as isize;
        let new_top = (cur_top + delta as isize)
            .max(0)
            .min((total_rows - 1).max(0)) as usize;
        self.host.viewport_mut().top_row = new_top;
        // Mirror to textarea so its viewport reads (still consumed by
        // a couple of helpers) stay accurate.
        let _ = cur_top;
        if height == 0 {
            return;
        }
        // Apply scrolloff: keep the cursor at least SCROLLOFF rows
        // from the visible viewport edges.
        let (cursor_row, cursor_col) = buf_cursor_rc(&self.buffer);
        let margin = Self::SCROLLOFF.min(height / 2);
        let min_row = new_top + margin;
        let max_row = new_top + height.saturating_sub(1).saturating_sub(margin);
        let target_row = cursor_row.clamp(min_row, max_row.max(min_row));
        if target_row != cursor_row {
            let line_len = buf_line(&self.buffer, target_row)
                .map(|l| l.chars().count())
                .unwrap_or(0);
            let target_col = cursor_col.min(line_len.saturating_sub(1));
            buf_set_cursor_rc(&mut self.buffer, target_row, target_col);
        }
    }

    pub fn goto_line(&mut self, line: usize) {
        let row = line.saturating_sub(1);
        let max = buf_row_count(&self.buffer).saturating_sub(1);
        let target = row.min(max);
        buf_set_cursor_rc(&mut self.buffer, target, 0);
    }

    /// Scroll so the cursor row lands at the given viewport position:
    /// `Center` → middle row, `Top` → first row, `Bottom` → last row.
    /// Cursor stays on its absolute line; only the viewport moves.
    pub(super) fn scroll_cursor_to(&mut self, pos: CursorScrollTarget) {
        let height = self.viewport_height.load(Ordering::Relaxed) as usize;
        if height == 0 {
            return;
        }
        let cur_row = buf_cursor_row(&self.buffer);
        let cur_top = self.host.viewport().top_row;
        // Scrolloff awareness: `zt` lands the cursor at the top edge
        // of the viable area (top + margin), `zb` at the bottom edge
        // (top + height - 1 - margin). Match the cap used by
        // `ensure_cursor_in_scrolloff` so contradictory bounds are
        // impossible on tiny viewports.
        let margin = Self::SCROLLOFF.min(height.saturating_sub(1) / 2);
        let new_top = match pos {
            CursorScrollTarget::Center => cur_row.saturating_sub(height / 2),
            CursorScrollTarget::Top => cur_row.saturating_sub(margin),
            CursorScrollTarget::Bottom => {
                cur_row.saturating_sub(height.saturating_sub(1).saturating_sub(margin))
            }
        };
        if new_top == cur_top {
            return;
        }
        self.host.viewport_mut().top_row = new_top;
    }

    /// Translate a terminal mouse position into a (row, col) inside
    /// the document. The outer editor area is described by `(area_x,
    /// area_y, area_width)` (height is unused). 1-row tab bar at the
    /// top, then the textarea with 1 cell of horizontal pane padding
    /// on each side. Clicks past the line's last character clamp to
    /// the last char (Normal-mode invariant) — never past it.
    /// Char-counted, not byte-counted.
    ///
    /// Ratatui-free; [`Editor::mouse_to_doc_pos`] (behind the
    /// `ratatui` feature) is a thin `Rect`-flavoured wrapper.
    fn mouse_to_doc_pos_xy(&self, area_x: u16, area_y: u16, col: u16, row: u16) -> (usize, usize) {
        let n = buf_row_count(&self.buffer);
        let inner_top = area_y.saturating_add(1); // tab bar row
        let lnum_width = n.to_string().len() as u16 + 2;
        let content_x = area_x.saturating_add(1).saturating_add(lnum_width);
        let rel_row = row.saturating_sub(inner_top) as usize;
        let top = self.host.viewport().top_row;
        let doc_row = (top + rel_row).min(n.saturating_sub(1));
        let rel_col = col.saturating_sub(content_x) as usize;
        let line_chars = buf_line(&self.buffer, doc_row)
            .map(|l| l.chars().count())
            .unwrap_or(0);
        let last_col = line_chars.saturating_sub(1);
        (doc_row, rel_col.min(last_col))
    }

    /// Jump the cursor to the given 1-based line/column, clamped to the document.
    pub fn jump_to(&mut self, line: usize, col: usize) {
        let r = line.saturating_sub(1);
        let max_row = buf_row_count(&self.buffer).saturating_sub(1);
        let r = r.min(max_row);
        let line_len = buf_line(&self.buffer, r)
            .map(|l| l.chars().count())
            .unwrap_or(0);
        let c = col.saturating_sub(1).min(line_len);
        buf_set_cursor_rc(&mut self.buffer, r, c);
    }

    /// Jump cursor to the terminal-space mouse position; exits Visual
    /// modes if active. Engine-native coordinate flavour — pass the
    /// outer editor rect's `(x, y)` plus the click `(col, row)`.
    /// Always available (no ratatui dependency).
    ///
    /// Renamed from `mouse_click_xy` in 0.0.32 — at 0.1.0 freeze the
    /// unprefixed name belongs to the universally-available variant.
    pub fn mouse_click(&mut self, area_x: u16, area_y: u16, col: u16, row: u16) {
        if self.vim.is_visual() {
            self.vim.force_normal();
        }
        // Mouse-position click counts as a motion — break the active
        // insert-mode undo group when the toggle is on (vim parity).
        crate::vim::break_undo_group_in_insert(self);
        let (r, c) = self.mouse_to_doc_pos_xy(area_x, area_y, col, row);
        buf_set_cursor_rc(&mut self.buffer, r, c);
    }

    /// Ratatui [`Rect`]-flavoured wrapper around
    /// [`Editor::mouse_click`]. Behind the `ratatui` feature.
    ///
    /// Renamed from `mouse_click` in 0.0.32 — the unprefixed name now
    /// belongs to the engine-native variant.
    #[cfg(feature = "ratatui")]
    pub fn mouse_click_in_rect(&mut self, area: Rect, col: u16, row: u16) {
        self.mouse_click(area.x, area.y, col, row);
    }

    /// Begin a mouse-drag selection: anchor at current cursor and enter Visual mode.
    pub fn mouse_begin_drag(&mut self) {
        if !self.vim.is_visual_char() {
            let cursor = self.cursor();
            self.vim.enter_visual(cursor);
        }
    }

    /// Extend an in-progress mouse drag to the given terminal-space
    /// position. Engine-native coordinate flavour. Always available.
    ///
    /// Renamed from `mouse_extend_drag_xy` in 0.0.32 — at 0.1.0 freeze
    /// the unprefixed name belongs to the universally-available variant.
    pub fn mouse_extend_drag(&mut self, area_x: u16, area_y: u16, col: u16, row: u16) {
        let (r, c) = self.mouse_to_doc_pos_xy(area_x, area_y, col, row);
        buf_set_cursor_rc(&mut self.buffer, r, c);
    }

    /// Ratatui [`Rect`]-flavoured wrapper around
    /// [`Editor::mouse_extend_drag`]. Behind the `ratatui` feature.
    ///
    /// Renamed from `mouse_extend_drag` in 0.0.32 — the unprefixed
    /// name now belongs to the engine-native variant.
    #[cfg(feature = "ratatui")]
    pub fn mouse_extend_drag_in_rect(&mut self, area: Rect, col: u16, row: u16) {
        self.mouse_extend_drag(area.x, area.y, col, row);
    }

    pub fn insert_str(&mut self, text: &str) {
        let pos = crate::types::Cursor::cursor(&self.buffer);
        crate::types::BufferEdit::insert_at(&mut self.buffer, pos, text);
        self.push_buffer_content_to_textarea();
        self.mark_content_dirty();
    }

    pub fn accept_completion(&mut self, completion: &str) {
        use crate::types::{BufferEdit, Cursor as CursorTrait, Pos};
        let cursor_pos = CursorTrait::cursor(&self.buffer);
        let cursor_row = cursor_pos.line as usize;
        let cursor_col = cursor_pos.col as usize;
        let line = buf_line(&self.buffer, cursor_row).unwrap_or("").to_string();
        let chars: Vec<char> = line.chars().collect();
        let prefix_len = chars[..cursor_col.min(chars.len())]
            .iter()
            .rev()
            .take_while(|c| c.is_alphanumeric() || **c == '_')
            .count();
        if prefix_len > 0 {
            let start = Pos {
                line: cursor_row as u32,
                col: (cursor_col - prefix_len) as u32,
            };
            BufferEdit::delete_range(&mut self.buffer, start..cursor_pos);
        }
        let cursor = CursorTrait::cursor(&self.buffer);
        BufferEdit::insert_at(&mut self.buffer, cursor, completion);
        self.push_buffer_content_to_textarea();
        self.mark_content_dirty();
    }

    pub(super) fn snapshot(&self) -> (Vec<String>, (usize, usize)) {
        let rc = buf_cursor_rc(&self.buffer);
        (buf_lines_to_vec(&self.buffer), rc)
    }

    /// Walk one step back through the undo history. Equivalent to the
    /// user pressing `u` in normal mode. Drains the most recent undo
    /// entry and pushes it onto the redo stack.
    pub fn undo(&mut self) {
        crate::vim::do_undo(self);
    }

    /// Walk one step forward through the redo history. Equivalent to
    /// `<C-r>` in normal mode.
    pub fn redo(&mut self) {
        crate::vim::do_redo(self);
    }

    /// Snapshot current buffer state onto the undo stack and clear
    /// the redo stack. Bounded by `settings.undo_levels` — older
    /// entries pruned. Call before any group of buffer mutations the
    /// user might want to undo as a single step.
    pub fn push_undo(&mut self) {
        let snap = self.snapshot();
        self.undo_stack.push(snap);
        self.cap_undo();
        self.redo_stack.clear();
    }

    /// Trim the undo stack down to `settings.undo_levels`, dropping
    /// the oldest entries. `undo_levels == 0` is treated as
    /// "unlimited" (vim's 0-means-no-undo semantics intentionally
    /// skipped — guarding with `> 0` is one line shorter than gating
    /// the cap path with an explicit zero-check above the call site).
    pub(crate) fn cap_undo(&mut self) {
        let cap = self.settings.undo_levels as usize;
        if cap > 0 && self.undo_stack.len() > cap {
            let diff = self.undo_stack.len() - cap;
            self.undo_stack.drain(..diff);
        }
    }

    /// Test-only accessor for the undo stack length.
    #[doc(hidden)]
    pub fn undo_stack_len(&self) -> usize {
        self.undo_stack.len()
    }

    /// Replace the buffer with `lines` joined by `\n` and set the
    /// cursor to `cursor`. Used by undo / `:e!` / snapshot restore
    /// paths. Marks the editor dirty.
    pub fn restore(&mut self, lines: Vec<String>, cursor: (usize, usize)) {
        let text = lines.join("\n");
        crate::types::BufferEdit::replace_all(&mut self.buffer, &text);
        buf_set_cursor_rc(&mut self.buffer, cursor.0, cursor.1);
        // Bulk replace — supersedes any queued ContentEdits.
        self.pending_content_edits.clear();
        self.pending_content_reset = true;
        self.mark_content_dirty();
    }

    /// Returns true if the key was consumed by the editor.
    #[cfg(feature = "crossterm")]
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        let input = crossterm_to_input(key);
        if input.key == Key::Null {
            return false;
        }
        let consumed = vim::step(self, input);
        self.emit_cursor_shape_if_changed();
        consumed
    }
}

#[cfg(feature = "crossterm")]
impl From<KeyEvent> for Input {
    fn from(key: KeyEvent) -> Self {
        let k = match key.code {
            KeyCode::Char(c) => Key::Char(c),
            KeyCode::Backspace => Key::Backspace,
            KeyCode::Delete => Key::Delete,
            KeyCode::Enter => Key::Enter,
            KeyCode::Left => Key::Left,
            KeyCode::Right => Key::Right,
            KeyCode::Up => Key::Up,
            KeyCode::Down => Key::Down,
            KeyCode::Home => Key::Home,
            KeyCode::End => Key::End,
            KeyCode::Tab => Key::Tab,
            KeyCode::Esc => Key::Esc,
            _ => Key::Null,
        };
        Input {
            key: k,
            ctrl: key.modifiers.contains(KeyModifiers::CONTROL),
            alt: key.modifiers.contains(KeyModifiers::ALT),
            shift: key.modifiers.contains(KeyModifiers::SHIFT),
        }
    }
}

/// Crossterm `KeyEvent` → engine `Input`. Thin wrapper that delegates
/// to the [`From`] impl above; kept as a free fn for the in-tree
/// callers in the legacy ratatui-coupled paths.
#[cfg(feature = "crossterm")]
pub(super) fn crossterm_to_input(key: KeyEvent) -> Input {
    Input::from(key)
}

#[cfg(all(test, feature = "crossterm", feature = "ratatui"))]
mod tests {
    use super::*;
    use crate::types::Host;
    use crossterm::event::KeyEvent;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn shift_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::SHIFT)
    }
    fn ctrl_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    #[test]
    fn vim_normal_to_insert() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.handle_key(key(KeyCode::Char('i')));
        assert_eq!(e.vim_mode(), VimMode::Insert);
    }

    #[test]
    fn with_options_constructs_from_spec_options() {
        // 0.0.33 (Patch C-γ): SPEC-shaped constructor preview.
        // Build with custom Options + DefaultHost; confirm the
        // settings translation honours the SPEC field names.
        let opts = crate::types::Options {
            shiftwidth: 4,
            tabstop: 4,
            expandtab: true,
            iskeyword: "@,a-z".to_string(),
            wrap: crate::types::WrapMode::Word,
            ..crate::types::Options::default()
        };
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            opts,
        );
        assert_eq!(e.settings().shiftwidth, 4);
        assert_eq!(e.settings().tabstop, 4);
        assert!(e.settings().expandtab);
        assert_eq!(e.settings().iskeyword, "@,a-z");
        assert_eq!(e.settings().wrap, hjkl_buffer::Wrap::Word);
        // Confirm input plumbing still works.
        e.handle_key(key(KeyCode::Char('i')));
        assert_eq!(e.vim_mode(), VimMode::Insert);
    }

    #[test]
    fn feed_input_char_routes_through_handle_key() {
        use crate::{Modifiers, PlannedInput};
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("abc");
        // `i` enters insert mode via SPEC input.
        e.feed_input(PlannedInput::Char('i', Modifiers::default()));
        assert_eq!(e.vim_mode(), VimMode::Insert);
        // Type 'X' via SPEC input.
        e.feed_input(PlannedInput::Char('X', Modifiers::default()));
        assert!(e.content().contains('X'));
    }

    #[test]
    fn feed_input_special_key_routes() {
        use crate::{Modifiers, PlannedInput, SpecialKey};
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("abc");
        e.feed_input(PlannedInput::Char('i', Modifiers::default()));
        assert_eq!(e.vim_mode(), VimMode::Insert);
        e.feed_input(PlannedInput::Key(SpecialKey::Esc, Modifiers::default()));
        assert_eq!(e.vim_mode(), VimMode::Normal);
    }

    #[test]
    fn feed_input_mouse_paste_focus_resize_no_op() {
        use crate::{MouseEvent, MouseKind, PlannedInput, Pos};
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("abc");
        let mode_before = e.vim_mode();
        let consumed = e.feed_input(PlannedInput::Mouse(MouseEvent {
            kind: MouseKind::Press,
            pos: Pos::new(0, 0),
            mods: Default::default(),
        }));
        assert!(!consumed);
        assert_eq!(e.vim_mode(), mode_before);
        assert!(!e.feed_input(PlannedInput::Paste("xx".into())));
        assert!(!e.feed_input(PlannedInput::FocusGained));
        assert!(!e.feed_input(PlannedInput::FocusLost));
        assert!(!e.feed_input(PlannedInput::Resize(80, 24)));
    }

    #[test]
    fn intern_style_dedups_engine_native_styles() {
        use crate::types::{Attrs, Color, Style};
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        let s = Style {
            fg: Some(Color(255, 0, 0)),
            bg: None,
            attrs: Attrs::BOLD,
        };
        let id_a = e.intern_style(s);
        // Re-interning the same engine style returns the same id.
        let id_b = e.intern_style(s);
        assert_eq!(id_a, id_b);
        // Engine accessor returns the same style back.
        let back = e.engine_style_at(id_a).expect("interned");
        assert_eq!(back, s);
    }

    #[test]
    fn engine_style_at_out_of_range_returns_none() {
        let e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        assert!(e.engine_style_at(99).is_none());
    }

    #[test]
    fn take_changes_emits_per_row_for_block_insert() {
        // Visual-block insert (`Ctrl-V` then `I` then text then Esc)
        // produces an InsertBlock buffer edit with one chunk per
        // selected row. take_changes should surface N EditOps,
        // not a single placeholder.
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("aaa\nbbb\nccc\nddd");
        // Place cursor at (0, 0), enter visual-block, extend down 2.
        e.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL));
        e.handle_key(key(KeyCode::Char('j')));
        e.handle_key(key(KeyCode::Char('j')));
        // `I` to enter insert mode at the block left edge.
        e.handle_key(shift_key(KeyCode::Char('I')));
        e.handle_key(key(KeyCode::Char('X')));
        e.handle_key(key(KeyCode::Esc));

        let changes = e.take_changes();
        // Expect at least 3 entries — one per row in the 3-row block.
        // Vim's block-I inserts on Esc; the cleanup may add more
        // EditOps for cursor sync, hence >= rather than ==.
        assert!(
            changes.len() >= 3,
            "expected >=3 EditOps for 3-row block insert, got {}: {changes:?}",
            changes.len()
        );
    }

    #[test]
    fn take_changes_drains_after_insert() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("abc");
        // Empty initially.
        assert!(e.take_changes().is_empty());
        // Type a char in insert mode.
        e.handle_key(key(KeyCode::Char('i')));
        e.handle_key(key(KeyCode::Char('X')));
        let changes = e.take_changes();
        assert!(
            !changes.is_empty(),
            "insert mode keystroke should produce a change"
        );
        // Drained — second call empty.
        assert!(e.take_changes().is_empty());
    }

    #[test]
    fn options_bridge_roundtrip() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        let opts = e.current_options();
        // 0.1.0: SPEC-faithful Options::default — shiftwidth=8 / tabstop=8.
        assert_eq!(opts.shiftwidth, 8);
        assert_eq!(opts.tabstop, 8);

        let new_opts = crate::types::Options {
            shiftwidth: 4,
            tabstop: 2,
            ignorecase: true,
            ..crate::types::Options::default()
        };
        e.apply_options(&new_opts);

        let after = e.current_options();
        assert_eq!(after.shiftwidth, 4);
        assert_eq!(after.tabstop, 2);
        assert!(after.ignorecase);
    }

    #[test]
    fn selection_highlight_none_in_normal() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("hello");
        assert!(e.selection_highlight().is_none());
    }

    #[test]
    fn selection_highlight_some_in_visual() {
        use crate::types::HighlightKind;
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("hello world");
        e.handle_key(key(KeyCode::Char('v')));
        e.handle_key(key(KeyCode::Char('l')));
        e.handle_key(key(KeyCode::Char('l')));
        let h = e
            .selection_highlight()
            .expect("visual mode should produce a highlight");
        assert_eq!(h.kind, HighlightKind::Selection);
        assert_eq!(h.range.start.line, 0);
        assert_eq!(h.range.end.line, 0);
    }

    #[test]
    fn highlights_emit_incsearch_during_active_prompt() {
        use crate::types::HighlightKind;
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("foo bar foo\nbaz\n");
        // Open the `/` prompt and type `f` `o` `o`.
        e.handle_key(key(KeyCode::Char('/')));
        e.handle_key(key(KeyCode::Char('f')));
        e.handle_key(key(KeyCode::Char('o')));
        e.handle_key(key(KeyCode::Char('o')));
        // Prompt should be active.
        assert!(e.search_prompt().is_some());
        let hs = e.highlights_for_line(0);
        assert_eq!(hs.len(), 2);
        for h in &hs {
            assert_eq!(h.kind, HighlightKind::IncSearch);
        }
    }

    #[test]
    fn highlights_empty_for_blank_prompt() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("foo");
        e.handle_key(key(KeyCode::Char('/')));
        // Nothing typed yet — prompt active but text empty.
        assert!(e.search_prompt().is_some());
        assert!(e.highlights_for_line(0).is_empty());
    }

    #[test]
    fn highlights_emit_search_matches() {
        use crate::types::HighlightKind;
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("foo bar foo\nbaz qux\n");
        // 0.0.35: arm via the engine search state. The buffer
        // accessor still works (deprecated) but new code goes
        // through Editor.
        e.set_search_pattern(Some(regex::Regex::new("foo").unwrap()));
        let hs = e.highlights_for_line(0);
        assert_eq!(hs.len(), 2);
        for h in &hs {
            assert_eq!(h.kind, HighlightKind::SearchMatch);
            assert_eq!(h.range.start.line, 0);
            assert_eq!(h.range.end.line, 0);
        }
    }

    #[test]
    fn highlights_empty_without_pattern() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("foo bar");
        assert!(e.highlights_for_line(0).is_empty());
    }

    #[test]
    fn highlights_empty_for_out_of_range_line() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("foo");
        e.set_search_pattern(Some(regex::Regex::new("foo").unwrap()));
        assert!(e.highlights_for_line(99).is_empty());
    }

    #[test]
    fn render_frame_reflects_mode_and_cursor() {
        use crate::types::{CursorShape, SnapshotMode};
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("alpha\nbeta");
        let f = e.render_frame();
        assert_eq!(f.mode, SnapshotMode::Normal);
        assert_eq!(f.cursor_shape, CursorShape::Block);
        assert_eq!(f.line_count, 2);

        e.handle_key(key(KeyCode::Char('i')));
        let f = e.render_frame();
        assert_eq!(f.mode, SnapshotMode::Insert);
        assert_eq!(f.cursor_shape, CursorShape::Bar);
    }

    #[test]
    fn snapshot_roundtrips_through_restore() {
        use crate::types::SnapshotMode;
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("alpha\nbeta\ngamma");
        e.jump_cursor(2, 3);
        let snap = e.take_snapshot();
        assert_eq!(snap.mode, SnapshotMode::Normal);
        assert_eq!(snap.cursor, (2, 3));
        assert_eq!(snap.lines.len(), 3);

        let mut other = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        other.restore_snapshot(snap).expect("restore");
        assert_eq!(other.cursor(), (2, 3));
        assert_eq!(other.buffer().lines().len(), 3);
    }

    #[test]
    fn restore_snapshot_rejects_version_mismatch() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        let mut snap = e.take_snapshot();
        snap.version = 9999;
        match e.restore_snapshot(snap) {
            Err(crate::EngineError::SnapshotVersion(got, want)) => {
                assert_eq!(got, 9999);
                assert_eq!(want, crate::types::EditorSnapshot::VERSION);
            }
            other => panic!("expected SnapshotVersion err, got {other:?}"),
        }
    }

    #[test]
    fn take_content_change_returns_some_on_first_dirty() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("hello");
        let first = e.take_content_change();
        assert!(first.is_some());
        let second = e.take_content_change();
        assert!(second.is_none());
    }

    #[test]
    fn take_content_change_none_until_mutation() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("hello");
        // drain
        e.take_content_change();
        assert!(e.take_content_change().is_none());
        // mutate via insert mode
        e.handle_key(key(KeyCode::Char('i')));
        e.handle_key(key(KeyCode::Char('x')));
        let after = e.take_content_change();
        assert!(after.is_some());
        assert!(after.unwrap().contains('x'));
    }

    #[test]
    fn vim_insert_to_normal() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.handle_key(key(KeyCode::Char('i')));
        e.handle_key(key(KeyCode::Esc));
        assert_eq!(e.vim_mode(), VimMode::Normal);
    }

    #[test]
    fn vim_normal_to_visual() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.handle_key(key(KeyCode::Char('v')));
        assert_eq!(e.vim_mode(), VimMode::Visual);
    }

    #[test]
    fn vim_visual_to_normal() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.handle_key(key(KeyCode::Char('v')));
        e.handle_key(key(KeyCode::Esc));
        assert_eq!(e.vim_mode(), VimMode::Normal);
    }

    #[test]
    fn vim_shift_i_moves_to_first_non_whitespace() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("   hello");
        e.jump_cursor(0, 8);
        e.handle_key(shift_key(KeyCode::Char('I')));
        assert_eq!(e.vim_mode(), VimMode::Insert);
        assert_eq!(e.cursor(), (0, 3));
    }

    #[test]
    fn vim_shift_a_moves_to_end_and_insert() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("hello");
        e.handle_key(shift_key(KeyCode::Char('A')));
        assert_eq!(e.vim_mode(), VimMode::Insert);
        assert_eq!(e.cursor().1, 5);
    }

    #[test]
    fn count_10j_moves_down_10() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content(
            (0..20)
                .map(|i| format!("line{i}"))
                .collect::<Vec<_>>()
                .join("\n")
                .as_str(),
        );
        for d in "10".chars() {
            e.handle_key(key(KeyCode::Char(d)));
        }
        e.handle_key(key(KeyCode::Char('j')));
        assert_eq!(e.cursor().0, 10);
    }

    #[test]
    fn count_o_repeats_insert_on_esc() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("hello");
        for d in "3".chars() {
            e.handle_key(key(KeyCode::Char(d)));
        }
        e.handle_key(key(KeyCode::Char('o')));
        assert_eq!(e.vim_mode(), VimMode::Insert);
        for c in "world".chars() {
            e.handle_key(key(KeyCode::Char(c)));
        }
        e.handle_key(key(KeyCode::Esc));
        assert_eq!(e.vim_mode(), VimMode::Normal);
        assert_eq!(e.buffer().lines().len(), 4);
        assert!(e.buffer().lines().iter().skip(1).all(|l| l == "world"));
    }

    #[test]
    fn count_i_repeats_text_on_esc() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("");
        for d in "3".chars() {
            e.handle_key(key(KeyCode::Char(d)));
        }
        e.handle_key(key(KeyCode::Char('i')));
        for c in "ab".chars() {
            e.handle_key(key(KeyCode::Char(c)));
        }
        e.handle_key(key(KeyCode::Esc));
        assert_eq!(e.vim_mode(), VimMode::Normal);
        assert_eq!(e.buffer().lines()[0], "ababab");
    }

    #[test]
    fn vim_shift_o_opens_line_above() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("hello");
        e.handle_key(shift_key(KeyCode::Char('O')));
        assert_eq!(e.vim_mode(), VimMode::Insert);
        assert_eq!(e.cursor(), (0, 0));
        assert_eq!(e.buffer().lines().len(), 2);
    }

    #[test]
    fn vim_gg_goes_to_top() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("a\nb\nc");
        e.jump_cursor(2, 0);
        e.handle_key(key(KeyCode::Char('g')));
        e.handle_key(key(KeyCode::Char('g')));
        assert_eq!(e.cursor().0, 0);
    }

    #[test]
    fn vim_shift_g_goes_to_bottom() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("a\nb\nc");
        e.handle_key(shift_key(KeyCode::Char('G')));
        assert_eq!(e.cursor().0, 2);
    }

    #[test]
    fn vim_dd_deletes_line() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("first\nsecond");
        e.handle_key(key(KeyCode::Char('d')));
        e.handle_key(key(KeyCode::Char('d')));
        assert_eq!(e.buffer().lines().len(), 1);
        assert_eq!(e.buffer().lines()[0], "second");
    }

    #[test]
    fn vim_dw_deletes_word() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("hello world");
        e.handle_key(key(KeyCode::Char('d')));
        e.handle_key(key(KeyCode::Char('w')));
        assert_eq!(e.vim_mode(), VimMode::Normal);
        assert!(!e.buffer().lines()[0].starts_with("hello"));
    }

    #[test]
    fn vim_yy_yanks_line() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("hello\nworld");
        e.handle_key(key(KeyCode::Char('y')));
        e.handle_key(key(KeyCode::Char('y')));
        assert!(e.last_yank.as_deref().unwrap_or("").starts_with("hello"));
    }

    #[test]
    fn vim_yy_does_not_move_cursor() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("first\nsecond\nthird");
        e.jump_cursor(1, 0);
        let before = e.cursor();
        e.handle_key(key(KeyCode::Char('y')));
        e.handle_key(key(KeyCode::Char('y')));
        assert_eq!(e.cursor(), before);
        assert_eq!(e.vim_mode(), VimMode::Normal);
    }

    #[test]
    fn vim_yw_yanks_word() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("hello world");
        e.handle_key(key(KeyCode::Char('y')));
        e.handle_key(key(KeyCode::Char('w')));
        assert_eq!(e.vim_mode(), VimMode::Normal);
        assert!(e.last_yank.is_some());
    }

    #[test]
    fn vim_cc_changes_line() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("hello\nworld");
        e.handle_key(key(KeyCode::Char('c')));
        e.handle_key(key(KeyCode::Char('c')));
        assert_eq!(e.vim_mode(), VimMode::Insert);
    }

    #[test]
    fn vim_u_undoes_insert_session_as_chunk() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("hello");
        e.handle_key(key(KeyCode::Char('i')));
        e.handle_key(key(KeyCode::Enter));
        e.handle_key(key(KeyCode::Enter));
        e.handle_key(key(KeyCode::Esc));
        assert_eq!(e.buffer().lines().len(), 3);
        e.handle_key(key(KeyCode::Char('u')));
        assert_eq!(e.buffer().lines().len(), 1);
        assert_eq!(e.buffer().lines()[0], "hello");
    }

    #[test]
    fn vim_undo_redo_roundtrip() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("hello");
        e.handle_key(key(KeyCode::Char('i')));
        for c in "world".chars() {
            e.handle_key(key(KeyCode::Char(c)));
        }
        e.handle_key(key(KeyCode::Esc));
        let after = e.buffer().lines()[0].clone();
        e.handle_key(key(KeyCode::Char('u')));
        assert_eq!(e.buffer().lines()[0], "hello");
        e.handle_key(ctrl_key(KeyCode::Char('r')));
        assert_eq!(e.buffer().lines()[0], after);
    }

    #[test]
    fn vim_u_undoes_dd() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("first\nsecond");
        e.handle_key(key(KeyCode::Char('d')));
        e.handle_key(key(KeyCode::Char('d')));
        assert_eq!(e.buffer().lines().len(), 1);
        e.handle_key(key(KeyCode::Char('u')));
        assert_eq!(e.buffer().lines().len(), 2);
        assert_eq!(e.buffer().lines()[0], "first");
    }

    #[test]
    fn vim_ctrl_r_redoes() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("hello");
        e.handle_key(ctrl_key(KeyCode::Char('r')));
    }

    #[test]
    fn vim_r_replaces_char() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("hello");
        e.handle_key(key(KeyCode::Char('r')));
        e.handle_key(key(KeyCode::Char('x')));
        assert_eq!(e.buffer().lines()[0].chars().next(), Some('x'));
    }

    #[test]
    fn vim_tilde_toggles_case() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("hello");
        e.handle_key(key(KeyCode::Char('~')));
        assert_eq!(e.buffer().lines()[0].chars().next(), Some('H'));
    }

    #[test]
    fn vim_visual_d_cuts() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("hello");
        e.handle_key(key(KeyCode::Char('v')));
        e.handle_key(key(KeyCode::Char('l')));
        e.handle_key(key(KeyCode::Char('l')));
        e.handle_key(key(KeyCode::Char('d')));
        assert_eq!(e.vim_mode(), VimMode::Normal);
        assert!(e.last_yank.is_some());
    }

    #[test]
    fn vim_visual_c_enters_insert() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("hello");
        e.handle_key(key(KeyCode::Char('v')));
        e.handle_key(key(KeyCode::Char('l')));
        e.handle_key(key(KeyCode::Char('c')));
        assert_eq!(e.vim_mode(), VimMode::Insert);
    }

    #[test]
    fn vim_normal_unknown_key_consumed() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        // Unknown keys are consumed (swallowed) rather than returning false.
        let consumed = e.handle_key(key(KeyCode::Char('z')));
        assert!(consumed);
    }

    #[test]
    fn force_normal_clears_operator() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.handle_key(key(KeyCode::Char('d')));
        e.force_normal();
        assert_eq!(e.vim_mode(), VimMode::Normal);
    }

    fn many_lines(n: usize) -> String {
        (0..n)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn prime_viewport<H: Host>(e: &mut Editor<hjkl_buffer::Buffer, H>, height: u16) {
        e.set_viewport_height(height);
    }

    #[test]
    fn zz_centers_cursor_in_viewport() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content(&many_lines(100));
        prime_viewport(&mut e, 20);
        e.jump_cursor(50, 0);
        e.handle_key(key(KeyCode::Char('z')));
        e.handle_key(key(KeyCode::Char('z')));
        assert_eq!(e.host().viewport().top_row, 40);
        assert_eq!(e.cursor().0, 50);
    }

    #[test]
    fn zt_puts_cursor_at_viewport_top_with_scrolloff() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content(&many_lines(100));
        prime_viewport(&mut e, 20);
        e.jump_cursor(50, 0);
        e.handle_key(key(KeyCode::Char('z')));
        e.handle_key(key(KeyCode::Char('t')));
        // Cursor lands at top of viable area = top + SCROLLOFF (5).
        // Viewport top therefore sits at cursor - 5.
        assert_eq!(e.host().viewport().top_row, 45);
        assert_eq!(e.cursor().0, 50);
    }

    #[test]
    fn ctrl_a_increments_number_at_cursor() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("x = 41");
        e.handle_key(ctrl_key(KeyCode::Char('a')));
        assert_eq!(e.buffer().lines()[0], "x = 42");
        assert_eq!(e.cursor(), (0, 5));
    }

    #[test]
    fn ctrl_a_finds_number_to_right_of_cursor() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("foo 99 bar");
        e.handle_key(ctrl_key(KeyCode::Char('a')));
        assert_eq!(e.buffer().lines()[0], "foo 100 bar");
        assert_eq!(e.cursor(), (0, 6));
    }

    #[test]
    fn ctrl_a_with_count_adds_count() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("x = 10");
        for d in "5".chars() {
            e.handle_key(key(KeyCode::Char(d)));
        }
        e.handle_key(ctrl_key(KeyCode::Char('a')));
        assert_eq!(e.buffer().lines()[0], "x = 15");
    }

    #[test]
    fn ctrl_x_decrements_number() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("n=5");
        e.handle_key(ctrl_key(KeyCode::Char('x')));
        assert_eq!(e.buffer().lines()[0], "n=4");
    }

    #[test]
    fn ctrl_x_crosses_zero_into_negative() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("v=0");
        e.handle_key(ctrl_key(KeyCode::Char('x')));
        assert_eq!(e.buffer().lines()[0], "v=-1");
    }

    #[test]
    fn ctrl_a_on_negative_number_increments_toward_zero() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("a = -5");
        e.handle_key(ctrl_key(KeyCode::Char('a')));
        assert_eq!(e.buffer().lines()[0], "a = -4");
    }

    #[test]
    fn ctrl_a_noop_when_no_digit_on_line() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("no digits here");
        e.handle_key(ctrl_key(KeyCode::Char('a')));
        assert_eq!(e.buffer().lines()[0], "no digits here");
    }

    #[test]
    fn zb_puts_cursor_at_viewport_bottom_with_scrolloff() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content(&many_lines(100));
        prime_viewport(&mut e, 20);
        e.jump_cursor(50, 0);
        e.handle_key(key(KeyCode::Char('z')));
        e.handle_key(key(KeyCode::Char('b')));
        // Cursor lands at bottom of viable area = top + height - 1 -
        // SCROLLOFF. For height 20, scrolloff 5: cursor at top + 14,
        // so top = cursor - 14 = 36.
        assert_eq!(e.host().viewport().top_row, 36);
        assert_eq!(e.cursor().0, 50);
    }

    /// Contract that the TUI drain relies on: `set_content` flags the
    /// editor dirty (so the next `take_dirty` call reports the change),
    /// and a second `take_dirty` returns `false` after consumption. The
    /// TUI drains this flag after every programmatic content load so
    /// opening a tab doesn't get mistaken for a user edit and mark the
    /// tab dirty (which would then trigger the quit-prompt on `:q`).
    #[test]
    fn set_content_dirties_then_take_dirty_clears() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("hello");
        assert!(
            e.take_dirty(),
            "set_content should leave content_dirty=true"
        );
        assert!(!e.take_dirty(), "take_dirty should clear the flag");
    }

    #[test]
    fn content_arc_returns_same_arc_until_mutation() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("hello");
        let a = e.content_arc();
        let b = e.content_arc();
        assert!(
            std::sync::Arc::ptr_eq(&a, &b),
            "repeated content_arc() should hit the cache"
        );

        // Any mutation must invalidate the cache.
        e.handle_key(key(KeyCode::Char('i')));
        e.handle_key(key(KeyCode::Char('!')));
        let c = e.content_arc();
        assert!(
            !std::sync::Arc::ptr_eq(&a, &c),
            "mutation should invalidate content_arc() cache"
        );
        assert!(c.contains('!'));
    }

    #[test]
    fn content_arc_cache_invalidated_by_set_content() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("one");
        let a = e.content_arc();
        e.set_content("two");
        let b = e.content_arc();
        assert!(!std::sync::Arc::ptr_eq(&a, &b));
        assert!(b.starts_with("two"));
    }

    /// Click past the last char of a line should land the cursor on
    /// the line's last char (Normal mode), not one past it. The
    /// previous bug clamped to the line's BYTE length and used `>=`
    /// past-end, so clicking deep into the trailing space parked the
    /// cursor at `chars().count()` — past where Normal mode lives.
    #[test]
    fn mouse_click_past_eol_lands_on_last_char() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("hello");
        // Outer editor area: x=0, y=0, width=80. mouse_to_doc_pos
        // reserves row 0 for the tab bar and adds gutter padding,
        // so click row 1, way past the line end.
        let area = ratatui::layout::Rect::new(0, 0, 80, 10);
        e.mouse_click_in_rect(area, 78, 1);
        assert_eq!(e.cursor(), (0, 4));
    }

    #[test]
    fn mouse_click_past_eol_handles_multibyte_line() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        // 5 chars, 6 bytes — old code's `String::len()` clamp was
        // wrong here.
        e.set_content("héllo");
        let area = ratatui::layout::Rect::new(0, 0, 80, 10);
        e.mouse_click_in_rect(area, 78, 1);
        assert_eq!(e.cursor(), (0, 4));
    }

    #[test]
    fn mouse_click_inside_line_lands_on_clicked_char() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("hello world");
        // Gutter is `lnum_width + 1` = (1-digit row count + 2) + 1
        // pane padding = 4 cells; click col 4 is the first char.
        let area = ratatui::layout::Rect::new(0, 0, 80, 10);
        e.mouse_click_in_rect(area, 4, 1);
        assert_eq!(e.cursor(), (0, 0));
        e.mouse_click_in_rect(area, 6, 1);
        assert_eq!(e.cursor(), (0, 2));
    }

    /// Vim parity: a mouse-position click during insert mode counts
    /// as a motion and breaks the active undo group (when
    /// `undo_break_on_motion` is on, the default). After clicking and
    /// typing more chars, `u` should reverse only the post-click run.
    #[test]
    fn mouse_click_breaks_insert_undo_group_when_undobreak_on() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("hello world");
        let area = ratatui::layout::Rect::new(0, 0, 80, 10);
        // Default settings.undo_break_on_motion = true.
        assert!(e.settings().undo_break_on_motion);
        // Enter insert mode and type "AAA" before the line content.
        e.handle_key(key(KeyCode::Char('i')));
        e.handle_key(key(KeyCode::Char('A')));
        e.handle_key(key(KeyCode::Char('A')));
        e.handle_key(key(KeyCode::Char('A')));
        // Mouse click somewhere else on the line (still insert mode).
        e.mouse_click_in_rect(area, 10, 1);
        // Type more chars at the new cursor position.
        e.handle_key(key(KeyCode::Char('B')));
        e.handle_key(key(KeyCode::Char('B')));
        e.handle_key(key(KeyCode::Char('B')));
        // Leave insert and undo once.
        e.handle_key(key(KeyCode::Esc));
        e.handle_key(key(KeyCode::Char('u')));
        let line = e.buffer().line(0).unwrap_or("").to_string();
        assert!(
            line.contains("AAA"),
            "AAA must survive undo (separate group): {line:?}"
        );
        assert!(
            !line.contains("BBB"),
            "BBB must be undone (post-click group): {line:?}"
        );
    }

    /// With `:set noundobreak`, the entire insert run — including
    /// chars typed before AND after a mouse click — should collapse
    /// into one undo group, so `u` clears everything.
    #[test]
    fn mouse_click_keeps_one_undo_group_when_undobreak_off() {
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("hello world");
        e.settings_mut().undo_break_on_motion = false;
        let area = ratatui::layout::Rect::new(0, 0, 80, 10);
        e.handle_key(key(KeyCode::Char('i')));
        e.handle_key(key(KeyCode::Char('A')));
        e.handle_key(key(KeyCode::Char('A')));
        e.mouse_click_in_rect(area, 10, 1);
        e.handle_key(key(KeyCode::Char('B')));
        e.handle_key(key(KeyCode::Char('B')));
        e.handle_key(key(KeyCode::Esc));
        e.handle_key(key(KeyCode::Char('u')));
        let line = e.buffer().line(0).unwrap_or("").to_string();
        assert!(
            !line.contains("AA") && !line.contains("BB"),
            "with undobreak off, single `u` must reverse whole insert: {line:?}"
        );
        assert_eq!(line, "hello world");
    }

    // ── Patch B (0.0.29): Host trait wired into Editor ──

    #[test]
    fn host_clipboard_round_trip_via_default_host() {
        // DefaultHost stores write_clipboard in-memory; read_clipboard
        // returns the most recent payload.
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.host_mut().write_clipboard("payload".to_string());
        assert_eq!(e.host_mut().read_clipboard().as_deref(), Some("payload"));
    }

    #[test]
    fn host_records_clipboard_on_yank() {
        // `yy` on a single-line buffer must drive `Host::write_clipboard`
        // (the new Patch B side-channel) in addition to the legacy
        // `last_yank` mirror.
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        );
        e.set_content("hello\n");
        e.handle_key(key(KeyCode::Char('y')));
        e.handle_key(key(KeyCode::Char('y')));
        // Clipboard cache holds the linewise yank.
        let clip = e.host_mut().read_clipboard();
        assert!(
            clip.as_deref().unwrap_or("").starts_with("hello"),
            "host clipboard should carry the yank: {clip:?}"
        );
        // Legacy mirror still populated for 0.0.28-era hosts.
        assert!(e.last_yank.as_deref().unwrap_or("").starts_with("hello"));
    }

    #[test]
    fn host_cursor_shape_via_shared_recorder() {
        // Recording host backed by a leaked `Mutex` so the test can
        // inspect the emit sequence after the editor has consumed the
        // host. (Host: Send rules out Rc/RefCell.)
        let shapes_ptr: &'static std::sync::Mutex<Vec<crate::types::CursorShape>> =
            Box::leak(Box::new(std::sync::Mutex::new(Vec::new())));
        struct LeakHost {
            shapes: &'static std::sync::Mutex<Vec<crate::types::CursorShape>>,
            viewport: crate::types::Viewport,
        }
        impl crate::types::Host for LeakHost {
            type Intent = ();
            fn write_clipboard(&mut self, _: String) {}
            fn read_clipboard(&mut self) -> Option<String> {
                None
            }
            fn now(&self) -> core::time::Duration {
                core::time::Duration::ZERO
            }
            fn prompt_search(&mut self) -> Option<String> {
                None
            }
            fn emit_cursor_shape(&mut self, s: crate::types::CursorShape) {
                self.shapes.lock().unwrap().push(s);
            }
            fn viewport(&self) -> &crate::types::Viewport {
                &self.viewport
            }
            fn viewport_mut(&mut self) -> &mut crate::types::Viewport {
                &mut self.viewport
            }
            fn emit_intent(&mut self, _: Self::Intent) {}
        }
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            LeakHost {
                shapes: shapes_ptr,
                viewport: crate::types::Viewport::default(),
            },
            crate::types::Options::default(),
        );
        e.set_content("abc");
        // Normal → Insert: Bar emit.
        e.handle_key(key(KeyCode::Char('i')));
        // Insert → Normal: Block emit.
        e.handle_key(key(KeyCode::Esc));
        let shapes = shapes_ptr.lock().unwrap().clone();
        assert_eq!(
            shapes,
            vec![
                crate::types::CursorShape::Bar,
                crate::types::CursorShape::Block,
            ],
            "host should observe Insert(Bar) → Normal(Block) transitions"
        );
    }

    #[test]
    fn host_now_drives_chord_timeout_deterministically() {
        // Custom host whose `now()` is host-controlled; we drive it
        // forward by `timeout_len + 1ms` between the first `g` and
        // the second so the chord-timeout fires regardless of
        // wall-clock progress.
        let now_ptr: &'static std::sync::Mutex<core::time::Duration> =
            Box::leak(Box::new(std::sync::Mutex::new(core::time::Duration::ZERO)));
        struct ClockHost {
            now: &'static std::sync::Mutex<core::time::Duration>,
            viewport: crate::types::Viewport,
        }
        impl crate::types::Host for ClockHost {
            type Intent = ();
            fn write_clipboard(&mut self, _: String) {}
            fn read_clipboard(&mut self) -> Option<String> {
                None
            }
            fn now(&self) -> core::time::Duration {
                *self.now.lock().unwrap()
            }
            fn prompt_search(&mut self) -> Option<String> {
                None
            }
            fn emit_cursor_shape(&mut self, _: crate::types::CursorShape) {}
            fn viewport(&self) -> &crate::types::Viewport {
                &self.viewport
            }
            fn viewport_mut(&mut self) -> &mut crate::types::Viewport {
                &mut self.viewport
            }
            fn emit_intent(&mut self, _: Self::Intent) {}
        }
        let mut e = Editor::new(
            hjkl_buffer::Buffer::new(),
            ClockHost {
                now: now_ptr,
                viewport: crate::types::Viewport::default(),
            },
            crate::types::Options::default(),
        );
        e.set_content("a\nb\nc\n");
        e.jump_cursor(2, 0);
        // First `g` — host time = 0ms, lands in g-pending.
        e.handle_key(key(KeyCode::Char('g')));
        // Advance host time well past timeout_len (default 1000ms).
        *now_ptr.lock().unwrap() = core::time::Duration::from_secs(60);
        // Second `g` — chord-timeout fires; bare `g` re-dispatches and
        // does nothing on its own. Cursor must NOT have jumped to row 0.
        e.handle_key(key(KeyCode::Char('g')));
        assert_eq!(
            e.cursor().0,
            2,
            "Host::now() must drive `:set timeoutlen` deterministically"
        );
    }

    // ── ContentEdit emission ─────────────────────────────────────────

    fn fresh_editor(initial: &str) -> Editor {
        let buffer = hjkl_buffer::Buffer::from_str(initial);
        Editor::new(
            buffer,
            crate::types::DefaultHost::new(),
            crate::types::Options::default(),
        )
    }

    #[test]
    fn content_edit_insert_char_at_origin() {
        let mut e = fresh_editor("");
        let _ = e.mutate_edit(hjkl_buffer::Edit::InsertChar {
            at: hjkl_buffer::Position::new(0, 0),
            ch: 'a',
        });
        let edits = e.take_content_edits();
        assert_eq!(edits.len(), 1);
        let ce = &edits[0];
        assert_eq!(ce.start_byte, 0);
        assert_eq!(ce.old_end_byte, 0);
        assert_eq!(ce.new_end_byte, 1);
        assert_eq!(ce.start_position, (0, 0));
        assert_eq!(ce.old_end_position, (0, 0));
        assert_eq!(ce.new_end_position, (0, 1));
    }

    #[test]
    fn content_edit_insert_str_multiline() {
        // Buffer "x\ny" — insert "ab\ncd" at end of row 0.
        let mut e = fresh_editor("x\ny");
        let _ = e.mutate_edit(hjkl_buffer::Edit::InsertStr {
            at: hjkl_buffer::Position::new(0, 1),
            text: "ab\ncd".into(),
        });
        let edits = e.take_content_edits();
        assert_eq!(edits.len(), 1);
        let ce = &edits[0];
        assert_eq!(ce.start_byte, 1);
        assert_eq!(ce.old_end_byte, 1);
        assert_eq!(ce.new_end_byte, 1 + 5);
        assert_eq!(ce.start_position, (0, 1));
        // Insertion contains one '\n', so row+1, col = bytes after last '\n' = 2.
        assert_eq!(ce.new_end_position, (1, 2));
    }

    #[test]
    fn content_edit_delete_range_charwise() {
        // "abcdef" — delete chars 1..4 ("bcd").
        let mut e = fresh_editor("abcdef");
        let _ = e.mutate_edit(hjkl_buffer::Edit::DeleteRange {
            start: hjkl_buffer::Position::new(0, 1),
            end: hjkl_buffer::Position::new(0, 4),
            kind: hjkl_buffer::MotionKind::Char,
        });
        let edits = e.take_content_edits();
        assert_eq!(edits.len(), 1);
        let ce = &edits[0];
        assert_eq!(ce.start_byte, 1);
        assert_eq!(ce.old_end_byte, 4);
        assert_eq!(ce.new_end_byte, 1);
        assert!(ce.old_end_byte > ce.new_end_byte);
    }

    #[test]
    fn content_edit_set_content_resets() {
        let mut e = fresh_editor("foo");
        let _ = e.mutate_edit(hjkl_buffer::Edit::InsertChar {
            at: hjkl_buffer::Position::new(0, 0),
            ch: 'X',
        });
        // set_content should clear queued edits and raise the reset
        // flag on the next take_content_reset.
        e.set_content("brand new");
        assert!(e.take_content_reset());
        // Subsequent call clears the flag.
        assert!(!e.take_content_reset());
        // Edits cleared on reset.
        assert!(e.take_content_edits().is_empty());
    }

    #[test]
    fn content_edit_multiple_replaces_in_order() {
        // Three Replace edits applied left-to-right (mimics the
        // substitute path's per-match Replace fan-out). Verify each
        // mutation queues exactly one ContentEdit and they're drained
        // in source-order with structurally valid byte spans.
        let mut e = fresh_editor("xax xbx xcx");
        let _ = e.take_content_edits();
        let _ = e.take_content_reset();
        // Replace each "x" with "yy", left to right. After each replace,
        // the next match's char-col shifts by +1 (since "yy" is 1 char
        // longer than "x" but they're both ASCII so byte = char here).
        let positions = [(0usize, 0usize), (0, 4), (0, 8)];
        for (row, col) in positions {
            let _ = e.mutate_edit(hjkl_buffer::Edit::Replace {
                start: hjkl_buffer::Position::new(row, col),
                end: hjkl_buffer::Position::new(row, col + 1),
                with: "yy".into(),
            });
        }
        let edits = e.take_content_edits();
        assert_eq!(edits.len(), 3);
        for ce in &edits {
            assert!(ce.start_byte <= ce.old_end_byte);
            assert!(ce.start_byte <= ce.new_end_byte);
        }
        // Document order.
        for w in edits.windows(2) {
            assert!(w[0].start_byte <= w[1].start_byte);
        }
    }
}
