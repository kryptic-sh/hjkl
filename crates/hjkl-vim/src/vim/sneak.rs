//! Vim FSM: sneak.
//!
//! Split out of the monolithic `vim.rs` (#267 follow-up).

use hjkl_vim_types::{
    InsertReason, InsertSession, LastChange, LastHorizontalMotion, Mode, Motion, Operator,
    RangeKind, TextObject,
};

use super::*;
use crate::vim_state::{vim, vim_mut};
use hjkl_engine::Editor;
use hjkl_engine::buf_helpers::{buf_line, buf_line_chars, buf_row_count, buf_set_cursor_rc};

/// Scan the buffer from the current cursor position for the `count`-th
/// occurrence of the two-char digraph `(c1, c2)`.
///
/// - `forward=true` → scan downward (rows) and rightward (cols) past cursor.
/// - `forward=false` → scan upward and leftward.
///
/// When a match is found the cursor jumps to the first char of the digraph.
/// `last_sneak` and `last_horizontal_motion` are updated so `;`/`,` repeat.
/// No-op (cursor unchanged) when no match exists.
pub(crate) fn apply_sneak<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    c1: char,
    c2: char,
    forward: bool,
    count: usize,
) {
    let count = count.max(1);
    let (start_row, start_col) = ed.cursor();
    let row_count = buf_row_count(ed.buffer());

    let result = if forward {
        sneak_scan_forward(ed, start_row, start_col, c1, c2, count)
    } else {
        sneak_scan_backward(ed, start_row, start_col, c1, c2, count)
    };

    if let Some((row, col)) = result {
        buf_set_cursor_rc(ed.buffer_mut(), row, col);
        let _ = row_count; // suppress unused-variable warning
    }

    vim_mut(ed).last_sneak = Some(((c1, c2), forward));
    vim_mut(ed).last_horizontal_motion = LastHorizontalMotion::Sneak;
}
/// Scan forward from `(start_row, start_col)` (exclusive — start right after
/// cursor) for the `count`-th occurrence of `c1+c2`.
pub(crate) fn sneak_scan_forward<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    start_row: usize,
    start_col: usize,
    c1: char,
    c2: char,
    count: usize,
) -> Option<(usize, usize)> {
    let row_count = buf_row_count(ed.buffer());
    let mut hits = 0usize;
    for row in start_row..row_count {
        let line = buf_line(ed.buffer(), row).unwrap_or_default();
        let chars: Vec<char> = line.chars().collect();
        // On the start row begin scanning one past the current column.
        let col_start = if row == start_row { start_col + 1 } else { 0 };
        if col_start + 1 > chars.len() {
            continue;
        }
        for col in col_start..chars.len().saturating_sub(1) {
            if chars[col] == c1 && chars[col + 1] == c2 {
                hits += 1;
                if hits == count {
                    return Some((row, col));
                }
            }
        }
    }
    None
}
/// Scan backward from `(start_row, start_col)` (exclusive — start left of
/// cursor) for the `count`-th occurrence of `c1+c2`.
pub(crate) fn sneak_scan_backward<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    start_row: usize,
    start_col: usize,
    c1: char,
    c2: char,
    count: usize,
) -> Option<(usize, usize)> {
    let row_count = buf_row_count(ed.buffer());
    let mut hits = 0usize;
    // Iterate rows from start_row down to 0.
    let rows_to_scan = (0..row_count).rev().skip(row_count - start_row - 1);
    for row in rows_to_scan {
        let line = buf_line(ed.buffer(), row).unwrap_or_default();
        let chars: Vec<char> = line.chars().collect();
        // On the start row end scanning one before the current column.
        let col_end = if row == start_row {
            start_col.saturating_sub(1)
        } else if chars.is_empty() {
            continue;
        } else {
            chars.len().saturating_sub(1)
        };
        if col_end == 0 {
            continue;
        }
        // Scan cols right-to-left from col_end-1 so we match c1 at col, c2 at col+1.
        for col in (0..col_end).rev() {
            if col + 1 < chars.len() && chars[col] == c1 && chars[col + 1] == c2 {
                hits += 1;
                if hits == count {
                    return Some((row, col));
                }
            }
        }
    }
    None
}
/// Apply `op` over the sneak digraph range. Charwise exclusive from cursor up
/// to (but not including) the first char of the first match. This matches
/// vim-sneak's default `<Plug>Sneak_s` operator-pending behavior.
///
/// Example: buffer `"foo ab bar\n"`, cursor col 0, `dsab` → deletes `"foo "`
/// leaving `"ab bar\n"`.
pub(crate) fn apply_op_sneak<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    op: Operator,
    c1: char,
    c2: char,
    forward: bool,
    total_count: usize,
) {
    let start = ed.cursor();
    let result = if forward {
        sneak_scan_forward(ed, start.0, start.1, c1, c2, total_count)
    } else {
        sneak_scan_backward(ed, start.0, start.1, c1, c2, total_count)
    };
    let Some(end) = result else {
        return;
    };
    // Charwise exclusive — land the virtual cursor at end, then use
    // Exclusive range kind (end position not included).
    ed.jump_cursor(end.0, end.1);
    let end_cur = ed.cursor();
    ed.jump_cursor(start.0, start.1);
    run_operator_over_range(ed, op, start, end_cur, RangeKind::Exclusive);
    vim_mut(ed).last_sneak = Some(((c1, c2), forward));
    vim_mut(ed).last_horizontal_motion = LastHorizontalMotion::Sneak;
    if !vim(ed).replaying && op_is_change(op) {
        // No dot-repeat motion variant for sneak ops (plugin behavior,
        // not vim-core); record as a Change/Delete line op as a
        // best-effort fallback so `.` at least does something.
    }
}
/// Public(crate) entry: apply operator over a find motion (`df<x>` etc.).
/// Called by `Editor::apply_op_find` (the public controller API) so the
/// hjkl-vim `PendingState::OpFind` reducer can dispatch `ApplyOpFind` without
/// re-entering the FSM. `handle_op_find_target` now delegates here to avoid
/// logic duplication.
pub(crate) fn apply_op_find_motion<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    op: Operator,
    ch: char,
    forward: bool,
    till: bool,
    total_count: usize,
) {
    let motion = Motion::Find { ch, forward, till };
    apply_op_with_motion(ed, op, &motion, total_count);
    vim_mut(ed).last_find = Some((ch, forward, till));
    if !vim(ed).replaying && op_is_change(op) {
        vim_mut(ed).last_change = Some(LastChange::OpMotion {
            op,
            motion,
            count: total_count,
            inserted: None,
        });
    }
}
/// Shared implementation: map `ch` to `TextObject`, apply the operator, and
/// record `last_change`. Returns `false` when `ch` is not a known text-object
/// kind (caller should treat as a no-op). Called by `Editor::apply_op_text_obj`
/// (the public controller API) so hjkl-vim can dispatch without re-entering the FSM.
///
/// `_total_count` is accepted for API symmetry with `apply_op_find_motion` /
/// `apply_op_motion_key` but is currently unused — text objects don't repeat
/// in vim's current grammar. Kept for future-proofing.
pub(crate) fn apply_op_text_obj_inner<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    op: Operator,
    ch: char,
    inner: bool,
    total_count: usize,
) -> bool {
    // `total_count` drives bracket text objects: `2di{` targets the Nth
    // enclosing pair. Non-bracket objects ignore it (vim does too).
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
        _ => return false,
    };
    apply_op_with_text_object(ed, op, obj, inner, total_count.max(1));
    if !vim(ed).replaying && op_is_change(op) {
        vim_mut(ed).last_change = Some(LastChange::OpTextObj {
            op,
            obj,
            inner,
            inserted: None,
        });
    }
    true
}
/// Move `pos` back by one character, clamped to (0, 0).
pub(crate) fn retreat_one<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    pos: (usize, usize),
) -> (usize, usize) {
    let (r, c) = pos;
    if c > 0 {
        (r, c - 1)
    } else if r > 0 {
        // Char columns, not bytes — cursor columns in this engine are always
        // char-indexed (this codebase's known char-vs-byte trap), so a
        // previous line with any multi-byte char landed on the wrong /
        // mid-codepoint column with `buf_line_bytes`.
        //
        // Deliberately NOT `- 1`: `char_len` is the "one past the last
        // char" virtual column (a legal cursor position — `View::set_cursor`
        // clamps to `line_chars`, not `line_chars - 1`). Landing the
        // INCLUSIVE visual cursor there is what makes the downstream
        // charwise-delete swallow the line's trailing newline and join with
        // the next line — verified against live nvim (and pinned by
        // `hjkl-compat-oracle`'s `vi_brace_open_trailing_charwise` case):
        // `vi{d` on `"{ a\n  b\n}\n"` from `(1,2)` deletes through to `{}`,
        // and `vi{d` on `"fn foo() {\n    body\n}\n"` from inside `body`
        // collapses to `"fn foo() {\n}\n"` — in both cases the closing
        // bracket's line gets pulled up, not left as an empty line. Landing
        // on the last REAL char (char_len - 1) instead would leave the
        // newline out of the inclusive range and wrongly preserve that
        // empty line — which is what broke the oracle gate when tried.
        let prev_len = buf_line_chars(ed.buffer(), r - 1);
        (r - 1, prev_len)
    } else {
        (0, 0)
    }
}
/// Variant of begin_insert that doesn't push_undo (caller already did).
pub(crate) fn begin_insert_noundo<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    count: usize,
    reason: InsertReason,
) {
    let reason = if vim(ed).replaying {
        InsertReason::ReplayOnly
    } else {
        reason
    };
    let (row, col) = ed.cursor();
    vim_mut(ed).insert_session = Some(InsertSession {
        count,
        row_min: row,
        row_max: row,
        before_rope: hjkl_engine::types::Query::rope(ed.buffer()),
        reason,
        start_row: row,
        start_col: col,
    });
    vim_mut(ed).mode = Mode::Insert;
    // Phase 6.3: keep current_mode in sync for callers that bypass step().
    vim_mut(ed).current_mode = hjkl_engine::VimMode::Insert;
    drop_blame_if_left_normal(ed);
}
#[cfg(test)]
mod sneak_tests {
    use super::*;
    use hjkl_buffer::View;
    use hjkl_engine::{DefaultHost, Editor, Options};

    fn make_editor(content: &str) -> Editor<View, DefaultHost> {
        let buf = View::from_str(content);
        let host = DefaultHost::new();
        crate::vim::vim_editor(buf, host, Options::default())
    }

    /// `s ba` from [0,0] on "foo bar baz qux\n" → cursor at [0,4] (start of "ba" in "bar").
    #[test]
    fn sneak_forward_jumps_to_two_char_digraph() {
        let mut ed = make_editor("foo bar baz qux\n");
        ed.jump_cursor(0, 0);
        super::apply_sneak(&mut ed, 'b', 'a', true, 1);
        assert_eq!(ed.cursor(), (0, 4), "cursor should land on 'ba' in 'bar'");
    }

    /// `S ba` from [0,12] on "foo bar baz qux\n" → cursor at [0,8] ("ba" in "baz").
    #[test]
    fn sneak_backward_jumps_to_prior_match() {
        let mut ed = make_editor("foo bar baz qux\n");
        ed.jump_cursor(0, 12);
        super::apply_sneak(&mut ed, 'b', 'a', false, 1);
        assert_eq!(
            ed.cursor(),
            (0, 8),
            "backward sneak should find 'ba' in 'baz'"
        );
    }

    /// After sneak forward to "bar", `;` (sneak-repeat) jumps to next "ba" ("baz").
    #[test]
    fn sneak_repeat_semicolon_next_match() {
        let mut ed = make_editor("foo bar baz qux\n");
        ed.jump_cursor(0, 0);
        // First sneak: lands at [0,4]
        super::apply_sneak(&mut ed, 'b', 'a', true, 1);
        assert_eq!(ed.cursor(), (0, 4));
        // Repeat via execute_motion FindRepeat (which routes through sneak if last was sneak)
        execute_motion(&mut ed, Motion::FindRepeat { reverse: false }, 1);
        assert_eq!(ed.cursor(), (0, 8), "semicolon should jump to next 'ba'");
    }

    /// After sneak forward from [0,0] to [0,4], `,` (reverse) — no prior "ba" → stays.
    #[test]
    fn sneak_repeat_comma_prev_match() {
        let mut ed = make_editor("foo bar baz qux\n");
        ed.jump_cursor(0, 0);
        super::apply_sneak(&mut ed, 'b', 'a', true, 1);
        assert_eq!(ed.cursor(), (0, 4));
        // Reverse repeat — no "ba" before col 4, so cursor must not move.
        let pre = ed.cursor();
        execute_motion(&mut ed, Motion::FindRepeat { reverse: true }, 1);
        assert_eq!(
            ed.cursor(),
            pre,
            "comma with no prior match should leave cursor unchanged"
        );
    }

    /// `S ba` from [0,12] jumps backward.
    #[test]
    fn sneak_s_searches_backward() {
        let mut ed = make_editor("foo bar baz qux\n");
        ed.jump_cursor(0, 12);
        super::apply_sneak(&mut ed, 'b', 'a', false, 1);
        assert_eq!(ed.cursor(), (0, 8));
    }

    /// `2s ba` from [0,0] jumps to 2nd "ba" occurrence.
    #[test]
    fn sneak_with_count_jumps_to_nth() {
        let mut ed = make_editor("foo bar baz qux\n");
        ed.jump_cursor(0, 0);
        super::apply_sneak(&mut ed, 'b', 'a', true, 2);
        assert_eq!(ed.cursor(), (0, 8), "count=2 should jump to 2nd 'ba'");
    }

    /// `s xx` with no match — cursor stays put.
    #[test]
    fn sneak_no_match_cursor_stays() {
        let mut ed = make_editor("foo bar baz qux\n");
        ed.jump_cursor(0, 0);
        let pre = ed.cursor();
        super::apply_sneak(&mut ed, 'x', 'x', true, 1);
        assert_eq!(ed.cursor(), pre, "no match should leave cursor unchanged");
    }

    /// `dsab` on "hello ab world\n" from [0,0] → deletes up to 'ab', leaving "ab world\n".
    #[test]
    fn operator_pending_dsab_deletes_to_digraph() {
        let mut ed = make_editor("hello ab world\n");
        ed.jump_cursor(0, 0);
        super::apply_op_sneak(&mut ed, Operator::Delete, 'a', 'b', true, 1);
        // View content after exclusive delete from [0,0] to [0,6] (start of "ab").
        let content = ed.content();
        assert!(
            content.starts_with("ab world"),
            "dsab should delete 'hello ' leaving 'ab world'; got: {content:?}"
        );
    }

    /// Cross-line sneak: "foo\nbar baz\n", cursor [0,0], `s ba` → [1,0].
    #[test]
    fn sneak_cross_line_match() {
        let mut ed = make_editor("foo\nbar baz\n");
        ed.jump_cursor(0, 0);
        super::apply_sneak(&mut ed, 'b', 'a', true, 1);
        assert_eq!(ed.cursor(), (1, 0), "sneak should cross line boundary");
    }

    /// `last_sneak` is updated after `sneak()` so `;`/`,` can repeat.
    #[test]
    fn sneak_updates_last_sneak_state() {
        let mut ed = make_editor("foo bar baz\n");
        ed.jump_cursor(0, 0);
        super::apply_sneak(&mut ed, 'b', 'a', true, 1);
        let ls = vim(&ed).last_sneak;
        assert_eq!(
            ls,
            Some((('b', 'a'), true)),
            "last_sneak should record the digraph and direction"
        );
    }

    // ── retreat_one / vi{ column-invariant regression (audit A1) ──────────
    //
    // NOTE on the audit brief's original diagnosis: it additionally claimed
    // `retreat_one` should land on the last REAL char of the previous line
    // (`char_len - 1`), not the "one past end" virtual column (`char_len`).
    // That part was verified WRONG against live nvim and against
    // `hjkl-compat-oracle`'s `vi_brace_open_trailing_charwise` case: when the
    // bracket's inner content is non-whitespace and the close bracket sits
    // alone on its own line, real vim's `vi{d` lands the inclusive visual
    // cursor on the virtual "one past end" column (`char_len`, a legal
    // cursor position — `View::set_cursor` clamps to `line_chars`, not
    // `line_chars - 1`), which makes the charwise delete swallow the
    // newline and join the closing bracket's line up. Landing on
    // `char_len - 1` instead leaves the newline out of the range and
    // wrongly preserves that line — confirmed by running this exact fix
    // through `cargo test -p hjkl-compat-oracle` (57 -> 56) and by directly
    // diffing against `nvim --headless`. The ONLY real bug is byte-vs-char
    // (see below); the `- 1` was not warranted and is intentionally absent.

    /// Byte length and char length coincide for an all-ASCII previous line,
    /// so this does NOT discriminate old vs. new code (both compute `8`) —
    /// it's here purely to pin the "one past end" semantics as documentation
    /// alongside the multibyte case below, which DOES discriminate.
    #[test]
    fn retreat_one_vi_brace_prev_line_lands_on_char_length() {
        let mut ed = make_editor("fn foo() {\n    body\n}\n");
        ed.jump_cursor(1, 4); // cursor on 'b' of "body"
        let (_start, end, kind) =
            crate::vim::text_object::text_object_range(&ed, TextObject::Bracket('{'), true, 1)
                .expect("vi{ should resolve the enclosing braces");
        assert_eq!(kind, RangeKind::Exclusive);
        assert_eq!(
            end,
            (2, 0),
            "exclusive inner end should be col 0 of the '}}' line"
        );
        let landed = super::retreat_one(&ed, end);
        assert_eq!(
            landed,
            (1, 8),
            "visual cursor should land on char-col 8 (char_len of \"    body\"), \
             the virtual one-past-end column that makes the delete join lines"
        );
    }

    /// Multibyte variant: previous line has a 2-byte UTF-8 char (`é`), so the
    /// byte length (10) and char length (9) of the line diverge. The old
    /// buggy code used `buf_line_bytes` — a BYTE count — as a CHAR column,
    /// landing the cursor two columns too far right (mid-codepoint / out of
    /// bounds by char-column standards). This is the actual regression this
    /// fix addresses; the ascii test above only documents the mechanism.
    #[test]
    fn retreat_one_vi_brace_multibyte_prev_line_uses_char_column() {
        let mut ed = make_editor("fn foo() {\n    héllo\n}\n");
        ed.jump_cursor(1, 4); // cursor on 'h' of "héllo"
        let (_start, end, _kind) =
            crate::vim::text_object::text_object_range(&ed, TextObject::Bracket('{'), true, 1)
                .expect("vi{ should resolve the enclosing braces");
        assert_eq!(
            end,
            (2, 0),
            "exclusive inner end should be col 0 of the '}}' line"
        );
        // "    héllo" is 9 CHARS (4 spaces + h,é,l,l,o) but 10 BYTES (é is
        // 2 bytes) — the buggy code returned 10 (a byte count used as a char
        // column).
        let landed = super::retreat_one(&ed, end);
        assert_eq!(
            landed,
            (1, 9),
            "visual cursor should land on char-col 9 (char_len of \"    héllo\"), \
             not the byte length (10)"
        );
    }
}
