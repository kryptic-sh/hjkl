//! Vim FSM: dot repeat.
//!
//! Split out of the monolithic `vim.rs` (#267 follow-up).

use hjkl_vim_types::{InsertEntry, LastChange, Mode};

use super::*;
use crate::vim_state::{vim, vim_mut};
use hjkl_engine::Editor;
use hjkl_engine::buf_helpers::{buf_cursor_pos, buf_line_chars, buf_set_cursor_rc};

/// Replay-side helper: insert `text` at the cursor through the
/// edit funnel, then leave insert mode (the original change ended
/// with Esc, so the dot-repeat must end the same way — including
/// the cursor step-back vim does on Esc-from-insert).
pub(crate) fn replay_insert_and_finish<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
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
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
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
        } => {
            let total = scaled(count.max(1));
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
            for _ in 0..count.max(1) {
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
