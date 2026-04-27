//! `CommentMarkerPass` — TODO/FIXME/NOTE/WARN comment-marker overlay.
//!
//! After `Highlighter::highlight_range` produces a `Vec<HighlightSpan>`,
//! call `CommentMarkerPass::apply` to append extra spans for marker words
//! (TODO, FIXME, FIX, NOTE, INFO, WARN) found inside comment spans. The
//! added spans carry capture names like `"comment.marker.todo"` and
//! `"comment.marker.tail.todo"` which `DotFallbackTheme` maps to coloured
//! badges.
//!
//! # Inheritance
//!
//! When two single-line comments are *consecutive* (only whitespace /
//! nothing between them), the second comment inherits the active marker
//! colour from the first. This mirrors sqeel's marker-overlay behaviour.
//! Set `with_inheritance(false)` to disable.
//!
//! # Seed scan
//!
//! When the highlight range starts mid-buffer the pass walks up to 500
//! lines backward (string-scan fallback) to seed the inherited colour so
//! the first visible comment already has the right tint.

use std::ops::Range;

use crate::highlighter::HighlightSpan;

// ---------------------------------------------------------------------------
// Public API types
// ---------------------------------------------------------------------------

/// A marker word and the capture names it emits.
#[derive(Clone, Debug)]
pub struct MarkerWord {
    /// The keyword to look for (ASCII, uppercase).
    pub word: &'static str,
    /// Capture name for the label (the word + surrounding badge).
    pub label_capture: &'static str,
    /// Capture name for the tail / continuation text.
    pub tail_capture: &'static str,
}

/// Default marker set.
pub fn default_markers() -> &'static [MarkerWord] {
    &[
        MarkerWord {
            word: "TODO",
            label_capture: "comment.marker.todo",
            tail_capture: "comment.marker.tail.todo",
        },
        MarkerWord {
            word: "FIXME",
            label_capture: "comment.marker.fixme",
            tail_capture: "comment.marker.tail.fixme",
        },
        MarkerWord {
            word: "FIX",
            label_capture: "comment.marker.fixme",
            tail_capture: "comment.marker.tail.fixme",
        },
        MarkerWord {
            word: "NOTE",
            label_capture: "comment.marker.note",
            tail_capture: "comment.marker.tail.note",
        },
        MarkerWord {
            word: "INFO",
            label_capture: "comment.marker.note",
            tail_capture: "comment.marker.tail.note",
        },
        MarkerWord {
            word: "WARN",
            label_capture: "comment.marker.warn",
            tail_capture: "comment.marker.tail.warn",
        },
    ]
}

/// Comment-marker overlay pass.
///
/// Call [`apply`](CommentMarkerPass::apply) after
/// `Highlighter::highlight_range` to splice marker + tail spans into the
/// flat span list. The caller then passes the augmented list to the theme
/// resolver / `build_by_row` as usual.
#[derive(Clone, Debug)]
pub struct CommentMarkerPass {
    markers: Vec<MarkerWord>,
    /// When `true` (default), consecutive single-line comments inherit the
    /// active marker capture from the previous comment line.
    inheritance: bool,
}

impl CommentMarkerPass {
    /// Create with the default marker set and inheritance enabled.
    pub fn new() -> Self {
        Self {
            markers: default_markers().to_vec(),
            inheritance: true,
        }
    }

    /// Replace the marker set.
    pub fn with_markers(mut self, markers: Vec<MarkerWord>) -> Self {
        self.markers = markers;
        self
    }

    /// Enable or disable cross-line inheritance.
    pub fn with_inheritance(mut self, on: bool) -> Self {
        self.inheritance = on;
        self
    }

    /// Append marker / tail spans onto `spans` in place.
    ///
    /// `bytes` is the full document so the pass can do a backward seed scan.
    /// `spans` must already contain comment spans from
    /// `Highlighter::highlight_range`; this pass identifies them by capture
    /// (`"comment"` or any capture starting with `"comment."`).
    pub fn apply(&self, spans: &mut Vec<HighlightSpan>, bytes: &[u8]) {
        // Collect comment spans sorted by start byte.
        let mut comments: Vec<Range<usize>> = spans
            .iter()
            .filter(|s| s.capture() == "comment" || s.capture().starts_with("comment."))
            .map(|s| s.byte_range.clone())
            .collect();
        comments.sort_by_key(|r| r.start);
        comments.dedup_by(|b, a| {
            // Merge overlapping / adjacent comment spans (block comments
            // can produce multiple spans for the same range).
            if b.start < a.end {
                a.end = a.end.max(b.end);
                true
            } else {
                false
            }
        });

        if comments.is_empty() {
            return;
        }

        // Seed the inherited capture by scanning backward from the first
        // comment span.
        let first_start = comments[0].start;
        let mut active: Option<&MarkerWord> = if self.inheritance {
            self.seed_active(bytes, first_start)
        } else {
            None
        };

        let mut extra: Vec<HighlightSpan> = Vec::new();

        let mut prev_end: Option<usize> = None;

        for comment_range in &comments {
            // Check whether this comment is consecutive with the previous one
            // (only whitespace between the two, on adjacent/same lines).
            let consecutive = if let Some(pe) = prev_end {
                self.inheritance && is_consecutive(bytes, pe, comment_range.start)
            } else {
                false
            };

            if !consecutive {
                // Gap — reset inherited colour.
                active = None;
            }

            // Compute body range (skip delimiter).
            let body_start = delimiter_skip(bytes, comment_range.start);
            let body_end = comment_range.end;

            if body_start >= body_end {
                prev_end = Some(comment_range.end);
                continue;
            }

            // Scan for markers in the body.
            let found = scan_markers(bytes, body_start, body_end, &self.markers);

            if found.is_empty() {
                // No marker on this comment — inherit active colour for
                // the whole body.
                if let Some(mw) = active {
                    extra.push(HighlightSpan {
                        byte_range: body_start..body_end,
                        capture: mw.tail_capture.to_string(),
                    });
                }
                prev_end = Some(comment_range.end);
                continue;
            }

            // Emit label + tail spans for each found marker.
            let mut cursor = body_start;
            for m in &found {
                // Tail from cursor to just before this marker's label start.
                let label_start = m.word_start.saturating_sub(1).max(body_start);
                if let Some(mw) = active
                    && cursor < label_start
                {
                    extra.push(HighlightSpan {
                        byte_range: cursor..label_start,
                        capture: mw.tail_capture.to_string(),
                    });
                }
                // Label span: char before marker through end of word.
                extra.push(HighlightSpan {
                    byte_range: label_start..m.word_end,
                    capture: m.marker.label_capture.to_string(),
                });
                // Trail char after the word (e.g. ':').
                let trail_end = if m.word_end < body_end {
                    m.word_end + 1
                } else {
                    m.word_end
                };
                if trail_end > m.word_end {
                    extra.push(HighlightSpan {
                        byte_range: m.word_end..trail_end,
                        capture: m.marker.label_capture.to_string(),
                    });
                }
                cursor = trail_end;
                active = Some(m.marker);
            }
            // Tail after the last marker.
            if let Some(mw) = active
                && cursor < body_end
            {
                extra.push(HighlightSpan {
                    byte_range: cursor..body_end,
                    capture: mw.tail_capture.to_string(),
                });
            }

            prev_end = Some(comment_range.end);
        }

        spans.extend(extra);
    }

    /// Scan backward from `first_comment_start` (up to 500 lines) using a
    /// string-scan fallback to seed the inherited colour.
    fn seed_active<'m>(
        &'m self,
        bytes: &[u8],
        first_comment_start: usize,
    ) -> Option<&'m MarkerWord> {
        const CAP: usize = 500;
        if first_comment_start == 0 {
            return None;
        }
        // Collect the starts of up to CAP lines before first_comment_start.
        let prefix = &bytes[..first_comment_start];
        let newline_positions: Vec<usize> = prefix
            .iter()
            .enumerate()
            .filter(|&(_, b)| *b == b'\n')
            .map(|(i, _)| i)
            .collect();
        let start_nl = newline_positions.len().saturating_sub(CAP);

        let line_starts: Vec<usize> = {
            let mut v = vec![if start_nl == 0 {
                0usize
            } else {
                newline_positions[start_nl - 1] + 1
            }];
            for &nl in &newline_positions[start_nl..] {
                v.push(nl + 1);
            }
            v
        };

        let mut active: Option<&'m MarkerWord> = None;

        for &ls in &line_starts {
            let le = bytes[ls..]
                .iter()
                .position(|&b| b == b'\n')
                .map(|p| ls + p)
                .unwrap_or(first_comment_start);
            let le = le.min(first_comment_start);
            let line_bytes = &bytes[ls..le];
            // String-scan fallback: look for comment delimiters.
            if let Some(del_off) = find_comment_delimiter(line_bytes) {
                let body_start = ls + del_off + 2;
                let body_end = le;
                if body_start < body_end {
                    let found = scan_markers(bytes, body_start, body_end, &self.markers);
                    if let Some(last) = found.last() {
                        active = Some(last.marker);
                    }
                    // else: inherit active unchanged.
                }
            } else {
                // Non-comment line resets.
                active = None;
            }
        }
        active
    }
}

impl Default for CommentMarkerPass {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// A found marker in a comment body.
struct FoundMarker<'m> {
    word_start: usize, // byte offset in `bytes`
    word_end: usize,
    marker: &'m MarkerWord,
}

/// Scan `bytes[body_start..body_end]` for word-boundary occurrences of each
/// marker word. Returns results sorted by `word_start`.
fn scan_markers<'m>(
    bytes: &[u8],
    body_start: usize,
    body_end: usize,
    markers: &'m [MarkerWord],
) -> Vec<FoundMarker<'m>> {
    let end = body_end.min(bytes.len());
    if body_start >= end {
        return Vec::new();
    }
    let body = &bytes[body_start..end];
    let mut out: Vec<FoundMarker<'m>> = Vec::new();

    for mw in markers {
        let wbytes = mw.word.as_bytes();
        let mut i = 0usize;
        while i + wbytes.len() <= body.len() {
            if &body[i..i + wbytes.len()] == wbytes {
                let left_ok = i == 0 || !body[i - 1].is_ascii_alphanumeric();
                let right_ok = body
                    .get(i + wbytes.len())
                    .map(|b| !b.is_ascii_alphanumeric())
                    .unwrap_or(true);
                if left_ok && right_ok {
                    out.push(FoundMarker {
                        word_start: body_start + i,
                        word_end: body_start + i + wbytes.len(),
                        marker: mw,
                    });
                    i += wbytes.len();
                    continue;
                }
            }
            i += 1;
        }
    }
    out.sort_by_key(|m| m.word_start);
    out
}

/// Skip over a known comment delimiter at `pos` in `bytes`.
/// Recognises `--`, `//`, `/*`, `#` (1 byte). Returns the byte offset of the
/// first body character.
fn delimiter_skip(bytes: &[u8], pos: usize) -> usize {
    if pos + 1 < bytes.len() {
        let (a, b) = (bytes[pos], bytes[pos + 1]);
        if (a == b'-' && b == b'-') || (a == b'/' && b == b'/') || (a == b'/' && b == b'*') {
            return pos + 2;
        }
    }
    if pos < bytes.len() && bytes[pos] == b'#' {
        return pos + 1;
    }
    pos
}

/// Returns the offset of the start of a comment delimiter within `line_bytes`
/// (relative to `line_bytes[0]`), or `None` if no delimiter is found.
/// Used by the seed fallback scanner.
fn find_comment_delimiter(line_bytes: &[u8]) -> Option<usize> {
    // Look for `--`, `//`, `/*`, `#`.
    let mut i = 0usize;
    while i + 1 < line_bytes.len() {
        let (a, b) = (line_bytes[i], line_bytes[i + 1]);
        if (a == b'-' && b == b'-') || (a == b'/' && b == b'/') || (a == b'/' && b == b'*') {
            return Some(i);
        }
        i += 1;
    }
    // Single-char `#`.
    if line_bytes.contains(&b'#') {
        return line_bytes.iter().position(|&b| b == b'#');
    }
    None
}

/// Return `true` when `bytes[prev_end..next_start]` contains only whitespace
/// (spaces, tabs, newlines) — i.e. the two comment spans are on adjacent
/// lines with nothing but blank lines (or nothing) between them.
fn is_consecutive(bytes: &[u8], prev_end: usize, next_start: usize) -> bool {
    if prev_end > next_start || next_start > bytes.len() {
        return false;
    }
    bytes[prev_end..next_start]
        .iter()
        .all(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Highlighter;
    use crate::registry::LanguageRegistry;

    // Build comment spans via the Rust highlighter for a given source.
    fn rust_comment_spans(src: &[u8]) -> Vec<HighlightSpan> {
        let reg = LanguageRegistry::new();
        let cfg = reg.by_name("rust").unwrap();
        let mut h = Highlighter::new(cfg).unwrap();
        h.parse_initial(src);
        h.highlight_range(src, 0..src.len())
    }

    // Helper: apply pass, return added spans (captures starting with
    // "comment.marker").
    fn marker_spans(src: &[u8]) -> Vec<HighlightSpan> {
        let mut spans = rust_comment_spans(src);
        let pass = CommentMarkerPass::new();
        pass.apply(&mut spans, src);
        spans
            .into_iter()
            .filter(|s| s.capture().starts_with("comment.marker"))
            .collect()
    }

    #[test]
    fn single_line_todo_emits_label_and_tail() {
        // "// TODO: refactor" — expect a label span (comment.marker.todo)
        // and a tail span (comment.marker.tail.todo).
        let src = b"// TODO: refactor";
        let ms = marker_spans(src);
        assert!(
            ms.iter().any(|s| s.capture() == "comment.marker.todo"),
            "expected label span; got {ms:#?}"
        );
        assert!(
            ms.iter().any(|s| s.capture() == "comment.marker.tail.todo"),
            "expected tail span; got {ms:#?}"
        );
    }

    #[test]
    fn multi_line_block_todo_spans_full_body() {
        // /* TODO: long\nexplanation */ — body crosses newline; both label and
        // tail should be present.
        let src = b"/* TODO: long\nexplanation */";
        let ms = marker_spans(src);
        assert!(
            ms.iter().any(|s| s.capture() == "comment.marker.todo"),
            "expected label; got {ms:#?}"
        );
        assert!(
            ms.iter().any(|s| s.capture() == "comment.marker.tail.todo"),
            "expected tail; got {ms:#?}"
        );
    }

    #[test]
    fn consecutive_single_line_inheritance() {
        // "// TODO foo\n// continuation" — second comment should carry
        // comment.marker.tail.todo from the first.
        let src = b"// TODO foo\n// continuation";
        let ms = marker_spans(src);
        // Second line starts at byte 12; find a tail span there.
        let has_inherited_tail = ms
            .iter()
            .any(|s| s.capture() == "comment.marker.tail.todo" && s.byte_range.start >= 12);
        assert!(
            has_inherited_tail,
            "expected inherited tail on second line; got {ms:#?}"
        );
    }

    #[test]
    fn inheritance_breaks_on_non_comment_line() {
        // "// TODO\n  let x = 1;\n// next" — third comment has no inherited colour.
        let src = b"// TODO\n  let x = 1;\n// next";
        let ms = marker_spans(src);
        // Comment starting at byte 21 (after "let" line) should have no tail.
        let last_comment_byte = src.iter().rposition(|&b| b == b'/').unwrap_or(0) - 1;
        let inherited = ms.iter().any(|s| {
            s.capture() == "comment.marker.tail.todo" && s.byte_range.start > last_comment_byte
        });
        assert!(
            !inherited,
            "expected no inherited tail on '// next'; got {ms:#?}"
        );
    }

    #[test]
    fn inheritance_off_does_not_carry() {
        let src = b"// TODO foo\n// continuation";
        let mut spans = rust_comment_spans(src);
        let pass = CommentMarkerPass::new().with_inheritance(false);
        pass.apply(&mut spans, src);
        let ms: Vec<_> = spans
            .into_iter()
            .filter(|s| s.capture().starts_with("comment.marker"))
            .collect();
        // The second comment line should have no marker spans at all.
        let has_second_line_marker = ms.iter().any(|s| s.byte_range.start >= 12);
        assert!(
            !has_second_line_marker,
            "expected no spans on second line with inheritance off; got {ms:#?}"
        );
    }

    #[test]
    fn marker_word_boundary_no_match() {
        // "TODOlist" and "XTODO" must not trigger.
        let src = b"// TODOlist\n// XTODO";
        let ms = marker_spans(src);
        assert!(
            ms.is_empty(),
            "expected no marker spans for non-boundary words; got {ms:#?}"
        );
    }

    #[test]
    fn multiple_markers_one_comment() {
        // "// TODO foo FIXME bar" — two label spans, different captures.
        let src = b"// TODO foo FIXME bar";
        let ms = marker_spans(src);
        let has_todo = ms.iter().any(|s| s.capture() == "comment.marker.todo");
        let has_fixme = ms.iter().any(|s| s.capture() == "comment.marker.fixme");
        assert!(has_todo, "expected todo label; got {ms:#?}");
        assert!(has_fixme, "expected fixme label; got {ms:#?}");
    }

    #[test]
    fn fixme_marker_emits_correct_capture() {
        let src = b"// FIXME: broken";
        let ms = marker_spans(src);
        assert!(
            ms.iter().any(|s| s.capture() == "comment.marker.fixme"),
            "expected fixme label; got {ms:#?}"
        );
        assert!(
            ms.iter()
                .any(|s| s.capture() == "comment.marker.tail.fixme"),
            "expected fixme tail; got {ms:#?}"
        );
    }

    #[test]
    fn fix_marker_uses_fixme_capture() {
        let src = b"// FIX: broken";
        let ms = marker_spans(src);
        assert!(
            ms.iter().any(|s| s.capture() == "comment.marker.fixme"),
            "FIX should map to comment.marker.fixme; got {ms:#?}"
        );
    }

    #[test]
    fn note_and_info_use_note_capture() {
        for word in [b"NOTE" as &[u8], b"INFO"] {
            let src = [b"// ".as_ref(), word, b": context"].concat();
            let ms = marker_spans(&src);
            assert!(
                ms.iter().any(|s| s.capture() == "comment.marker.note"),
                "{} should map to comment.marker.note; got {ms:#?}",
                std::str::from_utf8(word).unwrap()
            );
        }
    }

    #[test]
    fn warn_marker_emits_correct_capture() {
        let src = b"// WARN: danger";
        let ms = marker_spans(src);
        assert!(
            ms.iter().any(|s| s.capture() == "comment.marker.warn"),
            "expected warn label; got {ms:#?}"
        );
    }

    #[test]
    fn apply_is_idempotent_on_no_comments() {
        // No comment in the source — pass should be a no-op.
        let src = b"fn main() {}";
        let mut spans = rust_comment_spans(src);
        let before = spans.len();
        let pass = CommentMarkerPass::new();
        pass.apply(&mut spans, src);
        let after = spans.len();
        assert_eq!(before, after, "no-comment source should not grow spans");
    }

    #[test]
    fn default_pass_is_same_as_new() {
        let a = CommentMarkerPass::new();
        let b = CommentMarkerPass::default();
        assert_eq!(a.inheritance, b.inheritance);
        assert_eq!(a.markers.len(), b.markers.len());
    }

    #[test]
    fn scan_markers_word_boundary_left() {
        // "XTODO" — left boundary fails.
        let bytes = b"// XTODO";
        let markers = default_markers();
        let found = scan_markers(bytes, 3, bytes.len(), markers);
        assert!(
            found.is_empty(),
            "XTODO should not match; got {found:?}",
            found = found.iter().map(|m| m.marker.word).collect::<Vec<_>>()
        );
    }

    #[test]
    fn scan_markers_word_boundary_right() {
        // "TODOlist" — right boundary fails.
        let bytes = b"// TODOlist";
        let markers = default_markers();
        let found = scan_markers(bytes, 3, bytes.len(), markers);
        assert!(found.is_empty(), "TODOlist should not match");
    }

    #[test]
    fn is_consecutive_whitespace_only() {
        let bytes = b"// a\n// b";
        // prev_end=4 (after first comment), next_start=5 (start of second).
        assert!(is_consecutive(bytes, 4, 5));
    }

    #[test]
    fn is_consecutive_non_whitespace_between() {
        let bytes = b"// a\nlet x=1;\n// b";
        assert!(!is_consecutive(bytes, 4, 14));
    }
}
