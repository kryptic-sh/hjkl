//! Vim-shaped cursor motions, computed over the SPEC trait surface.
//!
//! Patch C (0.0.30) relocated the 24 motion helpers from `hjkl-buffer`
//! into the engine; bodies were concrete over `&mut hjkl_buffer::View`
//! at the time. **0.0.40 (Patch C-δ.5)** lifts every motion fn — and
//! the ten internal helpers — to a `B: Cursor + Query` bound, with
//! fold-aware vertical / screen-vertical motions taking a separate
//! `&dyn FoldProvider` parameter so callers thread their own fold
//! storage through. `Editor` itself remains concrete over
//! `hjkl_buffer::View` until the 0.1.0 freeze patch.
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
use hjkl_buffer::{
    KeywordSpec, Position, Wrap, char_col_to_visual_col, visual_col_to_char_col, wrap,
};

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
/// pre-0.0.40 `View::line(row) -> Option<&str>` shape every motion
/// body uses (the SPEC `Query::line` panics OOB; the bound check
/// keeps motion bodies's `unwrap_or("")` pattern intact).
#[inline]
fn read_line_opt<B: Query + ?Sized>(buf: &B, row: usize) -> Option<String> {
    let n = Query::line_count(buf) as usize;
    if row >= n {
        return None;
    }
    Some(Query::line(buf, row as u32))
}

/// Read line `row` as an owned `String`, returning an empty `String` for
/// out-of-bounds rows. All motion helpers that used `read_line(…).unwrap_or("")`
/// now call this directly.
fn read_line<B: Query + ?Sized>(buf: &B, row: usize) -> String {
    read_line_opt(buf, row).unwrap_or_default()
}

/// Number of lines (mirrors pre-0.0.40 `View::row_count() -> usize`).
#[inline]
fn read_row_count<B: Query + ?Sized>(buf: &B) -> usize {
    Query::line_count(buf) as usize
}

/// Row count clamped to skip vim's single phantom trailing empty row.
///
/// `ropey`'s `len_lines()` always synthesizes one extra empty final "line"
/// when the buffer text ends in `\n` (vim treats that `\n` as a line
/// *terminator*, not a separator). `G` (`move_bottom`) already accounted for
/// this; every other vertical/viewport motion that clamps against the
/// buffer's row count (`j`/`k`/`H`/`M`/`L`/wrapped-line stepping) must use
/// this instead of raw [`read_row_count`] so they all agree with `G` on
/// where the buffer "ends". A buffer whose *real* last line happens to be
/// empty (e.g. `"foo\n\n"`) is untouched — only a single trailing phantom
/// row is ever skipped, and single-row buffers (`""`, `"\n"`) are left
/// alone entirely (their one row is real, not phantom).
///
/// The emptiness test reads the row's byte length instead of cloning the row
/// into a `String` — `Query::line_bytes(row) == 0` iff `line(row).is_empty()`
/// for every backend (the canonical `View` override reads the rope slice's
/// length under one lock with no allocation), and this runs on every `j`/`k`.
///
/// Also the shared helper for hjkl-ex's `range.rs` and `global.rs` — those
/// consumers must call this rather than re-inline the logic.
pub fn content_row_count<B: Query + ?Sized>(buf: &B) -> usize {
    let raw_count = read_row_count(buf);
    let raw_last = raw_count.saturating_sub(1);
    // `raw_last < raw_count` here, so the row is always in bounds.
    if raw_last > 0 && buf.line_bytes(raw_last) == 0 {
        raw_last
    } else {
        raw_count
    }
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
    let line = read_line(buf, cursor.row);
    let limit = last_col(&line);
    let new_col = (cursor.col + count.max(1)).min(limit);
    write_cursor(buf, Position::new(cursor.row, new_col));
}

/// Operator-context `l`: allowed past the last char so a range
/// motion includes it. Clamps at `chars()` (one past end).
pub fn move_right_to_end<B: Cursor + Query>(buf: &mut B, count: usize) {
    let cursor = read_cursor(buf);
    let line = read_line(buf, cursor.row);
    let limit = line_chars(&line);
    let new_col = (cursor.col + count.max(1)).min(limit);
    write_cursor(buf, Position::new(cursor.row, new_col));
}

/// `<Space>` — vim's right-motion with line wrap (default `whichwrap=b,s`
/// includes `s`): moves right `count` chars, wrapping to the first column of
/// the next line at end-of-line, stopping at the last line. Cursor context
/// only; operator context (`d<Space>`) uses [`move_right_to_end`] so a
/// mid-line `d<Space>` deletes one char like `dl`.
pub fn move_space_fwd<B: Cursor + Query>(buf: &mut B, count: usize) {
    for _ in 0..count.max(1) {
        let cur = read_cursor(buf);
        let line = read_line(buf, cur.row);
        if cur.col < last_col(&line) {
            write_cursor(buf, Position::new(cur.row, cur.col + 1));
        } else if read_line_opt(buf, cur.row + 1).is_some() {
            write_cursor(buf, Position::new(cur.row + 1, 0));
        } else {
            break;
        }
    }
}

/// `<BS>` — vim's left-motion with line wrap (default `whichwrap=b,s` includes
/// `b`): moves left `count` chars, wrapping to the last char of the previous
/// line at beginning-of-line, stopping at the first line. Cursor context only;
/// operator context (`d<BS>`) uses [`move_left`] (mid-line, no wrap).
pub fn move_backspace_back<B: Cursor + Query>(buf: &mut B, count: usize) {
    for _ in 0..count.max(1) {
        let cur = read_cursor(buf);
        if cur.col > 0 {
            write_cursor(buf, Position::new(cur.row, cur.col - 1));
        } else if cur.row > 0 {
            let prev = read_line(buf, cur.row - 1);
            write_cursor(buf, Position::new(cur.row - 1, last_col(&prev)));
        } else {
            break;
        }
    }
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
        .chars()
        .position(|c| !c.is_whitespace())
        .unwrap_or(0);
    write_cursor(buf, Position::new(row, col));
}

/// `$` — last char on the row. Empty rows stay at column 0.
pub fn move_line_end<B: Cursor + Query>(buf: &mut B) {
    let row = read_cursor(buf).row;
    let col = last_col(&read_line(buf, row));
    write_cursor(buf, Position::new(row, col));
}

/// `{count}|` — jump to column `count` on the current line (1-based;
/// count=0 or no count → column 1 → index 0). Clamped to line length.
pub fn move_goto_column<B: Cursor + Query>(buf: &mut B, count: usize) {
    let cursor = read_cursor(buf);
    // count is 1-based in vim: `1|` → col 0, `5|` → col 4.
    // count=0 is treated as 1 (no count given → go to column 1).
    let target_col = if count == 0 { 0 } else { count - 1 };
    let line = read_line(buf, cursor.row);
    let clamped = target_col.min(last_col(&line));
    write_cursor(buf, Position::new(cursor.row, clamped));
}

/// `g_` — last non-blank char on the row. Empty / all-blank rows
/// stay at column 0.
pub fn move_last_non_blank<B: Cursor + Query>(buf: &mut B) {
    let row = read_cursor(buf).row;
    let line = read_line(buf, row);
    let col = line
        .char_indices()
        .rev()
        .find(|(_, c)| !c.is_whitespace())
        .map_or(0, |(byte, _)| line[..byte].chars().count());
    write_cursor(buf, Position::new(row, col));
}

/// Vim's `findpar()` row scan, shared by `{` and `}` (`:h paragraph`).
///
/// Starting on the cursor's row, walk in `dir` until a paragraph boundary
/// (an empty line) is reached, having passed at least one non-empty line
/// on the way — so a boundary is never reported for the row the scan
/// starts on, and a blank run the cursor already sits in is walked out of
/// rather than counted as the boundary. Only truly empty lines are
/// boundaries; a whitespace-only line is ordinary paragraph text, which is
/// what vim's `startPS` does with no `'paragraphs'` macros in play.
///
/// Returns `None` when a *counted* motion runs off the buffer with
/// repetitions still owed — vim's `findpar` returns `FAIL` there and the
/// cursor does not move at all. A final repetition that runs off the end
/// clamps to the edge row instead.
fn find_paragraph<B: Query + ?Sized>(
    buf: &B,
    start_row: usize,
    forward: bool,
    count: usize,
) -> Option<usize> {
    // `content_row_count`, not `read_row_count`: a trailing `\n` synthesizes a
    // phantom final row that vim does not have, and `}` must stop on the last
    // *content* row like `G`/`j` do.
    let last = content_row_count(buf).saturating_sub(1) as isize;
    let dir: isize = if forward { 1 } else { -1 };
    let mut curr = start_row as isize;
    let mut remaining = count.max(1);
    while remaining > 0 {
        remaining -= 1;
        // `did_skip`: a non-empty line has been seen this repetition.
        let mut did_skip = false;
        let mut first = true;
        loop {
            let empty = buf.line_bytes(curr as usize) == 0;
            if !empty {
                did_skip = true;
            }
            if !first && did_skip && empty {
                break;
            }
            curr += dir;
            first = false;
            if curr < 0 || curr > last {
                if remaining > 0 {
                    return None;
                }
                curr -= dir;
                break;
            }
        }
    }
    Some(curr as usize)
}

/// `{` — previous paragraph boundary (empty line) above the cursor, or row 0.
///
/// A run of consecutive blank lines counts as a single paragraph boundary
/// (matching vim/neovim), so `{` steps over the blank run the cursor sits in
/// before scanning up through the preceding paragraph to the next boundary.
pub fn move_paragraph_prev<B: Cursor + Query>(buf: &mut B, count: usize) {
    let cursor = read_cursor(buf);
    let Some(row) = find_paragraph(buf, cursor.row, false, count) else {
        return;
    };
    write_cursor(buf, Position::new(row, 0));
}

/// `}` — next paragraph boundary (empty line) below the cursor, or last row.
///
/// A run of consecutive blank lines counts as a single paragraph boundary
/// (matching vim/neovim), so `}` steps over the blank run the cursor sits in
/// before scanning down through the following paragraph to the next boundary.
///
/// Landing on the **last row** of the buffer puts the cursor on that row's
/// last character rather than column 0 (vim's `findpar` tail: "put the
/// cursor on the last character in the last line"), so `}` in the final
/// paragraph reaches end-of-buffer. An empty last row keeps column 0.
pub fn move_paragraph_next<B: Cursor + Query>(buf: &mut B, count: usize) {
    let cursor = read_cursor(buf);
    let Some(row) = find_paragraph(buf, cursor.row, true, count) else {
        return;
    };
    let last = content_row_count(buf).saturating_sub(1);
    let col = if row == last {
        line_chars(&read_line(buf, row)).saturating_sub(1)
    } else {
        0
    };
    write_cursor(buf, Position::new(row, col));
}

/// `[[` — backward to the previous `{` at column 0 (C section header).
/// Searches from the line above the cursor; clamps at row 0. Charwise exclusive.
pub fn move_section_backward<B: Cursor + Query>(buf: &mut B, count: usize) {
    let cursor = read_cursor(buf);
    let mut row = cursor.row;
    let mut found = 0;
    let rope = Query::rope(buf);
    while found < count.max(1) {
        if row == 0 {
            break;
        }
        row -= 1;
        if crate::viewport_math::rope_line_slice(&rope, row).starts_with('{') {
            found += 1;
        }
    }
    write_cursor(buf, Position::new(row, 0));
}

/// `]]` — forward to the next `{` at column 0 (C section header).
/// Searches from the line below the cursor; clamps at last row. Charwise exclusive.
pub fn move_section_forward<B: Cursor + Query>(buf: &mut B, count: usize) {
    let cursor = read_cursor(buf);
    let last = read_row_count(buf).saturating_sub(1);
    let mut row = cursor.row;
    let mut found = 0;
    let rope = Query::rope(buf);
    while found < count.max(1) {
        if row >= last {
            break;
        }
        row += 1;
        if crate::viewport_math::rope_line_slice(&rope, row).starts_with('{') {
            found += 1;
        }
    }
    write_cursor(buf, Position::new(row, 0));
}

/// `[]` — backward to the previous `}` at column 0 (C section end).
/// Searches from the line above the cursor; clamps at row 0. Charwise exclusive.
pub fn move_section_end_backward<B: Cursor + Query>(buf: &mut B, count: usize) {
    let cursor = read_cursor(buf);
    let mut row = cursor.row;
    let mut found = 0;
    let rope = Query::rope(buf);
    while found < count.max(1) {
        if row == 0 {
            break;
        }
        row -= 1;
        if crate::viewport_math::rope_line_slice(&rope, row).starts_with('}') {
            found += 1;
        }
    }
    write_cursor(buf, Position::new(row, 0));
}

/// `][` — forward to the next `}` at column 0 (C section end).
/// Searches from the line below the cursor; clamps at last row. Charwise exclusive.
pub fn move_section_end_forward<B: Cursor + Query>(buf: &mut B, count: usize) {
    let cursor = read_cursor(buf);
    let last = read_row_count(buf).saturating_sub(1);
    let mut row = cursor.row;
    let mut found = 0;
    let rope = Query::rope(buf);
    while found < count.max(1) {
        if row >= last {
            break;
        }
        row += 1;
        if crate::viewport_math::rope_line_slice(&rope, row).starts_with('}') {
            found += 1;
        }
    }
    write_cursor(buf, Position::new(row, 0));
}

/// `+` / `<CR>` — first non-blank of the next line (`count` lines down).
/// Linewise motion. On the last line, stays on the current line's first non-blank.
pub fn move_first_non_blank_next_line<B: Cursor + Query>(buf: &mut B, count: usize) {
    let cursor = read_cursor(buf);
    let last = read_row_count(buf).saturating_sub(1);
    let desired = cursor.row + count.max(1);
    // Vim: `+` on the last line is a no-op (no next line to move to).
    if desired > last {
        return;
    }
    write_cursor(buf, Position::new(desired, 0));
    move_first_non_blank(buf);
}

/// `-` — first non-blank of the previous line (`count` lines up).
/// Linewise motion. On the first line, the motion is a no-op (vim behavior).
pub fn move_first_non_blank_prev_line<B: Cursor + Query>(buf: &mut B, count: usize) {
    let cursor = read_cursor(buf);
    let steps = count.max(1);
    // Vim: `-` on the first line is a no-op (no previous line to move to).
    if cursor.row < steps {
        return;
    }
    write_cursor(buf, Position::new(cursor.row - steps, 0));
    move_first_non_blank(buf);
}

/// `_` — first non-blank of the current line, or `count-1` lines down.
/// Vim's `_` jumps to first-non-blank of `count-1` lines below (count=1 = current).
/// Linewise motion.
pub fn move_first_non_blank_line<B: Cursor + Query>(buf: &mut B, count: usize) {
    let cursor = read_cursor(buf);
    let last = read_row_count(buf).saturating_sub(1);
    let offset = count.max(1).saturating_sub(1);
    let desired = cursor.row + offset;
    // Vim: `_` with a count that overshoots the buffer is a no-op.
    if desired > last {
        return;
    }
    write_cursor(buf, Position::new(desired, 0));
    move_first_non_blank(buf);
}

// ── Vertical motions ────────────────────────────────────────────────

/// `k` — `count` rows up. `sticky_col` is read + written by the
/// caller (`Editor::sticky_col` per 0.0.28); pass `&mut None` if
/// the row's current column should bootstrap the sticky value.
/// `folds` drives fold-aware row stepping so closed folds count as
/// one visual line. `tabstop` drives display-column ↔ char-index
/// conversion so `j`/`k` preserve the virtual column across tabs.
pub fn move_up<B: Cursor + Query>(
    buf: &mut B,
    folds: &dyn FoldProvider,
    count: usize,
    sticky_col: &mut Option<usize>,
    tabstop: usize,
) {
    move_vertical(buf, folds, -(count.max(1) as isize), sticky_col, tabstop);
}

/// `j` — `count` rows down. See [`move_up`] for sticky / fold ownership.
pub fn move_down<B: Cursor + Query>(
    buf: &mut B,
    folds: &dyn FoldProvider,
    count: usize,
    sticky_col: &mut Option<usize>,
    tabstop: usize,
) {
    move_vertical(buf, folds, count.max(1) as isize, sticky_col, tabstop);
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
    tabstop: usize,
) {
    move_screen_vertical(
        buf,
        folds,
        viewport,
        -(count.max(1) as isize),
        sticky_col,
        tabstop,
    );
}

/// `gj` — `count` visual rows down. See [`move_screen_up`].
pub fn move_screen_down<B: Cursor + Query>(
    buf: &mut B,
    folds: &dyn FoldProvider,
    viewport: &hjkl_buffer::Viewport,
    count: usize,
    sticky_col: &mut Option<usize>,
    tabstop: usize,
) {
    move_screen_vertical(
        buf,
        folds,
        viewport,
        count.max(1) as isize,
        sticky_col,
        tabstop,
    );
}

/// `gg` — first row, first non-blank.
pub fn move_top<B: Cursor + Query>(buf: &mut B) {
    write_cursor(buf, Position::new(0, 0));
    move_first_non_blank(buf);
}

/// `G` — last row (or `count - 1` when `count > 0`), first non-blank.
/// `count = 0` (the unprefixed form) jumps to the buffer's bottom.
///
/// Vim treats a trailing `\n` as a line terminator rather than a
/// separator, so `"foo\nbar\nbaz\n"` is a 3-line file. The engine
/// stores it as `["foo", "bar", "baz", ""]` (4 rows). For bare `G` we
/// skip the trailing empty row so the cursor lands on the last
/// content-bearing line, matching vim's behaviour.
pub fn move_bottom<B: Cursor + Query>(buf: &mut B, count: usize) {
    // Compute the last *content* row: skip a single trailing empty row
    // produced by a trailing `\n`. Applies to both the bare-G case
    // (count == 0) and the counted case (`100G`) so they stay in sync.
    // Pre-0.5.8, counted `G` used `(count-1).min(raw_last)` without the
    // clamp, landing on the phantom row after a trailing newline.
    let last_content = content_row_count(buf).saturating_sub(1);
    let target = if count == 0 {
        last_content
    } else {
        (count - 1).min(last_content)
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
    let spec = KeywordSpec::parse(iskeyword);
    let mut cache = LineCache::default();
    for _ in 0..count.max(1) {
        let from = read_cursor(buf);
        if let Some(next) = next_word_start(buf, &mut cache, from, big, &spec) {
            write_cursor(buf, next);
        } else {
            break;
        }
    }
}

/// `b` / `B` — start of previous word.
pub fn move_word_back<B: Cursor + Query>(buf: &mut B, big: bool, count: usize, iskeyword: &str) {
    let spec = KeywordSpec::parse(iskeyword);
    let mut cache = LineCache::default();
    for _ in 0..count.max(1) {
        let from = read_cursor(buf);
        if let Some(prev) = prev_word_start(buf, &mut cache, from, big, &spec) {
            write_cursor(buf, prev);
        } else {
            break;
        }
    }
}

/// `e` / `E` — end of current/next word.
pub fn move_word_end<B: Cursor + Query>(buf: &mut B, big: bool, count: usize, iskeyword: &str) {
    let spec = KeywordSpec::parse(iskeyword);
    let mut cache = LineCache::default();
    for _ in 0..count.max(1) {
        let from = read_cursor(buf);
        if let Some(end) = next_word_end(buf, &mut cache, from, big, &spec) {
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
    let spec = KeywordSpec::parse(iskeyword);
    let mut cache = LineCache::default();
    for _ in 0..count.max(1) {
        let from = read_cursor(buf);
        match prev_word_end(buf, &mut cache, from, big, &spec) {
            Some(p) => write_cursor(buf, p),
            None => break,
        }
    }
}

// ── Find / match ────────────────────────────────────────────────────

/// Pure bracket-match query — returns the `(row, col)` of the bracket
/// that pairs with the character at `(row, col)` without moving the cursor.
///
/// C-style brackets only: `(`, `)`, `[`, `]`, `{`, `}`, `<`, `>`.
/// Nesting is depth-counted so nested pairs resolve correctly.
/// Cross-line scanning: openers scan forward, closers scan backward.
/// Returns `None` when:
/// - `(row, col)` is not a bracket character, or
/// - the bracket is unbalanced (no matching partner found).
///
/// # TODO(#240): tag-pair matching via tree-sitter
/// HTML/XML `<tag>…</tag>` matching is out of scope for the char-scan
/// implementation here. When tree-sitter grammar integration lands,
/// add a separate path that resolves element start/end nodes.
pub fn matching_bracket_pos<B: Cursor + Query>(
    buf: &B,
    row: usize,
    col: usize,
) -> Option<(usize, usize)> {
    let line = read_line_opt(buf, row)?;
    let ch = line.chars().nth(col)?;
    let (open, close, forward) = match ch {
        '(' => ('(', ')', true),
        ')' => ('(', ')', false),
        '[' => ('[', ']', true),
        ']' => ('[', ']', false),
        '{' => ('{', '}', true),
        '}' => ('{', '}', false),
        '<' => ('<', '>', true),
        '>' => ('<', '>', false),
        _ => return None,
    };
    let mut depth: i32 = 0;
    let row_count = read_row_count(buf);
    // One rope snapshot per query; each row borrows its chunk with no alloc.
    let rope = Query::rope(buf);
    if forward {
        let mut r = row;
        let mut c = col;
        loop {
            let line = crate::viewport_math::rope_line_slice(&rope, r);
            // `enumerate().skip(c)` yields the char's index within the line:
            // the first row starts at the cursor col, later rows at 0.
            for (i, here) in line.chars().enumerate().skip(c) {
                if here == open {
                    depth += 1;
                } else if here == close {
                    depth -= 1;
                    if depth == 0 {
                        return Some((r, i));
                    }
                }
            }
            if r + 1 >= row_count {
                return None;
            }
            r += 1;
            c = 0;
        }
    } else {
        let mut r = row;
        let mut c = col as isize;
        loop {
            let line = crate::viewport_math::rope_line_slice(&rope, r);
            let n = line.chars().count();
            // Walk chars at original indices c, c-1, …, 0: reverse the line's
            // chars and skip the ones past `c`. `i` is the offset from the
            // walk start, so the char position is `c - i`. No allocation.
            for (i, here) in line
                .chars()
                .rev()
                .skip(n.saturating_sub(c as usize + 1))
                .enumerate()
            {
                if here == close {
                    depth += 1;
                } else if here == open {
                    depth -= 1;
                    if depth == 0 {
                        let here_pos = c - i as isize;
                        return Some((r, here_pos as usize));
                    }
                }
            }
            if r == 0 {
                return None;
            }
            r -= 1;
            c = crate::viewport_math::rope_line_slice(&rope, r)
                .chars()
                .count()
                .saturating_sub(1) as isize;
        }
    }
}

/// `%` — jump to the matching bracket. Walks the buffer
/// counting nesting depth so nested pairs resolve correctly.
/// Returns `true` when the cursor moved.
///
/// Vim `%` does not require the cursor to sit on a bracket: it first scans
/// forward on the current line to the first bracket at or after the cursor,
/// then jumps to that bracket's match. It is a no-op when the rest of the
/// line holds no bracket.
pub fn match_bracket<B: Cursor + Query>(buf: &mut B) -> bool {
    let cursor = read_cursor(buf);
    let Some(line) = read_line_opt(buf, cursor.row) else {
        return false;
    };
    // Scan for the default `matchpairs` brackets only (`(:)`, `[:]`, `{:}`).
    // vim excludes `<:>` from the default set, so `%` neither scans to nor
    // jumps on angle brackets here. (`matching_bracket_pos` still resolves
    // `<>` for its other callers, e.g. matching-pair highlight.)
    let start_col = line
        .chars()
        .enumerate()
        .skip(cursor.col)
        .find(|(_, c)| matches!(c, '(' | ')' | '[' | ']' | '{' | '}'))
        .map(|(i, _)| i);
    let Some(start_col) = start_col else {
        return false;
    };
    match matching_bracket_pos(buf, cursor.row, start_col) {
        Some((r, c)) => {
            write_cursor(buf, Position::new(r, c));
            true
        }
        None => false,
    }
}

/// `f` / `F` / `t` / `T` — find `ch` on the current row.
/// `forward = true` searches right of the cursor; `till = true`
/// stops one cell short of the match (the `t`/`T` semantic).
///
/// `skip_adjacent` handles the `t`/`T` repeat quirk: after `t{c}` the cursor
/// sits one cell before `{c}`, so a naive repeat re-finds the same match and
/// sticks. When `skip_adjacent` is set (used by `;`/`,` and by the 2nd..Nth
/// step of a counted `t`/`T`), an immediately-adjacent target is skipped so
/// the motion advances to the next occurrence. It has no effect on `f`/`F`.
///
/// Returns `true` when the cursor moved.
pub fn find_char_on_line<B: Cursor + Query>(
    buf: &mut B,
    ch: char,
    forward: bool,
    till: bool,
    skip_adjacent: bool,
) -> bool {
    let cursor = read_cursor(buf);
    let Some(line) = read_line_opt(buf, cursor.row) else {
        return false;
    };
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() {
        return false;
    }
    let target_col = if forward {
        // Skip an immediately-adjacent target for a `t` repeat.
        let mut from = cursor.col + 1;
        if skip_adjacent && till && chars.get(from) == Some(&ch) {
            from += 1;
        }
        chars
            .iter()
            .enumerate()
            .skip(from)
            .find(|(_, c)| **c == ch)
            .map(|(i, _)| if till { i.saturating_sub(1) } else { i })
    } else {
        // Skip an immediately-adjacent target for a `T` repeat.
        let mut upto = cursor.col;
        if skip_adjacent && till && upto > 0 && chars.get(upto - 1) == Some(&ch) {
            upto -= 1;
        }
        (0..upto)
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
    let last = content_row_count(buf).saturating_sub(1);
    let target = viewport.top_row.saturating_add(offset).min(last);
    write_cursor(buf, Position::new(target, 0));
    move_first_non_blank(buf);
}

/// `M` — middle row of the visible viewport.
pub fn move_viewport_middle<B: Cursor + Query>(buf: &mut B, viewport: &hjkl_buffer::Viewport) {
    let last = content_row_count(buf).saturating_sub(1);
    let height = viewport.height as usize;
    let visible_bot = viewport
        .top_row
        .saturating_add(height.saturating_sub(1))
        .min(last);
    let mid = (viewport.top_row + visible_bot.saturating_sub(viewport.top_row) / 2).min(last);
    write_cursor(buf, Position::new(mid, 0));
    move_first_non_blank(buf);
}

/// `L` — bottom of the visible viewport, minus `offset` rows.
pub fn move_viewport_bottom<B: Cursor + Query>(
    buf: &mut B,
    viewport: &hjkl_buffer::Viewport,
    offset: usize,
) {
    let last = content_row_count(buf).saturating_sub(1);
    let height = viewport.height as usize;
    let visible_bot = viewport
        .top_row
        .saturating_add(height.saturating_sub(1))
        .min(last);
    let target = visible_bot
        .saturating_sub(offset)
        .max(viewport.top_row)
        .min(last);
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
    tabstop: usize,
) {
    if matches!(viewport.wrap, Wrap::None) || viewport.text_width == 0 {
        move_vertical(buf, folds, delta, sticky_col, tabstop);
        return;
    }
    // Preserve curswant across visual vertical motions through shorter
    // or empty segments — matching move_vertical / vim semantics.
    let cursor = read_cursor(buf);
    let want = sticky_col.unwrap_or(cursor.col);
    *sticky_col = Some(want);
    // The current row's wrap layout is computed ONCE here and threaded
    // through `step_screen`; it is only recomputed when a step crosses
    // into a different document row (the cross-row branches below). The
    // old code re-read and re-wrapped the current line on EVERY step, so
    // walking down a 10-segment wrapped line recomputed the same layout
    // ten times per keystroke.
    let mut line = read_line(buf, cursor.row);
    let mut segs = wrap::wrap_segments(&line, viewport.text_width, viewport.wrap);
    let seg_idx = wrap::segment_for_col(&segs, cursor.col);
    let visual_col = cursor.col.saturating_sub(segs[seg_idx].0);
    let down = delta > 0;
    for _ in 0..delta.unsigned_abs() {
        if !step_screen(buf, folds, viewport, down, visual_col, &mut line, &mut segs) {
            break;
        }
    }
    // sticky_col preserved as Some(want) — no overwrite here
}

/// One visual-row step under wrap. Returns false when stepping
/// would leave the buffer (top of buffer for `down=false`,
/// bottom for `down=true`).
///
/// `line`/`segs` are the CURRENT row's content and wrap layout,
/// threaded from the caller (or from a previous cross-row step) so the
/// wrap is not recomputed per step. The cross-row branches replace them
/// with the next/prev row's layout exactly once.
fn step_screen<B: Cursor + Query>(
    buf: &mut B,
    folds: &dyn FoldProvider,
    viewport: &hjkl_buffer::Viewport,
    down: bool,
    visual_col: usize,
    line: &mut String,
    segs: &mut Vec<(usize, usize)>,
) -> bool {
    let cursor = read_cursor(buf);
    let seg_idx = wrap::segment_for_col(segs, cursor.col);
    let row_count = content_row_count(buf);
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
        *line = read_line(buf, next_row);
        *segs = wrap::wrap_segments(line, viewport.text_width, viewport.wrap);
        let (s, e) = segs[0];
        let target = clamp_to_segment(s, e, visual_col, line);
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
        *line = read_line(buf, prev_row);
        *segs = wrap::wrap_segments(line, viewport.text_width, viewport.wrap);
        let (s, e) = *segs.last().unwrap_or(&(0, 0));
        let target = clamp_to_segment(s, e, visual_col, line);
        write_cursor(buf, Position::new(prev_row, target));
        true
    }
}

fn move_vertical<B: Cursor + Query>(
    buf: &mut B,
    folds: &dyn FoldProvider,
    delta: isize,
    sticky_col: &mut Option<usize>,
    tabstop: usize,
) {
    let cursor = read_cursor(buf);
    // sticky_col now stores a display column (vim's curswant).
    // Bootstrap from the current cursor position converted to display
    // columns so the first `j`/`k` after a horizontal move preserves the
    // visual column.
    let want = if let Some(col) = *sticky_col {
        col
    } else {
        let cursor_line = read_line(buf, cursor.row);
        char_col_to_visual_col(&cursor_line, cursor.col, tabstop)
    };
    *sticky_col = Some(want);
    // Walk one visible row at a time so closed folds count as one
    // visual line. Stops at top/bottom of buffer.
    let mut target_row = cursor.row;
    let row_count = content_row_count(buf);
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
    // Convert the display column back to a char index on the target line.
    // On a line with tabs, display col 8 might map to char index 5, etc.
    let line = read_line(buf, target_row);
    let char_col = visual_col_to_char_col(&line, want, tabstop);
    let max_col = last_col(&line);
    let target_col = char_col.min(max_col);
    write_cursor(buf, Position::new(target_row, target_col));
}

/// True if `c` qualifies as a word character under `spec`.
fn is_word(c: char, spec: &KeywordSpec) -> bool {
    spec.matches(c)
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

fn char_kind(c: char, big: bool, iskeyword: &KeywordSpec) -> CharKind {
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

/// Single-entry per-row line cache for word-motion scans.
///
/// Word scans (`w`/`b`/`e`/`ge`) step one character at a time and mostly
/// stay within a row before crossing to the next, so a single `(row,
/// Vec<char>)` slot collapses what used to be one whole-line clone *per
/// character examined* down to one clone *per row*. The row's chars are
/// materialized once (`read_line` + `collect()`), then both accessors are
/// O(1): `char_count` is the `Vec` length and `char_at` is a direct index
/// — the old `chars().nth(col)` re-decoded the line from its start on
/// EVERY step, making a `w` across a long line O(line²) in char decodes.
/// Every accessor mirrors [`read_line`] exactly — an out-of-bounds row
/// yields an empty `Vec`, so lookups are byte-for-byte identical to the
/// old `read_line_opt(..)?.chars()…` path (`chars().nth(col)` on an empty
/// string is `None`, matching the old early-return on an OOB row). The
/// cache holds an owned copy independent of `buf`, so it stays valid
/// across the cursor writes between counted iterations (word motions
/// never mutate text).
#[derive(Default)]
struct LineCache {
    row: usize,
    chars: Vec<char>,
    loaded: bool,
}

impl LineCache {
    /// Borrow row `row`'s chars, refreshing the slot when the scan
    /// crosses to a different row. Mirrors [`read_line`].
    #[inline]
    fn chars<B: Query + ?Sized>(&mut self, buf: &B, row: usize) -> &[char] {
        if !self.loaded || self.row != row {
            self.chars = read_line(buf, row).chars().collect();
            self.row = row;
            self.loaded = true;
        }
        &self.chars
    }

    /// Char count of row `row` — mirrors `line_chars(&read_line(buf, row))`.
    #[inline]
    fn char_count<B: Query + ?Sized>(&mut self, buf: &B, row: usize) -> usize {
        self.chars(buf, row).len()
    }

    /// Char at `pos`, or `None` past end-of-line / out-of-bounds row.
    /// Byte-for-byte identical to the pre-cache free `char_at`.
    #[inline]
    fn char_at<B: Query + ?Sized>(&mut self, buf: &B, pos: Position) -> Option<char> {
        self.chars(buf, pos.row).get(pos.col).copied()
    }
}

/// Step one position forward, wrapping into the next row.
///
/// The wrap is bounded by [`content_row_count`], never raw
/// [`read_row_count`]: stepping past the last char of a trailing-`\n`
/// buffer must return `None` instead of landing on ropey's phantom
/// trailing empty row (which every other vertical motion already
/// excludes). A *real* empty last line (`"foo\n\n"`) stays reachable —
/// [`content_row_count`] only skips the single phantom row.
fn step_forward<B: Query + ?Sized>(
    buf: &B,
    cache: &mut LineCache,
    pos: Position,
) -> Option<Position> {
    let len = cache.char_count(buf, pos.row);
    if pos.col + 1 < len {
        return Some(Position::new(pos.row, pos.col + 1));
    }
    if pos.row + 1 < content_row_count(buf) {
        return Some(Position::new(pos.row + 1, 0));
    }
    None
}

/// Vim's `dec()` — step one position back, wrapping onto the previous row's
/// VIRTUAL end-of-line cell (column `len`, one past the last character), not
/// onto its last character.
///
/// That cell is what makes a backward word walk stop at a line boundary:
/// `cls()` reads the line's NUL terminator there and answers "whitespace", so
/// a same-class run cannot continue from one row onto the row above. Stepping
/// straight to `len - 1` instead lands on a real character, and `b` on
/// `"abc def\nghi jkl"` from `(1, 1)` walked all the way back to `(0, 4)`
/// where vim stays on `(1, 0)`.
///
/// On an empty previous row the cell is `(row - 1, 0)`, which is also the
/// position vim's "an empty line is a word" rule stops on.
fn dec<B: Query + ?Sized>(buf: &B, cache: &mut LineCache, pos: Position) -> Option<Position> {
    if pos.col > 0 {
        return Some(Position::new(pos.row, pos.col - 1));
    }
    if pos.row == 0 {
        return None;
    }
    let prev_row = pos.row - 1;
    Some(Position::new(prev_row, cache.char_count(buf, prev_row)))
}

/// Vim's `inc()` — the inverse of [`dec`]. Inside a row it advances by one,
/// which can land ON the virtual end-of-line cell; from that cell it wraps to
/// column 0 of the next row. The wrap is bounded by [`content_row_count`]
/// (see [`step_forward`]) so it can never step onto the phantom trailing row.
fn inc<B: Query + ?Sized>(buf: &B, cache: &mut LineCache, pos: Position) -> Option<Position> {
    if pos.col < cache.char_count(buf, pos.row) {
        return Some(Position::new(pos.row, pos.col + 1));
    }
    if pos.row + 1 < content_row_count(buf) {
        return Some(Position::new(pos.row + 1, 0));
    }
    None
}

/// Vim's `cls()` — the character class at `pos`, where the virtual
/// end-of-line cell (and any out-of-line position) reads as whitespace.
fn cls<B: Query + ?Sized>(
    buf: &B,
    cache: &mut LineCache,
    pos: Position,
    big: bool,
    iskeyword: &KeywordSpec,
) -> CharKind {
    cache
        .char_at(buf, pos)
        .map_or(CharKind::Space, |c| char_kind(c, big, iskeyword))
}

/// Vim's `LINEEMPTY(lnum)` — the row holds no characters at all.
fn line_empty<B: Query + ?Sized>(buf: &B, cache: &mut LineCache, row: usize) -> bool {
    cache.char_count(buf, row) == 0
}

fn next_word_start<B: Query + ?Sized>(
    buf: &B,
    cache: &mut LineCache,
    from: Position,
    big: bool,
    iskeyword: &KeywordSpec,
) -> Option<Position> {
    let start_kind = cache
        .char_at(buf, from)
        .map(|c| char_kind(c, big, iskeyword));
    let mut cur = from;
    // Skip the rest of the current word kind. Vim treats line
    // breaks as whitespace separators for `w`, so a row crossing
    // implicitly ends the current word — break and let the
    // skip-space pass handle anything beyond.
    if let Some(kind) = start_kind {
        while cache
            .char_at(buf, cur)
            .map(|c| char_kind(c, big, iskeyword))
            == Some(kind)
        {
            let prev_row = cur.row;
            match step_forward(buf, cache, cur) {
                Some(next) => {
                    cur = next;
                    if next.row != prev_row {
                        break;
                    }
                }
                None => return Some(end_of_buffer(buf)),
            }
        }
    } else {
        // `from` sits on an empty line. Vim counts an empty line as a word
        // (`:help word`), so `w` steps off it. For `W` (big word), empty
        // lines are not WORDs — skip through consecutive empty lines to
        // find the next non-empty line.
        {
            let next = step_forward(buf, cache, cur)?;
            cur = next;
        }
        if big {
            while cache.char_at(buf, cur).is_none() {
                let next = step_forward(buf, cache, cur)?;
                cur = next;
            }
        }
    }
    // Skip whitespace runs (within row + across rows) to land on
    // the next non-space char.
    while cache
        .char_at(buf, cur)
        .map(|c| char_kind(c, big, iskeyword))
        == Some(CharKind::Space)
    {
        match step_forward(buf, cache, cur) {
            Some(next) => cur = next,
            None => return Some(end_of_buffer(buf)),
        }
    }
    Some(cur)
}

/// One past the last char of the last CONTENT row — vim's "end of buffer"
/// for operator-context word motions, so `yw` at end-of-line yanks up to and
/// including the last char. Must use [`content_row_count`], not raw
/// [`read_row_count`]: for a trailing-`\n` buffer the raw count's last row is
/// ropey's phantom empty row, and `yw` would anchor one past nothing instead
/// of one past the last real char.
fn end_of_buffer<B: Query + ?Sized>(buf: &B) -> Position {
    let last_row = content_row_count(buf).saturating_sub(1);
    let last_line = read_line(buf, last_row);
    Position::new(last_row, line_chars(&last_line))
}

fn prev_word_start<B: Query + ?Sized>(
    buf: &B,
    cache: &mut LineCache,
    from: Position,
    big: bool,
    iskeyword: &KeywordSpec,
) -> Option<Position> {
    // Vim's `bck_word(count = 1, bigword, stop = false)`. `b` and `B` always
    // pass `stop = false`, so the `sclass` early-out in vim's version can
    // never fire and is not reproduced here.
    let mut cur = dec(buf, cache, from)?; // vim: `dec_cursor() == -1` -> FAIL

    // Skip whitespace before the word, stopping on an empty line — an empty
    // line is a word (and a WORD) for the backward motions. Running out of
    // buffer inside that whitespace is vim's `dec_cursor() == -1 -> return
    // OK`: the motion succeeds where it stands.
    let mut on_empty_line = false;
    while cls(buf, cache, cur, big, iskeyword) == CharKind::Space {
        if cur.col == 0 && line_empty(buf, cache, cur.row) {
            on_empty_line = true;
            break;
        }
        match dec(buf, cache, cur) {
            Some(prev) => cur = prev,
            None => return Some(cur),
        }
    }
    if on_empty_line {
        return Some(cur);
    }

    // vim's `skip_chars(cls(), BACKWARD)` — walk back off the front of this
    // word, then `inc_cursor()` to undo the overshoot. Hitting the start of
    // the buffer mid-run is again a success where it stands, with no `inc`.
    let target = cls(buf, cache, cur, big, iskeyword);
    while cls(buf, cache, cur, big, iskeyword) == target {
        match dec(buf, cache, cur) {
            Some(prev) => cur = prev,
            None => return Some(cur),
        }
    }
    Some(inc(buf, cache, cur).unwrap_or(cur))
}

/// `ge` / `gE` — walk back to the end of the previous word. The
/// stopping rule mirrors `next_word_end`'s definition of "end":
/// non-whitespace position whose next char is a different kind
/// (or end-of-line / end-of-buffer).
fn prev_word_end<B: Query + ?Sized>(
    buf: &B,
    cache: &mut LineCache,
    from: Position,
    big: bool,
    iskeyword: &KeywordSpec,
) -> Option<Position> {
    // Vim's `bckend_word(count = 1, bigword, eol = false)`. `ge` and `gE`
    // both pass `eol = false`, so the "stop at end-of-line" arm of vim's
    // version can never fire and is not reproduced here.
    let sclass = cls(buf, cache, from, big, iskeyword);
    let mut cur = dec(buf, cache, from)?; // vim: `dec_cursor() == -1` -> FAIL

    // Move back off the start of the word the cursor was inside.
    if sclass != CharKind::Space {
        while cls(buf, cache, cur, big, iskeyword) == sclass {
            match dec(buf, cache, cur) {
                Some(prev) => cur = prev,
                None => return Some(cur),
            }
        }
    }

    // Move back to the end of the previous word, stopping on an empty line.
    while cls(buf, cache, cur, big, iskeyword) == CharKind::Space {
        if cur.col == 0 && line_empty(buf, cache, cur.row) {
            break;
        }
        match dec(buf, cache, cur) {
            Some(prev) => cur = prev,
            None => return Some(cur),
        }
    }
    Some(cur)
}

fn next_word_end<B: Query + ?Sized>(
    buf: &B,
    cache: &mut LineCache,
    from: Position,
    big: bool,
    iskeyword: &KeywordSpec,
) -> Option<Position> {
    // Vim's `e` advances at least one cell, then walks forward
    // until the *next* char is a different kind (or eof).
    let mut cur = step_forward(buf, cache, from)?;
    while cache
        .char_at(buf, cur)
        .map(|c| char_kind(c, big, iskeyword))
        == Some(CharKind::Space)
    {
        cur = step_forward(buf, cache, cur)?;
    }
    let kind = cache
        .char_at(buf, cur)
        .map(|c| char_kind(c, big, iskeyword))?;
    loop {
        let Some(next) = step_forward(buf, cache, cur) else {
            return Some(cur);
        };
        if cache
            .char_at(buf, next)
            .map(|c| char_kind(c, big, iskeyword))
            == Some(kind)
        {
            cur = next;
        } else {
            return Some(cur);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hjkl_buffer::View;

    use crate::buffer_impl::SnapshotFoldProvider;

    /// Default `iskeyword` spec used by tests — matches vim's default
    /// (`@,48-57,_,192-255`) and the engine's `Settings::default()`.
    const ISK: &str = "@,48-57,_,192-255";

    fn at(b: &View) -> Position {
        b.cursor()
    }

    /// Build a [`SnapshotFoldProvider`] from the supplied buffer.
    /// Tests build this once per assertion since the fold list is
    /// tiny — production call sites in `vim.rs` mirror this shape.
    /// Snapshot decouples from the buffer's lifetime so the caller
    /// can re-borrow `&mut buf` for the motion fn.
    fn folds(b: &View) -> SnapshotFoldProvider {
        SnapshotFoldProvider::from_buffer(b)
    }

    #[test]
    fn move_left_clamps_at_zero() {
        let mut b = View::from_str("abcd");
        move_right_in_line(&mut b, 3);
        assert_eq!(at(&b), Position::new(0, 3));
        move_left(&mut b, 10);
        assert_eq!(at(&b), Position::new(0, 0));
    }

    #[test]
    fn move_left_does_not_wrap_to_prev_row() {
        let mut b = View::from_str("abc\ndef");
        let mut sticky = None;
        {
            let f = folds(&b);
            move_down(&mut b, &f, 1, &mut sticky, 4);
        }
        assert_eq!(at(&b).row, 1);
        move_left(&mut b, 99);
        assert_eq!(at(&b), Position::new(1, 0));
    }

    #[test]
    fn move_right_in_line_stops_at_last_char() {
        let mut b = View::from_str("abcd");
        move_right_in_line(&mut b, 99);
        assert_eq!(at(&b), Position::new(0, 3));
    }

    #[test]
    fn move_right_to_end_allows_one_past() {
        let mut b = View::from_str("abcd");
        move_right_to_end(&mut b, 99);
        assert_eq!(at(&b), Position::new(0, 4));
    }

    #[test]
    fn move_line_start_end() {
        let mut b = View::from_str("  hello");
        move_line_end(&mut b);
        assert_eq!(at(&b), Position::new(0, 6));
        move_line_start(&mut b);
        assert_eq!(at(&b), Position::new(0, 0));
        move_first_non_blank(&mut b);
        assert_eq!(at(&b), Position::new(0, 2));
    }

    #[test]
    fn move_line_end_on_empty_row_stays_at_zero() {
        let mut b = View::from_str("");
        move_line_end(&mut b);
        assert_eq!(at(&b), Position::new(0, 0));
    }

    #[test]
    fn move_down_preserves_sticky_col_across_short_row() {
        let mut b = View::from_str("hello world\nhi\nlong line again");
        move_right_in_line(&mut b, 7);
        assert_eq!(at(&b), Position::new(0, 7));
        let mut sticky = None;
        {
            let f = folds(&b);
            move_down(&mut b, &f, 1, &mut sticky, 4);
        }
        assert_eq!(at(&b).row, 1);
        // Short row clamps to col 1 (last char of "hi").
        assert_eq!(at(&b).col, 1);
        {
            let f = folds(&b);
            move_down(&mut b, &f, 1, &mut sticky, 4);
        }
        // Sticky col 7 restored on the longer row.
        assert_eq!(at(&b), Position::new(2, 7));
    }

    #[test]
    fn move_down_preserves_sticky_col_across_empty_row() {
        let mut b = View::from_str("hello world\n\nlong line again");
        // position cursor at col 7
        move_right_in_line(&mut b, 7);
        assert_eq!(at(&b), Position::new(0, 7));
        let mut sticky = None;
        {
            let f = folds(&b);
            move_down(&mut b, &f, 1, &mut sticky, 4);
        }
        assert_eq!(at(&b).row, 1);
        assert_eq!(at(&b).col, 0); // empty line clamps to 0
        assert_eq!(sticky, Some(7)); // want preserved
        {
            let f = folds(&b);
            move_down(&mut b, &f, 1, &mut sticky, 4);
        }
        assert_eq!(at(&b), Position::new(2, 7)); // restored on long line
    }

    #[test]
    fn move_down_skips_closed_fold() {
        let mut b = View::from_str("a\nb\nc\nd\ne");
        b.add_fold(1, 3, true);
        let mut sticky = None;
        // From row 0, `j` should land on row 4 — the fold collapses
        // rows 1..=3 into a single visual line at row 1.
        {
            let f = folds(&b);
            move_down(&mut b, &f, 1, &mut sticky, 4);
        }
        assert_eq!(at(&b).row, 1);
        {
            let f = folds(&b);
            move_down(&mut b, &f, 1, &mut sticky, 4);
        }
        assert_eq!(at(&b).row, 4);
    }

    #[test]
    fn move_up_skips_closed_fold() {
        let mut b = View::from_str("a\nb\nc\nd\ne");
        b.add_fold(1, 3, true);
        b.set_cursor(Position::new(4, 0));
        let mut sticky = None;
        {
            let f = folds(&b);
            move_up(&mut b, &f, 1, &mut sticky, 4);
        }
        assert_eq!(at(&b).row, 1);
        {
            let f = folds(&b);
            move_up(&mut b, &f, 1, &mut sticky, 4);
        }
        assert_eq!(at(&b).row, 0);
    }

    #[test]
    fn open_fold_is_walked_normally() {
        let mut b = View::from_str("a\nb\nc\nd\ne");
        b.add_fold(1, 3, false);
        let mut sticky = None;
        // Open fold: every row is visible, plain row-by-row stepping.
        {
            let f = folds(&b);
            move_down(&mut b, &f, 2, &mut sticky, 4);
        }
        assert_eq!(at(&b).row, 2);
    }

    #[test]
    fn move_top_lands_on_first_non_blank() {
        let mut b = View::from_str("    indented\nrow2");
        let mut sticky = None;
        {
            let f = folds(&b);
            move_down(&mut b, &f, 1, &mut sticky, 4);
        }
        move_top(&mut b);
        assert_eq!(at(&b), Position::new(0, 4));
    }

    #[test]
    fn move_bottom_with_count_jumps_to_line() {
        let mut b = View::from_str("a\n  b\nc\nd");
        move_bottom(&mut b, 2);
        assert_eq!(at(&b), Position::new(1, 2));
    }

    #[test]
    fn move_bottom_zero_jumps_to_last_row() {
        let mut b = View::from_str("a\nb\nc");
        move_bottom(&mut b, 0);
        assert_eq!(at(&b), Position::new(2, 0));
    }

    #[test]
    fn move_word_fwd_skips_whitespace_runs() {
        let mut b = View::from_str("foo bar  baz");
        move_word_fwd(&mut b, false, 1, ISK);
        assert_eq!(at(&b), Position::new(0, 4));
        move_word_fwd(&mut b, false, 1, ISK);
        assert_eq!(at(&b), Position::new(0, 9));
    }

    #[test]
    fn move_word_fwd_separates_word_from_punct_in_small_w() {
        let mut b = View::from_str("foo.bar");
        move_word_fwd(&mut b, false, 1, ISK);
        assert_eq!(at(&b), Position::new(0, 3));
        move_word_fwd(&mut b, false, 1, ISK);
        assert_eq!(at(&b), Position::new(0, 4));
    }

    #[test]
    fn move_word_fwd_off_empty_line() {
        // vim: `w` on an empty line counts the empty line as a word and steps
        // to the first word of the next non-empty line.
        let mut b = View::from_str("foo\n\nbar");
        let mut sticky = None;
        {
            let f = folds(&b);
            move_down(&mut b, &f, 1, &mut sticky, 4);
        }
        assert_eq!(at(&b), Position::new(1, 0));
        move_word_fwd(&mut b, false, 1, ISK);
        assert_eq!(at(&b), Position::new(2, 0));
    }

    #[test]
    fn move_word_fwd_empty_line_stops_on_next_empty_line() {
        // Each empty line is its own word, so `w` from one blank line lands on
        // the next blank line, not skipping to the following text.
        let mut b = View::from_str("foo\n\n\nbar");
        let mut sticky = None;
        {
            let f = folds(&b);
            move_down(&mut b, &f, 1, &mut sticky, 4);
        }
        assert_eq!(at(&b), Position::new(1, 0));
        move_word_fwd(&mut b, false, 1, ISK);
        assert_eq!(at(&b), Position::new(2, 0));
    }

    #[test]
    fn move_word_fwd_big_collapses_word_and_punct() {
        let mut b = View::from_str("foo.bar baz");
        move_word_fwd(&mut b, true, 1, ISK);
        assert_eq!(at(&b), Position::new(0, 8));
    }

    /// `w` / `W` from the last word of a trailing-`\n` buffer must land one
    /// past the last char of the LAST REAL row — the same endpoint the
    /// non-terminated buffer produces — never on ropey's phantom trailing
    /// empty row. The vim layer's normal-mode clamp (`WordFwd` arm in
    /// `hjkl-vim/src/vim/motion.rs`) pulls that one-past-end endpoint back
    /// to the last char, so the observable cursor stays put on nvim; the
    /// operator-context `yw` still yanks through the last char.
    #[test]
    fn move_word_fwd_at_eof_newline_terminated_stays_off_phantom_row() {
        // Rows: ["abc def", ""] — the trailing `\n` synthesizes the phantom
        // empty row. From the last char, `w` must not step onto it.
        let mut b = View::from_str("abc def\n");
        b.set_cursor(Position::new(0, 6));
        move_word_fwd(&mut b, false, 1, ISK);
        assert_eq!(at(&b), Position::new(0, 7));

        // Big-word `W` shares the same walk.
        let mut b = View::from_str("abc def\n");
        b.set_cursor(Position::new(0, 6));
        move_word_fwd(&mut b, true, 1, ISK);
        assert_eq!(at(&b), Position::new(0, 7));

        // Non-newline-terminated buffer behaves as before: same landing.
        let mut b = View::from_str("abc def");
        b.set_cursor(Position::new(0, 6));
        move_word_fwd(&mut b, false, 1, ISK);
        assert_eq!(at(&b), Position::new(0, 7));

        // A REAL empty last line ("foo\n\n" -> rows ["foo", "", ""]) stays
        // reachable: `w` off "foo" steps onto it, since only the single
        // phantom row is excluded by `content_row_count`.
        let mut b = View::from_str("foo\n\n");
        b.set_cursor(Position::new(0, 2));
        move_word_fwd(&mut b, false, 1, ISK);
        assert_eq!(at(&b), Position::new(1, 0));
    }

    /// `w` across a very long line. The old `LineCache::char_at` ran
    /// `chars().nth(col)`, re-decoding the line from its start on every
    /// step — a 2000-char walk was O(line²) char decodes. The per-row
    /// `Vec<char>` cache indexes in O(1); this pins the landing position
    /// (the word start past the whitespace at the far end) so the
    /// refactor cannot drift the walk.
    #[test]
    fn move_word_fwd_across_long_line_lands_correctly() {
        let mut line = "word ".repeat(400); // 2000 chars
        line.push_str("target");
        let mut b = View::from_str(&line);
        // 400 `w` steps: one per "word", landing on "target"'s start.
        move_word_fwd(&mut b, false, 400, ISK);
        assert_eq!(at(&b), Position::new(0, 2000));
    }

    /// `e` / `E` from the last word of a trailing-`\n` buffer stays on the
    /// last char — no next word end exists. `e` never returned the phantom
    /// row (its walk gives up when `char_at` reads `None` there); this pins
    /// the behavior against a future `step_forward` change.
    #[test]
    fn move_word_end_at_eof_newline_terminated_stays_on_last_char() {
        let mut b = View::from_str("abc def\n");
        b.set_cursor(Position::new(0, 6));
        move_word_end(&mut b, false, 1, ISK);
        assert_eq!(at(&b), Position::new(0, 6));

        // Non-newline-terminated buffer behaves as before.
        let mut b = View::from_str("abc def");
        b.set_cursor(Position::new(0, 6));
        move_word_end(&mut b, false, 1, ISK);
        assert_eq!(at(&b), Position::new(0, 6));
    }

    #[test]
    fn move_word_back_lands_on_word_start() {
        let mut b = View::from_str("foo bar baz");
        move_line_end(&mut b);
        assert_eq!(at(&b), Position::new(0, 10));
        move_word_back(&mut b, false, 1, ISK);
        assert_eq!(at(&b), Position::new(0, 8));
        move_word_back(&mut b, false, 2, ISK);
        assert_eq!(at(&b), Position::new(0, 0));
    }

    #[test]
    fn move_word_end_lands_on_last_char() {
        let mut b = View::from_str("foo bar");
        move_word_end(&mut b, false, 1, ISK);
        assert_eq!(at(&b), Position::new(0, 2));
        move_word_end(&mut b, false, 1, ISK);
        assert_eq!(at(&b), Position::new(0, 6));
    }

    #[test]
    fn find_char_forward_lands_on_match() {
        let mut b = View::from_str("foo,bar,baz");
        assert!(find_char_on_line(&mut b, ',', true, false, false));
        assert_eq!(at(&b), Position::new(0, 3));
        assert!(find_char_on_line(&mut b, ',', true, false, false));
        assert_eq!(at(&b), Position::new(0, 7));
    }

    #[test]
    fn find_char_till_stops_one_short() {
        let mut b = View::from_str("foo,bar");
        assert!(find_char_on_line(&mut b, ',', true, true, false));
        assert_eq!(at(&b), Position::new(0, 2));
    }

    #[test]
    fn find_char_till_repeat_skips_adjacent() {
        // `t,` lands one cell before the comma; a repeat with `skip_adjacent`
        // advances to just before the NEXT comma instead of sticking.
        let mut b = View::from_str("a,b,c");
        assert!(find_char_on_line(&mut b, ',', true, true, false));
        assert_eq!(at(&b), Position::new(0, 0));
        assert!(find_char_on_line(&mut b, ',', true, true, true));
        assert_eq!(at(&b), Position::new(0, 2));
    }

    #[test]
    fn find_char_till_reverse_repeat_skips_adjacent() {
        // `T,` from the end lands one cell after the comma; a reverse repeat
        // advances to just after the previous comma.
        let mut b = View::from_str("a,b,c");
        b.set_cursor(Position::new(0, 4));
        assert!(find_char_on_line(&mut b, ',', false, true, false));
        assert_eq!(at(&b), Position::new(0, 4));
        assert!(find_char_on_line(&mut b, ',', false, true, true));
        assert_eq!(at(&b), Position::new(0, 2));
    }

    #[test]
    fn find_char_backward_lands_on_match() {
        let mut b = View::from_str("foo,bar,baz");
        b.set_cursor(Position::new(0, 10));
        assert!(find_char_on_line(&mut b, ',', false, false, false));
        assert_eq!(at(&b), Position::new(0, 7));
    }

    #[test]
    fn find_char_no_match_returns_false() {
        let mut b = View::from_str("hello");
        assert!(!find_char_on_line(&mut b, 'z', true, false, false));
        assert_eq!(at(&b), Position::new(0, 0));
    }

    #[test]
    fn move_viewport_top_with_offset() {
        let mut b = View::from_str("a\nb\nc\nd\ne\nf");
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
        let mut b = View::from_str("a\nb\nc\nd\ne");
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
        let mut b = View::from_str("a\nb\nc\nd\ne");
        let v = hjkl_buffer::Viewport {
            top_row: 0,
            height: 5,
            ..Default::default()
        };
        move_viewport_bottom(&mut b, &v, 1);
        assert_eq!(at(&b), Position::new(3, 0));
    }

    #[test]
    fn move_viewport_middle_clamps_when_top_row_past_end() {
        // Simulates a stale viewport (e.g. right after a bulk delete
        // shrinks the buffer, before the viewport re-clamps): top_row
        // is past the buffer's last row. The computed middle must still
        // clamp to the last row instead of going out of bounds.
        let mut b = View::from_str("a\nb\nc");
        let v = hjkl_buffer::Viewport {
            top_row: 10,
            height: 4,
            ..Default::default()
        };
        move_viewport_middle(&mut b, &v);
        assert_eq!(at(&b), Position::new(2, 0));
    }

    #[test]
    fn move_viewport_bottom_clamps_when_top_row_past_end() {
        let mut b = View::from_str("a\nb\nc");
        let v = hjkl_buffer::Viewport {
            top_row: 10,
            height: 4,
            ..Default::default()
        };
        move_viewport_bottom(&mut b, &v, 0);
        assert_eq!(at(&b), Position::new(2, 0));
    }

    #[test]
    fn move_word_end_back_lands_on_prev_word_end() {
        let mut b = View::from_str("foo bar baz");
        b.set_cursor(Position::new(0, 9));
        move_word_end_back(&mut b, false, 1, ISK);
        assert_eq!(at(&b), Position::new(0, 6));
        move_word_end_back(&mut b, false, 1, ISK);
        assert_eq!(at(&b), Position::new(0, 2));
    }

    /// vim's `dec()` steps from column 0 onto the previous row's VIRTUAL
    /// end-of-line cell, whose class is whitespace — that is what ends a
    /// same-class run at the line boundary. Stepping to the last character
    /// instead let the run continue onto the row above, so `b` from inside
    /// the first word of row 1 walked back into row 0. Every expectation
    /// measured on neovim 0.12.4.
    #[test]
    fn move_word_back_stops_at_the_line_boundary() {
        // Inside "ghi": the word start is on this row, so `b` stays here.
        let mut b = View::from_str("abc def\nghi jkl");
        b.set_cursor(Position::new(1, 1));
        move_word_back(&mut b, false, 1, ISK);
        assert_eq!(at(&b), Position::new(1, 0));

        // Same for `B` over a punctuation-heavy WORD.
        let mut b = View::from_str("'self.x'\n(foo) a, {xy}  ");
        b.set_cursor(Position::new(1, 1));
        move_word_back(&mut b, true, 1, ISK);
        assert_eq!(at(&b), Position::new(1, 0));

        // From column 0 there IS no word start left on the row, so the
        // motion crosses — onto the previous word's start, not into it.
        let mut b = View::from_str("abc def\nghi jkl");
        b.set_cursor(Position::new(1, 0));
        move_word_back(&mut b, false, 1, ISK);
        assert_eq!(at(&b), Position::new(0, 4));
    }

    /// An empty line is a word (and a WORD) for the backward motions: the
    /// whitespace skip stops on it instead of running through to the
    /// paragraph above.
    #[test]
    fn move_word_back_stops_on_an_empty_line() {
        let mut b = View::from_str("abc\n\ndef");
        b.set_cursor(Position::new(2, 1));
        move_word_back(&mut b, false, 1, ISK);
        assert_eq!(at(&b), Position::new(2, 0));
        move_word_back(&mut b, false, 1, ISK);
        assert_eq!(at(&b), Position::new(1, 0));
    }

    /// `ge` inside the first word of the buffer has no previous word end to
    /// find. vim walks off the front and succeeds at the buffer start; the
    /// old walk returned without moving at all.
    #[test]
    fn move_word_end_back_inside_first_word_lands_at_buffer_start() {
        let mut b = View::from_str("abc def\nghi jkl");
        b.set_cursor(Position::new(0, 2));
        move_word_end_back(&mut b, false, 1, ISK);
        assert_eq!(at(&b), Position::new(0, 0));

        let mut b = View::from_str("'self.x'\n(foo) a, {xy}  ");
        b.set_cursor(Position::new(0, 4));
        move_word_end_back(&mut b, true, 1, ISK);
        assert_eq!(at(&b), Position::new(0, 0));
    }

    #[test]
    fn move_word_end_back_big_skips_punct() {
        let mut b = View::from_str("foo-bar qux");
        b.set_cursor(Position::new(0, 10));
        move_word_end_back(&mut b, true, 1, ISK);
        assert_eq!(at(&b), Position::new(0, 6));
    }

    #[test]
    fn move_word_end_back_crosses_lines() {
        let mut b = View::from_str("abc\ndef");
        b.set_cursor(Position::new(1, 2));
        move_word_end_back(&mut b, false, 1, ISK);
        assert_eq!(at(&b), Position::new(0, 2));
    }

    #[test]
    fn match_bracket_pairs_within_line() {
        let mut b = View::from_str("if (x + y) {");
        b.set_cursor(Position::new(0, 3));
        assert!(match_bracket(&mut b));
        assert_eq!(at(&b), Position::new(0, 9));
        assert!(match_bracket(&mut b));
        assert_eq!(at(&b), Position::new(0, 3));
    }

    #[test]
    fn match_bracket_handles_nesting() {
        let mut b = View::from_str("((x))");
        b.set_cursor(Position::new(0, 0));
        assert!(match_bracket(&mut b));
        assert_eq!(at(&b), Position::new(0, 4));
    }

    #[test]
    fn match_bracket_crosses_lines() {
        let mut b = View::from_str("{\n  x\n}");
        b.set_cursor(Position::new(0, 0));
        assert!(match_bracket(&mut b));
        assert_eq!(at(&b), Position::new(2, 0));
    }

    #[test]
    fn match_bracket_returns_false_off_bracket() {
        let mut b = View::from_str("hello");
        assert!(!match_bracket(&mut b));
    }

    // ── matching_bracket_pos (pure) tests ─────────────────────────────

    #[test]
    fn matching_bracket_pos_same_line_pair() {
        let b = View::from_str("foo(bar)baz");
        // opener at col 3 → closer at col 7
        assert_eq!(matching_bracket_pos(&b, 0, 3), Some((0, 7)));
        // closer at col 7 → opener at col 3
        assert_eq!(matching_bracket_pos(&b, 0, 7), Some((0, 3)));
    }

    #[test]
    fn matching_bracket_pos_nesting() {
        let b = View::from_str("((x))");
        // outer opener at 0 → outer closer at 4
        assert_eq!(matching_bracket_pos(&b, 0, 0), Some((0, 4)));
        // inner opener at 1 → inner closer at 3
        assert_eq!(matching_bracket_pos(&b, 0, 1), Some((0, 3)));
    }

    #[test]
    fn matching_bracket_pos_cross_line() {
        let b = View::from_str("{\n  x\n}");
        assert_eq!(matching_bracket_pos(&b, 0, 0), Some((2, 0)));
        assert_eq!(matching_bracket_pos(&b, 2, 0), Some((0, 0)));
    }

    #[test]
    fn matching_bracket_pos_off_bracket_none() {
        let b = View::from_str("hello world");
        assert_eq!(matching_bracket_pos(&b, 0, 0), None);
        assert_eq!(matching_bracket_pos(&b, 0, 4), None);
    }

    #[test]
    fn matching_bracket_pos_unbalanced_none() {
        // Unmatched opener — no closing bracket
        let b = View::from_str("(abc");
        assert_eq!(matching_bracket_pos(&b, 0, 0), None);
        // Unmatched closer — no opening bracket
        let b2 = View::from_str("abc)");
        assert_eq!(matching_bracket_pos(&b2, 0, 3), None);
    }

    #[test]
    fn matching_bracket_pos_closer_finds_opener() {
        // Ensure backward scan from a closer correctly returns the opener.
        let b = View::from_str("[hello]");
        assert_eq!(matching_bracket_pos(&b, 0, 6), Some((0, 0)));
    }

    #[test]
    fn match_bracket_scans_forward_on_line() {
        // vim `%` doesn't need the cursor on a bracket: it scans forward on
        // the line to the first bracket, then jumps to its match.
        let mut b = View::from_str("foo(bar)");
        assert_eq!(at(&b), Position::new(0, 0));
        assert!(match_bracket(&mut b));
        assert_eq!(at(&b), Position::new(0, 7));
    }

    #[test]
    fn match_bracket_noop_when_no_bracket_on_line() {
        let mut b = View::from_str("foobar baz");
        assert!(!match_bracket(&mut b));
        assert_eq!(at(&b), Position::new(0, 0));
    }

    #[test]
    fn match_bracket_ignores_angle_brackets() {
        // Default `matchpairs` excludes `<:>`, so `%` does not scan to or jump
        // on angle brackets.
        let mut b = View::from_str("a<b>");
        assert!(!match_bracket(&mut b));
        assert_eq!(at(&b), Position::new(0, 0));
    }

    #[test]
    fn motion_count_zero_treated_as_one() {
        let mut b = View::from_str("abcd");
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
        let mut b = View::from_str("a\nb\nc");
        let v = hjkl_buffer::Viewport::default();
        let mut sticky = None;
        {
            let f = folds(&b);
            move_screen_down(&mut b, &f, &v, 1, &mut sticky, 4);
        }
        assert_eq!(at(&b), Position::new(1, 0));
        {
            let f = folds(&b);
            move_screen_down(&mut b, &f, &v, 1, &mut sticky, 4);
        }
        assert_eq!(at(&b), Position::new(2, 0));
    }

    #[test]
    fn screen_down_walks_within_wrapped_row() {
        // 12-char line, width 4 → segments (0,4), (4,8), (8,12).
        let mut b = View::from_str("aaaabbbbcccc\nx");
        let v = make_wrap_viewport(Wrap::Char, 4);
        b.set_cursor(Position::new(0, 1));
        let mut sticky = None;
        {
            let f = folds(&b);
            move_screen_down(&mut b, &f, &v, 1, &mut sticky, 4);
        }
        // visual_col = 1 → next segment starts at 4 → land col 5.
        assert_eq!(at(&b), Position::new(0, 5));
        {
            let f = folds(&b);
            move_screen_down(&mut b, &f, &v, 1, &mut sticky, 4);
        }
        assert_eq!(at(&b), Position::new(0, 9));
        // Past the last segment crosses to the next doc row.
        {
            let f = folds(&b);
            move_screen_down(&mut b, &f, &v, 1, &mut sticky, 4);
        }
        assert_eq!(at(&b), Position::new(1, 0));
    }

    #[test]
    fn screen_up_walks_within_wrapped_row() {
        let mut b = View::from_str("aaaabbbbcccc");
        let v = make_wrap_viewport(Wrap::Char, 4);
        b.set_cursor(Position::new(0, 9));
        let mut sticky = None;
        {
            let f = folds(&b);
            move_screen_up(&mut b, &f, &v, 1, &mut sticky, 4);
        }
        // visual_col = 9 - 8 = 1 → previous segment col = 4 + 1 = 5.
        assert_eq!(at(&b), Position::new(0, 5));
        {
            let f = folds(&b);
            move_screen_up(&mut b, &f, &v, 1, &mut sticky, 4);
        }
        assert_eq!(at(&b), Position::new(0, 1));
        // Already on first segment of first row — no further move.
        {
            let f = folds(&b);
            move_screen_up(&mut b, &f, &v, 1, &mut sticky, 4);
        }
        assert_eq!(at(&b), Position::new(0, 1));
    }

    #[test]
    fn screen_down_clamps_to_short_segment() {
        // First row wraps into a 6-char then a 2-char segment; second
        // row is only 1 char. Visual col 4 should clamp to row 1's
        // last col (0) when crossing into the short row.
        let mut b = View::from_str("aaaaaabb\nx");
        let v = make_wrap_viewport(Wrap::Char, 6);
        b.set_cursor(Position::new(0, 4));
        let mut sticky = None;
        {
            let f = folds(&b);
            move_screen_down(&mut b, &f, &v, 1, &mut sticky, 4);
        }
        // visual_col = 4 → segment 1 is (6, 8); want=10 clamps to 7.
        assert_eq!(at(&b), Position::new(0, 7));
        {
            let f = folds(&b);
            move_screen_down(&mut b, &f, &v, 1, &mut sticky, 4);
        }
        // crosses into row 1, segment (0, 1) — clamps to col 0.
        assert_eq!(at(&b), Position::new(1, 0));
    }

    #[test]
    fn screen_down_count_compounds() {
        let mut b = View::from_str("aaaabbbbcccc");
        let v = make_wrap_viewport(Wrap::Char, 4);
        b.set_cursor(Position::new(0, 0));
        let mut sticky = None;
        {
            let f = folds(&b);
            move_screen_down(&mut b, &f, &v, 2, &mut sticky, 4);
        }
        assert_eq!(at(&b), Position::new(0, 8));
    }

    /// `gj` walks one screen segment at a time down a wrapped line. The
    /// line "abcdefghijklmnop" at text_width 4 wraps into segments
    /// (0,4), (4,8), (8,12), (12,16); from col 0 the first two steps must
    /// land on cols 4 and 8 — the wrap layout must NOT be recomputed per
    /// step (each step lands where the manual trace puts it).
    #[test]
    fn screen_down_steps_through_segments_manual_trace() {
        let mut b = View::from_str("abcdefghijklmnop");
        let v = make_wrap_viewport(Wrap::Char, 4);
        b.set_cursor(Position::new(0, 0));
        let mut sticky = None;
        {
            let f = folds(&b);
            move_screen_down(&mut b, &f, &v, 1, &mut sticky, 4);
        }
        assert_eq!(at(&b), Position::new(0, 4));
        {
            let f = folds(&b);
            move_screen_down(&mut b, &f, &v, 1, &mut sticky, 4);
        }
        assert_eq!(at(&b), Position::new(0, 8));
    }

    #[test]
    fn motions_module_compiles_against_concrete_buffer() {
        // Compile-time assertion that the engine motions module is
        // physically reachable via the concrete `hjkl_buffer::View`.
        // 0.0.40 (Patch C-δ.5) lifted the bound to `B: Cursor + Query`;
        // the canonical concrete `View` still drives motions.
        let mut b = View::from_str("hello");
        super::move_right_in_line(&mut b, 1);
        assert_eq!(b.cursor(), Position::new(0, 1));
    }

    /// Mock-buffer compile-test: a non-canonical `Cursor + Query` impl
    /// drives motions correctly. Verifies the lift is on the trait
    /// surface — not pinned to `hjkl_buffer::View` — by exercising
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

            fn line(&self, idx: u32) -> String {
                self.lines[idx as usize].clone()
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

    // ── Section motions ([[/]]/[][/][) ──────────────────────────────────────

    #[test]
    fn section_forward_finds_brace_at_col0() {
        // "a\n{\nb\n{\nc" — two `{` at col 0 on rows 1 and 3.
        let mut b = View::from_str("a\n{\nb\n{\nc");
        // ]] from row 0 → row 1.
        move_section_forward(&mut b, 1);
        assert_eq!(at(&b), Position::new(1, 0));
        // ]] again → row 3.
        move_section_forward(&mut b, 1);
        assert_eq!(at(&b), Position::new(3, 0));
        // ]] again — no more `{` → stays at row 3 (last row clamped at row 4).
        move_section_forward(&mut b, 1);
        // No `{` below row 3; clamps at last row.
        assert_eq!(at(&b).row, 4); // "c" is row 4
    }

    #[test]
    fn section_forward_count_skips_n_braces() {
        let mut b = View::from_str("a\n{\nb\n{\nc");
        // 2]] from row 0 → row 3 (second `{`).
        move_section_forward(&mut b, 2);
        assert_eq!(at(&b), Position::new(3, 0));
    }

    #[test]
    fn section_backward_finds_brace_at_col0() {
        let mut b = View::from_str("a\n{\nb\n{\nc");
        b.set_cursor(Position::new(4, 0));
        // [[ from row 4 → row 3.
        move_section_backward(&mut b, 1);
        assert_eq!(at(&b), Position::new(3, 0));
        // [[ again → row 1.
        move_section_backward(&mut b, 1);
        assert_eq!(at(&b), Position::new(1, 0));
        // [[ again — no `{` above → row 0.
        move_section_backward(&mut b, 1);
        assert_eq!(at(&b).row, 0);
    }

    #[test]
    fn section_backward_count_skips_n_braces() {
        let mut b = View::from_str("a\n{\nb\n{\nc");
        b.set_cursor(Position::new(4, 0));
        // 2[[ → row 1 (two `{` back from row 4).
        move_section_backward(&mut b, 2);
        assert_eq!(at(&b), Position::new(1, 0));
    }

    #[test]
    fn section_forward_at_bottom_clamps() {
        let mut b = View::from_str("no braces here");
        move_section_forward(&mut b, 1);
        // No `{` found; stays at last row.
        assert_eq!(at(&b).row, 0);
    }

    #[test]
    fn section_backward_at_top_clamps() {
        let mut b = View::from_str("no braces here");
        move_section_backward(&mut b, 1);
        assert_eq!(at(&b).row, 0);
    }

    #[test]
    fn section_end_forward_finds_close_brace_at_col0() {
        let mut b = View::from_str("a\n}\nb\n}\nc");
        // ][ from row 0 → row 1.
        move_section_end_forward(&mut b, 1);
        assert_eq!(at(&b), Position::new(1, 0));
        // ][ again → row 3.
        move_section_end_forward(&mut b, 1);
        assert_eq!(at(&b), Position::new(3, 0));
    }

    #[test]
    fn section_end_backward_finds_close_brace_at_col0() {
        let mut b = View::from_str("a\n}\nb\n}\nc");
        b.set_cursor(Position::new(4, 0));
        // [] from row 4 → row 3.
        move_section_end_backward(&mut b, 1);
        assert_eq!(at(&b), Position::new(3, 0));
        // [] again → row 1.
        move_section_end_backward(&mut b, 1);
        assert_eq!(at(&b), Position::new(1, 0));
    }

    // ── First-non-blank line motions (+/-/_) ────────────────────────────────

    #[test]
    fn first_non_blank_next_line_lands_on_non_blank() {
        // `+` from row 0 → row 1, first non-blank.
        let mut b = View::from_str("foo\n    bar\nbaz");
        move_first_non_blank_next_line(&mut b, 1);
        assert_eq!(at(&b), Position::new(1, 4));
    }

    #[test]
    fn first_non_blank_next_line_count() {
        // `3+` from row 0 → row 3.
        let mut b = View::from_str("a\nb\n  c\nd");
        move_first_non_blank_next_line(&mut b, 3);
        assert_eq!(at(&b), Position::new(3, 0));
    }

    #[test]
    fn first_non_blank_next_line_at_last_row_clamps() {
        let mut b = View::from_str("foo\nbar");
        b.set_cursor(Position::new(1, 0));
        move_first_non_blank_next_line(&mut b, 1);
        assert_eq!(at(&b).row, 1);
    }

    #[test]
    fn first_non_blank_prev_line_lands_on_non_blank() {
        // `-` from row 1 → row 0, first non-blank.
        let mut b = View::from_str("    hello\nworld");
        b.set_cursor(Position::new(1, 0));
        move_first_non_blank_prev_line(&mut b, 1);
        assert_eq!(at(&b), Position::new(0, 4));
    }

    #[test]
    fn first_non_blank_prev_line_count() {
        // `3-` from row 3 → row 0.
        let mut b = View::from_str("  a\nb\nc\nd");
        b.set_cursor(Position::new(3, 0));
        move_first_non_blank_prev_line(&mut b, 3);
        assert_eq!(at(&b), Position::new(0, 2));
    }

    #[test]
    fn first_non_blank_prev_line_at_row_zero_clamps() {
        let mut b = View::from_str("  foo\nbar");
        move_first_non_blank_prev_line(&mut b, 1);
        assert_eq!(at(&b).row, 0);
    }

    #[test]
    fn first_non_blank_line_count_1_stays_on_current() {
        // `_` with count=1 stays on current line, first non-blank.
        let mut b = View::from_str("    hello\nworld");
        move_first_non_blank_line(&mut b, 1);
        assert_eq!(at(&b), Position::new(0, 4));
    }

    #[test]
    fn first_non_blank_line_count_2_goes_one_down() {
        // `2_` from row 0 → row 1 (count-1=1 line down), first non-blank.
        let mut b = View::from_str("foo\n  bar\nbaz");
        move_first_non_blank_line(&mut b, 2);
        assert_eq!(at(&b), Position::new(1, 2));
    }

    #[test]
    fn first_non_blank_line_noop_when_overshooting() {
        let mut b = View::from_str("a\nb");
        // 10_ from row 0 would overshoot; vim no-ops.
        let before = at(&b);
        move_first_non_blank_line(&mut b, 10);
        assert_eq!(at(&b), before, "overshooting _ should be a no-op");
        // 2_ from row 0 → row 1 (offset=1), first non-blank.
        move_first_non_blank_line(&mut b, 2);
        assert_eq!(at(&b).row, 1);
    }
}
