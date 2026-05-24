use std::sync::{Arc, Mutex, MutexGuard};

use crate::content::Content;
use crate::{Position, Viewport};

/// Per-window view onto a [`Content`].
///
/// `Buffer` is the type the rest of `hjkl-buffer` — and all consumers —
/// use directly. It owns exactly the state that is local to one editor
/// window:
///
/// - `cursor` — the charwise caret for this window.
///
/// All document-level state (text rows, dirty generation, folds) lives on
/// the inner [`Content`] and is accessed via `Arc<Mutex<Content>>`.
/// Two `Buffer` instances that share the same `Arc` share text + folds
/// but carry independent cursors — the Helix Document+View model.
///
/// ## `Send` + `Sync`
///
/// `Arc<Mutex<Content>>` is `Send + Sync`, so `Buffer` remains `Send`.
/// The engine trait surface requires `Buffer: Send`; this constraint
/// drove the choice of `Mutex` over `RefCell`. The mutex is never
/// contended in normal operation (single-threaded app loop), so the
/// lock cost is negligible (~5 ns uncontested).
///
/// ## 0.8.0 migration notes
///
/// The existing constructors ([`Buffer::new`], [`Buffer::from_str`],
/// [`Buffer::replace_all`], etc.) keep the same external signatures.
/// Callers that do not need multi-window sharing see no behaviour change.
/// Use [`Buffer::new_view`] to create a second window onto the same
/// [`Content`].
///
/// ## Viewport
///
/// The `lines` invariant — at least one entry, never empty — is
/// preserved by every mutation. The viewport itself (top_row, top_col,
/// width, height, wrap, text_width) lives on the engine `Host` adapter;
/// methods that need it take a `&Viewport` / `&mut Viewport` parameter
/// so the rope-walking math stays here while runtime state lives there.
pub struct Buffer {
    /// Shared per-document state (text, dirty gen, folds).
    pub(crate) content: Arc<Mutex<Content>>,
    /// Charwise cursor. `col` is bound by `lines[row].chars().count()`
    /// in normal mode, one past it in operator-pending / insert.
    cursor: Position,
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new()
    }
}

impl Buffer {
    // ── Constructors ──────────────────────────────────────────────

    /// Construct an empty buffer with one empty row + cursor at `(0, 0)`.
    pub fn new() -> Self {
        Self {
            content: Arc::new(Mutex::new(Content::new())),
            cursor: Position::default(),
        }
    }

    /// Build a buffer from a flat string. Splits on `\n`; a trailing
    /// `\n` produces a trailing empty line (matches every text
    /// editor's behaviour and keeps `from_text(buf.as_string())` an
    /// identity round-trip in the common case).
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(text: &str) -> Self {
        Self {
            content: Arc::new(Mutex::new(Content::from_str(text))),
            cursor: Position::default(),
        }
    }

    /// Create a second per-window view onto existing [`Content`].
    ///
    /// The new `Buffer` shares text + folds with every other view on the
    /// same `Arc`. Its cursor starts at `(0, 0)` independently. This is
    /// the primary entry point for split-window features.
    ///
    /// ```rust
    /// # use hjkl_buffer::{Buffer, Content, Position};
    /// # use std::sync::Arc;
    /// # use std::sync::Mutex;
    /// let a = Buffer::from_str("hello\nworld");
    /// let content = a.content_arc();
    /// let mut b = Buffer::new_view(Arc::clone(&content));
    ///
    /// // Cursors are independent.
    /// let mut a = Buffer::new_view(Arc::clone(&content));
    /// a.set_cursor(Position::new(1, 0));
    /// assert_eq!(b.cursor(), Position::new(0, 0));
    /// ```
    pub fn new_view(content: Arc<Mutex<Content>>) -> Self {
        Self {
            content,
            cursor: Position::default(),
        }
    }

    /// Return a clone of the `Arc<Mutex<Content>>` so callers can
    /// create additional views with [`Buffer::new_view`].
    pub fn content_arc(&self) -> Arc<Mutex<Content>> {
        Arc::clone(&self.content)
    }

    // ── Read-only accessors (delegate to Content) ─────────────────

    /// Returns a snapshot of every line as an owned `Vec<String>`.
    ///
    /// Owned rather than `&[String]` because a `Buffer` is a per-window
    /// view onto a shared `Content`; another view could mutate the rope
    /// between when this returns and when the caller reads the slice,
    /// invalidating any borrowed reference.
    pub fn lines(&self) -> Vec<String> {
        self.content_lock().lines.clone()
    }

    /// Returns a clone of the line at `row`, or `None` if out of bounds.
    ///
    /// Owned rather than `Option<&str>` for the same reason as [`Buffer::lines`]:
    /// another view sharing the same `Content` could reallocate the backing `Vec`
    /// between the lock release and the caller's use of the reference.
    pub fn line(&self, row: usize) -> Option<String> {
        self.content_lock().lines.get(row).cloned()
    }

    pub fn cursor(&self) -> Position {
        self.cursor
    }

    pub fn dirty_gen(&self) -> u64 {
        self.content.lock().unwrap().dirty_gen
    }

    /// Number of rows in the buffer. Always `>= 1`.
    pub fn row_count(&self) -> usize {
        self.content.lock().unwrap().lines.len()
    }

    /// Concatenate the rows into a single `String` joined by `\n`.
    pub fn as_string(&self) -> String {
        self.content.lock().unwrap().lines.join("\n")
    }

    // ── Cursor ops ────────────────────────────────────────────────

    /// Set cursor without scrolling. Clamps to valid positions.
    ///
    /// The optional sticky column for `j`/`k` motions is **not** reset
    /// by this call — it survives `set_cursor` intentionally.
    pub fn set_cursor(&mut self, pos: Position) {
        let c = self.content.lock().unwrap();
        let last_row = c.lines.len().saturating_sub(1);
        let row = pos.row.min(last_row);
        let line_chars = c.lines[row].chars().count();
        let col = pos.col.min(line_chars);
        drop(c);
        self.cursor = Position::new(row, col);
    }

    /// Bring the cursor into the visible [`Viewport`], scrolling by the
    /// minimum amount needed.
    pub fn ensure_cursor_visible(&mut self, viewport: &mut Viewport) {
        let cursor = self.cursor;
        let v = *viewport;
        let wrap_active = !matches!(v.wrap, crate::Wrap::None) && v.text_width > 0;
        if !wrap_active {
            viewport.ensure_visible(cursor);
            return;
        }
        if v.height == 0 {
            return;
        }
        // Cursor above the visible region: snap top_row to it.
        if cursor.row < v.top_row {
            viewport.top_row = cursor.row;
            viewport.top_col = 0;
            return;
        }
        let height = v.height as usize;
        // Push top_row forward until cursor lands inside [0, height).
        loop {
            let csr = self.cursor_screen_row_from(viewport, viewport.top_row);
            match csr {
                Some(row) if row < height => break,
                _ => {}
            }
            let next = {
                let c = self.content.lock().unwrap();
                let mut n = viewport.top_row + 1;
                while n <= cursor.row && c.folds.iter().any(|f| f.hides(n)) {
                    n += 1;
                }
                n
            };
            if next > cursor.row {
                viewport.top_row = cursor.row;
                break;
            }
            viewport.top_row = next;
        }
        viewport.top_col = 0;
    }

    /// Cursor's screen row offset (0-based) from `viewport.top_row`.
    pub fn cursor_screen_row(&self, viewport: &Viewport) -> Option<usize> {
        if matches!(viewport.wrap, crate::Wrap::None) || viewport.text_width == 0 {
            return None;
        }
        self.cursor_screen_row_from(viewport, viewport.top_row)
    }

    /// Number of screen rows the doc range `start..=end` occupies.
    pub fn screen_rows_between(&self, viewport: &Viewport, start: usize, end: usize) -> usize {
        if start > end {
            return 0;
        }
        let c = self.content.lock().unwrap();
        let last = c.lines.len().saturating_sub(1);
        let end = end.min(last);
        let v = *viewport;
        let mut total = 0usize;
        for r in start..=end {
            if c.folds.iter().any(|f| f.hides(r)) {
                continue;
            }
            if matches!(v.wrap, crate::Wrap::None) || v.text_width == 0 {
                total += 1;
            } else {
                let line = c.lines.get(r).map(String::as_str).unwrap_or("");
                total += crate::wrap::wrap_segments(line, v.text_width, v.wrap).len();
            }
        }
        total
    }

    /// Earliest `top_row` such that `screen_rows_between(top, last)`
    /// is at least `height`.
    pub fn max_top_for_height(&self, viewport: &Viewport, height: usize) -> usize {
        if height == 0 {
            return 0;
        }
        let c = self.content.lock().unwrap();
        let last = c.lines.len().saturating_sub(1);
        let mut total = 0usize;
        let mut row = last;
        loop {
            if !c.folds.iter().any(|f| f.hides(row)) {
                let v = *viewport;
                total += if matches!(v.wrap, crate::Wrap::None) || v.text_width == 0 {
                    1
                } else {
                    let line = c.lines.get(row).map(String::as_str).unwrap_or("");
                    crate::wrap::wrap_segments(line, v.text_width, v.wrap).len()
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

    /// Clamp `pos` to the buffer's content.
    pub fn clamp_position(&self, pos: Position) -> Position {
        let c = self.content.lock().unwrap();
        let last_row = c.lines.len().saturating_sub(1);
        let row = pos.row.min(last_row);
        let line_chars = c.lines[row].chars().count();
        let col = pos.col.min(line_chars);
        Position::new(row, col)
    }

    /// Replace the buffer's full text in place. Cursor is clamped to
    /// the new content.
    pub fn replace_all(&mut self, text: &str) {
        let new_cursor = {
            let mut c = self.content.lock().unwrap();
            let mut lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
            if lines.is_empty() {
                lines.push(String::new());
            }
            c.lines = lines;
            let last_row = c.lines.len().saturating_sub(1);
            let row = self.cursor.row.min(last_row);
            let line_chars = c.lines[row].chars().count();
            let col = self.cursor.col.min(line_chars);
            c.dirty_gen = c.dirty_gen.wrapping_add(1);
            c.cached_joined = None;
            Position::new(row, col)
        };
        self.cursor = new_cursor;
    }

    // ── Crate-internal accessors (used by folds.rs) ───────────────

    /// Bump the render-cache generation. Crate-internal.
    pub(crate) fn dirty_gen_bump(&mut self) {
        let mut c = self.content.lock().unwrap();
        c.dirty_gen = c.dirty_gen.wrapping_add(1);
        c.cached_joined = None;
    }

    /// Return an `Arc<String>` of the full document joined by `\n`,
    /// cached against `dirty_gen`. Multiple per-tick consumers (syntax
    /// pipeline, LSP notify, git signature, dirty hash) share the
    /// same `Arc` for the same generation — first caller pays the join
    /// cost (one alloc + one lock), the rest are O(1).
    ///
    /// Cache invalidates automatically on every `dirty_gen_bump` and on
    /// `set_content`, so callers never need to manage invalidation.
    pub fn content_joined(&self) -> std::sync::Arc<String> {
        let mut c = self.content.lock().unwrap();
        let dg = c.dirty_gen;
        if let Some((cached_dg, ref s)) = c.cached_joined
            && cached_dg == dg
        {
            return std::sync::Arc::clone(s);
        }
        let joined = std::sync::Arc::new(c.lines.join("\n"));
        c.cached_joined = Some((dg, std::sync::Arc::clone(&joined)));
        joined
    }

    /// Shared access to the folds vec. Crate-internal.
    pub(crate) fn content_lock(&self) -> MutexGuard<'_, Content> {
        self.content.lock().unwrap()
    }

    /// Exclusive access to Content. Crate-internal.
    pub(crate) fn content_lock_mut(&mut self) -> MutexGuard<'_, Content> {
        self.content.lock().unwrap()
    }

    // ── Screen-row helpers (private) ──────────────────────────────

    fn cursor_screen_row_from(&self, viewport: &Viewport, top: usize) -> Option<usize> {
        let cursor = self.cursor;
        if cursor.row < top {
            return None;
        }
        let c = self.content.lock().unwrap();
        let v = *viewport;
        let mut screen = 0usize;
        for r in top..=cursor.row {
            if c.folds.iter().any(|f| f.hides(r)) {
                continue;
            }
            let line = c.lines.get(r).map(String::as_str).unwrap_or("");
            let segs = crate::wrap::wrap_segments(line, v.text_width, v.wrap);
            if r == cursor.row {
                let seg_idx = crate::wrap::segment_for_col(&segs, cursor.col);
                return Some(screen + seg_idx);
            }
            screen += segs.len();
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_has_one_empty_row() {
        let b = Buffer::new();
        assert_eq!(b.row_count(), 1);
        assert_eq!(b.line(0).as_deref(), Some(""));
        assert_eq!(b.cursor(), Position::default());
    }

    #[test]
    fn from_str_splits_on_newline() {
        let b = Buffer::from_str("foo\nbar\nbaz");
        assert_eq!(b.row_count(), 3);
        assert_eq!(b.line(0).as_deref(), Some("foo"));
        assert_eq!(b.line(2).as_deref(), Some("baz"));
    }

    #[test]
    fn from_str_trailing_newline_keeps_empty_row() {
        let b = Buffer::from_str("foo\n");
        assert_eq!(b.row_count(), 2);
        assert_eq!(b.line(1).as_deref(), Some(""));
    }

    #[test]
    fn from_str_empty_input_keeps_one_row() {
        let b = Buffer::from_str("");
        assert_eq!(b.row_count(), 1);
        assert_eq!(b.line(0).as_deref(), Some(""));
    }

    #[test]
    fn as_string_round_trips() {
        let b = Buffer::from_str("a\nb\nc");
        assert_eq!(b.as_string(), "a\nb\nc");
    }

    #[test]
    fn dirty_gen_starts_at_zero() {
        assert_eq!(Buffer::new().dirty_gen(), 0);
    }

    fn vp_wrap(width: u16, height: u16) -> Viewport {
        Viewport {
            top_row: 0,
            top_col: 0,
            width,
            height,
            wrap: crate::Wrap::Char,
            text_width: width,
            tab_width: 0,
        }
    }

    #[test]
    fn ensure_cursor_visible_wrap_scrolls_when_cursor_below_screen() {
        let mut b = Buffer::from_str("aaaaaaaaaa\nb\nc");
        let mut v = vp_wrap(4, 3);
        b.set_cursor(Position::new(2, 0));
        b.ensure_cursor_visible(&mut v);
        assert_eq!(v.top_row, 1);
    }

    #[test]
    fn ensure_cursor_visible_wrap_no_scroll_when_visible() {
        let mut b = Buffer::from_str("aaaaaaaaaa\nb");
        let mut v = vp_wrap(4, 4);
        b.set_cursor(Position::new(0, 5));
        b.ensure_cursor_visible(&mut v);
        assert_eq!(v.top_row, 0);
    }

    #[test]
    fn ensure_cursor_visible_wrap_snaps_top_when_cursor_above() {
        let mut b = Buffer::from_str("a\nb\nc\nd\ne");
        let mut v = vp_wrap(4, 2);
        v.top_row = 3;
        b.set_cursor(Position::new(1, 0));
        b.ensure_cursor_visible(&mut v);
        assert_eq!(v.top_row, 1);
    }

    #[test]
    fn screen_rows_between_sums_segments_under_wrap() {
        let b = Buffer::from_str("aaaaaaaaa\nb\n");
        let v = vp_wrap(4, 0);
        assert_eq!(b.screen_rows_between(&v, 0, 0), 3);
        assert_eq!(b.screen_rows_between(&v, 0, 1), 4);
        assert_eq!(b.screen_rows_between(&v, 0, 2), 5);
        assert_eq!(b.screen_rows_between(&v, 1, 2), 2);
    }

    #[test]
    fn screen_rows_between_one_per_doc_row_when_wrap_off() {
        let b = Buffer::from_str("aaaaa\nb\nc");
        let v = Viewport::default();
        assert_eq!(b.screen_rows_between(&v, 0, 2), 3);
    }

    #[test]
    fn max_top_for_height_walks_back_until_height_reached() {
        let b = Buffer::from_str("a\nb\nc\nd\neeeeeeee");
        let v = vp_wrap(4, 0);
        assert_eq!(b.max_top_for_height(&v, 4), 2);
        assert_eq!(b.max_top_for_height(&v, 99), 0);
    }

    #[test]
    fn cursor_screen_row_returns_none_when_wrap_off() {
        let b = Buffer::from_str("a");
        let v = Viewport::default();
        assert!(b.cursor_screen_row(&v).is_none());
    }

    #[test]
    fn cursor_screen_row_under_wrap() {
        let mut b = Buffer::from_str("aaaaaaaaaa\nb");
        let v = vp_wrap(4, 0);
        b.set_cursor(Position::new(0, 5));
        assert_eq!(b.cursor_screen_row(&v), Some(1));
        b.set_cursor(Position::new(1, 0));
        assert_eq!(b.cursor_screen_row(&v), Some(3));
    }

    #[test]
    fn ensure_cursor_visible_falls_back_when_wrap_disabled() {
        let mut b = Buffer::from_str("a\nb\nc\nd\ne");
        let mut v = Viewport {
            top_row: 0,
            top_col: 0,
            width: 4,
            height: 2,
            wrap: crate::Wrap::None,
            text_width: 4,
            tab_width: 0,
        };
        b.set_cursor(Position::new(4, 0));
        b.ensure_cursor_visible(&mut v);
        assert_eq!(v.top_row, 3);
    }

    // ── View-split tests (new in 0.8.0) ──────────────────────────

    /// Two `Buffer` views sharing one `Content` must have independent
    /// cursors.
    #[test]
    fn buffer_views_independent_cursors() {
        let a = Buffer::from_str("hello\nworld");
        let arc = a.content_arc();
        let mut view_a = Buffer::new_view(Arc::clone(&arc));
        let mut view_b = Buffer::new_view(Arc::clone(&arc));

        view_a.set_cursor(Position::new(1, 3));
        // view_b cursor must remain at (0, 0).
        assert_eq!(view_b.cursor(), Position::new(0, 0));

        view_b.set_cursor(Position::new(0, 2));
        // view_a cursor must remain at (1, 3).
        assert_eq!(view_a.cursor(), Position::new(1, 3));
    }

    /// An edit applied via one view must be visible via the other.
    #[test]
    fn buffer_views_share_content() {
        use crate::edit::Edit;

        let a = Buffer::from_str("foo");
        let arc = a.content_arc();
        let mut view_a = Buffer::new_view(Arc::clone(&arc));
        let view_b = Buffer::new_view(Arc::clone(&arc));

        view_a.apply_edit(Edit::InsertStr {
            at: Position::new(0, 3),
            text: "bar".into(),
        });

        assert_eq!(view_a.line(0).as_deref(), Some("foobar"));
        assert_eq!(view_b.line(0).as_deref(), Some("foobar"));
    }
}
