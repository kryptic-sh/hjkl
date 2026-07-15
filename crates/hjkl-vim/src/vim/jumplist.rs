//! Vim FSM: jumplist.
//!
//! Split out of the monolithic `vim.rs` (#267 follow-up).

use hjkl_engine::types::JUMPLIST_MAX;
use hjkl_vim_types::Motion;

use hjkl_engine::Editor;
use hjkl_engine::buf_helpers::{buf_line_chars, buf_row_count};

/// `Ctrl-o` — jump back to the most recent pre-jump position. Saves
/// the current cursor onto the forward stack so `Ctrl-i` can return.
/// Returns `false` when the back stack is empty so counted loops stop.
pub(crate) fn jump_back<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) -> bool {
    let Some(target) = ed.jump_back_list_mut().pop() else {
        return false;
    };
    let cur = ed.cursor();
    ed.jump_fwd_list_mut().push(cur);
    let (r, c) = clamp_pos(ed, target);
    ed.jump_cursor(r, c);
    ed.set_sticky_col(Some(c));
    true
}
/// `Ctrl-i` / `Tab` — redo the last `Ctrl-o`. Saves the current cursor
/// onto the back stack.
/// Returns `false` when the forward stack is empty so counted loops stop.
pub(crate) fn jump_forward<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) -> bool {
    let Some(target) = ed.jump_fwd_list_mut().pop() else {
        return false;
    };
    let cur = ed.cursor();
    ed.jump_back_list_mut().push(cur);
    if ed.jump_back_list().len() > JUMPLIST_MAX {
        ed.jump_back_list_mut().remove(0);
    }
    let (r, c) = clamp_pos(ed, target);
    ed.jump_cursor(r, c);
    ed.set_sticky_col(Some(c));
    true
}
/// Clamp a stored `(row, col)` to the live buffer in case edits
/// shrunk the document between push and pop.
pub(crate) fn clamp_pos<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    pos: (usize, usize),
) -> (usize, usize) {
    let last_row = buf_row_count(ed.buffer()).saturating_sub(1);
    let r = pos.0.min(last_row);
    let line_len = buf_line_chars(ed.buffer(), r);
    let c = pos.1.min(line_len.saturating_sub(1));
    (r, c)
}
/// True for motions that vim treats as jumps (pushed onto the jumplist).
pub(crate) fn is_big_jump(motion: &Motion) -> bool {
    matches!(
        motion,
        Motion::FileTop
            | Motion::FileBottom
            | Motion::MatchBracket
            | Motion::WordAtCursor { .. }
            | Motion::SearchNext { .. }
            | Motion::ViewportTop
            | Motion::ViewportMiddle
            | Motion::ViewportBottom
    )
}
