//! Vim FSM: visual ops.
//!
//! Split out of the monolithic `vim.rs` (#267 follow-up).

use hjkl_vim_types::{InsertReason, Mode, Motion, Operator, RangeKind};

use hjkl_engine::rope_util::{rope_line_to_str, rope_to_lines_vec};

use super::*;
use crate::vim_state::{vim, vim_mut};
use hjkl_engine::Editor;
use hjkl_engine::buf_helpers::{buf_cursor_pos, buf_line_chars, buf_row_count, buf_set_cursor_rc};

pub(crate) fn apply_visual_operator<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    op: Operator,
    count: usize,
) {
    // `count` is the number of indent levels for `>` / `<` (vim `2>` = two
    // shiftwidths); other visual operators ignore it.
    let levels = count.max(1);
    match vim(ed).mode {
        Mode::VisualLine => {
            let cursor_row = buf_cursor_pos(ed.buffer()).row;
            let top = cursor_row.min(vim(ed).visual_line_anchor);
            let bot = cursor_row.max(vim(ed).visual_line_anchor);
            ed.set_yank_linewise(true);
            match op {
                Operator::Yank => {
                    let text = read_vim_range(ed, (top, 0), (bot, 0), RangeKind::Linewise);
                    if !text.is_empty() {
                        ed.record_yank_to_host(text.clone());
                        let target = vim_mut(ed).pending_register.take();
                        ed.record_yank(text, true, target);
                    }
                    buf_set_cursor_rc(ed.buffer_mut(), top, 0);
                    ed.push_buffer_cursor_to_textarea();
                    vim_mut(ed).mode = Mode::Normal;
                }
                Operator::Delete => {
                    ed.push_undo();
                    cut_vim_range(ed, (top, 0), (bot, 0), RangeKind::Linewise);
                    vim_mut(ed).mode = Mode::Normal;
                }
                Operator::Change => {
                    // Vim `Vc` / `Vjc`: same linewise-change semantics as
                    // `cc` — preserve first line's indent, enter insert.
                    change_linewise_rows(ed, top, bot);
                }
                Operator::Uppercase
                | Operator::Lowercase
                | Operator::ToggleCase
                | Operator::Rot13 => {
                    let bot = buf_cursor_pos(ed.buffer())
                        .row
                        .max(vim(ed).visual_line_anchor);
                    apply_case_op_to_selection(ed, op, (top, 0), (bot, 0), RangeKind::Linewise);
                    move_first_non_whitespace(ed);
                }
                Operator::Indent | Operator::Outdent => {
                    ed.push_undo();
                    let (cursor_row, _) = ed.cursor();
                    let bot = cursor_row.max(vim(ed).visual_line_anchor);
                    if op == Operator::Indent {
                        indent_rows(ed, top, bot, levels);
                    } else {
                        outdent_rows(ed, top, bot, levels);
                    }
                    vim_mut(ed).mode = Mode::Normal;
                }
                Operator::Reflow => {
                    ed.push_undo();
                    let (cursor_row, _) = ed.cursor();
                    let bot = cursor_row.max(vim(ed).visual_line_anchor);
                    reflow_rows(ed, top, bot);
                    vim_mut(ed).mode = Mode::Normal;
                }
                Operator::ReflowKeepCursor => {
                    let saved = ed.cursor();
                    ed.push_undo();
                    let (cursor_row, _) = ed.cursor();
                    let bot = cursor_row.max(vim(ed).visual_line_anchor);
                    let (before, after) = reflow_rows_keep_cursor(ed, top, bot);
                    let (new_row, new_col) =
                        reflow_keep_cursor(top, saved.0, saved.1, &before, &after);
                    buf_set_cursor_rc(ed.buffer_mut(), new_row, new_col);
                    ed.push_buffer_cursor_to_textarea();
                    vim_mut(ed).mode = Mode::Normal;
                }
                Operator::AutoIndent => {
                    ed.push_undo();
                    let (cursor_row, _) = ed.cursor();
                    let bot = cursor_row.max(vim(ed).visual_line_anchor);
                    auto_indent_rows(ed, top, bot);
                    vim_mut(ed).mode = Mode::Normal;
                }
                // Filter is dispatched through Editor::filter_range, not here.
                Operator::Filter => {}
                // Comment is dispatched through the app layer (engine_actions.rs), not here.
                Operator::Comment => {}
                // Visual `zf` is handled inline in `handle_after_z`,
                // never routed through this dispatcher.
                Operator::Fold => unreachable!("Visual zf takes its own path"),
            }
        }
        Mode::Visual => {
            ed.set_yank_linewise(false);
            let anchor = vim(ed).visual_anchor;
            let cursor = ed.cursor();
            let (top, bot) = order(anchor, cursor);
            match op {
                Operator::Yank => {
                    let text = read_vim_range(ed, top, bot, RangeKind::Inclusive);
                    if !text.is_empty() {
                        ed.record_yank_to_host(text.clone());
                        let target = vim_mut(ed).pending_register.take();
                        ed.record_yank(text, false, target);
                    }
                    buf_set_cursor_rc(ed.buffer_mut(), top.0, top.1);
                    ed.push_buffer_cursor_to_textarea();
                    vim_mut(ed).mode = Mode::Normal;
                }
                Operator::Delete => {
                    ed.push_undo();
                    cut_vim_range(ed, top, bot, RangeKind::Inclusive);
                    vim_mut(ed).mode = Mode::Normal;
                }
                Operator::Change => {
                    ed.push_undo();
                    cut_vim_range(ed, top, bot, RangeKind::Inclusive);
                    begin_insert_noundo(ed, 1, InsertReason::AfterChange);
                }
                Operator::Uppercase
                | Operator::Lowercase
                | Operator::ToggleCase
                | Operator::Rot13 => {
                    // Anchor stays where the visual selection started.
                    let anchor = vim(ed).visual_anchor;
                    let cursor = ed.cursor();
                    let (top, bot) = order(anchor, cursor);
                    apply_case_op_to_selection(ed, op, top, bot, RangeKind::Inclusive);
                }
                Operator::Indent | Operator::Outdent => {
                    ed.push_undo();
                    let anchor = vim(ed).visual_anchor;
                    let cursor = ed.cursor();
                    let (top, bot) = order(anchor, cursor);
                    if op == Operator::Indent {
                        indent_rows(ed, top.0, bot.0, levels);
                    } else {
                        outdent_rows(ed, top.0, bot.0, levels);
                    }
                    vim_mut(ed).mode = Mode::Normal;
                }
                Operator::Reflow => {
                    ed.push_undo();
                    let anchor = vim(ed).visual_anchor;
                    let cursor = ed.cursor();
                    let (top, bot) = order(anchor, cursor);
                    reflow_rows(ed, top.0, bot.0);
                    vim_mut(ed).mode = Mode::Normal;
                }
                Operator::ReflowKeepCursor => {
                    let saved = ed.cursor();
                    ed.push_undo();
                    let anchor = vim(ed).visual_anchor;
                    let cursor = ed.cursor();
                    let (top, bot) = order(anchor, cursor);
                    let (before, after) = reflow_rows_keep_cursor(ed, top.0, bot.0);
                    let (new_row, new_col) =
                        reflow_keep_cursor(top.0, saved.0, saved.1, &before, &after);
                    buf_set_cursor_rc(ed.buffer_mut(), new_row, new_col);
                    ed.push_buffer_cursor_to_textarea();
                    vim_mut(ed).mode = Mode::Normal;
                }
                Operator::AutoIndent => {
                    ed.push_undo();
                    let anchor = vim(ed).visual_anchor;
                    let cursor = ed.cursor();
                    let (top, bot) = order(anchor, cursor);
                    auto_indent_rows(ed, top.0, bot.0);
                    vim_mut(ed).mode = Mode::Normal;
                }
                // Filter is dispatched through Editor::filter_range, not here.
                Operator::Filter => {}
                // Comment is dispatched through the app layer (engine_actions.rs), not here.
                Operator::Comment => {}
                Operator::Fold => unreachable!("Visual zf takes its own path"),
            }
        }
        Mode::VisualBlock => apply_block_operator(ed, op, levels),
        _ => {}
    }
}
/// Compute `(top_row, bot_row, left_col, right_col)` for the current
/// VisualBlock selection. Columns are inclusive on both ends. Uses the
/// tracked virtual column (updated by h/l, preserved across j/k) so
/// ragged / empty rows don't collapse the block's width.
pub(crate) fn block_bounds<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
) -> (usize, usize, usize, usize) {
    let (ar, ac) = vim(ed).block_anchor;
    let (cr, _) = ed.cursor();
    let cc = vim(ed).block_vcol;
    let top = ar.min(cr);
    let bot = ar.max(cr);
    let left = ac.min(cc);
    let right = ac.max(cc);
    (top, bot, left, right)
}
/// Update the virtual column after a motion in VisualBlock mode.
/// Horizontal motions sync `block_vcol` to the new cursor column;
/// vertical / non-h/l motions leave it alone so the intended column
/// survives clamping to shorter lines.
///
/// `$` (`Motion::LineEnd`) additionally marks the block "ragged" (`:h
/// v_b_$`, [`crate::vim_state::VimState::block_to_eol`]) — every row then
/// resolves its own right edge to its own EOL until a DIFFERENT
/// horizontal motion re-establishes a fixed column, at which point the
/// flag clears. Vertical motions (the `_` arm) preserve it, matching vim.
pub(crate) fn update_block_vcol<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    motion: &Motion,
) {
    match motion {
        Motion::LineEnd => {
            vim_mut(ed).block_vcol = ed.cursor().1;
            vim_mut(ed).block_to_eol = true;
        }
        Motion::Left
        | Motion::Right
        | Motion::SpaceFwd
        | Motion::BackspaceBack
        | Motion::WordFwd
        | Motion::BigWordFwd
        | Motion::WordBack
        | Motion::BigWordBack
        | Motion::WordEnd
        | Motion::BigWordEnd
        | Motion::WordEndBack
        | Motion::BigWordEndBack
        | Motion::LineStart
        | Motion::FirstNonBlank
        | Motion::Find { .. }
        | Motion::FindRepeat { .. }
        | Motion::MatchBracket => {
            vim_mut(ed).block_vcol = ed.cursor().1;
            vim_mut(ed).block_to_eol = false;
        }
        // Up / Down / FileTop / FileBottom / Search — preserve vcol AND
        // the ragged flag.
        _ => {}
    }
}
/// Yank / delete / change / replace a rectangular selection. Yanked text
/// is stored as one string per row joined with `\n` so pasting reproduces
/// the block as sequential lines. (Vim's true block-paste reinserts as
/// columns; we render the content with our char-wise paste path.)
pub(crate) fn apply_block_operator<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    op: Operator,
    count: usize,
) {
    let (top, bot, left, right) = block_bounds(ed);
    // `$` (`:h v_b_$`) makes the block ragged: every row resolves its own
    // right edge to its own EOL instead of the fixed `right` above. Read
    // once here and thread through — vim's `$`-block affects every
    // operator, not just `d`/`y`/`A`.
    let to_eol = vim(ed).block_to_eol;
    // Snapshot the block text for yank / clipboard.
    let yank = block_yank(ed, top, bot, left, right, to_eol);

    match op {
        Operator::Yank => {
            if !yank.is_empty() {
                ed.record_yank_to_host(yank.clone());
                let target = vim_mut(ed).pending_register.take();
                ed.record_yank(yank, false, target);
            }
            vim_mut(ed).mode = Mode::Normal;
            ed.jump_cursor(top, left);
        }
        Operator::Delete => {
            ed.push_undo();
            delete_block_contents(ed, top, bot, left, right, to_eol);
            if !yank.is_empty() {
                ed.record_yank_to_host(yank.clone());
                let target = vim_mut(ed).pending_register.take();
                ed.record_delete(yank, false, target);
            }
            vim_mut(ed).mode = Mode::Normal;
            ed.jump_cursor(top, left);
        }
        Operator::Change => {
            ed.push_undo();
            // `c`'s replicated typed text always lands at the LEFT column
            // on every row (`:h v_b_c`) — unaffected by a ragged right
            // edge, so only the delete side needs `to_eol`.
            delete_block_contents(ed, top, bot, left, right, to_eol);
            if !yank.is_empty() {
                ed.record_yank_to_host(yank.clone());
                let target = vim_mut(ed).pending_register.take();
                ed.record_delete(yank, false, target);
            }
            ed.jump_cursor(top, left);
            begin_insert_noundo(
                ed,
                1,
                InsertReason::BlockChange {
                    top,
                    bot,
                    col: left,
                },
            );
        }
        Operator::Uppercase | Operator::Lowercase | Operator::ToggleCase | Operator::Rot13 => {
            ed.push_undo();
            transform_block_case(ed, op, top, bot, left, right, to_eol);
            vim_mut(ed).mode = Mode::Normal;
            ed.jump_cursor(top, left);
        }
        Operator::Indent | Operator::Outdent => {
            // VisualBlock `>` / `<` falls back to linewise indent over
            // the block's row range — vim does the same (column-wise
            // indent/outdent doesn't make sense).
            ed.push_undo();
            if op == Operator::Indent {
                indent_rows(ed, top, bot, count.max(1));
            } else {
                outdent_rows(ed, top, bot, count.max(1));
            }
            vim_mut(ed).mode = Mode::Normal;
        }
        Operator::Fold => unreachable!("Visual zf takes its own path"),
        Operator::Reflow => {
            // Reflow over the block falls back to linewise reflow over
            // the row range — column slicing for `gq` doesn't make
            // sense.
            ed.push_undo();
            reflow_rows(ed, top, bot);
            vim_mut(ed).mode = Mode::Normal;
        }
        Operator::ReflowKeepCursor => {
            // `gw` over a block: same fallback as `gq` but restore cursor.
            let saved = ed.cursor();
            ed.push_undo();
            let (before, after) = reflow_rows_keep_cursor(ed, top, bot);
            let (new_row, new_col) = reflow_keep_cursor(top, saved.0, saved.1, &before, &after);
            buf_set_cursor_rc(ed.buffer_mut(), new_row, new_col);
            ed.push_buffer_cursor_to_textarea();
            vim_mut(ed).mode = Mode::Normal;
        }
        Operator::AutoIndent => {
            // AutoIndent over the block falls back to linewise
            // auto-indent over the row range.
            ed.push_undo();
            auto_indent_rows(ed, top, bot);
            vim_mut(ed).mode = Mode::Normal;
        }
        // Filter is dispatched through Editor::filter_range, not here.
        Operator::Filter => {}
        // Comment is dispatched through the app layer (engine_actions.rs), not here.
        Operator::Comment => {}
    }
}
/// In-place case transform over the rectangular block
/// `(top..=bot, left..=right)`. Rows shorter than `left` are left
/// untouched — vim behaves the same way (ragged blocks). When `to_eol` is
/// set (`:h v_b_$`), each row's right edge is ITS OWN EOL instead of the
/// fixed `right`.
pub(crate) fn transform_block_case<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    op: Operator,
    top: usize,
    bot: usize,
    left: usize,
    right: usize,
    to_eol: bool,
) {
    let mut lines: Vec<String> = rope_to_lines_vec(&hjkl_engine::types::Query::rope(ed.buffer()));
    for r in top..=bot.min(lines.len().saturating_sub(1)) {
        let chars: Vec<char> = lines[r].chars().collect();
        if left >= chars.len() {
            continue;
        }
        let row_right = if to_eol { chars.len() } else { right };
        let end = (row_right + 1).min(chars.len());
        let head: String = chars[..left].iter().collect();
        let mid: String = chars[left..end].iter().collect();
        let tail: String = chars[end..].iter().collect();
        let transformed = match op {
            Operator::Uppercase => mid.to_uppercase(),
            Operator::Lowercase => mid.to_lowercase(),
            Operator::ToggleCase => toggle_case_str(&mid),
            Operator::Rot13 => rot13_str(&mid),
            _ => mid,
        };
        lines[r] = format!("{head}{transformed}{tail}");
    }
    let saved_yank = ed.yank().to_string();
    let saved_linewise = ed.yank_linewise();
    ed.restore(lines, (top, left));
    ed.set_yank(saved_yank);
    ed.set_yank_linewise(saved_linewise);
}
/// Yank the rectangular block `(top..=bot, left..=right)`. When `to_eol`
/// is set (`:h v_b_$`), each row's right edge is ITS OWN EOL instead of
/// the fixed `right`.
pub(crate) fn block_yank<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    top: usize,
    bot: usize,
    left: usize,
    right: usize,
    to_eol: bool,
) -> String {
    let rope = hjkl_engine::types::Query::rope(ed.buffer());
    let n = rope.len_lines();
    let mut rows: Vec<String> = Vec::new();
    for r in top..=bot {
        if r >= n {
            break;
        }
        let line = rope_line_to_str(&rope, r);
        let chars: Vec<char> = line.chars().collect();
        let row_right = if to_eol { chars.len() } else { right };
        let end = (row_right + 1).min(chars.len());
        if left >= chars.len() {
            rows.push(String::new());
        } else {
            rows.push(chars[left..end].iter().collect());
        }
    }
    rows.join("\n")
}
/// Delete the rectangular block `(top..=bot, left..=right)`. When
/// `to_eol` is set (`:h v_b_$`), each row deletes to ITS OWN EOL instead
/// of the fixed `right` — this can't be expressed as a single
/// [`hjkl_buffer::Edit::DeleteRange`] (which applies one column pair to
/// every row), so ragged deletes loop one single-row `DeleteRange` per
/// row instead. Both paths land in the caller's undo group (no
/// `push_undo` here).
pub(crate) fn delete_block_contents<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    top: usize,
    bot: usize,
    left: usize,
    right: usize,
    to_eol: bool,
) {
    use hjkl_buffer::{Edit, MotionKind, Position};
    ed.sync_buffer_content_from_textarea();
    let last_row = bot.min(buf_row_count(ed.buffer()).saturating_sub(1));
    if last_row < top {
        return;
    }
    if to_eol {
        for row in top..=last_row {
            let row_right = buf_line_chars(ed.buffer(), row);
            ed.mutate_edit(Edit::DeleteRange {
                start: Position::new(row, left),
                end: Position::new(row, row_right),
                kind: MotionKind::Block,
            });
        }
    } else {
        ed.mutate_edit(Edit::DeleteRange {
            start: Position::new(top, left),
            end: Position::new(last_row, right),
            kind: MotionKind::Block,
        });
    }
    ed.push_buffer_cursor_to_textarea();
}
/// Replace each character cell in the block with `ch`. Ragged (`:h
/// v_b_$`) per row when `vim(ed).block_to_eol` is set.
pub(crate) fn block_replace<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    ch: char,
) {
    let (top, bot, left, right) = block_bounds(ed);
    let to_eol = vim(ed).block_to_eol;
    ed.push_undo();
    ed.sync_buffer_content_from_textarea();
    let mut lines: Vec<String> = rope_to_lines_vec(&hjkl_engine::types::Query::rope(ed.buffer()));
    for r in top..=bot.min(lines.len().saturating_sub(1)) {
        let chars: Vec<char> = lines[r].chars().collect();
        if left >= chars.len() {
            continue;
        }
        let row_right = if to_eol { chars.len() } else { right };
        let end = (row_right + 1).min(chars.len());
        let before: String = chars[..left].iter().collect();
        let middle: String = std::iter::repeat_n(ch, end - left).collect();
        let after: String = chars[end..].iter().collect();
        lines[r] = format!("{before}{middle}{after}");
    }
    reset_textarea_lines(ed, lines);
    vim_mut(ed).mode = Mode::Normal;
    ed.jump_cursor(top, left);
}
/// Replace buffer content with `lines` while preserving the cursor.
/// Used by indent / outdent / block_replace to wholesale rewrite
/// rows without going through the per-edit funnel.
pub(crate) fn reset_textarea_lines<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    lines: Vec<String>,
) {
    let cursor = ed.cursor();
    hjkl_engine::types::BufferEdit::replace_all(ed.buffer_mut(), &lines.join("\n"));
    buf_set_cursor_rc(ed.buffer_mut(), cursor.0, cursor.1);
    ed.mark_content_dirty();
}
