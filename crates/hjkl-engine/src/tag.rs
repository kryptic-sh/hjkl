//! HTML/XML tag matching — discipline-agnostic buffer substrate.
//!
//! Everything here is a pure query over a [`View`](hjkl_buffer::View) (plus
//! one edit helper that goes through `Editor`): find the tag under the cursor,
//! find its structural partner, decide whether an element is void. None of it
//! knows what a mode or an operator is.
//!
//! It rode along into `hjkl-vim` when the vim FSM relocated (#267) purely
//! because that is where the file happened to be. It lives here now so any
//! discipline — vscode, a future helix/emacs — can match tags without depending
//! on the vim crate (#265).

use crate::Editor;
use crate::buf_helpers::{buf_line, buf_set_cursor_rc};

/// Tag kind detected at a cursor position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagKind {
    Open,
    Close,
}
/// A single tag instance located in the buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagSpan {
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
pub fn detect_tag_at_cursor(line: &str, row: usize, col: usize) -> Option<TagSpan> {
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
    // Cursor must be inside the name range, inclusive of both ends: any insert
    // mode leaves the cursor one past the last typed char, so landing right
    // after the name must still resolve to it.
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
pub fn find_matching_tag(buffer: &hjkl_buffer::View, anchor: &TagSpan) -> Option<TagSpan> {
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
pub fn scan_line_tags(chars: &[char], row: usize) -> Vec<TagSpan> {
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
pub fn sync_paired_tag_on_exit<H: crate::types::Host>(ed: &mut Editor<hjkl_buffer::View, H>) {
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
    buffer: &hjkl_buffer::View,
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
pub fn is_void_element(tag: &str) -> bool {
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
pub fn scan_tag_opener(line: &str, col: usize) -> Option<String> {
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

/// Filetypes that get HTML/XML-family treatment (`<` pairing + tag autoclose).
pub fn is_html_filetype(ft: &str) -> bool {
    matches!(
        ft,
        "html" | "xml" | "svg" | "jsx" | "tsx" | "vue" | "svelte"
    )
}
