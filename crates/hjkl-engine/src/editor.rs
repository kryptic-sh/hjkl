//! Editor — the public sqeel-vim type, layered over `hjkl_buffer::Buffer`.
//!
//! This file owns the public Editor API — construction, content access,
//! mouse and goto helpers, the (buffer-level) undo stack, and insert-mode
//! session bookkeeping. All vim-specific keyboard handling lives in
//! [`vim`] and communicates with Editor through a small internal API
//! exposed via `pub(super)` fields and helper methods.

use crate::input::Input;
use crate::vim::{self, VimState};
use crate::{KeybindingMode, VimMode};
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::SystemTime;

/// A single entry in the undo or redo stack.
///
/// The `timestamp` records the wall-clock time at which the snapshot was
/// taken (i.e. when `push_undo` was called), enabling the `:earlier` /
/// `:later` time-travel ex commands to walk the stack by duration rather
/// than by step count.
pub(crate) struct UndoEntry {
    pub(crate) rope: ropey::Rope,
    pub(crate) cursor: (usize, usize),
    pub(crate) timestamp: SystemTime,
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
/// Byte offset of the first byte of `row` within the canonical
/// `lines().join("\n")` byte rendering. Pre-rope this walked every row
/// from 0 to `row` allocating a `String` per row to read its `.len()` —
/// O(row) allocations per call, fired from `position_to_byte_coords` on
/// every `insert_char`. At the bottom of a 1.86 M-line buffer that was
/// 1.86 M String allocations per keystroke (the dominant cost of the
/// "edits at the bottom of the file are slow" symptom).
///
/// Now O(log N): ropey's `line_to_byte` walks the B-tree's per-node
/// byte counts. No String materialization.
#[inline]
fn buffer_byte_of_row(buf: &hjkl_buffer::Buffer, row: usize) -> usize {
    let rope = buf.rope();
    let row = row.min(rope.len_lines());
    rope.line_to_byte(row)
}

/// Convert an `hjkl_buffer::Position` (char-indexed col) into byte
/// coordinates `(byte_within_buffer, (row, col_byte))` against the
/// **pre-edit** buffer.
fn position_to_byte_coords(
    buf: &hjkl_buffer::Buffer,
    pos: hjkl_buffer::Position,
) -> (usize, (u32, u32)) {
    let row = pos.row.min(buf.row_count().saturating_sub(1));
    let rope = buf.rope();
    let line = hjkl_buffer::rope_line_str(&rope, row);
    let col_byte = pos.byte_offset(&line);
    let byte = buffer_byte_of_row(buf, row) + col_byte;
    (byte, (row as u32, col_byte as u32))
}

/// Walk `bytes[..end]` counting newlines and return the (row, col_byte)
/// position at byte offset `end`. `col_byte` is the byte distance from
/// the most recent `\n` (or buffer start). Used to translate a byte
/// offset into a tree-sitter `Point`.
fn byte_to_row_col(bytes: &[u8], end: usize) -> (u32, u32) {
    let end = end.min(bytes.len());
    let mut row: u32 = 0;
    let mut row_start: usize = 0;
    for (i, &b) in bytes[..end].iter().enumerate() {
        if b == b'\n' {
            row += 1;
            row_start = i + 1;
        }
    }
    (row, (end - row_start) as u32)
}

/// Rope-backed minimal content-edit diff for the undo/redo
/// `restore_text` path. Walks `old_rope` chunk-by-chunk for the
/// common-prefix / common-suffix scan instead of forcing a full
/// `content_joined()` materialization (~3 MB per undo on huge files).
///
/// `ropey::Rope::bytes()` and `bytes_at(n).reversed()` give O(log N)
/// seek + O(1)-per-byte step, so the scan cost matches the contiguous
/// `&[u8]` version without the materialization alloc.
fn minimal_content_edit_rope(old_rope: &ropey::Rope, new_text: &str) -> crate::types::ContentEdit {
    let new_bytes = new_text.as_bytes();
    let old_len = old_rope.len_bytes();
    let new_len = new_bytes.len();
    let common = old_len.min(new_len);

    // Common prefix length — forward walk through rope bytes.
    let mut prefix = 0;
    let mut fwd = old_rope.bytes();
    while prefix < common {
        match fwd.next() {
            Some(b) if b == new_bytes[prefix] => prefix += 1,
            _ => break,
        }
    }
    while prefix > 0 && prefix < old_len && (old_rope.byte(prefix) & 0b1100_0000) == 0b1000_0000 {
        prefix -= 1;
    }

    // Common suffix length — backward walk through rope bytes.
    let mut suffix = 0;
    let max_suffix = (old_len - prefix).min(new_len - prefix);
    let mut rev = old_rope.bytes_at(old_len).reversed();
    while suffix < max_suffix {
        match rev.next() {
            Some(b) if b == new_bytes[new_len - 1 - suffix] => suffix += 1,
            _ => break,
        }
    }
    while suffix > 0
        && suffix < old_len
        && (old_rope.byte(old_len - suffix) & 0b1100_0000) == 0b1000_0000
    {
        suffix -= 1;
    }

    let start_byte = prefix;
    let old_end_byte = old_len - suffix;
    let new_end_byte = new_len - suffix;

    crate::types::ContentEdit {
        start_byte,
        old_end_byte,
        new_end_byte,
        start_position: rope_byte_to_row_col(old_rope, start_byte),
        old_end_position: rope_byte_to_row_col(old_rope, old_end_byte),
        new_end_position: byte_to_row_col(new_bytes, new_end_byte),
    }
}

#[inline]
fn rope_byte_to_row_col(rope: &ropey::Rope, byte_idx: usize) -> (u32, u32) {
    let byte_idx = byte_idx.min(rope.len_bytes());
    let line = rope.byte_to_line(byte_idx);
    let line_start = rope.line_to_byte(line);
    (line as u32, (byte_idx - line_start) as u32)
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
                        let last_row = buf.row_count().saturating_sub(1);
                        buffer_byte_of_row(buf, buf.row_count())
                            + hjkl_buffer::rope_line_bytes(&buf.rope(), last_row)
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
            let buf_rope = buf.rope();
            let line = hjkl_buffer::rope_line_str(&buf_rope, row);
            let row_eol_byte = buffer_byte_of_row(buf, row) + line.len();
            let row_eol_col = line.len() as u32;
            let next_row_after = last_join_row + 1;
            let old_end_byte = if next_row_after < buf.row_count() {
                buffer_byte_of_row(buf, next_row_after).saturating_sub(1)
            } else {
                let last_row = buf.row_count().saturating_sub(1);
                buffer_byte_of_row(buf, buf.row_count())
                    + hjkl_buffer::rope_line_bytes(&buf_rope, last_row)
            };
            let last_line = hjkl_buffer::rope_line_str(&buf_rope, last_join_row);
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
            let split_rope = buf.rope();
            let line = hjkl_buffer::rope_line_str(&split_rope, row);
            let row_byte = buffer_byte_of_row(buf, row);
            let insert = if *inserted_space { "\n " } else { "\n" };
            for &c in cols {
                let pos = Position::new(row, c);
                let col_byte = pos.byte_offset(&line);
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
    apply_buffer_edit, buf_cursor_pos, buf_cursor_rc, buf_cursor_row, buf_line, buf_line_chars,
    buf_row_count, buf_set_cursor_rc,
};

/// Return value from the engine's `try_goto_mark_*` methods. Tells the
/// caller (app layer) whether a cross-buffer switch is required.
///
/// - `SameBuffer` — cursor moved (or mark was unset → no-op) within the
///   same buffer; no buffer switch needed.
/// - `CrossBuffer` — the mark lives in a different buffer. The app must
///   switch to the slot whose `buffer_id` matches, then position the cursor
///   at `(row, col)` using `Editor::jump_cursor`.
/// - `Unset` — mark not set; no action needed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkJump {
    SameBuffer,
    CrossBuffer {
        buffer_id: u64,
        row: usize,
        col: usize,
    },
    Unset,
}

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
    /// Undo history: each entry is `(joined_document, cursor)` before the
    /// edit. Stored as `Arc<String>` so it shares the
    /// Undo history: snapshots taken via `Buffer::rope()` — `ropey::Rope::clone`
    /// is O(1) (Arc-clone of the B-tree root). Previously stored
    /// `Arc<String>` from `content_joined()`, which on the rope storage
    /// builds the entire document `String` via `rope.to_string()` — that
    /// turned every `i` / `o` keystroke into a ~3 MB allocation on a
    /// 1.86 M-line file.
    pub(crate) undo_stack: Vec<UndoEntry>,
    /// Redo history: entries pushed when undoing.
    pub(super) redo_stack: Vec<UndoEntry>,
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
    /// for 0.1.0 — see `crate::buf_helpers::apply_buffer_edit`.
    pub(super) buffer: B,
    /// Engine-native style intern table. Opaque `Span::style` ids index
    /// into this table; the render path resolves ids back to
    /// [`crate::types::Style`]. Ratatui hosts convert at the boundary via
    /// `hjkl_engine_tui::style_to_ratatui`. Always present — no cfg-mutex.
    pub(super) style_table: Vec<crate::types::Style>,
    /// Vim-style register bank — `"`, `"0`–`"9`, `"a`–`"z`. Sources
    /// every `p` / `P` via the active selector (default unnamed).
    /// Internal — read via [`Editor::registers`]; mutated by yank /
    /// delete / paste FSM paths and by [`Editor::seed_yank`].
    pub(crate) registers: crate::registers::Registers,
    /// Per-row syntax styling in engine-native form. Always present —
    /// populated by [`Editor::install_syntax_spans`]. Ratatui hosts use
    /// `hjkl_engine_tui::EditorRatatuiExt::install_ratatui_syntax_spans`.
    pub styled_spans: Vec<Vec<(usize, usize, crate::types::Style)>>,
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
    /// Global (uppercase) marks that carry a `buffer_id` so they can jump
    /// across buffers. Keyed by `'A'`–`'Z'`; values are
    /// `(buffer_id, row, col)`. Set by `m{A-Z}`, resolved by
    /// `try_goto_mark_line` / `try_goto_mark_char`.
    pub(crate) global_marks: std::collections::BTreeMap<char, (u64, usize, usize)>,
    /// The `buffer_id` this editor instance is currently attached to.
    /// Updated by the host app on every `switch_to` / slot creation so
    /// global-mark writes record the correct id without requiring the app
    /// to pass the id on every keystroke.
    pub(crate) current_buffer_id: u64,
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
    /// [`Editor::install_syntax_spans`] (ratatui hosts use
    /// `hjkl_engine_tui::EditorRatatuiExt::install_ratatui_syntax_spans`)
    /// and, in due course, by `Host::syntax_highlights` once the engine
    /// drives that path directly.
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
    /// Row range touched by the most recent `auto_indent_rows` call.
    /// `(top_row, bot_row)` inclusive. Set by the engine after every
    /// auto-indent operation; drained (and cleared) by the host via
    /// [`Editor::take_last_indent_range`] so it can display a brief
    /// visual flash over the reindented rows.
    pub(crate) last_indent_range: Option<(usize, usize)>,
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
    /// Soft tab stop in spaces. When `> 0`, Tab inserts spaces to the
    /// next softtabstop boundary (when `expandtab`), and Backspace at the
    /// end of a softtabstop-aligned space run deletes the entire run as
    /// if it were one tab. `0` disables. Matches vim's `:set softtabstop`.
    pub softtabstop: usize,
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
    /// When `true`, bumps indent by one `shiftwidth` after a line ending
    /// in `{` / `(` / `[`, and strips one indent unit when the user types
    /// `}` / `)` / `]` on a whitespace-only line. See `compute_enter_indent`
    /// in `vim.rs` for the tree-sitter plug-in seam. Default `true`.
    pub smartindent: bool,
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
    /// When true, render absolute line numbers in the gutter. Matches
    /// vim's `:set number` / `:set nu`. Default `true`.
    pub number: bool,
    /// When true, render line numbers as offsets from the cursor row.
    /// Combined with `number`, the cursor row shows its absolute number
    /// while other rows show the relative offset (vim's `nu+rnu` hybrid).
    /// Matches vim's `:set relativenumber` / `:set rnu`. Default `false`.
    pub relativenumber: bool,
    /// Minimum gutter width in cells for the line-number column.
    /// Width grows past this to fit the largest displayed number.
    /// Matches vim's `:set numberwidth` / `:set nuw`. Default `4`.
    /// Range 1..=20.
    pub numberwidth: usize,
    /// Highlight the row where the cursor sits. Matches vim's `:set cursorline`.
    /// Default `false`.
    pub cursorline: bool,
    /// Highlight the column where the cursor sits. Matches vim's `:set cursorcolumn`.
    /// Default `false`.
    pub cursorcolumn: bool,
    /// Sign-column display mode. Matches vim's `:set signcolumn`.
    /// Default [`crate::types::SignColumnMode::Auto`].
    pub signcolumn: crate::types::SignColumnMode,
    /// Number of cells reserved for a fold-marker gutter.
    /// Matches vim's `:set foldcolumn`. Default `0`.
    pub foldcolumn: u32,
    /// Comma-separated 1-based column indices for vertical rulers.
    /// Matches vim's `:set colorcolumn`. Default `""`.
    pub colorcolumn: String,
    /// Format options flags (subset of vim's `formatoptions`).
    /// `r` — auto-continue line comments on `<Enter>` in insert mode.
    /// `o` — auto-continue line comments on `o` / `O` in normal mode.
    /// Default: both on (`"ro"`).
    pub formatoptions: String,
    /// Active filetype (language name) for the current buffer.
    /// Used by comment-continuation and future language-aware features.
    /// Matches vim's `:set filetype` / `:set ft`. Default `""` (plain text).
    pub filetype: String,
    /// Override comment-string for the current buffer.
    ///
    /// When non-empty, used by `toggle_comment_range` instead of the
    /// per-filetype default from `hjkl_lang::comment::commentstring_for_lang`.
    /// Follows vim's `:set commentstring=…` — use `%s` as the text placeholder
    /// (e.g. `"// %s"`) for compatibility; the toggle strips/inserts only the
    /// prefix/suffix portion (before/after `%s`).  An empty string means "use
    /// the filetype default".  Default `""`.
    pub commentstring: String,
    /// When `true`, typing an opening bracket or quote automatically inserts
    /// the matching close character and parks the cursor between them.
    /// Matches vim's `set autopairs` (Neovim) / nvim-autopairs behaviour.
    /// Default `true`.
    pub autopair: bool,
    /// When `true`, typing `>` to close an HTML/XML opening tag automatically
    /// inserts `</tagname>` after the cursor. Only fires for filetypes in the
    /// HTML/XML family (`html`, `xml`, `svg`, `jsx`, `tsx`, `vue`, `svelte`).
    /// Matches common editor "autoclose tag" behaviour. Default: `true` for
    /// those filetypes (the caller gates on filetype), `true` stored here so
    /// `:set noautoclose-tag` can disable it globally.
    pub autoclose_tag: bool,
    /// Minimum context rows kept visible above/below the cursor when scrolling.
    /// Capped at (height - 1) / 2 for tiny viewports. `0` = no margin.
    /// Matches vim's `:set scrolloff` / `:set so`. Default `5`.
    pub scrolloff: usize,
    /// Minimum context columns kept visible left/right of the cursor (no-wrap
    /// mode only). `0` = no margin (vim default). Matches `:set sidescrolloff`.
    /// Default `0`.
    pub sidescrolloff: usize,
    /// Enable vim-sneak style two-char digraph jump via `s` (forward) and
    /// `S` (backward). When `true` (default), `s`/`S` no longer behave as
    /// vim's built-in substitute-char / substitute-line; `;`/`,` smart-fall-
    /// back to sneak-repeat when the last horizontal motion was a sneak.
    /// Set `:set nomotion_sneak` to revert `s`/`S` to stock vim behavior.
    /// Default `true` — **BREAKING** for users relying on `s` = substitute-char.
    pub motion_sneak: bool,
    /// Render invisible characters (tabs, trailing spaces, EOL markers).
    /// Matches vim's `:set list` / `:set nolist`. Default `false`.
    pub list: bool,
    /// Characters used to represent invisibles when `list` is on.
    /// Matches vim's `:set listchars` / `:set lcs`.
    pub listchars: crate::types::ListChars,
    /// Render thin vertical indent guides at every `shiftwidth`-aligned
    /// column. hjkl-specific. Default `true`.
    pub indent_guides: bool,
    /// Character used to draw indent guides. Default `'│'`.
    pub indent_guide_char: char,
    /// Enable inline color-literal preview. hjkl-specific. Default `true`.
    pub colorizer: bool,
    /// Filetype allowlist for the colorizer. Default CSS/template languages.
    pub colorizer_filetypes: Vec<String>,
    /// Run hjkl-mangler formatter before each `:w` save. Default `false`.
    pub format_on_save: bool,
    /// Strip trailing whitespace before each `:w` save. Default `false`.
    pub trim_trailing_whitespace: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            shiftwidth: 4,
            tabstop: 4,
            softtabstop: 4,
            ignore_case: true,
            smartcase: true,
            wrapscan: true,
            textwidth: 79,
            expandtab: true,
            wrap: hjkl_buffer::Wrap::None,
            readonly: false,
            autoindent: true,
            smartindent: true,
            undo_levels: 1000,
            undo_break_on_motion: true,
            iskeyword: "@,48-57,_,192-255".to_string(),
            timeout_len: core::time::Duration::from_millis(1000),
            number: true,
            relativenumber: false,
            numberwidth: 4,
            cursorline: false,
            cursorcolumn: false,
            signcolumn: crate::types::SignColumnMode::Auto,
            foldcolumn: 0,
            colorcolumn: String::new(),
            formatoptions: "ro".to_string(),
            filetype: String::new(),
            commentstring: String::new(),
            autopair: true,
            autoclose_tag: true,
            scrolloff: 5,
            sidescrolloff: 0,
            motion_sneak: true,
            list: false,
            listchars: crate::types::ListChars::default(),
            indent_guides: true,
            indent_guide_char: '│',
            colorizer: true,
            colorizer_filetypes: vec![
                "css".to_string(),
                "scss".to_string(),
                "sass".to_string(),
                "less".to_string(),
                "html".to_string(),
                "vue".to_string(),
                "svelte".to_string(),
                "tailwindcss".to_string(),
                "toml".to_string(),
                "lua".to_string(),
                "vim".to_string(),
            ],
            format_on_save: false,
            trim_trailing_whitespace: false,
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
        softtabstop: o.softtabstop as usize,
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
        smartindent: o.smartindent,
        undo_levels: o.undo_levels,
        undo_break_on_motion: o.undo_break_on_motion,
        iskeyword: o.iskeyword.clone(),
        timeout_len: o.timeout_len,
        number: o.number,
        relativenumber: o.relativenumber,
        numberwidth: o.numberwidth,
        cursorline: o.cursorline,
        cursorcolumn: o.cursorcolumn,
        signcolumn: o.signcolumn,
        foldcolumn: o.foldcolumn,
        colorcolumn: o.colorcolumn.clone(),
        formatoptions: o.formatoptions.clone(),
        filetype: o.filetype.clone(),
        commentstring: String::new(),
        autopair: true,
        autoclose_tag: true,
        scrolloff: o.scrolloff,
        sidescrolloff: o.sidescrolloff,
        motion_sneak: o.motion_sneak,
        list: o.list,
        listchars: o.listchars.clone(),
        indent_guides: o.indent_guides,
        indent_guide_char: o.indent_guide_char,
        colorizer: o.colorizer,
        colorizer_filetypes: o.colorizer_filetypes.clone(),
        format_on_save: o.format_on_save,
        trim_trailing_whitespace: o.trim_trailing_whitespace,
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
            style_table: Vec::new(),
            registers: crate::registers::Registers::default(),
            styled_spans: Vec::new(),
            settings,
            marks: std::collections::BTreeMap::new(),
            global_marks: std::collections::BTreeMap::new(),
            current_buffer_id: 0,
            syntax_fold_ranges: Vec::new(),
            change_log: Vec::new(),
            sticky_col: None,
            host,
            last_emitted_mode: crate::VimMode::Normal,
            search_state: crate::search::SearchState::new(),
            buffer_spans: Vec::new(),
            pending_content_edits: Vec::new(),
            pending_content_reset: false,
            last_indent_range: None,
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
    pub fn emit_cursor_shape_if_changed(&mut self) {
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

    /// Look up an uppercase global mark by letter. Returns
    /// `(buffer_id, row, col)` if set; `None` otherwise.
    pub fn global_mark(&self, c: char) -> Option<(u64, usize, usize)> {
        self.global_marks.get(&c).copied()
    }

    /// Set an uppercase global mark `c` to `(buffer_id, row, col)`.
    pub fn set_global_mark(&mut self, c: char, buffer_id: u64, pos: (usize, usize)) {
        self.global_marks.insert(c, (buffer_id, pos.0, pos.1));
    }

    /// Return the `buffer_id` this editor is currently attached to.
    pub fn current_buffer_id(&self) -> u64 {
        self.current_buffer_id
    }

    /// Update the `buffer_id` this editor is attached to. Called by the
    /// app on every `switch_to` so global-mark sets record the correct id.
    pub fn set_current_buffer_id(&mut self, id: u64) {
        self.current_buffer_id = id;
    }

    /// Iterate all global marks (`'A'`–`'Z'`), yielding
    /// `(mark_char, buffer_id, row, col)`.
    pub fn global_marks_iter(&self) -> impl Iterator<Item = (char, u64, usize, usize)> + '_ {
        self.global_marks
            .iter()
            .map(|(c, &(bid, r, col))| (*c, bid, r, col))
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

    /// Set the active filetype (language name) for the current buffer.
    /// Used by comment-continuation and future language-aware features.
    /// Equivalent to `:set filetype=<lang>`. Pass `""` to clear.
    pub fn set_filetype(&mut self, lang: &str) {
        self.settings.filetype = lang.to_string();
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

    /// Snapshot of the unnamed register (the default `p` / `P` source).
    pub fn yank(&self) -> &str {
        &self.registers.unnamed.text
    }

    /// Borrow the full register bank — `"`, `"0`–`"9`, `"a`–`"z`.
    pub fn registers(&self) -> &crate::registers::Registers {
        &self.registers
    }

    /// Mutably borrow the full register bank. Hosts that share registers
    /// across multiple editors (e.g. multi-buffer `yy` / `p`) overwrite
    /// the slots here on buffer switch.
    pub fn registers_mut(&mut self) -> &mut crate::registers::Registers {
        &mut self.registers
    }

    /// Host hook: load the OS clipboard's contents into the `"+` / `"*`
    /// register slot. the host calls this before letting vim consume a
    /// paste so `"*p` / `"+p` reflect the live clipboard rather than a
    /// stale snapshot from the last yank.
    pub fn sync_clipboard_register(&mut self, text: String, linewise: bool) {
        self.registers.set_clipboard(text, linewise);
    }

    /// Return the user's pending register selection (set via `"<reg>` chord
    /// before an operator). `None` if no register was selected — caller should
    /// use the unnamed register `"`.
    ///
    /// Read-only — does not consume / clear the pending selection. The
    /// register is cleared by the engine after the next operator fires.
    ///
    /// Promoted in 0.6.X for Phase 4e to let the App's visual-op dispatch arm
    /// honor `"a` + visual op chord sequences.
    pub fn pending_register(&self) -> Option<char> {
        self.vim.pending_register
    }

    /// True when the user's pending register selector is `+` or `*`.
    /// the host peeks this so it can refresh `sync_clipboard_register`
    /// only when a clipboard read is actually about to happen.
    pub fn pending_register_is_clipboard(&self) -> bool {
        matches!(self.vim.pending_register, Some('+') | Some('*'))
    }

    /// Register currently being recorded into via `q{reg}`. `None` when
    /// no recording is active. Hosts use this to surface a "recording @r"
    /// indicator in the status line.
    pub fn recording_register(&self) -> Option<char> {
        self.vim.recording_macro
    }

    /// Pending repeat count the user has typed but not yet resolved
    /// (e.g. pressing `5` before `d`). `None` when nothing is pending.
    /// Hosts surface this in a "showcmd" area.
    pub fn pending_count(&self) -> Option<u32> {
        self.vim.pending_count_val()
    }

    /// The operator character for any in-flight operator that is waiting
    /// for a motion (e.g. `d` after the user types `d` but before a
    /// motion). Returns `None` when no operator is pending.
    pub fn pending_op(&self) -> Option<char> {
        self.vim.pending_op_char()
    }

    /// `true` when the engine is in any pending chord state — waiting for
    /// the next key to complete a command (e.g. `r<char>` replace,
    /// `f<char>` find, `m<a>` set-mark, `'<a>` goto-mark, operator-pending
    /// after `d` / `c` / `y`, `g`-prefix continuation, `z`-prefix continuation,
    /// register selection `"<reg>`, macro recording target, etc).
    ///
    /// Hosts use this to bypass their own chord dispatch (keymap tries, etc.)
    /// and forward keys directly to the engine so in-flight commands can
    /// complete without the host eating their continuation keys.
    pub fn is_chord_pending(&self) -> bool {
        self.vim.is_chord_pending()
    }

    /// `true` when `insert_ctrl_r_arm()` has been called and the dispatcher
    /// is waiting for the next typed character to name the register to paste.
    /// The dispatcher should call `insert_paste_register(c)` instead of
    /// `insert_char(c)` for the next printable key, then the flag auto-clears.
    ///
    /// Phase 6.5: exposed so the app-level `dispatch_insert_key` can branch
    /// without having to drive the full FSM.
    pub fn is_insert_register_pending(&self) -> bool {
        self.vim.insert_pending_register
    }

    /// Clear the `Ctrl-R` register-paste pending flag. Call this immediately
    /// before `insert_paste_register(c)` in app-level dispatchers so that the
    /// flag does not persist into the next key. Call before
    /// `insert_paste_register_bridge` (which `hjkl_vim::insert` does).
    ///
    /// Phase 6.5: used by `dispatch_insert_key` in the app crate.
    pub fn clear_insert_register_pending(&mut self) {
        self.vim.insert_pending_register = false;
    }

    /// Read-only view of the jump-back list (positions pushed on "big"
    /// motions). Newest entry is at the back — `Ctrl-o` pops from there.
    #[allow(clippy::type_complexity)]
    pub fn jump_list(&self) -> (&[(usize, usize)], &[(usize, usize)]) {
        (&self.vim.jump_back, &self.vim.jump_fwd)
    }

    /// Read-only view of the change list (positions of recent edits) plus
    /// the current walk cursor. Newest entry is at the back.
    pub fn change_list(&self) -> (&[(usize, usize)], Option<usize>) {
        (&self.vim.change_list, self.vim.change_list_cursor)
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
    pub fn set_named_register_text(&mut self, reg: char, text: String) {
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
    /// [`crate::types::Style`]. Always available — engine is ratatui-free.
    /// Ratatui hosts use
    /// `hjkl_engine_tui::EditorRatatuiExt::install_ratatui_syntax_spans`
    /// which converts at the boundary and delegates here.
    ///
    /// Renamed from `install_engine_syntax_spans` in 0.0.32 — at the
    /// 0.1.0 freeze the unprefixed name is the universally-available
    /// engine-native variant.
    pub fn install_syntax_spans(&mut self, spans: Vec<Vec<(usize, usize, crate::types::Style)>>) {
        // Note: do NOT pre-collect `line_byte_lens` here. `buf_line` clones
        // the row string under a content-mutex lock; pre-collecting for
        // every row turns a 10k-row file's install into 10k mutex-locked
        // String clones (visible as j/k cursor lag). The typical install
        // has spans on at most a few hundred rows (the parsed viewport
        // window); lazy lookup keeps the cost proportional to populated
        // rows, not file size.
        let mut by_row: Vec<Vec<hjkl_buffer::Span>> = Vec::with_capacity(spans.len());
        let mut engine_spans: Vec<Vec<(usize, usize, crate::types::Style)>> =
            Vec::with_capacity(spans.len());
        for (row, row_spans) in spans.iter().enumerate() {
            if row_spans.is_empty() {
                by_row.push(Vec::new());
                engine_spans.push(Vec::new());
                continue;
            }
            let line_len = buf_line(&self.buffer, row).map(|s| s.len()).unwrap_or(0);
            let mut translated = Vec::with_capacity(row_spans.len());
            let mut translated_e = Vec::with_capacity(row_spans.len());
            for (start, end, style) in row_spans {
                let end_clamped = (*end).min(line_len);
                if end_clamped <= *start {
                    continue;
                }
                let id = self.intern_style(*style);
                translated.push(hjkl_buffer::Span::new(*start, end_clamped, id));
                translated_e.push((*start, end_clamped, *style));
            }
            by_row.push(translated);
            engine_spans.push(translated_e);
        }
        self.buffer_spans = by_row;
        self.styled_spans = engine_spans;
    }

    /// Patch only `rows` of the installed `buffer_spans` / `styled_spans`,
    /// leaving rows outside that range untouched. `spans` is indexed by
    /// row offset within `rows` — `spans[0]` is for `rows.start`,
    /// `spans[1]` for `rows.start + 1`, etc.
    ///
    /// Use this instead of [`Self::install_syntax_spans`] when a sync
    /// `query_viewport` produced spans for the visible region only.
    /// Walking the full `line_count` and re-installing every row on
    /// every j/k that nudges the viewport dominated the per-keystroke
    /// cost on large files; patching just the changed range keeps the
    /// cost proportional to viewport size, not file size.
    ///
    /// Ensures `buffer_spans` / `styled_spans` are sized to the buffer's
    /// current `line_count` (resizes if a row-count edit shifted them).
    pub fn patch_syntax_spans_range(
        &mut self,
        rows: std::ops::Range<usize>,
        spans: &[Vec<(usize, usize, crate::types::Style)>],
    ) {
        let line_count = buf_row_count(&self.buffer);
        if self.buffer_spans.len() != line_count {
            self.buffer_spans.resize_with(line_count, Vec::new);
        }
        if self.styled_spans.len() != line_count {
            self.styled_spans.resize_with(line_count, Vec::new);
        }
        for (i, row_spans) in spans.iter().enumerate() {
            let row = rows.start + i;
            if row >= line_count {
                break;
            }
            if row_spans.is_empty() {
                self.buffer_spans[row] = Vec::new();
                self.styled_spans[row] = Vec::new();
                continue;
            }
            let line_len = buf_line(&self.buffer, row).map(|s| s.len()).unwrap_or(0);
            let mut translated = Vec::with_capacity(row_spans.len());
            let mut translated_e = Vec::with_capacity(row_spans.len());
            for (start, end, style) in row_spans {
                let end_clamped = (*end).min(line_len);
                if end_clamped <= *start {
                    continue;
                }
                let id = self.intern_style(*style);
                translated.push(hjkl_buffer::Span::new(*start, end_clamped, id));
                translated_e.push((*start, end_clamped, *style));
            }
            self.buffer_spans[row] = translated;
            self.styled_spans[row] = translated_e;
        }
    }

    /// Translate the cached `buffer_spans` / `styled_spans` row indices
    /// in-place to track a batch of [`crate::types::ContentEdit`]s without
    /// blanking the cache.
    ///
    /// Why: spans are installed by the async syntax worker, which can lag
    /// the buffer by one or more frames after an edit. If the edit changes
    /// the row count and we keep the old span rows in place, the renderer
    /// paints last-frame's spans at the wrong line — visibly garbled colours.
    /// The historical fix was to blank `buffer_spans` whenever a row-count
    /// change came through, but that produces a white flash on every Enter
    /// or backspace-at-BOL.
    ///
    /// What this does instead: for each edit, insert empty span rows where
    /// the edit grew the buffer and drain rows where it shrank, so the
    /// surviving rows still index the right line. Spans on the edited row
    /// itself stay (they'll show stale colours for that one row until the
    /// worker delivers a fresh parse, which is invisible compared to the
    /// blank flash).
    ///
    /// Edits are applied in order — each edit's `(row, col)` positions are
    /// taken to be relative to the post-state of the prior edits in the
    /// batch (matching the order the engine emitted them).
    pub fn shift_syntax_spans_for_edits(&mut self, edits: &[crate::types::ContentEdit]) {
        for edit in edits {
            let oer = edit.old_end_position.0 as usize;
            let ner = edit.new_end_position.0 as usize;
            if ner == oer {
                continue;
            }
            let start_row = edit.start_position.0 as usize;
            let start_col = edit.start_position.1 as usize;
            // Insert/drain index depends on whether the edit starts at
            // the BEGINNING of `start_row` or somewhere INSIDE it.
            //   col == 0 → edit is at the very start of `start_row`; new
            //              rows go BEFORE row `start_row`, so the affected
            //              indices begin AT `start_row`.
            //   col > 0 → edit is inside `start_row`; new rows go AFTER
            //              `start_row`, so affected indices begin at
            //              `start_row + 1`.
            //
            // Pre-fix this always used `oer + 1` (the col-> 0 branch),
            // which left row `start_row`'s spans at its old index while
            // the file's row `start_row` was now the freshly-pasted
            // content — visible as wrong-row colour mappings after
            // `ggP` / `P` / any insert at column 0.
            let affected_idx = if start_col == 0 {
                start_row
            } else {
                start_row + 1
            };
            if ner > oer {
                let n = ner - oer;
                // O(len + n) via splice; the prior per-row `insert(idx, ...)`
                // loop was O(n × (len - idx)), which on a 60k-row paste at
                // the BOL became ~1.8 G memmove ops (87 % of paste CPU per
                // samply). Splice memmove-shifts once, then fills.
                let idx = affected_idx.min(self.buffer_spans.len());
                self.buffer_spans
                    .splice(idx..idx, std::iter::repeat_with(Vec::new).take(n));
                let idx_s = affected_idx.min(self.styled_spans.len());
                self.styled_spans
                    .splice(idx_s..idx_s, std::iter::repeat_with(Vec::new).take(n));
            } else {
                let n = oer - ner;
                let len_b = self.buffer_spans.len();
                let start_b = affected_idx.min(len_b);
                let end_b = (start_b + n).min(len_b);
                if end_b > start_b {
                    self.buffer_spans.drain(start_b..end_b);
                }
                let len_s = self.styled_spans.len();
                let start_s = affected_idx.min(len_s);
                let end_s = (start_s + n).min(len_s);
                if end_s > start_s {
                    self.styled_spans.drain(start_s..end_s);
                }
            }
        }
    }

    /// Read-only view of the style table in engine-native form —
    /// id `i` → `style_table[i]`. Always available, no cfg gate.
    ///
    /// Ratatui hosts that need a `ratatui::style::Style` slice should
    /// use `hjkl_engine_tui::EditorRatatuiExt::ratatui_style_table` or
    /// convert individual entries via `hjkl_engine_tui::style_to_ratatui`.
    pub fn style_table(&self) -> &[crate::types::Style] {
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
    /// Engine-native — the unified `style_table` is always engine-native.
    /// Linear-scan dedup — the table grows only as new tree-sitter token
    /// kinds appear, so it stays tiny. Ratatui callers use
    /// `hjkl_engine_tui::EditorRatatuiExt::intern_ratatui_style` which
    /// converts at the boundary and delegates here.
    ///
    /// Renamed from `intern_engine_style` in 0.0.32 — at 0.1.0 freeze
    /// the unprefixed name is the universally-available engine-native
    /// variant.
    pub fn intern_style(&mut self, style: crate::types::Style) -> u32 {
        if let Some(idx) = self.style_table.iter().position(|s| *s == style) {
            return idx as u32;
        }
        self.style_table.push(style);
        (self.style_table.len() - 1) as u32
    }

    /// Look up an interned style by id and return it as a SPEC
    /// [`crate::types::Style`]. Returns `None` for ids past the end
    /// of the table.
    pub fn engine_style_at(&self, id: u32) -> Option<crate::types::Style> {
        self.style_table.get(id as usize).copied()
    }

    /// Historical reverse-sync hook from when the textarea mirrored
    /// the buffer. Now that Buffer is the cursor authority this is a
    /// no-op; call sites can remain in place during the migration.
    pub fn push_buffer_cursor_to_textarea(&mut self) {}

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
    ///
    /// Resets `sticky_col` (curswant) to `col` — every explicit jump
    /// (goto-line, jump-to-mark, search hit, click, `]d`) follows vim
    /// semantics. Only `j`/`k`/`+`/`-` READ `sticky_col`; everything
    /// else resets it to the column where the cursor actually landed.
    pub fn jump_cursor(&mut self, row: usize, col: usize) {
        buf_set_cursor_rc(&mut self.buffer, row, col);
        self.sticky_col = Some(col);
    }

    /// Set the cursor to `(row, col)` without modifying `sticky_col`.
    ///
    /// Use this for host-side state restores (viewport sync, snapshot
    /// replay) where the cursor was already at this position semantically
    /// and the host's sticky tracking should remain authoritative.
    ///
    /// For user-facing jumps (goto-line, search hit, picker `<CR>`, `]d`,
    /// click), use [`Editor::jump_cursor`] which DOES reset `sticky_col`
    /// per vim curswant semantics.
    pub fn set_cursor_quiet(&mut self, row: usize, col: usize) {
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
    pub fn sync_buffer_from_textarea(&mut self) {
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
        // Capture the pre-edit cursor for the dot mark (`'.` / `` `. ``).
        // Vim's `:h '.` says "the position where the last change was made",
        // meaning the change-start, not the post-insert cursor. We snap it
        // here before `apply_buffer_edit` moves the cursor.
        let (pre_edit_row, pre_edit_col) = buf_cursor_rc(&self.buffer);
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
        // Dot mark records the PRE-edit position (change start), matching
        // vim's `:h '.` semantics. Previously this stored the post-edit
        // cursor, which diverged from nvim on `iX<Esc>j`.
        self.vim.last_edit_pos = Some((pre_edit_row, pre_edit_col));
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

        // Shift global marks that belong to the current buffer.
        let cur_bid = self.current_buffer_id;
        let mut global_to_drop: Vec<char> = Vec::new();
        for (c, (bid, row, _col)) in self.global_marks.iter_mut() {
            if *bid != cur_bid {
                continue;
            }
            if (edit_start..drop_end).contains(row) {
                global_to_drop.push(*c);
            } else if *row >= shift_threshold {
                *row = ((*row as isize) + delta).max(0) as usize;
            }
        }
        for c in global_to_drop {
            self.global_marks.remove(&c);
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

    /// Width in cells of the line-number gutter for the current buffer
    /// and settings. Matches what [`Editor::cursor_screen_pos`] reserves
    /// in front of the text column. Returns `0` when both `number` and
    /// `relativenumber` are off.
    pub fn lnum_width(&self) -> u16 {
        if self.settings.number || self.settings.relativenumber {
            let needed = buf_row_count(&self.buffer).to_string().len() + 1;
            needed.max(self.settings.numberwidth) as u16
        } else {
            0
        }
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
    /// Accounts for line-number gutter, viewport scroll, and any extra
    /// gutter width to the left of the number column (sign column, fold
    /// column). Returns `None` if the cursor is outside the visible
    /// viewport. Always available (engine-native; no ratatui dependency).
    ///
    /// `extra_gutter_width` is added to the number-column width before
    /// computing the cursor x position. Callers (e.g. `apps/hjkl/src/render.rs`)
    /// pass `sign_w + fold_w` here so the cursor lands on the correct cell
    /// when a dedicated sign or fold column is present.
    ///
    /// Renamed from `cursor_screen_pos_xywh` in 0.0.32.
    pub fn cursor_screen_pos(
        &self,
        area_x: u16,
        area_y: u16,
        area_width: u16,
        area_height: u16,
        extra_gutter_width: u16,
    ) -> Option<(u16, u16)> {
        let (pos_row, pos_col) = buf_cursor_rc(&self.buffer);
        let v = self.host.viewport();
        if pos_row < v.top_row || pos_col < v.top_col {
            return None;
        }
        let lnum_width = self.lnum_width();
        // Full offset from the left edge of the window to the first text cell.
        let gutter_total = lnum_width + extra_gutter_width;
        let dy = (pos_row - v.top_row) as u16;
        // Convert char column to visual column so cursor lands on the
        // correct cell when the line contains tabs (which the renderer
        // expands to TAB_WIDTH stops). Tab width must match the renderer.
        let cursor_rope = self.buffer.rope();
        let pos_row_safe = pos_row.min(cursor_rope.len_lines().saturating_sub(1));
        let line = hjkl_buffer::rope_line_str(&cursor_rope, pos_row_safe);
        let tab_width = if v.tab_width == 0 {
            4
        } else {
            v.tab_width as usize
        };
        let visual_pos = visual_col_for_char(&line, pos_col, tab_width);
        let visual_top = visual_col_for_char(&line, v.top_col, tab_width);
        let dx = (visual_pos - visual_top) as u16;
        if dy >= area_height || dx + gutter_total >= area_width {
            return None;
        }
        Some((area_x + gutter_total + dx, area_y + dy))
    }

    /// Returns the current vim mode. Phase 6.3: reads from the stable
    /// `current_mode` field (kept in sync by both the FSM step loop and
    /// the Phase 6.3 primitive bridges) rather than deriving from the
    /// FSM-internal `mode` field via `public_mode()`.
    pub fn vim_mode(&self) -> VimMode {
        self.vim.current_mode
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

    /// The most recent successful `:s` command. `None` before the first substitute.
    /// Used by `:&` / `:&&` to repeat it.
    pub fn last_substitute(&self) -> Option<&crate::substitute::SubstituteCmd> {
        self.vim.last_substitute.as_ref()
    }

    /// Store the last successful substitute so `:&` / `:&&` can repeat it.
    pub fn set_last_substitute(&mut self, cmd: crate::substitute::SubstituteCmd) {
        self.vim.last_substitute = Some(cmd);
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
            s.push_str(&crate::types::Query::line(&self.buffer, r as u32));
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

    /// Whole-buffer replace that **preserves the undo history**.
    ///
    /// Equivalent to [`Editor::set_content`] but pushes the current buffer
    /// state onto the undo stack first, so a subsequent `u` walks back to
    /// the pre-replacement content. Use this for any operation the user
    /// expects to undo as a single step — e.g. external formatter output
    /// (`hjkl-mangler`) installed via the async [`crate::app::FormatWorker`].
    ///
    /// Like `push_undo`, this clears the redo stack (vim semantics: any
    /// new edit invalidates redo).
    pub fn set_content_undoable(&mut self, text: &str) {
        self.push_undo();
        let mut lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
        while lines.last().map(|l| l.is_empty()).unwrap_or(false) {
            lines.pop();
        }
        if lines.is_empty() {
            lines.push(String::new());
        }
        let _ = lines;
        crate::types::BufferEdit::replace_all(&mut self.buffer, text);
        // Whole-buffer replace supersedes any queued ContentEdits.
        self.pending_content_edits.clear();
        self.pending_content_reset = true;
        self.mark_content_dirty();
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
            softtabstop: self.settings.softtabstop as u32,
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
            smartindent: self.settings.smartindent,
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
        self.settings.softtabstop = opts.softtabstop as usize;
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
        self.settings.smartindent = opts.smartindent;
        self.settings.undo_levels = opts.undo_levels;
        self.settings.undo_break_on_motion = opts.undo_break_on_motion;
        self.set_iskeyword(opts.iskeyword.clone());
        self.settings.timeout_len = opts.timeout_len;
        self.settings.number = opts.number;
        self.settings.relativenumber = opts.relativenumber;
        self.settings.numberwidth = opts.numberwidth;
        self.settings.cursorline = opts.cursorline;
        self.settings.cursorcolumn = opts.cursorcolumn;
        self.settings.signcolumn = opts.signcolumn;
        self.settings.foldcolumn = opts.foldcolumn;
        self.settings.colorcolumn = opts.colorcolumn.clone();
        self.settings.scrolloff = opts.scrolloff;
        self.settings.sidescrolloff = opts.sidescrolloff;
        self.settings.list = opts.list;
        self.settings.listchars = opts.listchars.clone();
        self.settings.colorizer = opts.colorizer;
        self.settings.colorizer_filetypes = opts.colorizer_filetypes.clone();
        self.settings.format_on_save = opts.format_on_save;
        self.settings.trim_trailing_whitespace = opts.trim_trailing_whitespace;
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
            use crate::search::{CaseMode, resolve_case_mode};
            let base =
                CaseMode::from_options(self.settings().ignore_case, self.settings().smartcase);
            let (stripped, mode) = resolve_case_mode(&prompt.text, base);
            let src = if mode == CaseMode::Insensitive {
                format!("(?i){stripped}")
            } else {
                stripped
            };
            let Ok(re) = regex::Regex::new(&src) else {
                return Vec::new();
            };
            let Some(haystack) = buf_line(&self.buffer, row) else {
                return Vec::new();
            };
            return re
                .find_iter(&haystack)
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
        let rope = crate::types::Query::rope(&self.buffer);
        let lines: Vec<String> = (0..rope.len_lines())
            .map(|r| {
                let s = rope.line(r).to_string();
                if s.ends_with('\n') {
                    s[..s.len() - 1].to_string()
                } else {
                    s
                }
            })
            .collect();
        let viewport_top = self.host.viewport().top_row as u32;
        let marks = self
            .marks
            .iter()
            .map(|(c, (r, col))| (*c, (*r as u32, *col as u32)))
            .collect();
        let global_marks = self
            .global_marks
            .iter()
            .map(|(c, &(bid, r, col))| (*c, (bid, r as u32, col as u32)))
            .collect();
        EditorSnapshot {
            version: EditorSnapshot::VERSION,
            mode,
            cursor,
            lines,
            viewport_top,
            registers: self.registers.clone(),
            marks,
            global_marks,
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
        self.global_marks = snap
            .global_marks
            .into_iter()
            .map(|(c, (bid, r, col))| (c, (bid, r as usize, col as usize)))
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

    /// Scroll the viewport right by `cols` columns. Only the horizontal
    /// offset (`top_col`) moves — the cursor is NOT adjusted (matches
    /// vim's `zl` behaviour for horizontal scroll without wrap).
    pub fn scroll_right(&mut self, cols: i16) {
        let vp = self.host.viewport_mut();
        let cols_i = cols as isize;
        let new_top = (vp.top_col as isize + cols_i).max(0) as usize;
        vp.top_col = new_top;
    }

    /// Scroll the viewport left by `cols` columns. Delegates to
    /// `scroll_right` with a negated argument so the floor-at-zero
    /// clamp is shared.
    pub fn scroll_left(&mut self, cols: i16) {
        self.scroll_right(-cols);
    }

    /// Scroll the viewport so the cursor stays at least `scrolloff`
    /// rows from each edge. Replaces the bare
    /// `Buffer::ensure_cursor_visible` call at end-of-step so motions
    /// don't park the cursor on the very last visible row.
    pub fn ensure_cursor_in_scrolloff(&mut self) {
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
        let margin = self.settings.scrolloff.min(height.saturating_sub(1) / 2);
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
        // Apply scrolloff: keep the cursor at least scrolloff rows
        // from the visible viewport edges.
        let (cursor_row, cursor_col) = buf_cursor_rc(&self.buffer);
        let margin = self.settings.scrolloff.min(height / 2);
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
        // Vim: `:N` / `+N` jump scrolls the viewport too — without this
        // the cursor lands off-screen and the user has to scroll
        // manually to see it.
        self.ensure_cursor_in_scrolloff();
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
        let margin = self.settings.scrolloff.min(height.saturating_sub(1) / 2);
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

    // ── Host-agnostic doc-coord mouse primitives (Phase 1 of issue #114) ─────
    //
    // These primitives operate on document (row, col) coordinates that the HOST
    // computes from its own layout knowledge (cell geometry for the TUI host,
    // pixel geometry for the future GUI host). The engine has no u16 terminal
    // assumption here — it just moves the cursor in doc-space.

    /// Set the cursor to the given doc-space `(row, col)`, clamped to the
    /// document bounds. Hosts use this for programmatic cursor placement and
    /// as the building block for the mouse-click path.
    ///
    /// `col` may equal `line.chars().count()` (Insert-mode "one past end"
    /// position); values beyond that are clamped to `char_count`.
    pub fn set_cursor_doc(&mut self, row: usize, col: usize) {
        let max_row = buf_row_count(&self.buffer).saturating_sub(1);
        let r = row.min(max_row);
        let line_len = buf_line(&self.buffer, r)
            .map(|l| l.chars().count())
            .unwrap_or(0);
        let c = col.min(line_len);
        buf_set_cursor_rc(&mut self.buffer, r, c);
    }

    /// Handle a left-button click at doc-space `(row, col)`.
    ///
    /// Exits Visual mode if active, breaks the insert-mode undo group (Vim
    /// parity for `undo_break_on_motion`), then moves the cursor. The host
    /// performs cell→doc or pixel→doc translation before calling this.
    ///
    /// Mode-aware EOL clamp (neovim parity): in Normal / Visual modes the
    /// cursor lives on chars and never on the implicit `\n` — `col` is
    /// capped at `line.chars().count().saturating_sub(1)`. Insert mode
    /// allows the one-past-EOL insert position (`col == chars().count()`).
    ///
    /// Resets `sticky_col` to the clicked column so the next `j`/`k`
    /// motion uses the clicked column as the intended visual column
    /// (otherwise the cursor would snap back to the keyboard-tracked
    /// column on the first vertical motion after a click).
    pub fn mouse_click_doc(&mut self, row: usize, col: usize) {
        if self.vim.is_visual() {
            self.vim.force_normal();
        }
        // Mouse-position click counts as a motion — break the active
        // insert-mode undo group when the toggle is on (vim parity).
        crate::vim::break_undo_group_in_insert(self);

        let max_row = buf_row_count(&self.buffer).saturating_sub(1);
        let r = row.min(max_row);
        let line_len = buf_line(&self.buffer, r)
            .map(|l| l.chars().count())
            .unwrap_or(0);
        let cap = if self.vim.current_mode == crate::VimMode::Insert {
            line_len
        } else {
            line_len.saturating_sub(1)
        };
        let c = col.min(cap);
        buf_set_cursor_rc(&mut self.buffer, r, c);
        self.sticky_col = Some(c);
    }

    /// Begin a mouse-drag selection: anchor at the current cursor and enter
    /// Visual-char mode. Idempotent if already in Visual-char mode.
    pub fn mouse_begin_drag(&mut self) {
        if !self.vim.is_visual_char() {
            vim::enter_visual_char_bridge(self);
        }
    }

    /// Extend an in-progress mouse drag to doc-space `(row, col)`.
    ///
    /// Moves the live cursor; the Visual anchor stays where
    /// [`Editor::mouse_begin_drag`] set it. Call after the host has
    /// translated the drag position to doc coordinates.
    pub fn mouse_extend_drag_doc(&mut self, row: usize, col: usize) {
        self.set_cursor_doc(row, col);
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
        let line = buf_line(&self.buffer, cursor_row).unwrap_or_default();
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

    /// Capture the buffer state for undo / redo.  Uses
    /// [`Query::content_joined`], which the `Buffer` impl caches as an
    /// `Arc<String>` against `dirty_gen` — so when LSP / git / syntax
    /// already joined this generation, the snapshot is an `Arc::clone`
    /// (one ptr bump). Previously this cloned every line into a
    /// `Vec<String>` (162 k allocations on a 162 k-row buffer) and the
    /// matching `restore` re-joined them — samply showed it at ~9 % of
    /// CPU on a big-paste session.
    pub(super) fn snapshot(&self) -> (ropey::Rope, (usize, usize)) {
        use crate::types::Query;
        let rc = buf_cursor_rc(&self.buffer);
        (Query::rope(&self.buffer), rc)
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

    /// Undo `n` steps. Returns the number of steps actually applied
    /// (bounded by undo stack size).
    pub fn earlier_by_steps(&mut self, n: usize) -> usize {
        let mut count = 0;
        for _ in 0..n {
            if self.undo_stack.is_empty() {
                break;
            }
            crate::vim::do_undo(self);
            count += 1;
        }
        count
    }

    /// Redo `n` steps. Returns the number of steps actually applied
    /// (bounded by redo stack size).
    pub fn later_by_steps(&mut self, n: usize) -> usize {
        let mut count = 0;
        for _ in 0..n {
            if self.redo_stack.is_empty() {
                break;
            }
            crate::vim::do_redo(self);
            count += 1;
        }
        count
    }

    /// Undo back until the next-to-pop entry's timestamp is at or before
    /// `target`. Entries whose timestamp is strictly greater than `target`
    /// are popped (undone). Returns the number of steps applied.
    ///
    /// Vim `:earlier Ns` semantics: `target = SystemTime::now() - N seconds`.
    pub fn earlier_by_time(&mut self, target: SystemTime) -> usize {
        let mut count = 0;
        loop {
            match self.undo_stack.last() {
                None => break,
                Some(entry) => {
                    if entry.timestamp <= target {
                        break;
                    }
                }
            }
            crate::vim::do_undo(self);
            count += 1;
        }
        count
    }

    /// Redo forward while the next-to-pop redo entry's timestamp is at
    /// or before `target`. Returns the number of steps applied.
    ///
    /// Vim `:later Ns` semantics: `target = current_state_time + N seconds`.
    pub fn later_by_time(&mut self, target: SystemTime) -> usize {
        let mut count = 0;
        loop {
            match self.redo_stack.last() {
                None => break,
                Some(entry) => {
                    if entry.timestamp > target {
                        break;
                    }
                }
            }
            crate::vim::do_redo(self);
            count += 1;
        }
        count
    }

    /// Snapshot current buffer state onto the undo stack and clear
    /// the redo stack. Bounded by `settings.undo_levels` — older
    /// entries pruned. Call before any group of buffer mutations the
    /// user might want to undo as a single step.
    pub fn push_undo(&mut self) {
        self.push_undo_at(SystemTime::now());
    }

    /// Like [`push_undo`] but uses a caller-supplied timestamp. Used by
    /// tests that need deterministic time values without `sleep`.
    #[doc(hidden)]
    pub fn push_undo_at(&mut self, timestamp: SystemTime) {
        let (rope, cursor) = self.snapshot();
        self.undo_stack.push(UndoEntry {
            rope,
            cursor,
            timestamp,
        });
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
    ///
    /// Emits a single whole-buffer `ContentEdit` describing the
    /// transition so the syntax layer can apply it as an `InputEdit`
    /// on the retained tree and run an INCREMENTAL parse — tree-sitter
    /// reuses unchanged subtrees and `Tree::changed_ranges` reports
    /// just the bytes that differ, which lets the install path walk
    /// only the changed rows instead of the full viewport. Big undos
    /// that revert a large paste now refresh in ~1ms per affected
    /// row instead of a ~30ms full-viewport sync walk.
    pub fn restore(&mut self, lines: Vec<String>, cursor: (usize, usize)) {
        let text = lines.join("\n");
        self.restore_text(&text, cursor);
    }

    /// Restore the buffer from a `ropey::Rope` snapshot. Used by undo /
    /// redo: snapshots are stored as `Rope` (O(1) Arc-clone via
    /// `Buffer::rope()`), so this avoids the full-document `to_string`
    /// materialization that the old `Arc<String>` snapshot path forced
    /// on every undo group boundary.
    ///
    /// Internally materializes the rope to a `String` for `restore_text`
    /// — paying the cost on the restore side instead of the snapshot
    /// side trades one ~3 MB build per undo for none-per-snapshot. Undo
    /// is user-initiated and rare; snapshots fire on every `i` / `o`.
    pub fn restore_rope(&mut self, rope: ropey::Rope, cursor: (usize, usize)) {
        let text = rope.to_string();
        self.restore_text(&text, cursor);
    }

    fn restore_text(&mut self, text: &str, cursor: (usize, usize)) {
        // Diff the old rope (O(1) Arc-clone) against the incoming text
        // to emit a minimal ContentEdit — without it the syntax layer's
        // tree.edit() marks the whole document changed and tree-sitter
        // cold-parses on every undo.
        let old_rope = self.buffer.rope();
        let edit = minimal_content_edit_rope(&old_rope, text);

        crate::types::BufferEdit::replace_all(&mut self.buffer, text);
        buf_set_cursor_rc(&mut self.buffer, cursor.0, cursor.1);

        // Bulk replace supersedes any prior queued edits.
        self.pending_content_edits.clear();
        self.pending_content_edits.push(edit);
        self.mark_content_dirty();
    }

    /// Returns true if the key was consumed by the editor.
    /// Replace the char under the cursor with `ch`, `count` times. Matches
    /// vim `r<x>` semantics: cursor ends on the last replaced char, undo
    /// snapshot taken once at start. Promoted to public surface in 0.5.5
    /// so hjkl-vim's pending-state reducer can dispatch `Replace` without
    /// re-entering the FSM.
    pub fn replace_char_at(&mut self, ch: char, count: usize) {
        vim::replace_char(self, ch, count);
    }

    /// Apply vim's `f<x>` / `F<x>` / `t<x>` / `T<x>` motion. Moves the cursor
    /// to the `count`-th occurrence of `ch` on the current line, respecting
    /// `forward` (direction) and `till` (stop one char before target).
    /// Records `last_find` so `;` / `,` repeat work.
    ///
    /// No-op if the target char isn't on the current line within range.
    /// Cursor / scroll / sticky-col semantics match `f<x>` via `execute_motion`.
    pub fn find_char(&mut self, ch: char, forward: bool, till: bool, count: usize) {
        vim::apply_find_char(self, ch, forward, till, count.max(1));
    }

    /// Apply the g-chord effect for `g<ch>` with a pre-captured `count`.
    /// Mirrors the full `handle_after_g` dispatch table — `gg`, `gj`, `gk`,
    /// `gv`, `gU` / `gu` / `g~` (→ operator-pending), `gi`, `g*`, `g#`, etc.
    ///
    /// Promoted to public surface in 0.5.10 so hjkl-vim's
    /// `PendingState::AfterG` reducer can dispatch `AfterGChord` without
    /// re-entering the engine FSM.
    pub fn after_g(&mut self, ch: char, count: usize) {
        vim::apply_after_g(self, ch, count);
    }

    /// Apply the z-chord effect for `z<ch>` with a pre-captured `count`.
    /// Mirrors the full `handle_after_z` dispatch table — `zz` / `zt` / `zb`
    /// (scroll-cursor), `zo` / `zc` / `za` / `zR` / `zM` / `zE` / `zd`
    /// (fold ops), and `zf` (fold-add over visual selection or → op-pending).
    ///
    /// Promoted to public surface in 0.5.11 so hjkl-vim's
    /// `PendingState::AfterZ` reducer can dispatch `AfterZChord` without
    /// re-entering the engine FSM.
    pub fn after_z(&mut self, ch: char, count: usize) {
        vim::apply_after_z(self, ch, count);
    }

    /// Apply an operator over a single-key motion. `op` is the engine `Operator`
    /// and `motion_key` is the raw character (e.g. `'w'`, `'$'`, `'G'`). The
    /// engine resolves the char to a [`vim::Motion`] via `parse_motion`, applies
    /// the vim quirks (`cw` → `ce`, `cW` → `cE`, `FindRepeat` → stored find),
    /// then calls `apply_op_with_motion`. `total_count` is already the product of
    /// the prefix count and any inner count accumulated by the reducer.
    ///
    /// No-op when `motion_key` does not map to a known motion (engine silently
    /// cancels the operator, matching vim's behaviour on unknown motions).
    ///
    /// Promoted to the public surface in 0.5.12 so the hjkl-vim
    /// `PendingState::AfterOp` reducer can dispatch `ApplyOpMotion` without
    /// re-entering the engine FSM.
    pub fn apply_op_motion(
        &mut self,
        op: crate::vim::Operator,
        motion_key: char,
        total_count: usize,
    ) {
        vim::apply_op_motion_key(self, op, motion_key, total_count);
    }

    /// Apply a doubled-letter line op (`dd` / `yy` / `cc` / `>>` / `<<`).
    /// `total_count` is the product of prefix count and inner count.
    ///
    /// Promoted to the public surface in 0.5.12 so the hjkl-vim
    /// `PendingState::AfterOp` reducer can dispatch `ApplyOpDouble` without
    /// re-entering the engine FSM.
    pub fn apply_op_double(&mut self, op: crate::vim::Operator, total_count: usize) {
        vim::apply_op_double(self, op, total_count);
    }

    /// Apply an operator over a find motion (`df<x>` / `dF<x>` / `dt<x>` /
    /// `dT<x>`). Builds `Motion::Find { ch, forward, till }`, applies it via
    /// `apply_op_with_motion`, records `last_find` for `;` / `,` repeat, and
    /// updates `last_change` when `op` is Change (for dot-repeat).
    ///
    /// `total_count` is the product of prefix count and any inner count
    /// accumulated by the reducer — already folded at transition time.
    ///
    /// Promoted to the public surface in 0.5.14 so the hjkl-vim
    /// `PendingState::OpFind` reducer can dispatch `ApplyOpFind` without
    /// re-entering the engine FSM. `handle_op_find_target` (used by the
    /// chord-init op path) delegates here to avoid logic duplication.
    pub fn apply_op_find(
        &mut self,
        op: crate::vim::Operator,
        ch: char,
        forward: bool,
        till: bool,
        total_count: usize,
    ) {
        vim::apply_op_find_motion(self, op, ch, forward, till, total_count);
    }

    /// Apply an operator over a text-object range (`diw` / `daw` / `di"` etc.).
    /// Maps `ch` to a `TextObject` per the standard vim table, calls
    /// `apply_op_with_text_object`, and records `last_change` when `op` is
    /// Change (dot-repeat). Unknown `ch` values are silently ignored (no-op),
    /// matching the engine FSM's behaviour on unrecognised text-object chars.
    ///
    /// `total_count` is accepted for API symmetry with `apply_op_motion` /
    /// `apply_op_find` but is currently unused — text objects don't repeat in
    /// vim's current grammar. Kept for future-proofing.
    ///
    /// Promoted to the public surface in 0.5.15 so the hjkl-vim
    /// `PendingState::OpTextObj` reducer can dispatch `ApplyOpTextObj` without
    /// re-entering the engine FSM. `handle_text_object` (chord-init op path)
    /// delegates to the shared `apply_op_text_obj_inner` helper to avoid logic
    /// duplication.
    pub fn apply_op_text_obj(
        &mut self,
        op: crate::vim::Operator,
        ch: char,
        inner: bool,
        total_count: usize,
    ) {
        vim::apply_op_text_obj_inner(self, op, ch, inner, total_count);
    }

    /// Apply an operator over a g-chord motion or case-op linewise form
    /// (`dgg` / `dge` / `dgE` / `dgj` / `dgk` / `gUgU` etc.).
    ///
    /// - If `op` is Uppercase/Lowercase/ToggleCase and `ch` matches the op's
    ///   letter (`U`/`u`/`~`), executes the line op (linewise form).
    /// - Otherwise maps `ch` to a motion:
    ///   - `'g'` → `Motion::FileTop` (gg)
    ///   - `'e'` → `Motion::WordEndBack` (ge)
    ///   - `'E'` → `Motion::BigWordEndBack` (gE)
    ///   - `'j'` → `Motion::ScreenDown` (gj)
    ///   - `'k'` → `Motion::ScreenUp` (gk)
    ///   - unknown → no-op (silently ignored, matching engine FSM behaviour)
    /// - Updates `last_change` for dot-repeat when `op` is a change operator.
    ///
    /// `total_count` is the already-folded product of prefix and inner counts.
    ///
    /// Promoted to the public surface in 0.5.16 so the hjkl-vim
    /// `PendingState::OpG` reducer can dispatch `ApplyOpG` without
    /// re-entering the engine FSM. `handle_op_after_g` (chord-init op path)
    /// delegates to the shared `apply_op_g_inner` helper to avoid logic
    /// duplication.
    pub fn apply_op_g(&mut self, op: crate::vim::Operator, ch: char, total_count: usize) {
        vim::apply_op_g_inner(self, op, ch, total_count);
    }

    // ─── Range-query helpers for partial-format dispatch (#119) ─────────────

    /// Dry-run `motion_key` and return `(min_row, max_row)` between the cursor
    /// row and the motion's target row. Used by the app layer to compute the
    /// [`hjkl_mangler::RangeSpec`] for `=<motion>` before submitting the async
    /// format job.
    ///
    /// Returns `None` when `motion_key` does not map to a known motion (same
    /// condition that makes `apply_op_motion` a no-op).
    ///
    /// The cursor is restored to its original position after the probe —
    /// the buffer content is not touched.
    pub fn range_for_op_motion(
        &mut self,
        motion_key: char,
        total_count: usize,
    ) -> Option<(usize, usize)> {
        let start = self.cursor();
        // Reuse the same logic as apply_op_motion_key but only read the
        // target row — we parse the motion, apply it to move the cursor,
        // then immediately restore.
        let input = crate::input::Input {
            key: crate::input::Key::Char(motion_key),
            ctrl: false,
            alt: false,
            shift: false,
        };
        let motion = vim::parse_motion(&input)?;
        // Resolve FindRepeat and cw/cW quirks just like apply_op_motion_key.
        let motion = match motion {
            vim::Motion::FindRepeat { reverse } => match self.vim.last_find {
                Some((ch, forward, till)) => vim::Motion::Find {
                    ch,
                    forward: if reverse { !forward } else { forward },
                    till,
                },
                None => return None,
            },
            m => m,
        };
        vim::apply_motion_cursor_ctx(self, &motion, total_count, true);
        let end = self.cursor();
        // Restore cursor.
        buf_set_cursor_rc(&mut self.buffer, start.0, start.1);
        let (r0, r1) = (start.0.min(end.0), start.0.max(end.0));
        Some((r0, r1))
    }

    /// Dry-run a `g`-prefixed motion and return `(min_row, max_row)`. Used for
    /// `=gg` / `=gj` etc. Returns `None` for unknown `ch` values or case-op
    /// linewise forms that don't map to a row range.
    ///
    /// The cursor is restored after the probe.
    pub fn range_for_op_g(&mut self, ch: char, total_count: usize) -> Option<(usize, usize)> {
        let start = self.cursor();
        let motion = match ch {
            'g' => vim::Motion::FileTop,
            'e' => vim::Motion::WordEndBack,
            'E' => vim::Motion::BigWordEndBack,
            'j' => vim::Motion::ScreenDown,
            'k' => vim::Motion::ScreenUp,
            _ => return None,
        };
        vim::apply_motion_cursor_ctx(self, &motion, total_count, true);
        let end = self.cursor();
        buf_set_cursor_rc(&mut self.buffer, start.0, start.1);
        let (r0, r1) = (start.0.min(end.0), start.0.max(end.0));
        Some((r0, r1))
    }

    /// Dry-run a text-object lookup and return `(min_row, max_row)` for the
    /// matched region. Returns `None` when `ch` is not a known text-object
    /// kind or the text object could not be resolved (e.g. no enclosing bracket).
    ///
    /// The buffer is not mutated.
    pub fn range_for_op_text_obj(
        &self,
        ch: char,
        inner: bool,
        _total_count: usize,
    ) -> Option<(usize, usize)> {
        let obj = match ch {
            'w' => vim::TextObject::Word { big: false },
            'W' => vim::TextObject::Word { big: true },
            '"' | '\'' | '`' => vim::TextObject::Quote(ch),
            '(' | ')' | 'b' => vim::TextObject::Bracket('('),
            '[' | ']' => vim::TextObject::Bracket('['),
            '{' | '}' | 'B' => vim::TextObject::Bracket('{'),
            '<' | '>' => vim::TextObject::Bracket('<'),
            'p' => vim::TextObject::Paragraph,
            't' => vim::TextObject::XmlTag,
            's' => vim::TextObject::Sentence,
            _ => return None,
        };
        let (start, end, _kind) = vim::text_object_range(self, obj, inner)?;
        let (r0, r1) = (start.0.min(end.0), start.0.max(end.0));
        Some((r0, r1))
    }

    // ─── Phase 4a: pub range-mutation primitives (hjkl#70) ──────────────────
    //
    // These do not consume input — the caller (hjkl-vim's visual-mode operator
    // path, chunk 4e) has already resolved the range from the visual selection
    // before calling in. Normal-mode op dispatch continues to use
    // `apply_op_motion` / `apply_op_double` / `apply_op_find` / `apply_op_text_obj`.

    /// Delete the region `[start, end)` and stash the removed text in
    /// `register`. `'"'` selects the unnamed register (vim default); `'a'`–`'z'`
    /// select named registers.
    ///
    /// Pure range-mutation primitive — does not consume input. Called by
    /// hjkl-vim's visual-mode operator path which has already resolved the range
    /// from the visual selection.
    ///
    /// Promoted to the public surface in 0.6.7 for Phase 4 visual-mode op
    /// grammar migration (kryptic-sh/hjkl#70).
    pub fn delete_range(
        &mut self,
        start: (usize, usize),
        end: (usize, usize),
        kind: crate::vim::RangeKind,
        register: char,
    ) {
        vim::delete_range_bridge(self, start, end, kind, register);
    }

    /// Yank (copy) the region `[start, end)` into `register` without mutating
    /// the buffer. `'"'` selects the unnamed register; `'0'` the yank-only
    /// register; `'a'`–`'z'` select named registers.
    ///
    /// Pure range-mutation primitive — does not consume input. Called by
    /// hjkl-vim's visual-mode operator path which has already resolved the range
    /// from the visual selection.
    ///
    /// Promoted to the public surface in 0.6.7 for Phase 4 visual-mode op
    /// grammar migration (kryptic-sh/hjkl#70).
    pub fn yank_range(
        &mut self,
        start: (usize, usize),
        end: (usize, usize),
        kind: crate::vim::RangeKind,
        register: char,
    ) {
        vim::yank_range_bridge(self, start, end, kind, register);
    }

    /// Delete the region `[start, end)` and transition to Insert mode (vim `c`
    /// operator). The deleted text is stashed in `register`. On return the
    /// editor is in Insert mode; the caller must not issue further normal-mode
    /// ops until the insert session ends.
    ///
    /// Pure range-mutation primitive — does not consume input. Called by
    /// hjkl-vim's visual-mode operator path which has already resolved the range
    /// from the visual selection.
    ///
    /// Promoted to the public surface in 0.6.7 for Phase 4 visual-mode op
    /// grammar migration (kryptic-sh/hjkl#70).
    pub fn change_range(
        &mut self,
        start: (usize, usize),
        end: (usize, usize),
        kind: crate::vim::RangeKind,
        register: char,
    ) {
        vim::change_range_bridge(self, start, end, kind, register);
    }

    /// Indent (`count > 0`) or outdent (`count < 0`) the row span
    /// `[start.0, end.0]`. Column components are ignored — indent is always
    /// linewise. `shiftwidth` overrides the editor's configured shiftwidth for
    /// this call; pass `0` to use the current editor setting. `count == 0` is a
    /// no-op.
    ///
    /// Pure range-mutation primitive — does not consume input. Called by
    /// hjkl-vim's visual-mode operator path which has already resolved the range
    /// from the visual selection.
    ///
    /// Promoted to the public surface in 0.6.7 for Phase 4 visual-mode op
    /// grammar migration (kryptic-sh/hjkl#70).
    pub fn indent_range(
        &mut self,
        start: (usize, usize),
        end: (usize, usize),
        count: i32,
        shiftwidth: u32,
    ) {
        vim::indent_range_bridge(self, start, end, count, shiftwidth);
    }

    /// Apply a case transformation (`Operator::Uppercase` /
    /// `Operator::Lowercase` / `Operator::ToggleCase`) to the region
    /// `[start, end)`. Other `Operator` variants are silently ignored (no-op).
    /// Yanks registers are left untouched — vim's case operators do not write
    /// to registers.
    ///
    /// Pure range-mutation primitive — does not consume input. Called by
    /// hjkl-vim's visual-mode operator path which has already resolved the range
    /// from the visual selection.
    ///
    /// Promoted to the public surface in 0.6.7 for Phase 4 visual-mode op
    /// grammar migration (kryptic-sh/hjkl#70).
    pub fn case_range(
        &mut self,
        start: (usize, usize),
        end: (usize, usize),
        kind: crate::vim::RangeKind,
        op: crate::vim::Operator,
    ) {
        vim::case_range_bridge(self, start, end, kind, op);
    }

    // ─── Phase 4e: pub block-shape range-mutation primitives (hjkl#70) ──────
    //
    // Rectangular VisualBlock operations. `top_row`/`bot_row` are inclusive
    // line indices; `left_col`/`right_col` are inclusive char-column bounds.
    // Ragged-edge handling (short lines not reaching `right_col`) matches the
    // engine FSM's `apply_block_operator` path — short lines lose only the
    // chars that exist.
    //
    // `register` is the target register; `'"'` selects the unnamed register.

    /// Delete a rectangular VisualBlock selection. `top_row` / `bot_row` are
    /// inclusive line bounds; `left_col` / `right_col` are inclusive column
    /// bounds at the visual (display) column level. Ragged-edge handling
    /// matches engine FSM's VisualBlock op behavior — short lines that don't
    /// reach `right_col` lose only the chars that exist.
    ///
    /// `register` honors the user's pending register selection.
    ///
    /// Promoted in 0.6.X for Phase 4e block-op grammar migration.
    pub fn delete_block(
        &mut self,
        top_row: usize,
        bot_row: usize,
        left_col: usize,
        right_col: usize,
        register: char,
    ) {
        vim::delete_block_bridge(self, top_row, bot_row, left_col, right_col, register);
    }

    /// Yank a rectangular VisualBlock selection into `register` without
    /// mutating the buffer. `'"'` selects the unnamed register.
    ///
    /// Promoted in 0.6.X for Phase 4e block-op grammar migration.
    pub fn yank_block(
        &mut self,
        top_row: usize,
        bot_row: usize,
        left_col: usize,
        right_col: usize,
        register: char,
    ) {
        vim::yank_block_bridge(self, top_row, bot_row, left_col, right_col, register);
    }

    /// Delete a rectangular VisualBlock selection and enter Insert mode (`c`
    /// operator). The deleted text is stashed in `register`. Mode is Insert
    /// on return; the caller must not issue further normal-mode ops until the
    /// insert session ends.
    ///
    /// Promoted in 0.6.X for Phase 4e block-op grammar migration.
    pub fn change_block(
        &mut self,
        top_row: usize,
        bot_row: usize,
        left_col: usize,
        right_col: usize,
        register: char,
    ) {
        vim::change_block_bridge(self, top_row, bot_row, left_col, right_col, register);
    }

    /// Indent (`count > 0`) or outdent (`count < 0`) rows `top_row..=bot_row`.
    /// Column bounds are ignored — vim's block indent is always linewise.
    /// `count == 0` is a no-op.
    ///
    /// Promoted in 0.6.X for Phase 4e block-op grammar migration.
    pub fn indent_block(
        &mut self,
        top_row: usize,
        bot_row: usize,
        _left_col: usize,
        _right_col: usize,
        count: i32,
    ) {
        vim::indent_block_bridge(self, top_row, bot_row, count);
    }

    /// Auto-indent (v1 dumb shiftwidth) the row span `[start.0, end.0]`.
    /// Column components are ignored — auto-indent is always linewise.
    ///
    /// The algorithm is a naive bracket-depth counter: it scans the buffer from
    /// row 0 to compute the correct depth at `start.0`, then for each line in
    /// the target range strips existing leading whitespace and prepends
    /// `depth × indent_unit` where `indent_unit` is `"\t"` when `expandtab`
    /// is `false`, or `" " × shiftwidth` when `expandtab` is `true`. Lines
    /// whose first non-whitespace character is a close bracket (`}`, `)`, `]`)
    /// get one fewer indent level. Empty / whitespace-only lines are cleared.
    ///
    /// After the operation the cursor lands on the first non-whitespace
    /// character of `start_row` (vim parity for `==`).
    ///
    /// **v1 limitation**: the bracket scan does not detect brackets inside
    /// string literals or comments. Code such as `let s = "{";` will increment
    /// the depth counter even though the brace is not a structural opener.
    /// Tree-sitter / LSP indentation is deferred to a follow-up.
    pub fn auto_indent_range(&mut self, start: (usize, usize), end: (usize, usize)) {
        vim::auto_indent_range_bridge(self, start, end);
    }

    /// Drain the row range set by the most recent auto-indent operation.
    ///
    /// Returns `Some((top_row, bot_row))` (inclusive) on the first call after
    /// an `=` / `==` / `=G` / Visual-`=` operator, then clears the stored
    /// value so a subsequent call returns `None`. The host (e.g. `apps/hjkl`)
    /// uses this to arm a brief visual flash over the reindented rows.
    pub fn take_last_indent_range(&mut self) -> Option<(usize, usize)> {
        self.last_indent_range.take()
    }

    /// Filter rows `top_row..=bot_row` through an external shell command.
    ///
    /// Spawns `sh -c "<command>"` (or `cmd /C "<command>"` on Windows), pipes
    /// the selected lines (joined by `\n`) to stdin, and waits up to
    /// `timeout_secs` seconds (default 10) for the process to finish.
    ///
    /// On success: the rows are replaced with stdout. No trailing-newline trim.
    /// On non-zero exit, spawn failure, or timeout: returns `Err(stderr_or_msg)`
    /// without mutating the buffer.
    ///
    /// `top_row` and `bot_row` are clamped to the buffer's valid row range.
    pub fn filter_range(
        &mut self,
        top_row: usize,
        bot_row: usize,
        command: &str,
        timeout_secs: Option<u64>,
    ) -> Result<(), String> {
        use std::io::Write;
        use std::process::{Command, Stdio};
        use std::thread;
        use std::time::Instant;

        let timeout = std::time::Duration::from_secs(timeout_secs.unwrap_or(10));
        let rope = crate::types::Query::rope(self.buffer());
        let line_count = rope.len_lines();
        let top = top_row.min(line_count.saturating_sub(1));
        let bot = bot_row.min(line_count.saturating_sub(1));
        let (top, bot) = (top.min(bot), top.max(bot));
        let input_text = crate::vim::rope_row_range_str(&rope, top, bot);
        // Materialized for the splice-back after the command succeeds.
        let lines = crate::vim::rope_to_lines_vec(&rope);

        tracing::debug!(
            top_row = top,
            bot_row = bot,
            command = command,
            "filter_range: spawning shell command"
        );

        #[cfg(not(windows))]
        let mut child = Command::new("sh")
            .args(["-c", command])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("spawn failed: {e}"))?;

        #[cfg(windows)]
        let mut child = Command::new("cmd")
            .args(["/C", command])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("spawn failed: {e}"))?;

        // Write stdin on a thread to avoid deadlock when output > pipe buffer.
        let mut stdin = child.stdin.take().ok_or("no stdin handle")?;
        let input_bytes = input_text.into_bytes();
        thread::spawn(move || {
            let _ = stdin.write_all(&input_bytes);
            // stdin drops here, signalling EOF to the child.
        });

        // Drain stdout/stderr on separate threads so the child's pipes don't
        // fill and deadlock the child. Keep `child` here so we can kill it on
        // timeout.
        let mut stdout_pipe = child.stdout.take().ok_or("no stdout handle")?;
        let mut stderr_pipe = child.stderr.take().ok_or("no stderr handle")?;
        let stdout_thread = thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = std::io::Read::read_to_end(&mut stdout_pipe, &mut buf);
            buf
        });
        let stderr_thread = thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = std::io::Read::read_to_end(&mut stderr_pipe, &mut buf);
            buf
        });

        // Poll try_wait until exit or timeout. On timeout: SIGKILL the child
        // (std Child::kill sends SIGKILL on Unix / TerminateProcess on Windows).
        // A proper TERM→KILL escalation would need nix/libc; skip for v1.
        let start = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    if start.elapsed() >= timeout {
                        tracing::debug!(command, "filter_range: timeout — killing child");
                        let _ = child.kill();
                        let _ = child.wait(); // reap so the OS can free resources
                        return Err(format!("command timed out after {}s", timeout.as_secs()));
                    }
                    thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(e) => return Err(format!("wait failed: {e}")),
            }
        };

        let stdout_bytes = stdout_thread.join().unwrap_or_default();
        let stderr_bytes = stderr_thread.join().unwrap_or_default();

        if !status.success() {
            let stderr = String::from_utf8_lossy(&stderr_bytes).into_owned();
            tracing::debug!(
                command,
                exit_code = ?status.code(),
                "filter_range: command exited with non-zero status"
            );
            return Err(if stderr.is_empty() {
                format!("command exited with status {}", status.code().unwrap_or(-1))
            } else {
                stderr
            });
        }

        let stdout = String::from_utf8_lossy(&stdout_bytes).into_owned();
        tracing::debug!(
            command,
            stdout_bytes = stdout_bytes.len(),
            "filter_range: command succeeded, replacing rows"
        );

        // Replace the row range with the stdout lines.
        let mut all_lines = lines;
        let new_lines: Vec<String> = stdout.lines().map(|l| l.to_owned()).collect();
        // If stdout ended with a newline, stdout.lines() drops the trailing empty
        // entry — this preserves vim's "no trailing-newline trim" spec because
        // a trailing '\n' from the command means the last replacement line is the
        // line BEFORE the newline, not an empty line after it.
        let after = all_lines.split_off(bot + 1);
        all_lines.truncate(top);
        all_lines.extend(new_lines);
        all_lines.extend(after);

        self.push_undo();
        self.restore(all_lines, (top, 0));
        // Leave mode as Normal after a successful filter operation (vim parity).
        self.force_normal();

        Ok(())
    }

    // ─── Comment toggle (#187) ───────────────────────────────────────────────

    /// Toggle line comments on rows `top_row..=bot_row` (0-based, inclusive).
    ///
    /// **Algorithm** (vim-commentary parity):
    ///
    /// 1. Determine the comment marker(s) for the active filetype.
    ///    Priority: `settings.commentstring` (`:set commentstring=…`) → per-filetype
    ///    default from `hjkl_lang::comment::commentstring_for_lang` → no-op.
    /// 2. Scan non-blank lines.  If every non-blank line is already commented →
    ///    strip the comment marker from each.  Otherwise → add it to all non-blank
    ///    lines.
    /// 3. Blank / whitespace-only lines are skipped (no marker added or removed).
    /// 4. The marker is inserted AFTER the leading whitespace (indent-preserving).
    /// 5. The entire operation is a single undo step.
    ///
    /// For block-comment languages (HTML, CSS) each line is individually wrapped
    /// as `start text end` (per-line block style, not one multi-line block).
    ///
    /// `top_row` and `bot_row` are clamped to the buffer's valid row range.
    pub fn toggle_comment_range(&mut self, top_row: usize, bot_row: usize) {
        use hjkl_lang::comment::commentstring_for_lang;

        let lang = self.settings.filetype.clone();

        // Resolve the comment markers.
        // If `settings.commentstring` is set (non-empty) parse `start %s end`
        // from it; otherwise fall back to the filetype table.
        let (start, end) = if !self.settings.commentstring.is_empty() {
            let cs = &self.settings.commentstring;
            if let Some(idx) = cs.find("%s") {
                let s = cs[..idx].trim_end().to_string();
                let e_raw = cs[idx + 2..].trim_start();
                let e: Option<String> = if e_raw.is_empty() {
                    None
                } else {
                    Some(e_raw.to_string())
                };
                (s, e)
            } else {
                // No %s placeholder — treat the whole string as start marker.
                (cs.clone(), None)
            }
        } else {
            match commentstring_for_lang(&lang) {
                Some((s, e)) => (s.to_string(), e.map(|v| v.to_string())),
                None => return, // no known comment syntax → no-op
            }
        };

        let row_count = buf_row_count(&self.buffer);
        let top = top_row.min(row_count.saturating_sub(1));
        let bot = bot_row.min(row_count.saturating_sub(1));

        // Collect all lines in the range.
        let lines: Vec<String> = (top..=bot)
            .map(|r| buf_line(&self.buffer, r).unwrap_or_default())
            .collect();

        // Check whether every non-blank line is already commented.
        let all_commented = lines.iter().all(|line| {
            let trimmed = line.trim_start();
            if trimmed.is_empty() {
                return true; // blank lines don't count against "all commented"
            }
            if let Some(ref end_marker) = end {
                // Block style: line starts with start and ends with end.
                trimmed.starts_with(start.as_str())
                    && line.trim_end().ends_with(end_marker.as_str())
            } else {
                trimmed.starts_with(start.as_str())
            }
        });

        let mut new_lines: Vec<String> = Vec::with_capacity(lines.len());
        for line in &lines {
            let trimmed = line.trim_start();
            if trimmed.is_empty() {
                // Blank line — leave as-is.
                new_lines.push(line.clone());
                continue;
            }
            let indent_len = line.len() - trimmed.len();
            let indent = &line[..indent_len];

            if all_commented {
                // Uncomment: strip exactly one occurrence of start (+ optional space).
                if let Some(after_start) = trimmed.strip_prefix(start.as_str()) {
                    // Strip one leading space after the marker if present.
                    let after_space = after_start.strip_prefix(' ').unwrap_or(after_start);
                    // For block style also strip the trailing end marker.
                    let text = if let Some(ref end_marker) = end {
                        after_space
                            .trim_end()
                            .strip_suffix(end_marker.as_str())
                            .map(|s| s.trim_end())
                            .unwrap_or(after_space)
                    } else {
                        after_space
                    };
                    new_lines.push(format!("{indent}{text}"));
                } else {
                    new_lines.push(line.clone());
                }
            } else {
                // Comment: insert marker after indent.
                let commented = if let Some(ref end_marker) = end {
                    format!("{indent}{start} {trimmed} {end_marker}")
                } else {
                    format!("{indent}{start} {trimmed}")
                };
                new_lines.push(commented);
            }
        }

        // Replace the row range in the buffer — single undo step.
        self.push_undo();
        let row_count_after = buf_row_count(&self.buffer);
        let all_before: Vec<String> = (0..top)
            .map(|r| buf_line(&self.buffer, r).unwrap_or_default())
            .collect();
        let all_after: Vec<String> = ((bot + 1)..row_count_after)
            .map(|r| buf_line(&self.buffer, r).unwrap_or_default())
            .collect();
        let mut all: Vec<String> = all_before;
        all.extend(new_lines);
        all.extend(all_after);
        self.restore(all, (top, 0));
    }

    // ─── Phase 4b: pub text-object resolution (hjkl#70) ─────────────────────
    //
    // Pure functions — no cursor mutation, no mode change, no register write.
    // Each method delegates to `vim::text_object_*_bridge`, which in turn calls
    // the existing `word_text_object` private resolver in vim.rs.
    //
    // Called by hjkl-vim's `OpTextObj` reducer (chunk 4e) to resolve the range
    // before invoking a range-mutation primitive (`delete_range`, etc.).
    //
    // Return value: `Some((start, end))` where both positions are `(row, col)`
    // byte-column pairs and `end` is *exclusive* (one past the last byte to act
    // on), matching the convention used by `delete_range` / `yank_range` / etc.
    // Returns `None` when the cursor is on an empty line or the resolver cannot
    // find a word boundary.

    /// Resolve the range of `iw` (inner word) at the current cursor position.
    ///
    /// An inner word is the contiguous run of keyword characters (or punctuation
    /// characters if the cursor is on punctuation) under the cursor, without any
    /// surrounding whitespace. Whitespace-only positions return `None`.
    ///
    /// Pure function — does not move the cursor or change any editor state.
    /// Called by hjkl-vim's `OpTextObj` reducer to resolve the range before
    /// invoking a range-mutation primitive (`delete_range`, etc.).
    ///
    /// Promoted to the public surface in 0.6.X for Phase 4b text-object grammar
    /// migration (kryptic-sh/hjkl#70).
    pub fn text_object_inner_word(&self) -> Option<((usize, usize), (usize, usize))> {
        vim::text_object_inner_word_bridge(self)
    }

    /// Resolve the range of `aw` (around word) at the current cursor position.
    ///
    /// Like `iw` but extends the range to include trailing whitespace after the
    /// word. If no trailing whitespace exists, leading whitespace before the word
    /// is absorbed instead (vim `:help text-objects` behaviour).
    ///
    /// Pure function — does not move the cursor or change any editor state.
    ///
    /// Promoted to the public surface in 0.6.X for Phase 4b text-object grammar
    /// migration (kryptic-sh/hjkl#70).
    pub fn text_object_around_word(&self) -> Option<((usize, usize), (usize, usize))> {
        vim::text_object_around_word_bridge(self)
    }

    /// Resolve the range of `iW` (inner WORD) at the current cursor position.
    ///
    /// A WORD is any contiguous run of non-whitespace characters — punctuation
    /// is not treated as a word boundary. Returns the span of the WORD under the
    /// cursor, without surrounding whitespace.
    ///
    /// Pure function — does not move the cursor or change any editor state.
    ///
    /// Promoted to the public surface in 0.6.X for Phase 4b text-object grammar
    /// migration (kryptic-sh/hjkl#70).
    pub fn text_object_inner_big_word(&self) -> Option<((usize, usize), (usize, usize))> {
        vim::text_object_inner_big_word_bridge(self)
    }

    /// Resolve the range of `aW` (around WORD) at the current cursor position.
    ///
    /// Like `iW` but extends the range to include trailing whitespace after the
    /// WORD. If no trailing whitespace exists, leading whitespace before the WORD
    /// is absorbed instead.
    ///
    /// Pure function — does not move the cursor or change any editor state.
    ///
    /// Promoted to the public surface in 0.6.X for Phase 4b text-object grammar
    /// migration (kryptic-sh/hjkl#70).
    pub fn text_object_around_big_word(&self) -> Option<((usize, usize), (usize, usize))> {
        vim::text_object_around_big_word_bridge(self)
    }

    // ─── Phase 4c: pub text-object resolution — quote + bracket (hjkl#70) ───
    //
    // Pure functions — no cursor mutation, no mode change, no register write.
    // Each method delegates to `vim::text_object_*_bridge`, which in turn calls
    // the existing private resolvers (`quote_text_object`, `bracket_text_object`)
    // in vim.rs.
    //
    // Quote methods take the quote char itself (`'"'`, `'\''`, `` '`' ``).
    // Bracket methods take the OPEN bracket char (`'('`, `'{'`, `'['`, `'<'`);
    // close-bracket variants (`)`, `}`, `]`, `>`) are NOT accepted here — the
    // hjkl-vim grammar layer normalises close→open before calling these methods.
    //
    // Return value: `Some((start, end))` where both positions are `(row, col)`
    // byte-column pairs and `end` is *exclusive* (one past the last byte to act
    // on), matching the convention used by `delete_range` / `yank_range` / etc.
    // `bracket_text_object` internally distinguishes Linewise vs Exclusive
    // ranges for multi-line pairs; that tag is stripped here — callers receive
    // the same flat shape as all other text-object resolvers.

    /// Resolve the range of `i<quote>` (inner quote) at the cursor position.
    ///
    /// `quote` is one of `'"'`, `'\''`, or `` '`' ``. Returns `None` when the
    /// cursor's line contains fewer than two occurrences of `quote`, or when no
    /// matching pair can be found around or ahead of the cursor.
    ///
    /// Inner range excludes the quote characters themselves.
    ///
    /// Pure function — no cursor mutation.
    ///
    /// Promoted to the public surface in 0.6.X for Phase 4c text-object grammar
    /// migration (kryptic-sh/hjkl#70).
    pub fn text_object_inner_quote(&self, quote: char) -> Option<((usize, usize), (usize, usize))> {
        vim::text_object_inner_quote_bridge(self, quote)
    }

    /// Resolve the range of `a<quote>` (around quote) at the cursor position.
    ///
    /// Like `i<quote>` but includes the quote characters themselves plus
    /// surrounding whitespace on one side: trailing whitespace after the closing
    /// quote if any exists; otherwise leading whitespace before the opening
    /// quote. This matches vim `:help text-objects` behaviour.
    ///
    /// Pure function — no cursor mutation.
    ///
    /// Promoted to the public surface in 0.6.X for Phase 4c text-object grammar
    /// migration (kryptic-sh/hjkl#70).
    pub fn text_object_around_quote(
        &self,
        quote: char,
    ) -> Option<((usize, usize), (usize, usize))> {
        vim::text_object_around_quote_bridge(self, quote)
    }

    /// Resolve the range of `i<bracket>` (inner bracket pair) at the cursor.
    ///
    /// `open` must be one of `'('`, `'{'`, `'['`, `'<'` — the corresponding
    /// close bracket is derived automatically. Close-bracket chars (`)`, `}`,
    /// `]`, `>`) are **not** accepted; hjkl-vim normalises close→open before
    /// calling this method. Returns `None` when no enclosing pair is found.
    ///
    /// The cursor may be anywhere inside the pair or on a bracket character
    /// itself. When not inside any pair the resolver falls back to a forward
    /// scan (targets.vim-style: `ci(` works when the cursor is before `(`).
    ///
    /// Inner range excludes the bracket characters. Multi-line pairs are
    /// supported; the returned range spans the full content between the
    /// brackets.
    ///
    /// Pure function — no cursor mutation.
    ///
    /// `ib` / `iB` aliases live in the hjkl-vim grammar layer and are not
    /// handled here.
    ///
    /// Promoted to the public surface in 0.6.X for Phase 4c text-object grammar
    /// migration (kryptic-sh/hjkl#70).
    pub fn text_object_inner_bracket(
        &self,
        open: char,
    ) -> Option<((usize, usize), (usize, usize))> {
        vim::text_object_inner_bracket_bridge(self, open)
    }

    /// Resolve the range of `a<bracket>` (around bracket pair) at the cursor.
    ///
    /// Like `i<bracket>` but includes the bracket characters themselves.
    /// `open` must be one of `'('`, `'{'`, `'['`, `'<'`.
    ///
    /// Pure function — no cursor mutation.
    ///
    /// `aB` alias lives in the hjkl-vim grammar layer and is not handled here.
    ///
    /// Promoted to the public surface in 0.6.X for Phase 4c text-object grammar
    /// migration (kryptic-sh/hjkl#70).
    pub fn text_object_around_bracket(
        &self,
        open: char,
    ) -> Option<((usize, usize), (usize, usize))> {
        vim::text_object_around_bracket_bridge(self, open)
    }

    // ── Sentence text objects (is / as) ───────────────────────────────────

    /// Resolve `is` (inner sentence) at the cursor position.
    ///
    /// Returns the range of the current sentence, excluding trailing
    /// whitespace. Sentence boundaries follow vim's `is` semantics (period /
    /// `?` / `!` followed by whitespace or end-of-paragraph).
    ///
    /// Pure function — no cursor mutation.
    ///
    /// Promoted to the public surface in 0.6.X for Phase 4d text-object
    /// grammar migration (kryptic-sh/hjkl#70).
    pub fn text_object_inner_sentence(&self) -> Option<((usize, usize), (usize, usize))> {
        vim::text_object_inner_sentence_bridge(self)
    }

    /// Resolve `as` (around sentence) at the cursor position.
    ///
    /// Like `is` but includes trailing whitespace after the sentence
    /// terminator.
    ///
    /// Pure function — no cursor mutation.
    ///
    /// Promoted to the public surface in 0.6.X for Phase 4d text-object
    /// grammar migration (kryptic-sh/hjkl#70).
    pub fn text_object_around_sentence(&self) -> Option<((usize, usize), (usize, usize))> {
        vim::text_object_around_sentence_bridge(self)
    }

    // ── Paragraph text objects (ip / ap) ──────────────────────────────────

    /// Resolve `ip` (inner paragraph) at the cursor position.
    ///
    /// A paragraph is a block of non-blank lines bounded by blank lines or
    /// buffer edges. Returns `None` when the cursor is on a blank line.
    ///
    /// Pure function — no cursor mutation.
    ///
    /// Promoted to the public surface in 0.6.X for Phase 4d text-object
    /// grammar migration (kryptic-sh/hjkl#70).
    pub fn text_object_inner_paragraph(&self) -> Option<((usize, usize), (usize, usize))> {
        vim::text_object_inner_paragraph_bridge(self)
    }

    /// Resolve `ap` (around paragraph) at the cursor position.
    ///
    /// Like `ip` but includes one trailing blank line when present.
    ///
    /// Pure function — no cursor mutation.
    ///
    /// Promoted to the public surface in 0.6.X for Phase 4d text-object
    /// grammar migration (kryptic-sh/hjkl#70).
    pub fn text_object_around_paragraph(&self) -> Option<((usize, usize), (usize, usize))> {
        vim::text_object_around_paragraph_bridge(self)
    }

    // ── Tag text objects (it / at) ────────────────────────────────────────

    /// Resolve `it` (inner tag) at the cursor position.
    ///
    /// Matches XML/HTML-style `<tag>...</tag>` pairs. Returns the range of
    /// inner content between the open and close tags (excluding the tags
    /// themselves).
    ///
    /// Pure function — no cursor mutation.
    ///
    /// Promoted to the public surface in 0.6.X for Phase 4d text-object
    /// grammar migration (kryptic-sh/hjkl#70).
    pub fn text_object_inner_tag(&self) -> Option<((usize, usize), (usize, usize))> {
        vim::text_object_inner_tag_bridge(self)
    }

    /// Resolve `at` (around tag) at the cursor position.
    ///
    /// Like `it` but includes the open and close tag delimiters themselves.
    ///
    /// Pure function — no cursor mutation.
    ///
    /// Promoted to the public surface in 0.6.X for Phase 4d text-object
    /// grammar migration (kryptic-sh/hjkl#70).
    pub fn text_object_around_tag(&self) -> Option<((usize, usize), (usize, usize))> {
        vim::text_object_around_tag_bridge(self)
    }

    /// Execute a named cursor motion `kind` repeated `count` times.
    ///
    /// Maps the keymap-layer `crate::MotionKind` to the engine's internal
    /// motion primitives, bypassing the engine FSM. Identical cursor semantics
    /// to the FSM path — sticky column, scroll sync, and big-jump tracking are
    /// all applied via `vim::execute_motion` (for Down/Up) or the same helpers
    /// used by the FSM arms.
    ///
    /// Introduced in 0.6.1 as the host entry point for Phase 3a of
    /// kryptic-sh/hjkl#69: the app keymap dispatches `AppAction::Motion` and
    /// calls this method rather than re-entering the engine FSM.
    ///
    /// Engine FSM arms for `h`/`j`/`k`/`l`/`<BS>`/`<Space>`/`+`/`-` remain
    /// intact for macro-replay coverage (macros re-feed raw keys through the
    /// FSM). This method is the keymap / controller path only.
    pub fn apply_motion(&mut self, kind: crate::MotionKind, count: usize) {
        vim::apply_motion_kind(self, kind, count);
    }

    /// Set `vim.pending_register` to `Some(reg)` if `reg` is a valid register
    /// selector (`a`–`z`, `A`–`Z`, `0`–`9`, `"`, `+`, `*`, `_`). Invalid
    /// chars are silently ignored (no-op), matching the engine FSM's
    /// `handle_select_register` behaviour.
    ///
    /// Promoted to the public surface in 0.5.17 so the hjkl-vim
    /// `PendingState::SelectRegister` reducer can dispatch `SetPendingRegister`
    /// without re-entering the engine FSM. `handle_select_register` (engine FSM
    /// path for macro-replay / defensive coverage) delegates here to avoid
    /// logic duplication.
    pub fn set_pending_register(&mut self, reg: char) {
        if reg.is_ascii_alphanumeric() || matches!(reg, '"' | '+' | '*' | '_') {
            self.vim.pending_register = Some(reg);
        }
        // Invalid chars silently no-op (matches engine FSM behavior).
    }

    /// Record a mark named `ch` at the current cursor position.
    ///
    /// Validates `ch` (must be `a`–`z` or `A`–`Z` to match vim's mark-name
    /// rules). Invalid chars are silently ignored (no-op), matching the engine
    /// FSM's `handle_set_mark` behaviour.
    ///
    /// Promoted to the public surface in 0.6.7 so the hjkl-vim
    /// `PendingState::SetMark` reducer can dispatch `EngineCmd::SetMark`
    /// without re-entering the engine FSM. `handle_set_mark` delegates here.
    pub fn set_mark_at_cursor(&mut self, ch: char) {
        vim::set_mark_at_cursor(self, ch);
    }

    /// `.` dot-repeat: replay the last buffered change at the current cursor.
    /// `count` scales repeats (e.g. `3.` runs the last change 3 times). When
    /// `count` is 0, defaults to 1. No-op when no change has been buffered yet.
    ///
    /// Storage of `LastChange` stays inside engine for now; Phase 5c of
    /// kryptic-sh/hjkl#71 just lifts the `.` chord binding into the app
    /// keymap so the engine FSM `.` arm is no longer the entry point. Engine
    /// FSM `.` arm stays for macro-replay defensive coverage.
    pub fn replay_last_change(&mut self, count: usize) {
        vim::replay_last_change(self, count);
    }

    /// Jump to the mark named `ch`, linewise (row only; col snaps to first
    /// non-blank). Pushes the pre-jump position onto the jumplist if the
    /// cursor actually moved.
    ///
    /// Accepts the same mark chars as vim's `'<ch>` command: `a`–`z`,
    /// `A`–`Z`, `'`/`` ` `` (jump-back peek), `.` (last edit), and the
    /// special auto-marks `[`, `]`, `<`, `>`. Unset marks and invalid chars
    /// are silently ignored (no-op), matching the engine FSM's
    /// `handle_goto_mark` behaviour.
    ///
    /// Promoted to the public surface in 0.6.7 so the hjkl-vim
    /// `PendingState::GotoMarkLine` reducer can dispatch
    /// `EngineCmd::GotoMarkLine` without re-entering the engine FSM.
    pub fn goto_mark_line(&mut self, ch: char) {
        vim::goto_mark(self, ch, true);
    }

    /// Jump to the mark named `ch`, charwise (exact row + col). Pushes the
    /// pre-jump position onto the jumplist if the cursor actually moved.
    ///
    /// Accepts the same mark chars as vim's `` `<ch> `` command: `a`–`z`,
    /// `A`–`Z`, `'`/`` ` `` (jump-back peek), `.` (last edit), and the
    /// special auto-marks `[`, `]`, `<`, `>`. Unset marks and invalid chars
    /// are silently ignored (no-op), matching the engine FSM's
    /// `handle_goto_mark` behaviour.
    ///
    /// Promoted to the public surface in 0.6.7 so the hjkl-vim
    /// `PendingState::GotoMarkChar` reducer can dispatch
    /// `EngineCmd::GotoMarkChar` without re-entering the engine FSM.
    pub fn goto_mark_char(&mut self, ch: char) {
        vim::goto_mark(self, ch, false);
    }

    /// Jump to the mark named `ch`, linewise. For uppercase marks (`'A'`–`'Z'`)
    /// that live in a different buffer, returns `MarkJump::CrossBuffer` so the
    /// app can switch slots before positioning the cursor. Returns
    /// `MarkJump::SameBuffer` for same-buffer / lowercase / special marks, and
    /// `MarkJump::Unset` when the mark is not set.
    pub fn try_goto_mark_line(&mut self, ch: char) -> MarkJump {
        vim::try_goto_mark(self, ch, true)
    }

    /// Jump to the mark named `ch`, charwise. For uppercase marks (`'A'`–`'Z'`)
    /// that live in a different buffer, returns `MarkJump::CrossBuffer` so the
    /// app can switch slots before positioning the cursor. Returns
    /// `MarkJump::SameBuffer` for same-buffer / lowercase / special marks, and
    /// `MarkJump::Unset` when the mark is not set.
    pub fn try_goto_mark_char(&mut self, ch: char) -> MarkJump {
        vim::try_goto_mark(self, ch, false)
    }

    // ── Macro controller API (Phase 5b) ──────────────────────────────────────

    /// Begin recording keystrokes into register `reg`. The caller (app) is
    /// responsible for stopping the recording via `stop_macro_record` when the
    /// user presses bare `q`.
    ///
    /// - Uppercase `reg` (e.g. `'A'`) appends to the existing lowercase
    ///   recording by pre-seeding `recording_keys` with the decoded text of the
    ///   matching lowercase register, matching vim's capital-register append
    ///   semantics.
    /// - Lowercase `reg` clears `recording_keys` (fresh recording).
    /// - Invalid chars (non-alphabetic, non-digit) are silently ignored.
    ///
    /// Promoted to the public surface in Phase 5b so the app's
    /// `route_chord_key` can start a recording without re-entering the engine
    /// FSM. `handle_record_macro_target` (engine FSM path for macro-replay
    /// defensive coverage) continues to use the same logic via delegation.
    pub fn start_macro_record(&mut self, reg: char) {
        if !(reg.is_ascii_alphabetic() || reg.is_ascii_digit()) {
            return;
        }
        self.vim.recording_macro = Some(reg);
        if reg.is_ascii_uppercase() {
            // Seed recording_keys with the existing lowercase register's text
            // decoded back to inputs so capital-register append continues from
            // where the previous recording left off.
            let lower = reg.to_ascii_lowercase();
            let text = self
                .registers
                .read(lower)
                .map(|s| s.text.clone())
                .unwrap_or_default();
            self.vim.recording_keys = crate::input::decode_macro(&text);
        } else {
            self.vim.recording_keys.clear();
        }
    }

    /// Finalize the active recording: encode `recording_keys` as text and write
    /// to the matching (lowercase) named register. Clears both `recording_macro`
    /// and `recording_keys`. No-ops if no recording is active.
    ///
    /// Promoted to the public surface in Phase 5b so the app's `QChord` action
    /// can stop a recording when the user presses bare `q` without re-entering
    /// the engine FSM.
    pub fn stop_macro_record(&mut self) {
        let Some(reg) = self.vim.recording_macro.take() else {
            return;
        };
        let keys = std::mem::take(&mut self.vim.recording_keys);
        let text = crate::input::encode_macro(&keys);
        self.set_named_register_text(reg.to_ascii_lowercase(), text);
    }

    /// Returns `true` while a `q{reg}` recording is in progress.
    /// Hosts use this to show a "recording @r" status indicator and to decide
    /// whether bare `q` should stop the recording or open the `RecordMacroTarget`
    /// chord.
    pub fn is_recording_macro(&self) -> bool {
        self.vim.recording_macro.is_some()
    }

    /// Returns `true` while a macro is being replayed. The app sets this flag
    /// (via `play_macro`) and clears it (via `end_macro_replay`) around the
    /// re-feed loop so the recorder hook can skip double-capture.
    pub fn is_replaying_macro(&self) -> bool {
        self.vim.replaying_macro
    }

    /// Decode the named register `reg` into a `Vec<crate::input::Input>` and
    /// prepare for replay, returning the inputs the app should re-feed through
    /// `route_chord_key`.
    ///
    /// Resolves `reg`:
    /// - `'@'` → use `vim.last_macro`; returns empty vec if none.
    /// - Any other char → lowercase it, read the register, decode.
    ///
    /// Side-effects:
    /// - Sets `vim.last_macro` to the resolved register.
    /// - Sets `vim.replaying_macro = true` so the recorder hook skips during
    ///   replay. The app calls `end_macro_replay` after the loop finishes.
    ///
    /// Returns an empty vec (and no side-effects for `'@'`) if the register is
    /// unset or empty.
    pub fn play_macro(&mut self, reg: char, count: usize) -> Vec<crate::input::Input> {
        let resolved = if reg == '@' {
            match self.vim.last_macro {
                Some(r) => r,
                None => return vec![],
            }
        } else {
            reg.to_ascii_lowercase()
        };
        let text = match self.registers.read(resolved) {
            Some(slot) if !slot.text.is_empty() => slot.text.clone(),
            _ => return vec![],
        };
        let keys = crate::input::decode_macro(&text);
        self.vim.last_macro = Some(resolved);
        self.vim.replaying_macro = true;
        // Multiply by count (minimum 1).
        keys.repeat(count.max(1))
    }

    /// Clear the `replaying_macro` flag. Called by the app after the
    /// re-feed loop in the `PlayMacro` commit arm completes (or aborts).
    pub fn end_macro_replay(&mut self) {
        self.vim.replaying_macro = false;
    }

    /// Append `input` to the active recording (`recording_keys`) if and only
    /// if a recording is in progress AND we are not currently replaying.
    /// Called by the app's `route_chord_key` recorder hook so that user
    /// keystrokes captured through the app-level chord path are recorded
    /// (rather than relying solely on the engine FSM's in-step hook).
    pub fn record_input(&mut self, input: crate::input::Input) {
        if self.vim.recording_macro.is_some() && !self.vim.replaying_macro {
            self.vim.recording_keys.push(input);
        }
    }

    // ─── Phase 6.1: public insert-mode primitives (kryptic-sh/hjkl#87) ────────
    //
    // Each method is the publicly callable form of one insert-mode action.
    // All logic lives in the corresponding `vim::*_bridge` free function;
    // these methods are thin delegators so the public surface stays on `Editor`.
    //
    // Invariants (enforced by the bridge fns):
    //   - Buffer mutations go through `mutate_edit` (dirty/undo/change-list).
    //   - Navigation keys call `break_undo_group_in_insert` when the FSM did.
    //   - `push_buffer_cursor_to_textarea` is called after every mutation
    //     (currently a no-op, kept for migration hygiene).

    /// Insert `ch` at the cursor. In Replace mode, overstrike the cell under
    /// the cursor instead of inserting; at end-of-line, always appends. With
    /// `smartindent` on, closing brackets (`}`/`)`/`]`) trigger one-unit
    /// dedent on an otherwise-whitespace line.
    ///
    /// Callers must ensure the editor is in Insert or Replace mode before
    /// calling this method.
    pub fn insert_char(&mut self, ch: char) {
        if vim::insert_char_bridge(self, ch) {
            self.after_insert_mutation();
        }
    }

    /// Insert a newline at the cursor, applying autoindent / smartindent to
    /// prefix the new line with the appropriate leading whitespace.
    ///
    /// Callers must ensure the editor is in Insert mode before calling.
    pub fn insert_newline(&mut self) {
        if vim::insert_newline_bridge(self) {
            self.after_insert_mutation();
        }
    }

    /// Common post-mutation sync for the `insert_*` primitives. The vim
    /// FSM's `step` runs `ensure_cursor_in_scrolloff` at the end of every
    /// normal/visual motion; insert-mode primitives bypass `step` and
    /// must self-correct or the cursor scrolls off the viewport (held
    /// Enter, multi-line backspace at BOL, arrow keys at edge, etc.).
    ///
    /// Marks the content dirty, widens the insert row's autoindent
    /// tracking, and re-checks scrolloff.
    fn after_insert_mutation(&mut self) {
        self.mark_content_dirty();
        let (row, _) = self.cursor();
        self.vim.widen_insert_row(row);
        self.ensure_cursor_in_scrolloff();
    }

    /// Like `after_insert_mutation` but for cursor-only insert ops that
    /// don't change content (arrows, Home/End, PageUp/Down). Skips the
    /// dirty mark.
    fn after_insert_motion(&mut self) {
        let (row, _) = self.cursor();
        self.vim.widen_insert_row(row);
        self.ensure_cursor_in_scrolloff();
    }

    /// Insert a tab character (or spaces up to the next `softtabstop` boundary
    /// when `expandtab` is set).
    ///
    /// Callers must ensure the editor is in Insert mode before calling.
    pub fn insert_tab(&mut self) {
        if vim::insert_tab_bridge(self) {
            self.after_insert_mutation();
        }
    }

    /// Delete the character before the cursor (Backspace). With `softtabstop`
    /// active, deletes the entire soft-tab run at an aligned boundary. Joins
    /// with the previous line when at column 0.
    ///
    /// Callers must ensure the editor is in Insert mode before calling.
    pub fn insert_backspace(&mut self) {
        if vim::insert_backspace_bridge(self) {
            self.after_insert_mutation();
        }
    }

    /// Delete the character under the cursor (Delete key). Joins with the
    /// next line when at end-of-line.
    ///
    /// Callers must ensure the editor is in Insert mode before calling.
    pub fn insert_delete(&mut self) {
        if vim::insert_delete_bridge(self) {
            self.after_insert_mutation();
        }
    }

    /// Move the cursor one step in `dir` (arrow key), breaking the undo group
    /// per `undo_break_on_motion`.
    ///
    /// Callers must ensure the editor is in Insert mode before calling.
    pub fn insert_arrow(&mut self, dir: vim::InsertDir) {
        vim::insert_arrow_bridge(self, dir);
        self.after_insert_motion();
    }

    /// Move the cursor to the start of the current line (Home key), breaking
    /// the undo group.
    ///
    /// Callers must ensure the editor is in Insert mode before calling.
    pub fn insert_home(&mut self) {
        vim::insert_home_bridge(self);
        self.after_insert_motion();
    }

    /// Move the cursor to the end of the current line (End key), breaking the
    /// undo group.
    ///
    /// Callers must ensure the editor is in Insert mode before calling.
    pub fn insert_end(&mut self) {
        vim::insert_end_bridge(self);
        self.after_insert_motion();
    }

    /// Scroll up one full viewport height (PageUp), moving the cursor with it.
    /// `viewport_h` is the current viewport height in rows; pass
    /// `self.viewport_height_value()` if the stored value is current.
    ///
    /// Callers must ensure the editor is in Insert mode before calling.
    pub fn insert_pageup(&mut self, viewport_h: u16) {
        vim::insert_pageup_bridge(self, viewport_h);
        self.after_insert_motion();
    }

    /// Scroll down one full viewport height (PageDown), moving the cursor with
    /// it. `viewport_h` is the current viewport height in rows.
    ///
    /// Callers must ensure the editor is in Insert mode before calling.
    pub fn insert_pagedown(&mut self, viewport_h: u16) {
        vim::insert_pagedown_bridge(self, viewport_h);
        self.after_insert_motion();
    }

    /// Delete from the cursor back to the start of the previous word (`Ctrl-W`).
    /// At column 0, joins with the previous line (vim `b`-motion semantics).
    ///
    /// Callers must ensure the editor is in Insert mode before calling.
    pub fn insert_ctrl_w(&mut self) {
        if vim::insert_ctrl_w_bridge(self) {
            self.after_insert_mutation();
        }
    }

    /// Delete from the cursor back to the start of the current line (`Ctrl-U`).
    /// No-op when already at column 0.
    ///
    /// Callers must ensure the editor is in Insert mode before calling.
    pub fn insert_ctrl_u(&mut self) {
        if vim::insert_ctrl_u_bridge(self) {
            self.after_insert_mutation();
        }
    }

    /// Delete one character backwards (`Ctrl-H`) — alias for Backspace in
    /// insert mode. Joins with the previous line when at col 0.
    ///
    /// Callers must ensure the editor is in Insert mode before calling.
    pub fn insert_ctrl_h(&mut self) {
        if vim::insert_ctrl_h_bridge(self) {
            self.after_insert_mutation();
        }
    }

    /// Enter "one-shot normal" mode (`Ctrl-O`): suspend insert for the next
    /// complete normal-mode command, then return to insert automatically.
    ///
    /// Callers must ensure the editor is in Insert mode before calling.
    pub fn insert_ctrl_o_arm(&mut self) {
        vim::insert_ctrl_o_bridge(self);
    }

    /// Arm the register-paste selector (`Ctrl-R`). The next call to
    /// `insert_paste_register(reg)` will insert the register contents.
    /// Alternatively, feeding a `Key::Char(c)` through the FSM will consume
    /// the armed state and paste register `c`.
    ///
    /// Callers must ensure the editor is in Insert mode before calling.
    pub fn insert_ctrl_r_arm(&mut self) {
        vim::insert_ctrl_r_bridge(self);
    }

    /// Indent the current line by one `shiftwidth` and shift the cursor right
    /// by the same amount (`Ctrl-T`).
    ///
    /// Callers must ensure the editor is in Insert mode before calling.
    pub fn insert_ctrl_t(&mut self) {
        let mutated = vim::insert_ctrl_t_bridge(self);
        if mutated {
            self.mark_content_dirty();
            let (row, _) = self.cursor();
            self.vim.widen_insert_row(row);
        }
    }

    /// Outdent the current line by up to one `shiftwidth` and shift the cursor
    /// left by the amount stripped (`Ctrl-D`).
    ///
    /// Callers must ensure the editor is in Insert mode before calling.
    pub fn insert_ctrl_d(&mut self) {
        let mutated = vim::insert_ctrl_d_bridge(self);
        if mutated {
            self.mark_content_dirty();
            let (row, _) = self.cursor();
            self.vim.widen_insert_row(row);
        }
    }

    /// Paste the contents of register `reg` at the cursor (the commit arm of
    /// `Ctrl-R {reg}`). Unknown or empty registers are a no-op.
    ///
    /// Callers must ensure the editor is in Insert mode before calling.
    pub fn insert_paste_register(&mut self, reg: char) {
        vim::insert_paste_register_bridge(self, reg);
        let (row, _) = self.cursor();
        self.vim.widen_insert_row(row);
    }

    /// Exit insert mode to Normal: finish the insert session, step the cursor
    /// one cell left (vim convention on Esc), record the `gi` target position,
    /// and update the sticky column.
    ///
    /// Callers must ensure the editor is in Insert mode before calling.
    pub fn leave_insert_to_normal(&mut self) {
        vim::leave_insert_to_normal_bridge(self);
    }

    // ── Phase 6.2: normal-mode primitive controller methods ───────────────────
    //
    // Each method is a thin wrapper around a `pub(crate) fn *_bridge` in
    // `vim.rs` following the same pattern as Phase 6.1. The FSM's
    // `handle_normal_only` now calls the same bridges so both paths are
    // identical. See kryptic-sh/hjkl#88 for the full promotion plan.

    /// `i` — transition to Insert mode at the current cursor position.
    /// `count` is stored in the insert session and replayed by dot-repeat
    /// as a repeat count on the inserted text.
    pub fn enter_insert_i(&mut self, count: usize) {
        vim::enter_insert_i_bridge(self, count);
    }

    /// `I` — move to the first non-blank character on the line, then
    /// transition to Insert mode. `count` is stored for dot-repeat.
    pub fn enter_insert_shift_i(&mut self, count: usize) {
        vim::enter_insert_shift_i_bridge(self, count);
    }

    /// `a` — advance the cursor one cell past the current position, then
    /// transition to Insert mode (append). `count` is stored for dot-repeat.
    pub fn enter_insert_a(&mut self, count: usize) {
        vim::enter_insert_a_bridge(self, count);
    }

    /// `A` — move the cursor to the end of the line, then transition to
    /// Insert mode (append at end). `count` is stored for dot-repeat.
    pub fn enter_insert_shift_a(&mut self, count: usize) {
        vim::enter_insert_shift_a_bridge(self, count);
    }

    /// `o` — open a new line below the current line with smart-indent, then
    /// transition to Insert mode. `count` is stored for dot-repeat replay.
    pub fn open_line_below(&mut self, count: usize) {
        vim::open_line_below_bridge(self, count);
    }

    /// `O` — open a new line above the current line with smart-indent, then
    /// transition to Insert mode. `count` is stored for dot-repeat replay.
    pub fn open_line_above(&mut self, count: usize) {
        vim::open_line_above_bridge(self, count);
    }

    /// `R` — enter Replace mode: subsequent typed characters overstrike the
    /// cell under the cursor rather than inserting. `count` is for replay.
    pub fn enter_replace_mode(&mut self, count: usize) {
        vim::enter_replace_mode_bridge(self, count);
    }

    /// `x` — delete `count` characters forward from the cursor and write them
    /// to the unnamed register. No-op on an empty line. Records for `.`.
    pub fn delete_char_forward(&mut self, count: usize) {
        vim::delete_char_forward_bridge(self, count);
    }

    /// `X` — delete `count` characters backward from the cursor and write
    /// them to the unnamed register. No-op at column 0. Records for `.`.
    pub fn delete_char_backward(&mut self, count: usize) {
        vim::delete_char_backward_bridge(self, count);
    }

    /// `s` — substitute `count` characters: delete them (writing to the
    /// unnamed register) then enter Insert mode. Equivalent to `cl`.
    /// Records as `OpMotion { Change, Right }` for dot-repeat.
    pub fn substitute_char(&mut self, count: usize) {
        vim::substitute_char_bridge(self, count);
    }

    /// `S` — substitute the current line: wipe its contents (writing to the
    /// unnamed register) then enter Insert mode. Equivalent to `cc`.
    /// Records as `LineOp { Change }` for dot-repeat.
    pub fn substitute_line(&mut self, count: usize) {
        vim::substitute_line_bridge(self, count);
    }

    /// `D` — delete from the cursor to end-of-line, writing to the unnamed
    /// register. The cursor parks on the new last character. Records for `.`.
    pub fn delete_to_eol(&mut self) {
        vim::delete_to_eol_bridge(self);
    }

    /// `C` — change from the cursor to end-of-line: delete to EOL then enter
    /// Insert mode. Equivalent to `c$`. Does not record its own `last_change`
    /// (the insert session records `DeleteToEol` on exit, like `c` motions).
    pub fn change_to_eol(&mut self) {
        vim::change_to_eol_bridge(self);
    }

    /// `Y` — yank from the cursor to end-of-line into the unnamed register.
    /// Vim 8 default: equivalent to `y$`. `count` multiplies the motion.
    pub fn yank_to_eol(&mut self, count: usize) {
        vim::yank_to_eol_bridge(self, count);
    }

    /// `J` — join `count` lines (default 2) onto the current line, inserting
    /// a single space between each non-empty pair. Records for dot-repeat.
    pub fn join_line(&mut self, count: usize) {
        vim::join_line_bridge(self, count);
    }

    /// `~` — toggle the case of `count` characters from the cursor, advancing
    /// right after each toggle. Records `ToggleCase` for dot-repeat.
    pub fn toggle_case_at_cursor(&mut self, count: usize) {
        vim::toggle_case_at_cursor_bridge(self, count);
    }

    /// `p` — paste the unnamed register (or the register selected via `"r`)
    /// after the cursor. Linewise content opens a new line below; charwise
    /// content is inserted inline. Records `Paste { before: false }` for `.`.
    pub fn paste_after(&mut self, count: usize) {
        vim::paste_after_bridge(self, count);
    }

    /// `P` — paste the unnamed register (or the `"r` register) before the
    /// cursor. Linewise content opens a new line above; charwise is inline.
    /// Records `Paste { before: true }` for dot-repeat.
    pub fn paste_before(&mut self, count: usize) {
        vim::paste_before_bridge(self, count);
    }

    /// `<C-o>` — jump back `count` entries in the jumplist, saving the
    /// current position on the forward stack so `<C-i>` can return.
    pub fn jump_back(&mut self, count: usize) {
        vim::jump_back_bridge(self, count);
    }

    /// `<C-i>` / `Tab` — redo `count` entries on the forward jumplist stack,
    /// saving the current position on the backward stack.
    pub fn jump_forward(&mut self, count: usize) {
        vim::jump_forward_bridge(self, count);
    }

    /// `<C-f>` / `<C-b>` — scroll the cursor by one full viewport height
    /// (height − 2 rows, preserving two-line overlap). `count` multiplies.
    /// `dir = Down` for `<C-f>`, `Up` for `<C-b>`.
    pub fn scroll_full_page(&mut self, dir: vim::ScrollDir, count: usize) {
        vim::scroll_full_page_bridge(self, dir, count);
    }

    /// `<C-d>` / `<C-u>` — scroll the cursor by half the viewport height.
    /// `count` multiplies the step. `dir = Down` for `<C-d>`, `Up` for `<C-u>`.
    pub fn scroll_half_page(&mut self, dir: vim::ScrollDir, count: usize) {
        vim::scroll_half_page_bridge(self, dir, count);
    }

    /// `<C-e>` / `<C-y>` — scroll the viewport `count` lines without moving
    /// the cursor (cursor is clamped to the new visible region if necessary).
    /// `dir = Down` for `<C-e>` (scroll text up), `Up` for `<C-y>`.
    pub fn scroll_line(&mut self, dir: vim::ScrollDir, count: usize) {
        vim::scroll_line_bridge(self, dir, count);
    }

    /// `n` — repeat the last `/` or `?` search `count` times in its original
    /// direction. `forward = true` keeps the direction; `false` inverts (`N`).
    pub fn search_repeat(&mut self, forward: bool, count: usize) {
        vim::search_repeat_bridge(self, forward, count);
    }

    /// `*` / `#` / `g*` / `g#` — search for the word under the cursor.
    /// `forward` chooses direction; `whole_word` wraps the pattern in `\b`
    /// anchors (true for `*` / `#`, false for `g*` / `g#`). `count` repeats.
    pub fn word_search(&mut self, forward: bool, whole_word: bool, count: usize) {
        vim::word_search_bridge(self, forward, whole_word, count);
    }

    // ── Phase 6.3: visual-mode primitive controller methods ──────────────────
    //
    // Each method is a thin wrapper around a `pub(crate) fn *_bridge` in
    // `vim.rs` following the same pattern as Phase 6.1 / 6.2. Both the FSM
    // and these wrappers write `current_mode` so `vim_mode()` returns correct
    // values regardless of which path performed the transition.
    // See kryptic-sh/hjkl#89 for the full promotion plan.

    /// `v` from Normal — enter charwise Visual mode, anchoring the selection
    /// at the current cursor position.
    pub fn enter_visual_char(&mut self) {
        vim::enter_visual_char_bridge(self);
    }

    /// `V` from Normal — enter linewise Visual mode, anchoring on the current
    /// line. Motions extend the selection by whole lines.
    pub fn enter_visual_line(&mut self) {
        vim::enter_visual_line_bridge(self);
    }

    /// `<C-v>` from Normal — enter Visual-block mode. The selection is a
    /// rectangle whose corners are the anchor and the live cursor.
    pub fn enter_visual_block(&mut self) {
        vim::enter_visual_block_bridge(self);
    }

    /// Esc from any visual mode — set `<` / `>` marks, stash the selection
    /// for `gv` re-entry, then return to Normal mode.
    pub fn exit_visual_to_normal(&mut self) {
        vim::exit_visual_to_normal_bridge(self);
    }

    /// `o` in Visual / VisualLine / VisualBlock — swap the cursor and anchor
    /// so the user can extend the other end of the selection. Does NOT
    /// mutate the selection range; only the active endpoint changes.
    pub fn visual_o_toggle(&mut self) {
        vim::visual_o_toggle_bridge(self);
    }

    /// `gv` — restore the last visual selection (mode + anchor + cursor
    /// position). No-op when no visual selection has been exited yet.
    pub fn reenter_last_visual(&mut self) {
        vim::reenter_last_visual_bridge(self);
    }

    /// Direct mode-transition entry point. Sets both the internal FSM mode
    /// and the stable `current_mode` field read by [`Editor::vim_mode`].
    ///
    /// Prefer the semantic primitives (`enter_visual_char`, `enter_insert_i`,
    /// …) which also set up required bookkeeping (anchors, sessions, …).
    /// Use `set_mode` only when you need a raw mode flip without side-effects.
    pub fn set_mode(&mut self, mode: VimMode) {
        vim::set_mode_bridge(self, mode);
    }
}

// ── Phase 6.6b: FSM state accessors (for hjkl-vim ownership) ─────────────────
//
// The FSM (now in hjkl-vim) reads/writes `VimState` fields through public
// `Editor` accessors and mutators defined in this block. Each method gets a
// one-line `///` rustdoc. Fields mutated as a unit get a combined action method
// rather than individual getters + setters (e.g. `accumulate_count_digit`).

/// State carried between [`Editor::begin_step`] and [`Editor::end_step`].
///
/// Treat as opaque — construct by calling `begin_step` and pass the
/// returned value directly into `end_step` without modification.
/// The fields capture per-step pre-dispatch state that the epilogue
/// needs to run its invariants correctly.
pub struct StepBookkeeping {
    /// True when the pending chord before this step was a macro-chord
    /// (`q{reg}` or `@{reg}`). The recorder hook skips these bookkeeping
    /// keys so that only the *payload* keys enter `recording_keys`.
    pub pending_was_macro_chord: bool,
    /// True when the mode was Insert *before* the FSM body ran. Used by
    /// the Ctrl-o one-shot-normal epilogue to decide whether to bounce
    /// back into Insert.
    pub was_insert: bool,
    /// Pre-dispatch visual snapshot. When the FSM body transitions out of
    /// a visual mode the epilogue uses this to set the `<`/`>` marks and
    /// store `last_visual` for `gv`.
    pub pre_visual_snapshot: Option<vim::LastVisual>,
}

impl<H: crate::types::Host> Editor<hjkl_buffer::Buffer, H> {
    // ── Pending chord ─────────────────────────────────────────────────────────

    /// Return a clone of the current pending chord state.
    pub fn pending(&self) -> vim::Pending {
        self.vim.pending.clone()
    }

    /// Overwrite the pending chord state.
    pub fn set_pending(&mut self, p: vim::Pending) {
        self.vim.pending = p;
    }

    /// Atomically take the pending chord, replacing it with `Pending::None`.
    pub fn take_pending(&mut self) -> vim::Pending {
        std::mem::take(&mut self.vim.pending)
    }

    // ── Count prefix ──────────────────────────────────────────────────────────

    /// Return the raw digit-prefix count (`0` = no prefix typed yet).
    pub fn count(&self) -> usize {
        self.vim.count
    }

    /// Overwrite the digit-prefix count directly.
    pub fn set_count(&mut self, c: usize) {
        self.vim.count = c;
    }

    /// Accumulate one more digit into the count prefix (mirrors `count * 10 + digit`).
    pub fn accumulate_count_digit(&mut self, digit: usize) {
        self.vim.count = self.vim.count.saturating_mul(10) + digit;
    }

    /// Reset the count prefix to zero (no pending count).
    pub fn reset_count(&mut self) {
        self.vim.count = 0;
    }

    /// Consume the count and return it; resets to zero. Returns `1` when no
    /// prefix was typed (mirrors `take_count` in vim.rs).
    pub fn take_count(&mut self) -> usize {
        if self.vim.count > 0 {
            let n = self.vim.count;
            self.vim.count = 0;
            n
        } else {
            1
        }
    }

    // ── Internal FSM mode ─────────────────────────────────────────────────────

    /// Return the FSM-internal mode (Normal / Insert / Visual / …).
    pub fn fsm_mode(&self) -> vim::Mode {
        self.vim.mode
    }

    /// Overwrite the FSM-internal mode without side-effects. Prefer the
    /// semantic primitives (`enter_insert_i`, `enter_visual_char`, …).
    pub fn set_fsm_mode(&mut self, m: vim::Mode) {
        self.vim.mode = m;
        self.vim.current_mode = self.vim.public_mode();
    }

    // ── Replaying flag ────────────────────────────────────────────────────────

    /// `true` while the `.` dot-repeat replay is running.
    pub fn is_replaying(&self) -> bool {
        self.vim.replaying
    }

    /// Set or clear the dot-replay flag.
    pub fn set_replaying(&mut self, v: bool) {
        self.vim.replaying = v;
    }

    // ── One-shot normal (Ctrl-o) ──────────────────────────────────────────────

    /// `true` when we entered Normal from Insert via `Ctrl-o` and will return
    /// to Insert after the next complete command.
    pub fn is_one_shot_normal(&self) -> bool {
        self.vim.one_shot_normal
    }

    /// Set or clear the Ctrl-o one-shot-normal flag.
    pub fn set_one_shot_normal(&mut self, v: bool) {
        self.vim.one_shot_normal = v;
    }

    // ── Last find (f/F/t/T target) ────────────────────────────────────────────

    /// Return the last `f`/`F`/`t`/`T` target as `(char, forward, till)`, or
    /// `None` before any find command was executed.
    pub fn last_find(&self) -> Option<(char, bool, bool)> {
        self.vim.last_find
    }

    /// Overwrite the stored last-find target.
    pub fn set_last_find(&mut self, target: Option<(char, bool, bool)>) {
        self.vim.last_find = target;
    }

    // ── Sneak motion ──────────────────────────────────────────────────────────

    /// Perform a vim-sneak style two-char digraph jump. Scans the buffer
    /// from the current cursor for the `count`-th occurrence of `c1+c2`.
    /// `forward=true` searches ahead; `forward=false` searches backward.
    /// Respects `Settings::motion_sneak` — callers (hjkl-vim FSM) should
    /// already gate on the setting; this method always executes the sneak.
    pub fn sneak(&mut self, c1: char, c2: char, forward: bool, count: usize) {
        vim::apply_sneak(self, c1, c2, forward, count.max(1));
    }

    /// Apply an operator over a sneak digraph range. Charwise exclusive —
    /// deletes from cursor up to (not including) the first char of the match.
    pub fn apply_op_sneak(
        &mut self,
        op: vim::Operator,
        c1: char,
        c2: char,
        forward: bool,
        total_count: usize,
    ) {
        vim::apply_op_sneak(self, op, c1, c2, forward, total_count);
    }

    /// Return the last sneak digraph and direction stored after a sneak motion.
    /// `Some(((c1, c2), forward))` when a sneak has been performed this session;
    /// `None` before any sneak. Used by `;`/`,` repeat and tests.
    pub fn last_sneak(&self) -> Option<((char, char), bool)> {
        self.vim.last_sneak
    }

    // ── Last change (dot-repeat payload) ─────────────────────────────────────

    /// Return a clone of the last recorded mutating change, or `None` before
    /// any change has been made.
    pub fn last_change(&self) -> Option<vim::LastChange> {
        self.vim.last_change.clone()
    }

    /// Overwrite the stored last-change record.
    pub fn set_last_change(&mut self, lc: Option<vim::LastChange>) {
        self.vim.last_change = lc;
    }

    /// Borrow the last-change record mutably (e.g. to fill in an `inserted`
    /// field after the insert session completes).
    pub fn last_change_mut(&mut self) -> Option<&mut vim::LastChange> {
        self.vim.last_change.as_mut()
    }

    // ── Insert session ────────────────────────────────────────────────────────

    /// Borrow the active insert session, or `None` when not in Insert mode.
    pub fn insert_session(&self) -> Option<&vim::InsertSession> {
        self.vim.insert_session.as_ref()
    }

    /// Borrow the active insert session mutably.
    pub fn insert_session_mut(&mut self) -> Option<&mut vim::InsertSession> {
        self.vim.insert_session.as_mut()
    }

    /// Atomically take the insert session out, leaving `None`.
    pub fn take_insert_session(&mut self) -> Option<vim::InsertSession> {
        self.vim.insert_session.take()
    }

    /// Install a new insert session, replacing any existing one.
    pub fn set_insert_session(&mut self, s: Option<vim::InsertSession>) {
        self.vim.insert_session = s;
    }

    // ── Visual anchors ────────────────────────────────────────────────────────

    /// Return the charwise Visual-mode anchor `(row, col)`.
    pub fn visual_anchor(&self) -> (usize, usize) {
        self.vim.visual_anchor
    }

    /// Overwrite the charwise Visual-mode anchor.
    pub fn set_visual_anchor(&mut self, anchor: (usize, usize)) {
        self.vim.visual_anchor = anchor;
    }

    /// Return the VisualLine anchor row.
    pub fn visual_line_anchor(&self) -> usize {
        self.vim.visual_line_anchor
    }

    /// Overwrite the VisualLine anchor row.
    pub fn set_visual_line_anchor(&mut self, row: usize) {
        self.vim.visual_line_anchor = row;
    }

    /// Return the VisualBlock anchor `(row, col)`.
    pub fn block_anchor(&self) -> (usize, usize) {
        self.vim.block_anchor
    }

    /// Overwrite the VisualBlock anchor.
    pub fn set_block_anchor(&mut self, anchor: (usize, usize)) {
        self.vim.block_anchor = anchor;
    }

    /// Return the VisualBlock virtual column used to survive j/k row clamping.
    pub fn block_vcol(&self) -> usize {
        self.vim.block_vcol
    }

    /// Overwrite the VisualBlock virtual column.
    pub fn set_block_vcol(&mut self, vcol: usize) {
        self.vim.block_vcol = vcol;
    }

    // ── Yank linewise flag ────────────────────────────────────────────────────

    /// `true` when the last yank/cut was linewise (affects `p`/`P` layout).
    pub fn yank_linewise(&self) -> bool {
        self.vim.yank_linewise
    }

    /// Set or clear the linewise-yank flag.
    pub fn set_yank_linewise(&mut self, v: bool) {
        self.vim.yank_linewise = v;
    }

    // ── Pending register selector ─────────────────────────────────────────────
    // Note: `pending_register()` getter already exists at line ~1254 (Phase 4e).
    // Only the mutators are new here.

    /// Overwrite the pending register selector (Phase 6.6b mutator companion to
    /// the existing `pending_register()` getter).
    pub fn set_pending_register_raw(&mut self, reg: Option<char>) {
        self.vim.pending_register = reg;
    }

    /// Atomically take the pending register, returning `None` afterward.
    pub fn take_pending_register_raw(&mut self) -> Option<char> {
        self.vim.pending_register.take()
    }

    // ── Macro recording ───────────────────────────────────────────────────────

    /// Return the register currently being recorded into, or `None`.
    pub fn recording_macro(&self) -> Option<char> {
        self.vim.recording_macro
    }

    /// Overwrite the recording-macro target register.
    pub fn set_recording_macro(&mut self, reg: Option<char>) {
        self.vim.recording_macro = reg;
    }

    /// Append one input to the in-progress macro recording buffer.
    pub fn push_recording_key(&mut self, input: crate::input::Input) {
        self.vim.recording_keys.push(input);
    }

    /// Atomically take the recorded key sequence, leaving an empty vec.
    pub fn take_recording_keys(&mut self) -> Vec<crate::input::Input> {
        std::mem::take(&mut self.vim.recording_keys)
    }

    /// Overwrite the recording-keys buffer (e.g. to seed from a register).
    pub fn set_recording_keys(&mut self, keys: Vec<crate::input::Input>) {
        self.vim.recording_keys = keys;
    }

    /// Return the number of keys currently in the recording buffer.
    /// Useful for integration tests that verify macro-recording bookkeeping
    /// without draining the buffer via [`take_recording_keys`].
    pub fn recording_keys_len(&self) -> usize {
        self.vim.recording_keys.len()
    }

    // ── Macro replay flag ─────────────────────────────────────────────────────

    /// `true` while `@reg` macro replay is running (suppresses re-recording).
    pub fn is_replaying_macro_raw(&self) -> bool {
        self.vim.replaying_macro
    }

    /// Set or clear the macro-replay-in-progress flag.
    pub fn set_replaying_macro_raw(&mut self, v: bool) {
        self.vim.replaying_macro = v;
    }

    // ── Last macro register ───────────────────────────────────────────────────

    /// Return the register of the most recently played macro (`@@` source).
    pub fn last_macro(&self) -> Option<char> {
        self.vim.last_macro
    }

    /// Overwrite the last-played-macro register.
    pub fn set_last_macro(&mut self, reg: Option<char>) {
        self.vim.last_macro = reg;
    }

    // ── Last insert position ──────────────────────────────────────────────────

    /// Return the cursor position when Insert mode was last exited (for `gi`).
    pub fn last_insert_pos(&self) -> Option<(usize, usize)> {
        self.vim.last_insert_pos
    }

    /// Overwrite the stored last-insert position.
    pub fn set_last_insert_pos(&mut self, pos: Option<(usize, usize)>) {
        self.vim.last_insert_pos = pos;
    }

    // ── Last visual selection ─────────────────────────────────────────────────

    /// Return the saved visual selection snapshot for `gv`, or `None`.
    pub fn last_visual(&self) -> Option<vim::LastVisual> {
        self.vim.last_visual
    }

    /// Overwrite the saved visual selection snapshot.
    pub fn set_last_visual(&mut self, snap: Option<vim::LastVisual>) {
        self.vim.last_visual = snap;
    }

    // ── Viewport-pinned flag ──────────────────────────────────────────────────

    /// `true` when `zz`/`zt`/`zb` pinned the viewport this step (suppresses
    /// the end-of-step scrolloff pass).
    pub fn viewport_pinned(&self) -> bool {
        self.vim.viewport_pinned
    }

    /// Set or clear the viewport-pinned flag.
    pub fn set_viewport_pinned(&mut self, v: bool) {
        self.vim.viewport_pinned = v;
    }

    // ── Insert pending register (Ctrl-R wait) ─────────────────────────────────

    /// `true` while waiting for the register-name key after `Ctrl-R` in
    /// Insert mode.
    pub fn insert_pending_register(&self) -> bool {
        self.vim.insert_pending_register
    }

    /// Set or clear the `Ctrl-R` register-wait flag.
    pub fn set_insert_pending_register(&mut self, v: bool) {
        self.vim.insert_pending_register = v;
    }

    // ── Change-mark start ─────────────────────────────────────────────────────

    /// Return the stashed `[` mark start for a Change operation, or `None`.
    pub fn change_mark_start(&self) -> Option<(usize, usize)> {
        self.vim.change_mark_start
    }

    /// Atomically take the change-mark start, leaving `None`.
    pub fn take_change_mark_start(&mut self) -> Option<(usize, usize)> {
        self.vim.change_mark_start.take()
    }

    /// Overwrite the change-mark start.
    pub fn set_change_mark_start(&mut self, pos: Option<(usize, usize)>) {
        self.vim.change_mark_start = pos;
    }

    // ── Timeout tracking ──────────────────────────────────────────────────────

    /// Return the wall-clock `Instant` of the last keystroke.
    pub fn last_input_at(&self) -> Option<std::time::Instant> {
        self.vim.last_input_at
    }

    /// Overwrite the wall-clock last-input timestamp.
    pub fn set_last_input_at(&mut self, t: Option<std::time::Instant>) {
        self.vim.last_input_at = t;
    }

    /// Return the `Host::now()` duration at the last keystroke.
    pub fn last_input_host_at(&self) -> Option<core::time::Duration> {
        self.vim.last_input_host_at
    }

    /// Overwrite the host-clock last-input timestamp.
    pub fn set_last_input_host_at(&mut self, d: Option<core::time::Duration>) {
        self.vim.last_input_host_at = d;
    }

    // ── Search prompt ──────────────────────────────────────────────────────────

    /// Borrow the live search prompt, or `None` when not in search-prompt mode.
    pub fn search_prompt_state(&self) -> Option<&vim::SearchPrompt> {
        self.vim.search_prompt.as_ref()
    }

    /// Borrow the live search prompt mutably.
    pub fn search_prompt_state_mut(&mut self) -> Option<&mut vim::SearchPrompt> {
        self.vim.search_prompt.as_mut()
    }

    /// Atomically take the search prompt, leaving `None`.
    pub fn take_search_prompt_state(&mut self) -> Option<vim::SearchPrompt> {
        self.vim.search_prompt.take()
    }

    /// Install a new search prompt (entering search-prompt mode).
    pub fn set_search_prompt_state(&mut self, prompt: Option<vim::SearchPrompt>) {
        self.vim.search_prompt = prompt;
    }

    // ── Last search pattern / direction ───────────────────────────────────────
    // Note: `last_search_forward()` getter already exists at line ~1909.
    // `set_last_search()` combined mutator exists at line ~1918.
    // Only new / complementary accessors are added here.

    /// Return the most recently committed search pattern, or `None`.
    pub fn last_search_pattern(&self) -> Option<&str> {
        self.vim.last_search.as_deref()
    }

    /// Overwrite the stored last-search pattern without changing direction
    /// (use the existing `set_last_search` for the combined update).
    pub fn set_last_search_pattern_only(&mut self, pattern: Option<String>) {
        self.vim.last_search = pattern;
    }

    /// Overwrite only the last-search direction flag.
    pub fn set_last_search_forward_only(&mut self, forward: bool) {
        self.vim.last_search_forward = forward;
    }

    // ── Search history ────────────────────────────────────────────────────────

    /// Borrow the committed search-pattern history (oldest first).
    pub fn search_history(&self) -> &[String] {
        &self.vim.search_history
    }

    /// Borrow the search history mutably (e.g. to push a new entry).
    pub fn search_history_mut(&mut self) -> &mut Vec<String> {
        &mut self.vim.search_history
    }

    /// Return the current search-history navigation cursor index.
    pub fn search_history_cursor(&self) -> Option<usize> {
        self.vim.search_history_cursor
    }

    /// Overwrite the search-history navigation cursor.
    pub fn set_search_history_cursor(&mut self, idx: Option<usize>) {
        self.vim.search_history_cursor = idx;
    }

    // ── Jump lists ────────────────────────────────────────────────────────────

    /// Borrow the back half of the jump list (entries Ctrl-o pops from).
    pub fn jump_back_list(&self) -> &[(usize, usize)] {
        &self.vim.jump_back
    }

    /// Borrow the back jump list mutably (push / pop).
    pub fn jump_back_list_mut(&mut self) -> &mut Vec<(usize, usize)> {
        &mut self.vim.jump_back
    }

    /// Borrow the forward half of the jump list (entries Ctrl-i pops from).
    pub fn jump_fwd_list(&self) -> &[(usize, usize)] {
        &self.vim.jump_fwd
    }

    /// Borrow the forward jump list mutably (push / pop / clear).
    pub fn jump_fwd_list_mut(&mut self) -> &mut Vec<(usize, usize)> {
        &mut self.vim.jump_fwd
    }

    // ── Phase 6.6c: search + jump helpers (public Editor API) ───────────────
    //
    // `push_search_pattern`, `push_jump`, `record_search_history`, and
    // `walk_search_history` are public `Editor` methods so that `hjkl-vim`'s
    // search-prompt and normal-mode FSM can call them via the public API.

    /// Compile `pattern` into a regex and install it as the active search
    /// pattern. Respects `:set ignorecase` / `:set smartcase` and inline
    /// `\c`/`\C` overrides. An empty or invalid pattern clears the highlight
    /// without raising an error.
    pub fn push_search_pattern(&mut self, pattern: &str) {
        let compiled = if pattern.is_empty() {
            None
        } else {
            use crate::search::{CaseMode, resolve_case_mode};
            let base =
                CaseMode::from_options(self.settings().ignore_case, self.settings().smartcase);
            let (stripped, mode) = resolve_case_mode(pattern, base);
            let src = if mode == CaseMode::Insensitive {
                format!("(?i){stripped}")
            } else {
                stripped
            };
            regex::Regex::new(&src).ok()
        };
        let wrap = self.settings().wrapscan;
        self.set_search_pattern(compiled);
        self.search_state_mut().wrap_around = wrap;
    }

    /// Record a pre-jump cursor position onto the back jumplist. Called
    /// before any "big jump" motion (`gg`/`G`, `%`, `*`/`#`, `n`/`N`,
    /// committed `/` or `?`, …). Branching off the history clears the
    /// forward half, matching vim's "redo-is-lost" semantics.
    pub fn push_jump(&mut self, from: (usize, usize)) {
        self.vim.jump_back.push(from);
        if self.vim.jump_back.len() > vim::JUMPLIST_MAX {
            self.vim.jump_back.remove(0);
        }
        self.vim.jump_fwd.clear();
    }

    /// Push `pattern` onto the committed search history. Skips if the
    /// most recent entry already matches (consecutive dedupe) and trims
    /// the oldest entries beyond the history cap.
    pub fn record_search_history(&mut self, pattern: &str) {
        if pattern.is_empty() {
            return;
        }
        if self.vim.search_history.last().map(String::as_str) == Some(pattern) {
            return;
        }
        self.vim.search_history.push(pattern.to_string());
        let len = self.vim.search_history.len();
        if len > vim::SEARCH_HISTORY_MAX {
            self.vim
                .search_history
                .drain(0..len - vim::SEARCH_HISTORY_MAX);
        }
    }

    /// Walk the search-prompt history by `dir` steps. `dir = -1` moves
    /// toward older entries (Ctrl-P / Up); `dir = 1` toward newer ones
    /// (Ctrl-N / Down). Stops at the ends; does nothing if there is no
    /// active search prompt.
    pub fn walk_search_history(&mut self, dir: isize) {
        if self.vim.search_history.is_empty() || self.vim.search_prompt.is_none() {
            return;
        }
        let len = self.vim.search_history.len();
        let next_idx = match (self.vim.search_history_cursor, dir) {
            (None, -1) => Some(len - 1),
            (None, 1) => return,
            (Some(i), -1) => i.checked_sub(1),
            (Some(i), 1) if i + 1 < len => Some(i + 1),
            _ => None,
        };
        let Some(idx) = next_idx else {
            return;
        };
        self.vim.search_history_cursor = Some(idx);
        let text = self.vim.search_history[idx].clone();
        if let Some(prompt) = self.vim.search_prompt.as_mut() {
            prompt.cursor = text.chars().count();
            prompt.text = text.clone();
        }
        self.push_search_pattern(&text);
    }

    // ── Phase 6.6d: pre/post FSM bookkeeping ────────────────────────────────
    //
    // `begin_step` and `end_step` are the bookkeeping prelude/epilogue that
    // `hjkl_vim::dispatch_input` wraps around its per-mode FSM dispatch.

    /// Pre-dispatch bookkeeping that must run before every per-mode FSM step.
    ///
    /// Call this at the start of every step; pass the returned
    /// [`StepBookkeeping`] to [`end_step`] after the FSM body finishes.
    ///
    /// Returns `Ok(bk)` when the caller should proceed with FSM dispatch.
    /// Returns `Err(consumed)` when the prelude itself handled the input
    /// (macro-stop chord); in that case skip the FSM body and do NOT call
    /// `end_step` — the macro-stop path is a true short-circuit with no
    /// epilogue needed.
    ///
    /// This method does NOT handle the search-prompt intercept — callers
    /// must check `search_prompt_state().is_some()` before calling `begin_step`
    /// and dispatch to the search-prompt FSM body directly.
    pub fn begin_step(&mut self, input: Input) -> Result<StepBookkeeping, bool> {
        use crate::input::Key;
        use vim::{Mode, Pending};
        // ── Timestamps ───────────────────────────────────────────────────────
        // Phase 7f: sync buffer before motion handlers see it.
        self.sync_buffer_content_from_textarea();
        // `:set timeoutlen` chord-timeout handling.
        let now = std::time::Instant::now();
        let host_now = self.host.now();
        let timed_out = match self.vim.last_input_host_at {
            Some(prev) => host_now.saturating_sub(prev) > self.settings.timeout_len,
            None => false,
        };
        if timed_out {
            let chord_in_flight = !matches!(self.vim.pending, Pending::None)
                || self.vim.count != 0
                || self.vim.pending_register.is_some()
                || self.vim.insert_pending_register;
            if chord_in_flight {
                self.vim.clear_pending_prefix();
            }
        }
        self.vim.last_input_at = Some(now);
        self.vim.last_input_host_at = Some(host_now);
        // ── Macro-stop: bare `q` outside Insert ends the recording ───────────
        if self.vim.recording_macro.is_some()
            && !self.vim.replaying_macro
            && matches!(self.vim.pending, Pending::None)
            && self.vim.mode != Mode::Insert
            && input.key == Key::Char('q')
            && !input.ctrl
            && !input.alt
        {
            let reg = self.vim.recording_macro.take().unwrap();
            let keys = std::mem::take(&mut self.vim.recording_keys);
            let text = crate::input::encode_macro(&keys);
            self.set_named_register_text(reg.to_ascii_lowercase(), text);
            return Err(true);
        }
        // ── Snapshots for epilogue ────────────────────────────────────────────
        let pending_was_macro_chord = matches!(
            self.vim.pending,
            Pending::RecordMacroTarget | Pending::PlayMacroTarget { .. }
        );
        let was_insert = self.vim.mode == Mode::Insert;
        let pre_visual_snapshot = match self.vim.mode {
            Mode::Visual => Some(vim::LastVisual {
                mode: Mode::Visual,
                anchor: self.vim.visual_anchor,
                cursor: self.cursor(),
                block_vcol: 0,
            }),
            Mode::VisualLine => Some(vim::LastVisual {
                mode: Mode::VisualLine,
                anchor: (self.vim.visual_line_anchor, 0),
                cursor: self.cursor(),
                block_vcol: 0,
            }),
            Mode::VisualBlock => Some(vim::LastVisual {
                mode: Mode::VisualBlock,
                anchor: self.vim.block_anchor,
                cursor: self.cursor(),
                block_vcol: self.vim.block_vcol,
            }),
            _ => None,
        };
        Ok(StepBookkeeping {
            pending_was_macro_chord,
            was_insert,
            pre_visual_snapshot,
        })
    }

    /// Post-dispatch bookkeeping that must run after every per-mode FSM step.
    ///
    /// `input` is the same input that was passed to `begin_step`.
    /// `bk` is the [`StepBookkeeping`] returned by `begin_step`.
    /// `consumed` is the return value of the FSM body; this method returns
    /// it after running all epilogue invariants.
    ///
    /// Must NOT be called when `begin_step` returned `Err(...)`.
    pub fn end_step(&mut self, input: Input, bk: StepBookkeeping, consumed: bool) -> bool {
        use crate::input::Key;
        use vim::{Mode, Pending};
        let StepBookkeeping {
            pending_was_macro_chord,
            was_insert,
            pre_visual_snapshot,
        } = bk;
        // ── Visual-exit: set `<`/`>` marks and stash `last_visual` ───────────
        if let Some(snap) = pre_visual_snapshot
            && !matches!(
                self.vim.mode,
                Mode::Visual | Mode::VisualLine | Mode::VisualBlock
            )
        {
            let (lo, hi) = match snap.mode {
                Mode::Visual => {
                    if snap.anchor <= snap.cursor {
                        (snap.anchor, snap.cursor)
                    } else {
                        (snap.cursor, snap.anchor)
                    }
                }
                Mode::VisualLine => {
                    let r_lo = snap.anchor.0.min(snap.cursor.0);
                    let r_hi = snap.anchor.0.max(snap.cursor.0);
                    let vl_rope = self.buffer().rope();
                    let r_hi_clamped = r_hi.min(vl_rope.len_lines().saturating_sub(1));
                    let last_col = hjkl_buffer::rope_line_str(&vl_rope, r_hi_clamped)
                        .chars()
                        .count()
                        .saturating_sub(1);
                    ((r_lo, 0), (r_hi, last_col))
                }
                Mode::VisualBlock => {
                    let (r1, c1) = snap.anchor;
                    let (r2, c2) = snap.cursor;
                    ((r1.min(r2), c1.min(c2)), (r1.max(r2), c1.max(c2)))
                }
                _ => {
                    if snap.anchor <= snap.cursor {
                        (snap.anchor, snap.cursor)
                    } else {
                        (snap.cursor, snap.anchor)
                    }
                }
            };
            self.set_mark('<', lo);
            self.set_mark('>', hi);
            self.vim.last_visual = Some(snap);
        }
        // ── Ctrl-o one-shot-normal return to Insert ───────────────────────────
        if !was_insert
            && self.vim.one_shot_normal
            && self.vim.mode == Mode::Normal
            && matches!(self.vim.pending, Pending::None)
        {
            self.vim.one_shot_normal = false;
            self.vim.mode = Mode::Insert;
        }
        // ── Content + viewport sync ───────────────────────────────────────────
        self.sync_buffer_content_from_textarea();
        if !self.vim.viewport_pinned {
            self.ensure_cursor_in_scrolloff();
        }
        self.vim.viewport_pinned = false;
        // ── Recorder hook ─────────────────────────────────────────────────────
        if self.vim.recording_macro.is_some()
            && !self.vim.replaying_macro
            && input.key != Key::Char('q')
            && !pending_was_macro_chord
        {
            self.vim.recording_keys.push(input);
        }
        // ── Phase 6.3: current_mode sync ─────────────────────────────────────
        self.vim.current_mode = self.vim.public_mode();
        consumed
    }

    // ── Phase 6.6e: additional public primitives for hjkl-vim::normal ─────────

    /// `true` when the editor is in any visual mode (Visual / VisualLine /
    /// VisualBlock). Convenience wrapper around `vim_mode()` for hjkl-vim.
    pub fn is_visual(&self) -> bool {
        matches!(
            self.vim.mode,
            vim::Mode::Visual | vim::Mode::VisualLine | vim::Mode::VisualBlock
        )
    }

    /// Compute the VisualBlock rectangle corners: `(top_row, bot_row,
    /// left_col, right_col)`. Uses `block_anchor` and `block_vcol` (the
    /// virtual column, which survives j/k clamping to shorter rows).
    ///
    /// Promoted in Phase 6.6e so `hjkl-vim::normal` can compute the block
    /// extents needed for VisualBlock `I` / `A` / `r` without accessing
    /// engine-private helpers.
    pub fn visual_block_bounds(&self) -> (usize, usize, usize, usize) {
        let (ar, ac) = self.vim.block_anchor;
        let (cr, _) = self.cursor();
        let cc = self.vim.block_vcol;
        let top = ar.min(cr);
        let bot = ar.max(cr);
        let left = ac.min(cc);
        let right = ac.max(cc);
        (top, bot, left, right)
    }

    /// Return the character count (code-point count) of line `row`, or `0`
    /// when `row` is out of range. Used by hjkl-vim::normal for VisualBlock
    /// I / A column computations.
    pub fn line_char_count(&self, row: usize) -> usize {
        buf_line_chars(&self.buffer, row)
    }

    /// Apply operator over `motion` with `count` repetitions. The full
    /// vim-quirks path (operator context for `l`, clamping, etc.) is applied.
    ///
    /// Promoted to the public surface in Phase 6.6e so `hjkl-vim::normal`'s
    /// relocated `handle_after_op` can call it directly with a parsed `Motion`
    /// without re-entering the engine FSM.
    pub fn apply_op_with_motion_direct(
        &mut self,
        op: crate::vim::Operator,
        motion: &crate::vim::Motion,
        count: usize,
    ) {
        vim::apply_op_with_motion(self, op, motion, count);
    }

    /// `Ctrl-a` / `Ctrl-x` — adjust the number under or after the cursor.
    /// `delta = 1` increments; `delta = -1` decrements; larger deltas
    /// multiply as in vim's `5<C-a>`. Promoted in Phase 6.6e so
    /// `hjkl-vim::normal` can dispatch `Ctrl-a` / `Ctrl-x`.
    pub fn adjust_number(&mut self, delta: i64) {
        vim::adjust_number(self, delta);
    }

    /// Open the `/` or `?` search prompt. `forward = true` for `/`,
    /// `false` for `?`. Promoted in Phase 6.6e so `hjkl-vim::normal` can
    /// dispatch `/` and `?` without re-entering the engine FSM.
    pub fn enter_search(&mut self, forward: bool) {
        vim::enter_search(self, forward);
    }

    /// Enter Insert mode at the left edge of a VisualBlock selection for
    /// `I`. Moves the cursor to `(top, col)`, resets to Normal internally,
    /// then begins an insert session with `InsertReason::BlockEdge`.
    ///
    /// Promoted in Phase 6.6e so `hjkl-vim::normal` can dispatch the
    /// VisualBlock `I` command without accessing engine-private helpers.
    pub fn visual_block_insert_at_left(&mut self, top: usize, bot: usize, col: usize) {
        self.jump_cursor(top, col);
        self.vim.mode = vim::Mode::Normal;
        vim::begin_insert(self, 1, vim::InsertReason::BlockEdge { top, bot, col });
    }

    /// Enter Insert mode at the right edge of a VisualBlock selection for
    /// `A`. Moves the cursor to `(top, col)`, resets to Normal internally,
    /// then begins an insert session with `InsertReason::BlockEdge`.
    ///
    /// Promoted in Phase 6.6e so `hjkl-vim::normal` can dispatch the
    /// VisualBlock `A` command without accessing engine-private helpers.
    pub fn visual_block_append_at_right(&mut self, top: usize, bot: usize, col: usize) {
        self.jump_cursor(top, col);
        self.vim.mode = vim::Mode::Normal;
        vim::begin_insert(self, 1, vim::InsertReason::BlockEdge { top, bot, col });
    }

    /// Execute a motion (cursor movement), push to the jumplist for big jumps,
    /// and update the sticky column. Mirrors the engine FSM's `execute_motion`
    /// free function. Promoted in Phase 6.6e for `hjkl-vim::normal`.
    pub fn execute_motion(&mut self, motion: crate::vim::Motion, count: usize) {
        vim::execute_motion(self, motion, count);
    }

    /// Update the VisualBlock virtual column after a motion in VisualBlock mode.
    /// Horizontal motions sync `block_vcol` to the cursor column; vertical /
    /// non-h/l motions leave it alone so the intended column survives clamping
    /// to shorter rows. Promoted in Phase 6.6e for `hjkl-vim::normal`.
    pub fn update_block_vcol(&mut self, motion: &crate::vim::Motion) {
        vim::update_block_vcol(self, motion);
    }

    /// Apply `op` over the current visual selection (char-wise, linewise, or
    /// block). Mirrors the engine's internal `apply_visual_operator` free fn.
    /// Promoted in Phase 6.6e for `hjkl-vim::normal`.
    pub fn apply_visual_operator(&mut self, op: crate::vim::Operator) {
        vim::apply_visual_operator(self, op);
    }

    /// Replace each character cell in the current VisualBlock selection with
    /// `ch`. Mirrors the engine's `block_replace` free fn. Promoted in Phase
    /// 6.6e for the VisualBlock `r<ch>` command in `hjkl-vim::normal`.
    pub fn replace_block_char(&mut self, ch: char) {
        vim::block_replace(self, ch);
    }

    /// Extend the current visual selection to cover the text object identified
    /// by `ch` and `inner`. Maps `ch` to a `TextObject`, resolves its range
    /// via `text_object_range`, then updates the visual anchor and cursor.
    ///
    /// Promoted in Phase 6.6e for the visual-mode `i<ch>` / `a<ch>` commands
    /// in `hjkl-vim::normal::handle_visual_text_obj`.
    pub fn visual_text_obj_extend(&mut self, ch: char, inner: bool) {
        use crate::vim::{Mode, TextObject};
        let obj = match ch {
            'w' => TextObject::Word { big: false },
            'W' => TextObject::Word { big: true },
            '"' | '\'' | '`' => TextObject::Quote(ch),
            '(' | ')' | 'b' => TextObject::Bracket('('),
            '[' | ']' => TextObject::Bracket('['),
            '{' | '}' | 'B' => TextObject::Bracket('{'),
            '<' | '>' => TextObject::Bracket('<'),
            'p' => TextObject::Paragraph,
            't' => TextObject::XmlTag,
            's' => TextObject::Sentence,
            _ => return,
        };
        let Some((start, end, kind)) = vim::text_object_range(self, obj, inner) else {
            return;
        };
        match kind {
            crate::vim::RangeKind::Linewise => {
                self.vim.visual_line_anchor = start.0;
                self.vim.mode = Mode::VisualLine;
                self.vim.current_mode = VimMode::VisualLine;
                self.jump_cursor(end.0, 0);
            }
            _ => {
                self.vim.mode = Mode::Visual;
                self.vim.current_mode = VimMode::Visual;
                self.vim.visual_anchor = (start.0, start.1);
                let (er, ec) = vim::retreat_one(self, end);
                self.jump_cursor(er, ec);
            }
        }
    }
}

/// Visual column of the character at `char_col` in `line`, treating `\t`
/// as expansion to the next `tab_width` stop and every other char as
/// 1 cell wide. Wide-char support (CJK, emoji) is a separate concern —
/// the cursor math elsewhere also assumes single-cell chars.
fn visual_col_for_char(line: &str, char_col: usize, tab_width: usize) -> usize {
    let mut visual = 0usize;
    for (i, ch) in line.chars().enumerate() {
        if i >= char_col {
            break;
        }
        if ch == '\t' {
            visual += tab_width - (visual % tab_width);
        } else {
            visual += 1;
        }
    }
    visual
}

#[cfg(test)]
mod shift_syntax_spans_tests {
    use super::*;
    use crate::types::{ContentEdit, DefaultHost, Options, Style};
    use hjkl_buffer::Buffer;

    fn ed_with_spans(line_count: usize) -> Editor<Buffer, DefaultHost> {
        let text = (0..line_count)
            .map(|i| format!("row{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let buf = Buffer::from_str(&text);
        let mut e = Editor::new(buf, DefaultHost::new(), Options::default());
        // Synthesize span rows so we can detect which survive a shift.
        // Use a distinct fg colour per row so spans are identifiable.
        let style = Style::default();
        let spans: Vec<Vec<(usize, usize, Style)>> =
            (0..line_count).map(|_| vec![(0, 1, style)]).collect();
        e.install_syntax_spans(spans);
        e
    }

    fn edit_insert_newline_at(row: u32, col: u32) -> ContentEdit {
        // Pressing Enter: zero-width insertion that produces one new row.
        ContentEdit {
            start_byte: 0,
            old_end_byte: 0,
            new_end_byte: 1,
            start_position: (row, col),
            old_end_position: (row, col),
            new_end_position: (row + 1, 0),
        }
    }

    fn edit_join_rows(row: u32, col: u32) -> ContentEdit {
        // Backspace at start of `row+1`: removes the newline, joining the
        // two rows. old_end is on `row+1`, new_end on `row`.
        ContentEdit {
            start_byte: 0,
            old_end_byte: 1,
            new_end_byte: 0,
            start_position: (row, col),
            old_end_position: (row + 1, 0),
            new_end_position: (row, col),
        }
    }

    #[test]
    fn insert_grows_buffer_spans_in_place() {
        let mut e = ed_with_spans(4);
        // Newline at row 1 → buffer grew by one row.
        e.shift_syntax_spans_for_edits(&[edit_insert_newline_at(1, 1)]);
        assert_eq!(
            e.buffer_spans().len(),
            5,
            "row-count grew → spans rows must match"
        );
        // The empty row should be at index 2 (right after the split point).
        assert!(e.buffer_spans()[2].is_empty(), "inserted row sits at oer+1");
        // Surrounding rows kept their content.
        assert!(!e.buffer_spans()[0].is_empty());
        assert!(!e.buffer_spans()[1].is_empty());
        assert!(!e.buffer_spans()[3].is_empty());
        assert!(!e.buffer_spans()[4].is_empty());
    }

    #[test]
    fn delete_shrinks_buffer_spans_in_place() {
        let mut e = ed_with_spans(4);
        e.shift_syntax_spans_for_edits(&[edit_join_rows(1, 1)]);
        assert_eq!(
            e.buffer_spans().len(),
            3,
            "row-count shrank → spans rows must match"
        );
    }

    #[test]
    fn same_row_edit_leaves_rows_untouched() {
        let mut e = ed_with_spans(3);
        let edit = ContentEdit {
            start_byte: 0,
            old_end_byte: 0,
            new_end_byte: 1,
            start_position: (1, 0),
            old_end_position: (1, 0),
            new_end_position: (1, 1),
        };
        e.shift_syntax_spans_for_edits(&[edit]);
        assert_eq!(e.buffer_spans().len(), 3);
        for row in 0..3 {
            assert!(
                !e.buffer_spans()[row].is_empty(),
                "row {row} should still hold its span"
            );
        }
    }

    #[test]
    fn ordered_edits_apply_against_prior_state() {
        let mut e = ed_with_spans(3);
        // Two consecutive inserts: each adds a row.
        e.shift_syntax_spans_for_edits(&[
            edit_insert_newline_at(0, 1),
            edit_insert_newline_at(1, 1),
        ]);
        assert_eq!(e.buffer_spans().len(), 5);
    }

    /// Build a buffer with `line_count` rows where row `i` has a span at
    /// column `i + 1` so the rows are independently identifiable after a
    /// shift (otherwise all spans look identical and can't tell which
    /// original row's spans landed at which post-shift index).
    fn ed_with_distinguishable_spans(line_count: usize) -> Editor<Buffer, DefaultHost> {
        let text = (0..line_count)
            .map(|i| format!("rowwwwwwwwww{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let buf = Buffer::from_str(&text);
        let mut e = Editor::new(buf, DefaultHost::new(), Options::default());
        let style = Style::default();
        let spans: Vec<Vec<(usize, usize, Style)>> = (0..line_count)
            .map(|i| vec![(i + 1, i + 2, style)])
            .collect();
        e.install_syntax_spans(spans);
        e
    }

    /// Regression for off-by-one in `shift_syntax_spans_for_edits`.
    ///
    /// `P` (paste-before) at column 0 of row 0 inserts new lines BEFORE
    /// row 0. The pre-paste rows should shift down by N. The fix inserts
    /// empty rows at idx `start.row` (not `oer + 1`) when `start.col == 0`.
    ///
    /// Symptom before the fix: row 0's spans stayed at idx 0 after a
    /// 4-row `ggP`, but the file's row 0 was now the pasted content (no
    /// spans available yet). Display: pasted row 0 painted with the
    /// pre-paste row 0's spans (LUCKILY identical content in many cases)
    /// while the *shifted* pre-paste row 0 (now at file row 4) painted
    /// with the pre-paste row 1's spans — visible as the WRONG row
    /// showing the wrong-row colours.
    #[test]
    fn shift_for_paste_at_start_of_row_zero() {
        let mut e = ed_with_distinguishable_spans(7);
        // Snapshot: row i has a span at col (i+1, i+2).
        let pre = e.buffer_spans().to_vec();
        // P at (0, 0) inserting 4 lines.
        let edit = ContentEdit {
            start_byte: 0,
            old_end_byte: 0,
            new_end_byte: 4,
            start_position: (0, 0),
            old_end_position: (0, 0),
            new_end_position: (4, 0),
        };
        e.shift_syntax_spans_for_edits(&[edit]);
        assert_eq!(e.buffer_spans().len(), 11, "row count grew by 4");
        // Rows 0..4 are the new pasted lines — should be EMPTY placeholders.
        for row in 0..4 {
            assert!(
                e.buffer_spans()[row].is_empty(),
                "row {row} (new paste) must be empty placeholder, got {:?}",
                e.buffer_spans()[row]
            );
        }
        // Rows 4..11 are the original rows 0..7 shifted down by 4.
        for (orig_row, orig_spans) in pre.iter().enumerate() {
            let new_row = orig_row + 4;
            assert_eq!(
                &e.buffer_spans()[new_row],
                orig_spans,
                "original row {orig_row} should be at file row {new_row} after \
                 paste-before-row-0"
            );
        }
    }

    /// Same idea for paste at start of a non-zero row: `2GP` inserts 3
    /// lines before row 2.
    #[test]
    fn shift_for_paste_at_start_of_middle_row() {
        let mut e = ed_with_distinguishable_spans(5);
        let pre = e.buffer_spans().to_vec();
        // Insert 3 lines at (2, 0).
        let edit = ContentEdit {
            start_byte: 0,
            old_end_byte: 0,
            new_end_byte: 3,
            start_position: (2, 0),
            old_end_position: (2, 0),
            new_end_position: (5, 0),
        };
        e.shift_syntax_spans_for_edits(&[edit]);
        assert_eq!(e.buffer_spans().len(), 8);
        // Rows 0..2 unchanged (before the insertion point).
        assert_eq!(e.buffer_spans()[0], pre[0]);
        assert_eq!(e.buffer_spans()[1], pre[1]);
        // Rows 2..5 are new pasted lines.
        for row in 2..5 {
            assert!(
                e.buffer_spans()[row].is_empty(),
                "row {row} must be empty placeholder"
            );
        }
        // Rows 5..8 are originals 2..5 shifted down by 3.
        for (orig_row, orig_spans) in pre.iter().enumerate().take(5).skip(2) {
            let new_row = orig_row + 3;
            assert_eq!(
                &e.buffer_spans()[new_row],
                orig_spans,
                "original row {orig_row} should land at file row {new_row}"
            );
        }
    }

    /// Regression: pasting N rows at the beginning of the buffer used to
    /// run `Vec::insert(0, ...)` once per row → O(N²) memmove. samply
    /// showed this path eating 87 % of paste CPU on a 60 k-row paste.
    /// The splice rewrite is O(N).
    ///
    /// Asserting a hard wall-clock bound is brittle on slow CI, so we
    /// pick a budget the old code blows past by >10×: 60 k rows in
    /// under 200 ms even on a debug build. Old impl: ~3-5 seconds.
    #[test]
    fn shift_for_60k_row_paste_at_row_zero_is_under_200ms() {
        let mut e = ed_with_distinguishable_spans(8);
        let edit = ContentEdit {
            start_byte: 0,
            old_end_byte: 0,
            new_end_byte: 60_000,
            start_position: (0, 0),
            old_end_position: (0, 0),
            new_end_position: (60_000, 0),
        };
        let t = std::time::Instant::now();
        e.shift_syntax_spans_for_edits(&[edit]);
        let elapsed = t.elapsed();
        assert!(
            elapsed.as_millis() < 200,
            "60k-row shift took {elapsed:?}; budget is 200 ms (catches \
             reintroduction of the O(N²) per-row insert loop)"
        );
        assert_eq!(e.buffer_spans().len(), 60_008);
    }

    /// Regression: `push_undo` used to clone every line into a
    /// `Vec<String>` (162 k heap allocations on a 162 k-row buffer per
    /// snapshot). Now stores an `Arc<String>` shared with
    /// `Buffer::content_joined`'s per-dirty_gen cache — a warm snapshot
    /// is an `Arc::clone` (one ptr bump).
    ///
    /// Test: snapshot a 60 k-row buffer 100 times. With the Arc impl
    /// this is essentially free (one join then 99 Arc::clones). The
    /// old `Vec<String>` impl required 60 k allocations per call =
    /// 6 M allocations, easily seconds even on release.
    #[test]
    fn push_undo_snapshot_arc_clone_is_under_100ms_for_100_snapshots() {
        use crate::types::{DefaultHost, Options};
        let text = "x\n".repeat(60_000);
        let buf = hjkl_buffer::Buffer::from_str(&text);
        let mut e = Editor::new(buf, DefaultHost::default(), Options::default());
        // Warm the cache: one join, subsequent snapshots Arc::clone it.
        e.push_undo();
        let t = std::time::Instant::now();
        for _ in 0..100 {
            e.push_undo();
        }
        let elapsed = t.elapsed();
        assert!(
            elapsed.as_millis() < 100,
            "100 snapshots of a 60k-row buffer took {elapsed:?}; budget \
             100 ms. Likely regressed to per-line cloning."
        );
    }
}

#[cfg(test)]
mod earlier_later_tests {
    use super::*;
    use crate::types::{DefaultHost, Options};
    use hjkl_buffer::Buffer;
    use std::time::{Duration, SystemTime};

    fn make_ed(content: &str) -> Editor<Buffer, DefaultHost> {
        let buf = Buffer::from_str(content);
        Editor::new(buf, DefaultHost::default(), Options::default())
    }

    // ── step-based ───────────────────────────────────────────────────────────

    #[test]
    fn earlier_by_steps_n_undoes_n_changes() {
        let mut ed = make_ed("hello");
        ed.push_undo(); // snap 1
        ed.push_undo(); // snap 2
        ed.push_undo(); // snap 3
        assert_eq!(ed.undo_stack_len(), 3);
        let applied = ed.earlier_by_steps(2);
        assert_eq!(applied, 2);
        assert_eq!(ed.undo_stack_len(), 1);
    }

    #[test]
    fn earlier_by_steps_caps_at_stack_size() {
        let mut ed = make_ed("hello");
        ed.push_undo(); // snap 1
        // Ask for 10 but only 1 available.
        let applied = ed.earlier_by_steps(10);
        assert_eq!(applied, 1);
        assert_eq!(ed.undo_stack_len(), 0);
    }

    #[test]
    fn later_by_steps_n_redoes_n_changes() {
        let mut ed = make_ed("hello");
        ed.push_undo(); // snap 1
        ed.push_undo(); // snap 2
        ed.push_undo(); // snap 3
        // Undo all 3 so they're on redo stack.
        ed.earlier_by_steps(3);
        assert_eq!(ed.undo_stack_len(), 0);
        let applied = ed.later_by_steps(2);
        assert_eq!(applied, 2);
        assert_eq!(ed.undo_stack_len(), 2);
    }

    #[test]
    fn later_by_steps_caps_at_redo_stack_size() {
        let mut ed = make_ed("hello");
        ed.push_undo(); // snap 1
        ed.earlier_by_steps(1); // moves to redo
        let applied = ed.later_by_steps(99);
        assert_eq!(applied, 1);
    }

    // ── time-based ───────────────────────────────────────────────────────────

    fn epoch_plus(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn earlier_by_time_stops_at_target_boundary() {
        let mut ed = make_ed("hello");
        // Push 3 entries at t-30s, t-20s, t-10s (relative to epoch).
        ed.push_undo_at(epoch_plus(30));
        ed.push_undo_at(epoch_plus(40));
        ed.push_undo_at(epoch_plus(50));
        // Redo stack is empty; undo has 3 entries.
        // target = epoch+35 → should undo entries at t=50 and t=40, stop at t=30
        let target = epoch_plus(35);
        let applied = ed.earlier_by_time(target);
        assert_eq!(applied, 2, "should undo t=50 and t=40; stop at t=30");
        assert_eq!(ed.undo_stack_len(), 1, "t=30 entry remains");
    }

    #[test]
    fn earlier_by_time_empty_stack_returns_zero() {
        let mut ed = make_ed("hello");
        let applied = ed.earlier_by_time(epoch_plus(999));
        assert_eq!(applied, 0);
        assert_eq!(ed.undo_stack_len(), 0);
    }

    #[test]
    fn later_by_time_target_in_future_redoes_all() {
        let mut ed = make_ed("hello");
        ed.push_undo_at(epoch_plus(10));
        ed.push_undo_at(epoch_plus(20));
        // Undo both → they move to redo stack with their timestamps preserved.
        ed.earlier_by_steps(2);
        // target far in future: should redo all.
        let applied = ed.later_by_time(epoch_plus(9999));
        assert_eq!(applied, 2);
        assert_eq!(ed.undo_stack_len(), 2);
    }
}

#[cfg(test)]
mod insert_mode_scrolloff_tests {
    use super::*;
    use crate::types::{DefaultHost, Host, Options};
    use crate::vim::Mode;
    use hjkl_buffer::Buffer;

    fn ed_with_lines(line_count: usize) -> Editor<Buffer, DefaultHost> {
        let text = (0..line_count)
            .map(|i| format!("row{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let buf = Buffer::from_str(&text);
        let mut e = Editor::new(buf, DefaultHost::new(), Options::default());
        // Viewport: 20 rows tall, starts at top.
        let vp = e.host_mut().viewport_mut();
        vp.width = 80;
        vp.height = 20;
        vp.top_row = 0;
        vp.top_col = 0;
        e.set_viewport_height(20);
        e.vim.mode = Mode::Insert;
        e
    }

    /// Regression: holding Enter in insert mode used to scroll the cursor
    /// off the viewport because `insert_newline` (called from the app's
    /// `dispatch_insert_key`) bypasses the FSM `step` that runs
    /// `ensure_cursor_in_scrolloff`. The post-mutation helper now runs
    /// scrolloff for every insert primitive — the cursor must stay
    /// within `scrolloff` rows of the bottom edge.
    #[test]
    fn insert_newline_keeps_cursor_in_scrolloff() {
        let mut e = ed_with_lines(200);
        // Park cursor at the bottom edge of the viewport (row 19).
        e.set_cursor_doc(19, 0);
        // Press Enter 50 times. Cursor moves down each newline; without
        // scrolloff the cursor would slide off the bottom of the
        // viewport at row 20+ and the user would type blind.
        for _ in 0..50 {
            e.insert_newline();
        }
        let (cursor_row, _) = e.cursor();
        let vp = e.host().viewport();
        let cursor_screen_row = cursor_row.saturating_sub(vp.top_row);
        let scrolloff = e.settings().scrolloff;
        let margin = scrolloff.min(vp.height as usize - 1) / 2;
        let max_screen_row = vp.height as usize - 1 - margin;
        assert!(
            cursor_screen_row <= max_screen_row,
            "cursor screen row {cursor_screen_row} exceeded scrolloff bound {max_screen_row} \
             (cursor_row={cursor_row}, vp.top_row={vp_top}, vp.height={vp_h})",
            vp_top = vp.top_row,
            vp_h = vp.height,
        );
    }

    /// Same check for `insert_arrow(Down)` — cursor-only motion that also
    /// must trigger scrolloff.
    #[test]
    fn insert_arrow_down_keeps_cursor_in_scrolloff() {
        let mut e = ed_with_lines(200);
        e.set_cursor_doc(19, 0);
        for _ in 0..50 {
            e.insert_arrow(vim::InsertDir::Down);
        }
        let (cursor_row, _) = e.cursor();
        let vp = e.host().viewport();
        let cursor_screen_row = cursor_row.saturating_sub(vp.top_row);
        let scrolloff = e.settings().scrolloff;
        let margin = scrolloff.min(vp.height as usize - 1) / 2;
        let max_screen_row = vp.height as usize - 1 - margin;
        assert!(
            cursor_screen_row <= max_screen_row,
            "cursor screen row {cursor_screen_row} exceeded scrolloff bound {max_screen_row}"
        );
    }
}
