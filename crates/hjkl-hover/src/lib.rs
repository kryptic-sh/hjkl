//! Renderer-agnostic hover popup state and geometry.
//!
//! Handles `HoverState`, anchor/viewport types, position math (with
//! above/below overflow flip), and keyboard-driven scroll. No rendering
//! types are referenced — the TUI adapter lives in `hjkl-hover-tui`.
//!
//! # Quick start
//!
//! ```rust
//! use hjkl_hover::{HoverAnchor, HoverState, HoverViewport, position};
//! use std::time::Instant;
//!
//! let state = HoverState::new("hello world".to_string(), HoverAnchor::new(5, 3));
//! let vp = HoverViewport::new(80, 24);
//! let rect = position(&state, vp);
//! assert!(rect.x + rect.width <= 80);
//! assert!(rect.y + rect.height <= 24);
//! ```

use std::time::{Duration, Instant};

// ── Public types ──────────────────────────────────────────────────────────────

/// Duration before a stationary hover popup is automatically dismissed.
pub const HOVER_AUTO_FADE: Duration = Duration::from_secs(8);

/// Maximum popup width (content columns + 2 border).
pub const DEFAULT_MAX_WIDTH: u16 = 62;

/// Maximum popup height (content rows + 2 border).
pub const DEFAULT_MAX_HEIGHT: u16 = 14;

/// Cursor cell that the popup is anchored to.
///
/// `#[non_exhaustive]` — new fields may be added in minor releases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct HoverAnchor {
    /// 0-based terminal column (x).
    pub col: u16,
    /// 0-based terminal row (y).
    pub row: u16,
}

impl HoverAnchor {
    /// Convenience constructor.
    pub fn new(col: u16, row: u16) -> Self {
        Self { col, row }
    }
}

/// Available terminal area for popup placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct HoverViewport {
    pub width: u16,
    pub height: u16,
}

impl HoverViewport {
    /// Construct a new `HoverViewport`.
    pub fn new(width: u16, height: u16) -> Self {
        Self { width, height }
    }
}

/// On-screen bounding rect returned by [`position`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct HoverRect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl HoverRect {
    /// Construct a new `HoverRect`.
    pub fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

/// All state needed to display and manage a hover popup.
///
/// `#[non_exhaustive]` — new fields may be added in minor releases.
#[non_exhaustive]
pub struct HoverState {
    /// Raw markdown (or plain-text) hover content as returned by the LSP.
    pub content: String,
    /// Terminal cell the popup is anchored to.
    pub anchor: HoverAnchor,
    /// Whether the popup has been explicitly dismissed.
    pub dismissed: bool,
    /// Scroll offset: number of content lines scrolled past the top.
    pub scroll: usize,
    /// When the popup was first shown — used for auto-fade.
    pub displayed_at: Instant,
    /// Optional hard expiry override (overrides the default 8-second auto-fade).
    pub expiry: Option<Instant>,
    /// Maximum popup width in terminal columns (including border).
    pub max_width: u16,
    /// Maximum popup height in terminal rows (including border).
    pub max_height: u16,
}

impl HoverState {
    /// Create a new popup anchored at `anchor` with default size limits.
    pub fn new(content: String, anchor: HoverAnchor) -> Self {
        Self {
            content,
            anchor,
            dismissed: false,
            scroll: 0,
            displayed_at: Instant::now(),
            expiry: None,
            max_width: DEFAULT_MAX_WIDTH,
            max_height: DEFAULT_MAX_HEIGHT,
        }
    }

    /// Returns `true` when the popup has expired (`now` past the deadline).
    pub fn is_expired(&self, now: Instant) -> bool {
        if self.dismissed {
            return true;
        }
        if let Some(exp) = self.expiry {
            return now >= exp;
        }
        now.duration_since(self.displayed_at) >= HOVER_AUTO_FADE
    }

    /// Scroll the popup by `n` lines (positive = down, negative = up).
    ///
    /// Scroll is clamped at 0; the upper bound is enforced by the renderer
    /// against the rendered line count.
    pub fn scroll_lines(&mut self, n: isize) {
        if n < 0 {
            self.scroll = self.scroll.saturating_sub((-n) as usize);
        } else {
            self.scroll = self.scroll.saturating_add(n as usize);
        }
    }

    /// Number of non-empty content lines (for scroll clamping in the renderer).
    pub fn line_count(&self) -> usize {
        self.content.lines().count().max(1)
    }
}

// ── Position math ─────────────────────────────────────────────────────────────

/// Compute the on-screen bounding rect for a popup.
///
/// Preferred position: one row below the anchor column.
/// If there is not enough vertical room below, flips above the anchor row.
/// Horizontally, shifts left if the popup would overflow the right edge.
/// The result is always clamped to stay fully inside `viewport`.
pub fn position(state: &HoverState, viewport: HoverViewport) -> HoverRect {
    let ax = state.anchor.col;
    let ay = state.anchor.row;

    // Compute content dimensions from raw text. Clamp before the u16 casts:
    // a pathological LSP payload (a 65k-byte line or 65k lines) would
    // otherwise overflow on the `+ 2` below.
    let content_w: u16 = state
        .content
        .lines()
        .map(|l| l.len().min(u16::MAX as usize - 2))
        .max()
        .unwrap_or(8) as u16;
    let content_h: u16 = state
        .content
        .lines()
        .count()
        .clamp(1, u16::MAX as usize - 2) as u16;

    // Total popup size including border (1 cell each side).
    let popup_w = (content_w + 2)
        .min(state.max_width)
        .max(4)
        .min(viewport.width);
    let popup_h = (content_h + 2)
        .min(state.max_height)
        .max(3)
        .min(viewport.height);

    // Horizontal: prefer anchor col, shift left if overflowing right edge.
    let x = if ax.saturating_add(popup_w) <= viewport.width {
        ax
    } else {
        viewport.width.saturating_sub(popup_w)
    };

    // Vertical: prefer one row below; flip above if no room. Clamp so the
    // rect stays inside the viewport even for an out-of-range anchor.
    let below_y = ay.saturating_add(1);
    let y = if below_y.saturating_add(popup_h) <= viewport.height {
        below_y
    } else {
        ay.saturating_sub(popup_h)
            .min(viewport.height.saturating_sub(popup_h))
    };

    HoverRect {
        x,
        y,
        width: popup_w,
        height: popup_h,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn vp(w: u16, h: u16) -> HoverViewport {
        HoverViewport {
            width: w,
            height: h,
        }
    }

    fn state(content: &str, col: u16, row: u16) -> HoverState {
        HoverState::new(content.to_string(), HoverAnchor::new(col, row))
    }

    #[test]
    fn popup_stays_inside_right_edge() {
        let s = state("hello world\nsecond line", 75, 5);
        let r = position(&s, vp(80, 24));
        assert!(
            r.x + r.width <= 80,
            "overflow right: x={} w={} vp=80",
            r.x,
            r.width
        );
    }

    #[test]
    fn popup_flips_above_when_no_vertical_room() {
        let s = state("line1\nline2\nline3", 0, 22);
        let r = position(&s, vp(80, 24));
        assert!(r.y < 22, "should be above anchor row 22, got y={}", r.y);
    }

    #[test]
    fn popup_shows_below_when_room() {
        let s = state("short", 0, 0);
        let r = position(&s, vp(80, 24));
        assert!(r.y > 0, "should be below anchor at row 0");
    }

    #[test]
    fn huge_payload_does_not_panic() {
        // Regression: a 65k-byte line / 65k-line payload used to overflow the
        // `content_w + 2` / `content_h + 2` u16 math in debug builds.
        let long_line = "x".repeat(65_534);
        let s = state(&long_line, 0, 0);
        let r = position(&s, vp(80, 24));
        assert!(r.x + r.width <= 80);
        assert!(r.y + r.height <= 24);

        let many_lines = "y\n".repeat(65_534);
        let s = state(&many_lines, 0, 0);
        let r = position(&s, vp(80, 24));
        assert!(r.y + r.height <= 24);
    }

    #[test]
    fn out_of_range_anchor_is_clamped() {
        // Anchor outside the viewport must not produce a rect past its edges.
        let s = state("line1\nline2\nline3", 200, 200);
        let r = position(&s, vp(80, 24));
        assert!(r.x + r.width <= 80, "x={} w={}", r.x, r.width);
        assert!(r.y + r.height <= 24, "y={} h={}", r.y, r.height);
    }

    #[test]
    fn scroll_clamps_at_zero() {
        let mut s = state("x", 0, 0);
        s.scroll_lines(-10);
        assert_eq!(s.scroll, 0);
    }

    #[test]
    fn scroll_down() {
        let mut s = state("x", 0, 0);
        s.scroll_lines(3);
        assert_eq!(s.scroll, 3);
    }

    #[test]
    fn is_expired_after_explicit_expiry() {
        let mut s = state("x", 0, 0);
        s.expiry = Some(Instant::now() - Duration::from_millis(1));
        assert!(s.is_expired(Instant::now()));
    }

    #[test]
    fn not_expired_fresh() {
        let s = state("x", 0, 0);
        assert!(!s.is_expired(Instant::now()));
    }

    #[test]
    fn dismissed_counts_as_expired() {
        let mut s = state("x", 0, 0);
        s.dismissed = true;
        assert!(s.is_expired(Instant::now()));
    }
}
