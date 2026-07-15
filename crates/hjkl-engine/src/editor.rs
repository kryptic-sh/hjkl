//! Editor — the public sqeel-vim type, layered over `hjkl_buffer::View`.
//!
//! This file owns the public Editor API — construction, content access,
//! mouse and goto helpers, the (buffer-level) undo stack, and insert-mode
//! session bookkeeping. All vim-specific keyboard handling lives in
//! [`vim`] and communicates with Editor through a small internal API
//! exposed via `pub(super)` fields and helper methods.

use std::sync::atomic::{AtomicU16, Ordering};
use std::time::SystemTime;

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
fn buffer_byte_of_row(buf: &hjkl_buffer::View, row: usize) -> usize {
    let rope = buf.rope();
    let row = row.min(rope.len_lines());
    rope.line_to_byte(row)
}

/// Convert an `hjkl_buffer::Position` (char-indexed col) into byte
/// coordinates `(byte_within_buffer, (row, col_byte))` against the
/// **pre-edit** buffer.
fn position_to_byte_coords(
    buf: &hjkl_buffer::View,
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
    buf: &hjkl_buffer::View,
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
pub enum CursorScrollTarget {
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

/// Uppercase (global) vim marks, keyed by `'A'`–`'Z'`; values are
/// `(buffer_id, row, col)`. Shared across every window's [`Editor`] via
/// `Arc<Mutex<GlobalMarks>>` — see [`Editor::set_global_marks_arc`]. Named so
/// the app host can spell the shared-bank type without repeating the nested
/// generic (mirrors [`crate::Registers`]).
pub type GlobalMarks = std::collections::BTreeMap<char, (u64, usize, usize)>;

pub struct Editor<
    B: crate::types::View = hjkl_buffer::View,
    H: crate::types::Host = crate::types::DefaultHost,
> {
    /// The installed keyboard discipline's FSM state, type-erased (#265 G3).
    ///
    /// The engine never names the concrete type: it only projects a
    /// [`CoarseMode`] and asks for idle resets through
    /// [`DisciplineState`]. The owning discipline crate downcasts through
    /// [`Editor::discipline_mut`] to reach its own state (e.g. `hjkl-vim`'s
    /// `VimState`).
    ///
    /// [`CoarseMode`]: crate::CoarseMode
    /// [`DisciplineState`]: crate::DisciplineState
    discipline: Box<dyn crate::DisciplineState>,
    /// Secondary selections for multi-cursor editing (#63).
    ///
    /// The **primary** selection is not in here: its head stays `View::cursor`
    /// (so the ~130 places across the engine and the disciplines that move the
    /// cursor keep working untouched) and its anchor lives in the discipline's
    /// own state (vim's `visual_anchor`, helix's `anchor`). That asymmetry is
    /// deliberate — see [`crate::selection_shift::Sel`].
    ///
    /// Each entry carries BOTH ends, so an operator can act on a *range* at every
    /// cursor, not just the char under it. [`Editor::mutate_edit`] rewrites both
    /// ends against the pre-edit geometry after every edit, and drops the whole
    /// selection if either end becomes untrackable — never half of one.
    ///
    /// Char columns, matching `View::cursor` and [`hjkl_buffer::Edit`] — NOT
    /// the grapheme columns that `types::Pos` uses.
    ///
    /// Empty for a single-cursor editor, which is every editor today: vim drives
    /// one caret, so this costs an `is_empty()` check per edit and nothing else.
    extra_selections: Vec<crate::selection_shift::Sel>,
    /// Read-only view overlay (git blame, …) layered over the input mode.
    /// Discipline-agnostic engine substrate (#265 G3): hoisted out of
    /// `VimState` because the core edit funnel (`mutate_edit`) and render/chrome
    /// (`is_blame`/`view_mode`) read it, and any discipline can present an
    /// overlay. Orthogonal to the input mode; auto-reset to `Normal` whenever
    /// the input mode leaves Normal (see `drop_blame_if_left_normal`).
    pub(crate) view: crate::ViewMode,
    /// Position of the most recent buffer mutation, recorded by the core edit
    /// funnel ([`Editor::mutate_edit`]). Surfaced via the `'.` / `` `. `` marks.
    /// Discipline-agnostic substrate (#265 G3): the engine-core edit path writes
    /// it and any discipline can offer "back to last edit", so it lives on
    /// `Editor`, not `VimState`.
    pub(crate) last_edit_pos: Option<(usize, usize)>,
    /// Bounded ring of recent edit positions (newest at back), maintained by
    /// `mutate_edit`. `g;` walks toward older, `g,` toward newer. Capped at
    /// [`crate::types::CHANGE_LIST_MAX`]. Substrate — see [`Editor::last_edit_pos`].
    pub(crate) change_list: Vec<(usize, usize)>,
    /// Index into `change_list` while walking; `None` outside a walk (any new
    /// edit clears it and trims forward entries). Substrate.
    pub(crate) change_list_cursor: Option<usize>,
    /// Undo history: each entry is `(joined_document, cursor)` before the
    /// edit. Stored as `Arc<String>` so it shares the
    /// Undo history: snapshots taken via `View::rope()` — `ropey::Rope::clone`
    /// is O(1) (Arc-clone of the B-tree root). Previously stored
    /// `Arc<String>` from `content_joined()`, which on the rope storage
    /// builds the entire document `String` via `rope.to_string()` — that
    /// turned every `i` / `o` keystroke into a ~3 MB allocation on a
    /// 1.86 M-line file.
    // undo_stack, redo_stack, content_dirty, cached_content (as
    // cached_editor_content), pending_fold_ops, change_log,
    // pending_content_edits, pending_content_reset are now stored on
    // Buffer (inside self.buffer) and accessed via View accessor methods.
    /// Last rendered viewport height (text rows only, no chrome). Written
    /// by the draw path via [`set_viewport_height`] so the scroll helpers
    /// can clamp the cursor to stay visible without plumbing the height
    /// through every call.
    pub(super) viewport_height: AtomicU16,
    /// Pending LSP intent set by a normal-mode chord (e.g. `gd` for
    /// goto-definition). The host app drains this each step and fires
    /// the matching request against its own LSP client.
    pub(super) pending_lsp: Option<LspIntent>,
    /// View storage.
    ///
    /// 0.1.0 (Patch C-δ): generic over `B: View` per SPEC §"Editor
    /// surface". Default `B = hjkl_buffer::View`. The vim FSM body
    /// and `Editor::mutate_edit` are concrete on `hjkl_buffer::View`
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
    pub(crate) registers: std::sync::Arc<std::sync::Mutex<crate::registers::Registers>>,
    /// Per-row syntax styling in engine-native form. Always present —
    /// populated by [`Editor::install_syntax_spans`]. Ratatui hosts use
    /// `hjkl_engine_tui::EditorRatatuiExt::install_ratatui_syntax_spans`.
    pub styled_spans: Vec<Vec<(usize, usize, crate::types::Style)>>,
    /// Per-editor settings tweakable via `:set`. Exposed by reference
    /// so handlers (indent, search) read the live value rather than a
    /// snapshot taken at startup. Read via [`Editor::settings`];
    /// mutate via [`Editor::settings_mut`].
    pub(crate) settings: Settings,
    /// Global (uppercase) marks that carry a `buffer_id` so they can jump
    /// across buffers. Keyed by `'A'`–`'Z'`; values are
    /// `(buffer_id, row, col)`. Set by `m{A-Z}`, resolved by
    /// `try_goto_mark_line` / `try_goto_mark_char`.
    ///
    /// Shared via `Arc<Mutex<_>>` across every window's `Editor` (mirrors
    /// [`Editor::registers`]) — vim's uppercase marks are session-global, so
    /// setting `mA` in one split and jumping `'A` from another must see the
    /// same map. Internal — read/mutated via [`Editor::global_mark`] /
    /// [`Editor::set_global_mark`] / [`Editor::global_marks_iter`]; wired by
    /// [`Editor::set_global_marks_arc`].
    pub(crate) global_marks: std::sync::Arc<std::sync::Mutex<GlobalMarks>>,

    // ── Navigation history / viewport (discipline-agnostic, #265) ────────────
    //
    // Hoisted off `VimState` because they are not vim concepts: a jumplist is
    // navigation history (VSCode's Go Back / Go Forward wants the same list),
    // and the viewport flags are render state. A future helix/vscode
    // discipline needs these without depending on hjkl-vim, so they live on
    // the engine seam.
    /// Positions pushed on "big" motions. Newest at the back — `Ctrl-o` pops
    /// from here.
    pub(crate) jump_back: Vec<(usize, usize)>,
    /// Forward stack, refilled by `Ctrl-o` so `Ctrl-i` can return.
    pub(crate) jump_fwd: Vec<(usize, usize)>,
    /// When set, the viewport does not scroll-follow the cursor.
    pub(crate) viewport_pinned: bool,
    /// One-shot hint that the last scroll should be animated by the renderer.
    pub(crate) scroll_anim_hint: bool,

    // ── Search state (discipline-agnostic, #265) ─────────────────────────────
    //
    // Every editor has find. A vscode/helix discipline needs the pattern,
    // direction and history without depending on hjkl-vim.
    /// Live `/` or `?` prompt while the user is typing a pattern.
    pub(crate) search_prompt: Option<crate::search::SearchPrompt>,
    /// Last committed search pattern, for `n` / `N` (or Find Next).
    pub(crate) last_search: Option<String>,
    /// Direction of the last committed search.
    pub(crate) last_search_forward: bool,
    /// Search history, oldest first.
    pub(crate) search_history: Vec<String>,
    /// Cursor while walking search history with Up/Down.
    pub(crate) search_history_cursor: Option<usize>,

    // ── Input timing (discipline-agnostic) ───────────────────────────────────
    //
    // Any chorded FSM needs a timeout clock, not just vim.
    /// Instant of the last input, when the host supplies a monotonic clock.
    pub(crate) last_input_at: Option<std::time::Instant>,
    /// Host-supplied elapsed time at the last input (no_std hosts).
    pub(crate) last_input_host_at: Option<core::time::Duration>,

    /// Last `:s` command, for `:&` / `:&&`. This is ex-command state owned by
    /// the hjkl-ex seam, not vim FSM state.
    ///
    /// Shared via `Arc<Mutex<_>>` across every window's `Editor` (mirrors
    /// [`Editor::global_marks`]) — vim's last substitute is session-global,
    /// so running `:s` in one split and `:&` in another must see the same
    /// command. Internal — read/mutated via [`Editor::last_substitute`] /
    /// [`Editor::set_last_substitute`]; wired by
    /// [`Editor::set_last_substitute_arc`].
    pub(crate) last_substitute:
        std::sync::Arc<std::sync::Mutex<Option<crate::substitute::SubstituteCmd>>>,

    // ── Autopair / abbreviations (discipline-agnostic, #265) ─────────────────
    //
    // Neither is a vim concept. Autopair is an editor feature gated by
    // `Settings::autopair` (VSCode has it too), and the abbreviation table is
    // driven by hjkl-ex's `:abbreviate` / `:iabbrev` — hjkl-ex is in fact the
    // only caller of the add/remove/clear accessors.
    /// Close-brackets queued by autopair, as `(row, col, ch)`. Typing the
    /// matching close char consumes the queued one instead of inserting.
    pub(crate) pending_closes: Vec<(usize, usize, char)>,
    /// Active abbreviation table (insert-mode + cmdline entries).
    pub(crate) abbrevs: Vec<crate::abbrev::Abbrev>,

    /// Whether the unnamed register's current content is linewise. This is
    /// register metadata, not vim FSM state — any discipline that yanks and
    /// pastes needs it (#265).
    pub(crate) yank_linewise: bool,

    /// The `buffer_id` this editor instance is currently attached to.
    /// Updated by the host app on every `switch_to` / slot creation so
    /// global-mark writes record the correct id without requiring the app
    /// to pass the id on every keystroke.
    pub(crate) current_buffer_id: u64,
    // change_log moved to Buffer; accessed via self.buffer.take_change_log() etc.
    /// Vim's "sticky column" (curswant). `None` before the first
    /// motion — the next vertical motion bootstraps from the live
    /// cursor column. Horizontal motions refresh this to the new
    /// column; vertical motions read it back so bouncing through a
    /// shorter row doesn't drag the cursor to col 0. Hoisted out of
    /// `hjkl_buffer::View` (and `VimState`) in 0.0.28 — Editor is
    /// the single owner now. View motion methods that need it
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
    pub(crate) last_emitted_mode: crate::CoarseMode,
    /// Search FSM state (pattern + per-row match cache + wrapscan).
    /// 0.0.35: relocated out of `hjkl_buffer::View` per
    /// `DESIGN_33_METHOD_CLASSIFICATION.md` step 1.
    /// 0.0.37: the buffer-side bridge (`View::search_pattern`) is
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
    /// 0.0.37: lifted out of `hjkl_buffer::View` per step 3 of
    /// `DESIGN_33_METHOD_CLASSIFICATION.md`. The buffer-side cache +
    /// `View::set_spans` / `View::spans` accessors are gone.
    pub(crate) buffer_spans: Vec<Vec<hjkl_buffer::Span>>,
    // pending_content_edits and pending_content_reset moved to Buffer;
    // accessed via self.buffer.take_pending_content_edits() etc.
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
    /// When `false`, ALL buffer modifications are blocked, including entering
    /// insert/replace mode. Matches vim's `:set nomodifiable` / `:set noma`.
    /// Default `true`.
    pub modifiable: bool,
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
    /// How folds are automatically generated. Default `Expr` (tree-sitter).
    /// Alias `fdm`. Matches vim's `:set foldmethod`.
    pub foldmethod: crate::types::FoldMethod,
    /// Enable automatic folds. Default `true`. Alias `fen`.
    /// Matches vim's `:set foldenable`.
    pub foldenable: bool,
    /// Level at which auto-folds start open. `99` = all open (default). Alias `fls`.
    /// Matches vim's `:set foldlevelstart`.
    pub foldlevelstart: u32,
    /// Open/close markers for `foldmethod=marker`, comma-separated `open,close`.
    /// Matches vim's `:set foldmarker` / `fmr`. Default `"{{{,}}}"`.
    pub foldmarker: String,
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
    /// Program run by `:make` (vim's `makeprg`). Its stdout+stderr are parsed
    /// via the errorformat into the quickfix list. Default `"cargo check"`.
    pub makeprg: String,
    /// Comma-separated list of errorformat patterns used by `:cexpr` /
    /// `:lgetexpr` etc. to parse text into quickfix entries. Follows vim's
    /// `'errorformat'` / `'efm'`. Default: `"%f:%l:%c:%m,%f:%l:%m,%l:%c:%m"`.
    pub errorformat: String,
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
    /// Auto-reload a clean buffer when its file changes on disk. Matches vim's
    /// `:set autoread`. Default `true`. Consumed by the host's `:checktime`.
    pub autoreload: bool,
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
    /// Show Nerd-Font filetype icons in the tabline. `:set tabline_icons` /
    /// `:set notabline_icons`. Default `true`.
    pub tabline_icons: bool,
    /// Show inline git blame as end-of-line virtual text on the cursor line
    /// (gitsigns-style). Default `true`. (#202)
    pub blame_inline: bool,
    /// Inline diagnostic ghost-text mode (Error-Lens style `// message` at the
    /// end of the line). Default [`crate::types::DiagInlineMode::All`].
    pub diagnostics_inline: crate::types::DiagInlineMode,
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
    /// Enable helix-style rainbow bracket coloring. hjkl-specific. Default `true`.
    pub rainbow_brackets: bool,
    /// Milliseconds of inactivity before swap-file write. Default `4000`.
    /// Matches Vim's `updatetime`; alias `ut`.
    pub updatetime: u32,
    /// Highlight matching bracket pair under the cursor. hjkl-specific. Default `true`.
    /// `:set nomatchparen` / `:set mps` to toggle. Only the char-scan path
    /// (C-style brackets) is active; tag-pair matching is pending #240.
    pub matchparen: bool,
    /// Smooth-scroll animation duration for page/recenter motions, ms.
    /// `:set scroll_duration_ms`. Default `0` (instant — animation off).
    pub scroll_duration_ms: u16,
    /// When `true`, char-wise Visual selections are treated as
    /// **half-open** (exclusive end): the cell at the cursor/head position
    /// is NOT included in the selection. This matches VSCode / kakoune
    /// bar-cursor semantics where the caret sits *between* characters.
    /// Default `false` (vim inclusive). The vim oracle path must leave this
    /// at `false`; set it programmatically for VSCode keybinding mode.
    pub selection_exclusive: bool,
    /// How coarsely a single `u` (or Ctrl+Z) step walks back through
    /// changes made during an insert session.
    ///
    /// - `InsertSession` (default, vim parity): one undo step reverts the
    ///   entire session from `i` to `<Esc>`. This is byte-identical to
    ///   vim's behaviour and must never be changed for the vim path.
    /// - `Word`: mid-session undo breaks are inserted at word boundaries
    ///   (non-whitespace char following whitespace, or a newline). One
    ///   step of `u` then reverts roughly one word of typing at a time —
    ///   matching VSCode's "edit-chunked Ctrl+Z" experience.
    ///
    /// The vim oracle path **must** leave this at `InsertSession`.
    /// VSCode keybinding mode sets it to `Word` via
    /// `propagate_vscode_settings`. Other future FSMs may choose freely.
    pub undo_granularity: UndoGranularity,
}

/// Controls the granularity of per-insert-session undo steps.
///
/// Discipline-agnostic: vim uses `InsertSession`, VSCode uses `Word`.
/// Future FSMs (emacs, kakoune, …) may adopt either or add new variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UndoGranularity {
    /// One `u` step reverts the entire insert session (vim default).
    #[default]
    InsertSession,
    /// Mid-session undo breaks at word boundaries (non-whitespace after
    /// whitespace, or newline). Matches VSCode's Ctrl+Z granularity.
    Word,
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
            modifiable: true,
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
            foldmethod: crate::types::FoldMethod::Expr,
            foldenable: true,
            foldlevelstart: 99,
            foldmarker: "{{{,}}}".to_string(),
            colorcolumn: String::new(),
            formatoptions: "ro".to_string(),
            filetype: String::new(),
            commentstring: String::new(),
            makeprg: "cargo check".to_string(),
            errorformat: "%f:%l:%c:%m,%f:%l:%m,%l:%c:%m".to_string(),
            autopair: true,
            autoclose_tag: true,
            scrolloff: 5,
            sidescrolloff: 0,
            autoreload: true,
            motion_sneak: true,
            list: false,
            tabline_icons: true,
            blame_inline: true,
            diagnostics_inline: crate::types::DiagInlineMode::All,
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
            format_on_save: true,
            trim_trailing_whitespace: false,
            rainbow_brackets: true,
            updatetime: 4000,
            matchparen: true,
            scroll_duration_ms: 0,
            selection_exclusive: false,
            undo_granularity: UndoGranularity::InsertSession,
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
        modifiable: o.modifiable,
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
        foldmethod: o.foldmethod,
        foldenable: o.foldenable,
        foldlevelstart: o.foldlevelstart,
        foldmarker: o.foldmarker.clone(),
        colorcolumn: o.colorcolumn.clone(),
        formatoptions: o.formatoptions.clone(),
        filetype: o.filetype.clone(),
        commentstring: String::new(),
        makeprg: "cargo check".to_string(),
        errorformat: "%f:%l:%c:%m,%f:%l:%m,%l:%c:%m".to_string(),
        autopair: true,
        autoclose_tag: true,
        scrolloff: o.scrolloff,
        sidescrolloff: o.sidescrolloff,
        autoreload: o.autoreload,
        motion_sneak: o.motion_sneak,
        list: o.list,
        tabline_icons: true,
        blame_inline: true,
        diagnostics_inline: crate::types::DiagInlineMode::All,
        listchars: o.listchars.clone(),
        indent_guides: o.indent_guides,
        indent_guide_char: o.indent_guide_char,
        colorizer: o.colorizer,
        colorizer_filetypes: o.colorizer_filetypes.clone(),
        format_on_save: o.format_on_save,
        trim_trailing_whitespace: o.trim_trailing_whitespace,
        rainbow_brackets: o.rainbow_brackets,
        updatetime: o.updatetime,
        matchparen: o.matchparen,
        scroll_duration_ms: 0,
        // `selection_exclusive` is not part of `Options` — it is set
        // programmatically by the host (e.g. VSCode keybinding mode via
        // `propagate_vscode_settings`). Default to `false` (vim inclusive).
        selection_exclusive: false,
        // `undo_granularity` is not part of `Options` — set programmatically
        // by the host. Default: `InsertSession` (vim parity).
        undo_granularity: UndoGranularity::InsertSession,
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

impl<H: crate::types::Host> Editor<hjkl_buffer::View, H> {
    /// Build an [`Editor`] from a buffer, host adapter, and SPEC options.
    ///
    /// 0.1.0 (Patch C-δ): canonical, frozen constructor per SPEC §"Editor
    /// surface". Replaces the pre-0.1.0 `Editor::new(KeybindingMode)` /
    /// `with_host` / `with_options` triad — there is no shim.
    ///
    /// Consumers that don't need a custom host pass
    /// [`crate::types::DefaultHost::new()`]; consumers that don't need
    /// custom options pass [`crate::types::Options::default()`].
    pub fn new(buffer: hjkl_buffer::View, host: H, options: crate::types::Options) -> Self {
        let settings = settings_from_options(&options);
        Self {
            // No discipline: the engine cannot name one. Callers that want vim
            // keys build through `hjkl_vim::vim_editor` (or call
            // `hjkl_vim::install_vim_discipline`), which fills this slot.
            discipline: Box::new(crate::NoDiscipline),
            extra_selections: Vec::new(),
            view: crate::ViewMode::default(),
            last_edit_pos: None,
            change_list: Vec::new(),
            change_list_cursor: None,
            viewport_height: AtomicU16::new(0),
            pending_lsp: None,
            buffer,
            style_table: Vec::new(),
            registers: std::sync::Arc::new(std::sync::Mutex::new(
                crate::registers::Registers::default(),
            )),
            styled_spans: Vec::new(),
            settings,
            global_marks: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::BTreeMap::new(),
            )),
            jump_back: Vec::new(),
            jump_fwd: Vec::new(),
            viewport_pinned: false,
            scroll_anim_hint: false,
            search_prompt: None,
            last_search: None,
            last_search_forward: true,
            search_history: Vec::new(),
            search_history_cursor: None,
            last_input_at: None,
            last_input_host_at: None,
            last_substitute: std::sync::Arc::new(std::sync::Mutex::new(None)),
            pending_closes: Vec::new(),
            abbrevs: Vec::new(),
            yank_linewise: false,
            current_buffer_id: 0,
            sticky_col: None,
            host,
            last_emitted_mode: crate::CoarseMode::Normal,
            search_state: crate::search::SearchState::new(),
            buffer_spans: Vec::new(),
            last_indent_range: None,
        }
    }
}

impl<B: crate::types::View, H: crate::types::Host> Editor<B, H> {
    /// Borrow the buffer (typed `&B`). Host renders through this via
    /// `hjkl_buffer::BufferView` when `B = hjkl_buffer::View`.
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

impl<H: crate::types::Host> Editor<hjkl_buffer::View, H> {
    /// Update the active `iskeyword` spec for word motions
    /// (`w`/`b`/`e`/`ge` and engine-side `*`/`#` pickup). 0.0.28
    /// hoisted iskeyword storage out of `View` — `Editor` is the
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
        // Coarse, not vim: the engine emits render chrome for whatever
        // discipline is installed (#265).
        let mode = self.coarse_mode();
        if mode == self.last_emitted_mode {
            return;
        }
        let exclusive = self.settings.selection_exclusive;
        let shape = match mode {
            crate::CoarseMode::Insert => crate::types::CursorShape::Bar,
            // VSCode: exclusive-visual also uses a bar caret (caret between chars).
            crate::CoarseMode::Select if exclusive => crate::types::CursorShape::Bar,
            _ => crate::types::CursorShape::Block,
        };
        self.host.emit_cursor_shape(shape);
        self.last_emitted_mode = mode;
    }

    /// Record a yank/cut payload. Forwards the text to
    /// [`crate::types::Host::write_clipboard`] so the platform-clipboard
    /// integration can store or transmit it.
    pub fn record_yank_to_host(&mut self, text: String) {
        self.host.write_clipboard(text);
    }

    /// Vim's sticky column (curswant). `None` before the first motion;
    /// hosts shouldn't normally need to read this directly — it's
    /// surfaced for migration off `View::sticky_col` and for
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
        self.buffer.mark(c)
    }

    /// Set the named mark `c` to `(row, col)`. Used by the FSM's
    /// `m{a-zA-Z}` keystroke and by [`Editor::restore_snapshot`].
    pub fn set_mark(&mut self, c: char, pos: (usize, usize)) {
        self.buffer.set_mark(c, pos);
    }

    /// Remove the named mark `c` (no-op if unset).
    pub fn clear_mark(&mut self, c: char) {
        self.buffer.clear_mark(c);
    }

    /// Look up an uppercase global mark by letter. Returns
    /// `(buffer_id, row, col)` if set; `None` otherwise.
    pub fn global_mark(&self, c: char) -> Option<(u64, usize, usize)> {
        self.global_marks.lock().unwrap().get(&c).copied()
    }

    /// Set an uppercase global mark `c` to `(buffer_id, row, col)`.
    pub fn set_global_mark(&mut self, c: char, buffer_id: u64, pos: (usize, usize)) {
        self.global_marks
            .lock()
            .unwrap()
            .insert(c, (buffer_id, pos.0, pos.1));
    }

    /// Point this editor at a shared global-marks bank. All editors in the
    /// app share one bank (mirrors [`Editor::set_registers_arc`]) so
    /// uppercase marks set in one window/split are visible from every other
    /// window — vim's `mA`/`'A` are session-global, not per-window.
    pub fn set_global_marks_arc(
        &mut self,
        global_marks: std::sync::Arc<std::sync::Mutex<GlobalMarks>>,
    ) {
        self.global_marks = global_marks;
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
    pub fn global_marks_iter(&self) -> Vec<(char, u64, usize, usize)> {
        self.global_marks
            .lock()
            .unwrap()
            .iter()
            .map(|(c, &(bid, r, col))| (*c, bid, r, col))
            .collect()
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
        self.buffer.pop_undo_entry().is_some()
    }

    /// Read all named marks set this session — both lowercase
    /// (`'a`–`'z`) and uppercase (`'A`–`'Z`). Iteration is
    /// deterministic (BTreeMap-ordered) so snapshot / `:marks`
    /// output is stable.
    pub fn marks(&self) -> impl Iterator<Item = (char, (usize, usize))> {
        self.buffer.marks_cloned().into_iter()
    }

    /// Read all buffer-local lowercase marks. Kept for source
    /// compatibility with pre-0.0.36 callers (e.g. `:marks` ex
    /// command); new code should use [`Editor::marks`] which
    /// iterates the unified map.
    #[deprecated(
        since = "0.0.36",
        note = "use Editor::marks — lowercase + uppercase marks now live in a single map"
    )]
    pub fn buffer_marks(&self) -> impl Iterator<Item = (char, (usize, usize))> {
        self.buffer
            .marks_cloned()
            .into_iter()
            .filter(|(c, _)| c.is_ascii_lowercase())
    }

    /// Position of the last edit (where `.` would replay). `None` if
    /// no edit has happened yet in this session.
    pub fn last_edit_pos(&self) -> Option<(usize, usize)> {
        self.last_edit_pos
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
    pub fn file_marks(&self) -> impl Iterator<Item = (char, (usize, usize))> {
        self.buffer
            .marks_cloned()
            .into_iter()
            .filter(|(c, _)| c.is_ascii_uppercase())
    }

    /// Read-only view of the cached syntax-derived block ranges that
    /// `:foldsyntax` consumes. Returns the slice the host last
    /// installed via [`Editor::set_syntax_fold_ranges`]; empty when
    /// no syntax integration is active.
    pub fn syntax_fold_ranges(&self) -> Vec<(usize, usize)> {
        self.buffer.syntax_fold_ranges_cloned()
    }

    pub fn set_syntax_fold_ranges(&mut self, ranges: Vec<(usize, usize)>) {
        self.buffer.set_syntax_fold_ranges(ranges);
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

    /// Returns `true` when the buffer is modifiable (default). When `false`
    /// (`:set nomodifiable`), ALL edits and insert-mode entry are blocked.
    pub fn is_modifiable(&self) -> bool {
        self.settings.modifiable
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
    /// Opens any fold hiding the match row (vim-correct: search reveals folds).
    pub fn search_advance_forward(&mut self, skip_current: bool) -> bool {
        let found =
            crate::search::search_forward(&mut self.buffer, &mut self.search_state, skip_current);
        if found {
            let row = crate::types::Cursor::cursor(&self.buffer).line as usize;
            self.buffer.reveal_row(row);
        }
        found
    }

    /// Drive `N` — symmetric counterpart of [`Editor::search_advance_forward`].
    /// Opens any fold hiding the match row (vim-correct: search reveals folds).
    pub fn search_advance_backward(&mut self, skip_current: bool) -> bool {
        let found =
            crate::search::search_backward(&mut self.buffer, &mut self.search_state, skip_current);
        if found {
            let row = crate::types::Cursor::cursor(&self.buffer).line as usize;
            self.buffer.reveal_row(row);
        }
        found
    }

    /// Snapshot of the unnamed register (the default `p` / `P` source).
    pub fn yank(&self) -> String {
        self.registers.lock().unwrap().unnamed.text.clone()
    }

    /// Borrow the full register bank — `"`, `"0`–`"9`, `"a`–`"z`.
    pub fn registers(&self) -> std::sync::MutexGuard<'_, crate::registers::Registers> {
        self.registers.lock().unwrap()
    }

    /// Mutably borrow the full register bank. Returns a guard so callers
    /// can mutate in place. Signature changed from `&mut self` to `&self`
    /// because the interior mutability is now via `Arc<Mutex<>>`.
    pub fn registers_mut(&self) -> std::sync::MutexGuard<'_, crate::registers::Registers> {
        self.registers.lock().unwrap()
    }

    /// Point this editor at a shared register bank. All editors in the
    /// app share one bank so yank/paste work cross-buffer without copying.
    pub fn set_registers_arc(
        &mut self,
        registers: std::sync::Arc<std::sync::Mutex<crate::registers::Registers>>,
    ) {
        self.registers = registers;
    }

    /// Host hook: load the OS clipboard's contents into the `"+` / `"*`
    /// register slot. the host calls this before letting vim consume a
    /// paste so `"*p` / `"+p` reflect the live clipboard rather than a
    /// stale snapshot from the last yank.
    pub fn sync_clipboard_register(&mut self, text: String, linewise: bool) {
        self.registers.lock().unwrap().set_clipboard(text, linewise);
    }

    /// Read-only view of the change list (positions of recent edits) plus
    /// the current walk cursor. Newest entry is at the back.
    pub fn change_list(&self) -> (&[(usize, usize)], Option<usize>) {
        (&self.change_list, self.change_list_cursor)
    }

    /// Replace the unnamed register without touching any other slot.
    /// For host-driven imports (e.g. system clipboard); operator
    /// code uses [`record_yank`] / [`record_delete`].
    pub fn set_yank(&mut self, text: impl Into<String>) {
        let text = text.into();
        let linewise = self.yank_linewise;
        self.registers.lock().unwrap().unnamed = crate::registers::Slot { text, linewise };
    }

    /// Record a yank into `"` and `"0`, plus the named target if the
    /// user prefixed `"reg`. Updates `vim.yank_linewise` for the
    /// paste path.
    pub fn record_yank(&mut self, text: String, linewise: bool, target: Option<char>) {
        self.yank_linewise = linewise;
        self.registers
            .lock()
            .unwrap()
            .record_yank(text, linewise, target);
    }

    /// Direct write to a named register slot — bypasses the unnamed
    /// `"` and `"0` updates that `record_yank` does. Used by the
    /// macro recorder so finishing a `q{reg}` recording doesn't
    /// pollute the user's last yank.
    pub fn set_named_register_text(&mut self, reg: char, text: String) {
        let mut regs = self.registers.lock().unwrap();
        if let Some(slot) = match reg {
            'a'..='z' => Some(&mut regs.named[(reg as u8 - b'a') as usize]),
            'A'..='Z' => Some(&mut regs.named[(reg.to_ascii_lowercase() as u8 - b'a') as usize]),
            _ => None,
        } {
            slot.text = text;
            slot.linewise = false;
        }
    }

    /// Record a delete / change into `"` and, by size, the `"1`–`"9`
    /// ring or the `"-` small-delete register. Honours the active
    /// named-register prefix.
    pub fn record_delete(&mut self, text: String, linewise: bool, target: Option<char>) {
        self.yank_linewise = linewise;
        self.registers
            .lock()
            .unwrap()
            .record_delete(text, linewise, target);
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
    /// the buffer. Now that View is the cursor authority this is a
    /// no-op; call sites can remain in place during the migration.
    pub fn push_buffer_cursor_to_textarea(&mut self) {}

    /// Force the host viewport's top row without touching the
    /// cursor. Used by tests that simulate a scroll without the
    /// SCROLLOFF cursor adjustment that `scroll_down` / `scroll_up`
    /// apply.
    ///
    /// 0.0.34 (Patch C-δ.1): writes through `Host::viewport_mut`
    /// instead of the (now-deleted) `View::viewport_mut`.
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

    /// The character under the cursor, or `None` at/after end of line (or on
    /// an empty line). Used by callers that need vim's on-blank distinctions
    /// (e.g. `cw` only acts like `ce` when the cursor is on a non-blank).
    pub fn char_at_cursor(&self) -> Option<char> {
        let (row, col) = self.cursor();
        crate::buf_helpers::buf_line(&self.buffer, row).and_then(|l| l.chars().nth(col))
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
    /// in-tree [`hjkl_buffer::View`] fold storage via
    /// [`crate::buffer_impl::BufferFoldProviderMut`], so hosts that
    /// don't track folds independently can ignore the queue
    /// (or simply never call this drain).
    ///
    /// Introduced in 0.0.38 (Patch C-δ.4).
    pub fn take_fold_ops(&mut self) -> Vec<crate::types::FoldOp> {
        self.buffer.take_fold_ops()
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
        self.buffer.push_fold_op(op);
        let mut provider = crate::buffer_impl::BufferFoldProviderMut::new(&mut self.buffer);
        provider.apply(op);
        // BUG 2 fix: after a close/toggle-that-closes, the cursor may sit on a
        // hidden row (inside the fold body). Vim snaps the cursor to the fold's
        // first line (start_row). Do it here so every call site — keyboard `za`/
        // `zc` AND the gutter-click path — converges on the same behaviour.
        let cursor_row = buf_cursor_row(&self.buffer);
        if self.buffer.is_row_hidden(cursor_row)
            && let Some(fold) = self.buffer.fold_at_row(cursor_row)
        {
            let snap_row = fold.start_row;
            buf_set_cursor_rc(&mut self.buffer, snap_row, 0);
            self.sticky_col = Some(0);
        }
    }

    /// Refresh the host viewport's height from the cached
    /// `viewport_height_value()`. Called from the per-step
    /// boilerplate; was the textarea → buffer mirror before Phase 7f
    /// put View in charge. 0.0.28 hoisted sticky_col out of
    /// `View`. 0.0.34 (Patch C-δ.1) routes the height write through
    /// `Host::viewport_mut`.
    pub fn sync_buffer_from_textarea(&mut self) {
        let height = self.viewport_height_value();
        self.host.viewport_mut().height = height;
    }

    /// Was the full textarea → buffer content sync. View is the
    /// content authority now; this remains as a no-op so the per-step
    /// call sites don't have to be ripped in the same patch.
    pub fn sync_buffer_content_from_textarea(&mut self) {
        self.sync_buffer_from_textarea();
    }

    /// Push a `(row, col)` onto the back-jumplist so `Ctrl-o` returns
    /// to it later. Used by host-driven jumps (e.g. `gd`) that move
    /// the cursor without going through the vim engine's motion
    /// machinery, where push_jump fires automatically.
    pub fn record_jump(&mut self, pos: (usize, usize)) {
        const JUMPLIST_MAX: usize = 100;
        self.jump_back.push(pos);
        if self.jump_back.len() > JUMPLIST_MAX {
            self.jump_back.remove(0);
        }
        self.jump_fwd.clear();
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
        // `nomodifiable` OR the BLAME view overlay short-circuits every
        // mutation funnel: no buffer change, no dirty flag, no undo entry,
        // no change-log emission. We swallow the requested `edit` and hand
        // back a self-inverse no-op (`InsertStr` of an empty string at the
        // current cursor) so callers that push the return value onto an undo
        // stack still get a structurally valid round trip.
        // Note: `readonly` no longer blocks edits here — it only gates `:w`.
        if !self.settings.modifiable || self.view == crate::ViewMode::Blame {
            let _ = edit;
            return hjkl_buffer::Edit::InsertStr {
                at: buf_cursor_pos(&self.buffer),
                text: String::new(),
            };
        }
        // Multi-cursor (#63): every edit cascades, so the secondary selections
        // have to be rewritten against the *pre-edit* geometry or they end up
        // pointing at the wrong text. This is the single edit funnel, so doing it
        // here covers every mutation in the engine by construction. BOTH ends move
        // together, and a selection the shift cannot track exactly is dropped
        // whole, never guessed and never half-tracked — see `selection_shift`.
        if !self.extra_selections.is_empty() {
            let edit_ref = &edit;
            // `JoinLines` geometry depends on how long each row was *before* the
            // join, so the metrics have to be read here — after `apply_buffer_edit`
            // they describe the wrong buffer.
            let rows = buf_row_count(&self.buffer);
            let lens: Vec<usize> = (0..rows).map(|r| buf_line_chars(&self.buffer, r)).collect();
            self.extra_selections.retain_mut(|s| {
                match crate::selection_shift::shift_sel(
                    *s,
                    edit_ref,
                    |r| lens.get(r).copied().unwrap_or(0),
                    rows,
                ) {
                    Some(shifted) => {
                        *s = shifted;
                        true
                    }
                    None => false,
                }
            });
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
        self.buffer.extend_change_log(edit_to_editops(&edit));
        // Compute ContentEdit fan-out from the pre-edit buffer state.
        // Done before `apply_buffer_edit` consumes `edit` so we can
        // inspect the operation's fields and the buffer's pre-edit row
        // bytes (needed for byte_of_row / col_byte conversion). Edits
        // are pushed onto pending_content_edits for host drain.
        let content_edits = content_edits_from_buffer_edit(&self.buffer, &edit);
        self.buffer.extend_pending_content_edits(content_edits);
        // 0.0.42 (Patch C-δ.7): the `apply_edit` reach is centralized
        // in [`crate::buf_helpers::apply_buffer_edit`] (option (c) of
        // the 0.0.42 plan — see that fn's doc comment). The free fn
        // takes `&mut hjkl_buffer::View` so the editor body itself
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
        self.last_edit_pos = Some((pre_edit_row, pre_edit_col));
        // Append to the change-list ring (skip when the cursor sits on
        // the same cell as the last entry — back-to-back keystrokes on
        // one column shouldn't pollute the ring). A new edit while
        // walking the ring trims the forward half, vim style.
        let entry = (pos_row, pos_col);
        if self.change_list.last() != Some(&entry) {
            if let Some(idx) = self.change_list_cursor.take() {
                self.change_list.truncate(idx + 1);
            }
            self.change_list.push(entry);
            let len = self.change_list.len();
            if len > crate::types::CHANGE_LIST_MAX {
                self.change_list
                    .drain(0..len - crate::types::CHANGE_LIST_MAX);
            }
        }
        self.change_list_cursor = None;
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

        self.buffer
            .rebase_marks(edit_start, drop_end, shift_threshold, delta);

        // Shift global marks that belong to the current buffer.
        let cur_bid = self.current_buffer_id;
        let mut global_marks = self.global_marks.lock().unwrap();
        let mut global_to_drop: Vec<char> = Vec::new();
        for (c, (bid, row, _col)) in global_marks.iter_mut() {
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
            global_marks.remove(&c);
        }
        drop(global_marks);

        let shift_jumps = |entries: &mut Vec<(usize, usize)>| {
            entries.retain(|(row, _)| !(edit_start..drop_end).contains(row));
            for (row, _) in entries.iter_mut() {
                if *row >= shift_threshold {
                    *row = ((*row as isize) + delta).max(0) as usize;
                }
            }
        };
        shift_jumps(&mut self.jump_back);
        shift_jumps(&mut self.jump_fwd);
    }

    /// Reverse-sync helper paired with [`Editor::mutate_edit`]: rebuild
    /// the textarea from the buffer's lines + cursor, preserving yank
    /// text. Heavy (allocates a fresh `TextArea`) but correct; the
    /// textarea field disappears at the end of Phase 7f anyway.
    /// No-op since View is the content authority. Retained as a
    /// shim so call sites in `mutate_edit` and friends don't have to
    /// be ripped in lockstep with the field removal.
    pub(crate) fn push_buffer_content_to_textarea(&mut self) {}

    /// Single choke-point for "the buffer just changed". Sets the
    /// dirty flag and drops the cached `content_arc` snapshot so
    /// subsequent reads rebuild from the live textarea. Callers
    /// mutating `textarea` directly (e.g. the TUI's bracketed-paste
    /// path) must invoke this to keep the cache honest.
    pub fn mark_content_dirty(&mut self) {
        self.buffer.mark_content_dirty();
    }

    /// Returns true if content changed since the last call, then clears the flag.
    pub fn take_dirty(&mut self) -> bool {
        self.buffer.take_dirty()
    }

    /// Drain the one-shot smooth-scroll hint (#195). True if the last step ran
    /// a page/recenter motion the app may animate.
    pub fn take_scroll_anim_hint(&mut self) -> bool {
        let h = self.scroll_anim_hint;
        self.scroll_anim_hint = false;
        h
    }

    // ── Jumplist / viewport-pin (discipline-agnostic seam, #265) ─────────────
    //
    // Navigation history and viewport pinning are not vim concepts — VSCode's
    // Go Back / Go Forward wants the same jumplist, and any discipline can pin
    // the viewport. These accessors live on the engine so a future
    // helix/vscode discipline reaches them without depending on hjkl-vim. The
    // vim *keybindings* on top (`Ctrl-o` / `Ctrl-i`) stay in hjkl-vim.

    /// Read-only view of the jumplist as `(jump_back, jump_fwd)`. Newest entry
    /// is at the back of each. Backs `:jumps`.
    #[allow(clippy::type_complexity)]
    pub fn jump_list(&self) -> (&[(usize, usize)], &[(usize, usize)]) {
        (&self.jump_back, &self.jump_fwd)
    }

    /// Position the cursor was at when the user last jumped back. `None`
    /// before any jump.
    pub fn last_jump_back(&self) -> Option<(usize, usize)> {
        self.jump_back.last().copied()
    }

    /// Read-only view of the jump-back stack.
    pub fn jump_back_list(&self) -> &[(usize, usize)] {
        &self.jump_back
    }

    /// Mutable access to the jump-back stack.
    pub fn jump_back_list_mut(&mut self) -> &mut Vec<(usize, usize)> {
        &mut self.jump_back
    }

    /// Read-only view of the jump-forward stack.
    pub fn jump_fwd_list(&self) -> &[(usize, usize)] {
        &self.jump_fwd
    }

    /// Mutable access to the jump-forward stack.
    pub fn jump_fwd_list_mut(&mut self) -> &mut Vec<(usize, usize)> {
        &mut self.jump_fwd
    }

    /// Whether the viewport is pinned (suppresses scroll-follow).
    pub fn viewport_pinned(&self) -> bool {
        self.viewport_pinned
    }

    /// Set the viewport-pinned flag.
    pub fn set_viewport_pinned(&mut self, v: bool) {
        self.viewport_pinned = v;
    }

    /// Queue an LSP intent for the host to service on the next tick.
    pub fn set_pending_lsp(&mut self, intent: Option<crate::editor::LspIntent>) {
        self.pending_lsp = intent;
    }

    /// Record the row range touched by the most recent auto-indent, for the
    /// host to pick up via `take_last_indent_range`.
    pub fn set_last_indent_range(&mut self, range: Option<(usize, usize)>) {
        self.last_indent_range = range;
    }

    /// Walk cursor into the change list (`g;` / `g,`), or `None` when not
    /// walking.
    pub fn change_list_cursor(&self) -> Option<usize> {
        self.change_list_cursor
    }

    /// Set the change-list walk cursor.
    pub fn set_change_list_cursor(&mut self, idx: Option<usize>) {
        self.change_list_cursor = idx;
    }

    /// Arm the one-shot hint that the next scroll should be animated.
    pub fn set_scroll_anim_hint(&mut self, v: bool) {
        self.scroll_anim_hint = v;
    }

    /// Set the read-only view overlay (Normal / Blame).
    pub fn set_view_mode(&mut self, v: crate::ViewMode) {
        self.view = v;
    }

    /// The active abbreviation table.
    pub fn abbrevs(&self) -> &[crate::abbrev::Abbrev] {
        &self.abbrevs
    }

    /// Autopair's queued close-brackets, as `(row, col, ch)`. A discipline's
    /// insert path consumes a queued close when the user types the matching
    /// character instead of inserting a second one.
    pub fn pending_closes(&self) -> &[(usize, usize, char)] {
        &self.pending_closes
    }

    /// Mutable access to autopair's queued close-brackets.
    pub fn pending_closes_mut(&mut self) -> &mut Vec<(usize, usize, char)> {
        &mut self.pending_closes
    }

    /// Whether the unnamed register's content is linewise.
    pub fn yank_linewise(&self) -> bool {
        self.yank_linewise
    }

    /// Set the linewise flag for the unnamed register.
    pub fn set_yank_linewise(&mut self, v: bool) {
        self.yank_linewise = v;
    }

    // ── Search state (discipline-agnostic seam, #265) ────────────────────────
    //
    // Every editor has find. These live on the engine so a helix/vscode
    // discipline reaches the pattern, direction and history without depending
    // on hjkl-vim. The vim *keybindings* on top (`/`, `?`, `n`, `N`, `*`) stay
    // in hjkl-vim.

    /// The live `/` or `?` search-prompt state, if a prompt is open.
    pub fn search_prompt_state(&self) -> Option<&crate::search::SearchPrompt> {
        self.search_prompt.as_ref()
    }

    /// Mutable access to the live search-prompt state.
    pub fn search_prompt_state_mut(&mut self) -> Option<&mut crate::search::SearchPrompt> {
        self.search_prompt.as_mut()
    }

    /// Take (and close) the search-prompt state.
    pub fn take_search_prompt_state(&mut self) -> Option<crate::search::SearchPrompt> {
        self.search_prompt.take()
    }

    /// Install (or clear) the search-prompt state.
    pub fn set_search_prompt_state(&mut self, prompt: Option<crate::search::SearchPrompt>) {
        self.search_prompt = prompt;
    }

    /// The last committed search pattern, for `n` / `N` (or Find Next).
    pub fn last_search_pattern(&self) -> Option<&str> {
        self.last_search.as_deref()
    }

    /// Set the last search pattern without touching direction or highlight.
    pub fn set_last_search_pattern_only(&mut self, pattern: Option<String>) {
        self.last_search = pattern;
    }

    /// Set the last search direction without touching the pattern.
    pub fn set_last_search_forward_only(&mut self, forward: bool) {
        self.last_search_forward = forward;
    }

    /// Read-only view of the search history (oldest first).
    pub fn search_history(&self) -> &[String] {
        &self.search_history
    }

    /// Mutable access to the search history.
    pub fn search_history_mut(&mut self) -> &mut Vec<String> {
        &mut self.search_history
    }

    /// Cursor position while walking search history with Up/Down.
    pub fn search_history_cursor(&self) -> Option<usize> {
        self.search_history_cursor
    }

    /// Set the search-history walk cursor.
    pub fn set_search_history_cursor(&mut self, idx: Option<usize>) {
        self.search_history_cursor = idx;
    }

    // ── Input timing (discipline-agnostic seam) ──────────────────────────────
    //
    // Any chorded FSM needs a timeout clock, not just vim.

    /// Instant of the last input, when the host supplies a monotonic clock.
    pub fn last_input_at(&self) -> Option<std::time::Instant> {
        self.last_input_at
    }

    /// Set the instant of the last input.
    pub fn set_last_input_at(&mut self, t: Option<std::time::Instant>) {
        self.last_input_at = t;
    }

    /// Host-supplied elapsed time at the last input (no_std hosts).
    pub fn last_input_host_at(&self) -> Option<core::time::Duration> {
        self.last_input_host_at
    }

    /// Set the host-supplied elapsed time at the last input.
    pub fn set_last_input_host_at(&mut self, d: Option<core::time::Duration>) {
        self.last_input_host_at = d;
    }

    // ── Scrolling (discipline-agnostic seam, #265) ───────────────────────────
    //
    // Scrolling a viewport is not a vim concept — every discipline does it.
    // These carry zero vim FSM state (the one field they used to touch,
    // `scroll_anim_hint`, now lives on the Editor), so they belong here. The
    // vim *keybindings* on top (`Ctrl-F`/`Ctrl-B`, `Ctrl-D`/`Ctrl-U`,
    // `Ctrl-E`/`Ctrl-Y`) stay in hjkl-vim.

    /// Rows spanned by half a viewport, times `count` (min 1).
    pub fn viewport_half_rows(&self, count: usize) -> usize {
        let h = self.viewport_height_value() as usize;
        (h / 2).max(1).saturating_mul(count.max(1))
    }

    /// Rows spanned by a full viewport (less a two-line overlap), times
    /// `count` (min 1).
    pub fn viewport_full_rows(&self, count: usize) -> usize {
        let h = self.viewport_height_value() as usize;
        h.saturating_sub(2).max(1).saturating_mul(count.max(1))
    }

    /// Move the cursor `delta` rows (clamped to the buffer), landing on the
    /// first non-blank of the target row and resetting the sticky column.
    pub fn scroll_cursor_rows(&mut self, delta: isize) {
        if delta == 0 {
            return;
        }
        self.sync_buffer_content_from_textarea();
        let (row, _) = self.cursor();
        let last_row = buf_row_count(&self.buffer).saturating_sub(1);
        let target = (row as isize + delta).max(0).min(last_row as isize) as usize;
        buf_set_cursor_rc(&mut self.buffer, target, 0);
        crate::motions::move_first_non_blank(&mut self.buffer);
        self.push_buffer_cursor_to_textarea();
        self.sticky_col = Some(buf_cursor_pos(&self.buffer).col);
    }

    /// Scroll the cursor by one full viewport height (height − 2 rows,
    /// preserving a two-line overlap). `count` multiplies the step.
    pub fn scroll_full_page(&mut self, dir: crate::types::ScrollDir, count: usize) {
        self.scroll_anim_hint = true;
        let rows = self.viewport_full_rows(count) as isize;
        match dir {
            crate::types::ScrollDir::Down => self.scroll_cursor_rows(rows),
            crate::types::ScrollDir::Up => self.scroll_cursor_rows(-rows),
        }
    }

    /// Scroll the cursor by half the viewport height. `count` multiplies.
    pub fn scroll_half_page(&mut self, dir: crate::types::ScrollDir, count: usize) {
        self.scroll_anim_hint = true;
        let rows = self.viewport_half_rows(count) as isize;
        match dir {
            crate::types::ScrollDir::Down => self.scroll_cursor_rows(rows),
            crate::types::ScrollDir::Up => self.scroll_cursor_rows(-rows),
        }
    }

    /// Scroll the viewport `count` lines without moving the cursor (the cursor
    /// is clamped into the new visible region if it would fall outside).
    pub fn scroll_line(&mut self, dir: crate::types::ScrollDir, count: usize) {
        let n = count.max(1);
        let total = buf_row_count(&self.buffer);
        let last = total.saturating_sub(1);
        let h = self.viewport_height_value() as usize;
        let cur_top = self.host().viewport().top_row;
        let new_top = match dir {
            crate::types::ScrollDir::Down => (cur_top + n).min(last),
            crate::types::ScrollDir::Up => cur_top.saturating_sub(n),
        };
        self.set_viewport_top(new_top);
        // Clamp cursor to stay within the new visible region.
        let (row, col) = self.cursor();
        let bot = (new_top + h).saturating_sub(1).min(last);
        let clamped = row.max(new_top).min(bot);
        if clamped != row {
            buf_set_cursor_rc(&mut self.buffer, clamped, col);
            self.push_buffer_cursor_to_textarea();
        }
    }

    /// Drain the queue of [`crate::types::ContentEdit`]s emitted since
    /// the last call. Each entry corresponds to a single buffer
    /// mutation funnelled through [`Editor::mutate_edit`]; block edits
    /// fan out to one entry per row touched.
    ///
    /// Hosts call this each frame (after [`Editor::take_content_reset`])
    /// to fan edits into a tree-sitter parser via `Tree::edit`.
    pub fn take_content_edits(&mut self) -> Vec<crate::types::ContentEdit> {
        self.buffer.take_pending_content_edits()
    }

    /// Returns `true` if a bulk buffer replacement happened since the
    /// last call (e.g. `set_content` / `restore` / undo restore), then
    /// clears the flag. When this returns `true`, hosts should drop
    /// any retained syntax tree before consuming
    /// [`Editor::take_content_edits`].
    pub fn take_content_reset(&mut self) -> bool {
        self.buffer.take_pending_content_reset()
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
        if !self.buffer.content_dirty() {
            return None;
        }
        let arc = self.content_arc();
        self.buffer.set_content_dirty(false);
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
        // Screen row delta: delegate to the single fold- and wrap-aware
        // calculator that already drives scrolling + scrolloff, rather than
        // recomputing `pos_row - top_row` here. That naive delta ignored rows
        // collapsed by closed folds, painting the cursor block N rows too low
        // while the (fold-aware) text + line-highlight rendered correctly.
        // One source of truth → no drift between scroll math and cursor math. (#244)
        let folds = crate::buffer_impl::SnapshotFoldProvider::from_buffer(&self.buffer);
        let dy = crate::viewport_math::cursor_screen_row_from(&self.buffer, &folds, v, v.top_row)?
            as u16;
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

    /// Discipline-agnostic coarse mode for app chrome (status badge, cursor
    /// shape). App code that only needs "inserting / selecting / idle" — not the
    /// precise vim mode — should read this so it works identically under any
    /// keybinding discipline (vim, vscode, future helix/emacs). See
    /// [`crate::CoarseMode`] (epic #265 G3). Today this projects from the vim
    /// mode; once FSM state is pluggable each discipline supplies its own.
    pub fn coarse_mode(&self) -> crate::CoarseMode {
        self.discipline.coarse_mode()
    }

    /// The secondary selections, in char columns. Empty for a single-cursor
    /// editor.
    ///
    /// The primary selection is *not* included: its head is [`Editor::cursor`]
    /// and its anchor lives in the discipline — see the `extra_selections` field
    /// docs for why.
    pub fn extra_selections(&self) -> &[crate::selection_shift::Sel] {
        &self.extra_selections
    }

    /// The **heads** of the secondary selections — the carets a user sees.
    ///
    /// Convenience view over [`Editor::extra_selections`] for callers that only
    /// care where the carets are (rendering, tests).
    pub fn extra_cursors(&self) -> Vec<hjkl_buffer::Position> {
        self.extra_selections.iter().map(|s| s.head).collect()
    }

    /// Replace the whole secondary set.
    ///
    /// Selections whose head duplicates the primary head, or an earlier entry's
    /// head, are dropped: two carets on one spot would apply every edit twice at
    /// the same place. Same invariant [`Editor::add_cursor`] enforces, applied to
    /// a bulk write — a discipline recomputing every selection after a motion
    /// (helix does this on every keystroke) must not be able to smuggle a
    /// duplicate in through the back door.
    pub fn set_extra_selections(&mut self, sels: Vec<crate::selection_shift::Sel>) {
        let (row, col) = self.cursor();
        let primary = hjkl_buffer::Position::new(row, col);
        self.extra_selections.clear();
        for s in sels {
            if s.head == primary || self.extra_selections.iter().any(|e| e.head == s.head) {
                continue;
            }
            self.extra_selections.push(s);
        }
    }

    /// Add a secondary selection. Same dedup rule as [`Editor::add_cursor`].
    pub fn add_selection(&mut self, sel: crate::selection_shift::Sel) {
        let (row, col) = self.cursor();
        if sel.head == hjkl_buffer::Position::new(row, col)
            || self.extra_selections.iter().any(|s| s.head == sel.head)
        {
            return;
        }
        self.extra_selections.push(sel);
    }

    /// Add a secondary cursor: a zero-width selection at `pos`. Ignores a
    /// position that duplicates the primary head or an existing secondary head,
    /// so a set never carries two carets at one spot — that would apply an edit
    /// twice at the same place.
    pub fn add_cursor(&mut self, pos: hjkl_buffer::Position) {
        self.add_selection(crate::selection_shift::Sel::caret(pos));
    }

    /// Drop every secondary selection, collapsing back to the primary.
    pub fn clear_extra_cursors(&mut self) {
        self.extra_selections.clear();
    }

    /// Apply an edit at **every** cursor — the primary and all secondaries —
    /// and leave each cursor where its own edit left it (#63).
    ///
    /// `make` is handed each cursor's position and returns the edit to apply
    /// there, so the caller writes the edit once and it fans out:
    ///
    /// ```ignore
    /// ed.edit_at_all_cursors(|at| Edit::InsertStr { at, text: "x".into() });
    /// ```
    ///
    /// Returns the inverse of each applied edit, in application order, so a
    /// caller can push them as one undo step. This does **not** touch the undo
    /// stack itself — `mutate_edit` never does, and a multi-cursor keystroke is
    /// one user action, so the discipline pushes undo once before calling.
    ///
    /// # Why the order matters
    ///
    /// Edits are applied **bottom-up** (last cursor in the document first). An
    /// edit at position P only moves positions at or after P, so working
    /// backwards leaves every not-yet-visited cursor's coordinates still valid.
    /// Going top-down would invalidate them all after the first edit.
    ///
    /// Each cursor that has already been edited is parked in `extra_cursors`,
    /// so [`Editor::mutate_edit`]'s shift keeps it correct as the remaining
    /// (earlier) edits land. The bookkeeping is the same machinery, reused.
    ///
    /// # Degradation
    ///
    /// If any cursor becomes untrackable mid-apply (see `selection_shift`), the
    /// secondaries are dropped and the editor collapses to the primary rather
    /// than carrying on with a caret that no longer knows where it is.
    pub fn edit_at_all_cursors(
        &mut self,
        make: impl Fn(hjkl_buffer::Position) -> hjkl_buffer::Edit,
    ) -> Vec<hjkl_buffer::Edit> {
        let (pr, pc) = self.cursor();
        let primary = hjkl_buffer::Position::new(pr, pc);
        let (inverses, _) = self.edit_at_all_selections(primary, |s| make(s.head));
        inverses
    }

    /// Apply an edit at **every selection** — the primary and all secondaries —
    /// where `make` sees the whole selection, not just its head (#63).
    ///
    /// This is what an operator needs: `d` on three selections has to delete
    /// three *ranges*, and only the caller-visible [`Sel`] carries both ends.
    /// [`Editor::edit_at_all_cursors`] is the caret-only special case of this.
    ///
    /// `primary_anchor` is passed in — and the primary's *new* anchor is returned
    /// — because the primary selection's anchor lives in the discipline's state,
    /// not the engine's (see the `extra_selections` field docs).
    ///
    /// Returns `(inverse of each applied edit in application order, new primary
    /// anchor)`. This does **not** touch the undo stack — `mutate_edit` never
    /// does, and a multi-cursor keystroke is one user action, so the discipline
    /// pushes undo once before calling.
    ///
    /// # Why the order matters
    ///
    /// Edits are applied **bottom-up** (last selection in the document first). An
    /// edit at position P only moves positions at or after P, so working
    /// backwards leaves every not-yet-visited selection's coordinates still valid.
    /// Going top-down would invalidate them all after the first edit.
    ///
    /// Each selection that has already been edited is parked in
    /// `extra_selections`, so [`Editor::mutate_edit`]'s shift keeps it correct as
    /// the remaining (earlier) edits land.
    ///
    /// # What happens to the anchors
    ///
    /// Each selection's anchor is shifted through *its own* edit with the same
    /// insertion-point semantics [`crate::selection_shift`] uses everywhere: an
    /// anchor swallowed by a deletion collapses onto the deletion start, which is
    /// exactly where the head lands — so `d` / `c` leave a caret at each edit
    /// site, with no bookkeeping. An anchor sitting exactly at an insertion point
    /// slides right with the text. A caller that needs a selection *preserved*
    /// across a same-length rewrite (helix's `~`, `>`) should re-set the
    /// selections afterwards via [`Editor::set_extra_selections`] rather than
    /// rely on that shift.
    ///
    /// # Degradation
    ///
    /// If any selection becomes untrackable mid-apply (see `selection_shift`), the
    /// secondaries are dropped and the editor collapses to the primary rather than
    /// carrying on with a selection that no longer knows where it is.
    ///
    /// [`Sel`]: crate::selection_shift::Sel
    pub fn edit_at_all_selections(
        &mut self,
        primary_anchor: hjkl_buffer::Position,
        make: impl Fn(crate::selection_shift::Sel) -> hjkl_buffer::Edit,
    ) -> (Vec<hjkl_buffer::Edit>, hjkl_buffer::Position) {
        use crate::selection_shift::Sel;

        let (pr, pc) = self.cursor();
        let primary = Sel::new(primary_anchor, hjkl_buffer::Position::new(pr, pc));

        let mut all: Vec<Sel> = std::iter::once(primary)
            .chain(self.extra_selections.iter().copied())
            .collect();
        // Bottom-up by where each selection's edit *starts* — its earlier end.
        // For a caret that is just the head, so this is the same order as before.
        all.sort_by_key(|s| std::cmp::Reverse((s.start().row, s.start().col)));

        // Rebuilt as we go: a selection lands in here the moment its edit is done,
        // which enrols it in the shift for every later edit.
        self.extra_selections.clear();

        let mut inverses = Vec::with_capacity(all.len());
        let mut primary_idx: Option<usize> = None;
        let mut lost_a_selection = false;

        for (i, s) in all.iter().copied().enumerate() {
            // Every previous iteration should have parked exactly one selection.
            // If the count slipped, `mutate_edit` dropped one it could not track.
            if self.extra_selections.len() != i {
                lost_a_selection = true;
                break;
            }
            let edit = make(s);
            // The anchor has to be shifted against the PRE-edit geometry, same as
            // the parked selections are — so read the metrics before applying.
            let rows = buf_row_count(&self.buffer);
            let lens: Vec<usize> = (0..rows).map(|r| buf_line_chars(&self.buffer, r)).collect();
            let shifted_anchor = crate::selection_shift::shift_position(
                s.anchor,
                &edit,
                |r| lens.get(r).copied().unwrap_or(0),
                rows,
            );

            self.set_cursor_quiet(s.head.row, s.head.col);
            inverses.push(self.mutate_edit(edit));
            let (nr, nc) = self.cursor();

            let Some(anchor) = shifted_anchor else {
                lost_a_selection = true;
                break;
            };
            if s == primary && primary_idx.is_none() {
                primary_idx = Some(self.extra_selections.len());
            }
            self.extra_selections
                .push(Sel::new(anchor, hjkl_buffer::Position::new(nr, nc)));
        }

        match (lost_a_selection, primary_idx) {
            (false, Some(idx)) if idx < self.extra_selections.len() => {
                // Pull the primary back out of the parked set; the rest stay.
                let landed = self.extra_selections.remove(idx);
                self.set_cursor_quiet(landed.head.row, landed.head.col);
                (inverses, landed.anchor)
            }
            _ => {
                // Something went untrackable: collapse to a single selection rather
                // than leave one pointing at text it no longer owns.
                self.extra_selections.clear();
                let (row, col) = self.cursor();
                (inverses, hjkl_buffer::Position::new(row, col))
            }
        }
    }

    /// The installed discipline's FSM state, type-erased.
    ///
    /// A discipline crate reaches its own concrete state by downcasting:
    /// `ed.discipline().as_any().downcast_ref::<VimState>()`.
    pub fn discipline(&self) -> &dyn crate::DisciplineState {
        &*self.discipline
    }

    /// Mutable counterpart of [`Editor::discipline`].
    pub fn discipline_mut(&mut self) -> &mut dyn crate::DisciplineState {
        &mut *self.discipline
    }

    /// Install a keyboard discipline, replacing whatever was there.
    ///
    /// Host apps call this once at construction (e.g.
    /// `hjkl_vim::install_vim_discipline(&mut ed)`); an `Editor` that never
    /// receives discipline input keeps the default
    /// [`NoDiscipline`](crate::NoDiscipline).
    pub fn set_discipline(&mut self, discipline: Box<dyn crate::DisciplineState>) {
        self.discipline = discipline;
    }

    /// The active read-only view overlay (see [`crate::ViewMode`]). Independent
    /// of [`Editor::vim_mode`]; the host renderer reads this as the source of
    /// truth for whether to draw the git-blame framing.
    pub fn view_mode(&self) -> crate::ViewMode {
        self.view
    }

    /// `true` when the git-blame read-only overlay is active. Masked on the
    /// input mode: BLAME is only meaningful in Normal, so this returns `false`
    /// the instant the editor enters Insert/Visual/etc., even before the
    /// overlay flag is dropped. Use this for both rendering and mode-label.
    pub fn is_blame(&self) -> bool {
        self.view == crate::ViewMode::Blame && self.coarse_mode() == crate::CoarseMode::Normal
    }

    /// Enter the git-blame read-only overlay. No-op unless the editor is in
    /// Normal mode (BLAME is a Normal-only view). While active, every mutation
    /// funnel is blocked and the host renders the per-commit framing.
    pub fn enter_blame(&mut self) {
        if self.coarse_mode() == crate::CoarseMode::Normal {
            self.view = crate::ViewMode::Blame;
        }
    }

    /// Leave the git-blame overlay, returning to a plain Normal view. Idempotent.
    pub fn exit_blame(&mut self) {
        self.view = crate::ViewMode::Normal;
    }

    /// Bounds of the active visual-block rectangle as
    /// `(top_row, bot_row, left_col, right_col)` — all inclusive.
    /// `None` when we're not in VisualBlock mode.
    /// Read-only view of the live `/` or `?` prompt. `None` outside
    /// search-prompt mode.
    pub fn search_prompt(&self) -> Option<&crate::search::SearchPrompt> {
        self.search_prompt.as_ref()
    }

    /// Most recent committed search pattern (persists across `n` / `N`
    /// and across prompt exits). `None` before the first search.
    pub fn last_search(&self) -> Option<&str> {
        self.last_search.as_deref()
    }

    /// Whether the last committed search was a forward `/` (`true`) or
    /// a backward `?` (`false`). `n` and `N` consult this to honour the
    /// direction the user committed.
    pub fn last_search_forward(&self) -> bool {
        self.last_search_forward
    }

    /// Set the most recent committed search text + direction. Used by
    /// host-driven prompts (e.g. apps/hjkl's `/` `?` prompt that lives
    /// outside the engine's vim FSM) so `n` / `N` repeat the host's
    /// most recent commit with the right direction. Pass `None` /
    /// `true` to clear.
    pub fn set_last_search(&mut self, text: Option<String>, forward: bool) {
        self.last_search = text;
        self.last_search_forward = forward;
    }

    /// The most recent successful `:s` command. `None` before the first substitute.
    /// Used by `:&` / `:&&` to repeat it. Returns an owned clone (the value
    /// lives behind a shared `Mutex`, so a borrow can't outlive the guard —
    /// mirrors [`Editor::global_marks_iter`]).
    pub fn last_substitute(&self) -> Option<crate::substitute::SubstituteCmd> {
        self.last_substitute.lock().unwrap().clone()
    }

    /// Store the last successful substitute so `:&` / `:&&` can repeat it.
    pub fn set_last_substitute(&mut self, cmd: crate::substitute::SubstituteCmd) {
        *self.last_substitute.lock().unwrap() = Some(cmd);
    }

    /// Point this editor at a shared last-substitute bank. All editors in
    /// the app share one bank (mirrors [`Editor::set_global_marks_arc`]) so
    /// `:&` run in one window repeats the `:s` most recently executed in any
    /// window — vim's last substitute is session-global, not per-window.
    pub fn set_last_substitute_arc(
        &mut self,
        last_substitute: std::sync::Arc<std::sync::Mutex<Option<crate::substitute::SubstituteCmd>>>,
    ) {
        self.last_substitute = last_substitute;
    }

    /// Number of rows (lines) in the buffer.
    ///
    /// Convenience accessor for call sites that only need the row count without
    /// routing through the `Query` trait directly (e.g. the VSCode selection
    /// dispatcher computing buffer-end positions).
    pub fn row_count(&self) -> usize {
        buf_row_count(&self.buffer)
    }

    /// Row `row` as an owned `String` (no trailing newline), or `None` when
    /// `row` is out of bounds.
    ///
    /// Mode-agnostic buffer read. Hosts and discipline crates (e.g. the vim
    /// accessors on `hjkl_vim::VimEditorExt`) use this instead of reaching for
    /// the engine's private `buf_line` helper.
    pub fn line(&self, row: usize) -> Option<String> {
        buf_line(&self.buffer, row)
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
        if let Some(arc) = self.buffer.cached_editor_content() {
            return arc;
        }
        let arc = std::sync::Arc::new(self.content());
        self.buffer
            .set_cached_editor_content(std::sync::Arc::clone(&arc));
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
        self.buffer.clear_undo_redo();
        // Whole-buffer replace supersedes any queued ContentEdits.
        self.buffer.clear_pending_content_edits();
        self.buffer.set_pending_content_reset(true);
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
        self.buffer.clear_pending_content_edits();
        self.buffer.set_pending_content_reset(true);
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
        self.buffer.take_change_log()
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
            modifiable: self.settings.modifiable,
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
        self.settings.modifiable = opts.modifiable;
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
        self.settings.foldmethod = opts.foldmethod;
        self.settings.foldenable = opts.foldenable;
        self.settings.foldlevelstart = opts.foldlevelstart;
        self.settings.colorcolumn = opts.colorcolumn.clone();
        self.settings.scrolloff = opts.scrolloff;
        self.settings.sidescrolloff = opts.sidescrolloff;
        self.settings.autoreload = opts.autoreload;
        self.settings.list = opts.list;
        self.settings.listchars = opts.listchars.clone();
        self.settings.colorizer = opts.colorizer;
        self.settings.colorizer_filetypes = opts.colorizer_filetypes.clone();
        self.settings.format_on_save = opts.format_on_save;
        self.settings.trim_trailing_whitespace = opts.trim_trailing_whitespace;
        self.settings.rainbow_brackets = opts.rainbow_brackets;
        self.settings.matchparen = opts.matchparen;
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
        // Coarse, not vim: render output must not depend on which discipline
        // is installed (#265). CoarseMode is a bijection with SnapshotMode.
        let (mode, shape) = match self.coarse_mode() {
            crate::CoarseMode::Normal => (SnapshotMode::Normal, CursorShape::Block),
            crate::CoarseMode::Insert => (SnapshotMode::Insert, CursorShape::Bar),
            crate::CoarseMode::Select => (SnapshotMode::Visual, CursorShape::Block),
            crate::CoarseMode::SelectLine => (SnapshotMode::VisualLine, CursorShape::Block),
            crate::CoarseMode::SelectBlock => (SnapshotMode::VisualBlock, CursorShape::Block),
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
    /// `Editor<B: View, H: Host>` constructor — this method's surface
    /// stays stable; only the snapshot's internal fields grow.
    ///
    /// Distinct from the internal `snapshot` used by undo (which
    /// returns `(Vec<String>, (usize, usize))`); host-facing
    /// persistence goes through this one.
    pub fn take_snapshot(&self) -> crate::types::EditorSnapshot {
        use crate::types::{EditorSnapshot, SnapshotMode};
        let mode = match self.coarse_mode() {
            crate::CoarseMode::Normal => SnapshotMode::Normal,
            crate::CoarseMode::Insert => SnapshotMode::Insert,
            crate::CoarseMode::Select => SnapshotMode::Visual,
            crate::CoarseMode::SelectLine => SnapshotMode::VisualLine,
            crate::CoarseMode::SelectBlock => SnapshotMode::VisualBlock,
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
            .buffer
            .marks_cloned()
            .into_iter()
            .map(|(c, (r, col))| (c, (r as u32, col as u32)))
            .collect();
        let global_marks = self
            .global_marks
            .lock()
            .unwrap()
            .iter()
            .map(|(c, &(bid, r, col))| (*c, (bid, r as u32, col as u32)))
            .collect();
        EditorSnapshot {
            version: EditorSnapshot::VERSION,
            mode,
            cursor,
            lines,
            viewport_top,
            registers: self.registers.lock().unwrap().clone(),
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
        *self.registers.lock().unwrap() = snap.registers;
        self.buffer.set_marks(
            snap.marks
                .into_iter()
                .map(|(c, (r, col))| (c, (r as usize, col as usize)))
                .collect(),
        );
        *self.global_marks.lock().unwrap() = snap
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
        self.yank_linewise = linewise;
        self.registers.lock().unwrap().unnamed = crate::registers::Slot { text, linewise };
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
    /// `View::ensure_cursor_visible` call at end-of-step so motions
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
        // Screen rows ≠ doc rows only under soft-wrap (a doc row spans many
        // screen lines) or folds (a closed fold collapses many doc rows to
        // one); doc-row margin math drifts in those cases. Dispatch:
        //   • wrap            → the incremental screen-row walk.
        //   • folds, no wrap  → the O(height) fold-aware clamp below.
        //   • neither         → the fast O(1) doc-row math (every plain j/k/G).
        let wrapped = !matches!(self.host.viewport().wrap, hjkl_buffer::Wrap::None);
        if wrapped {
            self.ensure_scrolloff_vertical(height, margin);
            return;
        }
        if !self.buffer.folds().is_empty() {
            self.ensure_scrolloff_folds_nowrap(height, margin);
            // Column-side (horizontal) scroll only — keep the fold-aware
            // top_row by snapshotting it across `ensure_visible`.
            let cursor = buf_cursor_pos(&self.buffer);
            let saved_top = self.host.viewport().top_row;
            self.host.viewport_mut().ensure_visible(cursor);
            self.host.viewport_mut().top_row = saved_top;
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
        // Column-side scroll (vim default `sidescrolloff = 0`).
        let cursor = buf_cursor_pos(&self.buffer);
        self.host.viewport_mut().ensure_visible(cursor);
    }

    /// Fold-aware vertical scrolloff for `Wrap::None`, in **O(height)**.
    ///
    /// A closed fold collapses its body to one screen row, so the cursor's
    /// screen row is the count of *visible* rows above it — not the doc-row
    /// delta. Instead of re-walking that count on every candidate `top_row`
    /// (the incremental [`Self::ensure_scrolloff_vertical`], O(n²) on a big
    /// jump like `G` over a fold-heavy file), compute the valid `top_row`
    /// window directly: at most `height-1-margin` visible rows may sit above
    /// the cursor (bottom edge) and at least `margin` (top edge). Walk those
    /// two bounds up from the cursor via `prev_visible_row`, clamp the current
    /// `top_row` into the window, then clamp to `max_top_for_height` so the
    /// buffer's bottom never leaves blank rows. Each walk is bounded by
    /// `height`, so the whole thing is O(height) regardless of jump distance.
    fn ensure_scrolloff_folds_nowrap(&mut self, height: usize, margin: usize) {
        let cursor_row = buf_cursor_row(&self.buffer);
        let max_csr = height.saturating_sub(1).saturating_sub(margin);
        // `top_lo`: the row `max_csr` visible rows above the cursor — `top_row`
        // must be >= this to keep the cursor within the bottom margin.
        let mut top_lo = cursor_row;
        for _ in 0..max_csr {
            match self.buffer.prev_visible_row(top_lo) {
                Some(p) => top_lo = p,
                None => break,
            }
        }
        // `top_hi`: the row `margin` visible rows above the cursor — `top_row`
        // must be <= this to keep the cursor below the top margin.
        let mut top_hi = cursor_row;
        for _ in 0..margin {
            match self.buffer.prev_visible_row(top_hi) {
                Some(p) => top_hi = p,
                None => break,
            }
        }
        // `max_csr >= margin` (margin is capped at (height-1)/2), so
        // `top_lo <= top_hi` and the clamp range is well-formed.
        let cur = self.host.viewport().top_row;
        let mut new_top = cur.clamp(top_lo, top_hi);
        let max_top = {
            let folds = crate::buffer_impl::BufferFoldProvider::new(&self.buffer);
            crate::viewport_math::max_top_for_height(
                &self.buffer,
                &folds,
                self.host.viewport(),
                height,
            )
        };
        if new_top > max_top {
            new_top = max_top;
        }
        self.host.viewport_mut().top_row = new_top;
    }

    /// Screen-row-aware vertical scrolloff. Walks `top_row` one visible
    /// doc row at a time so the cursor's *screen* row stays inside
    /// `[margin, height - 1 - margin]`, then clamps `top_row` so the
    /// buffer's bottom never leaves blank rows below it.
    ///
    /// Correct under BOTH soft-wrap (a doc row spans many screen lines)
    /// and folds (a closed fold collapses many doc rows to one screen
    /// row): [`crate::viewport_math::cursor_screen_row_from`] counts
    /// visible/wrapped screen rows, so doc-row arithmetic can't drift the
    /// margin around a fold. Horizontal (column) scroll is the caller's
    /// job — this only moves `top_row`.
    fn ensure_scrolloff_vertical(&mut self, height: usize, margin: usize) {
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
            let top = self.host.viewport().top_row;
            let csr = crate::viewport_math::cursor_screen_row_from(
                &self.buffer,
                &folds,
                self.host.viewport(),
                top,
            )
            .unwrap_or(0);
            if csr <= max_csr {
                break;
            }
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
            let top = self.host.viewport().top_row;
            let csr = crate::viewport_math::cursor_screen_row_from(
                &self.buffer,
                &folds,
                self.host.viewport(),
                top,
            )
            .unwrap_or(0);
            if csr >= margin {
                break;
            }
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
        // If the target row is hidden inside one or more closed folds, open
        // every fold that collapses it so the landing line is actually
        // visible — a jump to an unseen row is useless. `reveal_row` opens
        // all hiding folds (outer + nested) in one pass; `open_fold_at` /
        // `FoldOp::OpenAt` can't, because they only act on the first fold
        // containing the row and so can never reach a nested inner fold.
        self.buffer.reveal_row(target);
        buf_set_cursor_rc(&mut self.buffer, target, 0);
        // Vim: `:N` / `+N` jump scrolls the viewport too — without this
        // the cursor lands off-screen and the user has to scroll
        // manually to see it.
        self.ensure_cursor_in_scrolloff();
    }

    /// Scroll so the cursor row lands at the given viewport position:
    /// `Center` → middle row, `Top` → first row, `Bottom` → last row.
    /// Cursor stays on its absolute line; only the viewport moves.
    pub fn scroll_cursor_to(&mut self, pos: CursorScrollTarget) {
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
    /// [`Query::content_joined`], which the `View` impl caches as an
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

    // ── Undo / redo (discipline-agnostic, #265) ──────────────────────────────
    //
    // The rope-level work is generic — every discipline undoes. The only
    // discipline-specific part is what state the editor is left in afterwards,
    // which goes through `DisciplineState::reset_to_idle` plus a coarse cursor
    // clamp, so the engine never names vim.

    /// Rope-level undo, then return the discipline to idle.
    fn undo_core(&mut self) {
        if let Some(entry) = self.buffer.pop_undo_entry() {
            let (cur_rope, cur_cursor) = self.snapshot();
            self.buffer.push_redo_entry(hjkl_buffer::UndoEntry {
                rope: cur_rope,
                cursor: cur_cursor,
                timestamp: entry.timestamp,
            });
            self.restore_rope(entry.rope, entry.cursor);
        }
        self.settle_after_history_jump();
    }

    /// Rope-level redo, then return the discipline to idle.
    fn redo_core(&mut self) {
        if let Some(entry) = self.buffer.pop_redo_entry() {
            let (cur_rope, cur_cursor) = self.snapshot();
            let before = cur_rope.clone();
            self.buffer.push_undo_entry(hjkl_buffer::UndoEntry {
                rope: cur_rope,
                cursor: cur_cursor,
                timestamp: entry.timestamp,
            });
            self.cap_undo();
            self.restore_rope(entry.rope, entry.cursor);
            // Park the cursor at the START of the reapplied change rather than
            // the end-of-insert position stored in the redo snapshot (vim
            // parity). Recompute from the first differing character.
            let after = crate::types::Query::rope(&self.buffer);
            if let Some((row, col)) = first_diff_pos(&before, &after) {
                buf_set_cursor_rc(&mut self.buffer, row, col);
                self.push_buffer_cursor_to_textarea();
            }
        }
        self.settle_after_history_jump();
    }

    /// Leave the editor in a known resting state after jumping through history
    /// (undo / redo) or after a `:!` filter rewrote the buffer.
    ///
    /// Asks the installed discipline to put its *mode* back to idle — without
    /// discarding an open insert session, which vscode-mode undo depends on —
    /// then clamps the cursor to a valid column.
    pub(crate) fn settle_after_history_jump(&mut self) {
        self.discipline.reset_mode_after_history();
        // Undo / redo restore a whole snapshot: the secondary selections were
        // computed against a document that no longer exists, and nothing tracked
        // them across the rewind. Drop them rather than leave carets pointing at
        // text that moved — the same "drop, never guess" rule `selection_shift`
        // applies to a single untrackable edit.
        self.extra_selections.clear();
        // Unconditional clamp: the restored cursor came from a snapshot that may
        // have been taken mid-insert and can sit one past the last valid column.
        let (row, col) = self.cursor();
        let max_col = buf_line_chars(&self.buffer, row).saturating_sub(1);
        if col > max_col {
            buf_set_cursor_rc(&mut self.buffer, row, max_col);
            self.push_buffer_cursor_to_textarea();
        }
    }

    /// Walk one step back through the undo history. Equivalent to the
    /// user pressing `u` in normal mode. Drains the most recent undo
    /// entry and pushes it onto the redo stack.
    pub fn undo(&mut self) {
        self.undo_core();
    }

    /// Walk one step forward through the redo history. Equivalent to
    /// `<C-r>` in normal mode.
    pub fn redo(&mut self) {
        self.redo_core();
    }

    /// Undo `n` steps. Returns the number of steps actually applied
    /// (bounded by undo stack size).
    pub fn earlier_by_steps(&mut self, n: usize) -> usize {
        let mut count = 0;
        for _ in 0..n {
            if self.buffer.undo_stack_is_empty() {
                break;
            }
            self.undo_core();
            count += 1;
        }
        count
    }

    /// Redo `n` steps. Returns the number of steps actually applied
    /// (bounded by redo stack size).
    pub fn later_by_steps(&mut self, n: usize) -> usize {
        let mut count = 0;
        for _ in 0..n {
            if self.buffer.redo_stack_is_empty() {
                break;
            }
            self.redo_core();
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
            match self.buffer.peek_undo_timestamp() {
                None => break,
                Some(ts) => {
                    if ts <= target {
                        break;
                    }
                }
            }
            self.undo_core();
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
            match self.buffer.peek_redo_timestamp() {
                None => break,
                Some(ts) => {
                    if ts > target {
                        break;
                    }
                }
            }
            self.redo_core();
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
        self.buffer.push_undo_entry(hjkl_buffer::UndoEntry {
            rope,
            cursor,
            timestamp,
        });
        self.cap_undo();
        self.buffer.clear_redo();
    }

    /// Trim the undo stack down to `settings.undo_levels`, dropping
    /// the oldest entries. `undo_levels == 0` is treated as
    /// "unlimited" (vim's 0-means-no-undo semantics intentionally
    /// skipped — guarding with `> 0` is one line shorter than gating
    /// the cap path with an explicit zero-check above the call site).
    pub(crate) fn cap_undo(&mut self) {
        let cap = self.settings.undo_levels as usize;
        self.buffer.cap_undo(cap);
    }

    /// Test-only accessor for the undo stack length.
    #[doc(hidden)]
    pub fn undo_stack_len(&self) -> usize {
        self.buffer.undo_stack_len()
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
    /// `View::rope()`), so this avoids the full-document `to_string`
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
        self.buffer.clear_pending_content_edits();
        self.buffer.push_pending_content_edit(edit);
        self.mark_content_dirty();
    }

    // ─── Range-query helpers for partial-format dispatch (#119) ─────────────

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
        let input_text = crate::rope_util::rope_row_range_str(&rope, top, bot);
        // Materialized for the splice-back after the command succeeds.
        let lines = crate::rope_util::rope_to_lines_vec(&rope);

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
        // Leave the editor idle after a successful filter (vim parity: Normal).
        // Goes through the discipline hook, so the engine does not name vim.
        self.discipline.reset_to_idle();

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

    // ─── Phase 6.1: public insert-mode primitives (kryptic-sh/hjkl#87) ────────
    //
    // Each method is the publicly callable form of one insert-mode action.
    // All logic lives in the corresponding `vim::*_bridge` free function;
    // these methods are thin delegators so the public surface stays on `Editor`.
    //
    // Invariants (enforced by the bridge fns):
    //   - View mutations go through `mutate_edit` (dirty/undo/change-list).
    //   - Navigation keys call `break_undo_group_in_insert` when the FSM did.
    //   - `push_buffer_cursor_to_textarea` is called after every mutation
    //     (currently a no-op, kept for migration hygiene).
}

// ── Phase 6.6b: FSM state accessors (for hjkl-vim ownership) ─────────────────
//
// The FSM (now in hjkl-vim) reads/writes `VimState` fields through public
// `Editor` accessors and mutators defined in this block. Each method gets a
// one-line `///` rustdoc. Fields mutated as a unit get a combined action method
// rather than individual getters + setters (e.g. `accumulate_count_digit`).

impl<H: crate::types::Host> Editor<hjkl_buffer::View, H> {
    // ── Pending chord ─────────────────────────────────────────────────────────

    // ── Abbreviations ─────────────────────────────────────────────────────────

    /// Register an abbreviation. If an entry for `lhs` already exists (same
    /// mode flags), it is replaced. Inserts at the front so newer definitions
    /// take priority (first-match wins in `try_abbrev_expand`).
    pub fn add_abbrev(&mut self, lhs: &str, rhs: &str, insert: bool, cmdline: bool, noremap: bool) {
        // Remove existing entry with same lhs + overlapping mode flags.
        self.abbrevs
            .retain(|a| a.lhs != lhs || (a.insert && !insert) || (a.cmdline && !cmdline));
        self.abbrevs.insert(
            0,
            crate::abbrev::Abbrev {
                lhs: lhs.to_string(),
                rhs: rhs.to_string(),
                insert,
                cmdline,
                noremap,
            },
        );
    }

    /// Remove the abbreviation with the given `lhs`. Only removes entries
    /// whose mode flags overlap with the requested `insert`/`cmdline` flags.
    pub fn remove_abbrev(&mut self, lhs: &str, insert: bool, cmdline: bool) {
        self.abbrevs
            .retain(|a| a.lhs != lhs || (!insert || !a.insert) && (!cmdline || !a.cmdline));
    }

    /// Clear all abbreviations matching the given mode flags.
    ///
    /// `insert=true` removes insert-mode abbrevs; `cmdline=true` removes
    /// cmdline-mode abbrevs. Both `true` clears everything.
    pub fn clear_abbrevs(&mut self, insert: bool, cmdline: bool) {
        self.abbrevs.retain(|a| {
            // Keep entries that do NOT match any of the cleared modes.
            let cleared = (insert && a.insert) || (cmdline && a.cmdline);
            !cleared
        });
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
        self.jump_back.push(from);
        if self.jump_back.len() > crate::types::JUMPLIST_MAX {
            self.jump_back.remove(0);
        }
        self.jump_fwd.clear();
    }

    /// Push `pattern` onto the committed search history. Skips if the
    /// most recent entry already matches (consecutive dedupe) and trims
    /// the oldest entries beyond the history cap.
    pub fn record_search_history(&mut self, pattern: &str) {
        if pattern.is_empty() {
            return;
        }
        if self.search_history.last().map(String::as_str) == Some(pattern) {
            return;
        }
        self.search_history.push(pattern.to_string());
        let len = self.search_history.len();
        if len > crate::types::SEARCH_HISTORY_MAX {
            self.search_history
                .drain(0..len - crate::types::SEARCH_HISTORY_MAX);
        }
    }

    /// Walk the search-prompt history by `dir` steps. `dir = -1` moves
    /// toward older entries (Ctrl-P / Up); `dir = 1` toward newer ones
    /// (Ctrl-N / Down). Stops at the ends; does nothing if there is no
    /// active search prompt.
    pub fn walk_search_history(&mut self, dir: isize) {
        if self.search_history.is_empty() || self.search_prompt.is_none() {
            return;
        }
        let len = self.search_history.len();
        let next_idx = match (self.search_history_cursor, dir) {
            (None, -1) => Some(len - 1),
            (None, 1) => return,
            (Some(i), -1) => i.checked_sub(1),
            (Some(i), 1) if i + 1 < len => Some(i + 1),
            _ => None,
        };
        let Some(idx) = next_idx else {
            return;
        };
        self.search_history_cursor = Some(idx);
        let text = self.search_history[idx].clone();
        if let Some(prompt) = self.search_prompt.as_mut() {
            prompt.cursor = text.chars().count();
            prompt.text = text.clone();
        }
        self.push_search_pattern(&text);
    }

    // The per-step prelude/epilogue (`begin_step`/`end_step` + `StepBookkeeping`)
    // moved to `hjkl_vim::step` (#267); the engine no longer owns FSM bookkeeping.

    /// Return the character count (code-point count) of line `row`, or `0`
    /// when `row` is out of range.
    ///
    /// A raw buffer read with no vim semantics, so it stays on the engine core
    /// while the vim-specific visual/block primitives move to
    /// `hjkl_vim::VimEditorExt` (#267).
    pub fn line_char_count(&self, row: usize) -> usize {
        buf_line_chars(&self.buffer, row)
    }
}

/// First `(row, col)` where two ropes differ, or `None` if identical. Used to
/// place the cursor at the start of a redone change (vim parity).
fn first_diff_pos(a: &ropey::Rope, b: &ropey::Rope) -> Option<(usize, usize)> {
    let rows = a.len_lines().max(b.len_lines());
    for r in 0..rows {
        let la = if r < a.len_lines() {
            hjkl_buffer::rope_line_str(a, r)
        } else {
            String::new()
        };
        let lb = if r < b.len_lines() {
            hjkl_buffer::rope_line_str(b, r)
        } else {
            String::new()
        };
        if la != lb {
            let col = la
                .chars()
                .zip(lb.chars())
                .take_while(|(x, y)| x == y)
                .count();
            return Some((r, col));
        }
    }
    None
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
    use hjkl_buffer::View;

    fn ed_with_spans(line_count: usize) -> Editor<View, DefaultHost> {
        let text = (0..line_count)
            .map(|i| format!("row{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let buf = View::from_str(&text);
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
    fn ed_with_distinguishable_spans(line_count: usize) -> Editor<View, DefaultHost> {
        let text = (0..line_count)
            .map(|i| format!("rowwwwwwwwww{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let buf = View::from_str(&text);
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
    /// `View::content_joined`'s per-dirty_gen cache — a warm snapshot
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
        let buf = hjkl_buffer::View::from_str(&text);
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
    use hjkl_buffer::View;
    use std::time::{Duration, SystemTime};

    fn make_ed(content: &str) -> Editor<View, DefaultHost> {
        let buf = View::from_str(content);
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

// ─── modifiable / readonly semantics tests ────────────────────────────────────

#[cfg(test)]
mod shared_registers_tests {
    use super::*;
    use crate::types::{DefaultHost, Options};
    use hjkl_buffer::View;

    #[test]
    fn shared_register_bank_visible_across_editors() {
        let shared =
            std::sync::Arc::new(std::sync::Mutex::new(crate::registers::Registers::default()));
        let mut a = Editor::new(View::new(), DefaultHost::default(), Options::default());
        a.set_registers_arc(shared.clone());
        let mut b = Editor::new(View::new(), DefaultHost::default(), Options::default());
        b.set_registers_arc(shared.clone());
        // Write to editor A's unnamed register
        a.registers_mut().unnamed = crate::registers::Slot {
            text: "hello".to_string(),
            linewise: false,
        };
        // Read from editor B — same bank, no copy needed
        assert_eq!(b.registers().unnamed.text, "hello");
    }
}

// ─── shared global-marks bank tests (#279 slice 1) ────────────────────────────

#[cfg(test)]
mod shared_global_marks_tests {
    use super::*;
    use crate::types::{DefaultHost, Options};
    use hjkl_buffer::View;

    #[test]
    fn shared_global_marks_bank_visible_across_editors() {
        let shared = std::sync::Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new()));
        let mut a = Editor::new(View::new(), DefaultHost::default(), Options::default());
        a.set_global_marks_arc(shared.clone());
        let mut b = Editor::new(View::new(), DefaultHost::default(), Options::default());
        b.set_global_marks_arc(shared.clone());
        // Set a global mark on editor A.
        a.set_global_mark('A', 7, (3, 5));
        // Read from editor B — same bank, no copy needed.
        assert_eq!(b.global_mark('A'), Some((7, 3, 5)));
    }

    #[test]
    fn unshared_global_marks_stay_isolated() {
        let mut a = Editor::new(View::new(), DefaultHost::default(), Options::default());
        let b = Editor::new(View::new(), DefaultHost::default(), Options::default());
        a.set_global_mark('A', 1, (0, 0));
        // No shared Arc wired — B must not see A's mark.
        assert_eq!(b.global_mark('A'), None);
    }
}

// ─── shared last-substitute bank tests (#279 slice 2) ─────────────────────

#[cfg(test)]
mod shared_last_substitute_tests {
    use super::*;
    use crate::types::{DefaultHost, Options};
    use hjkl_buffer::View;

    fn dummy_cmd(replacement: &str) -> crate::substitute::SubstituteCmd {
        crate::substitute::SubstituteCmd {
            pattern: Some("foo".to_string()),
            replacement: replacement.to_string(),
            flags: crate::substitute::SubstFlags::default(),
            count: None,
        }
    }

    #[test]
    fn shared_last_substitute_bank_visible_across_editors() {
        let shared = std::sync::Arc::new(std::sync::Mutex::new(None));
        let mut a = Editor::new(View::new(), DefaultHost::default(), Options::default());
        a.set_last_substitute_arc(shared.clone());
        let mut b = Editor::new(View::new(), DefaultHost::default(), Options::default());
        b.set_last_substitute_arc(shared.clone());
        // Run `:s` (set the last substitute) on editor A.
        a.set_last_substitute(dummy_cmd("bar"));
        // Read from editor B — same bank, no copy needed.
        assert_eq!(b.last_substitute(), Some(dummy_cmd("bar")));
    }

    #[test]
    fn unshared_last_substitute_stays_isolated() {
        let mut a = Editor::new(View::new(), DefaultHost::default(), Options::default());
        let b = Editor::new(View::new(), DefaultHost::default(), Options::default());
        a.set_last_substitute(dummy_cmd("bar"));
        // No shared Arc wired — B must not see A's last substitute.
        assert_eq!(b.last_substitute(), None);
    }
}

#[cfg(test)]
mod scroll_anim_tests {
    use super::*;
    use crate::types::{DefaultHost, Host, Options};
    use hjkl_buffer::View;

    fn make_editor_with_content(content: &str) -> Editor<View, DefaultHost> {
        let mut buf = View::new();
        crate::types::BufferEdit::replace_all(&mut buf, content);
        let host = DefaultHost::new();
        Editor::new(buf, host, Options::default())
    }

    #[test]
    fn scroll_duration_default_is_zero() {
        let buf = View::new();
        let host = DefaultHost::new();
        let ed = Editor::new(buf, host, Options::default());
        assert_eq!(ed.settings().scroll_duration_ms, 0);
    }

    #[test]
    fn take_scroll_anim_hint_false_initially() {
        let buf = View::new();
        let host = DefaultHost::new();
        let mut ed = Editor::new(buf, host, Options::default());
        assert!(!ed.take_scroll_anim_hint());
    }

    #[test]
    fn take_scroll_anim_hint_one_shot() {
        // Half-page scroll sets the hint; second drain clears it.
        let content: String = (0..50).map(|i| format!("line {i}\n")).collect();
        let mut ed = make_editor_with_content(&content);
        // Set viewport height so scroll actually moves
        ed.host_mut().viewport_mut().height = 20;
        ed.host_mut().viewport_mut().width = 80;
        ed.host_mut().viewport_mut().text_width = 80;
        ed.scroll_half_page(crate::types::ScrollDir::Down, 1);
        assert!(
            ed.take_scroll_anim_hint(),
            "hint should be set after half-page"
        );
        assert!(
            !ed.take_scroll_anim_hint(),
            "hint should be cleared on second drain"
        );
    }

    #[test]
    fn line_scroll_does_not_set_hint() {
        let content: String = (0..50).map(|i| format!("line {i}\n")).collect();
        let mut ed = make_editor_with_content(&content);
        ed.host_mut().viewport_mut().height = 20;
        ed.host_mut().viewport_mut().width = 80;
        ed.host_mut().viewport_mut().text_width = 80;
        ed.scroll_line(crate::types::ScrollDir::Down, 1);
        assert!(
            !ed.take_scroll_anim_hint(),
            "hint must NOT be set for C-e/C-y"
        );
    }
}

// ── UndoGranularity unit tests ───────────────────────────────────────────────
//
// These tests prove the critical invariant: vim (InsertSession) is byte-
// identical before and after this feature; Word granularity splits undo at
// word boundaries.
