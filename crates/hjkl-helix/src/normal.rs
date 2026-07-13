//! Normal and Select mode.
//!
//! The difference between them is one line: after a motion, Normal collapses the
//! anchor onto the head (the selection is *replaced*), Select leaves it (the
//! selection is *extended*). Everything else is shared.

use hjkl_buffer::{Buffer, Edit, MotionKind, Position};
use hjkl_engine::Editor;
use hjkl_engine::input::{Input, Key};
use hjkl_engine::types::Host;

use crate::motion::{self, Motion};
use crate::{HelixMode, head, hx, hx_mut};

/// Run one key in Normal or Select mode. Every key is consumed — an unknown key
/// in a modal grammar is a no-op, not something to bubble to the host.
pub(crate) fn step<H: Host>(ed: &mut Editor<Buffer, H>, input: Input) -> bool {
    if input.alt {
        return true;
    }

    // ── Multi-cursor ────────────────────────────────────────────────────────
    if input.key == Key::Char('C') && !input.ctrl {
        add_cursor_below(ed);
        return true;
    }
    // Helix's `,` — collapse to a single selection.
    if input.key == Key::Char(',') && !input.ctrl {
        ed.clear_extra_cursors();
        return true;
    }

    // ── Motions ─────────────────────────────────────────────────────────────
    if let Some(m) = motion_for(input) {
        motion::apply(ed, m, 1);
        if hx(ed).mode == HelixMode::Normal {
            // Normal replaces the selection: the anchor follows the head.
            let h = head(ed);
            hx_mut(ed).anchor = h;
        }
        // Select leaves the anchor where it is, so the range grows.
        return true;
    }

    match input.key {
        Key::Esc => {
            let st = hx_mut(ed);
            st.mode = HelixMode::Normal;
            let h = head(ed);
            hx_mut(ed).anchor = h;
            ed.clear_extra_cursors();
        }
        // `v` toggles Select — helix's extend mode.
        Key::Char('v') if !input.ctrl => {
            let st = hx_mut(ed);
            st.mode = match st.mode {
                HelixMode::Select => HelixMode::Normal,
                _ => HelixMode::Select,
            };
            if hx(ed).mode == HelixMode::Select {
                let h = head(ed);
                hx_mut(ed).anchor = h;
            }
        }
        // `i` — insert at the selection start. `a` — append after the head.
        Key::Char('i') if !input.ctrl => {
            let (a, h) = selection(ed);
            let start = a.min(h);
            ed.set_cursor_quiet(start.row, start.col);
            enter_insert(ed);
        }
        Key::Char('a') if !input.ctrl => {
            let (a, h) = selection(ed);
            let end = a.max(h);
            // Append lands one past the head, clamped by the engine.
            ed.set_cursor_quiet(end.row, end.col + 1);
            enter_insert(ed);
        }
        // `d` — delete the selection (helix: the selection IS the operand).
        Key::Char('d') if !input.ctrl => delete_selection(ed),
        // `x` — select the whole line, the way helix's `x` does.
        Key::Char('x') if !input.ctrl => select_line(ed),
        _ => {}
    }
    true
}

/// The primary selection as `(anchor, head)`.
fn selection<H: Host>(ed: &Editor<Buffer, H>) -> (Position, Position) {
    (hx(ed).anchor, head(ed))
}

fn enter_insert<H: Host>(ed: &mut Editor<Buffer, H>) {
    hx_mut(ed).mode = HelixMode::Insert;
    let h = head(ed);
    hx_mut(ed).anchor = h;
}

/// Delete the primary selection, and the char under every secondary cursor.
///
/// The secondaries are bare carets in this scaffold (no anchors of their own —
/// see the crate docs), so they delete one char each. That is enough to prove
/// the fan-out works; ranged secondary selections are the next slice.
fn delete_selection<H: Host>(ed: &mut Editor<Buffer, H>) {
    let (a, h) = selection(ed);
    ed.push_undo();

    if a == h && ed.extra_cursors().is_empty() {
        // A bare caret deletes the char under it (helix's `d` on a 1-wide
        // selection), not nothing.
        ed.mutate_edit(Edit::DeleteRange {
            start: h,
            end: Position::new(h.row, h.col + 1),
            kind: MotionKind::Char,
        });
        collapse(ed);
        return;
    }

    if a == h {
        // Multi-caret, no extent: one char under each.
        ed.edit_at_all_cursors(|at| Edit::DeleteRange {
            start: at,
            end: Position::new(at.row, at.col + 1),
            kind: MotionKind::Char,
        });
        collapse(ed);
        return;
    }

    // Ranged primary selection. Helix's selection is inclusive of the head, so
    // the exclusive-end `DeleteRange` needs one more column.
    let (start, end) = if a <= h { (a, h) } else { (h, a) };
    ed.mutate_edit(Edit::DeleteRange {
        start,
        end: Position::new(end.row, end.col + 1),
        kind: MotionKind::Char,
    });
    ed.set_cursor_quiet(start.row, start.col);
    collapse(ed);
}

/// Select the current line: anchor at col 0, head at the last char.
fn select_line<H: Host>(ed: &mut Editor<Buffer, H>) {
    let h = head(ed);
    let len = ed.line(h.row).map(|l| l.chars().count()).unwrap_or(0);
    hx_mut(ed).anchor = Position::new(h.row, 0);
    let last = len.saturating_sub(1);
    ed.set_cursor_quiet(h.row, last);
    hx_mut(ed).mode = HelixMode::Select;
}

/// Helix's `C` — put another cursor on the next line, same column.
///
/// This is the grammar side of #63: the engine holds the extra caret and keeps
/// it correct across every edit; the discipline only decides *where* to put it.
fn add_cursor_below<H: Host>(ed: &mut Editor<Buffer, H>) {
    // The lowest existing caret is the one to extend from, so repeated `C`
    // walks down the file instead of piling up on row + 1.
    let lowest = ed
        .extra_cursors()
        .iter()
        .copied()
        .chain(std::iter::once(head(ed)))
        .max_by_key(|p| (p.row, p.col))
        .unwrap_or_else(|| head(ed));

    let next_row = lowest.row + 1;
    let Some(line) = ed.line(next_row) else {
        return; // No line below — nothing to add. Helix does the same.
    };
    // Clamp to the next line's length: a short line gets a caret at its end
    // rather than a column that does not exist.
    let col = lowest.col.min(line.chars().count());
    ed.add_cursor(Position::new(next_row, col));
}

/// Collapse the primary selection onto the cursor.
fn collapse<H: Host>(ed: &mut Editor<Buffer, H>) {
    let h = head(ed);
    hx_mut(ed).anchor = h;
    hx_mut(ed).mode = HelixMode::Normal;
}

/// Map a key to a motion, or `None` if it is not one.
fn motion_for(input: Input) -> Option<Motion> {
    if input.ctrl {
        return None;
    }
    Some(match input.key {
        Key::Char('h') | Key::Left => Motion::Left,
        Key::Char('l') | Key::Right => Motion::Right,
        Key::Char('k') | Key::Up => Motion::Up,
        Key::Char('j') | Key::Down => Motion::Down,
        Key::Char('w') => Motion::WordForward,
        Key::Char('b') => Motion::WordBack,
        Key::Char('e') => Motion::WordEnd,
        Key::Home => Motion::LineStart,
        Key::End => Motion::LineEnd,
        _ => return None,
    })
}
