//! Viewport-math helpers — engine-side relocation of the three
//! viewport-aware methods that lived on `hjkl_buffer::Buffer` through
//! 0.0.41:
//!
//! - `Buffer::ensure_cursor_visible` → [`ensure_cursor_visible`]
//! - `Buffer::cursor_screen_row` → [`cursor_screen_row`]
//! - `Buffer::max_top_for_height` → [`max_top_for_height`]
//!
//! 0.0.42 (Patch C-δ.7): SPEC.md "Viewport on Host" decision excludes
//! viewport math from the `Buffer` trait surface. Pre-0.0.42 the engine
//! reached through to the inherent buffer methods (4 resistant reaches
//! flagged in the 0.0.41 CHANGELOG); this module lifts that math onto
//! engine free fns over `B: Query` + `&dyn FoldProvider` + `&Viewport`.
//! Behavior is byte-for-byte identical to the prior buffer-inherent
//! implementation; the lift is purely a re-homing.
//!
//! The buffer-side `Buffer::ensure_cursor_visible` / `cursor_screen_row`
//! / `max_top_for_height` inherent methods stay in place for now (other
//! call sites — e.g. the buffer's own tests — depend on them). 0.1.0
//! removes the buffer-side copies once every consumer migrates.

use hjkl_buffer::{Viewport, Wrap};

use crate::types::{Cursor, FoldProvider, Query};

/// Bring the cursor into the visible viewport, scrolling by the
/// minimum amount needed. When `viewport.wrap != Wrap::None` and
/// `viewport.text_width > 0`, scrolling is screen-line aware:
/// `top_row` is advanced one visible doc row at a time until the
/// cursor's screen row falls inside the viewport's height.
///
/// Replaces the pre-0.0.42 inherent
/// [`hjkl_buffer::Buffer::ensure_cursor_visible`].
pub fn ensure_cursor_visible<B>(buf: &B, folds: &dyn FoldProvider, viewport: &mut Viewport)
where
    B: Cursor + Query + ?Sized,
{
    let cursor = Cursor::cursor(buf);
    let cursor_row = cursor.line as usize;
    let cursor_col = cursor.col as usize;
    let v = *viewport;
    let wrap_active = !matches!(v.wrap, Wrap::None) && v.text_width > 0;
    if !wrap_active {
        // Re-implement `Viewport::ensure_visible` with the engine's
        // grapheme cursor coords. This mirrors `Viewport::ensure_visible`
        // exactly — kept here so the math doesn't depend on a
        // `Position` that the trait doesn't expose.
        let pos = hjkl_buffer::Position::new(cursor_row, cursor_col);
        viewport.ensure_visible(pos);
        return;
    }
    if v.height == 0 {
        return;
    }
    if cursor_row < v.top_row {
        viewport.top_row = cursor_row;
        viewport.top_col = 0;
        return;
    }
    let height = v.height as usize;
    loop {
        let csr = cursor_screen_row_from(buf, folds, viewport, viewport.top_row);
        match csr {
            Some(row) if row < height => break,
            _ => {}
        }
        let mut next = viewport.top_row + 1;
        while next <= cursor_row && folds.is_row_hidden(next) {
            next += 1;
        }
        if next > cursor_row {
            viewport.top_row = cursor_row;
            break;
        }
        viewport.top_row = next;
    }
    viewport.top_col = 0;
}

/// Cursor's screen row offset (0-based) from `viewport.top_row` under
/// the current wrap mode + `text_width`. `None` when wrap is off, the
/// cursor row is hidden by a fold, or the cursor sits above `top_row`.
///
/// Replaces the pre-0.0.42 inherent
/// [`hjkl_buffer::Buffer::cursor_screen_row`].
pub fn cursor_screen_row<B>(buf: &B, folds: &dyn FoldProvider, viewport: &Viewport) -> Option<usize>
where
    B: Cursor + Query + ?Sized,
{
    if matches!(viewport.wrap, Wrap::None) || viewport.text_width == 0 {
        return None;
    }
    cursor_screen_row_from(buf, folds, viewport, viewport.top_row)
}

/// Earliest `top_row` such that the buffer's screen rows from `top` to
/// the last row total at least `height`. Lets host-side scrolloff math
/// clamp `top_row` so the buffer never leaves blank rows below the
/// content. When the buffer's total screen rows are smaller than
/// `height` this returns 0.
///
/// Replaces the pre-0.0.42 inherent
/// [`hjkl_buffer::Buffer::max_top_for_height`].
pub fn max_top_for_height<B>(
    buf: &B,
    folds: &dyn FoldProvider,
    viewport: &Viewport,
    height: usize,
) -> usize
where
    B: Query + ?Sized,
{
    if height == 0 {
        return 0;
    }
    let row_count = Query::line_count(buf) as usize;
    if row_count == 0 {
        return 0;
    }
    let last = row_count - 1;
    let mut total = 0usize;
    let mut row = last;
    let v = *viewport;
    loop {
        if !folds.is_row_hidden(row) {
            total += if matches!(v.wrap, Wrap::None) || v.text_width == 0 {
                1
            } else {
                let line = Query::line(buf, row as u32);
                hjkl_buffer::wrap::wrap_segments(line, v.text_width, v.wrap).len()
            };
        }
        if total >= height {
            return row;
        }
        if row == 0 {
            return 0;
        }
        row -= 1;
    }
}

/// Cursor's screen row counted from `top` rather than `viewport.top_row`.
/// Internal — drives both [`ensure_cursor_visible`] (which feeds
/// successive candidate `top` rows) and [`cursor_screen_row`].
fn cursor_screen_row_from<B>(
    buf: &B,
    folds: &dyn FoldProvider,
    viewport: &Viewport,
    top: usize,
) -> Option<usize>
where
    B: Cursor + Query + ?Sized,
{
    let cursor = Cursor::cursor(buf);
    let cursor_row = cursor.line as usize;
    let cursor_col = cursor.col as usize;
    if cursor_row < top {
        return None;
    }
    let v = *viewport;
    let mut screen = 0usize;
    for r in top..=cursor_row {
        if folds.is_row_hidden(r) {
            continue;
        }
        let line = Query::line(buf, r as u32);
        let segs = hjkl_buffer::wrap::wrap_segments(line, v.text_width, v.wrap);
        if r == cursor_row {
            let seg_idx = hjkl_buffer::wrap::segment_for_col(&segs, cursor_col);
            return Some(screen + seg_idx);
        }
        screen += segs.len();
    }
    None
}
