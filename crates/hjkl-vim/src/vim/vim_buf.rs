//! The buffer as vim's text-object code walks it: [`VimBuf`] and vim's
//! cursor-stepping primitives (`inc` / `dec` / `gchar` …), shared by the
//! ported sentence and word objects.

use super::text_object::Pos;
use hjkl_engine::Editor;

/// Run `f` over the buffer as vim's text-object code sees it ([`VimBuf`]),
/// from the cursor clamped onto a real cell. `None` for a buffer without a
/// single character.
pub fn with_vim_buf<H: hjkl_engine::types::Host, T>(
    ed: &Editor<hjkl_buffer::View, H>,
    f: impl FnOnce(&VimBuf<'_>, Pos) -> T,
) -> Option<T> {
    let rope = hjkl_engine::types::Query::rope(ed.buffer());
    if rope.chars().all(|c| c == '\n') {
        return None;
    }
    let line_chars = |r: usize| -> Vec<char> {
        let start = rope.line_to_byte(r);
        rope.byte_slice(start..start + hjkl_buffer::rope_line_bytes(&rope, r))
            .chars()
            .collect()
    };
    let buf = VimBuf::new(rope.len_lines(), &line_chars);
    let row = ed.cursor().0.min(buf.n_lines - 1);
    let cursor = (row, ed.cursor().1.min(buf.len(row)));
    Some(f(&buf, cursor))
}

/// vim `ascii_iswhite`.
pub fn is_vim_white(c: char) -> bool {
    c == ' ' || c == '\t'
}

/// The buffer the way vim's text-object code sees it: lines without their
/// break, read from the rope on demand, and no phantom row after a trailing
/// newline. A position is `(row, col)` in chars; `col == len` is the NUL at
/// the end of the line.
///
/// The walk moves a character at a time, so the row it is on is cached:
/// a character read is an index, and a line is fetched once per visit.
pub struct VimBuf<'a> {
    line_chars: &'a dyn Fn(usize) -> Vec<char>,
    pub n_lines: usize,
    line: std::cell::RefCell<Option<(usize, Vec<char>)>>,
}

impl<'a> VimBuf<'a> {
    /// `raw_lines` counts the phantom row, as the rope does.
    pub fn new(raw_lines: usize, line_chars: &'a dyn Fn(usize) -> Vec<char>) -> Self {
        let phantom = raw_lines > 1 && line_chars(raw_lines - 1).is_empty();
        Self {
            line_chars,
            n_lines: raw_lines - usize::from(phantom),
            line: std::cell::RefCell::new(None),
        }
    }

    fn with_line<T>(&self, row: usize, f: impl FnOnce(&[char]) -> T) -> T {
        let mut line = self.line.borrow_mut();
        if line.as_ref().is_none_or(|(cached, _)| *cached != row) {
            *line = Some((row, (self.line_chars)(row)));
        }
        f(line.as_ref().map_or(&[], |(_, chars)| chars))
    }

    pub fn len(&self, row: usize) -> usize {
        self.with_line(row, <[char]>::len)
    }

    /// vim `gchar_pos`: the character at `p`, `None` for the NUL past the
    /// end of the line.
    pub fn gchar(&self, p: Pos) -> Option<char> {
        self.with_line(p.0, |line| line.get(p.1).copied())
    }

    /// vim `inc`: 0 moved within the line, 2 moved onto the line's NUL,
    /// 1 moved to the next line, -1 at the end of the buffer.
    pub fn inc(&self, p: &mut Pos) -> i32 {
        let len = self.len(p.0);
        if p.1 < len {
            p.1 += 1;
            return if p.1 < len { 0 } else { 2 };
        }
        if p.0 + 1 < self.n_lines {
            *p = (p.0 + 1, 0);
            return 1;
        }
        -1
    }

    /// vim `incl`: `inc`, skipping the NUL at the end of a non-empty line.
    pub fn incl(&self, p: &mut Pos) -> i32 {
        let mut r = self.inc(p);
        if r >= 1 && p.1 != 0 {
            r = self.inc(p);
        }
        r
    }

    /// vim `dec`: 0 within the line, 1 onto the previous line's NUL, -1 at
    /// the start of the buffer.
    pub fn dec(&self, p: &mut Pos) -> i32 {
        if p.1 > 0 {
            p.1 -= 1;
            return 0;
        }
        if p.0 > 0 {
            p.0 -= 1;
            p.1 = self.len(p.0);
            return 1;
        }
        -1
    }

    /// vim `decl`: `dec`, skipping the NUL at the end of a non-empty line.
    pub fn decl(&self, p: &mut Pos) -> i32 {
        let mut r = self.dec(p);
        if r == 1 && p.1 != 0 {
            r = self.dec(p);
        }
        r
    }
}
