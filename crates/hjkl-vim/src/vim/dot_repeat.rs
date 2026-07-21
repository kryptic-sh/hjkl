//! Vim FSM: dot repeat.
//!
//! Split out of the monolithic `vim.rs` (#267 follow-up).

use hjkl_vim_types::{InsertEntry, LastChange, Mode, VisualExtent};

use super::*;
use crate::vim_state::{vim, vim_mut};
use hjkl_engine::Editor;
use hjkl_engine::buf_helpers::{buf_cursor_pos, buf_line_chars, buf_set_cursor_rc};

/// Replay-side helper: insert `text` at the cursor through the
/// edit funnel, then leave insert mode (the original change ended
/// with Esc, so the dot-repeat must end the same way — including
/// the cursor step-back vim does on Esc-from-insert).
pub(crate) fn replay_insert_and_finish<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    text: &str,
) {
    use hjkl_buffer::{Edit, Position};
    let cursor = ed.cursor();
    ed.mutate_edit(Edit::InsertStr {
        at: Position::new(cursor.0, cursor.1),
        text: text.to_string(),
    });
    if vim_mut(ed).insert_session.take().is_some() {
        if ed.cursor().1 > 0 {
            hjkl_engine::motions::move_left(ed.buffer_mut(), 1);
            ed.push_buffer_cursor_to_textarea();
        }
        vim_mut(ed).mode = Mode::Normal;
    }
}
pub(crate) fn replay_last_change<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    outer_count: usize,
) {
    let Some(change) = vim(ed).last_change.clone() else {
        return;
    };
    vim_mut(ed).replaying = true;
    // Dot-repeat with an explicit `[count].` *replaces* the change's stored
    // count (`:h .`): `3x` then `2.` deletes 2, not 6. `outer_count == 0`
    // means the user typed no count, so the original stored count is reused.
    // Both counts are individually capped; re-clamp at vim's ceiling
    // (`:h count`) so replay loops stay bounded.
    let explicit = if outer_count > 0 {
        Some(outer_count.min(MAX_COUNT))
    } else {
        None
    };
    let scaled = |c: usize| explicit.unwrap_or(c).min(MAX_COUNT);
    match change {
        LastChange::OpMotion {
            op,
            motion,
            count,
            inserted,
        } => {
            let total = scaled(count.max(1));
            apply_op_with_motion(ed, op, &motion, total);
            if let Some(text) = inserted {
                replay_insert_and_finish(ed, &text);
            }
        }
        LastChange::OpTextObj {
            op,
            obj,
            inner,
            inserted,
        } => {
            // Dot-repeat replays the text object at count 1 (the original
            // count is not retained in `LastChange::OpTextObj`).
            apply_op_with_text_object(ed, op, obj, inner, 1);
            if let Some(text) = inserted {
                replay_insert_and_finish(ed, &text);
            }
        }
        LastChange::LineOp {
            op,
            count,
            inserted,
            register,
        } => {
            let total = scaled(count.max(1));
            // Restore the original change's explicit register (if any) so
            // `"add.` deletes into `"a` again, not the unnamed register
            // (`:h redo-register`) — `execute_line_op` consumes
            // `pending_register` internally, same as the live FSM path.
            vim_mut(ed).pending_register = register;
            execute_line_op(ed, op, total);
            if let Some(text) = inserted {
                replay_insert_and_finish(ed, &text);
            }
        }
        LastChange::CharDel { forward, count } => {
            do_char_delete(ed, forward, scaled(count));
        }
        LastChange::ReplaceChar { ch, count } => {
            replace_char(ed, ch, scaled(count));
        }
        LastChange::ToggleCase { count } => {
            for _ in 0..scaled(count) {
                ed.push_undo();
                if !toggle_case_at_cursor(ed) {
                    break;
                }
            }
        }
        LastChange::JoinLine { count } => {
            for _ in 0..scaled(count) {
                ed.push_undo();
                if !join_line(ed) {
                    break;
                }
            }
        }
        LastChange::Paste {
            before,
            count,
            cursor_after,
            reindent,
        } => {
            do_paste(ed, before, scaled(count), cursor_after, reindent);
        }
        LastChange::GnOp {
            op,
            forward,
            inserted,
        } => {
            gn_operate(ed, Some(op), forward, 1);
            if let Some(text) = inserted {
                replay_insert_and_finish(ed, &text);
            }
        }
        LastChange::VisualOp {
            op,
            extent,
            inserted,
        } => {
            // B1: replay a visual-mode operator over a same-SIZE region
            // anchored at the CURRENT cursor (`:h v_.`), not the original
            // absolute range. `Line` extents are exactly `[count]dd`-style
            // (execute_line_op already implements that count semantics for
            // every operator it supports). `Char` extents synthesize an
            // inclusive charwise range: single-line selections keep their
            // raw width from the cursor; multi-line selections run the
            // first line cursor-to-EOL, middle lines whole, and the last
            // line's first `width` chars — the same shape
            // `run_operator_over_range` already cuts for a live multi-line
            // charwise visual selection.
            match extent {
                VisualExtent::Line { lines } => {
                    execute_line_op(ed, op, lines);
                    if let Some(text) = inserted {
                        replay_insert_and_finish(ed, &text);
                    }
                }
                VisualExtent::Char { lines, width } => {
                    let (r, c) = ed.cursor();
                    let end = if lines <= 1 {
                        (r, c + width.saturating_sub(1))
                    } else {
                        (r + lines - 1, width.saturating_sub(1))
                    };
                    run_operator_over_range(
                        ed,
                        op,
                        (r, c),
                        end,
                        hjkl_vim_types::RangeKind::Inclusive,
                    );
                    if let Some(text) = inserted {
                        replay_insert_and_finish(ed, &text);
                    }
                }
                // B-block (`:h v_.` for blocks): reconstruct a same-SIZE
                // rectangle top-left at the cursor and re-run the op. Block
                // `c` re-inserts `inserted` itself (it never went through
                // insert mode here), so DON'T also `replay_insert_and_finish`.
                VisualExtent::Block { rows, cols, to_eol } => {
                    replay_block_visual_op(ed, op, rows, cols, to_eol, inserted);
                }
            }
        }
        LastChange::VisualReplace { ch, extent } => {
            replay_visual_replace(ed, ch, extent);
        }
        LastChange::VisualBlockReplace {
            ch,
            rows,
            cols,
            to_eol,
        } => {
            replay_block_replace(ed, ch, rows, cols, to_eol);
        }
        LastChange::VisualBlockInsert {
            text,
            rows,
            cols,
            to_eol,
            append,
        } => {
            replay_block_insert(ed, &text, rows, cols, to_eol, append);
        }
        LastChange::ReplaceMode { text } => {
            use hjkl_buffer::{Edit, MotionKind, Position};
            ed.push_undo();
            for ch in text.chars() {
                let cursor = buf_cursor_pos(ed.buffer());
                let line_chars = buf_line_chars(ed.buffer(), cursor.row);
                if cursor.col < line_chars {
                    // Overtype the char under the cursor.
                    ed.mutate_edit(Edit::DeleteRange {
                        start: cursor,
                        end: Position::new(cursor.row, cursor.col + 1),
                        kind: MotionKind::Char,
                    });
                }
                ed.mutate_edit(Edit::InsertChar { at: cursor, ch });
                buf_set_cursor_rc(ed.buffer_mut(), cursor.row, cursor.col + 1);
            }
            // Esc step-back onto the last overtyped char.
            if ed.cursor().1 > 0 {
                hjkl_engine::motions::move_left(ed.buffer_mut(), 1);
            }
            ed.push_buffer_cursor_to_textarea();
        }
        LastChange::DeleteToEol { inserted } => {
            use hjkl_buffer::{Edit, Position};
            ed.push_undo();
            delete_to_eol(ed);
            if let Some(text) = inserted {
                let cursor = ed.cursor();
                ed.mutate_edit(Edit::InsertStr {
                    at: Position::new(cursor.0, cursor.1),
                    text,
                });
            }
        }
        LastChange::OpenLine { above, inserted } => {
            use hjkl_buffer::{Edit, Position};
            ed.push_undo();
            ed.sync_buffer_content_from_textarea();
            let row = buf_cursor_pos(ed.buffer()).row;
            if above {
                ed.mutate_edit(Edit::InsertStr {
                    at: Position::new(row, 0),
                    text: "\n".to_string(),
                });
                let folds = hjkl_engine::SnapshotFoldProvider::from_buffer(ed.buffer());
                let mut sticky = ed.sticky_col();
                hjkl_engine::motions::move_up(ed.buffer_mut(), &folds, 1, &mut sticky);
                ed.set_sticky_col(sticky);
            } else {
                let line_chars = buf_line_chars(ed.buffer(), row);
                ed.mutate_edit(Edit::InsertStr {
                    at: Position::new(row, line_chars),
                    text: "\n".to_string(),
                });
            }
            ed.push_buffer_cursor_to_textarea();
            let cursor = ed.cursor();
            ed.mutate_edit(Edit::InsertStr {
                at: Position::new(cursor.0, cursor.1),
                text: inserted,
            });
        }
        LastChange::InsertAt {
            entry,
            inserted,
            count,
        } => {
            use hjkl_buffer::{Edit, Position};
            ed.push_undo();
            match entry {
                InsertEntry::I => {}
                InsertEntry::ShiftI => move_first_non_whitespace(ed),
                InsertEntry::A => {
                    hjkl_engine::motions::move_right_to_end(ed.buffer_mut(), 1);
                    ed.push_buffer_cursor_to_textarea();
                }
                InsertEntry::ShiftA => {
                    hjkl_engine::motions::move_line_end(ed.buffer_mut());
                    hjkl_engine::motions::move_right_to_end(ed.buffer_mut(), 1);
                    ed.push_buffer_cursor_to_textarea();
                }
            }
            for _ in 0..scaled(count).max(1) {
                let cursor = ed.cursor();
                ed.mutate_edit(Edit::InsertStr {
                    at: Position::new(cursor.0, cursor.1),
                    text: inserted.clone(),
                });
            }
        }
    }
    vim_mut(ed).replaying = false;
}
/// The substring of `after` that differs from `before` (first-diff to
/// last-diff). Unlike [`extract_inserted`] this works for equal-length or
/// shorter results, so it captures `R` overstrike text for dot-repeat.
pub(crate) fn changed_run(before: &str, after: &str) -> String {
    let a: Vec<char> = before.chars().collect();
    let b: Vec<char> = after.chars().collect();
    let prefix = a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count();
    let max_suffix = a.len().min(b.len()) - prefix;
    let suffix = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take(max_suffix)
        .take_while(|(x, y)| x == y)
        .count();
    b[prefix..b.len() - suffix].iter().collect()
}
pub(crate) fn extract_inserted(before: &str, after: &str) -> String {
    let before_chars: Vec<char> = before.chars().collect();
    let after_chars: Vec<char> = after.chars().collect();
    if after_chars.len() <= before_chars.len() {
        return String::new();
    }
    let prefix = before_chars
        .iter()
        .zip(after_chars.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let max_suffix = before_chars.len() - prefix;
    let suffix = before_chars
        .iter()
        .rev()
        .zip(after_chars.iter().rev())
        .take(max_suffix)
        .take_while(|(a, b)| a == b)
        .count();
    after_chars[prefix..after_chars.len() - suffix]
        .iter()
        .collect()
}
