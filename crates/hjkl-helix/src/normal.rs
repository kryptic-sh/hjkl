//! Normal and Select mode — the keymap.
//!
//! The difference between the two modes is one flag: after a motion, Normal lets
//! the motion decide the anchor (a point motion collapses the selection, a word
//! motion sets both ends), while Select keeps the existing anchor so the range
//! grows. Everything else is shared, which is why they are one function.

use hjkl_buffer::{Buffer, Edit, Position};
use hjkl_engine::input::{Input, Key};
use hjkl_engine::types::Host;
use hjkl_engine::{Editor, Sel};

use crate::doc::Doc;
use crate::motion::{self, Motion};
use crate::word::WordTarget;
use crate::{HelixMode, Pending, hx, hx_mut, ops, sels, set_sels};

/// Run one key in Normal or Select mode. Every key is consumed — an unknown key
/// in a modal grammar is a no-op, not something to bubble to the host.
pub(crate) fn step<H: Host>(ed: &mut Editor<Buffer, H>, input: Input) -> bool {
    // A pending chord (`g`, `f`, `r`, …) owns the next key outright.
    if let Some(pending) = hx_mut(ed).pending.take() {
        resolve_pending(ed, pending, input);
        return true;
    }

    if input.ctrl {
        return true;
    }

    // `Alt-C` — add a cursor above. The only Alt key bound so far; the rest are
    // swallowed rather than mistaken for their unmodified twins.
    if input.alt {
        if input.key == Key::Char('C') || (input.key == Key::Char('c') && input.shift) {
            add_cursor(ed, false);
        }
        return true;
    }

    // Counts: `3w`, `5j`. `0` is only ever a digit here — helix has no `0` motion,
    // it uses `gh` for the line start — but a leading `0` would still be a no-op
    // count, so it is only taken once a count is under way.
    if let Key::Char(c) = input.key
        && let Some(d) = c.to_digit(10)
        && (d != 0 || hx(ed).count > 0)
    {
        let st = hx_mut(ed);
        st.count = st.count.saturating_mul(10).saturating_add(d as usize);
        return true;
    }

    // Chord openers, BEFORE the count is consumed: `2f.` and `3gg` are counted
    // chords, so the count has to survive until the chord's second key arrives.
    let chord = match input.key {
        Key::Char('g') => Some(Pending::Goto),
        Key::Char('f') => Some(Pending::Find {
            till: false,
            fwd: true,
        }),
        Key::Char('t') => Some(Pending::Find {
            till: true,
            fwd: true,
        }),
        Key::Char('F') => Some(Pending::Find {
            till: false,
            fwd: false,
        }),
        Key::Char('T') => Some(Pending::Find {
            till: true,
            fwd: false,
        }),
        Key::Char('r') => Some(Pending::Replace),
        _ => None,
    };
    if let Some(chord) = chord {
        hx_mut(ed).pending = Some(chord);
        return true;
    }

    let count = hx(ed).count.max(1);
    let counted = hx(ed).count > 0;
    let extend = hx(ed).mode == HelixMode::Select;
    hx_mut(ed).count = 0;

    if let Some(m) = motion_for(input) {
        motion::apply(ed, m, count, extend);
        return true;
    }

    match input.key {
        Key::Esc => {
            ed.clear_extra_cursors();
            collapse(ed);
        }
        // `v` toggles Select — helix's extend mode.
        Key::Char('v') => {
            let st = hx_mut(ed);
            st.mode = match st.mode {
                HelixMode::Select => HelixMode::Normal,
                _ => HelixMode::Select,
            };
        }
        // ── Selection shape ─────────────────────────────────────────────────
        Key::Char('x') => {
            let doc = Doc::new(ed);
            let moved: Vec<Sel> = sels(ed)
                .iter()
                .map(|s| motion::extend_line_below(&doc, *s, count))
                .collect();
            drop(doc);
            set_sels(ed, &moved);
        }
        Key::Char('X') => {
            let doc = Doc::new(ed);
            let moved: Vec<Sel> = sels(ed)
                .iter()
                .map(|s| motion::extend_to_line_bounds(&doc, *s))
                .collect();
            drop(doc);
            set_sels(ed, &moved);
        }
        // `G` — goto line N, or the last line when there is no count.
        Key::Char('G') => {
            let m = if counted {
                Motion::GotoLine(count)
            } else {
                Motion::FileEnd
            };
            motion::apply(ed, m, 1, extend);
        }
        Key::Char('%') => {
            let doc = Doc::new(ed);
            let all = motion::select_all(&doc);
            drop(doc);
            ed.clear_extra_cursors();
            set_sels(ed, &[all]);
        }
        // `;` collapses each selection onto its cursor; `,` throws the extra
        // selections away. Two different kinds of "shrink" — helix binds both.
        Key::Char(';') => ops::collapse_to_heads(ed),
        Key::Char(',') => ed.clear_extra_cursors(),
        // ── Multi-cursor ────────────────────────────────────────────────────
        Key::Char('C') => add_cursor(ed, true),
        Key::Char(')') => rotate_primary(ed, 1),
        Key::Char('(') => rotate_primary(ed, -1),
        // ── Insert ──────────────────────────────────────────────────────────
        Key::Char('i') => enter_insert_at(ed, InsertAt::SelectionStart),
        Key::Char('a') => enter_insert_at(ed, InsertAt::AfterSelection),
        Key::Char('I') => enter_insert_at(ed, InsertAt::FirstNonBlank),
        Key::Char('A') => enter_insert_at(ed, InsertAt::LineEnd),
        Key::Char('o') => open_line(ed, true),
        Key::Char('O') => open_line(ed, false),
        // ── Operators ───────────────────────────────────────────────────────
        Key::Char('d') => ops::delete(ed),
        Key::Char('c') => ops::change(ed),
        Key::Char('y') => ops::yank(ed),
        Key::Char('p') => ops::paste(ed, true),
        Key::Char('P') => ops::paste(ed, false),
        Key::Char('~') => ops::switch_case(ed),
        Key::Char('J') => ops::join(ed),
        Key::Char('>') => ops::indent(ed, true),
        Key::Char('<') => ops::indent(ed, false),
        // ── History ─────────────────────────────────────────────────────────
        Key::Char('u') => history(ed, true, count),
        Key::Char('U') => history(ed, false, count),
        _ => {}
    }
    true
}

/// The second key of a chord.
fn resolve_pending<H: Host>(ed: &mut Editor<Buffer, H>, pending: Pending, input: Input) {
    let count = hx(ed).count.max(1);
    let counted = hx(ed).count > 0;
    let extend = hx(ed).mode == HelixMode::Select;
    hx_mut(ed).count = 0;

    if input.key == Key::Esc {
        return; // Chord abandoned.
    }

    match pending {
        Pending::Goto => {
            let m = match input.key {
                // `gg` with a count goes to that line — helix's only counted goto.
                Key::Char('g') if counted => Motion::GotoLine(count),
                Key::Char('g') => Motion::FileStart,
                Key::Char('e') => Motion::FileEnd,
                Key::Char('h') => Motion::LineStart,
                Key::Char('l') => Motion::LineEnd,
                Key::Char('s') => Motion::FirstNonBlank,
                _ => return,
            };
            motion::apply(ed, m, 1, extend);
        }
        Pending::Find { till, fwd } => {
            let Key::Char(ch) = input.key else { return };
            hx_mut(ed).last_find = Some((ch, till, fwd));
            motion::apply(ed, Motion::Find { ch, till, fwd }, count, extend);
        }
        Pending::Replace => {
            let Key::Char(ch) = input.key else { return };
            ops::replace_char(ed, ch);
        }
    }
}

/// Where `i` / `a` / `I` / `A` put the caret.
enum InsertAt {
    SelectionStart,
    AfterSelection,
    FirstNonBlank,
    LineEnd,
}

/// Enter insert mode at every selection, not just the primary — otherwise typing
/// with three cursors would only ever edit one of them.
fn enter_insert_at<H: Host>(ed: &mut Editor<Buffer, H>, at: InsertAt) {
    let doc = Doc::new(ed);
    let carets: Vec<Sel> = sels(ed)
        .iter()
        .map(|s| {
            let p = match at {
                InsertAt::SelectionStart => s.start(),
                // One past the end — insert mode is allowed to sit at end-of-line,
                // which is exactly what appending after the last char means.
                InsertAt::AfterSelection => {
                    let e = s.end();
                    Position::new(e.row, (e.col + 1).min(doc.len(e.row)))
                }
                InsertAt::FirstNonBlank => {
                    let row = s.head.row;
                    let col = (0..doc.len(row))
                        .find(|c| {
                            doc.char_at(Position::new(row, *c))
                                .map(|ch| !ch.is_whitespace())
                                .unwrap_or(false)
                        })
                        .unwrap_or(0);
                    Position::new(row, col)
                }
                InsertAt::LineEnd => Position::new(s.head.row, doc.len(s.head.row)),
            };
            Sel::caret(p)
        })
        .collect();
    drop(doc);
    set_sels(ed, &carets);
    hx_mut(ed).mode = HelixMode::Insert;
}

/// `o` / `O` — open a blank line below / above every selection and start typing.
fn open_line<H: Host>(ed: &mut Editor<Buffer, H>, below: bool) {
    let doc = Doc::new(ed);
    // `o` appends a newline at the end of the row (the caret lands on the fresh
    // row below); `O` inserts one at column 0, which pushes the row down — so the
    // caret has to be pulled back up onto the blank row it just made.
    let ats: Vec<(Sel, Position)> = sels(ed)
        .into_iter()
        .map(|s| {
            let row = if below { s.end().row } else { s.start().row };
            let at = if below {
                Position::new(row, doc.len(row))
            } else {
                Position::new(row, 0)
            };
            (s, at)
        })
        .collect();
    drop(doc);

    let anchor = hx(ed).anchor;
    ed.push_undo();
    ed.edit_at_all_selections(anchor, |s| {
        let at = ats
            .iter()
            .find(|(sel, _)| *sel == s)
            .map(|(_, at)| *at)
            .unwrap_or(s.head);
        Edit::InsertStr {
            at,
            text: "\n".to_string(),
        }
    });

    if !below {
        // Every caret landed on the pushed-down original row; the blank row is the
        // one above it.
        let lifted: Vec<Sel> = sels(ed)
            .iter()
            .map(|s| Sel::caret(Position::new(s.head.row.saturating_sub(1), 0)))
            .collect();
        set_sels(ed, &lifted);
    }
    hx_mut(ed).mode = HelixMode::Insert;
}

/// `u` / `U` — undo / redo. The engine drops the secondary selections across a
/// history jump (they were computed against a document that no longer exists), so
/// the discipline only has to re-seat its own anchor.
fn history<H: Host>(ed: &mut Editor<Buffer, H>, undo: bool, count: usize) {
    for _ in 0..count {
        if undo {
            ed.undo();
        } else {
            ed.redo();
        }
    }
    collapse(ed);
}

/// `C` / `Alt-C` — put another cursor on the next / previous line, same column.
fn add_cursor<H: Host>(ed: &mut Editor<Buffer, H>, below: bool) {
    let all = sels(ed);
    // Extend from the outermost caret so repeated presses walk the file instead of
    // piling up on the same neighbouring row.
    let from = if below {
        all.iter().map(|s| s.head).max_by_key(|p| (p.row, p.col))
    } else {
        all.iter().map(|s| s.head).min_by_key(|p| (p.row, p.col))
    };
    let Some(from) = from else { return };

    let row = if below {
        from.row + 1
    } else {
        match from.row.checked_sub(1) {
            Some(r) => r,
            None => return, // Already on the first line.
        }
    };
    let Some(line) = ed.line(row) else {
        return; // No line there — helix does the same and adds nothing.
    };
    // Clamp to the target line's length: a short line gets a caret at its end
    // rather than a column that does not exist.
    let col = from.col.min(line.chars().count());
    ed.add_cursor(Position::new(row, col));
}

/// `)` / `(` — make the next / previous selection the primary one.
///
/// Only the primary is visible in the status line and only the primary is what
/// `;` keeps, so rotating is how a user picks which caret leads.
fn rotate_primary<H: Host>(ed: &mut Editor<Buffer, H>, delta: isize) {
    let mut all = sels(ed);
    if all.len() < 2 {
        return;
    }
    let primary = all[0];
    all.sort_by_key(|s| (s.head.row, s.head.col));
    let idx = all.iter().position(|s| *s == primary).unwrap_or(0) as isize;
    let n = all.len() as isize;
    let next = ((idx + delta) % n + n) % n;

    let new_primary = all[next as usize];
    let rest: Vec<Sel> = all
        .into_iter()
        .enumerate()
        .filter(|(i, _)| *i != next as usize)
        .map(|(_, s)| s)
        .collect();

    let mut ordered = vec![new_primary];
    ordered.extend(rest);
    set_sels(ed, &ordered);
}

/// Collapse the primary selection onto the cursor and drop back to Normal.
fn collapse<H: Host>(ed: &mut Editor<Buffer, H>) {
    let (row, col) = ed.cursor();
    let st = hx_mut(ed);
    st.anchor = Position::new(row, col);
    st.mode = HelixMode::Normal;
    st.count = 0;
    st.pending = None;
}

/// Map a key to a motion, or `None` if it is not one.
fn motion_for(input: Input) -> Option<Motion> {
    Some(match input.key {
        Key::Char('h') | Key::Left => Motion::Left,
        Key::Char('l') | Key::Right => Motion::Right,
        Key::Char('k') | Key::Up => Motion::Up,
        Key::Char('j') | Key::Down => Motion::Down,
        Key::Char('w') => Motion::Word(WordTarget::NextWordStart),
        Key::Char('b') => Motion::Word(WordTarget::PrevWordStart),
        Key::Char('e') => Motion::Word(WordTarget::NextWordEnd),
        Key::Char('W') => Motion::Word(WordTarget::NextLongWordStart),
        Key::Char('B') => Motion::Word(WordTarget::PrevLongWordStart),
        Key::Char('E') => Motion::Word(WordTarget::NextLongWordEnd),
        Key::Home => Motion::LineStart,
        Key::End => Motion::LineEnd,
        _ => return None,
    })
}
