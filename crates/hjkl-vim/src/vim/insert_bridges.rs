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

/// Insert `text` at the cursor the way typing it would: under
/// `'formatoptions'` `t` each character goes through [`auto_wrap_before`]
/// first, so a repeated insert wraps where its own text reaches the margin,
/// as vim's redo does when it retypes the keys.
pub fn type_text<H: hjkl_engine::types::Host>(ed: &mut Editor<hjkl_buffer::View, H>, text: &str) {
    use hjkl_buffer::{Edit, Position};
    let wraps = ed.settings().textwidth > 0 && ed.settings().formatoptions.contains('t');
    let chunks: Vec<String> = if wraps {
        text.chars().map(String::from).collect()
    } else {
        vec![text.to_string()]
    };
    for chunk in chunks {
        if let Some(ch) = chunk.chars().next().filter(|_| wraps) {
            auto_wrap_before(ed, ch);
        }
        let (row, col) = ed.cursor();
        ed.mutate_edit(Edit::InsertStr {
            at: Position::new(row, col),
            text: chunk,
        });
    }
}
/// vim's `internal_format` under `'formatoptions'` `t`: a non-blank `ch`
/// typed where it would end past `'textwidth'` first breaks the line at the
/// last run of blanks that starts at or before the margin (or, when every
/// run starts past it, at the leftmost), dropping that run and giving the new
/// line the old one's indent under `'autoindent'`. A line with no blank after
/// its indent is left long. Repeats while the cursor's line is still too long.
/// Not ported: comment leaders (`c`), `'formatexpr'`, and the `l` / `b` /
/// `1` / `2` flags.
pub fn auto_wrap_before<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    ch: char,
) {
    use hjkl_buffer::{Edit, Position};
    let textwidth = ed.settings().textwidth;
    if textwidth == 0 || ch.is_whitespace() || !ed.settings().formatoptions.contains('t') {
        return;
    }
    let tabstop = ed.settings().tabstop;
    let is_blank = |c: char| c == ' ' || c == '\t';
    loop {
        let (row, col) = ed.cursor();
        let line = buf_line(ed.buffer(), row).unwrap_or_default();
        let chars: Vec<char> = line.chars().collect();
        let col = col.min(chars.len());
        let typed: String = chars[..col].iter().chain(std::iter::once(&ch)).collect();
        if hjkl_buffer::char_col_to_visual_col(&typed, col + 1, tabstop) <= textwidth {
            return;
        }
        let margin = hjkl_buffer::visual_col_to_char_col(&line, textwidth, tabstop);
        // Walk back from the cursor for the start of a blank run.
        let mut found = None;
        let mut at = col;
        loop {
            if chars.get(at).copied().is_some_and(is_blank) {
                let mut start = at;
                while start > 0 && is_blank(chars[start - 1]) {
                    start -= 1;
                }
                if start == 0 {
                    break;
                }
                found = Some(start);
                if start <= margin {
                    break;
                }
                at = start;
            }
            if at == 0 {
                break;
            }
            at -= 1;
        }
        let Some(start) = found else {
            return;
        };
        let mut end = start;
        while end < chars.len() && is_blank(chars[end]) {
            end += 1;
        }
        let indent: String = if ed.settings().autoindent {
            chars.iter().take_while(|&&c| is_blank(c)).collect()
        } else {
            String::new()
        };
        let new_col = indent.chars().count() + col.saturating_sub(end);
        let at = hjkl_engine::types::Query::rope(ed.buffer()).line_to_char(row) + start;
        if !vim(ed).replaying
            && let Some(session) = vim_mut(ed).insert_session.as_mut()
        {
            session.wraps.push(hjkl_vim_types::InsertWrap {
                at,
                blanks: chars[start..end].iter().collect(),
                indent: indent.chars().count(),
            });
        }
        ed.mutate_edit(Edit::Replace {
            start: Position::new(row, start),
            end: Position::new(row, end),
            with: format!("\n{indent}"),
        });
        buf_set_cursor_rc(ed.buffer_mut(), row + 1, new_col);
    }
}
/// Insert a single character at the cursor. Handles replace-mode overstrike
/// (when `InsertSession::reason` is `Replace`) and smart-indent dedent of
/// closing brackets (}/)]/). Also handles autopair insertion and skip-over.
/// Returns `true`.
pub fn insert_char_bridge<H: hjkl_engine::types::Host>(
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
    if !in_replace {
        auto_wrap_before(ed, ch);
    }

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
            let autoclose_tag = ed.settings().autoclose_tag;
            if ch == '>' && autoclose_tag && is_html_filetype(&ed.settings().filetype) {
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
                        .map_or(line.len(), |(b, _)| b);
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
        let autoclose_tag = ed.settings().autoclose_tag;

        // The two chars left of the cursor, read straight off the rope: this
        // runs for EVERY typed char, so no line `String` and no `Vec<char>`.
        // Indices are rope CHAR indices (same space as `cursor.col`), and the
        // `< n` bound reproduces the old `Vec::get()` → `None` past the end of
        // the newline-stripped line.
        let (prev_char, prev2_char) = {
            let rope = hjkl_engine::types::Query::rope(ed.buffer());
            let row = cursor.row;
            if row < rope.len_lines() {
                let line = rope.line(row);
                // ropey keeps the trailing '\n' on non-final lines; the old
                // `Vec<char>` was built from the stripped line.
                let mut n = line.len_chars();
                if n > 0 && line.char(n - 1) == '\n' {
                    n -= 1;
                }
                let at = |i: usize| (i < n).then(|| line.char(i));
                (
                    cursor.col.checked_sub(1).and_then(at),
                    cursor.col.checked_sub(2).and_then(at),
                )
            } else {
                (None, None)
            }
        };

        if autopair {
            if let Some(close) =
                autopair_close_for(ch, &ed.settings().filetype, prev_char, prev2_char)
            {
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
                return true;
            }

            // Tag autoclose: `>` in HTML/XML family (no prior `<` pair).
            // This fires when autopair did NOT match `>` (e.g. `>` was
            // typed directly, not via a skip-over of an auto-inserted `>`).
            if ch == '>' && autoclose_tag && is_html_filetype(&ed.settings().filetype) {
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
                        .map_or(line.len(), |(b, _)| b);
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
                return true;
            }
        }

        // Plain insert — do not clear the pending-closes stack here.
        // The stack is cleared on cursor motion or mode change (Esc).
        // Clearing here would prevent skip-over from firing after the
        // user types content inside an auto-paired bracket.
        ed.mutate_edit(Edit::InsertChar { at: cursor, ch });
    }
    true
}
/// Insert a newline at the cursor, applying autoindent / smartindent and
/// optionally continuing a line comment when `formatoptions` has `r`.
/// Also handles open-pair-newline: Enter between `{|}` / `(|)` / `[|]`
/// produces an indented block with the close on its own line.
/// Returns `true`.
pub fn insert_newline_bridge<H: hjkl_engine::types::Host>(
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
    let prev_line = buf_line(ed.buffer(), cursor.row).unwrap_or_default();

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
        // The indent comes from the line as it exists AFTER the split — the
        // part left of the cursor — not from the whole line. At column 0 that
        // part is empty, which is why neovim gives `['', 'foo']` for `    foo`
        // rather than re-indenting the row it just pushed down.
        let before_cursor: String = prev_line.chars().take(cursor.col).collect();
        let indent = compute_enter_indent(ed.settings(), &before_cursor);
        format!("\n{indent}")
    };

    // Autoindent REPLACES the moved text's own leading whitespace — it does not
    // prepend to it. Inserting in front of that whitespace compounds it: at
    // column 0 of an indented line the new row keeps the old indent AND gains a
    // fresh copy, so every Enter doubles it and a held Enter grows the buffer
    // geometrically. Measured against neovim 0.12.4 with `set autoindent`:
    // `    foo` + `<CR>` at column 0 gives `['', 'foo']`, not `['', '    foo']`;
    // mid-line (`    foo|bar`) gives `['    foo', '    bar']`, which is the same
    // rule with an empty run to consume. `noautoindent` moves the text down
    // untouched, so the consumption is gated on the option that causes it.
    let ws_run = if ed.settings().autoindent {
        prev_line
            .chars()
            .skip(cursor.col)
            .take_while(|c| *c == ' ' || *c == '\t')
            .count()
    } else {
        0
    };
    if ws_run > 0 {
        ed.mutate_edit(Edit::Replace {
            start: cursor,
            end: hjkl_buffer::Position::new(cursor.row, cursor.col + ws_run),
            with: text,
        });
    } else {
        ed.mutate_edit(Edit::InsertStr { at: cursor, text });
    }
    true
}
/// Insert a tab character (or spaces up to the next softtabstop boundary when
/// `expandtab` is set). Returns `true`.
pub fn insert_tab_bridge<H: hjkl_engine::types::Host>(
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
pub fn insert_backspace_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) -> bool {
    ed.sync_buffer_content_from_textarea();
    if matches!(
        vim(ed).insert_session.as_ref().map(|s| &s.reason),
        Some(InsertReason::Replace)
    ) {
        return replace_backspace(ed);
    }
    count_backspace_past_start(ed, generic_backspace)
}

/// Run one `<BS>` / `<C-h>` and, when it deleted at or before the insert's
/// start, record it on the session the way vim moves `Insstart` and keeps the
/// key in its redo buffer: `.` then replays the key before the typed text. A
/// press that changed nothing (at the start of the buffer) is not recorded,
/// as vim records only a backspace that succeeded.
fn count_backspace_past_start<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    backspace: fn(&mut Editor<hjkl_buffer::View, H>) -> bool,
) -> bool {
    let before_cursor = ed.cursor();
    let before_len = hjkl_engine::types::Query::rope(ed.buffer()).len_chars();
    let result = backspace(ed);
    let after_cursor = ed.cursor();
    let after_len = hjkl_engine::types::Query::rope(ed.buffer()).len_chars();
    if after_cursor == before_cursor && after_len == before_len {
        return result;
    }
    if let Some(session) = vim_mut(ed).insert_session.as_mut()
        && before_cursor <= (session.start_row, session.start_col)
    {
        session.backspaces += 1;
        session.deleted += before_len as isize - after_len as isize;
        (session.start_row, session.start_col) = after_cursor;
        session.row_min = session.row_min.min(after_cursor.0);
    }
    result
}

/// The plain-insert-mode `<BS>` body (shared by [`insert_backspace_bridge`]
/// and, as the past-the-session-start fallback, [`replace_backspace`]).
pub fn generic_backspace<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) -> bool {
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
                return true;
            }
        }
    }

    // vim's `ins_bs` under `'softtabstop'`: with whitespace before the cursor,
    // delete back to the previous `softtabstop` stop — counted in display
    // columns, so a tab counts its width — and no further than the
    // whitespace reaches. A tab that overshoots the stop is made up with
    // spaces. From column 2 of `"  q"` that is column 0, two spaces in one
    // stroke; a lone space after text is just that space.
    let sts = ed.settings().softtabstop;
    let line = buf_line(ed.buffer(), cursor.row).unwrap_or_default();
    let chars: Vec<char> = line.chars().collect();
    let is_blank = |c: Option<&char>| matches!(c, Some(' ' | '\t'));
    if sts > 0 && cursor.col > 0 && is_blank(chars.get(cursor.col - 1)) {
        let tabstop = ed.settings().tabstop;
        let vcol = |col: usize| hjkl_buffer::char_col_to_visual_col(&line, col, tabstop);
        let want = (vcol(cursor.col) - 1) / sts * sts;
        let mut start = cursor.col;
        while start > 0 && is_blank(chars.get(start - 1)) && vcol(start - 1) >= want {
            start -= 1;
        }
        if start < cursor.col {
            let pad = want.saturating_sub(vcol(start));
            ed.mutate_edit(Edit::DeleteRange {
                start: Position::new(cursor.row, start),
                end: cursor,
                kind: MotionKind::Char,
            });
            if pad > 0 {
                ed.mutate_edit(Edit::InsertStr {
                    at: Position::new(cursor.row, start),
                    text: " ".repeat(pad),
                });
            }
            return true;
        }
    }
    if cursor.col > 0 {
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
    }
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
    true
}

/// Delete the character under the cursor (vim `Delete`). Joins with the
/// next line when at end-of-line. Returns `true` when something was deleted.
pub fn insert_delete_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) -> bool {
    use hjkl_buffer::{Edit, MotionKind, Position};
    ed.sync_buffer_content_from_textarea();
    let cursor = buf_cursor_pos(ed.buffer());
    let line_chars = buf_line_chars(ed.buffer(), cursor.row);
    if cursor.col < line_chars {
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
    }
}
/// Move the cursor one step in `dir`, breaking the undo group per
/// `undo_break_on_motion`. Clears the autopair pending-closes stack (cursor
/// moved off the pair). Returns `false` (no mutation).
pub fn insert_arrow_bridge<H: hjkl_engine::types::Host>(
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
        InsertDir::Up | InsertDir::Down => insert_vertical(ed, dir == InsertDir::Down),
    }
    break_undo_group_in_insert(ed);
    false
}
/// Insert-mode `<Down>` / `<Up>`. The want is the sticky column only while
/// the previous Insert-mode keys were vertical (`insert_want_current`);
/// otherwise it comes from the cursor, as vim resets curswant on every other
/// cursor change. Insert mode may stand one past the last character, so the
/// landing column is clamped to the line length, not its last character.
fn insert_vertical<H: hjkl_engine::types::Host>(ed: &mut Editor<hjkl_buffer::View, H>, down: bool) {
    let (start_row, col) = ed.cursor();
    if !vim(ed).insert_want_current {
        let want = ed.cursor_want(start_row, col);
        ed.set_sticky_col(Some(want));
    }
    vim_mut(ed).insert_want_current = true;
    let folds = hjkl_engine::SnapshotFoldProvider::from_buffer(ed.buffer());
    let mut sticky = ed.sticky_col();
    let tabstop = ed.settings().tabstop;
    if down {
        hjkl_engine::motions::move_down(ed.buffer_mut(), &folds, 1, &mut sticky, tabstop);
    } else {
        hjkl_engine::motions::move_up(ed.buffer_mut(), &folds, 1, &mut sticky, tabstop);
    }
    ed.set_sticky_col(sticky);
    let row = ed.cursor().0;
    if row != start_row {
        let line = buf_line(ed.buffer(), row).unwrap_or_default();
        let want = sticky.unwrap_or_default();
        let col =
            hjkl_buffer::visual_col_to_char_col(&line, want, tabstop).min(line.chars().count());
        buf_set_cursor_rc(ed.buffer_mut(), row, col);
    }
}
/// Move the cursor to the start of the current line, breaking the undo group.
/// Clears the autopair pending-closes stack. Returns `false` (no mutation).
pub fn insert_home_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) -> bool {
    ed.sync_buffer_content_from_textarea();
    ed.pending_closes_mut().clear();
    hjkl_engine::motions::move_line_start(ed.buffer_mut());
    break_undo_group_in_insert(ed);
    false
}
/// Move the cursor to the end of the current line, breaking the undo group.
/// Clears the autopair pending-closes stack. Returns `false` (no mutation).
pub fn insert_end_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) -> bool {
    ed.sync_buffer_content_from_textarea();
    ed.pending_closes_mut().clear();
    hjkl_engine::motions::move_line_end(ed.buffer_mut());
    // vim's `<End>` sets curswant to MAXCOL, so `<Down>` after it keeps to
    // the end of each line.
    ed.set_sticky_col(Some(usize::MAX));
    vim_mut(ed).insert_want_current = true;
    break_undo_group_in_insert(ed);
    false
}
/// Scroll up one full viewport height, moving the cursor with it.
/// Breaks the undo group. Returns `false` (no mutation).
pub fn insert_pageup_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    viewport_h: u16,
) -> bool {
    let rows = viewport_h.saturating_sub(2).max(1) as isize;
    ed.scroll_cursor_rows(-rows);
    false
}
/// Scroll down one full viewport height, moving the cursor with it.
/// Breaks the undo group. Returns `false` (no mutation).
pub fn insert_pagedown_bridge<H: hjkl_engine::types::Host>(
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
pub fn insert_ctrl_w_bridge<H: hjkl_engine::types::Host>(
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
        return true;
    }
    let iskeyword = ed.settings().iskeyword.clone();
    // Folds are irrelevant to insert-mode `<C-w>`: vim's `'foldopen'` lists
    // `insert`, so entering insert mode has already opened the fold under the
    // cursor by the time this runs.
    hjkl_engine::motions::move_word_back(
        ed.buffer_mut(),
        &hjkl_engine::types::NoopFoldProvider,
        false,
        1,
        &iskeyword,
    );
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
pub fn insert_ctrl_u_bridge<H: hjkl_engine::types::Host>(
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
    }
    true
}
/// Delete one character backwards (`Ctrl-H`) — alias for Backspace in insert
/// mode. Joins with the previous line when at col 0. Returns `true` when
/// something was deleted.
pub fn insert_ctrl_h_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) -> bool {
    ed.sync_buffer_content_from_textarea();
    count_backspace_past_start(ed, ctrl_h_backspace)
}

/// The `<C-h>` body: delete one character back, or join at column 0.
fn ctrl_h_backspace<H: hjkl_engine::types::Host>(ed: &mut Editor<hjkl_buffer::View, H>) -> bool {
    use hjkl_buffer::{Edit, MotionKind, Position};
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
    true
}
/// B1: insert the text typed during the most recent insert session
/// (`Ctrl-A`, `:h i_CTRL-A`). No-op when nothing has been inserted yet this
/// buffer session. Returns `true` when text was inserted.
pub fn insert_ctrl_a_bridge<H: hjkl_engine::types::Host>(
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
    true
}
/// B1: insert the character in the same column of the line BELOW the cursor
/// (`Ctrl-E`, `:h i_CTRL-E`). No-op when there's no line below, or that line
/// is too short to have a char at this column. Returns `true` when a
/// character was inserted.
pub fn insert_ctrl_e_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) -> bool {
    insert_char_from_adjacent_line(ed, 1)
}
/// B1: insert the character in the same column of the line ABOVE the cursor
/// (`Ctrl-Y`, `:h i_CTRL-Y`). No-op when there's no line above, or that line
/// is too short to have a char at this column. Returns `true` when a
/// character was inserted.
pub fn insert_ctrl_y_bridge<H: hjkl_engine::types::Host>(
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
    true
}
/// Indent the current line by one `shiftwidth` and shift the cursor right by
/// the same amount (`Ctrl-T`). Returns `true`.
pub fn insert_ctrl_t_bridge<H: hjkl_engine::types::Host>(
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
pub fn insert_ctrl_d_bridge<H: hjkl_engine::types::Host>(
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
pub fn insert_ctrl_o_bridge<H: hjkl_engine::types::Host>(
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
pub fn insert_ctrl_r_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) -> bool {
    vim_mut(ed).insert_pending_register = true;
    false
}
/// Paste the contents of `reg` at the cursor (the body of `Ctrl-R {reg}`).
/// Unknown or empty registers are a no-op. Returns `true` when text was
/// inserted.
pub fn insert_paste_register_bridge<H: hjkl_engine::types::Host>(
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
pub fn leave_insert_to_normal_bridge<H: hjkl_engine::types::Host>(
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
    }
    remember_cursor_column(ed);
    true
}
