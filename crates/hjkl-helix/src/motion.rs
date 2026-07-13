//! Motions, expressed against the engine's public motion primitives.
//!
//! These move the **head** of the primary selection (the engine's cursor). The
//! caller decides what happens to the anchor: in Normal it collapses onto the
//! head (the selection is replaced), in Select it stays put (the selection is
//! extended). That single difference is most of what makes helix helix.
//!
//! Nothing here re-implements motion logic. Word boundaries honour the editor's
//! `iskeyword`, and vertical motions honour folds, because they go through the
//! same engine primitives vim uses — a discipline that forked them would drift.

use hjkl_buffer::Buffer;
use hjkl_engine::types::Host;
use hjkl_engine::{Editor, SnapshotFoldProvider, motions};

/// The motions this scaffold understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    WordForward,
    WordBack,
    WordEnd,
    LineStart,
    LineEnd,
}

/// Move the cursor (the selection head) by `count`.
pub(crate) fn apply<H: Host>(ed: &mut Editor<Buffer, H>, m: Motion, count: usize) {
    let count = count.max(1);
    let folds = SnapshotFoldProvider::from_buffer(ed.buffer());
    let iskeyword = ed.settings().iskeyword.clone();
    let mut sticky = ed.sticky_col();

    match m {
        Motion::Left => motions::move_left(ed.buffer_mut(), count),
        Motion::Right => motions::move_right_in_line(ed.buffer_mut(), count),
        Motion::Up => motions::move_up(ed.buffer_mut(), &folds, count, &mut sticky),
        Motion::Down => motions::move_down(ed.buffer_mut(), &folds, count, &mut sticky),
        Motion::WordForward => motions::move_word_fwd(ed.buffer_mut(), false, count, &iskeyword),
        Motion::WordBack => motions::move_word_back(ed.buffer_mut(), false, count, &iskeyword),
        Motion::WordEnd => motions::move_word_end(ed.buffer_mut(), false, count, &iskeyword),
        Motion::LineStart => motions::move_line_start(ed.buffer_mut()),
        Motion::LineEnd => motions::move_line_end(ed.buffer_mut()),
    }

    // Vertical motions own the sticky column; horizontal ones clear it, or a `j`
    // after an end-of-line motion would drag the old target column along.
    match m {
        Motion::Up | Motion::Down => ed.set_sticky_col(sticky),
        _ => ed.set_sticky_col(None),
    }
    ed.push_buffer_cursor_to_textarea();
    ed.ensure_cursor_in_scrolloff();
}
