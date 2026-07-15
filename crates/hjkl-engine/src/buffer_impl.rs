//! Canonical [`View`] trait impl over [`hjkl_buffer::View`].
//!
//! Wires the engine trait surface (`Cursor` / `Query` / `BufferEdit` /
//! `Search`, sealed via [`crate::types::sealed::Sealed`]) onto the
//! in-tree rope-backed buffer. Pos⇄Position conversion lives at this
//! boundary — engine code (FSM, editor) keeps using `hjkl_buffer`'s
//! concrete API directly until the motion / fold relocation lands;
//! external trait users see the engine trait surface.
//!
//! # Why concrete-Editor today
//!
//! The trait surface here is 13 methods. The engine FSM today calls
//! ~46 distinct methods on `hjkl_buffer::View` — most of them are
//! motion / fold / viewport helpers that don't belong on `View`
//! (they're computed over the buffer, not delegated to it). Generic-ifying
//! `Editor<B: View, H: Host>` therefore requires relocating those
//! ~33 helpers from `hjkl-buffer` into `hjkl-engine` as free functions
//! over `B: Cursor + Query`. That's a separate, multi-thousand-LOC
//! patch tracked for the 0.1.0 cut.
//!
//! Until then this module ships the canonical impl + a compile-time
//! assertion that `hjkl_buffer::View` satisfies the trait, so
//! downstream callers can write `fn f<B: hjkl_engine::View>(…)`
//! today and the engine's own `Editor` becomes generic over `B` in a
//! follow-up patch without breaking the trait contract.

use std::borrow::Cow;

use hjkl_buffer::Position;
use hjkl_buffer::View as RopeBuffer;
use regex::Regex;

use crate::types::sealed::Sealed;
use crate::types::{BufferEdit, Cursor, FoldOp, FoldProvider, Pos, Query, Search, View};

// ── Pos ⇄ Position conversion ──────────────────────────────────────

/// Engine [`Pos`] → buffer [`Position`].
///
/// Engine `Pos` is `(line: u32, col: u32)` grapheme-indexed; buffer
/// [`Position`] is `(row: usize, col: usize)` char-indexed. The two
/// indexings happen to match for the in-tree rope today (graphemes
/// without combining marks == chars); future grapheme-aware backends
/// will need to thread a real grapheme→char map through this fn.
#[inline]
pub(crate) fn pos_to_position(p: Pos) -> Position {
    Position {
        row: p.line as usize,
        col: p.col as usize,
    }
}

/// View [`Position`] → engine [`Pos`].
#[inline]
pub(crate) fn position_to_pos(p: Position) -> Pos {
    Pos {
        line: p.row as u32,
        col: p.col as u32,
    }
}

// ── Sealed marker ──────────────────────────────────────────────────

impl Sealed for RopeBuffer {}

// ── Cursor ─────────────────────────────────────────────────────────

impl Cursor for RopeBuffer {
    fn cursor(&self) -> Pos {
        position_to_pos(RopeBuffer::cursor(self))
    }

    fn set_cursor(&mut self, pos: Pos) {
        RopeBuffer::set_cursor(self, pos_to_position(pos));
    }

    fn byte_offset(&self, pos: Pos) -> usize {
        let p = pos_to_position(pos);
        let rope = self.rope();
        let n = rope.len_lines();
        // O(log N) rope index — no per-row String materialization.
        let row = p.row.min(n);
        let line_start = rope.line_to_byte(row);
        if p.row < n {
            // Only the cursor's own line needs to materialize for the
            // col→byte offset translation.
            let line = hjkl_buffer::rope_line_str(&rope, p.row);
            line_start + p.byte_offset(&line)
        } else {
            line_start
        }
    }

    fn pos_at_byte(&self, byte: usize) -> Pos {
        let rope = self.rope();
        let total = rope.len_bytes();
        if total == 0 {
            return Pos { line: 0, col: 0 };
        }
        // Clamp into [0, total]; `byte_to_line` panics on OOB.
        let byte_clamped = byte.min(total);
        // O(log N) rope index — no per-row scan.
        let row = rope.byte_to_line(byte_clamped);
        let line_start = rope.line_to_byte(row);
        let line = hjkl_buffer::rope_line_str(&rope, row);
        let mut col_byte = byte_clamped.saturating_sub(line_start);
        // Clamp to the line body (exclude the trailing `\n` if any).
        col_byte = col_byte.min(line.len());
        // Round down to the nearest char boundary so a byte index
        // landing inside a multi-byte codepoint maps to the column
        // of the char that contains it (not a slice panic).
        while col_byte > 0 && !line.is_char_boundary(col_byte) {
            col_byte -= 1;
        }
        let col = line[..col_byte].chars().count();
        Pos {
            line: row as u32,
            col: col as u32,
        }
    }
}

// ── Query ──────────────────────────────────────────────────────────

impl Query for RopeBuffer {
    fn line_count(&self) -> u32 {
        self.row_count() as u32
    }

    fn line(&self, idx: u32) -> String {
        let row = idx as usize;
        let rope = self.rope();
        // SPEC: panic on OOB rather than silently return empty.
        if row >= rope.len_lines() {
            panic!(
                "Query::line: index {idx} out of bounds (line_count = {})",
                self.row_count()
            );
        }
        hjkl_buffer::rope_line_str(&rope, row)
    }

    fn len_bytes(&self) -> usize {
        // Use cached byte_len — O(1), no allocation.
        RopeBuffer::byte_len(self)
    }

    fn dirty_gen(&self) -> u64 {
        RopeBuffer::dirty_gen(self)
    }

    fn content_joined(&self) -> std::sync::Arc<String> {
        RopeBuffer::content_joined(self)
    }

    fn line_bytes(&self, row: usize) -> usize {
        // One lock, zero allocations via rope API.
        let rope = self.rope();
        hjkl_buffer::rope_line_bytes(&rope, row)
    }

    fn rope(&self) -> ropey::Rope {
        // O(1): Arc-clone of the rope root — no byte copying.
        RopeBuffer::rope(self)
    }

    fn slice(&self, range: core::ops::Range<Pos>) -> Cow<'_, str> {
        let start = pos_to_position(range.start);
        let end = pos_to_position(range.end);
        if start >= end {
            return Cow::Borrowed("");
        }
        let rope = self.rope();
        let n = rope.len_lines();
        // Single-line slice — allocate since View::rope_line_str returns owned String.
        if start.row == end.row {
            if start.row < n {
                let line = hjkl_buffer::rope_line_str(&rope, start.row);
                let lo = start.byte_offset(&line).min(line.len());
                let hi = end.byte_offset(&line).min(line.len());
                return Cow::Owned(line[lo..hi].to_owned());
            }
            return Cow::Borrowed("");
        }
        // Multi-line: allocate.
        let mut out = String::new();
        for r in start.row..=end.row.min(self.row_count().saturating_sub(1)) {
            let line = if r < n {
                hjkl_buffer::rope_line_str(&rope, r)
            } else {
                String::new()
            };
            if r == start.row {
                let lo = start.byte_offset(&line).min(line.len());
                out.push_str(&line[lo..]);
                out.push('\n');
            } else if r == end.row {
                let hi = end.byte_offset(&line).min(line.len());
                out.push_str(&line[..hi]);
            } else {
                out.push_str(&line);
                out.push('\n');
            }
        }
        Cow::Owned(out)
    }
}

// ── BufferEdit ─────────────────────────────────────────────────────

impl BufferEdit for RopeBuffer {
    fn insert_at(&mut self, pos: Pos, text: &str) {
        let at = clamp_to_buf(self, pos_to_position(pos));
        let _ = self.apply_edit(hjkl_buffer::Edit::InsertStr {
            at,
            text: text.to_string(),
        });
    }

    fn delete_range(&mut self, range: core::ops::Range<Pos>) {
        let start = clamp_to_buf(self, pos_to_position(range.start));
        let end = clamp_to_buf(self, pos_to_position(range.end));
        if start >= end {
            return;
        }
        let _ = self.apply_edit(hjkl_buffer::Edit::DeleteRange {
            start,
            end,
            kind: hjkl_buffer::MotionKind::Char,
        });
    }

    fn replace_range(&mut self, range: core::ops::Range<Pos>, replacement: &str) {
        let start = clamp_to_buf(self, pos_to_position(range.start));
        let end = clamp_to_buf(self, pos_to_position(range.end));
        if start >= end {
            // Treat as pure insert at `start`.
            let _ = self.apply_edit(hjkl_buffer::Edit::InsertStr {
                at: start,
                text: replacement.to_string(),
            });
            return;
        }
        let _ = self.apply_edit(hjkl_buffer::Edit::Replace {
            start,
            end,
            with: replacement.to_string(),
        });
    }

    fn replace_all(&mut self, text: &str) {
        // Forward to the inherent in-tree fast path which rebuilds
        // the line vector in one pass + bumps `dirty_gen`.
        RopeBuffer::replace_all(self, text);
    }
}

#[inline]
fn clamp_to_buf(buf: &RopeBuffer, p: Position) -> Position {
    buf.clamp_position(p)
}

// ── Search ─────────────────────────────────────────────────────────

impl Search for RopeBuffer {
    fn find_next(&self, from: Pos, pat: &Regex) -> Option<core::ops::Range<Pos>> {
        let start = pos_to_position(from);
        let total = self.row_count();
        if total == 0 {
            return None;
        }
        // Scan the from-row from `start.col` onward, then every row
        // after, then wrap to rows before. SPEC: "first match
        // at-or-after `from`". 0.0.37: wrap policy now lives on the
        // engine's `SearchState::wrap_around` (see
        // `DESIGN_33_METHOD_CLASSIFICATION.md` step 3); the trait
        // impl always wraps and the engine's `search_*` free
        // functions are responsible for honouring `wrapscan` by
        // wrapping or not invoking the trait at all.
        let wrap = true;
        let rope = self.rope();
        let from_line = hjkl_buffer::rope_line_str(&rope, start.row);
        let from_byte = start.byte_offset(&from_line).min(from_line.len());
        if let Some(m) = pat.find_at(&from_line, from_byte) {
            return Some(byte_range_to_pos_range(
                start.row,
                m.start(),
                start.row,
                m.end(),
                &from_line,
            ));
        }
        for offset in 1..total {
            let row = start.row + offset;
            if row >= total && !wrap {
                break;
            }
            let row = row % total;
            if !wrap && row <= start.row {
                break;
            }
            let line = hjkl_buffer::rope_line_str(&rope, row);
            if let Some(m) = pat.find(&line) {
                return Some(byte_range_to_pos_range(row, m.start(), row, m.end(), &line));
            }
            if row == start.row {
                break;
            }
        }
        None
    }

    fn find_prev(&self, from: Pos, pat: &Regex) -> Option<core::ops::Range<Pos>> {
        let start = pos_to_position(from);
        let total = self.row_count();
        if total == 0 {
            return None;
        }
        // 0.0.37: wrap moved to engine SearchState; trait impl always wraps.
        let wrap = true;
        // Last match at-or-before `from`. We can't run the regex
        // backwards, so iterate matches and pick the last one with
        // start <= from-byte on the from-row, then walk previous rows
        // taking the last match per row.
        let rope = self.rope();
        let from_line = hjkl_buffer::rope_line_str(&rope, start.row);
        let from_byte = start.byte_offset(&from_line).min(from_line.len());
        let mut best: Option<(usize, usize)> = None;
        for m in pat.find_iter(&from_line) {
            if m.start() <= from_byte {
                best = Some((m.start(), m.end()));
            } else {
                break;
            }
        }
        if let Some((s, e)) = best {
            return Some(byte_range_to_pos_range(
                start.row, s, start.row, e, &from_line,
            ));
        }
        for offset in 1..total {
            // Walk backwards.
            let row = if offset > start.row {
                if !wrap {
                    break;
                }
                total - (offset - start.row)
            } else {
                start.row - offset
            };
            if !wrap && row >= start.row {
                break;
            }
            let line = hjkl_buffer::rope_line_str(&rope, row);
            let last = pat.find_iter(&line).last();
            if let Some(m) = last {
                return Some(byte_range_to_pos_range(row, m.start(), row, m.end(), &line));
            }
            if row == start.row {
                break;
            }
        }
        None
    }
}

#[inline]
fn byte_range_to_pos_range(
    s_row: usize,
    s_byte: usize,
    e_row: usize,
    e_byte: usize,
    line: &str,
) -> core::ops::Range<Pos> {
    let s_col = line[..s_byte.min(line.len())].chars().count();
    let e_col = line[..e_byte.min(line.len())].chars().count();
    Pos {
        line: s_row as u32,
        col: s_col as u32,
    }..Pos {
        line: e_row as u32,
        col: e_col as u32,
    }
}

// ── View super-trait ─────────────────────────────────────────────

impl View for RopeBuffer {}

// ── Fold provider ──────────────────────────────────────────────────

/// [`FoldProvider`] adapter wrapping a `&hjkl_buffer::View`. Lets
/// engine call sites ask the buffer's fold storage about visible
/// rows without reaching into `View::next_visible_row` &c. directly.
///
/// Construct with [`BufferFoldProvider::new`]. Hosts that want to
/// expose their own fold model (a separate fold tree, LSP-derived
/// folding ranges, …) can implement `FoldProvider` against their own
/// state and skip this adapter entirely.
///
/// Introduced in 0.0.32 (Patch C-β) as part of the fold-iteration
/// relocation. Fold *storage* still lives on the buffer for
/// `dirty_gen` / render-cache reasons; only the iteration API moved.
pub struct BufferFoldProvider<'a> {
    buffer: &'a RopeBuffer,
}

impl<'a> BufferFoldProvider<'a> {
    pub fn new(buffer: &'a RopeBuffer) -> Self {
        Self { buffer }
    }
}

impl FoldProvider for BufferFoldProvider<'_> {
    fn next_visible_row(&self, row: usize, _row_count: usize) -> Option<usize> {
        // View ignores the row_count hint — it knows its own size.
        RopeBuffer::next_visible_row(self.buffer, row)
    }

    fn prev_visible_row(&self, row: usize) -> Option<usize> {
        RopeBuffer::prev_visible_row(self.buffer, row)
    }

    fn is_row_hidden(&self, row: usize) -> bool {
        RopeBuffer::is_row_hidden(self.buffer, row)
    }

    fn fold_at_row(&self, row: usize) -> Option<(usize, usize, bool)> {
        let f = self.buffer.fold_at_row(row)?;
        Some((f.start_row, f.end_row, f.closed))
    }

    // `apply` / `invalidate_range` use the trait's default no-op impl
    // because `BufferFoldProvider` only borrows the buffer immutably.
    // For fold mutation, use [`BufferFoldProviderMut`] instead.
}

/// Mutable [`FoldProvider`] adapter wrapping a `&mut hjkl_buffer::View`.
/// Engine call sites that need to dispatch a [`FoldOp`] (vim's `z…`
/// keystrokes, the `:fold*` Ex commands, edit-pipeline invalidation)
/// construct this on the fly from `&mut self.buffer` and call
/// [`FoldProvider::apply`] / [`FoldProvider::invalidate_range`] on it.
///
/// Introduced in 0.0.38 (Patch C-δ.4) as part of routing fold mutation
/// through the [`FoldProvider`] surface. Fold *storage* still lives
/// on [`hjkl_buffer::View`] for `dirty_gen` / render-cache reasons;
/// only the dispatch path moved.
pub struct BufferFoldProviderMut<'a> {
    buffer: &'a mut RopeBuffer,
}

impl<'a> BufferFoldProviderMut<'a> {
    pub fn new(buffer: &'a mut RopeBuffer) -> Self {
        Self { buffer }
    }
}

impl FoldProvider for BufferFoldProviderMut<'_> {
    fn next_visible_row(&self, row: usize, _row_count: usize) -> Option<usize> {
        RopeBuffer::next_visible_row(self.buffer, row)
    }

    fn prev_visible_row(&self, row: usize) -> Option<usize> {
        RopeBuffer::prev_visible_row(self.buffer, row)
    }

    fn is_row_hidden(&self, row: usize) -> bool {
        RopeBuffer::is_row_hidden(self.buffer, row)
    }

    fn fold_at_row(&self, row: usize) -> Option<(usize, usize, bool)> {
        let f = self.buffer.fold_at_row(row)?;
        Some((f.start_row, f.end_row, f.closed))
    }

    fn apply(&mut self, op: FoldOp) {
        match op {
            FoldOp::Add {
                start_row,
                end_row,
                closed,
            } => {
                self.buffer.add_fold(start_row, end_row, closed);
            }
            FoldOp::RemoveAt(row) => {
                self.buffer.remove_fold_at(row);
            }
            FoldOp::OpenAt(row) => {
                self.buffer.open_fold_at(row);
            }
            FoldOp::CloseAt(row) => {
                self.buffer.close_fold_at(row);
            }
            FoldOp::ToggleAt(row) => {
                self.buffer.toggle_fold_at(row);
            }
            FoldOp::OpenAll => {
                self.buffer.open_all_folds();
            }
            FoldOp::CloseAll => {
                self.buffer.close_all_folds();
            }
            FoldOp::ClearAll => {
                self.buffer.clear_all_folds();
            }
            FoldOp::Invalidate { start_row, end_row } => {
                self.buffer.invalidate_folds_in_range(start_row, end_row);
            }
            // FoldOp is #[non_exhaustive] — new variants added in hjkl-buffer
            // are silently ignored here until buffer_impl is updated.
            _ => {}
        }
    }

    fn invalidate_range(&mut self, start_row: usize, end_row: usize) {
        self.buffer.invalidate_folds_in_range(start_row, end_row);
    }
}

/// Owned-snapshot [`FoldProvider`] adapter. Carries a copy of the
/// buffer's fold list (one `Vec<Fold>` clone — fold lists are tiny in
/// practice) plus the buffer's `row_count`, so the call site can hold
/// the snapshot for fold queries while passing `&mut hjkl_buffer::View`
/// to a motion function that needs cursor mutation.
///
/// Introduced in 0.0.40 (Patch C-δ.5) so the lifted motion fns can
/// take `&dyn FoldProvider` separately from `&mut B: Cursor + Query`
/// without the call site running into the immutable-vs-mutable
/// borrow conflict that arises with [`BufferFoldProvider`] /
/// [`BufferFoldProviderMut`] (both of which hold a buffer borrow).
///
/// The snapshot is read-only — `apply` and `invalidate_range` are
/// no-ops (any fold mutation must go through the canonical
/// [`BufferFoldProviderMut`] adapter against the live buffer).
pub struct SnapshotFoldProvider {
    folds: Vec<hjkl_buffer::Fold>,
    row_count: usize,
}

impl SnapshotFoldProvider {
    /// Snapshot the current fold list + row-count from `buffer`.
    /// The snapshot is decoupled from the buffer's lifetime, so the
    /// caller can immediately re-borrow the buffer mutably.
    pub fn from_buffer(buffer: &RopeBuffer) -> Self {
        Self {
            folds: buffer.folds().to_vec(),
            row_count: buffer.row_count(),
        }
    }

    /// True iff `row` is hidden by any closed fold in the snapshot.
    /// Mirrors [`hjkl_buffer::View::is_row_hidden`] over the
    /// snapshotted fold list.
    fn snapshot_is_row_hidden(&self, row: usize) -> bool {
        self.folds.iter().any(|f| f.hides(row))
    }
}

impl FoldProvider for SnapshotFoldProvider {
    fn next_visible_row(&self, row: usize, _row_count: usize) -> Option<usize> {
        // Mirrors [`hjkl_buffer::View::next_visible_row`]: walk
        // forward, skipping closed-fold-hidden rows, stop at end.
        let last = self.row_count.saturating_sub(1);
        if last == 0 && row == 0 {
            return None;
        }
        let mut r = row.checked_add(1)?;
        while r <= last && self.snapshot_is_row_hidden(r) {
            r += 1;
        }
        (r <= last).then_some(r)
    }

    fn prev_visible_row(&self, row: usize) -> Option<usize> {
        // Mirrors [`hjkl_buffer::View::prev_visible_row`].
        let mut r = row.checked_sub(1)?;
        while self.snapshot_is_row_hidden(r) {
            r = r.checked_sub(1)?;
        }
        Some(r)
    }

    fn is_row_hidden(&self, row: usize) -> bool {
        self.snapshot_is_row_hidden(row)
    }

    fn fold_at_row(&self, row: usize) -> Option<(usize, usize, bool)> {
        self.folds
            .iter()
            .find(|f| f.contains(row))
            .map(|f| (f.start_row, f.end_row, f.closed))
    }

    // `apply` / `invalidate_range` use the trait's default no-op impl.
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-time check: the in-tree `hjkl_buffer::View` satisfies
    /// the SPEC `View` super-trait (and therefore all four sub-traits).
    /// If this stops compiling, the trait surface diverged from the
    /// canonical impl — fix the impl, not this assertion.
    #[test]
    fn rope_buffer_implements_spec_buffer() {
        fn assert_buffer<B: View>() {}
        fn assert_cursor<B: Cursor>() {}
        fn assert_query<B: Query>() {}
        fn assert_edit<B: BufferEdit>() {}
        fn assert_search<B: Search>() {}
        assert_buffer::<RopeBuffer>();
        assert_cursor::<RopeBuffer>();
        assert_query::<RopeBuffer>();
        assert_edit::<RopeBuffer>();
        assert_search::<RopeBuffer>();
    }

    #[test]
    fn cursor_roundtrip() {
        let mut b = RopeBuffer::from_str("hello\nworld");
        Cursor::set_cursor(&mut b, Pos::new(1, 3));
        assert_eq!(Cursor::cursor(&b), Pos::new(1, 3));
    }

    #[test]
    fn query_line_count_and_line() {
        let b = RopeBuffer::from_str("a\nb\nc");
        assert_eq!(Query::line_count(&b), 3);
        assert_eq!(Query::line(&b, 0), "a");
        assert_eq!(Query::line(&b, 2), "c");
    }

    #[test]
    fn query_len_bytes_matches_join() {
        let b = RopeBuffer::from_str("foo\nbar\nbaz");
        assert_eq!(Query::len_bytes(&b), b.as_string().len());
    }

    #[test]
    fn query_slice_single_line_borrows() {
        let b = RopeBuffer::from_str("hello world");
        let s = Query::slice(&b, Pos::new(0, 0)..Pos::new(0, 5));
        assert_eq!(&*s, "hello");
        // View::line now returns owned String; single-line slice is Owned.
        assert!(matches!(s, Cow::Owned(_)));
    }

    #[test]
    fn query_slice_multiline_allocates() {
        let b = RopeBuffer::from_str("ab\ncd\nef");
        let s = Query::slice(&b, Pos::new(0, 1)..Pos::new(2, 1));
        assert_eq!(&*s, "b\ncd\ne");
        assert!(matches!(s, Cow::Owned(_)));
    }

    #[test]
    fn cursor_byte_offset_and_inverse() {
        let b = RopeBuffer::from_str("hello\nworld");
        // Start of row 1 = 6 bytes ('h','e','l','l','o','\n').
        let p = Pos::new(1, 0);
        assert_eq!(Cursor::byte_offset(&b, p), 6);
        assert_eq!(Cursor::pos_at_byte(&b, 6), p);
        // Roundtrip mid-line.
        let p2 = Pos::new(1, 3);
        let off = Cursor::byte_offset(&b, p2);
        assert_eq!(Cursor::pos_at_byte(&b, off), p2);
    }

    #[test]
    fn buffer_edit_insert_delete_replace() {
        let mut b = RopeBuffer::from_str("hello");
        BufferEdit::insert_at(&mut b, Pos::new(0, 5), " world");
        assert_eq!(b.as_string(), "hello world");
        BufferEdit::delete_range(&mut b, Pos::new(0, 5)..Pos::new(0, 11));
        assert_eq!(b.as_string(), "hello");
        BufferEdit::replace_range(&mut b, Pos::new(0, 0)..Pos::new(0, 5), "HI");
        assert_eq!(b.as_string(), "HI");
    }

    /// Default `BufferEdit::replace_all` impl forwards to
    /// `replace_range(ORIGIN..MAX, text)`. Non-canonical backends that
    /// don't override `replace_all` rely on this; locked in here with
    /// a minimal mock that records the calls.
    #[test]
    fn buffer_edit_default_replace_all_routes_through_replace_range() {
        struct MockBuf {
            cursor: Pos,
            lines: Vec<String>,
            last_replace_range: Option<core::ops::Range<Pos>>,
        }
        impl Sealed for MockBuf {}
        impl Cursor for MockBuf {
            fn cursor(&self) -> Pos {
                self.cursor
            }
            fn set_cursor(&mut self, p: Pos) {
                self.cursor = p;
            }
            fn byte_offset(&self, _p: Pos) -> usize {
                0
            }
            fn pos_at_byte(&self, _b: usize) -> Pos {
                Pos::ORIGIN
            }
        }
        impl Query for MockBuf {
            fn line_count(&self) -> u32 {
                self.lines.len() as u32
            }
            fn line(&self, idx: u32) -> String {
                self.lines[idx as usize].clone()
            }
            fn len_bytes(&self) -> usize {
                0
            }
            fn slice(&self, _r: core::ops::Range<Pos>) -> Cow<'_, str> {
                Cow::Borrowed("")
            }
        }
        impl BufferEdit for MockBuf {
            fn insert_at(&mut self, _p: Pos, _t: &str) {}
            fn delete_range(&mut self, _r: core::ops::Range<Pos>) {}
            fn replace_range(&mut self, range: core::ops::Range<Pos>, _t: &str) {
                self.last_replace_range = Some(range);
            }
        }
        impl Search for MockBuf {
            fn find_next(&self, _f: Pos, _p: &Regex) -> Option<core::ops::Range<Pos>> {
                None
            }
            fn find_prev(&self, _f: Pos, _p: &Regex) -> Option<core::ops::Range<Pos>> {
                None
            }
        }
        impl View for MockBuf {}

        let mut m = MockBuf {
            cursor: Pos::ORIGIN,
            lines: vec!["hi".into()],
            last_replace_range: None,
        };
        BufferEdit::replace_all(&mut m, "new content");
        let r = m
            .last_replace_range
            .expect("default impl must hit replace_range");
        assert_eq!(r.start, Pos::ORIGIN);
        assert_eq!(r.end.line, u32::MAX);
        assert_eq!(r.end.col, u32::MAX);
    }

    #[test]
    fn buffer_edit_replace_all_rebuilds_content() {
        let mut b = RopeBuffer::from_str("hello\nworld");
        Cursor::set_cursor(&mut b, Pos::new(1, 3));
        BufferEdit::replace_all(&mut b, "alpha\nbeta\ngamma");
        assert_eq!(b.as_string(), "alpha\nbeta\ngamma");
        assert_eq!(Query::line_count(&b), 3);
        // Cursor clamped to surviving content (`replace_all` invariant).
        let c = Cursor::cursor(&b);
        assert!((c.line as usize) < Query::line_count(&b) as usize);
    }

    #[test]
    fn search_find_next_same_row() {
        let b = RopeBuffer::from_str("abc def abc");
        let pat = Regex::new("abc").unwrap();
        let r = Search::find_next(&b, Pos::new(0, 0), &pat).unwrap();
        assert_eq!(r, Pos::new(0, 0)..Pos::new(0, 3));
        let r2 = Search::find_next(&b, Pos::new(0, 1), &pat).unwrap();
        assert_eq!(r2, Pos::new(0, 8)..Pos::new(0, 11));
    }

    #[test]
    fn search_find_next_wraps() {
        let b = RopeBuffer::from_str("foo\nbar\nfoo");
        // 0.0.37: wrap policy moved to engine `SearchState::wrap_around`.
        // The trait impl always wraps; engine code that wants
        // non-wrap semantics short-circuits before invoking the trait.
        let pat = Regex::new("foo").unwrap();
        // Starting on row 1: should find row 2's "foo".
        let r = Search::find_next(&b, Pos::new(1, 0), &pat).unwrap();
        assert_eq!(r, Pos::new(2, 0)..Pos::new(2, 3));
    }

    #[test]
    fn search_find_prev_same_row() {
        let b = RopeBuffer::from_str("abc def abc");
        let pat = Regex::new("abc").unwrap();
        let r = Search::find_prev(&b, Pos::new(0, 11), &pat).unwrap();
        assert_eq!(r, Pos::new(0, 8)..Pos::new(0, 11));
    }

    #[test]
    fn pos_position_roundtrip() {
        let p = Pos::new(7, 3);
        assert_eq!(position_to_pos(pos_to_position(p)), p);
    }

    // ── BufferFoldProviderMut dispatch (0.0.38, Patch C-δ.4) ───────

    #[test]
    fn fold_provider_mut_apply_add_open_close_toggle() {
        let mut buf = RopeBuffer::from_str("a\nb\nc\nd\ne");
        {
            let mut p = BufferFoldProviderMut::new(&mut buf);
            p.apply(FoldOp::Add {
                start_row: 1,
                end_row: 3,
                closed: true,
            });
            assert_eq!(p.fold_at_row(2), Some((1, 3, true)));
            p.apply(FoldOp::OpenAt(2));
            assert_eq!(p.fold_at_row(2), Some((1, 3, false)));
            p.apply(FoldOp::CloseAt(2));
            assert_eq!(p.fold_at_row(2), Some((1, 3, true)));
            p.apply(FoldOp::ToggleAt(2));
            assert_eq!(p.fold_at_row(2), Some((1, 3, false)));
        }
        assert_eq!(buf.folds().len(), 1);
    }

    #[test]
    fn fold_provider_mut_apply_open_close_clear_all() {
        let mut buf = RopeBuffer::from_str("a\nb\nc\nd\ne");
        buf.add_fold(0, 1, false);
        buf.add_fold(2, 3, true);
        {
            let mut p = BufferFoldProviderMut::new(&mut buf);
            p.apply(FoldOp::CloseAll);
        }
        assert!(buf.folds().iter().all(|f| f.closed));
        {
            let mut p = BufferFoldProviderMut::new(&mut buf);
            p.apply(FoldOp::OpenAll);
        }
        assert!(buf.folds().iter().all(|f| !f.closed));
        {
            let mut p = BufferFoldProviderMut::new(&mut buf);
            p.apply(FoldOp::ClearAll);
        }
        assert!(buf.folds().is_empty());
    }

    #[test]
    fn fold_provider_mut_invalidate_range_drops_overlapping() {
        let mut buf = RopeBuffer::from_str("a\nb\nc\nd\ne");
        buf.add_fold(0, 1, true);
        buf.add_fold(2, 3, true);
        buf.add_fold(4, 4, true);
        {
            let mut p = BufferFoldProviderMut::new(&mut buf);
            p.invalidate_range(2, 3);
        }
        let starts: Vec<usize> = buf.folds().iter().map(|f| f.start_row).collect();
        assert_eq!(starts, vec![0, 4]);
    }

    #[test]
    fn fold_provider_mut_apply_remove_at() {
        let mut buf = RopeBuffer::from_str("a\nb\nc\nd\ne");
        buf.add_fold(1, 3, true);
        {
            let mut p = BufferFoldProviderMut::new(&mut buf);
            p.apply(FoldOp::RemoveAt(2));
        }
        assert!(buf.folds().is_empty());
    }

    #[test]
    fn noop_fold_provider_apply_is_noop() {
        // The default `apply` impl on the trait is a no-op; verify
        // NoopFoldProvider inherits it without panicking.
        let mut p = crate::types::NoopFoldProvider;
        FoldProvider::apply(&mut p, FoldOp::OpenAll);
        FoldProvider::invalidate_range(&mut p, 0, 5);
        // Read methods unaffected.
        assert!(!FoldProvider::is_row_hidden(&p, 3));
    }
}
