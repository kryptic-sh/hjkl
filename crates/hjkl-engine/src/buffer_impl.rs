//! Canonical [`Buffer`] trait impl over [`hjkl_buffer::Buffer`].
//!
//! Wires the SPEC trait surface (`Cursor` / `Query` / `BufferEdit` /
//! `Search`, sealed via [`crate::types::sealed::Sealed`]) onto the
//! in-tree rope-backed buffer. Pos⇄Position conversion lives at this
//! boundary — engine code (FSM, editor) keeps using `hjkl_buffer`'s
//! concrete API directly until the motion / fold relocation lands;
//! external trait users see the SPEC surface.
//!
//! See `crates/hjkl-engine/SPEC.md` §"`Buffer` trait surface".
//!
//! # Why concrete-Editor today
//!
//! The trait surface here is 13 methods. The engine FSM today calls
//! ~46 distinct methods on `hjkl_buffer::Buffer` — most of them are
//! motion / fold / viewport helpers that SPEC.md explicitly **excludes**
//! from the trait ("motions don't belong on `Buffer` — they're computed
//! over the buffer, not delegated to it"). Generic-ifying
//! `Editor<B: Buffer, H: Host>` therefore requires relocating those
//! ~33 helpers from `hjkl-buffer` into `hjkl-engine` as free functions
//! over `B: Cursor + Query`. That's a separate, multi-thousand-LOC
//! patch tracked for the 0.1.0 cut.
//!
//! Until then this module ships the canonical impl + a compile-time
//! assertion that `hjkl_buffer::Buffer` satisfies the trait, so
//! downstream callers can write `fn f<B: hjkl_engine::SpecBuffer>(…)`
//! today and the engine's own `Editor` becomes generic over `B` in a
//! follow-up patch without breaking the trait contract.

use std::borrow::Cow;

use hjkl_buffer::Buffer as RopeBuffer;
use hjkl_buffer::Position;
use regex::Regex;

use crate::types::sealed::Sealed;
use crate::types::{Buffer, BufferEdit, Cursor, Pos, Query, Search};

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

/// Buffer [`Position`] → engine [`Pos`].
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
        // Sum byte lengths of every line strictly above `p.row` plus
        // the trailing `\n`, then the col-byte-offset on `p.row`.
        let mut byte = 0usize;
        for r in 0..p.row.min(self.row_count()) {
            byte += self.line(r).map(str::len).unwrap_or(0) + 1; // +1 for '\n'
        }
        if let Some(line) = self.line(p.row) {
            byte += p.byte_offset(line);
        }
        byte
    }

    fn pos_at_byte(&self, byte: usize) -> Pos {
        let mut remaining = byte;
        for r in 0..self.row_count() {
            let line = self.line(r).unwrap_or("");
            let line_bytes = line.len();
            // Each row contributes its bytes plus the trailing `\n`.
            // `byte` indexing the trailing `\n` itself maps to the
            // start of the next row (col 0).
            if remaining <= line_bytes {
                // Convert byte offset within line to char column.
                let col = line[..remaining.min(line_bytes)].chars().count();
                return Pos {
                    line: r as u32,
                    col: col as u32,
                };
            }
            remaining -= line_bytes + 1;
        }
        // Past end → clamp to end of last line.
        let last = self.row_count().saturating_sub(1);
        let line = self.line(last).unwrap_or("");
        Pos {
            line: last as u32,
            col: line.chars().count() as u32,
        }
    }
}

// ── Query ──────────────────────────────────────────────────────────

impl Query for RopeBuffer {
    fn line_count(&self) -> u32 {
        self.row_count() as u32
    }

    fn line(&self, idx: u32) -> &str {
        // SPEC: panic on OOB rather than silently return empty.
        match RopeBuffer::line(self, idx as usize) {
            Some(s) => s,
            None => panic!(
                "Query::line: index {idx} out of bounds (line_count = {})",
                self.row_count()
            ),
        }
    }

    fn len_bytes(&self) -> usize {
        // Sum of every line's bytes + a `\n` between them. Matches
        // `as_string().len()` without allocating the join.
        let n = self.row_count();
        let mut total = 0usize;
        for r in 0..n {
            total += self.line(r).map(str::len).unwrap_or(0);
        }
        // n-1 separators between n lines (no trailing newline).
        total + n.saturating_sub(1)
    }

    fn slice(&self, range: core::ops::Range<Pos>) -> Cow<'_, str> {
        let start = pos_to_position(range.start);
        let end = pos_to_position(range.end);
        if start >= end {
            return Cow::Borrowed("");
        }
        // Single-line slice can borrow.
        if start.row == end.row {
            if let Some(line) = RopeBuffer::line(self, start.row) {
                let lo = start.byte_offset(line).min(line.len());
                let hi = end.byte_offset(line).min(line.len());
                return Cow::Borrowed(&line[lo..hi]);
            }
            return Cow::Borrowed("");
        }
        // Multi-line: allocate.
        let mut out = String::new();
        for r in start.row..=end.row.min(self.row_count().saturating_sub(1)) {
            let line = RopeBuffer::line(self, r).unwrap_or("");
            if r == start.row {
                let lo = start.byte_offset(line).min(line.len());
                out.push_str(&line[lo..]);
                out.push('\n');
            } else if r == end.row {
                let hi = end.byte_offset(line).min(line.len());
                out.push_str(&line[..hi]);
            } else {
                out.push_str(line);
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
        // after, then optionally wrap to rows before. SPEC: "first
        // match at-or-after `from`". Wrap-around honours the buffer's
        // current `wrapscan` setting.
        let wrap = self.search_wraps();
        let from_line = RopeBuffer::line(self, start.row).unwrap_or("");
        let from_byte = start.byte_offset(from_line).min(from_line.len());
        if let Some(m) = pat.find_at(from_line, from_byte) {
            return Some(byte_range_to_pos_range(
                start.row,
                m.start(),
                start.row,
                m.end(),
                from_line,
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
            let line = RopeBuffer::line(self, row).unwrap_or("");
            if let Some(m) = pat.find(line) {
                return Some(byte_range_to_pos_range(row, m.start(), row, m.end(), line));
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
        let wrap = self.search_wraps();
        // Last match at-or-before `from`. We can't run the regex
        // backwards, so iterate matches and pick the last one with
        // start <= from-byte on the from-row, then walk previous rows
        // taking the last match per row.
        let from_line = RopeBuffer::line(self, start.row).unwrap_or("");
        let from_byte = start.byte_offset(from_line).min(from_line.len());
        let mut best: Option<(usize, usize)> = None;
        for m in pat.find_iter(from_line) {
            if m.start() <= from_byte {
                best = Some((m.start(), m.end()));
            } else {
                break;
            }
        }
        if let Some((s, e)) = best {
            return Some(byte_range_to_pos_range(
                start.row, s, start.row, e, from_line,
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
            let line = RopeBuffer::line(self, row).unwrap_or("");
            let last = pat.find_iter(line).last();
            if let Some(m) = last {
                return Some(byte_range_to_pos_range(row, m.start(), row, m.end(), line));
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

// ── Buffer super-trait ─────────────────────────────────────────────

impl Buffer for RopeBuffer {}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-time check: the in-tree `hjkl_buffer::Buffer` satisfies
    /// the SPEC `Buffer` super-trait (and therefore all four sub-traits).
    /// If this stops compiling, the trait surface diverged from the
    /// canonical impl — fix the impl, not this assertion.
    #[test]
    fn rope_buffer_implements_spec_buffer() {
        fn assert_buffer<B: Buffer>() {}
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
        assert!(matches!(s, Cow::Borrowed(_)));
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
        let mut b = RopeBuffer::from_str("foo\nbar\nfoo");
        b.set_search_wrap(true);
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
}
