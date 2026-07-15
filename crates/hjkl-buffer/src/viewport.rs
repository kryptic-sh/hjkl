use crate::{Position, Wrap};

/// Where the buffer is scrolled to and how big the visible area is.
///
/// `Viewport` is an **input** to [`crate::View::ensure_cursor_visible`],
/// not a derived value. The host writes `top_row`, `top_col`, `width`, and
/// `height` per render frame; the buffer clamps the cursor inside the
/// declared area.
///
/// `top_row` and `top_col` are the first visible row / column; `top_col` is
/// a char index, matching [`Position`].
///
/// `wrap` and `text_width` together drive soft-wrap-aware scrolling and
/// motion. `text_width` is the cell width of the text area (i.e. `width`
/// minus any gutter the host renders) so the buffer can compute screen-line
/// splits without duplicating gutter logic.
///
/// `scroll_off` is not a field on `Viewport` itself; the host computes it
/// and adjusts `top_row` before handing the viewport to
/// [`crate::View::ensure_cursor_visible`].
///
/// [`Wrap::None`] / [`crate::Wrap::Char`] / [`crate::Wrap::Word`] change
/// which screen-row arithmetic the buffer uses. Switching mid-session is
/// supported but the host must call
/// [`crate::View::ensure_cursor_visible`] afterwards.
#[derive(Debug, Clone, Copy, Default)]
pub struct Viewport {
    pub top_row: usize,
    pub top_col: usize,
    pub width: u16,
    pub height: u16,
    /// Soft-wrap mode the renderer + scroll math is using. Default
    /// is [`Wrap::None`] (no wrap, horizontal scroll via `top_col`).
    pub wrap: Wrap,
    /// Cell width of the text area (after the host's gutter is
    /// subtracted from the editor area). Used by wrap-aware scroll
    /// and motion code; ignored when `wrap == Wrap::None`. Set to 0
    /// before the first frame; wrap math falls back to no-op then.
    pub text_width: u16,
    /// Cells per `\t` expansion stop. The renderer uses this to align
    /// tab characters; cursor_screen_pos uses it to map char column to
    /// visual column. `0` is treated as the renderer's fallback (4) so
    /// hosts that don't publish a value still render legibly.
    pub tab_width: u16,
}

impl Viewport {
    pub const fn new() -> Self {
        Self {
            top_row: 0,
            top_col: 0,
            width: 0,
            height: 0,
            wrap: Wrap::None,
            text_width: 0,
            tab_width: 0,
        }
    }

    /// Effective tab width — falls back to 4 when `tab_width == 0` so
    /// uninitialized viewports still expand tabs sensibly.
    pub fn effective_tab_width(self) -> usize {
        if self.tab_width == 0 {
            4
        } else {
            self.tab_width as usize
        }
    }

    /// Last document row that's currently on screen (inclusive).
    /// Returns `top_row` when `height == 0` so callers don't have
    /// to special-case the pre-first-draw state.
    pub fn bottom_row(self) -> usize {
        self.top_row
            .saturating_add((self.height as usize).max(1).saturating_sub(1))
    }

    /// True when `pos` lies inside the current viewport rect.
    pub fn contains(self, pos: Position) -> bool {
        let in_rows = pos.row >= self.top_row && pos.row <= self.bottom_row();
        let in_cols = pos.col >= self.top_col
            && pos.col < self.top_col.saturating_add((self.width as usize).max(1));
        in_rows && in_cols
    }

    /// Adjust `top_row` / `top_col` so `pos` is visible, scrolling by
    /// the minimum amount needed. Used after motions and after
    /// content edits that move the cursor.
    pub fn ensure_visible(&mut self, pos: Position) {
        if self.height == 0 || self.width == 0 {
            return;
        }
        let rows = self.height as usize;
        if pos.row < self.top_row {
            self.top_row = pos.row;
        } else if pos.row >= self.top_row + rows {
            self.top_row = pos.row + 1 - rows;
        }
        let cols = self.width as usize;
        if pos.col < self.top_col {
            self.top_col = pos.col;
        } else if pos.col >= self.top_col + cols {
            self.top_col = pos.col + 1 - cols;
        }
    }
}

/// `true` when a viewport scroll from `prev_top` to `cur_top` lands
/// more than `viewport_height` rows away from the previous position —
/// hosts use this signal to decide whether to block briefly on a fresh
/// parse (avoids the un-highlighted flash on `gg` / `G` / `<C-d>` / `:N`).
///
/// Host-agnostic: pure math.
pub fn is_big_viewport_jump(prev_top: usize, cur_top: usize, viewport_height: usize) -> bool {
    prev_top.abs_diff(cur_top) > viewport_height
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp(top_row: usize, height: u16) -> Viewport {
        Viewport {
            top_row,
            top_col: 0,
            width: 80,
            height,
            wrap: Wrap::None,
            text_width: 80,
            tab_width: 0,
        }
    }

    #[test]
    fn contains_inside_window() {
        let v = vp(10, 5);
        assert!(v.contains(Position::new(10, 0)));
        assert!(v.contains(Position::new(14, 79)));
    }

    #[test]
    fn contains_outside_window() {
        let v = vp(10, 5);
        assert!(!v.contains(Position::new(9, 0)));
        assert!(!v.contains(Position::new(15, 0)));
        assert!(!v.contains(Position::new(12, 80)));
    }

    #[test]
    fn ensure_visible_scrolls_down() {
        let mut v = vp(0, 5);
        v.ensure_visible(Position::new(10, 0));
        assert_eq!(v.top_row, 6);
    }

    #[test]
    fn ensure_visible_scrolls_up() {
        let mut v = vp(20, 5);
        v.ensure_visible(Position::new(15, 0));
        assert_eq!(v.top_row, 15);
    }

    #[test]
    fn ensure_visible_no_scroll_when_inside() {
        let mut v = vp(10, 5);
        v.ensure_visible(Position::new(12, 4));
        assert_eq!(v.top_row, 10);
    }

    #[test]
    fn ensure_visible_zero_dim_is_noop() {
        let mut v = Viewport::default();
        v.ensure_visible(Position::new(100, 100));
        assert_eq!(v.top_row, 0);
        assert_eq!(v.top_col, 0);
    }

    #[test]
    fn is_big_viewport_jump_within_one_height_is_not_big() {
        // Scroll within ±1 viewport-height stays in the over-provisioned band.
        assert!(!is_big_viewport_jump(100, 100, 30));
        assert!(!is_big_viewport_jump(100, 130, 30));
        assert!(!is_big_viewport_jump(100, 70, 30));
    }

    #[test]
    fn is_big_viewport_jump_past_one_height_is_big() {
        // gg from row 500 to row 0 — clearly past 30.
        assert!(is_big_viewport_jump(500, 0, 30));
        // G to last row from row 0.
        assert!(is_big_viewport_jump(0, 9999, 30));
        // Exactly one height + 1 row is the boundary (jump > viewport_height).
        assert!(is_big_viewport_jump(0, 31, 30));
        // Exactly viewport_height is NOT a big jump (the row at the band's edge).
        assert!(!is_big_viewport_jump(0, 30, 30));
    }
}
