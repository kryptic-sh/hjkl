//! Vim FSM: range ops.
//!
//! Split out of the monolithic `vim.rs` (#267 follow-up).

use hjkl_vim_types::{Mode, Operator, RangeKind};

use super::*;
use crate::vim_state::{vim, vim_mut};
use hjkl_engine::Editor;
use hjkl_engine::buf_helpers::{buf_line_chars, buf_set_cursor_rc};

/// Delete the range `[start, end)` (interpretation determined by `kind`) and
/// stash the deleted text in `register`. `'"'` is the unnamed register.
pub(crate) fn delete_range_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    start: (usize, usize),
    end: (usize, usize),
    kind: RangeKind,
    register: char,
) {
    vim_mut(ed).pending_register = Some(register);
    run_operator_over_range(ed, Operator::Delete, start, end, kind);
}
/// Yank (copy) the range `[start, end)` into `register` without mutating the
/// buffer. `'"'` is the unnamed register.
pub(crate) fn yank_range_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    start: (usize, usize),
    end: (usize, usize),
    kind: RangeKind,
    register: char,
) {
    vim_mut(ed).pending_register = Some(register);
    run_operator_over_range(ed, Operator::Yank, start, end, kind);
}
/// Delete the range `[start, end)` and enter Insert mode (vim `c` operator).
/// The deleted text is stashed in `register`. Mode transitions to Insert on
/// return; the caller must not issue further normal-mode ops until the insert
/// session ends.
pub(crate) fn change_range_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    start: (usize, usize),
    end: (usize, usize),
    kind: RangeKind,
    register: char,
) {
    vim_mut(ed).pending_register = Some(register);
    run_operator_over_range(ed, Operator::Change, start, end, kind);
}
/// Indent (`count > 0`) or outdent (`count < 0`) the row span `[start.0,
/// end.0]`. `shiftwidth` overrides the editor's `settings().shiftwidth` for
/// this call; pass `0` to use the editor setting. The column parts of `start`
/// / `end` are ignored — indent is always linewise.
pub(crate) fn indent_range_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    start: (usize, usize),
    end: (usize, usize),
    count: i32,
    shiftwidth: u32,
) {
    if count == 0 {
        return;
    }
    let (top_row, bot_row) = if start.0 <= end.0 {
        (start.0, end.0)
    } else {
        (end.0, start.0)
    };
    // Temporarily override shiftwidth when the caller provides one.
    let original_sw = ed.settings().shiftwidth;
    if shiftwidth > 0 {
        ed.settings_mut().shiftwidth = shiftwidth as usize;
    }
    ed.push_undo();
    let abs_count = count.unsigned_abs() as usize;
    if count > 0 {
        indent_rows(ed, top_row, bot_row, abs_count);
    } else {
        outdent_rows(ed, top_row, bot_row, abs_count);
    }
    if shiftwidth > 0 {
        ed.settings_mut().shiftwidth = original_sw;
    }
    vim_mut(ed).mode = Mode::Normal;
}
/// Apply a case transformation (`Uppercase` / `Lowercase` / `ToggleCase`) to
/// the range `[start, end)`. Only the three case `Operator` variants are valid;
/// other variants are silently ignored (no-op).
pub(crate) fn case_range_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    start: (usize, usize),
    end: (usize, usize),
    kind: RangeKind,
    op: Operator,
) {
    match op {
        Operator::Uppercase | Operator::Lowercase | Operator::ToggleCase | Operator::Rot13 => {}
        _ => return,
    }
    let (top, bot) = order(start, end);
    apply_case_op_to_selection(ed, op, top, bot, kind);
}
/// Delete a rectangular VisualBlock selection. `top_row`/`bot_row` are
/// inclusive line bounds; `left_col`/`right_col` are inclusive char-column
/// bounds. Short lines that don't reach `right_col` lose only the chars
/// that exist (ragged-edge, matching engine FSM). `register` is honoured;
/// `'"'` selects the unnamed register.
pub(crate) fn delete_block_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    top_row: usize,
    bot_row: usize,
    left_col: usize,
    right_col: usize,
    register: char,
) {
    vim_mut(ed).pending_register = Some(register);
    let saved_anchor = vim(ed).block_anchor;
    let saved_vcol = vim(ed).block_vcol;
    vim_mut(ed).block_anchor = (top_row, left_col);
    vim_mut(ed).block_vcol = right_col;
    // Compute clamped col before the mutable borrow for buf_set_cursor_rc.
    let clamped = right_col.min(buf_line_chars(ed.buffer(), bot_row).saturating_sub(1));
    // Place cursor at bot_row / right_col so block_bounds resolves correctly.
    buf_set_cursor_rc(ed.buffer_mut(), bot_row, clamped);
    apply_block_operator(ed, Operator::Delete, 1);
    // Restore — block_anchor/vcol are only meaningful in VisualBlock mode;
    // after the op we're in Normal so restoring is a no-op for the user but
    // keeps state coherent if the caller inspects fields.
    vim_mut(ed).block_anchor = saved_anchor;
    vim_mut(ed).block_vcol = saved_vcol;
}
/// Yank a rectangular VisualBlock selection into `register`.
pub(crate) fn yank_block_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    top_row: usize,
    bot_row: usize,
    left_col: usize,
    right_col: usize,
    register: char,
) {
    vim_mut(ed).pending_register = Some(register);
    let saved_anchor = vim(ed).block_anchor;
    let saved_vcol = vim(ed).block_vcol;
    vim_mut(ed).block_anchor = (top_row, left_col);
    vim_mut(ed).block_vcol = right_col;
    let clamped = right_col.min(buf_line_chars(ed.buffer(), bot_row).saturating_sub(1));
    buf_set_cursor_rc(ed.buffer_mut(), bot_row, clamped);
    apply_block_operator(ed, Operator::Yank, 1);
    vim_mut(ed).block_anchor = saved_anchor;
    vim_mut(ed).block_vcol = saved_vcol;
}
/// Delete a rectangular VisualBlock selection and enter Insert mode (`c`).
/// The deleted text is stashed in `register`. Mode is Insert on return.
pub(crate) fn change_block_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    top_row: usize,
    bot_row: usize,
    left_col: usize,
    right_col: usize,
    register: char,
) {
    vim_mut(ed).pending_register = Some(register);
    let saved_anchor = vim(ed).block_anchor;
    let saved_vcol = vim(ed).block_vcol;
    vim_mut(ed).block_anchor = (top_row, left_col);
    vim_mut(ed).block_vcol = right_col;
    let clamped = right_col.min(buf_line_chars(ed.buffer(), bot_row).saturating_sub(1));
    buf_set_cursor_rc(ed.buffer_mut(), bot_row, clamped);
    apply_block_operator(ed, Operator::Change, 1);
    vim_mut(ed).block_anchor = saved_anchor;
    vim_mut(ed).block_vcol = saved_vcol;
}
/// Indent (`count > 0`) or outdent (`count < 0`) rows `top_row..=bot_row`.
/// Column bounds are ignored — vim's block indent is always linewise.
/// `count == 0` is a no-op.
pub(crate) fn indent_block_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    top_row: usize,
    bot_row: usize,
    count: i32,
) {
    if count == 0 {
        return;
    }
    ed.push_undo();
    let abs = count.unsigned_abs() as usize;
    if count > 0 {
        indent_rows(ed, top_row, bot_row, abs);
    } else {
        outdent_rows(ed, top_row, bot_row, abs);
    }
    vim_mut(ed).mode = Mode::Normal;
}
/// Auto-indent (v1 dumb shiftwidth) the row span `[start.0, end.0]`. Column
/// parts are ignored — auto-indent is always linewise. See
/// `auto_indent_rows` for the algorithm and its v1 limitations.
pub(crate) fn auto_indent_range_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    start: (usize, usize),
    end: (usize, usize),
) {
    let (top_row, bot_row) = if start.0 <= end.0 {
        (start.0, end.0)
    } else {
        (end.0, start.0)
    };
    ed.push_undo();
    auto_indent_rows(ed, top_row, bot_row);
    vim_mut(ed).mode = Mode::Normal;
}
/// Resolve the range of `iw` (inner word) at the current cursor position.
/// Returns `None` if no word exists at the cursor.
pub(crate) fn text_object_inner_word_bridge<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
) -> Option<((usize, usize), (usize, usize))> {
    word_text_object(ed, true, false, 1)
}
/// Resolve the range of `aw` (around word) at the current cursor position.
/// Includes trailing whitespace (or leading whitespace if no trailing exists).
pub(crate) fn text_object_around_word_bridge<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
) -> Option<((usize, usize), (usize, usize))> {
    word_text_object(ed, false, false, 1)
}
/// Resolve the range of `iW` (inner WORD) at the current cursor position.
/// A WORD is any run of non-whitespace characters (no punctuation splitting).
pub(crate) fn text_object_inner_big_word_bridge<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
) -> Option<((usize, usize), (usize, usize))> {
    word_text_object(ed, true, true, 1)
}
/// Resolve the range of `aW` (around WORD) at the current cursor position.
/// Includes trailing whitespace (or leading whitespace if no trailing exists).
pub(crate) fn text_object_around_big_word_bridge<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
) -> Option<((usize, usize), (usize, usize))> {
    word_text_object(ed, false, true, 1)
}
