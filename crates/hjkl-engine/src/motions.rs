//! Vim-shaped cursor motions, computed over the SPEC trait surface.
//!
//! Patch C (0.0.30) relocated the 24 motion helpers from `hjkl-buffer`
//! into the engine; bodies were concrete over `&mut hjkl_buffer::Buffer`
//! at the time. **0.0.40 (Patch C-δ.5)** lifts every motion fn — and
//! the ten internal helpers — to a `B: Cursor + Query` bound, with
//! fold-aware vertical / screen-vertical motions taking a separate
//! `&dyn FoldProvider` parameter so callers thread their own fold
//! storage through. `Editor` itself remains concrete over
//! `hjkl_buffer::Buffer` until the 0.1.0 freeze patch.
//!
//! Cast plumbing: motion bodies still walk vim's `Position { row, col
//! : usize }` shape internally — converting to/from the engine's
//! grapheme-indexed [`Pos { line: u32, col: u32 }`](crate::types::Pos)
//! at the trait boundary keeps the body diff small and lets the
//! grapheme story land later without re-touching every motion. The
//! cast is a const-time `as`-coercion at the read-cursor / write-cursor
//! / read-line sites.
//!
//! Vertical motions (`move_up` / `move_down` / `move_screen_up` /
//! `move_screen_down`) take a caller-owned `sticky_col` (vim's
//! `curswant`) so bouncing through a shorter row doesn't drag the
//! cursor back to col 0. Word motions (`move_word_*`) take an
//! `iskeyword` spec so the host can change it without re-publishing
//! it onto the buffer. Both have lived as `Editor` fields since 0.0.28.
//!
//! [SPEC.md]: https://github.com/kryptic-sh/hjkl/blob/main/crates/hjkl-engine/SPEC.md

use hjkl_buffer::{Position, Wrap, is_keyword_char, wrap};

use crate::types::{Cursor, FoldProvider, Pos, Query};

// ── Pos ⇄ Position cast helpers ─────────────────────────────────────

/// Read the cursor as a `Position` (the row/col `usize` shape every
/// motion body uses internally). One inline cast at the trait boundary.
#[inline]
fn read_cursor<B: Cursor + ?Sized>(buf: &B) -> Position {
    let p = Cursor::cursor(buf);
    Position::new(p.line as usize, p.col as usize)
}

/// Write a `Position` cursor back through the trait surface.
#[inline]
fn write_cursor<B: Cursor + ?Sized>(buf: &mut B, pos: Position) {
    Cursor::set_cursor(
        buf,
        Pos {
            line: pos.row as u32,
            col: pos.col as u32,
        },
    );
}

/// Borrow line `row`, returning `None` if out of bounds. Mirrors the
/// pre-0.0.40 `Buffer::line(row) -> Option<&str>` shape every motion
/// body uses (the SPEC `Query::line` panics OOB; the bound check
/// keeps motion bodies's `unwrap_or("")` pattern intact).
#[inline]
fn read_line<B: Query + ?Sized>(buf: &B, row: usize) -> Option<&str> {
    let n = Query::line_count(buf) as usize;
    if row >= n {
        return None;
    }
    Some(Query::line(buf, row as u32))
}

/// Number of lines (mirrors pre-0.0.40 `Buffer::row_count() -> usize`).
#[inline]
fn read_row_count<B: Query + ?Sized>(buf: &B) -> usize {
    Query::line_count(buf) as usize
}

// ── Generic helpers ─────────────────────────────────────────────────

/// Returns the char count of `line` — the column you'd see when the
/// cursor is parked one past the end.
fn line_chars(line: &str) -> usize {
    line.chars().count()
}

/// Last valid column for normal-mode motions (`hjkl`, etc.).
/// Empty rows clamp at 0; otherwise it's `chars - 1`.
fn last_col(line: &str) -> usize {
    line_chars(line).saturating_sub(1)
}

/// Pick a target column inside the screen segment `[start, end)` for
/// a `gj` / `gk` step that wants `visual_col` cells from the segment
/// start. Clamps to the segment's last position and to the line's
/// last char so the cursor never lands past the line end.
fn clamp_to_segment(start: usize, end: usize, visual_col: usize, line: &str) -> usize {
    let line_max = last_col(line);
    let seg_max = if end > start { end - 1 } else { start };
    let want = start.saturating_add(visual_col);
    want.min(seg_max).min(line_max).max(start.min(line_max))
}

// ── Horizontal motions ──────────────────────────────────────────────

/// `h` — clamps at column 0; never wraps to the previous line.
pub fn move_left<B: Cursor + Query>(buf: &mut B, count: usize) {
    let cursor = read_cursor(buf);
    let new_col = cursor.col.saturating_sub(count.max(1));
    write_cursor(buf, Position::new(cursor.row, new_col));
}

/// `l` — clamps at the last char on the line. Operator
/// callers wanting "one past end" use [`move_right_to_end`].
pub fn move_right_in_line<B: Cursor + Query>(buf: &mut B, count: usize) {
    let cursor = read_cursor(buf);
    let line = read_line(buf, cursor.row).unwrap_or("");
    let limit = last_col(line);
    let new_col = (cursor.col + count.max(1)).min(limit);
    write_cursor(buf, Position::new(cursor.row, new_col));
}

/// Operator-context `l`: allowed past the last char so a range
/// motion includes it. Clamps at `chars()` (one past end).
pub fn move_right_to_end<B: Cursor + Query>(buf: &mut B, count: usize) {
    let cursor = read_cursor(buf);
    let line = read_line(buf, cursor.row).unwrap_or("");
    let limit = line_chars(line);
    let new_col = (cursor.col + count.max(1)).min(limit);
    write_cursor(buf, Position::new(cursor.row, new_col));
}

/// `0` — first column of the current row.
pub fn move_line_start<B: Cursor + Query>(buf: &mut B) {
    let row = read_cursor(buf).row;
    write_cursor(buf, Position::new(row, 0));
}

/// `^` — first non-blank column. On a blank line it lands on 0.
pub fn move_first_non_blank<B: Cursor + Query>(buf: &mut B) {
    let row = read_cursor(buf).row;
    let col = read_line(buf, row)
        .unwrap_or("")
        .chars()
        .position(|c| !c.is_whitespace())
        .unwrap_or(0);
    write_cursor(buf, Position::new(row, col));
}

/// `$` — last char on the row. Empty rows stay at column 0.
pub fn move_line_end<B: Cursor + Query>(buf: &mut B) {
    let row = read_cursor(buf).row;
    let col = last_col(read_line(buf, row).unwrap_or(""));
    write_cursor(buf, Position::new(row, col));
}

/// `g_` — last non-blank char on the row. Empty / all-blank rows
/// stay at column 0.
pub fn move_last_non_blank<B: Cursor + Query>(buf: &mut B) {
    let row = read_cursor(buf).row;
    let line = read_line(buf, row).unwrap_or("");
    let col = line
        .char_indices()
        .rev()
        .find(|(_, c)| !c.is_whitespace())
        .map(|(byte, _)| line[..byte].chars().count())
        .unwrap_or(0);
    write_cursor(buf, Position::new(row, col));
}

/// `{` — previous blank line above the cursor, or row 0.
pub fn move_paragraph_prev<B: Cursor + Query>(buf: &mut B, count: usize) {
    let mut row = read_cursor(buf).row;
    for _ in 0..count.max(1) {
        if row == 0 {
            break;
        }
        // Step over any contiguous blank rows the cursor sits on
        // so a single press doesn't stick.
        let mut r = row.saturating_sub(1);
        while r > 0 && read_line(buf, r).is_some_and(|l| l.is_empty()) {
            r -= 1;
        }
        while r > 0 && read_line(buf, r).is_some_and(|l| !l.is_empty()) {
            r -= 1;
        }
        row = r;
    }
    write_cursor(buf, Position::new(row, 0));
}

/// `}` — next blank line below the cursor, or last row.
pub fn move_paragraph_next<B: Cursor + Query>(buf: &mut B, count: usize) {
    let last = read_row_count(buf).saturating_sub(1);
    let mut row = read_cursor(buf).row;
    for _ in 0..count.max(1) {
        if row >= last {
            break;
        }
        let mut r = row.saturating_add(1);
        while r < last && read_line(buf, r).is_some_and(|l| l.is_empty()) {
            r += 1;
        }
        while r < last && read_line(buf, r).is_some_and(|l| !l.is_empty()) {
            r += 1;
        }
        row = r;
    }
    write_cursor(buf, Position::new(row, 0));
}

// ── Vertical motions ────────────────────────────────────────────────

/// `k` — `count` rows up. `sticky_col` is read + written by the
/// caller (`Editor::sticky_col` per 0.0.28); pass `&mut None` if
/// the row's current column should bootstrap the sticky value.
/// `folds` drives fold-aware row stepping so closed folds count as
/// one visual line.
pub fn move_up<B: Cursor + Query>(
    buf: &mut B,
    folds: &dyn FoldProvider,
    count: usize,
    sticky_col: &mut Option<usize>,
) {
    move_vertical(buf, folds, -(count.max(1) as isize), sticky_col);
}

/// `j` — `count` rows down. See [`move_up`] for sticky / fold ownership.
pub fn move_down<B: Cursor + Query>(
    buf: &mut B,
    folds: &dyn FoldProvider,
    count: usize,
    sticky_col: &mut Option<usize>,
) {
    move_vertical(buf, folds, count.max(1) as isize, sticky_col);
}

/// `gk` — `count` visual rows up. With `Wrap::None` (or before
/// the host has published `text_width`), falls back to `move_up`
/// so existing tests + non-wrap callers behave unchanged. Under
/// wrap, walks one screen segment at a time, crossing into the
/// previous doc row only after exhausting the current row's
/// segments. `sticky_col` ownership matches [`move_up`].
///
/// 0.0.34 (Patch C-δ.1): viewport now lives on the engine `Host`;
/// callers pass `host.viewport()`.
pub fn move_screen_up<B: Cursor + Query>(
    buf: &mut B,
    folds: &dyn FoldProvider,
    viewport: &hjkl_buffer::Viewport,
    count: usize,
    sticky_col: &mut Option<usize>,
) {
    move_screen_vertical(buf, folds, viewport, -(count.max(1) as isize), sticky_col);
}

/// `gj` — `count` visual rows down. See [`move_screen_up`].
pub fn move_screen_down<B: Cursor + Query>(
    buf: &mut B,
    folds: &dyn FoldProvider,
    viewport: &hjkl_buffer::Viewport,
    count: usize,
    sticky_col: &mut Option<usize>,
) {
    move_screen_vertical(buf, folds, viewport, count.max(1) as isize, sticky_col);
}

/// `gg` — first row, first non-blank.
pub fn move_top<B: Cursor + Query>(buf: &mut B) {
    write_cursor(buf, Position::new(0, 0));
    move_first_non_blank(buf);
}

/// `G` — last row (or `count - 1` when `count > 0`), first non-blank.
/// `count = 0` (the unprefixed form) jumps to the buffer's bottom.
pub fn move_bottom<B: Cursor + Query>(buf: &mut B, count: usize) {
    let last = read_row_count(buf).saturating_sub(1);
    let target = if count == 0 {
        last
    } else {
        (count - 1).min(last)
    };
    write_cursor(buf, Position::new(target, 0));
    move_first_non_blank(buf);
}

// ── Word motions ────────────────────────────────────────────────────

/// `w` / `W` — start of next word. `big = true` treats every
/// non-whitespace run as one word (vim's WORD). `iskeyword` is
/// the live spec from `Editor::settings.iskeyword`; it's caller-
/// supplied since 0.0.28 (was a buffer field before).
pub fn move_word_fwd<B: Cursor + Query>(buf: &mut B, big: bool, count: usize, iskeyword: &str) {
    for _ in 0..count.max(1) {
        let from = read_cursor(buf);
        if let Some(next) = next_word_start(buf, from, big, iskeyword) {
            write_cursor(buf, next);
        } else {
            break;
        }
    }
}

/// `b` / `B` — start of previous word.
pub fn move_word_back<B: Cursor + Query>(buf: &mut B, big: bool, count: usize, iskeyword: &str) {
    for _ in 0..count.max(1) {
        let from = read_cursor(buf);
        if let Some(prev) = prev_word_start(buf, from, big, iskeyword) {
            write_cursor(buf, prev);
        } else {
            break;
        }
    }
}

/// `e` / `E` — end of current/next word.
pub fn move_word_end<B: Cursor + Query>(buf: &mut B, big: bool, count: usize, iskeyword: &str) {
    for _ in 0..count.max(1) {
        let from = read_cursor(buf);
        if let Some(end) = next_word_end(buf, from, big, iskeyword) {
            write_cursor(buf, end);
        } else {
            break;
        }
    }
}

/// `ge` / `gE` — end of previous word. Walks backward until
/// the cursor sits on the last char of a word (the next char
/// is a different kind, or end-of-line).
pub fn move_word_end_back<B: Cursor + Query>(
    buf: &mut B,
    big: bool,
    count: usize,
    iskeyword: &str,
) {
    for _ in 0..count.max(1) {
        let from = read_cursor(buf);
        match prev_word_end(buf, from, big, iskeyword) {
            Some(p) => write_cursor(buf, p),
            None => break,
        }
    }
}

// ── Find / match ────────────────────────────────────────────────────

/// `%` — jump to the matching bracket. Walks the buffer
/// counting nesting depth so nested pairs resolve correctly.
/// Returns `true` when the cursor moved.
pub fn match_bracket<B: Cursor + Query>(buf: &mut B) -> bool {
    let cursor = read_cursor(buf);
    let line = match read_line(buf, cursor.row) {
        Some(l) => l,
        None => return false,
    };
    let ch = match line.chars().nth(cursor.col) {
        Some(c) => c,
        None => return false,
    };
    let (open, close, forward) = match ch {
        '(' => ('(', ')', true),
        ')' => ('(', ')', false),
        '[' => ('[', ']', true),
        ']' => ('[', ']', false),
        '{' => ('{', '}', true),
        '}' => ('{', '}', false),
        '<' => ('<', '>', true),
        '>' => ('<', '>', false),
        _ => return false,
    };
    let mut depth: i32 = 0;
    let row_count = read_row_count(buf);
    if forward {
        let mut r = cursor.row;
        let mut c = cursor.col;
        loop {
            let chars: Vec<char> = read_line(buf, r).unwrap_or("").chars().collect();
            while c < chars.len() {
                let here = chars[c];
                if here == open {
                    depth += 1;
                } else if here == close {
                    depth -= 1;
                    if depth == 0 {
                        write_cursor(buf, Position::new(r, c));
                        return true;
                    }
                }
                c += 1;
            }
            if r + 1 >= row_count {
                return false;
            }
            r += 1;
            c = 0;
        }
    } else {
        let mut r = cursor.row;
        let mut c = cursor.col as isize;
        loop {
            let chars: Vec<char> = read_line(buf, r).unwrap_or("").chars().collect();
            while c >= 0 {
                let here = chars[c as usize];
                if here == close {
                    depth += 1;
                } else if here == open {
                    depth -= 1;
                    if depth == 0 {
                        write_cursor(buf, Position::new(r, c as usize));
                        return true;
                    }
                }
                c -= 1;
            }
            if r == 0 {
                return false;
            }
            r -= 1;
            c = read_line(buf, r).unwrap_or("").chars().count() as isize - 1;
        }
    }
}

/// `f` / `F` / `t` / `T` — find `ch` on the current row.
/// `forward = true` searches right of the cursor; `till = true`
/// stops one cell short of the match (the `t`/`T` semantic).
/// Returns `true` when the cursor moved.
pub fn find_char_on_line<B: Cursor + Query>(
    buf: &mut B,
    ch: char,
    forward: bool,
    till: bool,
) -> bool {
    let cursor = read_cursor(buf);
    let line = match read_line(buf, cursor.row) {
        Some(l) => l,
        None => return false,
    };
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() {
        return false;
    }
    let target_col = if forward {
        chars
            .iter()
            .enumerate()
            .skip(cursor.col + 1)
            .find(|(_, c)| **c == ch)
            .map(|(i, _)| if till { i.saturating_sub(1) } else { i })
    } else {
        (0..cursor.col)
            .rev()
            .find(|&i| chars[i] == ch)
            .map(|i| if till { i + 1 } else { i })
    };
    match target_col {
        Some(col) => {
            write_cursor(buf, Position::new(cursor.row, col));
            true
        }
        None => false,
    }
}

// ── Viewport-relative motions ───────────────────────────────────────

/// `H` — top of the visible viewport plus `offset` rows
/// (0-based; vim uses 1-based count where bare `H` = 0). Lands
/// on the first non-blank of the resolved row.
///
/// 0.0.34 (Patch C-δ.1): viewport reads route through the host.
pub fn move_viewport_top<B: Cursor + Query>(
    buf: &mut B,
    viewport: &hjkl_buffer::Viewport,
    offset: usize,
) {
    let last = read_row_count(buf).saturating_sub(1);
    let target = viewport.top_row.saturating_add(offset).min(last);
    write_cursor(buf, Position::new(target, 0));
    move_first_non_blank(buf);
}

/// `M` — middle row of the visible viewport.
pub fn move_viewport_middle<B: Cursor + Query>(buf: &mut B, viewport: &hjkl_buffer::Viewport) {
    let last = read_row_count(buf).saturating_sub(1);
    let height = viewport.height as usize;
    let visible_bot = viewport
        .top_row
        .saturating_add(height.saturating_sub(1))
        .min(last);
    let mid = viewport.top_row + (visible_bot - viewport.top_row) / 2;
    write_cursor(buf, Position::new(mid, 0));
    move_first_non_blank(buf);
}

/// `L` — bottom of the visible viewport, minus `offset` rows.
pub fn move_viewport_bottom<B: Cursor + Query>(
    buf: &mut B,
    viewport: &hjkl_buffer::Viewport,
    offset: usize,
) {
    let last = read_row_count(buf).saturating_sub(1);
    let height = viewport.height as usize;
    let visible_bot = viewport
        .top_row
        .saturating_add(height.saturating_sub(1))
        .min(last);
    let target = visible_bot.saturating_sub(offset).max(viewport.top_row);
    write_cursor(buf, Position::new(target, 0));
    move_first_non_blank(buf);
}

// ── Internals ───────────────────────────────────────────────────────

fn move_screen_vertical<B: Cursor + Query>(
    buf: &mut B,
    folds: &dyn FoldProvider,
    viewport: &hjkl_buffer::Viewport,
    delta: isize,
    sticky_col: &mut Option<usize>,
) {
    if matches!(viewport.wrap, Wrap::None) || viewport.text_width == 0 {
        move_vertical(buf, folds, delta, sticky_col);
        return;
    }
    // Snapshot the visual col (offset within the current segment)
    // up front so a chain of `gj` / `gk` presses lands at the
    // same visual column even when crossing short visual lines.
    let cursor = read_cursor(buf);
    let line = read_line(buf, cursor.row).unwrap_or("");
    let segs = wrap::wrap_segments(line, viewport.text_width, viewport.wrap);
    let seg_idx = wrap::segment_for_col(&segs, cursor.col);
    let visual_col = cursor.col.saturating_sub(segs[seg_idx].0);
    let down = delta > 0;
    for _ in 0..delta.unsigned_abs() {
        if !step_screen(buf, folds, viewport, down, visual_col) {
            break;
        }
    }
    *sticky_col = Some(read_cursor(buf).col);
}

/// One visual-row step under wrap. Returns false when stepping
/// would leave the buffer (top of buffer for `down=false`,
/// bottom for `down=true`).
fn step_screen<B: Cursor + Query>(
    buf: &mut B,
    folds: &dyn FoldProvider,
    viewport: &hjkl_buffer::Viewport,
    down: bool,
    visual_col: usize,
) -> bool {
    let cursor = read_cursor(buf);
    let line = read_line(buf, cursor.row).unwrap_or("");
    let segs = wrap::wrap_segments(line, viewport.text_width, viewport.wrap);
    let seg_idx = wrap::segment_for_col(&segs, cursor.col);
    let row_count = read_row_count(buf);
    if down {
        if seg_idx + 1 < segs.len() {
            let (s, e) = segs[seg_idx + 1];
            let target = clamp_to_segment(s, e, visual_col, line);
            write_cursor(buf, Position::new(cursor.row, target));
            return true;
        }
        let Some(next_row) = folds.next_visible_row(cursor.row, row_count) else {
            return false;
        };
        let next_line = read_line(buf, next_row).unwrap_or("");
        let next_segs = wrap::wrap_segments(next_line, viewport.text_width, viewport.wrap);
        let (s, e) = next_segs[0];
        let target = clamp_to_segment(s, e, visual_col, next_line);
        write_cursor(buf, Position::new(next_row, target));
        true
    } else {
        if seg_idx > 0 {
            let (s, e) = segs[seg_idx - 1];
            let target = clamp_to_segment(s, e, visual_col, line);
            write_cursor(buf, Position::new(cursor.row, target));
            return true;
        }
        let Some(prev_row) = folds.prev_visible_row(cursor.row) else {
            return false;
        };
        let prev_line = read_line(buf, prev_row).unwrap_or("");
        let prev_segs = wrap::wrap_segments(prev_line, viewport.text_width, viewport.wrap);
        let (s, e) = *prev_segs.last().unwrap_or(&(0, 0));
        let target = clamp_to_segment(s, e, visual_col, prev_line);
        write_cursor(buf, Position::new(prev_row, target));
        true
    }
}

fn move_vertical<B: Cursor + Query>(
    buf: &mut B,
    folds: &dyn FoldProvider,
    delta: isize,
    sticky_col: &mut Option<usize>,
) {
    let cursor = read_cursor(buf);
    let want = sticky_col.unwrap_or(cursor.col);
    // Sticky col only bootstraps from the cursor on the first
    // vertical move; subsequent moves read it back so a short
    // row clamping us to col 3 doesn't lose the desired col 12.
    *sticky_col = Some(want);
    // Walk one visible row at a time so closed folds count as one
    // visual line. Stops at top/bottom of buffer.
    let mut target_row = cursor.row;
    let row_count = read_row_count(buf);
    if delta < 0 {
        for _ in 0..(-delta) as usize {
            match folds.prev_visible_row(target_row) {
                Some(r) => target_row = r,
                None => break,
            }
        }
    } else {
        for _ in 0..delta as usize {
            match folds.next_visible_row(target_row, row_count) {
                Some(r) => target_row = r,
                None => break,
            }
        }
    }
    let line = read_line(buf, target_row).unwrap_or("");
    let max_col = last_col(line);
    let target_col = want.min(max_col);
    write_cursor(buf, Position::new(target_row, target_col));
}

/// True if `c` qualifies as a word character under `spec`.
fn is_word(c: char, spec: &str) -> bool {
    is_keyword_char(c, spec)
}

/// Classify a char into vim's three "word kinds" so transitions
/// between them can drive `w` / `b` / `e`. `Big = true` collapses
/// `Word` and `Punct` into one bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharKind {
    Word,
    Punct,
    Space,
}

fn char_kind(c: char, big: bool, iskeyword: &str) -> CharKind {
    if c.is_whitespace() {
        CharKind::Space
    } else if big || is_word(c, iskeyword) {
        // `Big` collapses Word + Punct into a single non-space bucket
        // so `W` / `B` / `E` skip across punctuation runs.
        CharKind::Word
    } else {
        CharKind::Punct
    }
}

/// Step one position forward, wrapping into the next row.
fn step_forward<B: Query + ?Sized>(buf: &B, pos: Position) -> Option<Position> {
    let line = read_line(buf, pos.row)?;
    let len = line_chars(line);
    if pos.col + 1 < len {
        return Some(Position::new(pos.row, pos.col + 1));
    }
    if pos.row + 1 < read_row_count(buf) {
        return Some(Position::new(pos.row + 1, 0));
    }
    None
}

/// Step one position back, wrapping into the previous row.
fn step_back<B: Query + ?Sized>(buf: &B, pos: Position) -> Option<Position> {
    if pos.col > 0 {
        return Some(Position::new(pos.row, pos.col - 1));
    }
    if pos.row == 0 {
        return None;
    }
    let prev_row = pos.row - 1;
    let prev_len = line_chars(read_line(buf, prev_row).unwrap_or(""));
    Some(Position::new(prev_row, prev_len.saturating_sub(1)))
}

fn char_at<B: Query + ?Sized>(buf: &B, pos: Position) -> Option<char> {
    read_line(buf, pos.row)?.chars().nth(pos.col)
}

fn next_word_start<B: Query + ?Sized>(
    buf: &B,
    from: Position,
    big: bool,
    iskeyword: &str,
) -> Option<Position> {
    let start_kind = char_at(buf, from).map(|c| char_kind(c, big, iskeyword));
    let mut cur = from;
    // Skip the rest of the current word kind. Vim treats line
    // breaks as whitespace separators for `w`, so a row crossing
    // implicitly ends the current word — break and let the
    // skip-space pass handle anything beyond.
    if let Some(kind) = start_kind {
        while char_at(buf, cur).map(|c| char_kind(c, big, iskeyword)) == Some(kind) {
            let prev_row = cur.row;
            match step_forward(buf, cur) {
                Some(next) => {
                    cur = next;
                    if next.row != prev_row {
                        break;
                    }
                }
                None => return Some(end_of_buffer(buf)),
            }
        }
    }
    // Skip whitespace runs (within row + across rows) to land on
    // the next non-space char.
    while char_at(buf, cur).map(|c| char_kind(c, big, iskeyword)) == Some(CharKind::Space) {
        match step_forward(buf, cur) {
            Some(next) => cur = next,
            None => return Some(end_of_buffer(buf)),
        }
    }
    Some(cur)
}

/// One past the last char of the last row — vim's "end of buffer"
/// for operator-context word motions, so `yw` at end-of-line yanks
/// up to and including the last char.
fn end_of_buffer<B: Query + ?Sized>(buf: &B) -> Position {
    let last_row = read_row_count(buf).saturating_sub(1);
    let last_line = read_line(buf, last_row).unwrap_or("");
    Position::new(last_row, line_chars(last_line))
}

fn prev_word_start<B: Query + ?Sized>(
    buf: &B,
    from: Position,
    big: bool,
    iskeyword: &str,
) -> Option<Position> {
    let mut cur = step_back(buf, from)?;
    // Skip whitespace backwards.
    while char_at(buf, cur).map(|c| char_kind(c, big, iskeyword)) == Some(CharKind::Space) {
        cur = step_back(buf, cur)?;
    }
    let target_kind = char_at(buf, cur).map(|c| char_kind(c, big, iskeyword))?;
    // Walk back while the previous char is still the same kind.
    loop {
        let Some(prev) = step_back(buf, cur) else {
            return Some(cur);
        };
        if char_at(buf, prev).map(|c| char_kind(c, big, iskeyword)) == Some(target_kind) {
            cur = prev;
        } else {
            return Some(cur);
        }
    }
}

/// `ge` / `gE` — walk back to the end of the previous word. The
/// stopping rule mirrors `next_word_end`'s definition of "end":
/// non-whitespace position whose next char is a different kind
/// (or end-of-line / end-of-buffer).
fn prev_word_end<B: Query + ?Sized>(
    buf: &B,
    from: Position,
    big: bool,
    iskeyword: &str,
) -> Option<Position> {
    let mut cur = step_back(buf, from)?;
    loop {
        // Skip whitespace; if it spans across a row boundary, the
        // step_back walk handles the row crossing for us.
        if char_at(buf, cur).map(|c| char_kind(c, big, iskeyword)) == Some(CharKind::Space) {
            cur = step_back(buf, cur)?;
            continue;
        }
        let here = char_kind_or_space(buf, cur, big, iskeyword);
        let next = next_char_kind_in_row(buf, cur, big, iskeyword);
        let same = if big {
            here != CharKind::Space && next != CharKind::Space
        } else {
            here == next
        };
        if !same {
            return Some(cur);
        }
        cur = step_back(buf, cur)?;
    }
}

/// Returns the kind of the char at `pos`, treating an out-of-line
/// position as `Space`. Used by `prev_word_end` so the stopping
/// rule matches the original sqeel-vim helper that synthesised an
/// implicit whitespace at end-of-line.
fn char_kind_or_space<B: Query + ?Sized>(
    buf: &B,
    pos: Position,
    big: bool,
    iskeyword: &str,
) -> CharKind {
    char_at(buf, pos)
        .map(|c| char_kind(c, big, iskeyword))
        .unwrap_or(CharKind::Space)
}

/// Kind of the next char on the same row as `pos`. End-of-line
/// counts as `Space` — vim treats line breaks as separators for
/// `e` / `ge` end-of-word detection.
fn next_char_kind_in_row<B: Query + ?Sized>(
    buf: &B,
    pos: Position,
    big: bool,
    iskeyword: &str,
) -> CharKind {
    let line = read_line(buf, pos.row).unwrap_or("");
    let len = line_chars(line);
    if pos.col + 1 < len {
        char_kind_or_space(buf, Position::new(pos.row, pos.col + 1), big, iskeyword)
    } else {
        CharKind::Space
    }
}

fn next_word_end<B: Query + ?Sized>(
    buf: &B,
    from: Position,
    big: bool,
    iskeyword: &str,
) -> Option<Position> {
    // Vim's `e` advances at least one cell, then walks forward
    // until the *next* char is a different kind (or eof).
    let mut cur = step_forward(buf, from)?;
    while char_at(buf, cur).map(|c| char_kind(c, big, iskeyword)) == Some(CharKind::Space) {
        cur = step_forward(buf, cur)?;
    }
    let kind = char_at(buf, cur).map(|c| char_kind(c, big, iskeyword))?;
    loop {
        let Some(next) = step_forward(buf, cur) else {
            return Some(cur);
        };
        if char_at(buf, next).map(|c| char_kind(c, big, iskeyword)) == Some(kind) {
            cur = next;
        } else {
            return Some(cur);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hjkl_buffer::Buffer;

    use crate::buffer_impl::SnapshotFoldProvider;

    /// Default `iskeyword` spec used by tests — matches vim's default
    /// (`@,48-57,_,192-255`) and the engine's `Settings::default()`.
    const ISK: &str = "@,48-57,_,192-255";

    fn at(b: &Buffer) -> Position {
        b.cursor()
    }

    /// Build a [`SnapshotFoldProvider`] from the supplied buffer.
    /// Tests build this once per assertion since the fold list is
    /// tiny — production call sites in `vim.rs` mirror this shape.
    /// Snapshot decouples from the buffer's lifetime so the caller
    /// can re-borrow `&mut buf` for the motion fn.
    fn folds(b: &Buffer) -> SnapshotFoldProvider {
        SnapshotFoldProvider::from_buffer(b)
    }

    #[test]
    fn move_left_clamps_at_zero() {
        let mut b = Buffer::from_str("abcd");
        move_right_in_line(&mut b, 3);
        assert_eq!(at(&b), Position::new(0, 3));
        move_left(&mut b, 10);
        assert_eq!(at(&b), Position::new(0, 0));
    }

    #[test]
    fn move_left_does_not_wrap_to_prev_row() {
        let mut b = Buffer::from_str("abc\ndef");
        let mut sticky = None;
        {
            let f = folds(&b);
            move_down(&mut b, &f, 1, &mut sticky);
        }
        assert_eq!(at(&b).row, 1);
        move_left(&mut b, 99);
        assert_eq!(at(&b), Position::new(1, 0));
    }

    #[test]
    fn move_right_in_line_stops_at_last_char() {
        let mut b = Buffer::from_str("abcd");
        move_right_in_line(&mut b, 99);
        assert_eq!(at(&b), Position::new(0, 3));
    }

    #[test]
    fn move_right_to_end_allows_one_past() {
        let mut b = Buffer::from_str("abcd");
        move_right_to_end(&mut b, 99);
        assert_eq!(at(&b), Position::new(0, 4));
    }

    #[test]
    fn move_line_start_end() {
        let mut b = Buffer::from_str("  hello");
        move_line_end(&mut b);
        assert_eq!(at(&b), Position::new(0, 6));
        move_line_start(&mut b);
        assert_eq!(at(&b), Position::new(0, 0));
        move_first_non_blank(&mut b);
        assert_eq!(at(&b), Position::new(0, 2));
    }

    #[test]
    fn move_line_end_on_empty_row_stays_at_zero() {
        let mut b = Buffer::from_str("");
        move_line_end(&mut b);
        assert_eq!(at(&b), Position::new(0, 0));
    }

    #[test]
    fn move_down_preserves_sticky_col_across_short_row() {
        let mut b = Buffer::from_str("hello world\nhi\nlong line again");
        move_right_in_line(&mut b, 7);
        assert_eq!(at(&b), Position::new(0, 7));
        let mut sticky = None;
        {
            let f = folds(&b);
            move_down(&mut b, &f, 1, &mut sticky);
        }
        assert_eq!(at(&b).row, 1);
        // Short row clamps to col 1 (last char of "hi").
        assert_eq!(at(&b).col, 1);
        {
            let f = folds(&b);
            move_down(&mut b, &f, 1, &mut sticky);
        }
        // Sticky col 7 restored on the longer row.
        assert_eq!(at(&b), Position::new(2, 7));
    }

    #[test]
    fn move_down_skips_closed_fold() {
        let mut b = Buffer::from_str("a\nb\nc\nd\ne");
        b.add_fold(1, 3, true);
        let mut sticky = None;
        // From row 0, `j` should land on row 4 — the fold collapses
        // rows 1..=3 into a single visual line at row 1.
        {
            let f = folds(&b);
            move_down(&mut b, &f, 1, &mut sticky);
        }
        assert_eq!(at(&b).row, 1);
        {
            let f = folds(&b);
            move_down(&mut b, &f, 1, &mut sticky);
        }
        assert_eq!(at(&b).row, 4);
    }

    #[test]
    fn move_up_skips_closed_fold() {
        let mut b = Buffer::from_str("a\nb\nc\nd\ne");
        b.add_fold(1, 3, true);
        b.set_cursor(Position::new(4, 0));
        let mut sticky = None;
        {
            let f = folds(&b);
            move_up(&mut b, &f, 1, &mut sticky);
        }
        assert_eq!(at(&b).row, 1);
        {
            let f = folds(&b);
            move_up(&mut b, &f, 1, &mut sticky);
        }
        assert_eq!(at(&b).row, 0);
    }

    #[test]
    fn open_fold_is_walked_normally() {
        let mut b = Buffer::from_str("a\nb\nc\nd\ne");
        b.add_fold(1, 3, false);
        let mut sticky = None;
        // Open fold: every row is visible, plain row-by-row stepping.
        {
            let f = folds(&b);
            move_down(&mut b, &f, 2, &mut sticky);
        }
        assert_eq!(at(&b).row, 2);
    }

    #[test]
    fn move_top_lands_on_first_non_blank() {
        let mut b = Buffer::from_str("    indented\nrow2");
        let mut sticky = None;
        {
            let f = folds(&b);
            move_down(&mut b, &f, 1, &mut sticky);
        }
        move_top(&mut b);
        assert_eq!(at(&b), Position::new(0, 4));
    }

    #[test]
    fn move_bottom_with_count_jumps_to_line() {
        let mut b = Buffer::from_str("a\n  b\nc\nd");
        move_bottom(&mut b, 2);
        assert_eq!(at(&b), Position::new(1, 2));
    }

    #[test]
    fn move_bottom_zero_jumps_to_last_row() {
        let mut b = Buffer::from_str("a\nb\nc");
        move_bottom(&mut b, 0);
        assert_eq!(at(&b), Position::new(2, 0));
    }

    #[test]
    fn move_word_fwd_skips_whitespace_runs() {
        let mut b = Buffer::from_str("foo bar  baz");
        move_word_fwd(&mut b, false, 1, ISK);
        assert_eq!(at(&b), Position::new(0, 4));
        move_word_fwd(&mut b, false, 1, ISK);
        assert_eq!(at(&b), Position::new(0, 9));
    }

    #[test]
    fn move_word_fwd_separates_word_from_punct_in_small_w() {
        let mut b = Buffer::from_str("foo.bar");
        move_word_fwd(&mut b, false, 1, ISK);
        assert_eq!(at(&b), Position::new(0, 3));
        move_word_fwd(&mut b, false, 1, ISK);
        assert_eq!(at(&b), Position::new(0, 4));
    }

    #[test]
    fn move_word_fwd_big_collapses_word_and_punct() {
        let mut b = Buffer::from_str("foo.bar baz");
        move_word_fwd(&mut b, true, 1, ISK);
        assert_eq!(at(&b), Position::new(0, 8));
    }

    #[test]
    fn move_word_back_lands_on_word_start() {
        let mut b = Buffer::from_str("foo bar baz");
        move_line_end(&mut b);
        assert_eq!(at(&b), Position::new(0, 10));
        move_word_back(&mut b, false, 1, ISK);
        assert_eq!(at(&b), Position::new(0, 8));
        move_word_back(&mut b, false, 2, ISK);
        assert_eq!(at(&b), Position::new(0, 0));
    }

    #[test]
    fn move_word_end_lands_on_last_char() {
        let mut b = Buffer::from_str("foo bar");
        move_word_end(&mut b, false, 1, ISK);
        assert_eq!(at(&b), Position::new(0, 2));
        move_word_end(&mut b, false, 1, ISK);
        assert_eq!(at(&b), Position::new(0, 6));
    }

    #[test]
    fn find_char_forward_lands_on_match() {
        let mut b = Buffer::from_str("foo,bar,baz");
        assert!(find_char_on_line(&mut b, ',', true, false));
        assert_eq!(at(&b), Position::new(0, 3));
        assert!(find_char_on_line(&mut b, ',', true, false));
        assert_eq!(at(&b), Position::new(0, 7));
    }

    #[test]
    fn find_char_till_stops_one_short() {
        let mut b = Buffer::from_str("foo,bar");
        assert!(find_char_on_line(&mut b, ',', true, true));
        assert_eq!(at(&b), Position::new(0, 2));
    }

    #[test]
    fn find_char_backward_lands_on_match() {
        let mut b = Buffer::from_str("foo,bar,baz");
        b.set_cursor(Position::new(0, 10));
        assert!(find_char_on_line(&mut b, ',', false, false));
        assert_eq!(at(&b), Position::new(0, 7));
    }

    #[test]
    fn find_char_no_match_returns_false() {
        let mut b = Buffer::from_str("hello");
        assert!(!find_char_on_line(&mut b, 'z', true, false));
        assert_eq!(at(&b), Position::new(0, 0));
    }

    #[test]
    fn move_viewport_top_with_offset() {
        let mut b = Buffer::from_str("a\nb\nc\nd\ne\nf");
        let v = hjkl_buffer::Viewport {
            top_row: 1,
            height: 4,
            ..Default::default()
        };
        move_viewport_top(&mut b, &v, 2);
        assert_eq!(at(&b), Position::new(3, 0));
    }

    #[test]
    fn move_viewport_middle_picks_center_of_visible() {
        let mut b = Buffer::from_str("a\nb\nc\nd\ne");
        let v = hjkl_buffer::Viewport {
            top_row: 0,
            height: 5,
            ..Default::default()
        };
        move_viewport_middle(&mut b, &v);
        assert_eq!(at(&b), Position::new(2, 0));
    }

    #[test]
    fn move_viewport_bottom_with_offset() {
        let mut b = Buffer::from_str("a\nb\nc\nd\ne");
        let v = hjkl_buffer::Viewport {
            top_row: 0,
            height: 5,
            ..Default::default()
        };
        move_viewport_bottom(&mut b, &v, 1);
        assert_eq!(at(&b), Position::new(3, 0));
    }

    #[test]
    fn move_word_end_back_lands_on_prev_word_end() {
        let mut b = Buffer::from_str("foo bar baz");
        b.set_cursor(Position::new(0, 9));
        move_word_end_back(&mut b, false, 1, ISK);
        assert_eq!(at(&b), Position::new(0, 6));
        move_word_end_back(&mut b, false, 1, ISK);
        assert_eq!(at(&b), Position::new(0, 2));
    }

    #[test]
    fn move_word_end_back_big_skips_punct() {
        let mut b = Buffer::from_str("foo-bar qux");
        b.set_cursor(Position::new(0, 10));
        move_word_end_back(&mut b, true, 1, ISK);
        assert_eq!(at(&b), Position::new(0, 6));
    }

    #[test]
    fn move_word_end_back_crosses_lines() {
        let mut b = Buffer::from_str("abc\ndef");
        b.set_cursor(Position::new(1, 2));
        move_word_end_back(&mut b, false, 1, ISK);
        assert_eq!(at(&b), Position::new(0, 2));
    }

    #[test]
    fn match_bracket_pairs_within_line() {
        let mut b = Buffer::from_str("if (x + y) {");
        b.set_cursor(Position::new(0, 3));
        assert!(match_bracket(&mut b));
        assert_eq!(at(&b), Position::new(0, 9));
        assert!(match_bracket(&mut b));
        assert_eq!(at(&b), Position::new(0, 3));
    }

    #[test]
    fn match_bracket_handles_nesting() {
        let mut b = Buffer::from_str("((x))");
        b.set_cursor(Position::new(0, 0));
        assert!(match_bracket(&mut b));
        assert_eq!(at(&b), Position::new(0, 4));
    }

    #[test]
    fn match_bracket_crosses_lines() {
        let mut b = Buffer::from_str("{\n  x\n}");
        b.set_cursor(Position::new(0, 0));
        assert!(match_bracket(&mut b));
        assert_eq!(at(&b), Position::new(2, 0));
    }

    #[test]
    fn match_bracket_returns_false_off_bracket() {
        let mut b = Buffer::from_str("hello");
        assert!(!match_bracket(&mut b));
    }

    #[test]
    fn motion_count_zero_treated_as_one() {
        let mut b = Buffer::from_str("abcd");
        move_right_in_line(&mut b, 0);
        assert_eq!(at(&b), Position::new(0, 1));
    }

    fn make_wrap_viewport(mode: Wrap, text_width: u16) -> hjkl_buffer::Viewport {
        hjkl_buffer::Viewport {
            top_row: 0,
            top_col: 0,
            width: text_width,
            height: 10,
            wrap: mode,
            text_width,
            tab_width: 0,
        }
    }

    #[test]
    fn screen_down_falls_back_to_move_down_when_wrap_off() {
        let mut b = Buffer::from_str("a\nb\nc");
        let v = hjkl_buffer::Viewport::default();
        let mut sticky = None;
        {
            let f = folds(&b);
            move_screen_down(&mut b, &f, &v, 1, &mut sticky);
        }
        assert_eq!(at(&b), Position::new(1, 0));
        {
            let f = folds(&b);
            move_screen_down(&mut b, &f, &v, 1, &mut sticky);
        }
        assert_eq!(at(&b), Position::new(2, 0));
    }

    #[test]
    fn screen_down_walks_within_wrapped_row() {
        // 12-char line, width 4 → segments (0,4), (4,8), (8,12).
        let mut b = Buffer::from_str("aaaabbbbcccc\nx");
        let v = make_wrap_viewport(Wrap::Char, 4);
        b.set_cursor(Position::new(0, 1));
        let mut sticky = None;
        {
            let f = folds(&b);
            move_screen_down(&mut b, &f, &v, 1, &mut sticky);
        }
        // visual_col = 1 → next segment starts at 4 → land col 5.
        assert_eq!(at(&b), Position::new(0, 5));
        {
            let f = folds(&b);
            move_screen_down(&mut b, &f, &v, 1, &mut sticky);
        }
        assert_eq!(at(&b), Position::new(0, 9));
        // Past the last segment crosses to the next doc row.
        {
            let f = folds(&b);
            move_screen_down(&mut b, &f, &v, 1, &mut sticky);
        }
        assert_eq!(at(&b), Position::new(1, 0));
    }

    #[test]
    fn screen_up_walks_within_wrapped_row() {
        let mut b = Buffer::from_str("aaaabbbbcccc");
        let v = make_wrap_viewport(Wrap::Char, 4);
        b.set_cursor(Position::new(0, 9));
        let mut sticky = None;
        {
            let f = folds(&b);
            move_screen_up(&mut b, &f, &v, 1, &mut sticky);
        }
        // visual_col = 9 - 8 = 1 → previous segment col = 4 + 1 = 5.
        assert_eq!(at(&b), Position::new(0, 5));
        {
            let f = folds(&b);
            move_screen_up(&mut b, &f, &v, 1, &mut sticky);
        }
        assert_eq!(at(&b), Position::new(0, 1));
        // Already on first segment of first row — no further move.
        {
            let f = folds(&b);
            move_screen_up(&mut b, &f, &v, 1, &mut sticky);
        }
        assert_eq!(at(&b), Position::new(0, 1));
    }

    #[test]
    fn screen_down_clamps_to_short_segment() {
        // First row wraps into a 6-char then a 2-char segment; second
        // row is only 1 char. Visual col 4 should clamp to row 1's
        // last col (0) when crossing into the short row.
        let mut b = Buffer::from_str("aaaaaabb\nx");
        let v = make_wrap_viewport(Wrap::Char, 6);
        b.set_cursor(Position::new(0, 4));
        let mut sticky = None;
        {
            let f = folds(&b);
            move_screen_down(&mut b, &f, &v, 1, &mut sticky);
        }
        // visual_col = 4 → segment 1 is (6, 8); want=10 clamps to 7.
        assert_eq!(at(&b), Position::new(0, 7));
        {
            let f = folds(&b);
            move_screen_down(&mut b, &f, &v, 1, &mut sticky);
        }
        // crosses into row 1, segment (0, 1) — clamps to col 0.
        assert_eq!(at(&b), Position::new(1, 0));
    }

    #[test]
    fn screen_down_count_compounds() {
        let mut b = Buffer::from_str("aaaabbbbcccc");
        let v = make_wrap_viewport(Wrap::Char, 4);
        b.set_cursor(Position::new(0, 0));
        let mut sticky = None;
        {
            let f = folds(&b);
            move_screen_down(&mut b, &f, &v, 2, &mut sticky);
        }
        assert_eq!(at(&b), Position::new(0, 8));
    }

    #[test]
    fn motions_module_compiles_against_concrete_buffer() {
        // Compile-time assertion that the engine motions module is
        // physically reachable via the concrete `hjkl_buffer::Buffer`.
        // 0.0.40 (Patch C-δ.5) lifted the bound to `B: Cursor + Query`;
        // the canonical concrete `Buffer` still drives motions.
        let mut b = Buffer::from_str("hello");
        super::move_right_in_line(&mut b, 1);
        assert_eq!(b.cursor(), Position::new(0, 1));
    }

    /// Mock-buffer compile-test: a non-canonical `Cursor + Query` impl
    /// drives motions correctly. Verifies the lift is on the trait
    /// surface — not pinned to `hjkl_buffer::Buffer` — by exercising
    /// every public motion that doesn't need fold or viewport state.
    #[test]
    fn motions_drive_non_canonical_cursor_query_impl() {
        use std::borrow::Cow;

        struct MockBuf {
            lines: Vec<String>,
            cursor: Pos,
        }

        impl crate::types::Cursor for MockBuf {
            fn cursor(&self) -> Pos {
                self.cursor
            }

            fn set_cursor(&mut self, pos: Pos) {
                self.cursor = pos;
            }

            fn byte_offset(&self, _pos: Pos) -> usize {
                0
            }

            fn pos_at_byte(&self, _byte: usize) -> Pos {
                Pos::ORIGIN
            }
        }

        impl crate::types::Query for MockBuf {
            fn line_count(&self) -> u32 {
                self.lines.len() as u32
            }

            fn line(&self, idx: u32) -> &str {
                &self.lines[idx as usize]
            }

            fn len_bytes(&self) -> usize {
                self.lines
                    .iter()
                    .map(|l| l.len() + 1)
                    .sum::<usize>()
                    .saturating_sub(1)
            }

            fn slice(&self, _range: core::ops::Range<Pos>) -> Cow<'_, str> {
                Cow::Borrowed("")
            }
        }

        let mut m = MockBuf {
            lines: vec!["foo bar".into(), "baz qux".into()],
            cursor: Pos::ORIGIN,
        };

        // h/l/0/$
        super::move_right_in_line(&mut m, 2);
        assert_eq!(m.cursor, Pos::new(0, 2));
        super::move_left(&mut m, 1);
        assert_eq!(m.cursor, Pos::new(0, 1));
        super::move_line_end(&mut m);
        assert_eq!(m.cursor, Pos::new(0, 6));
        super::move_line_start(&mut m);
        assert_eq!(m.cursor, Pos::new(0, 0));

        // Word motion via the non-canonical buffer.
        super::move_word_fwd(&mut m, false, 1, ISK);
        assert_eq!(m.cursor, Pos::new(0, 4));

        // gg / G via the non-canonical buffer.
        super::move_bottom(&mut m, 0);
        assert_eq!(m.cursor.line, 1);
        super::move_top(&mut m);
        assert_eq!(m.cursor, Pos::new(0, 0));
    }
}
