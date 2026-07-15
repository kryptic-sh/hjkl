//! Vim FSM: bridges.
//!
//! Split out of the monolithic `vim.rs` (#267 follow-up).

use hjkl_vim_types::{InsertEntry, InsertReason, LastChange, Motion, Operator, TextObject};

use super::*;
use crate::vim_state::{vim, vim_mut};
use hjkl_engine::Editor;
use hjkl_engine::buf_helpers::{
    buf_cursor_pos, buf_line, buf_line_chars, buf_row_count, buf_set_cursor_rc,
};

/// `i` — begin Insert at the cursor. `count` is stored in the session for
/// insert-exit replay. Returns `true`.
pub(crate) fn enter_insert_i_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    count: usize,
) {
    begin_insert(ed, count.max(1), InsertReason::Enter(InsertEntry::I));
}
/// `I` — move to first non-blank then begin Insert. `count` stored for replay.
pub(crate) fn enter_insert_shift_i_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    count: usize,
) {
    move_first_non_whitespace(ed);
    begin_insert(ed, count.max(1), InsertReason::Enter(InsertEntry::ShiftI));
}
/// `a` — advance past the cursor char then begin Insert. `count` for replay.
pub(crate) fn enter_insert_a_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    count: usize,
) {
    hjkl_engine::motions::move_right_to_end(ed.buffer_mut(), 1);
    ed.push_buffer_cursor_to_textarea();
    begin_insert(ed, count.max(1), InsertReason::Enter(InsertEntry::A));
}
/// `A` — move to end-of-line then begin Insert. `count` for replay.
pub(crate) fn enter_insert_shift_a_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    count: usize,
) {
    hjkl_engine::motions::move_line_end(ed.buffer_mut());
    hjkl_engine::motions::move_right_to_end(ed.buffer_mut(), 1);
    ed.push_buffer_cursor_to_textarea();
    begin_insert(ed, count.max(1), InsertReason::Enter(InsertEntry::ShiftA));
}
/// `o` — open a new line below the cursor and begin Insert.
/// When `formatoptions` has `o` and the current line is a comment, the
/// continuation prefix is inserted automatically.
pub(crate) fn open_line_below_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    count: usize,
) {
    use hjkl_buffer::{Edit, Position};
    ed.push_undo();
    begin_insert_noundo(ed, count.max(1), InsertReason::Open { above: false });
    ed.sync_buffer_content_from_textarea();
    let row = buf_cursor_pos(ed.buffer()).row;
    let line_chars = buf_line_chars(ed.buffer(), row);
    let prev_line = buf_line(ed.buffer(), row).unwrap_or_default();

    // formatoptions `o`: continue comment on open-below.
    let comment_cont = if ed.settings().formatoptions.contains('o') {
        continue_comment(ed.buffer(), ed.settings(), row)
    } else {
        None
    };

    let suffix = if let Some(cont) = comment_cont {
        format!("\n{cont}")
    } else {
        let indent = compute_enter_indent(ed.settings(), &prev_line);
        format!("\n{indent}")
    };
    ed.mutate_edit(Edit::InsertStr {
        at: Position::new(row, line_chars),
        text: suffix,
    });
    ed.push_buffer_cursor_to_textarea();
}
/// `O` — open a new line above the cursor and begin Insert.
/// When `formatoptions` has `o` and the current line is a comment, the
/// continuation prefix is inserted automatically on the new line above.
pub(crate) fn open_line_above_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    count: usize,
) {
    use hjkl_buffer::{Edit, Position};
    ed.push_undo();
    begin_insert_noundo(ed, count.max(1), InsertReason::Open { above: true });
    ed.sync_buffer_content_from_textarea();
    let row = buf_cursor_pos(ed.buffer()).row;

    // formatoptions `o`: continue comment on open-above (current line drives).
    let comment_cont = if ed.settings().formatoptions.contains('o') {
        continue_comment(ed.buffer(), ed.settings(), row)
    } else {
        None
    };

    // `new_line_content` is the text of the new line (without the trailing `\n`).
    // Used to position the cursor at the end of that content after the move.
    let (insert_text, new_line_content) = if let Some(cont) = comment_cont {
        let content = cont.clone();
        (format!("{cont}\n"), content)
    } else {
        // vim `O` autoindent copies the CURRENT line's indent (the line the
        // cursor sits on, which becomes the line *below* the new one), NOT the
        // line above. Using the line above wrongly inherits a deeper child's
        // indent when the cursor is on a shallower line (e.g. explorer tree:
        // `O` on a dir whose preceding row is its own nested child).
        let cur = buf_line(ed.buffer(), row).unwrap_or_default();
        let indent = compute_enter_indent(ed.settings(), &cur);
        let content = indent.clone();
        (format!("{indent}\n"), content)
    };
    ed.mutate_edit(Edit::InsertStr {
        at: Position::new(row, 0),
        text: insert_text,
    });
    let folds = hjkl_engine::SnapshotFoldProvider::from_buffer(ed.buffer());
    let mut sticky = ed.sticky_col();
    hjkl_engine::motions::move_up(ed.buffer_mut(), &folds, 1, &mut sticky);
    ed.set_sticky_col(sticky);
    let new_row = buf_cursor_pos(ed.buffer()).row;
    buf_set_cursor_rc(ed.buffer_mut(), new_row, new_line_content.chars().count());
    ed.push_buffer_cursor_to_textarea();
}
/// `R` — enter Replace mode (overstrike). `count` stored for replay.
pub(crate) fn enter_replace_mode_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    count: usize,
) {
    // Guard delegated to begin_insert which already checks modifiable/Blame.
    begin_insert(ed, count.max(1), InsertReason::Replace);
}
/// `x` — delete `count` chars forward from the cursor, writing to the unnamed
/// register. Records `LastChange::CharDel` for dot-repeat.
pub(crate) fn delete_char_forward_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    count: usize,
) {
    do_char_delete(ed, true, count.max(1));
    if !vim(ed).replaying {
        vim_mut(ed).last_change = Some(LastChange::CharDel {
            forward: true,
            count: count.max(1),
        });
    }
}
/// `X` — delete `count` chars backward from the cursor, writing to the unnamed
/// register. Records `LastChange::CharDel` for dot-repeat.
pub(crate) fn delete_char_backward_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    count: usize,
) {
    do_char_delete(ed, false, count.max(1));
    if !vim(ed).replaying {
        vim_mut(ed).last_change = Some(LastChange::CharDel {
            forward: false,
            count: count.max(1),
        });
    }
}
/// `s` — substitute `count` chars (delete then enter Insert). Equivalent to
/// `cl`. Records `LastChange::OpMotion` for dot-repeat.
pub(crate) fn substitute_char_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    count: usize,
) {
    use hjkl_buffer::{Edit, MotionKind, Position};
    ed.push_undo();
    ed.sync_buffer_content_from_textarea();
    for _ in 0..count.max(1) {
        let cursor = buf_cursor_pos(ed.buffer());
        let line_chars = buf_line_chars(ed.buffer(), cursor.row);
        if cursor.col >= line_chars {
            break;
        }
        ed.mutate_edit(Edit::DeleteRange {
            start: cursor,
            end: Position::new(cursor.row, cursor.col + 1),
            kind: MotionKind::Char,
        });
    }
    ed.push_buffer_cursor_to_textarea();
    begin_insert_noundo(ed, 1, InsertReason::AfterChange);
    if !vim(ed).replaying {
        vim_mut(ed).last_change = Some(LastChange::OpMotion {
            op: Operator::Change,
            motion: Motion::Right,
            count: count.max(1),
            inserted: None,
        });
    }
}
/// `S` — substitute the whole line (delete line contents then enter Insert).
/// Equivalent to `cc`. Records `LastChange::LineOp` for dot-repeat.
pub(crate) fn substitute_line_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    count: usize,
) {
    execute_line_op(ed, Operator::Change, count.max(1));
    if !vim(ed).replaying {
        vim_mut(ed).last_change = Some(LastChange::LineOp {
            op: Operator::Change,
            count: count.max(1),
            inserted: None,
        });
    }
}
/// `D` — delete from the cursor to end-of-line, writing to the unnamed
/// register. Cursor parks on the new last char. Records for dot-repeat.
pub(crate) fn delete_to_eol_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) {
    ed.push_undo();
    delete_to_eol(ed);
    hjkl_engine::motions::move_left(ed.buffer_mut(), 1);
    ed.push_buffer_cursor_to_textarea();
    if !vim(ed).replaying {
        vim_mut(ed).last_change = Some(LastChange::DeleteToEol { inserted: None });
    }
}
/// `C` — change from the cursor to end-of-line (delete then enter Insert).
/// Equivalent to `c$`. Shares the delete path with `D`.
pub(crate) fn change_to_eol_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) {
    ed.push_undo();
    delete_to_eol(ed);
    begin_insert_noundo(ed, 1, InsertReason::DeleteToEol);
}
/// `Y` — yank from the cursor to end-of-line (same as `y$` in Vim 8 default).
pub(crate) fn yank_to_eol_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    count: usize,
) {
    apply_op_with_motion(ed, Operator::Yank, &Motion::LineEnd, count.max(1));
}
/// `J` — join `count` lines (default 2) onto the current one, inserting a
/// single space between each pair (vim semantics). Records for dot-repeat.
pub(crate) fn join_line_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    count: usize,
) {
    // vim `[count]J` joins `count` lines together — i.e. `count - 1` joins.
    // Bare `J` (and `1J`) join the current line with the one below (1 join).
    let joins = count.max(2) - 1;
    for _ in 0..joins {
        ed.push_undo();
        if !join_line(ed) {
            break;
        }
    }
    if !vim(ed).replaying {
        vim_mut(ed).last_change = Some(LastChange::JoinLine { count: joins });
    }
}
/// `~` — toggle the case of `count` chars from the cursor, advancing right.
/// Records `LastChange::ToggleCase` for dot-repeat.
pub(crate) fn toggle_case_at_cursor_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    count: usize,
) {
    for _ in 0..count.max(1) {
        ed.push_undo();
        if !toggle_case_at_cursor(ed) {
            break;
        }
    }
    if !vim(ed).replaying {
        vim_mut(ed).last_change = Some(LastChange::ToggleCase {
            count: count.max(1),
        });
    }
}
/// `p` — paste the unnamed register (or `"reg` register) after the cursor.
/// Linewise yanks open a new line below; charwise pastes inline.
/// Records `LastChange::Paste` for dot-repeat.
pub(crate) fn paste_after_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    count: usize,
) {
    paste_bridge(ed, false, count, false, false);
}
/// `P` — paste the unnamed register (or `"reg` register) before the cursor.
/// Linewise yanks open a new line above; charwise pastes inline.
/// Records `LastChange::Paste` for dot-repeat.
pub(crate) fn paste_before_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    count: usize,
) {
    paste_bridge(ed, true, count, false, false);
}
/// Shared paste entry for `p`/`P`, `gp`/`gP` (`cursor_after`), and
/// `]p`/`[p` (`reindent`). Records `LastChange::Paste` for dot-repeat.
pub(crate) fn paste_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    before: bool,
    count: usize,
    cursor_after: bool,
    reindent: bool,
) {
    do_paste(ed, before, count.max(1), cursor_after, reindent);
    if !vim(ed).replaying {
        vim_mut(ed).last_change = Some(LastChange::Paste {
            before,
            count: count.max(1),
            cursor_after,
            reindent,
        });
    }
}
/// `<C-o>` — jump back `count` entries in the jumplist, saving the current
/// position on the forward stack so `<C-i>` can return.
pub(crate) fn jump_back_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    count: usize,
) {
    for _ in 0..count.max(1) {
        if !jump_back(ed) {
            break;
        }
    }
}
/// `<C-i>` / `Tab` — redo `count` jumps on the forward stack, saving the
/// current position on the backward stack.
pub(crate) fn jump_forward_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    count: usize,
) {
    for _ in 0..count.max(1) {
        if !jump_forward(ed) {
            break;
        }
    }
}
pub(crate) fn force_normal_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) {
    vim_mut(ed).force_normal();
}
pub(crate) fn mouse_click_doc_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    row: usize,
    col: usize,
) {
    if vim(ed).is_visual() {
        vim_mut(ed).force_normal();
    }
    // Mouse-position click counts as a motion — break the active
    // insert-mode undo group when the toggle is on (vim parity).
    break_undo_group_in_insert(ed);

    let max_row = buf_row_count(ed.buffer()).saturating_sub(1);
    let r = row.min(max_row);
    let line_len = buf_line(ed.buffer(), r)
        .map(|l| l.chars().count())
        .unwrap_or(0);
    let cap = if vim(ed).current_mode == hjkl_engine::VimMode::Insert {
        line_len
    } else {
        line_len.saturating_sub(1)
    };
    let c = col.min(cap);
    buf_set_cursor_rc(ed.buffer_mut(), r, c);
    ed.set_sticky_col(Some(c));
}
pub(crate) fn mouse_begin_drag_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) {
    if !vim(ed).is_visual_char() {
        enter_visual_char_bridge(ed);
    }
}
pub(crate) fn range_for_op_motion_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    motion_key: char,
    total_count: usize,
) -> Option<(usize, usize)> {
    let start = ed.cursor();
    // Reuse the same logic as apply_op_motion_key but only read the
    // target row — we parse the motion, apply it to move the cursor,
    // then immediately restore.
    let input = hjkl_engine::input::Input {
        key: hjkl_engine::input::Key::Char(motion_key),
        ctrl: false,
        alt: false,
        shift: false,
    };
    let motion = parse_motion(&input)?;
    // Resolve FindRepeat and cw/cW quirks just like apply_op_motion_key.
    let motion = match motion {
        Motion::FindRepeat { reverse } => match vim(ed).last_find {
            Some((ch, forward, till)) => Motion::Find {
                ch,
                forward: if reverse { !forward } else { forward },
                till,
            },
            None => return None,
        },
        m => m,
    };
    apply_motion_cursor_ctx(ed, &motion, total_count, true);
    let end = ed.cursor();
    // Restore cursor.
    buf_set_cursor_rc(ed.buffer_mut(), start.0, start.1);
    let (r0, r1) = (start.0.min(end.0), start.0.max(end.0));
    Some((r0, r1))
}
pub(crate) fn range_for_op_g_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    ch: char,
    total_count: usize,
) -> Option<(usize, usize)> {
    let start = ed.cursor();
    let motion = match ch {
        'g' => Motion::FileTop,
        'e' => Motion::WordEndBack,
        'E' => Motion::BigWordEndBack,
        'j' => Motion::ScreenDown,
        'k' => Motion::ScreenUp,
        _ => return None,
    };
    apply_motion_cursor_ctx(ed, &motion, total_count, true);
    let end = ed.cursor();
    buf_set_cursor_rc(ed.buffer_mut(), start.0, start.1);
    let (r0, r1) = (start.0.min(end.0), start.0.max(end.0));
    Some((r0, r1))
}
pub(crate) fn range_for_op_text_obj_bridge<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    ch: char,
    inner: bool,
    total_count: usize,
) -> Option<(usize, usize)> {
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
        _ => return None,
    };
    let (start, end, _kind) = text_object_range(ed, obj, inner, total_count.max(1))?;
    let (r0, r1) = (start.0.min(end.0), start.0.max(end.0));
    Some((r0, r1))
}
/// `n` / `N` — repeat the last search `count` times. `forward = true` means
/// repeat in the original search direction; `false` inverts it (like `N`).
pub(crate) fn search_repeat_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    forward: bool,
    count: usize,
) {
    if let Some(pattern) = ed.last_search_pattern() {
        ed.push_search_pattern(&pattern);
    }
    if ed.search_state().pattern.is_none() {
        return;
    }
    let go_forward = ed.last_search_forward() == forward;
    for _ in 0..count.max(1) {
        if go_forward {
            ed.search_advance_forward(true);
        } else {
            ed.search_advance_backward(true);
        }
    }
    ed.push_buffer_cursor_to_textarea();
}
/// `*` / `#` / `g*` / `g#` — search for the word under the cursor.
/// `forward` picks search direction; `whole_word` wraps in `\b...\b`.
/// `count` repeats the advance.
pub(crate) fn word_search_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    forward: bool,
    whole_word: bool,
    count: usize,
) {
    word_at_cursor_search(ed, forward, whole_word, count.max(1));
}
