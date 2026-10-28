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
pub fn expand_linewise_over_closed_folds(
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
/// Park the cursor where vim's linewise delete leaves it (`dd`, `dj`, `dap`,
/// `Vd`, …): the first non-blank of the row that now holds `top`, or of the
/// row above when the delete took the buffer's last line — never the phantom
/// empty row after a trailing newline, which vim treats as a terminator.
/// `deleted_through_last` has to be measured before the cut, while the row
/// count still includes the deleted rows.
pub fn land_after_linewise_delete<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    top: usize,
    deleted_through_last: bool,
) {
    let total_after = buf_row_count(ed.buffer());
    let raw_target = if deleted_through_last {
        top.saturating_sub(1).min(total_after.saturating_sub(1))
    } else {
        top.min(total_after.saturating_sub(1))
    };
    let target_row = if raw_target > 0
        && raw_target + 1 == total_after
        && buf_line(ed.buffer(), raw_target).is_some_and(|s| s.is_empty())
    {
        raw_target - 1
    } else {
        raw_target
    };
    buf_set_cursor_rc(ed.buffer_mut(), target_row, 0);
    move_first_non_whitespace(ed);
}

/// A doubled line operator (`dd`, `>>`, `3gUU`, …) and its `.` repeat.
pub fn execute_line_op<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    op: Operator,
    count: usize,
) {
    // vim's `nv_lineop`: with no count the doubled operator's motion stays on
    // the line and lands on its first non-blank; the earlier of that and the
    // cursor becomes the operator's start — the cursor the undo step saves,
    // so `>>u` returns there. With a count the motion ends on a later line
    // and the start keeps its column. `gqq` / `gww` keep it either way.
    if count == 1
        && !matches!(
            op,
            Operator::Yank | Operator::Reflow | Operator::ReflowKeepCursor
        )
    {
        let (row, col) = ed.cursor();
        move_first_non_whitespace(ed);
        if ed.cursor().1 > col {
            buf_set_cursor_rc(ed.buffer_mut(), row, col);
        }
    }
    execute_line_op_from_cursor(ed, op, count, 1);
}

/// `op` over `count` lines from the cursor's row, starting where the cursor
/// stands — the Visual-line `.` repeat, which has no `nv_lineop` motion.
pub fn execute_line_op_from_cursor<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    op: Operator,
    count: usize,
    levels: usize,
) {
    line_op_body(ed, op, count, levels);
    remember_operator_column(ed);
}

/// `levels` is the shift count for `>` / `<` (more than 1 only for the `.` of
/// a counted Visual shift); other operators ignore it.
fn line_op_body<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    op: Operator,
    count: usize,
    levels: usize,
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
    if count >= 2 && row >= last_content_row(ed) {
        return;
    }
    let end_row = (row + count.saturating_sub(1)).min(total.saturating_sub(1));

    // Vim: a linewise operator (`dd`/`yy`/`cc`/`>>`/…) with the cursor on a
    // CLOSED fold operates on the ENTIRE fold, not just the cursor line. Expand
    // the `[row, end_row]` range to cover any closed fold it touches (repeats
    // until stable so nested folds are absorbed too).
    let (row, end_row) = expand_linewise_over_closed_folds(ed.buffer(), row, end_row);
    if matches!(op, Operator::Delete | Operator::Yank | Operator::Change)
        && refuse_read_only_register(ed)
    {
        // `dd` / `cc` start on the first non-blank, as vim's `nv_lineop`
        // puts them there before the operator runs.
        buf_set_cursor_rc(ed.buffer_mut(), row, col);
        if op != Operator::Yank {
            move_first_non_whitespace(ed);
        }
        let at = ed.cursor();
        refused_operator(ed, op, at, RangeKind::Linewise);
        return;
    }

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
            vim_mut(ed).mode = Mode::Normal;
        }
        Operator::Delete => {
            ed.push_undo();
            let deleted_through_last = end_row + 1 >= total;
            cut_vim_range(ed, (row, col), (end_row, 0), RangeKind::Linewise);
            land_after_linewise_delete(ed, row, deleted_through_last);
            remember_cursor_column(ed);
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
                indent_rows(ed, row, end_row, levels);
            } else {
                outdent_rows(ed, row, end_row, levels);
            }
            remember_cursor_column(ed);
            vim_mut(ed).mode = Mode::Normal;
        }
        // No doubled form — `zfzf` is two consecutive `zf` chords.
        Operator::Fold => unreachable!("Fold has no line-op double"),
        Operator::Reflow => {
            // `gqq` / `Ngqq` — reflow `count` rows starting at the cursor.
            ed.push_undo();
            reflow_rows(ed, row, end_row);
            move_first_non_whitespace(ed);
            remember_cursor_column(ed);
            vim_mut(ed).mode = Mode::Normal;
        }
        Operator::ReflowKeepCursor => {
            // `gww` / `Ngww` — reflow `count` rows starting at the cursor,
            // but leave the cursor at the character it was on before reflow.
            let saved = ed.cursor();
            ed.push_undo();
            let (before, after, opts) = reflow_rows_keep_cursor(ed, row, end_row);
            let (new_row, new_col) =
                reflow_keep_cursor(row, saved.0, saved.1, &before, &after, opts);
            buf_set_cursor_rc(ed.buffer_mut(), new_row, new_col);
            remember_cursor_column(ed);
            vim_mut(ed).mode = Mode::Normal;
        }
        Operator::AutoIndent => {
            // `==` / `N==` — auto-indent `count` rows starting at cursor.
            ed.push_undo();
            auto_indent_rows(ed, row, end_row);
            remember_cursor_column(ed);
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
