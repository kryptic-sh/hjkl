use crate::{Position, Wrap};

/// Where the buffer is scrolled to and how big the visible area is.
///
/// `Viewport` is an **input** to [`crate::Buffer::ensure_cursor_visible`],
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
/// [`crate::Buffer::ensure_cursor_visible`].
///
/// [`Wrap::None`] / [`crate::Wrap::Char`] / [`crate::Wrap::Word`] change
/// which screen-row arithmetic the buffer uses. Switching mid-session is
/// supported but the host must call
/// [`crate::Buffer::ensure_cursor_visible`] afterwards.
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
/// past the over-provisioned band computed by [`over_provisioned_range`].
///
/// The over-provisioned range extends `±viewport_height` rows around
/// the viewport, so a jump of MORE than `viewport_height` rows in either
/// direction guarantees the new viewport sits on un-cached territory —
/// hosts use this signal to decide whether to block briefly on a fresh
/// parse (avoids the un-highlighted flash on `gg` / `G` / `<C-d>` / `:N`).
///
/// Host-agnostic: same shape as [`over_provisioned_range`], pure math.
pub fn is_big_viewport_jump(prev_top: usize, cur_top: usize, viewport_height: usize) -> bool {
    prev_top.abs_diff(cur_top) > viewport_height
}

/// Compute an **over-provisioned** row range for ahead-of-scroll work
/// (syntax highlight, diagnostics gather, etc.): one viewport above +
/// the current viewport + one viewport below, clamped to `[0, line_count)`.
///
/// Host-agnostic: takes only document line counts and viewport extents,
/// no terminal cells or pixels. Future GUI hosts (floem, web, …) call
/// the same function with their own viewport dimensions.
///
/// Returns `(top, height)` satisfying:
/// - `top <= viewport_top`
/// - `top + height >= viewport_top + viewport_height` (when room exists)
/// - `top + height <= line_count` (clamped at the bottom edge)
/// - `height <= viewport_height * 3`
///
/// Why 3×: gives the worker enough margin that a fast scroll within one
/// viewport-height stays inside an already-parsed region. Halving (2×)
/// is too tight in practice; quadrupling adds cost without payoff.
pub fn over_provisioned_range(
    viewport_top: usize,
    viewport_height: usize,
    line_count: usize,
) -> (usize, usize) {
    let top = viewport_top.saturating_sub(viewport_height);
    let max_height = line_count.saturating_sub(top);
    let height = viewport_height.saturating_mul(3).min(max_height);
    (top, height)
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

    // ── over_provisioned_range (host-agnostic ahead-of-scroll helper) ──────

    #[test]
    fn over_provisioned_range_middle_of_buffer() {
        // 1000-line buffer, viewport at row 100, height 30.
        // Expect: top = 100 - 30 = 70, height = 90 (3 viewports).
        let (top, height) = over_provisioned_range(100, 30, 1000);
        assert_eq!(top, 70);
        assert_eq!(height, 90);
    }

    #[test]
    fn over_provisioned_range_top_of_buffer() {
        // Near the top: viewport at row 5, height 30 → top saturates at 0,
        // height tries 90 but the buffer is only 1000 rows from 0 so it can
        // fit the full 90.
        let (top, height) = over_provisioned_range(5, 30, 1000);
        assert_eq!(top, 0);
        assert_eq!(height, 90);
    }

    #[test]
    fn over_provisioned_range_bottom_of_buffer_clamps_height() {
        // 50-line buffer, viewport at row 30, height 30 → top = 0, but
        // height is capped at line_count - top = 50, not 90.
        let (top, height) = over_provisioned_range(30, 30, 50);
        assert_eq!(top, 0);
        assert_eq!(height, 50);
    }

    #[test]
    fn over_provisioned_range_zero_viewport_height() {
        // Defensive: zero-height viewport returns zero-height oversize.
        let (top, height) = over_provisioned_range(10, 0, 100);
        assert_eq!(top, 10);
        assert_eq!(height, 0);
    }

    #[test]
    fn over_provisioned_range_zero_line_count() {
        // Empty buffer — everything zero.
        let (top, height) = over_provisioned_range(0, 30, 0);
        assert_eq!(top, 0);
        assert_eq!(height, 0);
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

    #[test]
    fn over_provisioned_range_covers_viewport() {
        // The over-provisioned range MUST cover the original viewport when
        // there's enough buffer to do so — that's the load-bearing invariant
        // (the renderer paints rows in the original viewport, the cache holds
        // ones above + below).
        let viewport_top = 100;
        let viewport_height = 30;
        let line_count = 1000;
        let (top, height) = over_provisioned_range(viewport_top, viewport_height, line_count);
        assert!(top <= viewport_top, "top must not exceed viewport_top");
        assert!(
            top + height >= viewport_top + viewport_height,
            "oversize range must end at or past the viewport's bottom"
        );
        assert!(
            top + height <= line_count,
            "oversize range must stay inside the buffer"
        );
    }
}
