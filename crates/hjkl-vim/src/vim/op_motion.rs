//! Vim FSM: op motion.
//!
//! Split out of the monolithic `vim.rs` (#267 follow-up).

use hjkl_vim_types::{InsertReason, Mode, Motion, Operator, RangeKind, TextObject};

use super::*;
use crate::vim_state::vim_mut;
use hjkl_engine::Editor;
use hjkl_engine::buf_helpers::{buf_line, buf_line_chars, buf_row_count, buf_set_cursor_rc};

pub fn apply_op_with_motion<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    op: Operator,
    motion: &Motion,
    count: usize,
) {
    // A charwise operator whose cursor sits at COLUMN 0 of a CLOSED fold acts
    // on the whole fold (vim `:h fold`: "a closed fold is included as a
    // whole"). Run it as a single range before any motion is applied — the
    // motion is irrelevant once the fold wins. Delete/case/indent become
    // linewise (so `dw`/`x`/`gUw` record the fold with a trailing newline, like
    // `dd`); Yank/Change run charwise over the fold so `yw`/`cw` record the
    // fold text WITHOUT a trailing newline (nvim keeps those registers
    // charwise even though the fold is consumed whole). At column > 0 nvim
    // keeps the normal charwise semantics — only column 0 triggers the whole-
    // fold promotion. `Comment` is dispatched to `toggle_comment_range` below,
    // so it keeps its normal motion-driven path.
    if op != Operator::Comment {
        let (cursor_row, cursor_col) = ed.cursor();
        let (fold_start, fold_end) =
            expand_linewise_over_closed_folds(ed.buffer(), cursor_row, cursor_row);
        if cursor_col == 0 && (fold_start, fold_end) != (cursor_row, cursor_row) {
            match op {
                Operator::Yank | Operator::Change => {
                    let last_col = buf_line_chars(ed.buffer(), fold_end).saturating_sub(1);
                    run_operator_over_range(
                        ed,
                        op,
                        (fold_start, 0),
                        (fold_end, last_col),
                        RangeKind::Inclusive,
                    );
                }
                _ => {
                    run_operator_over_range(
                        ed,
                        op,
                        (fold_start, 0),
                        (fold_end, 0),
                        RangeKind::Linewise,
                    );
                }
            }
            return;
        }
    }
    let mut start = ed.cursor();
    // Where the cursor is parked before the operator runs. Held separately
    // from `start` because the exclusive-motion adjustment below can move the
    // range's *backward* endpoint, which must not drag the parked cursor.
    let cursor_restore = start;
    // Tentatively apply motion to find the endpoint. Operator context
    // so `l` on the last char advances past-last (standard vim
    // exclusive-motion endpoint behaviour), enabling `dl` / `cl` /
    // `yl` to cover the final char.
    apply_motion_cursor_ctx(ed, motion, count, true);
    let mut end = ed.cursor();
    // Linewise motions normally cover their row even when their endpoint has
    // the same row. `_` is a successful zero-distance motion, but `j`/`k` and
    // `+`/`-` fail at a buffer edge and must not operate on the current row.
    if start.0 == end.0
        && matches!(
            motion,
            Motion::Up
                | Motion::Down
                | Motion::ScreenUp
                | Motion::ScreenDown
                | Motion::FirstNonBlankNextLine
                | Motion::FirstNonBlankPrevLine
        )
    {
        return;
    }
    // If the motion made no progress, abort the operator (vim behavior).
    // A charwise edge motion that cannot advance leaves the buffer unchanged.
    let mut kind = motion_kind(motion);
    // Vim's `findpar` tail (`:h }`): a `}` that lands on the LAST line of the
    // buffer sits on that line's last character and sets `*pincl`, making the
    // motion inclusive — that is what lets `d}` in the final paragraph reach
    // end-of-buffer instead of stopping one char short.
    //
    // Vim applies this to the zero-distance form too: with the cursor already
    // on the last char of the last line, `}` does not move and `d}` still
    // deletes that char. That case cannot be expressed as a zero-width
    // INCLUSIVE range here, because the `start == end` abort below and the
    // matching guard in `run_operator_over_range` reject every zero-width
    // charwise range — and they must keep doing so, or `d$` on an empty line
    // stops being a no-op. It is written as the equivalent one-char EXCLUSIVE
    // range instead, the same shape `dl` on a line's final char produces.
    //
    // `last_row` is the last *content* row: ropey synthesizes a phantom empty
    // final row for a trailing `\n`, and the motion itself stops on the last
    // real row (the `content_row_count` clamp `hjkl_engine::motions` shares
    // with `G`), so this must use the same row or `d}` never sees the landing.
    let last_row = last_content_row(ed);
    if matches!(motion, Motion::ParagraphNext)
        && kind == RangeKind::Exclusive
        && end.0 == last_row
        && buf_line_chars(ed.buffer(), last_row) > 0
        && end.1 + 1 == buf_line_chars(ed.buffer(), last_row)
    {
        if start == end {
            end = (end.0, end.1 + 1);
        } else {
            kind = RangeKind::Inclusive;
        }
    }
    // Linewise motions always cover their row when successful even if
    // `start == end` (e.g. `d_` with count 1 on a single row deletes it).
    if start == end && !matches!(kind, RangeKind::Linewise) {
        return;
    }
    // Vim special case (`:h word`): when `w`/`W` is used with an operator and
    // the last word moved over ends its line, the operated text stops at the
    // end of that word instead of eating the line break into the next line's
    // first word. `d2w` that ends mid-line on a later row is unaffected, since
    // its last word is not at end-of-line. When the word-forward motion
    // crossed onto a later row, clamp `end` back to the last non-blank char it
    // moved over and make the range inclusive.
    if matches!(motion, Motion::WordFwd | Motion::BigWordFwd)
        && kind == RangeKind::Exclusive
        && end.0 > start.0
        && let Some(word_end) = last_word_end_before(ed, start, end)
        && word_end.0 < end.0
    {
        end = word_end;
        kind = RangeKind::Inclusive;
    }
    // Vim's exclusive-motion adjustment (`:h exclusive`), motion form — the
    // same rule `apply_op_with_text_object` applies to inner bracket objects.
    // When an exclusive charwise range ends in column 0 of a later row, the
    // end pulls back to the end of the previous line and the motion becomes
    // inclusive; when the start also sits at or before the first non-blank of
    // its own line, the whole range promotes to linewise. This is what makes
    // `d}` from column 0 delete its paragraph linewise (so `y}P` pastes above
    // like nvim) while `d}` from mid-line stops at end-of-line instead of
    // eating the line break.
    if kind == RangeKind::Exclusive {
        let forward = end.0 >= start.0;
        let (top, bot) = if forward { (start, end) } else { (end, start) };
        if bot.1 == 0 && bot.0 > top.0 {
            let prev = bot.0 - 1;
            let prev_len = buf_line_chars(ed.buffer(), prev);
            let indent = buf_line(ed.buffer(), top.0)
                .unwrap_or_default()
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .count();
            let new_bot = if top.1 <= indent {
                kind = RangeKind::Linewise;
                (prev, 0)
            } else if prev_len > 0 {
                kind = RangeKind::Inclusive;
                (prev, prev_len - 1)
            } else {
                // Previous line is empty: nothing to include, stay exclusive.
                (prev, 0)
            };
            if forward {
                end = new_bot;
            } else {
                start = new_bot;
            }
        }
    }
    // Vim's "strange Vi behaviour" in `op_delete()`: a charwise DELETE that
    // spans more than one line, whose range ends with nothing but whitespace
    // left on the last line and whose start sits in the leading indent, is
    // promoted to linewise — so `d}` over the final paragraph removes those
    // rows outright instead of leaving an empty one behind, and the register
    // gets linewise content. Delete only; `y}` / `c}` keep their charwise
    // range, which is why this is not folded into the adjustment above.
    if op == Operator::Delete && kind != RangeKind::Linewise {
        let (top, bot) = order(start, end);
        if bot.0 > top.0 {
            let bot_line = buf_line(ed.buffer(), bot.0).unwrap_or_default();
            let bot_chars: Vec<char> = bot_line.chars().collect();
            let mut idx = bot.1;
            if idx < bot_chars.len() && kind == RangeKind::Inclusive {
                idx += 1;
            }
            let tail_blank = bot_chars[idx.min(bot_chars.len())..]
                .iter()
                .all(|c| *c == ' ' || *c == '\t');
            let indent = buf_line(ed.buffer(), top.0)
                .unwrap_or_default()
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .count();
            if tail_blank && top.1 <= indent {
                kind = RangeKind::Linewise;
            }
        }
    }
    // Restore cursor before selecting (so Yank leaves cursor at start).
    ed.jump_cursor(cursor_restore.0, cursor_restore.1);

    // Comment is always linewise regardless of motion kind — toggle rows.
    if op == Operator::Comment {
        let top = start.0.min(end.0);
        let bot = start.0.max(end.0);
        ed.toggle_comment_range(top, bot);
        vim_mut(ed).mode = Mode::Normal;
        return;
    }

    run_operator_over_range(ed, op, start, end, kind);
}
/// Position of the last non-blank char in the half-open range `[start, end)`,
/// scanning rows from the bottom up. Used to clamp `dw`/`dW` at end-of-line
/// (vim's `:h word` special case): the returned position is the end of the
/// last word moved over. For a counted `d{n}w` the last word can sit on the
/// landing row itself (before `end.1`), so that row is scanned too — only
/// columns `[.., end.1)` there — which is what keeps `d2w` ending mid-line
/// (last word not at EOL, `word_end.0 == end.0`) from being clamped.
pub fn last_word_end_before<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    start: (usize, usize),
    end: (usize, usize),
) -> Option<(usize, usize)> {
    for r in (start.0..=end.0).rev() {
        let line = buf_line(ed.buffer(), r).unwrap_or_default();
        let lo = if r == start.0 { start.1 } else { 0 };
        let hi = if r == end.0 {
            end.1
        } else {
            line.chars().count()
        };
        let last = line
            .chars()
            .enumerate()
            .filter(|(i, ch)| *i >= lo && *i < hi && *ch != ' ' && *ch != '\t')
            .map(|(i, _)| i)
            .last();
        if let Some(col) = last {
            return Some((r, col));
        }
    }
    None
}
pub fn apply_op_with_text_object<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    op: Operator,
    obj: TextObject,
    inner: bool,
    count: usize,
) {
    // Folded counts can exceed a single prefix's cap; re-clamp at vim's
    // count ceiling (`:h count`).
    let count = count.min(MAX_COUNT);
    let Some((mut start, mut end, mut kind)) = text_object_range(ed, obj, inner, count) else {
        return;
    };
    // vim's exclusive-motion adjustment (`:h exclusive`), applied to the
    // OPERATOR form of an inner bracket object spanning multiple lines (the
    // visual form keeps the raw charwise region). When the exclusive end sits
    // in column 0, pull it back to the end of the previous line and make the
    // motion inclusive; if the start is at or before the first non-blank of its
    // line, promote to linewise. This is what makes `di{` on a contentful
    // multi-line block collapse to bare braces ("{\n}") and a clean block
    // delete its body linewise.
    if inner
        && matches!(obj, TextObject::Bracket(_))
        && kind == RangeKind::Exclusive
        && end.0 > start.0
        && end.1 == 0
    {
        let prev = end.0 - 1;
        let prev_len = buf_line_chars(ed.buffer(), prev);
        let fnb = buf_line(ed.buffer(), start.0)
            .unwrap_or_default()
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .count();
        if start.1 <= fnb {
            start = (start.0, 0);
            end = (prev, prev_len);
            kind = RangeKind::Linewise;
        } else {
            end = (prev, prev_len.saturating_sub(1));
            kind = RangeKind::Inclusive;
        }
    }
    ed.jump_cursor(start.0, start.1);
    run_operator_over_range(ed, op, start, end, kind);
}
pub fn motion_kind(motion: &Motion) -> RangeKind {
    match motion {
        Motion::Up | Motion::Down | Motion::ScreenUp | Motion::ScreenDown => RangeKind::Linewise,
        Motion::FileTop | Motion::FileBottom => RangeKind::Linewise,
        Motion::ViewportTop | Motion::ViewportMiddle | Motion::ViewportBottom => {
            RangeKind::Linewise
        }
        Motion::WordEnd | Motion::BigWordEnd | Motion::WordEndBack | Motion::BigWordEndBack => {
            RangeKind::Inclusive
        }
        Motion::Find { .. } => RangeKind::Inclusive,
        Motion::MatchBracket => RangeKind::Inclusive,
        // `[(` / `])` etc. are exclusive: `d])` deletes up to but not including
        // the bracket; `d[(` deletes back to but not past the open bracket.
        Motion::UnmatchedBracket { .. } => RangeKind::Exclusive,
        // `$` now lands on the last char — operator ranges include it.
        Motion::LineEnd => RangeKind::Inclusive,
        // Linewise motions: +/-/_ land on the first non-blank of a line.
        Motion::FirstNonBlankNextLine
        | Motion::FirstNonBlankPrevLine
        | Motion::FirstNonBlankLine => RangeKind::Linewise,
        // [[/]]/[][/][ are charwise exclusive (land on the brace, brace excluded from operator).
        Motion::SectionBackward
        | Motion::SectionForward
        | Motion::SectionEndBackward
        | Motion::SectionEndForward => RangeKind::Exclusive,
        _ => RangeKind::Exclusive,
    }
}
/// Linewise change of rows `[top_row, end_row]` (vim `cc`/`cj`/`Vc`/`cip`…).
///
/// Deletes the spanned lines, leaves one line carrying the first row's
/// leading whitespace (when `autoindent` is on), parks the cursor after
/// the indent, and enters insert mode. Records the full linewise payload
/// to the yank + delete registers and sets `change_mark_start` for the
/// `[`/`]` deferral. Calls `push_undo` internally — callers must NOT also
/// call it.
pub fn change_linewise_rows<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    top_row: usize,
    end_row: usize,
) {
    use hjkl_buffer::{Edit, MotionKind as BufKind, Position};
    // Vim `:h '[`: stash change start for `]` deferral on insert-exit.
    vim_mut(ed).change_mark_start = Some((top_row, 0));
    ed.push_undo();
    ed.sync_buffer_content_from_textarea();
    // Read the cut payload first so yank reflects every original line.
    let payload = read_vim_range(ed, (top_row, 0), (end_row, 0), RangeKind::Linewise);
    // Drop every row after the first (rows [top_row+1, end_row]).
    if end_row > top_row {
        ed.mutate_edit(Edit::DeleteRange {
            start: Position::new(top_row + 1, 0),
            end: Position::new(end_row, 0),
            kind: BufKind::Line,
        });
    }
    // Preserve the first row's leading whitespace when autoindent is on;
    // wipe the whole line content otherwise (cursor lands at col 0).
    let indent_chars = if ed.settings().autoindent {
        let line =
            hjkl_buffer::rope_line_str(&hjkl_engine::types::Query::rope(ed.buffer()), top_row);
        line.chars().take_while(|c| *c == ' ' || *c == '\t').count()
    } else {
        0
    };
    let line_chars = buf_line_chars(ed.buffer(), top_row);
    if line_chars > indent_chars {
        ed.mutate_edit(Edit::DeleteRange {
            start: Position::new(top_row, indent_chars),
            end: Position::new(top_row, line_chars),
            kind: BufKind::Char,
        });
    }
    if !payload.is_empty() {
        ed.record_yank_to_host(payload.clone());
        let target = vim_mut(ed).pending_register.take();
        ed.record_delete(payload, true, target);
    }
    buf_set_cursor_rc(ed.buffer_mut(), top_row, indent_chars);
    begin_insert_noundo(ed, 1, InsertReason::AfterChange);
}
pub fn run_operator_over_range<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    op: Operator,
    start: (usize, usize),
    end: (usize, usize),
    kind: RangeKind,
) {
    let (top, bot) = order(start, end);
    // Charwise empty range (same position). For Delete/Yank there is nothing to
    // act on. For Change, vim still enters insert at that point — `ci(` on `()`
    // and `ci{` on a whitespace-only block both place the cursor inside and
    // start inserting without deleting anything.
    if top == bot && !matches!(kind, RangeKind::Linewise) {
        if op == Operator::Change {
            vim_mut(ed).change_mark_start = Some(top);
            ed.push_undo();
            begin_insert_noundo(ed, 1, InsertReason::AfterChange);
        }
        return;
    }

    match op {
        Operator::Yank => {
            let text = read_vim_range(ed, top, bot, kind);
            if !text.is_empty() {
                ed.record_yank_to_host(text.clone());
                let target = vim_mut(ed).pending_register.take();
                ed.record_yank(text, matches!(kind, RangeKind::Linewise), target);
            }
            // Vim `:h '[` / `:h ']`: after a yank `[` = first yanked char,
            // `]` = last yanked char. Mode-aware: linewise snaps to line
            // edges; charwise uses the actual inclusive endpoint.
            let rbr = match kind {
                RangeKind::Linewise => {
                    let last_col = buf_line_chars(ed.buffer(), bot.0).saturating_sub(1);
                    (bot.0, last_col)
                }
                RangeKind::Inclusive => (bot.0, bot.1),
                RangeKind::Exclusive => (bot.0, bot.1.saturating_sub(1)),
            };
            ed.set_mark('[', top);
            ed.set_mark(']', rbr);
            buf_set_cursor_rc(ed.buffer_mut(), top.0, top.1);
        }
        Operator::Delete => {
            ed.push_undo();
            // Linewise deletes must not land the cursor on `top.0` when that
            // row no longer exists after the cut (or is now the trailing
            // phantom row) — mirrors the clamp in linewise.rs's `dd` path
            // (H2), which `run_operator_over_range` (reached by
            // motion-driven deletes like `dG`/`dj`) was missing. Capture
            // `total`/`deleted_through_last` before the cut since the row
            // count changes underneath it. `total` includes ropey's phantom
            // trailing row, so `deleted_through_last` only fires when the
            // buffer has no trailing newline (no phantom row to absorb the
            // boundary) — the far more common "reaches the last content
            // line" case is instead caught by the phantom-row pullback
            // below, which must run unconditionally, not just when this
            // flag is set.
            let total = buf_row_count(ed.buffer());
            let deleted_through_last = matches!(kind, RangeKind::Linewise) && bot.0 + 1 >= total;
            cut_vim_range(ed, top, bot, kind);
            // After a charwise / inclusive delete the buffer cursor is
            // placed at `start` by the edit path. In Normal mode the
            // cursor max col is `line_len - 1`; clamp it here so e.g.
            // `d$` doesn't leave the cursor one past the new line end.
            if !matches!(kind, RangeKind::Linewise) {
                clamp_cursor_to_normal_mode(ed);
            } else {
                let total_after = buf_row_count(ed.buffer());
                let raw_target = if deleted_through_last {
                    top.0.saturating_sub(1).min(total_after.saturating_sub(1))
                } else {
                    top.0.min(total_after.saturating_sub(1))
                };
                // Clamp off the trailing phantom empty row that arises from
                // a buffer with a trailing newline (stored as ["...", ""]) —
                // same rule as linewise.rs's `dd` path.
                let target_row = if raw_target > 0
                    && raw_target + 1 == total_after
                    && buf_line(ed.buffer(), raw_target).is_some_and(|s| s.is_empty())
                {
                    raw_target - 1
                } else {
                    raw_target
                };
                buf_set_cursor_rc(ed.buffer_mut(), target_row, 0);
            }
            vim_mut(ed).mode = Mode::Normal;
            // Vim `:h '[` / `:h ']`: after a delete both marks park at
            // the cursor position where the deletion collapsed (the join
            // point). Set after the cut and clamp so the position is final.
            let pos = ed.cursor();
            ed.set_mark('[', pos);
            ed.set_mark(']', pos);
        }
        Operator::Change => {
            // Vim `:h '[`: `[` is set to the start of the changed range
            // before the cut. `]` is deferred to insert-exit (AfterChange
            // path in finish_insert_session) where the cursor sits on the
            // last inserted char.
            if matches!(kind, RangeKind::Linewise) {
                // Linewise change (`cj`/`ck`/`cip`/`cap`/…): preserve the
                // first line's indent and leave exactly one row open for
                // insert. The helper handles push_undo + insert entry.
                change_linewise_rows(ed, top.0, bot.0);
            } else {
                // Charwise change: cut the range and enter insert.
                vim_mut(ed).change_mark_start = Some(top);
                ed.push_undo();
                cut_vim_range(ed, top, bot, kind);
                begin_insert_noundo(ed, 1, InsertReason::AfterChange);
            }
        }
        Operator::Uppercase | Operator::Lowercase | Operator::ToggleCase | Operator::Rot13 => {
            apply_case_op_to_selection(ed, op, top, bot, kind);
        }
        Operator::Indent | Operator::Outdent => {
            // Indent / outdent are always linewise even when triggered
            // by a char-wise motion (e.g. `>w` indents the whole line).
            ed.push_undo();
            if op == Operator::Indent {
                indent_rows(ed, top.0, bot.0, 1);
            } else {
                outdent_rows(ed, top.0, bot.0, 1);
            }
            vim_mut(ed).mode = Mode::Normal;
        }
        Operator::Fold => {
            // Always linewise — fold the spanned rows regardless of the
            // motion's natural kind. Cursor lands on `top.0` to mirror
            // the visual `zf` path.
            if bot.0 >= top.0 {
                ed.apply_fold_op(hjkl_engine::types::FoldOp::Add {
                    start_row: top.0,
                    end_row: bot.0,
                    closed: true,
                });
            }
            buf_set_cursor_rc(ed.buffer_mut(), top.0, top.1);
            vim_mut(ed).mode = Mode::Normal;
        }
        Operator::Reflow => {
            ed.push_undo();
            reflow_rows(ed, top.0, bot.0);
            vim_mut(ed).mode = Mode::Normal;
        }
        Operator::ReflowKeepCursor => {
            // `gw{motion}` — reflow like `gq` but restore the cursor to the
            // character it was on before the reflow (vim's gw behaviour).
            let saved = ed.cursor();
            ed.push_undo();
            let (before, after) = reflow_rows_keep_cursor(ed, top.0, bot.0);
            let (new_row, new_col) = reflow_keep_cursor(top.0, saved.0, saved.1, &before, &after);
            buf_set_cursor_rc(ed.buffer_mut(), new_row, new_col);
            ed.set_sticky_col(Some(new_col));
            vim_mut(ed).mode = Mode::Normal;
        }
        Operator::AutoIndent => {
            // Always linewise — like Indent/Outdent.
            ed.push_undo();
            auto_indent_rows(ed, top.0, bot.0);
            vim_mut(ed).mode = Mode::Normal;
        }
        Operator::Filter => {
            // Filter is not dispatched through run_operator_over_range.
            // The app calls Editor::filter_range directly with a command string.
            // Reaching this arm means a caller invoked run_operator_over_range
            // with Operator::Filter by mistake — silently no-op.
        }
        Operator::Comment => {
            // Comment is dispatched through Editor::toggle_comment_range.
            // Reaching this arm is a caller mistake — silently no-op.
        }
    }
}

#[cfg(test)]
mod fold_charwise_tests {
    use hjkl_buffer::{View, rope_line_str};
    use hjkl_engine::types::FoldOp;
    use hjkl_engine::{DefaultHost, Editor, Options};
    use hjkl_vim_types::{Mode, Motion, Operator};

    use super::apply_op_with_motion;

    fn make_editor(content: &str, fold: Option<(usize, usize)>) -> Editor<View, DefaultHost> {
        let mut ed = crate::vim::vim_editor(
            View::from_str(content),
            DefaultHost::new(),
            Options::default(),
        );
        ed.jump_cursor(0, 0);
        if let Some((start, end)) = fold {
            ed.apply_fold_op(FoldOp::Add {
                start_row: start,
                end_row: end,
                closed: true,
            });
        }
        ed
    }

    fn full_buffer(ed: &Editor<View, DefaultHost>) -> String {
        let rope = ed.buffer().rope();
        (0..rope.len_lines())
            .map(|i| rope_line_str(&rope, i).to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn dw_on_closed_fold_deletes_whole_fold_linewise() {
        let mut ed = make_editor("abc\ndef\nghi\n", Some((0, 1)));
        apply_op_with_motion(&mut ed, Operator::Delete, &Motion::WordFwd, 1);
        assert_eq!(full_buffer(&ed), "ghi\n");
        assert_eq!(ed.yank(), "abc\ndef\n");
        assert_eq!(ed.cursor(), (0, 0));
    }

    #[test]
    fn yw_on_closed_fold_yanks_whole_fold_charwise() {
        let mut ed = make_editor("abc\ndef\nghi\n", Some((0, 1)));
        apply_op_with_motion(&mut ed, Operator::Yank, &Motion::WordFwd, 1);
        assert_eq!(full_buffer(&ed), "abc\ndef\nghi\n");
        assert_eq!(ed.yank(), "abc\ndef");
        assert_eq!(ed.cursor(), (0, 0));
    }

    #[test]
    fn g_upper_w_on_closed_fold_uppercases_whole_fold_linewise() {
        let mut ed = make_editor("abc\ndef\nghi\n", Some((0, 1)));
        apply_op_with_motion(&mut ed, Operator::Uppercase, &Motion::WordFwd, 1);
        assert_eq!(full_buffer(&ed), "ABC\nDEF\nghi\n");
        assert_eq!(ed.yank(), "");
    }

    #[test]
    fn cw_on_closed_fold_changes_whole_fold_with_charwise_register() {
        let mut ed = make_editor("abc\ndef\nghi\n", Some((0, 1)));
        apply_op_with_motion(&mut ed, Operator::Change, &Motion::WordFwd, 1);
        assert_eq!(full_buffer(&ed), "\nghi\n");
        assert_eq!(ed.yank(), "abc\ndef");
        assert_eq!(ed.cursor(), (0, 0));
        assert_eq!(crate::vim_state::vim(&ed).mode, Mode::Insert);
    }

    #[test]
    fn dw_on_plain_buffer_is_unchanged_charwise() {
        let mut ed = make_editor("abc\ndef\nghi\n", None);
        apply_op_with_motion(&mut ed, Operator::Delete, &Motion::WordFwd, 1);
        assert_eq!(full_buffer(&ed), "\ndef\nghi\n");
        assert_eq!(ed.yank(), "abc");
    }

    #[test]
    fn dw_on_single_line_fold_stays_charwise() {
        let mut ed = make_editor("abc\ndef\nghi\n", Some((0, 0)));
        apply_op_with_motion(&mut ed, Operator::Delete, &Motion::WordFwd, 1);
        assert_eq!(full_buffer(&ed), "\ndef\nghi\n");
        assert_eq!(ed.yank(), "abc");
    }

    #[test]
    fn dw_at_col1_on_closed_fold_stays_charwise() {
        let mut ed = make_editor("abc\ndef\nghi\n", Some((0, 1)));
        ed.jump_cursor(0, 1);
        apply_op_with_motion(&mut ed, Operator::Delete, &Motion::WordFwd, 1);
        assert_eq!(full_buffer(&ed), "a\ndef\nghi\n");
        assert_eq!(ed.yank(), "bc");
    }
}
