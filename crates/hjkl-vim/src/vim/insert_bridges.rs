//! Vim FSM: insert-mode bridges.
//!
//! Split out of the monolithic `vim.rs` (#267 follow-up).

use hjkl_engine::abbrev::AbbrevTrigger;
use hjkl_engine::types::InsertDir;
use hjkl_vim_types::{InsertReason, Mode};

use super::*;
use crate::vim_state::{vim, vim_mut};
use hjkl_engine::Editor;
use hjkl_engine::buf_helpers::{
    buf_cursor_pos, buf_line, buf_line_bytes, buf_line_chars, buf_row_count, buf_set_cursor_pos,
    buf_set_cursor_rc,
};
use hjkl_engine::tag::{is_html_filetype, scan_tag_opener, sync_paired_tag_on_exit};

/// Insert a single character at the cursor. Handles replace-mode overstrike
/// (when `InsertSession::reason` is `Replace`) and smart-indent dedent of
/// closing brackets (}/)]/). Also handles autopair insertion and skip-over.
/// Returns `true`.
pub(crate) fn insert_char_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    ch: char,
) -> bool {
    use hjkl_buffer::{Edit, MotionKind, Position};
    ed.sync_buffer_content_from_textarea();
    let in_replace = matches!(
        vim(ed).insert_session.as_ref().map(|s| &s.reason),
        Some(InsertReason::Replace)
    );

    // ── Abbreviation expansion (insert mode, non-replace) ────────────────────
    // A non-keyword char typed in insert mode can trigger expansion.
    // We check BEFORE inserting the character; if an abbrev matches, we delete
    // the lhs and insert the rhs, then continue to insert `ch` as normal.
    // `<C-v>` (literal-insert) must bypass this — callers that want literal
    // insertion should NOT call this bridge; they use insert_char_literal.
    if !in_replace && !ed.abbrevs_is_empty() {
        let iskeyword = ed.settings().iskeyword.clone();
        if !is_keyword_char(ch, &iskeyword) {
            // Only non-keyword trigger chars fire abbreviation expansion.
            check_and_apply_abbrev(ed, AbbrevTrigger::NonKeyword(ch));
            // (we do NOT return early; continue to insert `ch` below)
        }
    }
    // ── Word-boundary undo break (Word granularity only; no-op for vim) ────────
    // Must fire after abbreviation expansion (the expansion may have changed the
    // cursor position) but before any actual buffer mutation so the snapshot
    // captures the pre-char state.
    maybe_word_undo_break(ed, ch);

    // Read cursor (after any abbreviation expansion that may have changed the buffer).
    let cursor = buf_cursor_pos(ed.buffer());
    let line_chars = buf_line_chars(ed.buffer(), cursor.row);

    // ── Skip-over: if the typed char matches the top of the pending-closes
    // stack AND the char currently under the cursor IS that close char,
    // pop the stack and advance the cursor instead of inserting.
    //
    // We check the actual char in the buffer (not a stored col) so that
    // characters typed between the pair don't invalidate the skip — the
    // close char shifts right as the user types inside, but the buffer
    // char check always finds it correctly.
    if !in_replace
        && !ed.pending_closes().is_empty()
        && let Some(&(pr, _pc, pch)) = ed.pending_closes().last()
        && ch == pch
        && cursor.row == pr
    {
        let char_at_cursor =
            buf_line(ed.buffer(), cursor.row).and_then(|l| l.chars().nth(cursor.col));
        if char_at_cursor == Some(ch) {
            ed.pending_closes_mut().pop();
            // For `>` skip-over in HTML/XML: also run tag autoclose.
            let filetype = ed.settings().filetype.clone();
            let autoclose_tag = ed.settings().autoclose_tag;
            if ch == '>' && autoclose_tag && is_html_filetype(&filetype) {
                // Skip past the `>` that was auto-inserted.
                let new_col = cursor.col + 1;
                buf_set_cursor_rc(ed.buffer_mut(), cursor.row, new_col);
                // Now check for tag autoclose on the line up to new_col.
                // `new_col.saturating_sub(1)` is a char index; convert to a
                // byte offset before slicing in scan_tag_opener.
                if let Some(line) = buf_line(ed.buffer(), cursor.row) {
                    let char_col = new_col.saturating_sub(1);
                    let byte_col = line
                        .char_indices()
                        .nth(char_col)
                        .map(|(b, _)| b)
                        .unwrap_or(line.len());
                    if let Some(tag) = scan_tag_opener(&line, byte_col) {
                        let close_tag = format!("</{tag}>");
                        let insert_pos = Position::new(cursor.row, new_col);
                        ed.mutate_edit(Edit::InsertStr {
                            at: insert_pos,
                            text: close_tag,
                        });
                        // Cursor stays at new_col (between > and </tag>).
                        buf_set_cursor_rc(ed.buffer_mut(), cursor.row, new_col);
                    }
                }
            } else {
                buf_set_cursor_rc(ed.buffer_mut(), cursor.row, cursor.col + 1);
            }
            ed.push_buffer_cursor_to_textarea();
            return true;
        }
    }

    if in_replace && cursor.col < line_chars {
        // Replace mode: clear pending closes (edit outside the pair).
        ed.pending_closes_mut().clear();
        ed.mutate_edit(Edit::DeleteRange {
            start: cursor,
            end: Position::new(cursor.row, cursor.col + 1),
            kind: MotionKind::Char,
        });
        ed.mutate_edit(Edit::InsertChar { at: cursor, ch });
    } else if !try_dedent_close_bracket(ed, cursor, ch) {
        // Normal insert. Check autopair first.
        let autopair = ed.settings().autopair;
        let filetype = ed.settings().filetype.clone();
        let autoclose_tag = ed.settings().autoclose_tag;

        let (prev_char, prev2_char) = {
            let line = buf_line(ed.buffer(), cursor.row).unwrap_or_default();
            let chars: Vec<char> = line.chars().collect();
            let p1 = if cursor.col > 0 {
                chars.get(cursor.col - 1).copied()
            } else {
                None
            };
            let p2 = if cursor.col > 1 {
                chars.get(cursor.col - 2).copied()
            } else {
                None
            };
            (p1, p2)
        };

        if autopair {
            if let Some(close) = autopair_close_for(ch, &filetype, prev_char, prev2_char) {
                // Insert open char.
                ed.mutate_edit(Edit::InsertChar { at: cursor, ch });
                // Insert close char immediately after the open char.
                // After inserting open at cursor, buffer cursor is at cursor.col+1.
                let after = Position::new(cursor.row, cursor.col + 1);
                ed.mutate_edit(Edit::InsertChar {
                    at: after,
                    ch: close,
                });
                // After inserting close, buffer cursor is at cursor.col+2.
                // We want cursor between open and close: cursor.col+1.
                let between_col = cursor.col + 1;
                buf_set_cursor_rc(ed.buffer_mut(), cursor.row, between_col);
                // Record the close char for skip-over. We store the row and
                // the close char; col is not tracked precisely because chars
                // typed inside the pair shift the close right. The skip-over
                // logic checks the actual buffer char at cursor instead.
                ed.pending_closes_mut()
                    .push((cursor.row, between_col, close));
                ed.push_buffer_cursor_to_textarea();
                return true;
            }

            // Tag autoclose: `>` in HTML/XML family (no prior `<` pair).
            // This fires when autopair did NOT match `>` (e.g. `>` was
            // typed directly, not via a skip-over of an auto-inserted `>`).
            if ch == '>' && autoclose_tag && is_html_filetype(&filetype) {
                ed.mutate_edit(Edit::InsertChar { at: cursor, ch });
                let new_col = cursor.col + 1;
                // scan_tag_opener looks at the line up to (new_col-1), i.e.
                // the char just inserted is at index new_col-1.
                // `new_col.saturating_sub(1)` is a char index; convert to a
                // byte offset before slicing in scan_tag_opener.
                if let Some(line) = buf_line(ed.buffer(), cursor.row) {
                    let char_col = new_col.saturating_sub(1);
                    let byte_col = line
                        .char_indices()
                        .nth(char_col)
                        .map(|(b, _)| b)
                        .unwrap_or(line.len());
                    if let Some(tag) = scan_tag_opener(&line, byte_col) {
                        let close_tag = format!("</{tag}>");
                        let insert_pos = Position::new(cursor.row, new_col);
                        ed.mutate_edit(Edit::InsertStr {
                            at: insert_pos,
                            text: close_tag,
                        });
                        // Cursor stays at new_col (between `>` and `</tag>`).
                        buf_set_cursor_rc(ed.buffer_mut(), cursor.row, new_col);
                    }
                }
                ed.push_buffer_cursor_to_textarea();
                return true;
            }
        }

        // Plain insert — do not clear the pending-closes stack here.
        // The stack is cleared on cursor motion or mode change (Esc).
        // Clearing here would prevent skip-over from firing after the
        // user types content inside an auto-paired bracket.
        ed.mutate_edit(Edit::InsertChar { at: cursor, ch });
    }
    ed.push_buffer_cursor_to_textarea();
    true
}
/// Insert a newline at the cursor, applying autoindent / smartindent and
/// optionally continuing a line comment when `formatoptions` has `r`.
/// Also handles open-pair-newline: Enter between `{|}` / `(|)` / `[|]`
/// produces an indented block with the close on its own line.
/// Returns `true`.
pub(crate) fn insert_newline_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) -> bool {
    use hjkl_buffer::Edit;
    ed.sync_buffer_content_from_textarea();

    // ── Abbreviation expansion on CR ─────────────────────────────────────────
    // CR triggers expansion for full-id / end-id / non-id abbreviations.
    // We expand BEFORE the newline is inserted; CR is then inserted as normal.
    if !ed.abbrevs_is_empty() {
        check_and_apply_abbrev(ed, AbbrevTrigger::Cr);
    }

    // ── Word-boundary undo break for newline (Word granularity only) ────────────
    // Newline always starts a new undo unit in Word mode. Fire after
    // abbreviation expansion but before any buffer mutation.
    maybe_word_undo_break(ed, '\n');

    let cursor = buf_cursor_pos(ed.buffer());
    let prev_line = buf_line(ed.buffer(), cursor.row)
        .unwrap_or_default()
        .to_string();

    // Open-pair-newline: if autopair is on and the cursor is between a
    // matching open/close bracket pair, split into two newlines so the
    // close ends up on its own dedented line.
    if ed.settings().autopair && !ed.pending_closes().is_empty() {
        // Check: char before cursor is an open bracket AND char at cursor
        // is the matching close bracket (from our pending-closes stack).
        let prev_char = if cursor.col > 0 {
            prev_line.chars().nth(cursor.col - 1)
        } else {
            None
        };
        let next_char = prev_line.chars().nth(cursor.col);
        let is_open_pair = matches!(
            (prev_char, next_char),
            (Some('{'), Some('}')) | (Some('('), Some(')')) | (Some('['), Some(']'))
        );
        if is_open_pair {
            // The pending-closes stack refers to the close char at cursor.col.
            // We clear it because the newline expansion moves the close.
            ed.pending_closes_mut().clear();
            // Compute indents: inner gets one extra unit, close gets base.
            let base_indent: String = prev_line
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .collect();
            let inner_indent = if ed.settings().expandtab {
                let unit = if ed.settings().softtabstop > 0 {
                    ed.settings().softtabstop
                } else {
                    ed.settings().shiftwidth
                };
                format!("{base_indent}{}", " ".repeat(unit))
            } else {
                format!("{base_indent}\t")
            };
            // Insert: \n<inner_indent>\n<base_indent>
            // Then cursor lands after the first \n (inside the block).
            let text = format!("\n{inner_indent}\n{base_indent}");
            ed.mutate_edit(Edit::InsertStr { at: cursor, text });
            // Move cursor to end of first new line (inner_indent line).
            let new_row = cursor.row + 1;
            let new_col = inner_indent.len();
            buf_set_cursor_rc(ed.buffer_mut(), new_row, new_col);
            ed.push_buffer_cursor_to_textarea();
            return true;
        }
    }

    // Code-fence expansion: line content is ` ``` ` (3+ backticks) followed
    // by a non-empty language tag, cursor sits at end of line → insert the
    // matching closing fence on the line below and park the cursor on a
    // blank middle line. Matches the open-pair-newline shape but for
    // markdown / doc-comment code blocks. Gated on a language tag because
    // a bare ` ``` ` could just as easily be a closing fence — we'd need
    // full document parity tracking to handle that safely, which v1
    // doesn't have.
    if ed.settings().autopair
        && let Some(fence) = detect_code_fence_opener(&prev_line, cursor.col)
    {
        ed.pending_closes_mut().clear();
        let base_indent: String = prev_line
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        let text = format!("\n{base_indent}\n{base_indent}{fence}");
        ed.mutate_edit(Edit::InsertStr { at: cursor, text });
        let new_row = cursor.row + 1;
        let new_col = base_indent.chars().count();
        buf_set_cursor_rc(ed.buffer_mut(), new_row, new_col);
        ed.push_buffer_cursor_to_textarea();
        return true;
    }

    // formatoptions `r`: continue comment on Enter in insert mode.
    let comment_cont = if ed.settings().formatoptions.contains('r') {
        continue_comment(ed.buffer(), ed.settings(), cursor.row)
    } else {
        None
    };

    // Any Enter clears the pending-closes stack (cursor moved off the pair).
    ed.pending_closes_mut().clear();

    let text = if let Some(cont) = comment_cont {
        // Comment continuation overrides autoindent: the indent is already
        // baked into the continuation prefix.
        format!("\n{cont}")
    } else {
        let indent = compute_enter_indent(ed.settings(), &prev_line);
        format!("\n{indent}")
    };
    ed.mutate_edit(Edit::InsertStr { at: cursor, text });
    ed.push_buffer_cursor_to_textarea();
    true
}
/// Insert a tab character (or spaces up to the next softtabstop boundary when
/// `expandtab` is set). Returns `true`.
pub(crate) fn insert_tab_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) -> bool {
    use hjkl_buffer::Edit;
    ed.sync_buffer_content_from_textarea();
    let cursor = buf_cursor_pos(ed.buffer());
    if ed.settings().expandtab {
        let sts = ed.settings().softtabstop;
        let n = if sts > 0 {
            sts - (cursor.col % sts)
        } else {
            ed.settings().tabstop.max(1)
        };
        ed.mutate_edit(Edit::InsertStr {
            at: cursor,
            text: " ".repeat(n),
        });
    } else {
        ed.mutate_edit(Edit::InsertChar {
            at: cursor,
            ch: '\t',
        });
    }
    ed.push_buffer_cursor_to_textarea();
    true
}
/// Delete the character before the cursor (vim Backspace / `^H`). With
/// `softtabstop` active, deletes the entire soft-tab run at an aligned
/// boundary. Joins with the previous line when at column 0.
///
/// **Comment-continuation backspace**: when the current line's entire content
/// is the auto-inserted comment prefix (e.g. `// ` with nothing after it),
/// a single Backspace removes the whole prefix in one stroke — vim parity.
///
/// Returns `true` when something was deleted, `false` at the very start of the
/// buffer.
pub(crate) fn insert_backspace_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) -> bool {
    ed.sync_buffer_content_from_textarea();
    if matches!(
        vim(ed).insert_session.as_ref().map(|s| &s.reason),
        Some(InsertReason::Replace)
    ) {
        return replace_backspace(ed);
    }
    generic_backspace(ed)
}

/// The plain-insert-mode `<BS>` body (shared by [`insert_backspace_bridge`]
/// and, as the past-the-session-start fallback, [`replace_backspace`]).
fn generic_backspace<H: hjkl_engine::types::Host>(ed: &mut Editor<hjkl_buffer::View, H>) -> bool {
    use hjkl_buffer::{Edit, MotionKind, Position};
    let cursor = buf_cursor_pos(ed.buffer());

    // Comment-continuation backspace: if the line is just the prefix (with no
    // user content after it), delete the whole prefix in one stroke.
    if cursor.col > 0 {
        let line = buf_line(ed.buffer(), cursor.row).unwrap_or_default();
        if let Some((indent, prefix)) = detect_comment_on_line(&ed.settings().filetype, &line) {
            let full_prefix = format!("{indent}{prefix}");
            // The cursor must be at the end of (or within) the prefix with no
            // additional content after — i.e. the line equals the prefix exactly.
            let line_trimmed = line.trim_end_matches(' ');
            let prefix_trimmed = full_prefix.trim_end_matches(' ');
            if line_trimmed == prefix_trimmed && cursor.col == full_prefix.chars().count() {
                // Delete everything from col 0 to cursor.
                ed.mutate_edit(Edit::DeleteRange {
                    start: Position::new(cursor.row, 0),
                    end: cursor,
                    kind: MotionKind::Char,
                });
                ed.push_buffer_cursor_to_textarea();
                return true;
            }
        }
    }

    let sts = ed.settings().softtabstop;
    if sts > 0 && cursor.col >= sts && cursor.col.is_multiple_of(sts) {
        let line = buf_line(ed.buffer(), cursor.row).unwrap_or_default();
        let chars: Vec<char> = line.chars().collect();
        let run_start = cursor.col - sts;
        if (run_start..cursor.col).all(|i| chars.get(i).copied() == Some(' ')) {
            ed.mutate_edit(Edit::DeleteRange {
                start: Position::new(cursor.row, run_start),
                end: cursor,
                kind: MotionKind::Char,
            });
            ed.push_buffer_cursor_to_textarea();
            return true;
        }
    }
    let result = if cursor.col > 0 {
        ed.mutate_edit(Edit::DeleteRange {
            start: Position::new(cursor.row, cursor.col - 1),
            end: cursor,
            kind: MotionKind::Char,
        });
        true
    } else if cursor.row > 0 {
        let prev_row = cursor.row - 1;
        let prev_chars = buf_line_chars(ed.buffer(), prev_row);
        ed.mutate_edit(Edit::JoinLines {
            row: prev_row,
            count: 1,
            with_space: false,
        });
        buf_set_cursor_rc(ed.buffer_mut(), prev_row, prev_chars);
        true
    } else {
        false
    };
    ed.push_buffer_cursor_to_textarea();
    result
}

/// Replace-mode `<BS>` (`:h Replace-mode`): restores the character that was
/// overtyped at the position one left of the cursor, rather than deleting
/// it. If that position was typed *past* the line's original end (a pure
/// append — there was nothing there to overtype), the char is deleted
/// instead, undoing the append. Backspacing before the point where this
/// Replace run started (`InsertSession::start_row` / `start_col`) just
/// moves the cursor left without touching text — vim won't undo edits
/// outside the current Replace session. Falls back to
/// [`generic_backspace`] at column 0 (BOL-join with the previous line is
/// unrelated to overtype/restore and behaves the same in both modes).
fn replace_backspace<H: hjkl_engine::types::Host>(ed: &mut Editor<hjkl_buffer::View, H>) -> bool {
    use hjkl_buffer::{Edit, MotionKind, Position};
    let cursor = buf_cursor_pos(ed.buffer());
    if cursor.col == 0 {
        return generic_backspace(ed);
    }
    let new_col = cursor.col - 1;

    let Some(session) = vim(ed).insert_session.as_ref() else {
        return generic_backspace(ed);
    };
    let start_row = session.start_row;
    let start_col = session.start_col;
    let before_rope = session.before_rope.clone(); // Arc-clone, not a byte copy.

    if cursor.row != start_row || new_col < start_col {
        // Before where this Replace run started — move left, no restore.
        buf_set_cursor_rc(ed.buffer_mut(), cursor.row, new_col);
        ed.push_buffer_cursor_to_textarea();
        return false;
    }

    let before_line = hjkl_buffer::rope_line_str(&before_rope, start_row);
    let orig_ch = before_line.chars().nth(new_col);
    ed.mutate_edit(Edit::DeleteRange {
        start: Position::new(cursor.row, new_col),
        end: cursor,
        kind: MotionKind::Char,
    });
    if let Some(ch) = orig_ch {
        // This column had a real character before the session began —
        // restore it (overtype, not delete).
        ed.mutate_edit(Edit::InsertChar {
            at: Position::new(cursor.row, new_col),
            ch,
        });
    }
    // Else: `new_col` was past the original line end (a pure append past
    // EOL) — the DeleteRange above already undid it; nothing to restore.
    buf_set_cursor_rc(ed.buffer_mut(), cursor.row, new_col);
    ed.push_buffer_cursor_to_textarea();
    true
}

/// Delete the character under the cursor (vim `Delete`). Joins with the
/// next line when at end-of-line. Returns `true` when something was deleted.
pub(crate) fn insert_delete_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) -> bool {
    use hjkl_buffer::{Edit, MotionKind, Position};
    ed.sync_buffer_content_from_textarea();
    let cursor = buf_cursor_pos(ed.buffer());
    let line_chars = buf_line_chars(ed.buffer(), cursor.row);
    let result = if cursor.col < line_chars {
        ed.mutate_edit(Edit::DeleteRange {
            start: cursor,
            end: Position::new(cursor.row, cursor.col + 1),
            kind: MotionKind::Char,
        });
        buf_set_cursor_pos(ed.buffer_mut(), cursor);
        true
    } else if cursor.row + 1 < buf_row_count(ed.buffer()) {
        ed.mutate_edit(Edit::JoinLines {
            row: cursor.row,
            count: 1,
            with_space: false,
        });
        buf_set_cursor_pos(ed.buffer_mut(), cursor);
        true
    } else {
        false
    };
    ed.push_buffer_cursor_to_textarea();
    result
}
/// Move the cursor one step in `dir`, breaking the undo group per
/// `undo_break_on_motion`. Clears the autopair pending-closes stack (cursor
/// moved off the pair). Returns `false` (no mutation).
pub(crate) fn insert_arrow_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    dir: InsertDir,
) -> bool {
    ed.sync_buffer_content_from_textarea();
    ed.pending_closes_mut().clear();
    match dir {
        InsertDir::Left => {
            hjkl_engine::motions::move_left(ed.buffer_mut(), 1);
        }
        InsertDir::Right => {
            hjkl_engine::motions::move_right_to_end(ed.buffer_mut(), 1);
        }
        InsertDir::Up => {
            let folds = hjkl_engine::SnapshotFoldProvider::from_buffer(ed.buffer());
            let mut sticky = ed.sticky_col();
            hjkl_engine::motions::move_up(ed.buffer_mut(), &folds, 1, &mut sticky);
            ed.set_sticky_col(sticky);
        }
        InsertDir::Down => {
            let folds = hjkl_engine::SnapshotFoldProvider::from_buffer(ed.buffer());
            let mut sticky = ed.sticky_col();
            hjkl_engine::motions::move_down(ed.buffer_mut(), &folds, 1, &mut sticky);
            ed.set_sticky_col(sticky);
        }
    }
    break_undo_group_in_insert(ed);
    ed.push_buffer_cursor_to_textarea();
    false
}
/// Move the cursor to the start of the current line, breaking the undo group.
/// Clears the autopair pending-closes stack. Returns `false` (no mutation).
pub(crate) fn insert_home_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) -> bool {
    ed.sync_buffer_content_from_textarea();
    ed.pending_closes_mut().clear();
    hjkl_engine::motions::move_line_start(ed.buffer_mut());
    break_undo_group_in_insert(ed);
    ed.push_buffer_cursor_to_textarea();
    false
}
/// Move the cursor to the end of the current line, breaking the undo group.
/// Clears the autopair pending-closes stack. Returns `false` (no mutation).
pub(crate) fn insert_end_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) -> bool {
    ed.sync_buffer_content_from_textarea();
    ed.pending_closes_mut().clear();
    hjkl_engine::motions::move_line_end(ed.buffer_mut());
    break_undo_group_in_insert(ed);
    ed.push_buffer_cursor_to_textarea();
    false
}
/// Scroll up one full viewport height, moving the cursor with it.
/// Breaks the undo group. Returns `false` (no mutation).
pub(crate) fn insert_pageup_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    viewport_h: u16,
) -> bool {
    let rows = viewport_h.saturating_sub(2).max(1) as isize;
    ed.scroll_cursor_rows(-rows);
    false
}
/// Scroll down one full viewport height, moving the cursor with it.
/// Breaks the undo group. Returns `false` (no mutation).
pub(crate) fn insert_pagedown_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    viewport_h: u16,
) -> bool {
    let rows = viewport_h.saturating_sub(2).max(1) as isize;
    ed.scroll_cursor_rows(rows);
    false
}
/// Delete from the cursor back to the start of the previous word (`Ctrl-W`).
/// At col 0, joins with the previous line (vim semantics). Returns `true`
/// when something was deleted.
pub(crate) fn insert_ctrl_w_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) -> bool {
    use hjkl_buffer::{Edit, MotionKind};
    ed.sync_buffer_content_from_textarea();
    let cursor = buf_cursor_pos(ed.buffer());
    if cursor.row == 0 && cursor.col == 0 {
        return true;
    }
    // B2: at col 0 of any row but the first, `Ctrl-W` joins with the
    // previous line (deletes just the newline) and stops — it must NOT
    // continue past the line boundary and eat the previous line's last
    // word. Verified against nvim: `A<CR><C-w><Esc>` on "abc" restores
    // "abc" (a bare join, not `J`'s space-inserting join).
    if cursor.col == 0 {
        let prev_row = cursor.row - 1;
        let prev_chars = buf_line_chars(ed.buffer(), prev_row);
        ed.mutate_edit(Edit::JoinLines {
            row: prev_row,
            count: 1,
            with_space: false,
        });
        buf_set_cursor_rc(ed.buffer_mut(), prev_row, prev_chars);
        ed.push_buffer_cursor_to_textarea();
        return true;
    }
    let iskeyword = ed.settings().iskeyword.clone();
    hjkl_engine::motions::move_word_back(ed.buffer_mut(), false, 1, &iskeyword);
    let word_start = buf_cursor_pos(ed.buffer());
    if word_start == cursor {
        return true;
    }
    buf_set_cursor_pos(ed.buffer_mut(), cursor);
    ed.mutate_edit(Edit::DeleteRange {
        start: word_start,
        end: cursor,
        kind: MotionKind::Char,
    });
    ed.push_buffer_cursor_to_textarea();
    true
}
/// Delete backward on the current line (`Ctrl-U`, `:h i_CTRL-U`). No-op when
/// already at column 0. Returns `true` when something was deleted.
///
/// B3: vim deletes only the text THIS insert session typed on the current
/// line, not the whole line back to column 0. When there's nothing
/// session-typed left to delete (either the session didn't start on this
/// row, or the cursor is already at/before its start column), it falls back
/// to deleting to the first non-blank column — and if the cursor is already
/// at or before THAT indent boundary, all the way to column 0 (vim's
/// documented two-consecutive-presses behaviour). All three tiers verified
/// against nvim probes.
pub(crate) fn insert_ctrl_u_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) -> bool {
    use hjkl_buffer::{Edit, MotionKind, Position};
    ed.sync_buffer_content_from_textarea();
    let cursor = buf_cursor_pos(ed.buffer());
    if cursor.col == 0 {
        return true;
    }
    let session_start_col = vim(ed)
        .insert_session
        .as_ref()
        .filter(|s| s.start_row == cursor.row)
        .map(|s| s.start_col);
    let target = match session_start_col {
        Some(start_col) if cursor.col > start_col => start_col,
        _ => {
            let line = buf_line(ed.buffer(), cursor.row).unwrap_or_default();
            let first_non_blank = line
                .chars()
                .position(|c| c != ' ' && c != '\t')
                .unwrap_or(0);
            if cursor.col > first_non_blank {
                first_non_blank
            } else {
                0
            }
        }
    };
    if target < cursor.col {
        ed.mutate_edit(Edit::DeleteRange {
            start: Position::new(cursor.row, target),
            end: cursor,
            kind: MotionKind::Char,
        });
        ed.push_buffer_cursor_to_textarea();
    }
    true
}
/// Delete one character backwards (`Ctrl-H`) — alias for Backspace in insert
/// mode. Joins with the previous line when at col 0. Returns `true` when
/// something was deleted.
pub(crate) fn insert_ctrl_h_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) -> bool {
    use hjkl_buffer::{Edit, MotionKind, Position};
    ed.sync_buffer_content_from_textarea();
    let cursor = buf_cursor_pos(ed.buffer());
    if cursor.col > 0 {
        ed.mutate_edit(Edit::DeleteRange {
            start: Position::new(cursor.row, cursor.col - 1),
            end: cursor,
            kind: MotionKind::Char,
        });
    } else if cursor.row > 0 {
        let prev_row = cursor.row - 1;
        let prev_chars = buf_line_chars(ed.buffer(), prev_row);
        ed.mutate_edit(Edit::JoinLines {
            row: prev_row,
            count: 1,
            with_space: false,
        });
        buf_set_cursor_rc(ed.buffer_mut(), prev_row, prev_chars);
    }
    ed.push_buffer_cursor_to_textarea();
    true
}
/// B1: insert the text typed during the most recent insert session
/// (`Ctrl-A`, `:h i_CTRL-A`). No-op when nothing has been inserted yet this
/// buffer session. Returns `true` when text was inserted.
pub(crate) fn insert_ctrl_a_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) -> bool {
    use hjkl_buffer::Edit;
    let Some(text) = vim(ed).last_insert_text.clone() else {
        return false;
    };
    if text.is_empty() {
        return false;
    }
    ed.sync_buffer_content_from_textarea();
    let cursor = buf_cursor_pos(ed.buffer());
    ed.mutate_edit(Edit::InsertStr { at: cursor, text });
    ed.push_buffer_cursor_to_textarea();
    true
}
/// B1: insert the character in the same column of the line BELOW the cursor
/// (`Ctrl-E`, `:h i_CTRL-E`). No-op when there's no line below, or that line
/// is too short to have a char at this column. Returns `true` when a
/// character was inserted.
pub(crate) fn insert_ctrl_e_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) -> bool {
    insert_char_from_adjacent_line(ed, 1)
}
/// B1: insert the character in the same column of the line ABOVE the cursor
/// (`Ctrl-Y`, `:h i_CTRL-Y`). No-op when there's no line above, or that line
/// is too short to have a char at this column. Returns `true` when a
/// character was inserted.
pub(crate) fn insert_ctrl_y_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) -> bool {
    insert_char_from_adjacent_line(ed, -1)
}
/// Shared body for `Ctrl-E`/`Ctrl-Y`: copy the char at the cursor's column
/// from the row `delta` away (below for `+1`, above for `-1`) and insert it
/// like a typed character. `delta = -1` at row 0 and `delta = +1` past the
/// last row are both no-ops (no such line); a column beyond that line's
/// length is also a no-op (no such char).
fn insert_char_from_adjacent_line<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    delta: isize,
) -> bool {
    use hjkl_buffer::Edit;
    ed.sync_buffer_content_from_textarea();
    let cursor = buf_cursor_pos(ed.buffer());
    let Some(other_row) = cursor.row.checked_add_signed(delta) else {
        return false;
    };
    if other_row >= buf_row_count(ed.buffer()) {
        return false;
    }
    let Some(ch) = buf_line(ed.buffer(), other_row).and_then(|l| l.chars().nth(cursor.col)) else {
        return false;
    };
    ed.mutate_edit(Edit::InsertChar { at: cursor, ch });
    ed.push_buffer_cursor_to_textarea();
    true
}
/// Indent the current line by one `shiftwidth` and shift the cursor right by
/// the same amount (`Ctrl-T`). Returns `true`.
pub(crate) fn insert_ctrl_t_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) -> bool {
    let (row, col) = ed.cursor();
    let sw = ed.settings().shiftwidth;
    indent_rows(ed, row, row, 1);
    ed.jump_cursor(row, col + sw);
    true
}
/// Outdent the current line by up to one `shiftwidth` and shift the cursor
/// left by the amount stripped (`Ctrl-D`). Returns `true`.
pub(crate) fn insert_ctrl_d_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) -> bool {
    let (row, col) = ed.cursor();
    let before_len = buf_line_bytes(ed.buffer(), row);
    outdent_rows(ed, row, row, 1);
    let after_len = buf_line_bytes(ed.buffer(), row);
    let stripped = before_len.saturating_sub(after_len);
    let new_col = col.saturating_sub(stripped);
    ed.jump_cursor(row, new_col);
    true
}
/// Enter "one-shot normal" mode (`Ctrl-O`): suspend insert for the next
/// complete normal-mode command, then return to insert. Returns `false`
/// (no buffer mutation — only mode state changes).
pub(crate) fn insert_ctrl_o_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) -> bool {
    vim_mut(ed).one_shot_normal = true;
    vim_mut(ed).mode = Mode::Normal;
    // Phase 6.3: keep current_mode in sync for callers that bypass step().
    vim_mut(ed).current_mode = hjkl_engine::VimMode::Normal;
    false
}
/// Arm the register-paste selector (`Ctrl-R`): the next typed character
/// names the register whose text will be inserted inline. Returns `false`
/// (no buffer mutation yet — mutation happens when the register char arrives).
pub(crate) fn insert_ctrl_r_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) -> bool {
    vim_mut(ed).insert_pending_register = true;
    false
}
/// Paste the contents of `reg` at the cursor (the body of `Ctrl-R {reg}`).
/// Unknown or empty registers are a no-op. Returns `true` when text was
/// inserted.
pub(crate) fn insert_paste_register_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    reg: char,
) -> bool {
    insert_register_text(ed, reg);
    // insert_register_text already calls mark_content_dirty internally;
    // return true to signal that the session row window should be widened.
    true
}
/// Exit insert mode to Normal: finish the insert session, step the cursor one
/// cell left (vim convention), record the `gi` target, and update the sticky
/// column. Clears the autopair pending-closes stack. Returns `true` (always
/// consumed — even if no buffer mutation, the mode change itself is a
/// meaningful step).
pub(crate) fn leave_insert_to_normal_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) -> bool {
    ed.pending_closes_mut().clear();

    // ── Abbreviation expansion on Esc ────────────────────────────────────────
    // Esc triggers expansion for all abbreviation types.
    if !ed.abbrevs_is_empty() {
        check_and_apply_abbrev(ed, AbbrevTrigger::Esc);
    }

    finish_insert_session(ed);
    // Paired-tag auto-rename (issue #182). Must run BEFORE the cursor moves
    // left (the move-left is vim's "leave-insert cursor adjustment"; the
    // sync needs the post-insert cursor position to detect the tag name).
    sync_paired_tag_on_exit(ed);
    vim_mut(ed).mode = Mode::Normal;
    // Phase 6.3: keep current_mode in sync for callers that bypass step().
    vim_mut(ed).current_mode = hjkl_engine::VimMode::Normal;
    let col = ed.cursor().1;
    vim_mut(ed).last_insert_pos = Some(ed.cursor());
    if col > 0 {
        hjkl_engine::motions::move_left(ed.buffer_mut(), 1);
        ed.push_buffer_cursor_to_textarea();
    }
    ed.set_sticky_col(Some(ed.cursor().1));
    true
}
