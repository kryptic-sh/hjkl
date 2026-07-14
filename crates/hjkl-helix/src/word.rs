//! Helix word motions — a port of `helix-core`'s `movement::word_move`.
//!
//! This is the one place where "act like helix" and "reuse the engine" pull apart,
//! so it is worth being explicit about why the engine's `move_word_fwd` is *not*
//! used here.
//!
//! In vim, `w` **moves a caret**: it lands on the first char of the next word. In
//! helix, `w` **produces a selection**: from `|Basic forward`, it selects
//! `Basic ` — the word *and the whitespace that follows it* — leaving the cursor
//! on that trailing space. `e` selects `Basic` with the cursor on the `c`. That is
//! not vim's `w` with an anchor bolted on; the head lands on a different char, the
//! anchor snaps forward when the motion starts on a boundary, and both motions
//! step onto line endings as if they were ordinary chars. Wrapping the vim motion
//! would be a guess that is wrong in exactly the cases a helix user notices.
//!
//! So the algorithm is ported directly: same `reached_target` boundary table, same
//! `range_to_target` walk, same char categories. It runs over [`crate::doc::Doc`]'s
//! char space, which is the same shape as the rope index space helix works in.

use hjkl_buffer::Position;
use hjkl_engine::types::Host;

use crate::doc::{Chars, Doc, HxRange};

/// Which word boundary a motion is looking for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WordTarget {
    /// `w` — next word start (selects the word plus its trailing whitespace).
    NextWordStart,
    /// `e` — next word end (selects up to the word's last char).
    NextWordEnd,
    /// `b` — previous word start.
    PrevWordStart,
    /// `W` — like `w`, but a word is anything not whitespace.
    NextLongWordStart,
    /// `E` — like `e`, but a word is anything not whitespace.
    NextLongWordEnd,
    /// `B` — like `b`, but a word is anything not whitespace.
    PrevLongWordStart,
}

impl WordTarget {
    fn is_prev(self) -> bool {
        matches!(self, Self::PrevWordStart | Self::PrevLongWordStart)
    }
}

/// Helix's char categories. `_` counts as a word char; a line ending is its own
/// category, which is what makes `w` stop at the end of a line instead of
/// swallowing it as whitespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cat {
    Eol,
    Whitespace,
    Word,
    Punctuation,
}

fn categorize(ch: char) -> Cat {
    if ch == '\n' || ch == '\r' {
        Cat::Eol
    } else if ch.is_whitespace() {
        Cat::Whitespace
    } else if ch.is_alphanumeric() || ch == '_' {
        Cat::Word
    } else {
        Cat::Punctuation
    }
}

fn is_line_ending(ch: char) -> bool {
    categorize(ch) == Cat::Eol
}

fn is_word_boundary(a: char, b: char) -> bool {
    categorize(a) != categorize(b)
}

/// A "long word" (vim's WORD) is delimited by whitespace only, so a
/// word↔punctuation transition is *not* a boundary.
fn is_long_word_boundary(a: char, b: char) -> bool {
    match (categorize(a), categorize(b)) {
        (Cat::Word, Cat::Punctuation) | (Cat::Punctuation, Cat::Word) => false,
        (a, b) => a != b,
    }
}

fn reached_target(target: WordTarget, prev_ch: char, next_ch: char) -> bool {
    match target {
        WordTarget::NextWordStart => {
            is_word_boundary(prev_ch, next_ch)
                && (is_line_ending(next_ch) || !next_ch.is_whitespace())
        }
        WordTarget::NextWordEnd | WordTarget::PrevWordStart => {
            is_word_boundary(prev_ch, next_ch)
                && (!prev_ch.is_whitespace() || is_line_ending(next_ch))
        }
        WordTarget::NextLongWordStart => {
            is_long_word_boundary(prev_ch, next_ch)
                && (is_line_ending(next_ch) || !next_ch.is_whitespace())
        }
        WordTarget::NextLongWordEnd | WordTarget::PrevLongWordStart => {
            is_long_word_boundary(prev_ch, next_ch)
                && (!prev_ch.is_whitespace() || is_line_ending(next_ch))
        }
    }
}

/// The engine's `Sel` after `count` word motions from `sel`.
///
/// Returns the *whole* new selection: helix word motions set the anchor as well
/// as the head (that is the point of them), so this cannot be reduced to "a new
/// cursor position" the way a vim motion can.
pub(crate) fn word_move<H: Host>(
    doc: &Doc<'_, H>,
    sel: hjkl_engine::Sel,
    target: WordTarget,
    count: usize,
) -> hjkl_engine::Sel {
    let range = HxRange::from_sel(sel, doc);
    word_move_range(doc, range, target, count).to_sel(doc)
}

/// Where the cursor lands after `count` word motions — used by Select mode, which
/// keeps its own anchor and only takes the head.
pub(crate) fn word_move_cursor<H: Host>(
    doc: &Doc<'_, H>,
    sel: hjkl_engine::Sel,
    target: WordTarget,
    count: usize,
) -> Position {
    let range = HxRange::from_sel(sel, doc);
    word_move_range(doc, range, target, count).cursor(doc)
}

fn word_move_range<H: Host>(
    doc: &Doc<'_, H>,
    range: HxRange,
    target: WordTarget,
    count: usize,
) -> HxRange {
    let is_prev = target.is_prev();

    // Nothing to do at the edge of the document.
    if (is_prev && range.head == doc.start()) || (!is_prev && range.head == doc.eof()) {
        return range;
    }

    // Normalise the range so the walk starts from the block cursor, whichever way
    // the selection currently points. The incoming anchor is irrelevant to the
    // result: a helix word motion replaces it.
    let mut range = if is_prev {
        if range.anchor < range.head {
            HxRange::new(range.head, doc.retreat(range.head))
        } else {
            HxRange::new(doc.advance(range.head), range.head)
        }
    } else if range.anchor < range.head {
        HxRange::new(doc.retreat(range.head), range.head)
    } else {
        HxRange::new(range.head, doc.advance(range.head))
    };

    for _ in 0..count.max(1) {
        let next = range_to_target(doc, target, range);
        if next == range {
            break;
        }
        range = next;
    }
    range
}

/// One step of the walk. Ported from `helix-core`'s `CharHelpers::range_to_target`.
///
/// Note the anchor only moves when the head starts *on* a boundary (directly, or
/// after skipping line endings) — every other anchor decision belongs to the
/// caller.
fn range_to_target<H: Host>(doc: &Doc<'_, H>, target: WordTarget, origin: HxRange) -> HxRange {
    let is_prev = target.is_prev();
    let mut chars = Chars::at(doc, origin.head);
    if is_prev {
        chars.reverse();
    }
    let advance = |doc: &Doc<'_, H>, p: Position| {
        if is_prev {
            doc.retreat(p)
        } else {
            doc.advance(p)
        }
    };

    let mut anchor = origin.anchor;
    let mut head = origin.head;
    let mut prev_ch = {
        let ch = chars.prev();
        if ch.is_some() {
            chars.next();
        }
        ch
    };

    // Skip any initial line endings.
    while let Some(ch) = chars.next() {
        if is_line_ending(ch) {
            prev_ch = Some(ch);
            head = advance(doc, head);
        } else {
            chars.prev();
            break;
        }
    }
    if prev_ch.map(is_line_ending).unwrap_or(false) {
        anchor = head;
    }

    // Walk to the target boundary.
    let head_start = head;
    while let Some(next_ch) = chars.next() {
        if prev_ch.is_none() || reached_target(target, prev_ch.unwrap(), next_ch) {
            if head == head_start {
                anchor = head;
            } else {
                break;
            }
        }
        prev_ch = Some(next_ch);
        head = advance(doc, head);
    }

    HxRange::new(anchor, head)
}
