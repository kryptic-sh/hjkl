//! Vim FSM: tag.
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

/// Tag kind detected at a cursor position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TagKind {
    Open,
    Close,
}
/// A single tag instance located in the buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TagSpan {
    kind: TagKind,
    name: String,
    /// Row index in the buffer.
    row: usize,
    /// Char-column range of the tag NAME (excluding `<`, `</`, attributes, `>`).
    name_start_col: usize,
    name_end_col: usize,
}
/// Detect the tag containing `(row, col)` in `line`. Returns the tag kind
/// (Open / Close), its name, and the char-column range of that name.
/// Returns `None` when the cursor is not inside a tag-name region.
pub(crate) fn detect_tag_at_cursor(line: &str, row: usize, col: usize) -> Option<TagSpan> {
    let chars: Vec<char> = line.chars().collect();
    // Find the nearest `<` at or before the cursor column.
    let mut lt = None;
    let mut i = col.min(chars.len());
    while i > 0 {
        i -= 1;
        let c = chars[i];
        if c == '<' {
            lt = Some(i);
            break;
        }
        // Bail if we cross a `>` (we're outside any open tag).
        if c == '>' {
            return None;
        }
    }
    let lt = lt?;
    // Detect close tag (`</`) vs open (`<`).
    let (kind, name_start) = if chars.get(lt + 1) == Some(&'/') {
        (TagKind::Close, lt + 2)
    } else {
        (TagKind::Open, lt + 1)
    };
    // First char of the name must be a letter.
    let first = chars.get(name_start)?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    // Tag name = [A-Za-z][A-Za-z0-9-]*
    let mut name_end = name_start;
    while name_end < chars.len()
        && (chars[name_end].is_ascii_alphanumeric() || chars[name_end] == '-')
    {
        name_end += 1;
    }
    // Cursor must be inside the name range (inclusive of both ends so that
    // landing right after the name still resolves — vim Insert leaves the
    // cursor one past the last typed char).
    if col < name_start || col > name_end {
        return None;
    }
    let name: String = chars[name_start..name_end].iter().collect();
    Some(TagSpan {
        kind,
        name,
        row,
        name_start_col: name_start,
        name_end_col: name_end,
    })
}
/// Scan the buffer to find the structural partner of `anchor` using a
/// depth counter. Names are intentionally NOT compared during the scan —
/// the anchor is the source of truth and the partner inherits its name.
/// Otherwise an in-flight rename (the whole point of this feature) would
/// look like a malformed pair and bail.
///
/// Forward scan from an opener: opens increment depth, closes decrement
/// depth. The close that brings depth back to zero is the partner.
/// Backward scan from a closer is symmetric (closes increment, opens
/// decrement).
///
/// Returns `None` when the buffer end is reached before depth hits zero
/// (orphan tag or malformed input).
pub(crate) fn find_matching_tag(buffer: &hjkl_buffer::Buffer, anchor: &TagSpan) -> Option<TagSpan> {
    let row_count = buffer.row_count();
    let scan_forward = anchor.kind == TagKind::Open;
    let row_iter: Box<dyn Iterator<Item = usize>> = if scan_forward {
        Box::new(anchor.row..row_count)
    } else {
        Box::new((0..=anchor.row).rev())
    };
    let push_kind = if scan_forward {
        TagKind::Open
    } else {
        TagKind::Close
    };
    let mut depth: usize = 1;

    for r in row_iter {
        let line = buf_line(buffer, r)?;
        let chars: Vec<char> = line.chars().collect();
        let tags = scan_line_tags(&chars, r);
        let tags_iter: Box<dyn Iterator<Item = TagSpan>> = if scan_forward {
            Box::new(tags.into_iter())
        } else {
            Box::new(tags.into_iter().rev())
        };
        for tag in tags_iter {
            // Skip the anchor itself when we walk over its line.
            if r == anchor.row
                && tag.name_start_col == anchor.name_start_col
                && tag.kind == anchor.kind
            {
                continue;
            }
            // On the anchor's own row, gate by direction relative to anchor
            // so the scan only inspects tags AFTER the anchor (forward) or
            // BEFORE the anchor (backward).
            if r == anchor.row {
                if scan_forward && tag.name_start_col < anchor.name_start_col {
                    continue;
                }
                if !scan_forward && tag.name_start_col > anchor.name_start_col {
                    continue;
                }
            }
            if tag.kind == push_kind {
                depth += 1;
            } else {
                depth -= 1;
                if depth == 0 {
                    return Some(tag);
                }
            }
        }
    }
    None
}
/// Collect all tag opens / closes on a single line in left-to-right order.
/// Skips comments (`<!-- ... -->`) and self-closing tags (`<br />`), and
/// excludes void HTML elements that don't form a pair.
pub(crate) fn scan_line_tags(chars: &[char], row: usize) -> Vec<TagSpan> {
    let mut out = Vec::new();
    let n = chars.len();
    let mut i = 0;
    while i < n {
        if chars[i] != '<' {
            i += 1;
            continue;
        }
        // `<!--` comment — skip to `-->`.
        if chars[i..].starts_with(&['<', '!', '-', '-']) {
            let mut j = i + 4;
            while j + 2 < n && !(chars[j] == '-' && chars[j + 1] == '-' && chars[j + 2] == '>') {
                j += 1;
            }
            i = (j + 3).min(n);
            continue;
        }
        let (kind, name_start) = if chars.get(i + 1) == Some(&'/') {
            (TagKind::Close, i + 2)
        } else {
            (TagKind::Open, i + 1)
        };
        // Validate name start.
        if chars
            .get(name_start)
            .is_none_or(|c| !c.is_ascii_alphabetic())
        {
            i += 1;
            continue;
        }
        let mut name_end = name_start;
        while name_end < n && (chars[name_end].is_ascii_alphanumeric() || chars[name_end] == '-') {
            name_end += 1;
        }
        // Find the closing `>` to know whether this tag is self-closing.
        let mut k = name_end;
        let mut self_closing = false;
        while k < n {
            if chars[k] == '>' {
                if k > name_end && chars[k - 1] == '/' {
                    self_closing = true;
                }
                break;
            }
            k += 1;
        }
        if k >= n {
            // Unterminated tag on this line — bail.
            break;
        }
        let name: String = chars[name_start..name_end].iter().collect();
        // Skip self-closing and void elements (no pair).
        if !(self_closing || kind == TagKind::Open && is_void_element(&name)) {
            out.push(TagSpan {
                kind,
                name,
                row,
                name_start_col: name_start,
                name_end_col: name_end,
            });
        }
        i = k + 1;
    }
    out
}
/// If the cursor sits inside an HTML/XML tag name AND the paired tag's name
/// differs, rewrite the paired tag's name to match. Called from
/// `leave_insert_to_normal_bridge` so the magical sync fires exactly when
/// the user finishes editing.
pub(crate) fn sync_paired_tag_on_exit<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) {
    if !is_html_filetype(&ed.settings().filetype) {
        return;
    }
    let (row, col) = ed.cursor();
    let line = match buf_line(ed.buffer(), row) {
        Some(l) => l,
        None => return,
    };
    let anchor = match detect_tag_at_cursor(&line, row, col) {
        Some(t) => t,
        None => return,
    };
    let partner = match find_matching_tag(ed.buffer(), &anchor) {
        Some(t) => t,
        None => return,
    };
    if partner.name == anchor.name {
        return;
    }
    // Rewrite the partner's name range with the anchor's name.
    use hjkl_buffer::{Edit, MotionKind, Position};
    let start = Position::new(partner.row, partner.name_start_col);
    let end = Position::new(partner.row, partner.name_end_col);
    ed.mutate_edit(Edit::DeleteRange {
        start,
        end,
        kind: MotionKind::Char,
    });
    ed.mutate_edit(Edit::InsertStr {
        at: start,
        text: anchor.name.clone(),
    });
    // Restore the user's cursor — mutate_edit may have moved it during the
    // partner-side rewrite when the partner is on a row before the cursor.
    buf_set_cursor_rc(ed.buffer_mut(), row, col);
    ed.push_buffer_cursor_to_textarea();
}
/// Resolve the HTML/XML tag-name pair under the cursor for matchparen-style
/// highlight (#243). Returns `[(row, name_start_col, name_end_col); 2]` for
/// the tag under the cursor and its structural partner, or `None` when the
/// cursor is not on a tag name or the tag is unpaired. Char-column ranges
/// (display), consistent with `motions::matching_bracket_pos`.
pub fn matching_tag_pair(
    buffer: &hjkl_buffer::Buffer,
    row: usize,
    col: usize,
) -> Option<[(usize, usize, usize); 2]> {
    let line = buf_line(buffer, row)?;
    let anchor = detect_tag_at_cursor(&line, row, col)?;
    let partner = find_matching_tag(buffer, &anchor)?;
    Some([
        (anchor.row, anchor.name_start_col, anchor.name_end_col),
        (partner.row, partner.name_start_col, partner.name_end_col),
    ])
}
/// Void HTML elements that must never get an auto-close tag.
pub(crate) fn is_void_element(tag: &str) -> bool {
    matches!(
        tag.to_ascii_lowercase().as_str(),
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}
/// Scan backward from `col` (exclusive) in `line` for a `<tagname…` opener.
///
/// Returns `Some(tag_name)` when:
/// - An opening `<` is found
/// - The tag name matches `[A-Za-z][A-Za-z0-9-]*`
/// - The tag is not self-closing (does not end with `/` before `>`)
/// - The tag is not a void element
///
/// Returns `None` otherwise (no opener, self-closing, void, or malformed).
pub(crate) fn scan_tag_opener(line: &str, col: usize) -> Option<String> {
    // col is where `>` was just inserted (the char is already in the line).
    // We look at the slice BEFORE the `>`.
    let before = if col > 0 { &line[..col] } else { return None };

    // Walk backward to find the matching `<`.
    let lt_pos = before.rfind('<')?;
    let inner = &before[lt_pos + 1..]; // e.g. "div class=\"foo\""

    // A `!` opener is a comment/doctype — skip.
    if inner.starts_with('!') {
        return None;
    }
    // Self-closing if the last non-space char before `>` was `/`.
    if inner.trim_end().ends_with('/') {
        return None;
    }

    // Extract tag name: first token of `inner`.
    let tag: String = inner
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    if tag.is_empty() {
        return None;
    }
    // First char must be a letter.
    if !tag
        .chars()
        .next()
        .map(|c| c.is_ascii_alphabetic())
        .unwrap_or(false)
    {
        return None;
    }
    if is_void_element(&tag) {
        return None;
    }
    Some(tag)
}
/// Insert a single character at the cursor. Handles replace-mode overstrike
/// (when `InsertSession::reason` is `Replace`) and smart-indent dedent of
/// closing brackets (}/)]/). Also handles autopair insertion and skip-over.
/// Returns `true`.
pub(crate) fn insert_char_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
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
    if !in_replace && !ed.abbrevs().is_empty() {
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
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) -> bool {
    use hjkl_buffer::Edit;
    ed.sync_buffer_content_from_textarea();

    // ── Abbreviation expansion on CR ─────────────────────────────────────────
    // CR triggers expansion for full-id / end-id / non-id abbreviations.
    // We expand BEFORE the newline is inserted; CR is then inserted as normal.
    if !ed.abbrevs().is_empty() {
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
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
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
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) -> bool {
    use hjkl_buffer::{Edit, MotionKind, Position};
    ed.sync_buffer_content_from_textarea();
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
/// Delete the character under the cursor (vim `Delete`). Joins with the
/// next line when at end-of-line. Returns `true` when something was deleted.
pub(crate) fn insert_delete_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
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
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
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
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
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
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
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
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    viewport_h: u16,
) -> bool {
    let rows = viewport_h.saturating_sub(2).max(1) as isize;
    ed.scroll_cursor_rows(-rows);
    false
}
/// Scroll down one full viewport height, moving the cursor with it.
/// Breaks the undo group. Returns `false` (no mutation).
pub(crate) fn insert_pagedown_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
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
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) -> bool {
    use hjkl_buffer::{Edit, MotionKind};
    ed.sync_buffer_content_from_textarea();
    let cursor = buf_cursor_pos(ed.buffer());
    if cursor.row == 0 && cursor.col == 0 {
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
/// Delete from the cursor back to the start of the current line (`Ctrl-U`).
/// No-op when already at column 0. Returns `true` when something was deleted.
pub(crate) fn insert_ctrl_u_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) -> bool {
    use hjkl_buffer::{Edit, MotionKind, Position};
    ed.sync_buffer_content_from_textarea();
    let cursor = buf_cursor_pos(ed.buffer());
    if cursor.col > 0 {
        ed.mutate_edit(Edit::DeleteRange {
            start: Position::new(cursor.row, 0),
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
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
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
/// Indent the current line by one `shiftwidth` and shift the cursor right by
/// the same amount (`Ctrl-T`). Returns `true`.
pub(crate) fn insert_ctrl_t_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
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
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
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
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
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
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) -> bool {
    vim_mut(ed).insert_pending_register = true;
    false
}
/// Paste the contents of `reg` at the cursor (the body of `Ctrl-R {reg}`).
/// Unknown or empty registers are a no-op. Returns `true` when text was
/// inserted.
pub(crate) fn insert_paste_register_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
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
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) -> bool {
    ed.pending_closes_mut().clear();

    // ── Abbreviation expansion on Esc ────────────────────────────────────────
    // Esc triggers expansion for all abbreviation types.
    if !ed.abbrevs().is_empty() {
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
