//! Vim FSM: linewise.
//!
//! Split out of the monolithic `vim.rs` (#267 follow-up).

use hjkl_vim_types::{Mode, Operator, RangeKind};

use super::*;
use crate::vim_state::vim_mut;
use hjkl_engine::Editor;
use hjkl_engine::buf_helpers::{buf_line, buf_line_chars, buf_row_count, buf_set_cursor_rc};

/// Expand a linewise `[start, end]` row range so it fully covers every CLOSED
/// fold it overlaps — vim's rule that a linewise operator on a closed fold acts
/// on the whole fold. Loops until stable so nested closed folds are absorbed.
pub(crate) fn expand_linewise_over_closed_folds(
    buf: &hjkl_buffer::View,
    mut start: usize,
    mut end: usize,
) -> (usize, usize) {
    let folds = buf.folds();
    if folds.is_empty() {
        return (start, end);
    }
    loop {
        let mut changed = false;
        for f in &folds {
            if !f.closed {
                continue;
            }
            // Does this closed fold overlap the current range?
            if f.start_row <= end && f.end_row >= start {
                if f.start_row < start {
                    start = f.start_row;
                    changed = true;
                }
                if f.end_row > end {
                    end = f.end_row;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    (start, end)
}
pub(crate) fn execute_line_op<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    op: Operator,
    count: usize,
) {
    // Folded counts (`2d3d` → 6) can exceed a single prefix's cap; re-clamp
    // at vim's count ceiling (`:h count`).
    let count = count.min(MAX_COUNT);
    let (row, col) = ed.cursor();
    let total = buf_row_count(ed.buffer());
    // Vim: `[count]op` for a linewise operator implies a `count_` motion that
    // moves `count - 1` lines down. On the last line that motion can't move at
    // all, so the whole operator aborts (E16) — `2dd`/`2yy`/`5>>`/`5<<` on the
    // final line are no-ops, not "operate on the one remaining line". When the
    // cursor is above the last line the motion clamps to the buffer end instead.
    //
    // A trailing newline is stored as a phantom empty final row, so the last
    // *content* line is one above it; use that as the boundary.
    let last_content_row = if total >= 2
        && buf_line(ed.buffer(), total - 1)
            .map(|s| s.is_empty())
            .unwrap_or(false)
    {
        total - 2
    } else {
        total.saturating_sub(1)
    };
    if count >= 2 && row >= last_content_row {
        return;
    }
    let end_row = (row + count.saturating_sub(1)).min(total.saturating_sub(1));

    // Vim: a linewise operator (`dd`/`yy`/`cc`/`>>`/…) with the cursor on a
    // CLOSED fold operates on the ENTIRE fold, not just the cursor line. Expand
    // the `[row, end_row]` range to cover any closed fold it touches (repeats
    // until stable so nested folds are absorbed too).
    let (row, end_row) = expand_linewise_over_closed_folds(ed.buffer(), row, end_row);

    match op {
        Operator::Yank => {
            // yy must not move the cursor.
            let text = read_vim_range(ed, (row, col), (end_row, 0), RangeKind::Linewise);
            if !text.is_empty() {
                ed.record_yank_to_host(text.clone());
                let target = vim_mut(ed).pending_register.take();
                ed.record_yank(text, true, target);
            }
            // Vim `:h '[` / `:h ']`: yy/Nyy — linewise yank; `[` =
            // (top_row, 0), `]` = (bot_row, last_col).
            let last_col = buf_line_chars(ed.buffer(), end_row).saturating_sub(1);
            ed.set_mark('[', (row, 0));
            ed.set_mark(']', (end_row, last_col));
            buf_set_cursor_rc(ed.buffer_mut(), row, col);
            ed.push_buffer_cursor_to_textarea();
            vim_mut(ed).mode = Mode::Normal;
        }
        Operator::Delete => {
            ed.push_undo();
            let deleted_through_last = end_row + 1 >= total;
            cut_vim_range(ed, (row, col), (end_row, 0), RangeKind::Linewise);
            // Vim's `dd` / `Ndd` leaves the cursor on the *first
            // non-blank* of the line that now occupies `row` — or, if
            // the deletion consumed the last line, the line above it.
            let total_after = buf_row_count(ed.buffer());
            let raw_target = if deleted_through_last {
                row.saturating_sub(1).min(total_after.saturating_sub(1))
            } else {
                row.min(total_after.saturating_sub(1))
            };
            // Clamp off the trailing phantom empty row that arises from a
            // buffer with a trailing newline (stored as ["...", ""]). If
            // the target row is the trailing empty row and there is a real
            // content row above it, use that instead — matching vim's view
            // that the trailing `\n` is a terminator, not a separator.
            let target_row = if raw_target > 0
                && raw_target + 1 == total_after
                && buf_line(ed.buffer(), raw_target)
                    .map(|s| s.is_empty())
                    .unwrap_or(false)
            {
                raw_target - 1
            } else {
                raw_target
            };
            buf_set_cursor_rc(ed.buffer_mut(), target_row, 0);
            ed.push_buffer_cursor_to_textarea();
            move_first_non_whitespace(ed);
            ed.set_sticky_col(Some(ed.cursor().1));
            vim_mut(ed).mode = Mode::Normal;
            // Vim `:h '[` / `:h ']`: dd/Ndd — both marks park at the
            // post-delete cursor position (the join point).
            let pos = ed.cursor();
            ed.set_mark('[', pos);
            ed.set_mark(']', pos);
        }
        Operator::Change => {
            // `cc` / `3cc`: delegate to the shared linewise-change helper
            // which preserves the first line's indent, leaves one row open,
            // and enters insert mode.
            change_linewise_rows(ed, row, end_row);
        }
        Operator::Uppercase | Operator::Lowercase | Operator::ToggleCase | Operator::Rot13 => {
            // `gUU` / `guu` / `g~~` / `g??` — linewise case/rot13 transform over
            // [row, end_row]. Preserve cursor on `row` (first non-blank
            // lines up with vim's behaviour).
            apply_case_op_to_selection(ed, op, (row, col), (end_row, 0), RangeKind::Linewise);
            // After case-op on a linewise range vim puts the cursor on
            // the first non-blank of the starting line.
            move_first_non_whitespace(ed);
        }
        Operator::Indent | Operator::Outdent => {
            // `>>` / `N>>` / `<<` / `N<<` — linewise indent / outdent.
            ed.push_undo();
            if op == Operator::Indent {
                indent_rows(ed, row, end_row, 1);
            } else {
                outdent_rows(ed, row, end_row, 1);
            }
            ed.set_sticky_col(Some(ed.cursor().1));
            vim_mut(ed).mode = Mode::Normal;
        }
        // No doubled form — `zfzf` is two consecutive `zf` chords.
        Operator::Fold => unreachable!("Fold has no line-op double"),
        Operator::Reflow => {
            // `gqq` / `Ngqq` — reflow `count` rows starting at the cursor.
            ed.push_undo();
            reflow_rows(ed, row, end_row);
            move_first_non_whitespace(ed);
            ed.set_sticky_col(Some(ed.cursor().1));
            vim_mut(ed).mode = Mode::Normal;
        }
        Operator::ReflowKeepCursor => {
            // `gww` / `Ngww` — reflow `count` rows starting at the cursor,
            // but leave the cursor at the character it was on before reflow.
            let saved = ed.cursor();
            ed.push_undo();
            let (before, after) = reflow_rows_keep_cursor(ed, row, end_row);
            let (new_row, new_col) = reflow_keep_cursor(row, saved.0, saved.1, &before, &after);
            buf_set_cursor_rc(ed.buffer_mut(), new_row, new_col);
            ed.push_buffer_cursor_to_textarea();
            ed.set_sticky_col(Some(new_col));
            vim_mut(ed).mode = Mode::Normal;
        }
        Operator::AutoIndent => {
            // `==` / `N==` — auto-indent `count` rows starting at cursor.
            ed.push_undo();
            auto_indent_rows(ed, row, end_row);
            ed.set_sticky_col(Some(ed.cursor().1));
            vim_mut(ed).mode = Mode::Normal;
        }
        Operator::Filter => {
            // Filter is dispatched through Editor::filter_range, not here.
        }
        Operator::Comment => {
            // Comment is dispatched through Editor::toggle_comment_range, not here.
            // The doubled `gcc` path calls toggle_comment_range directly in
            // apply_after_g, then records last_change. execute_line_op should
            // not be reached for Comment — no-op if it is.
        }
    }
}
