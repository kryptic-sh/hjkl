//! Motions.
//!
//! Every motion runs at **every** selection — that is what a helix user expects:
//! with three cursors down a file, `w` selects a word at each of them. The primary
//! is not special-cased.
//!
//! # Two kinds of motion
//!
//! - **Point** motions (`h j k l`, goto-line, line start/end) put the cursor
//!   somewhere. In Normal mode the selection collapses onto it; in Select mode the
//!   anchor stays and the range grows.
//! - **Range** motions (`w b e W B E`, `f t F T`) produce a *selection*: they set
//!   the anchor themselves. `w` selects the word plus its trailing whitespace;
//!   `f x` selects from the cursor through the `x`. In Select mode they keep the
//!   existing anchor and only take the new head.
//!
//! Getting that split wrong is the single most "this isn't helix" thing a port can
//! do, so it is encoded in [`Motion::is_range`] rather than left to each call site.

use hjkl_buffer::{Buffer, Position};
use hjkl_engine::types::Host;
use hjkl_engine::{Editor, Sel, SnapshotFoldProvider, motions};

use crate::doc::Doc;
use crate::word::{self, WordTarget};

/// The motions the discipline understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    /// `w b e W B E` — a helix word motion; sets the anchor as well as the head.
    Word(WordTarget),
    /// `gh` — the first column.
    LineStart,
    /// `gl` — the last char of the line.
    LineEnd,
    /// `gs` — the first non-whitespace char of the line.
    FirstNonBlank,
    /// `gg` (no count) — the start of the file.
    FileStart,
    /// `ge` — the last char of the file.
    FileEnd,
    /// `gg` with a count, `G` — line `n`, 1-based.
    GotoLine(usize),
    /// `f t F T` — to the `count`-th `ch` on this line. `till` stops one char
    /// short; `fwd` searches right.
    Find {
        ch: char,
        till: bool,
        fwd: bool,
    },
}

impl Motion {
    /// True when the motion sets the anchor itself (helix calls these "selecting"
    /// motions). See the module docs.
    fn is_range(self) -> bool {
        matches!(self, Motion::Word(_) | Motion::Find { .. })
    }
}

/// Move every selection by `count`. `extend` is Select mode: keep the anchors.
pub(crate) fn apply<H: Host>(ed: &mut Editor<Buffer, H>, m: Motion, count: usize, extend: bool) {
    let count = count.max(1);
    let sels = crate::sels(ed);

    let moved: Vec<Sel> = match m {
        Motion::Up | Motion::Down => vertical(ed, &sels, m, count, extend),
        _ => {
            let doc = Doc::new(ed);
            sels.iter()
                .map(|s| horizontal(&doc, *s, m, count, extend))
                .collect()
        }
    };

    crate::set_sels(ed, &moved);
    // A horizontal motion clears the sticky column, or a later `j` would drag the
    // old target column along; the vertical ones own it and set it themselves.
    if !matches!(m, Motion::Up | Motion::Down) {
        ed.set_sticky_col(None);
    }
    ed.push_buffer_cursor_to_textarea();
    ed.ensure_cursor_in_scrolloff();
}

/// `j` / `k`, through the engine's own vertical motion so folds and the sticky
/// column behave exactly as they do under vim.
///
/// The sticky column is a single editor-wide value, so only the **primary**
/// commits one — the secondaries borrow it read-only. A per-caret sticky column
/// would need engine state that does not exist yet, and this is the honest
/// approximation: the primary's column is the one the user is steering.
fn vertical<H: Host>(
    ed: &mut Editor<Buffer, H>,
    sels: &[Sel],
    m: Motion,
    count: usize,
    extend: bool,
) -> Vec<Sel> {
    let folds = SnapshotFoldProvider::from_buffer(ed.buffer());
    let base_sticky = ed.sticky_col();
    let mut primary_sticky = base_sticky;
    let mut out = Vec::with_capacity(sels.len());

    for (i, s) in sels.iter().enumerate() {
        let mut sticky = base_sticky;
        ed.set_cursor_quiet(s.head.row, s.head.col);
        match m {
            Motion::Up => motions::move_up(ed.buffer_mut(), &folds, count, &mut sticky),
            Motion::Down => motions::move_down(ed.buffer_mut(), &folds, count, &mut sticky),
            _ => unreachable!("vertical() only handles Up / Down"),
        }
        let (row, col) = ed.cursor();
        let head = Position::new(row, col);
        if i == 0 {
            primary_sticky = sticky;
        }
        out.push(if extend {
            Sel::new(s.anchor, head)
        } else {
            Sel::caret(head)
        });
    }

    ed.set_sticky_col(primary_sticky);
    out
}

/// Everything that is not `j` / `k`: computed against the char stream, so a
/// motion behaves the same at the primary and at every secondary.
fn horizontal<H: Host>(doc: &Doc<'_, H>, s: Sel, m: Motion, count: usize, extend: bool) -> Sel {
    // Range motions set their own anchor (Normal) or keep the old one (Select).
    if let Motion::Word(target) = m {
        return if extend {
            Sel::new(s.anchor, word::word_move_cursor(doc, s, target, count))
        } else {
            word::word_move(doc, s, target, count)
        };
    }

    let head = match m {
        Motion::Left => Position::new(s.head.row, s.head.col.saturating_sub(count)),
        Motion::Right => {
            let last = last_col(doc, s.head.row);
            Position::new(s.head.row, (s.head.col + count).min(last))
        }
        Motion::LineStart => Position::new(s.head.row, 0),
        Motion::LineEnd => Position::new(s.head.row, last_col(doc, s.head.row)),
        Motion::FirstNonBlank => Position::new(s.head.row, first_non_blank(doc, s.head.row)),
        Motion::FileStart => doc.start(),
        Motion::FileEnd => last_char(doc),
        Motion::GotoLine(n) => {
            let row = n.saturating_sub(1).min(doc.rows() - 1);
            Position::new(row, 0)
        }
        Motion::Find { ch, till, fwd } => match find_char(doc, s.head, ch, till, fwd, count) {
            Some(p) => p,
            // No match: helix leaves the selection exactly as it was.
            None => return s,
        },
        Motion::Word(_) | Motion::Up | Motion::Down => unreachable!("handled above"),
    };

    if extend {
        Sel::new(s.anchor, head)
    } else if m.is_range() {
        // `f x` in Normal selects from where the cursor was through the match.
        Sel::new(s.head, head)
    } else {
        Sel::caret(head)
    }
}

/// The last column a cursor may sit on: the last char, or column 0 on an empty
/// row (where the cursor sits on the line ending).
fn last_col<H: Host>(doc: &Doc<'_, H>, row: usize) -> usize {
    doc.len(row).saturating_sub(1)
}

fn first_non_blank<H: Host>(doc: &Doc<'_, H>, row: usize) -> usize {
    let len = doc.len(row);
    for col in 0..len {
        match doc.char_at(Position::new(row, col)) {
            Some(c) if !c.is_whitespace() => return col,
            _ => {}
        }
    }
    last_col(doc, row)
}

/// The last char in the document.
fn last_char<H: Host>(doc: &Doc<'_, H>) -> Position {
    let eof = doc.eof();
    if eof == doc.start() {
        eof
    } else {
        doc.retreat(eof)
    }
}

/// `f` / `t` / `F` / `T`. Searches **within the row**, like helix's default.
fn find_char<H: Host>(
    doc: &Doc<'_, H>,
    from: Position,
    ch: char,
    till: bool,
    fwd: bool,
    count: usize,
) -> Option<Position> {
    let row = from.row;
    let len = doc.len(row);
    let mut found = 0usize;

    if fwd {
        for col in (from.col + 1)..len {
            if doc.char_at(Position::new(row, col)) == Some(ch) {
                found += 1;
                if found == count {
                    let col = if till { col.saturating_sub(1) } else { col };
                    return Some(Position::new(row, col));
                }
            }
        }
    } else {
        for col in (0..from.col).rev() {
            if doc.char_at(Position::new(row, col)) == Some(ch) {
                found += 1;
                if found == count {
                    let col = if till { (col + 1).min(len) } else { col };
                    return Some(Position::new(row, col));
                }
            }
        }
    }
    None
}

// ── Line selections (`x` / `X`) ─────────────────────────────────────────────

/// `x` — select the whole line. Pressing it again extends one line down, which is
/// how helix grows a line selection.
///
/// The head lands on the line **ending**, not the last char, so a following `d`
/// removes the row rather than leaving an empty one behind.
pub(crate) fn extend_line_below<H: Host>(doc: &Doc<'_, H>, s: Sel, count: usize) -> Sel {
    let start_row = s.start().row;
    let end_row = s.end().row;
    let already_whole = s.anchor == Position::new(start_row, 0)
        && s.head == Position::new(end_row, doc.len(end_row));

    let grow = if already_whole { count } else { count - 1 };
    let end_row = (end_row + grow).min(doc.rows() - 1);
    Sel::new(
        Position::new(start_row, 0),
        Position::new(end_row, doc.len(end_row)),
    )
}

/// `X` — snap the selection out to whole lines without growing it.
pub(crate) fn extend_to_line_bounds<H: Host>(doc: &Doc<'_, H>, s: Sel) -> Sel {
    let start_row = s.start().row;
    let end_row = s.end().row;
    Sel::new(
        Position::new(start_row, 0),
        Position::new(end_row, doc.len(end_row)),
    )
}

/// `%` — the whole file.
pub(crate) fn select_all<H: Host>(doc: &Doc<'_, H>) -> Sel {
    Sel::new(doc.start(), last_char(doc))
}
