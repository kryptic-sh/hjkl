//! Vim FSM: op motion.
//!
//! Split out of the monolithic `vim.rs` (#267 follow-up).

use hjkl_vim_types::{InsertReason, LastChange, Mode, Motion, Operator, RangeKind, TextObject};

use super::*;
use crate::vim_state::{vim, vim_mut};
use hjkl_engine::Editor;
use hjkl_engine::buf_helpers::{buf_line, buf_line_chars, buf_row_count, buf_set_cursor_rc};

pub fn apply_op_with_motion<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    op: Operator,
    motion: &Motion,
    count: usize,
) {
    let start = ed.cursor();
    // Peeked before the motion consumes it: a `;`/`,` repeat of `t`/`T`
    // skips the adjacent match, so staying put there means it failed.
    let find_repeat = vim(ed).find_repeat_skip;
    // Tentatively apply motion to find the endpoint. Operator context
    // so `l` on the last char advances past-last (standard vim
    // exclusive-motion endpoint behaviour), enabling `dl` / `cl` /
    // `yl` to cover the final char.
    // `)` goes through vim's own `findsent`, which knows when it stopped on
    // the NUL past the end (`sentence_inclusive`) and when it failed; the
    // Normal-mode `)` cannot report either.
    let mut sentence_inclusive = false;
    if matches!(motion, Motion::SentenceNext) {
        let Some((pos, inclusive)) =
            crate::vim::text_object::sentence_forward_for_operator(ed, count)
        else {
            return;
        };
        ed.jump_cursor(pos.0, pos.1);
        sentence_inclusive = inclusive;
    } else {
        apply_motion_cursor_ctx(ed, motion, count, true);
    }
    let mut end = ed.cursor();
    // vim's `nv_lineop` runs `beginline` for every operator but a yank, so
    // `y_` keeps the column the cursor had.
    if op == Operator::Yank && matches!(motion, Motion::FirstNonBlankLine) {
        end.1 = start.1;
    }
    // Under wrap vim's `gj` / `gk` are charwise exclusive (`nv_screengo`),
    // so `dgj` takes the text between two screen lines of one row; without
    // wrap they are `j` / `k`.
    let screen_charwise =
        matches!(motion, Motion::ScreenUp | Motion::ScreenDown) && screen_wraps(ed);
    // Linewise motions normally cover their row even when their endpoint has
    // the same row. `_` is a successful zero-distance motion, but `j`/`k` and
    // `+`/`-` fail at a buffer edge and must not operate on the current row.
    if start.0 == end.0
        && !screen_charwise
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
    let mut kind = if screen_charwise {
        RangeKind::Exclusive
    } else {
        motion_kind(motion)
    };
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
    //
    // `)` has the same tail when vim's `findsent` stops on the NUL past a
    // line's end (`sentence_inclusive`): `d)` in the last sentence takes its
    // last character, and so does `d)` already on it.
    let last_row = last_content_row(ed);
    let last_len = buf_line_chars(ed.buffer(), last_row);
    let lands_past_the_end = match motion {
        Motion::ParagraphNext => end.0 == last_row && end.1 + 1 == last_len,
        Motion::SentenceNext => sentence_inclusive,
        _ => false,
    };
    if lands_past_the_end && kind == RangeKind::Exclusive && last_len > 0 {
        if start == end {
            end = (end.0, end.1 + 1);
        } else {
            kind = RangeKind::Inclusive;
        }
    }
    // An inclusive motion that succeeds without moving operates on the
    // cursor's character: `d$` on a line's last char, `de` at the end of the
    // buffer, `cw` on a word's last char (vim's `end_word()` `stop` flag),
    // `dtb` with `b` adjacent. Same zero-distance shape as the `}` tail
    // above, and written the same way — a one-char EXCLUSIVE range, because
    // the abort below and `run_operator_over_range` both reject zero-width
    // charwise ranges. Such a range stays on the cursor's character even on a
    // closed fold: `d$` from a folded line's last character deletes just it
    // in nvim 0.12.5, where `d$` from earlier in the line takes the fold.
    let mut in_place = false;
    if start == end
        && end.1 < buf_line_chars(ed.buffer(), end.0)
        && succeeds_in_place(ed, motion, count, find_repeat)
    {
        end = (end.0, end.1 + 1);
        kind = RangeKind::Exclusive;
        in_place = true;
    }
    // `l` from a line's last character cannot move: vim's `nv_right` keeps the
    // cursor there and makes the operator inclusive, so `dl` / `x` take just
    // that character even on a closed fold. The past-the-end landing above
    // already has the one-char shape; only the fold rule differs.
    if matches!(motion, Motion::Right) && start.1 + 1 == buf_line_chars(ed.buffer(), start.0) {
        in_place = true;
    }
    // `c` over a motion that stayed put without cancelling the operator: the
    // empty range still starts Insert at the cursor (`run_operator_over_range`
    // does that for an empty exclusive range), where every other operator has
    // nothing to act on.
    if op == Operator::Change
        && start == end
        && kind != RangeKind::Linewise
        && change_survives_in_place(motion, count)
    {
        run_operator_over_range(ed, op, start, end, RangeKind::Exclusive);
        return;
    }
    let word_fwd = matches!(motion, Motion::WordFwd | Motion::BigWordFwd);
    operate_over_motion_range(ed, op, start, end, kind, word_fwd, !in_place);
}

/// The motions that leave a pending operator in place when they cannot move,
/// measured with `c` against nvim 0.12.5: `h` / `<BS>` at column 0, `l` on an
/// empty line, `0`, `^`, `g0`, `g^`, `|`, `(` and `{` at the buffer start,
/// and `w` / `W` / `e` / `E` / `$` / `g$` / `)` / `}` on an empty last line
/// (vim clears the operator only for a failed `w` or `e` without one). A
/// count does not change that, except for `$` / `g$` and the sentence and
/// paragraph motions, which a count they cannot satisfy fails (`2C` on the
/// last line, `2c(` at the buffer start). `b`, `ge`, `j`, `k`, a missed `f` /
/// `t`, a `%` with no bracket and the line motions do cancel it.
fn change_survives_in_place(motion: &Motion, count: usize) -> bool {
    match motion {
        Motion::SentencePrev
        | Motion::ParagraphPrev
        | Motion::SentenceNext
        | Motion::ParagraphNext
        | Motion::LineEnd
        | Motion::ScreenLineEnd => count <= 1,
        _ => matches!(
            motion,
            Motion::Left
                | Motion::Right
                | Motion::BackspaceBack
                | Motion::SpaceFwd
                | Motion::LineStart
                | Motion::FirstNonBlank
                | Motion::ScreenLineStart
                | Motion::ScreenLineFirstNonBlank
                | Motion::GotoColumn
                | Motion::WordFwd
                | Motion::BigWordFwd
                | Motion::WordEnd
                | Motion::BigWordEnd
                | Motion::ChangeWordEnd { .. }
        ),
    }
}

/// Whether `motion`, having left the cursor where it was, succeeded rather
/// than failed — the motion layer reports no status, so each case reads it
/// back from the buffer. Measured against nvim 0.12.5: `$`, `g$` and `g_` succeed
/// unless a count runs them off the last line (`2d$` there is a no-op), and
/// in place they stand on the line's last (non-blank) character; `e` / `E` at the
/// end of the buffer succeed, counted or not; `t` / `T` succeed in place only
/// on an adjacent target with no count and outside a `;` / `,` repeat; a `%`
/// that finds no match acts on the cursor's character (`d%` on `"ab"` deletes
/// `a`, even with an unmatched bracket further on); `ge` at the buffer start
/// and every other motion fail.
fn succeeds_in_place<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    motion: &Motion,
    count: usize,
    find_repeat: bool,
) -> bool {
    match *motion {
        Motion::LineEnd | Motion::LastNonBlank | Motion::ScreenLineEnd | Motion::MatchBracket => {
            count <= 1
        }
        Motion::WordEnd | Motion::BigWordEnd | Motion::ChangeWordEnd { .. } => true,
        Motion::Find {
            ch,
            forward,
            till: true,
        } => till_succeeds_in_place(ed, ch, forward, count, find_repeat),
        _ => false,
    }
}

/// `d'a` / ``d`a`` — `op` over the motion to mark `ch`: linewise for `'`,
/// charwise exclusive for `` ` ``. An unset mark, or an uppercase one set in
/// another buffer, aborts the operator as it does in vim. A count is ignored,
/// as vim ignores it for mark motions.
pub fn apply_op_mark<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    op: Operator,
    ch: char,
    linewise: bool,
) {
    let target = if ch.is_ascii_uppercase() {
        same_buffer_global_mark_pos(ed, ch)
    } else {
        mark_pos(ed, ch)
    };
    let Some(target) = target else {
        return;
    };
    // Read before the operator consumes it, so `.` can reuse it. A `.` of this
    // change looks the mark up again, and does nothing if it is gone by then.
    let register = vim(ed).pending_register;
    if !vim(ed).replaying && op_is_change(op) {
        vim_mut(ed).last_change = Some(LastChange::OpMark {
            op,
            ch,
            linewise,
            inserted: None,
            register,
        });
    }
    let start = ed.cursor();
    let (row, col) = clamp_pos(ed, target);
    // The endpoint is where the motion lands: for `'` that is the mark row's
    // first non-blank, not the mark's own column, so a backward `y'a` leaves
    // the cursor there.
    let (end, kind) = if linewise {
        buf_set_cursor_rc(ed.buffer_mut(), row, 0);
        move_first_non_whitespace(ed);
        (ed.cursor(), RangeKind::Linewise)
    } else {
        ((row, col), RangeKind::Exclusive)
    };
    operate_over_motion_range(ed, op, start, end, kind, false, true);
}

/// The half of [`apply_op_with_motion`] that runs once the motion's endpoint
/// is known: vim's range adjustments (`:h exclusive`, closed folds, the
/// charwise-delete promotion), then the operator itself. `start` is where the
/// cursor was; `word_fwd` marks `w` / `W`, whose end-of-line special case
/// (`:h word`) is the one adjustment that depends on the motion.
/// `include_folds` is false for a motion that succeeded without moving, whose
/// range stays on the cursor's character inside a closed fold.
fn operate_over_motion_range<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    op: Operator,
    mut start: (usize, usize),
    mut end: (usize, usize),
    mut kind: RangeKind,
    word_fwd: bool,
    include_folds: bool,
) {
    // Where the cursor is parked before the operator runs: vim's operator
    // start, the earlier of the cursor and the motion's end, which is also the
    // cursor the undo step saves (`<ku` / `cbx<Esc>u` return there). `gq` /
    // `gw` keep the cursor's own (`gqku` returns to it in nvim 0.12.5), and
    // `gw` reads the parked cursor as the one to put back. Held separately
    // from `start` because the exclusive-motion adjustment below can move the
    // range's *backward* endpoint, which must not drag the parked cursor.
    let cursor_restore = if matches!(op, Operator::Reflow | Operator::ReflowKeepCursor) {
        start
    } else {
        start.min(end)
    };
    // Linewise motions always cover their row when successful even if
    // `start == end` (e.g. `d_` with count 1 on a single row deletes it).
    if start == end && !matches!(kind, RangeKind::Linewise) {
        return;
    }
    // Vim special case (`:h word`): when `w`/`W` is used with an operator and
    // the last word moved over ends its line, the operated text stops at the
    // end of that line instead of eating the line break into the next line's
    // first word. `d2w` that ends mid-line on a later row is unaffected, since
    // its last word is not at end-of-line. vim's `fwd_word` does this by
    // stopping on the NUL of the last word's line, so the white space after
    // that word goes too (`dw` on `"ab  "` takes `"ab  "`): when the motion
    // crossed onto a later row, `end` becomes that NUL, still exclusive.
    if word_fwd
        && kind == RangeKind::Exclusive
        && end.0 > start.0
        && let Some(word_end) = last_word_end_before(ed, start, end)
        && word_end.0 < end.0
    {
        end = (word_end.0, buf_line_chars(ed.buffer(), word_end.0));
    }
    // Vim's exclusive-motion adjustment (`:h exclusive`), motion form — the
    // same rule `apply_op_with_text_object` applies to inner bracket objects.
    // When an exclusive charwise range ends in column 0 of a later row, the
    // end pulls back to the end of the previous line and the motion becomes
    // inclusive; when the start also sits at or before the first non-blank of
    // its own line, the whole range promotes to linewise. This is what makes
    // `d}` from column 0 delete its paragraph linewise (so `y}P` pastes above
    // like nvim) while `d}` from mid-line stops at end-of-line instead of
    // eating the line break. Vim records that it did this (`end_adjusted`);
    // `gq` reads it below.
    let mut end_adjusted = false;
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
            end_adjusted = true;
        }
    }
    // Vim's `do_pending_operator()`: "Include folded lines completely." An
    // endpoint that lands inside a CLOSED fold is pushed out to that fold's
    // far edge — `:h fold`, "a closed fold is included as a whole" — so
    // `x` / `y$` on a fold take the whole fold, and `dj` / `d3w` whose motion
    // stops part-way into a later fold take all of it. This runs AFTER the
    // word-EOL clamp and the exclusive-motion adjustment: `d3w` whose third
    // word ends at the line above a fold pulls back to that line and never
    // reaches the fold (measured against nvim 0.12.5).
    //
    // `Comment` is dispatched to `toggle_comment_range` below, so it keeps its
    // normal motion-driven path.
    if op != Operator::Comment && include_folds {
        let (top, bot) = order(start, end);
        if kind == RangeKind::Linewise {
            // Linewise: both edges grow to cover every closed fold the row
            // range touches, exactly as `dd` on a fold does.
            let (fold_top, fold_bot) = expand_linewise_over_closed_folds(ed.buffer(), top.0, bot.0);
            let (new_top, new_bot) = ((fold_top, top.1), (fold_bot, bot.1));
            if start == top {
                (start, end) = (new_top, new_bot);
            } else {
                (start, end) = (new_bot, new_top);
            }
        } else if start < end {
            // Charwise, only when the motion went FORWARD: a backward or
            // zero-length charwise motion keeps its range as-is (`db` / `d0`
            // from inside a closed fold delete just the characters before
            // the cursor). The near edge goes to column 0 of the fold it sits
            // in, so `x` / `dw` from column 2 of a closed fold take the fold
            // from its first character.
            let start_fold = expand_linewise_over_closed_folds(ed.buffer(), start.0, start.0);
            if start_fold != (start.0, start.0) {
                start = (start_fold.0, 0);
            }
            // The far edge grows unless it is an exclusive column 0, which
            // covers nothing on its row.
            let (_, fold_end) = expand_linewise_over_closed_folds(ed.buffer(), end.0, end.0);
            if fold_end > end.0 && (end.1 > 0 || kind == RangeKind::Inclusive) {
                // Vim parks the endpoint on the fold's last row at
                // `strlen(line)`, one past its final character; the equivalent
                // hjkl range is that final character, INCLUSIVE. The
                // charwise-delete promotion below then turns `x` / `dw` on a
                // fold into the linewise register nvim records, while `yw` /
                // `cw` keep the charwise one.
                let last_col = buf_line_chars(ed.buffer(), fold_end).saturating_sub(1);
                kind = RangeKind::Inclusive;
                end = (fold_end, last_col);
            }
        }
    }
    // Delete only; `y}` / `c}` keep their charwise range, which is why this is
    // not folded into the adjustment above.
    if delete_goes_linewise(ed, op, start, end, kind) {
        kind = RangeKind::Linewise;
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
    if op == Operator::Reflow && end_adjusted {
        step_past_adjusted_format(ed);
    }
}
/// vim's `op_format`: when the exclusive-motion adjustment pulled the range's
/// end back a line, `gq` leaves the cursor on the line after the formatted
/// text — `gq}` lands on the blank line the motion stopped at.
fn step_past_adjusted_format<H: hjkl_engine::types::Host>(ed: &mut Editor<hjkl_buffer::View, H>) {
    let row = ed.cursor().0;
    if row < last_content_row(ed) {
        buf_set_cursor_rc(ed.buffer_mut(), row + 1, 0);
        move_first_non_whitespace(ed);
    }
}
/// Position of the last non-blank char in the half-open range `[start, end)`,
/// scanning rows from the bottom up. Used to clamp `dw`/`dW` at end-of-line
/// (vim's `:h word` special case): the returned position is the end of the
/// last word moved over, whose row the clamp stops at. For a counted `d{n}w` the last word can sit on the
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
/// Vim's "strange Vi behaviour" in `op_delete()`: a charwise DELETE that
/// spans more than one line, whose range ends with nothing but whitespace left
/// on the last line and whose start sits in the leading indent, is promoted to
/// linewise — so `d}` over the final paragraph removes those rows outright
/// instead of leaving an empty one behind, and the register gets linewise
/// content. It lives in `op_delete`, so motions and text objects (`2daw` from
/// an empty line) both get it.
fn delete_goes_linewise<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    op: Operator,
    start: (usize, usize),
    end: (usize, usize),
    kind: RangeKind,
) -> bool {
    if op != Operator::Delete || kind == RangeKind::Linewise {
        return false;
    }
    let (top, bot) = order(start, end);
    if bot.0 <= top.0 {
        return false;
    }
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
    tail_blank && top.1 <= indent
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
        // A word object that runs out of words cancels the operator but
        // leaves the cursor where vim's walk stopped.
        if let TextObject::Word { big } = obj
            && let Some((row, col)) = word_object_stop(ed, inner, big, count)
        {
            ed.jump_cursor(row, col);
            clamp_cursor_to_normal_mode(ed);
        }
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
    // vim pulls an exclusive end in column 0 back onto the previous line
    // (`:h exclusive`) before `op_delete` looks at it, so `das` that stops at
    // a blank line is a one-line delete; judge the promotion on that end.
    let (judged_end, judged_kind) = if kind == RangeKind::Exclusive && end.1 == 0 && end.0 > start.0
    {
        let prev = end.0 - 1;
        let last = buf_line_chars(ed.buffer(), prev).saturating_sub(1);
        ((prev, last), RangeKind::Inclusive)
    } else {
        (end, kind)
    };
    let end_adjusted = judged_end != end;
    if delete_goes_linewise(ed, op, start, judged_end, judged_kind) {
        // The rows are the adjusted range's: the column-0 end's own row is
        // not part of it.
        end = judged_end;
        kind = RangeKind::Linewise;
    } else if matches!(
        op,
        Operator::Indent
            | Operator::Outdent
            | Operator::Fold
            | Operator::Reflow
            | Operator::ReflowKeepCursor
            | Operator::AutoIndent
            | Operator::Comment
    ) {
        // The operators that act on whole rows take the adjusted end's row
        // too: `>is` on a blank line before a sentence shifts nothing below.
        end = judged_end;
    }
    ed.jump_cursor(start.0, start.1);
    run_operator_over_range(ed, op, start, end, kind);
    if op == Operator::Reflow && end_adjusted {
        step_past_adjusted_format(ed);
    }
}
pub fn motion_kind(motion: &Motion) -> RangeKind {
    match motion {
        Motion::Up | Motion::Down | Motion::ScreenUp | Motion::ScreenDown => RangeKind::Linewise,
        Motion::FileTop | Motion::FileBottom => RangeKind::Linewise,
        Motion::ViewportTop | Motion::ViewportMiddle | Motion::ViewportBottom => {
            RangeKind::Linewise
        }
        Motion::WordEnd
        | Motion::BigWordEnd
        | Motion::ChangeWordEnd { .. }
        | Motion::WordEndBack
        | Motion::BigWordEndBack => RangeKind::Inclusive,
        Motion::Find { .. } => RangeKind::Inclusive,
        Motion::MatchBracket => RangeKind::Inclusive,
        // `[(` / `])` etc. are exclusive: `d])` deletes up to but not including
        // the bracket; `d[(` deletes back to but not past the open bracket.
        Motion::UnmatchedBracket { .. } => RangeKind::Exclusive,
        // `$` now lands on the last char — operator ranges include it.
        Motion::LineEnd => RangeKind::Inclusive,
        // `g_` and `g$` too (`:h g_`, `:h g$`, "inclusive").
        Motion::LastNonBlank | Motion::ScreenLineEnd => RangeKind::Inclusive,
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
    operator_over_range_body(ed, op, start, end, kind);
    // `zf` over a linewise motion leaves the cursor's column alone but makes
    // the curswant 0, as vim does once it starts a linewise range at column 0;
    // over a charwise motion the curswant stays as it was.
    if op != Operator::Fold {
        remember_operator_column(ed);
    } else if kind == RangeKind::Linewise {
        ed.set_sticky_col(Some(0));
    }
}

fn operator_over_range_body<H: hjkl_engine::types::Host>(
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
    // start inserting without deleting anything. Only an EXCLUSIVE range is
    // empty at one position: an inclusive one covers that character, which is
    // what `dw` on a line's last character clamps to (`:h word`).
    if top == bot && kind == RangeKind::Exclusive {
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
            // vim saves the undo state with the cursor already at the range's
            // start, which is where `u` puts it back: `d2k` then `u` lands on
            // the top row, not the row the delete was typed on.
            buf_set_cursor_rc(ed.buffer_mut(), top.0, top.1);
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
                land_after_linewise_delete(ed, top.0, deleted_through_last);
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
                // Charwise change: cut the range and enter insert. The undo
                // step saves the range's start, so `u` returns there even when
                // a closed fold moved it before the cursor (`zfjcl`).
                vim_mut(ed).change_mark_start = Some(top);
                buf_set_cursor_rc(ed.buffer_mut(), top.0, top.1);
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
            let (before, after, opts) = reflow_rows_keep_cursor(ed, top.0, bot.0);
            let (new_row, new_col) =
                reflow_keep_cursor(top.0, saved.0, saved.1, &before, &after, opts);
            buf_set_cursor_rc(ed.buffer_mut(), new_row, new_col);
            remember_cursor_column(ed);
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

    /// `cw` — the motion `apply_op_motion_key` rewrites `c` + `w` into. The
    /// fold jump lands on the fold's last character, which is a word end, so
    /// `end_word()`'s `stop` flag holds the motion there: the fold and no
    /// more, with a charwise register.
    #[test]
    fn cw_on_closed_fold_changes_whole_fold_with_charwise_register() {
        let mut ed = make_editor("abc\ndef\nghi\n", Some((0, 1)));
        apply_op_with_motion(
            &mut ed,
            Operator::Change,
            &Motion::ChangeWordEnd { big: false },
            1,
        );
        assert_eq!(full_buffer(&ed), "\nghi\n");
        assert_eq!(ed.yank(), "abc\ndef");
        assert_eq!(ed.cursor(), (0, 0));
        assert_eq!(crate::vim_state::vim(&ed).mode, Mode::Insert);
    }

    /// `2cw` spends its first iteration on the fold and its second walking on
    /// — one word end past it, where `cw` stopped.
    #[test]
    fn counted_cw_on_closed_fold_walks_past_the_fold() {
        let mut ed = make_editor("abc\ndef\nghi jkl\n", Some((0, 1)));
        apply_op_with_motion(
            &mut ed,
            Operator::Change,
            &Motion::ChangeWordEnd { big: false },
            2,
        );
        assert_eq!(full_buffer(&ed), " jkl\n");
        assert_eq!(ed.yank(), "abc\ndef\nghi");
    }

    /// `ce` has no `stop` flag, so even uncounted it runs past the fold to
    /// the next word's end — the divergence item 2 recorded.
    #[test]
    fn ce_on_closed_fold_runs_past_the_fold() {
        let mut ed = make_editor("abc\ndef\nghi jkl\n", Some((0, 1)));
        apply_op_with_motion(&mut ed, Operator::Change, &Motion::WordEnd, 1);
        assert_eq!(full_buffer(&ed), " jkl\n");
        assert_eq!(ed.yank(), "abc\ndef\nghi");
    }

    /// `de` — same word-end motion under a delete; the range stays charwise
    /// because it stops mid-row.
    #[test]
    fn de_on_closed_fold_runs_past_the_fold() {
        let mut ed = make_editor("abc\ndef\nghi jkl\n", Some((0, 1)));
        apply_op_with_motion(&mut ed, Operator::Delete, &Motion::WordEnd, 1);
        assert_eq!(full_buffer(&ed), " jkl\n");
        assert_eq!(ed.yank(), "abc\ndef\nghi");
    }

    /// `2dw` re-applies the count on top of the fold: the first `w` consumes
    /// the fold and lands on the next row's first word, the second walks on.
    #[test]
    fn counted_dw_on_closed_fold_walks_past_the_fold() {
        let mut ed = make_editor("abc\ndef\nghi jkl\n", Some((0, 1)));
        apply_op_with_motion(&mut ed, Operator::Delete, &Motion::WordFwd, 2);
        assert_eq!(full_buffer(&ed), "jkl\n");
        assert_eq!(ed.yank(), "abc\ndef\nghi ");
    }

    /// `2yw` keeps the charwise register `yw` on a fold already had.
    #[test]
    fn counted_yw_on_closed_fold_walks_past_the_fold() {
        let mut ed = make_editor("abc\ndef\nghi jkl\n", Some((0, 1)));
        apply_op_with_motion(&mut ed, Operator::Yank, &Motion::WordFwd, 2);
        assert_eq!(full_buffer(&ed), "abc\ndef\nghi jkl\n");
        assert_eq!(ed.yank(), "abc\ndef\nghi ");
    }

    /// A motion that STOPS inside a later closed fold takes all of it — vim's
    /// "include folded lines completely" on the operator's far endpoint.
    #[test]
    fn dj_ending_in_a_later_fold_takes_all_of_it() {
        let mut ed = make_editor("aa\nBB\nCC\ndd\n", Some((1, 2)));
        apply_op_with_motion(&mut ed, Operator::Delete, &Motion::Down, 1);
        assert_eq!(full_buffer(&ed), "dd\n");
        assert_eq!(ed.yank(), "aa\nBB\nCC\n");
    }

    /// A backward motion that cannot move aborts the operator — the fold is
    /// never promoted behind it.
    #[test]
    fn db_on_closed_fold_start_is_a_noop() {
        let mut ed = make_editor("abc\ndef\nghi\n", Some((0, 1)));
        apply_op_with_motion(&mut ed, Operator::Delete, &Motion::WordBack, 1);
        assert_eq!(full_buffer(&ed), "abc\ndef\nghi\n");
        assert_eq!(ed.yank(), "");
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
        // `l` opens the fold (`'foldopen'` `hor`), which is what keeps the
        // `dw` charwise; a fold still closed under it would go whole.
        crate::vim::execute_motion(&mut ed, Motion::Right, 1);
        apply_op_with_motion(&mut ed, Operator::Delete, &Motion::WordFwd, 1);
        assert_eq!(full_buffer(&ed), "a\ndef\nghi\n");
        assert_eq!(ed.yank(), "bc");
    }
}
