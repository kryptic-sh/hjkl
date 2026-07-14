//! Operators — the keys that change text.
//!
//! In helix the selection **is** the operand: there is no operator-pending mode
//! and no motion to compose with. `d` deletes what is selected, at every
//! selection, and that "at every selection" is the whole reason the engine grew
//! [`Editor::edit_at_all_selections`]: one keystroke, N ranges, one undo step.

use hjkl_buffer::{Buffer, Edit, MotionKind, Position};
use hjkl_engine::types::Host;
use hjkl_engine::{Editor, Sel};

use crate::doc::Doc;
use crate::{HelixMode, hx, hx_mut, sels, set_sels};

/// One char past a selection's end — the exclusive end an [`Edit`] wants.
///
/// A selection whose head sits on a line ending yields the first column of the
/// next row, so deleting it takes the `\n` with it and the row really goes away
/// (helix's `x` `d` deletes the line; it does not leave a blank one).
fn exclusive_end<H: Host>(doc: &Doc<'_, H>, s: Sel) -> Position {
    doc.advance(s.end())
}

/// Pre-compute `(selection, exclusive end)` for every selection.
///
/// The exclusive end needs the buffer (line lengths), but the fan-out closure runs
/// while the editor is mutably borrowed — so the geometry is resolved up front and
/// looked up by selection inside the closure.
fn ends<H: Host>(ed: &Editor<Buffer, H>) -> Vec<(Sel, Position)> {
    let doc = Doc::new(ed);
    sels(ed)
        .into_iter()
        .map(|s| (s, exclusive_end(&doc, s)))
        .collect()
}

fn end_of(pairs: &[(Sel, Position)], s: Sel) -> Position {
    pairs
        .iter()
        .find(|(sel, _)| *sel == s)
        .map(|(_, e)| *e)
        // Unreachable: the closure only ever sees selections that were in the set.
        // Falling back to the head keeps a 1-char edit rather than panicking in a
        // user's editor.
        .unwrap_or(s.head)
}

/// The text under every selection, in document order, joined by newlines when
/// there is more than one.
///
/// Helix keeps one clipboard *entry per selection* and pastes them back
/// pair-wise. Our register bank is a single slot per name (it is vim's), so the
/// selections are joined. Round-tripping `y` then `p` with N cursors therefore
/// pastes the whole joined text at each cursor — a real deviation, and the
/// alternative is a per-selection register model in the engine.
fn selected_text<H: Host>(ed: &Editor<Buffer, H>) -> String {
    let doc = Doc::new(ed);
    let mut all = sels(ed);
    all.sort_by_key(|s| (s.start().row, s.start().col));
    all.iter()
        .map(|s| doc.text_between(s.start(), s.end()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `y` — yank every selection. The selections survive, as they do in helix.
pub(crate) fn yank<H: Host>(ed: &mut Editor<Buffer, H>) {
    let text = selected_text(ed);
    let linewise = text.ends_with('\n');
    ed.registers_mut().record_yank(text, linewise, None);
}

/// `d` — delete every selection. Each one collapses to a caret at its own hole.
pub(crate) fn delete<H: Host>(ed: &mut Editor<Buffer, H>) {
    let text = selected_text(ed);
    let linewise = text.ends_with('\n');
    ed.registers_mut().record_delete(text, linewise, None);

    let pairs = ends(ed);
    let anchor = hx(ed).anchor;
    ed.push_undo();
    let (_, new_anchor) = ed.edit_at_all_selections(anchor, |s| Edit::DeleteRange {
        start: s.start(),
        end: end_of(&pairs, s),
        kind: MotionKind::Char,
    });
    hx_mut(ed).anchor = new_anchor;
    hx_mut(ed).mode = HelixMode::Normal;
}

/// `c` — delete every selection and start typing at each hole.
pub(crate) fn change<H: Host>(ed: &mut Editor<Buffer, H>) {
    delete(ed);
    hx_mut(ed).mode = HelixMode::Insert;
}

/// `p` / `P` — paste the unnamed register at every selection.
///
/// Linewise text (a yank that ended on a line ending, i.e. an `x` selection) lands
/// as whole rows below (`p`) or above (`P`) the selection. Charwise text lands
/// right after the selection's end (`p`) or at its start (`P`).
pub(crate) fn paste<H: Host>(ed: &mut Editor<Buffer, H>, after: bool) {
    let Some(slot) = ed.registers().read('"').cloned() else {
        return;
    };
    if slot.text.is_empty() {
        return;
    }

    let doc = Doc::new(ed);
    let last_row = doc.rows() - 1;
    // Where each selection's paste lands, and what exactly gets inserted there.
    // Resolved up front: the fan-out closure runs while the editor is mutably
    // borrowed and cannot ask the buffer anything.
    let plan: Vec<(Sel, Position, String)> = sels(ed)
        .into_iter()
        .map(|s| {
            if !slot.linewise {
                let at = if after {
                    exclusive_end(&doc, s)
                } else {
                    s.start()
                };
                return (s, at, slot.text.clone());
            }
            if !after {
                // The text ends in a newline, so column 0 of the selection's first
                // row pushes that row down — the pasted lines land above it.
                return (s, Position::new(s.start().row, 0), slot.text.clone());
            }
            let row = s.end().row;
            if row < last_row {
                (s, Position::new(row + 1, 0), slot.text.clone())
            } else {
                // No row below to insert at: append to the end of the last row,
                // leading with the newline instead of trailing with it.
                let text = format!("\n{}", slot.text.trim_end_matches('\n'));
                (s, Position::new(row, doc.len(row)), text)
            }
        })
        .collect();
    drop(doc);

    let anchor = hx(ed).anchor;
    ed.push_undo();
    let (_, new_anchor) = ed.edit_at_all_selections(anchor, |s| {
        let (_, at, text) = plan
            .iter()
            .find(|(sel, _, _)| *sel == s)
            .expect("every selection in the fan-out was planned");
        Edit::InsertStr {
            at: *at,
            text: text.clone(),
        }
    });
    hx_mut(ed).anchor = new_anchor;
}

/// `r<char>` — overwrite every char of every selection, keeping line endings and
/// keeping the selection (helix does both).
pub(crate) fn replace_char<H: Host>(ed: &mut Editor<Buffer, H>, ch: char) {
    let doc = Doc::new(ed);
    let plan: Vec<(Sel, Position, String)> = sels(ed)
        .into_iter()
        .map(|s| {
            let end = exclusive_end(&doc, s);
            let with: String = doc
                .text_between(s.start(), s.end())
                .chars()
                .map(|c| if c == '\n' { '\n' } else { ch })
                .collect();
            (s, end, with)
        })
        .collect();
    drop(doc);
    rewrite_in_place(ed, plan);
}

/// `~` — swap the case of every char of every selection, keeping the selection.
pub(crate) fn switch_case<H: Host>(ed: &mut Editor<Buffer, H>) {
    let doc = Doc::new(ed);
    let plan: Vec<(Sel, Position, String)> = sels(ed)
        .into_iter()
        .map(|s| {
            let end = exclusive_end(&doc, s);
            let with: String = doc
                .text_between(s.start(), s.end())
                .chars()
                .flat_map(|c| {
                    if c.is_lowercase() {
                        c.to_uppercase().collect::<Vec<_>>()
                    } else if c.is_uppercase() {
                        c.to_lowercase().collect::<Vec<_>>()
                    } else {
                        vec![c]
                    }
                })
                .collect();
            (s, end, with)
        })
        .collect();
    drop(doc);
    rewrite_in_place(ed, plan);
}

/// Replace each selection's text with a same-length string, then put the
/// selections back exactly where they were.
///
/// The selections have to be re-set explicitly: `edit_at_all_selections` shifts an
/// anchor with insertion-point semantics, and an anchor sitting exactly at the
/// start of a `Replace` slides to the *end* of the replacement — correct for an
/// insert, wrong for a rewrite that is supposed to leave the selection alone. The
/// geometry is known here (same length in, same length out), so we assert it
/// rather than infer it.
fn rewrite_in_place<H: Host>(ed: &mut Editor<Buffer, H>, plan: Vec<(Sel, Position, String)>) {
    let before: Vec<Sel> = plan.iter().map(|(s, _, _)| *s).collect();
    let anchor = hx(ed).anchor;
    ed.push_undo();
    ed.edit_at_all_selections(anchor, |s| {
        let (_, end, with) = plan
            .iter()
            .find(|(sel, _, _)| *sel == s)
            .expect("every selection in the fan-out was planned");
        Edit::Replace {
            start: s.start(),
            end: *end,
            with: with.clone(),
        }
    });
    // Same length in, same length out — the selections are unchanged.
    set_sels(ed, &before);
}

/// `J` — join the lines each selection spans, as helix does: one join per
/// selection, a single space at each seam.
///
/// This is `hjkl_buffer::Edit::JoinLines`, whose contract is *not* vim's `J`: it
/// drops the `\n` and inserts a space only when both sides are non-empty, and it
/// does **not** strip the next line's leading whitespace. Helix's `J` does strip
/// it. Matching helix would mean hand-rolling the join out of a delete plus an
/// insert and losing the tracked geometry `JoinLines` gives the secondary
/// selections — so this follows the buffer, and the deviation is a known one.
pub(crate) fn join<H: Host>(ed: &mut Editor<Buffer, H>) {
    let rows = ed.row_count();
    let anchor = hx(ed).anchor;
    ed.push_undo();
    let (_, new_anchor) = ed.edit_at_all_selections(anchor, |s| {
        let spanned = s.end().row - s.start().row;
        Edit::JoinLines {
            row: s.start().row,
            // A caret joins the row below it; a multi-row selection joins every
            // seam inside it.
            count: spanned.max(1).min(rows.saturating_sub(1 + s.start().row)),
            with_space: true,
        }
    });
    hx_mut(ed).anchor = new_anchor;
    collapse_to_heads(ed);
}

/// `>` / `<` — indent or unindent every row any selection touches.
///
/// One selection can span many rows, so this cannot go through the one-edit-per-
/// selection fan-out. It walks the touched rows bottom-up instead and shifts the
/// primary selection by hand — the secondaries ride along on `mutate_edit`'s own
/// shift, which is the same machinery, just already wired up.
pub(crate) fn indent<H: Host>(ed: &mut Editor<Buffer, H>, add: bool) {
    let width = ed.settings().shiftwidth.max(1);
    let pad = " ".repeat(width);

    let mut rows: Vec<usize> = sels(ed)
        .iter()
        .flat_map(|s| s.start().row..=s.end().row)
        .collect();
    rows.sort_unstable();
    rows.dedup();
    if rows.is_empty() {
        return;
    }

    let mut primary = sels(ed)[0];
    ed.push_undo();

    // Bottom-up: an edit on a lower row cannot disturb a higher one.
    for row in rows.into_iter().rev() {
        let line = ed.line(row).unwrap_or_default();
        let edit = if add {
            if line.is_empty() {
                continue; // Helix leaves blank lines blank.
            }
            Edit::InsertStr {
                at: Position::new(row, 0),
                text: pad.clone(),
            }
        } else {
            let strip = line.chars().take(width).take_while(|c| *c == ' ').count();
            if strip == 0 {
                continue;
            }
            Edit::DeleteRange {
                start: Position::new(row, 0),
                end: Position::new(row, strip),
                kind: MotionKind::Char,
            }
        };
        // The secondaries are shifted by `mutate_edit`; the primary lives outside
        // the engine's selection set, so it is shifted here with the same function.
        let row_count = ed.row_count();
        primary = hjkl_engine::shift_sel(primary, &edit, |_| 0, row_count).unwrap_or(primary);
        ed.mutate_edit(edit);
    }

    ed.set_cursor_quiet(primary.head.row, primary.head.col);
    hx_mut(ed).anchor = primary.anchor;
}

/// Collapse every selection onto its head — helix's `;`.
pub(crate) fn collapse_to_heads<H: Host>(ed: &mut Editor<Buffer, H>) {
    let collapsed: Vec<Sel> = sels(ed).into_iter().map(|s| Sel::caret(s.head)).collect();
    set_sels(ed, &collapsed);
}
