//! The buffer as a flat stream of chars, and selections over it.
//!
//! Helix's grammar is defined over a rope's char index space: a document is one
//! sequence of chars in which a line ending is *just another char* the cursor can
//! sit on. Row/col is the wrong shape for that — `w` at the end of a line has to
//! step onto the `\n` and off it again — so this module gives the rest of the
//! crate a linear view without materialising one.
//!
//! # The position space
//!
//! A [`Position`] `(row, col)` with `col <= len(row)` is one char slot:
//!
//! - `col < len(row)` — the char at that column.
//! - `col == len(row)` — the row's line ending (`\n`). On an empty row that is
//!   `col == 0`, which is exactly where helix parks a cursor on a blank line.
//! - the last row has no line ending, so `col == len(last)` is EOF: past the end,
//!   and the one position [`Doc::char_at`] answers `None` for.
//!
//! [`Doc::advance`] / [`Doc::retreat`] step through that space, so `(row, col)`
//! ordered lexicographically *is* the char index — no offset table, no O(rows)
//! precompute per keystroke.
//!
//! # Units
//!
//! **Chars**, not graphemes: this is the same unit [`hjkl_buffer::Edit`] and
//! `Buffer::cursor` speak, so a position computed here can be handed straight to
//! an edit. `types::Pos` counts graphemes and must never be mixed in.

use std::cell::RefCell;

use hjkl_buffer::{Buffer, Position};
use hjkl_engine::Editor;
use hjkl_engine::types::Host;

/// A read-only char-stream view of the editor's buffer.
///
/// Caches the chars of the row it last touched: a word motion walks a handful of
/// chars on one or two rows, and re-fetching the row per char would turn every
/// keystroke into a stream of `String` allocations.
pub(crate) struct Doc<'a, H: Host> {
    ed: &'a Editor<Buffer, H>,
    rows: usize,
    cache: RefCell<Option<(usize, Vec<char>)>>,
}

impl<'a, H: Host> Doc<'a, H> {
    pub(crate) fn new(ed: &'a Editor<Buffer, H>) -> Self {
        Self {
            ed,
            rows: ed.row_count(),
            cache: RefCell::new(None),
        }
    }

    /// Number of rows in the buffer (always >= 1).
    pub(crate) fn rows(&self) -> usize {
        self.rows
    }

    /// Char length of `row`, not counting its line ending.
    pub(crate) fn len(&self, row: usize) -> usize {
        self.with_row(row, |chars| chars.len())
    }

    fn with_row<T>(&self, row: usize, f: impl FnOnce(&[char]) -> T) -> T {
        {
            let cache = self.cache.borrow();
            if let Some((r, chars)) = cache.as_ref()
                && *r == row
            {
                return f(chars);
            }
        }
        let chars: Vec<char> = self
            .ed
            .line(row)
            .map(|l| l.chars().collect())
            .unwrap_or_default();
        let out = f(&chars);
        *self.cache.borrow_mut() = Some((row, chars));
        out
    }

    /// The char at `p`, or `None` at EOF. A position on a row's line-ending slot
    /// answers `'\n'`.
    pub(crate) fn char_at(&self, p: Position) -> Option<char> {
        if p.row >= self.rows {
            return None;
        }
        let len = self.len(p.row);
        if p.col < len {
            self.with_row(p.row, |chars| chars.get(p.col).copied())
        } else if p.row + 1 < self.rows {
            Some('\n')
        } else {
            None
        }
    }

    /// EOF — one past the last char.
    pub(crate) fn eof(&self) -> Position {
        let last = self.rows - 1;
        Position::new(last, self.len(last))
    }

    /// The first position in the document.
    pub(crate) fn start(&self) -> Position {
        Position::new(0, 0)
    }

    /// One char forward. Saturates at EOF.
    pub(crate) fn advance(&self, p: Position) -> Position {
        if p.col < self.len(p.row) {
            Position::new(p.row, p.col + 1)
        } else if p.row + 1 < self.rows {
            Position::new(p.row + 1, 0)
        } else {
            p
        }
    }

    /// One char back. Saturates at the start of the document.
    pub(crate) fn retreat(&self, p: Position) -> Position {
        if p.col > 0 {
            Position::new(p.row, p.col - 1)
        } else if p.row > 0 {
            Position::new(p.row - 1, self.len(p.row - 1))
        } else {
            p
        }
    }

    /// The text covered by `[start, end]` — both ends **inclusive**, the way a
    /// helix selection is.
    pub(crate) fn text_between(&self, start: Position, end: Position) -> String {
        let mut out = String::new();
        let mut p = start;
        let stop = self.advance(end);
        while p < stop {
            match self.char_at(p) {
                Some(c) => out.push(c),
                None => break,
            }
            let next = self.advance(p);
            if next == p {
                break;
            }
            p = next;
        }
        out
    }
}

/// A bidirectional char cursor, modelled on `ropey::iter::Chars`.
///
/// The cursor sits in the *gap* between two chars: `next` returns the char after
/// the gap and steps over it, `prev` returns the char before the gap and steps
/// back. [`Chars::reverse`] swaps the two, which is how helix runs one word-motion
/// routine in both directions — and the reason this is a faithful port rather
/// than a re-derivation.
pub(crate) struct Chars<'d, 'a, H: Host> {
    doc: &'d Doc<'a, H>,
    at: Position,
    reversed: bool,
}

impl<'d, 'a, H: Host> Chars<'d, 'a, H> {
    pub(crate) fn at(doc: &'d Doc<'a, H>, at: Position) -> Self {
        Self {
            doc,
            at,
            reversed: false,
        }
    }

    pub(crate) fn reverse(&mut self) {
        self.reversed = !self.reversed;
    }

    #[allow(clippy::should_implement_trait)] // Mirrors ropey's Chars, not Iterator.
    pub(crate) fn next(&mut self) -> Option<char> {
        if self.reversed {
            self.step_back()
        } else {
            self.step_fwd()
        }
    }

    pub(crate) fn prev(&mut self) -> Option<char> {
        if self.reversed {
            self.step_fwd()
        } else {
            self.step_back()
        }
    }

    fn step_fwd(&mut self) -> Option<char> {
        let c = self.doc.char_at(self.at)?;
        self.at = self.doc.advance(self.at);
        Some(c)
    }

    fn step_back(&mut self) -> Option<char> {
        if self.at == self.doc.start() {
            return None;
        }
        self.at = self.doc.retreat(self.at);
        self.doc.char_at(self.at)
    }
}

// ── Helix ranges ────────────────────────────────────────────────────────────

/// A helix `Range`: `head` is **exclusive** when it is ahead of `anchor` — the
/// block cursor renders on the char *before* it.
///
/// Only the word-motion port speaks this; everything else in the crate uses
/// [`hjkl_engine::Sel`], whose ends are both inclusive. [`HxRange::to_sel`] and
/// [`HxRange::from_sel`] are the only bridge, so the two conventions cannot leak
/// into each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HxRange {
    pub anchor: Position,
    pub head: Position,
}

impl HxRange {
    pub(crate) fn new(anchor: Position, head: Position) -> Self {
        Self { anchor, head }
    }

    /// Where the block cursor sits: the char before an exclusive head.
    pub(crate) fn cursor<H: Host>(&self, doc: &Doc<'_, H>) -> Position {
        if self.head > self.anchor {
            doc.retreat(self.head)
        } else {
            self.head
        }
    }

    /// To the engine's inclusive-both-ends selection.
    pub(crate) fn to_sel<H: Host>(self, doc: &Doc<'_, H>) -> hjkl_engine::Sel {
        if self.head > self.anchor {
            hjkl_engine::Sel::new(self.anchor, doc.retreat(self.head))
        } else if self.head < self.anchor {
            hjkl_engine::Sel::new(doc.retreat(self.anchor), self.head)
        } else {
            hjkl_engine::Sel::caret(self.head)
        }
    }

    /// From the engine's inclusive-both-ends selection.
    pub(crate) fn from_sel<H: Host>(sel: hjkl_engine::Sel, doc: &Doc<'_, H>) -> Self {
        if sel.head >= sel.anchor {
            Self::new(sel.anchor, doc.advance(sel.head))
        } else {
            Self::new(doc.advance(sel.anchor), sel.head)
        }
    }
}
