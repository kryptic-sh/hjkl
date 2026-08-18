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
//! When the highlight range starts mid-buffer the pass walks backward
//! (bounded by `SEED_SCAN_CAP` lines) to seed the inherited colour so the
//! first visible comment already has the right tint. The walk stops at the
//! first line that decides the seed — a comment line with a marker, or a
//! non-comment line that resets the colour — instead of always scanning the
//! whole window.

use std::ops::Range;
use std::sync::Arc;

use crate::highlighter::HighlightSpan;
use crate::rope_slice::{ceil_char_boundary, floor_char_boundary};

/// Max lines the seed scan walks backward from the first comment to find the
/// inherited colour (and thus how far above the first comment the seed
/// window may need to reach). Bounded so the scan never touches the whole
/// buffer — a seed beyond the bound is simply missed, exactly as the
/// historical fixed window already missed it.
const SEED_SCAN_CAP: usize = 500;

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
                        capture: Arc::from(mw.tail_capture),
                        metadata: None,
                    });
                }
                prev_end = Some(comment_range.end);
                continue;
            }

            // Emit label + tail spans for each found marker.
            let mut cursor = body_start;
            for m in &found {
                // Tail from cursor to just before this marker's label start.
                // `word_start - 1` is a raw byte offset; when the char before
                // the word is multi-byte it lands mid-char, so snap back to
                // the enclosing char boundary.
                let label_start = floor_char_boundary_bytes(bytes, m.word_start.saturating_sub(1))
                    .max(body_start);
                if let Some(mw) = active
                    && cursor < label_start
                {
                    extra.push(HighlightSpan {
                        byte_range: cursor..label_start,
                        capture: Arc::from(mw.tail_capture),
                        metadata: None,
                    });
                }
                // Label span: char before marker through end of word.
                extra.push(HighlightSpan {
                    byte_range: label_start..m.word_end,
                    capture: Arc::from(m.marker.label_capture),
                    metadata: None,
                });
                // Trail char after the word (e.g. ':').
                let trail_end = if m.word_end < body_end {
                    // `word_end + 1` can land mid-char when the char after the
                    // word is multi-byte — snap forward to the char boundary.
                    ceil_char_boundary_bytes(bytes, m.word_end + 1)
                } else {
                    m.word_end
                };
                if trail_end > m.word_end {
                    extra.push(HighlightSpan {
                        byte_range: m.word_end..trail_end,
                        capture: Arc::from(m.marker.label_capture),
                        metadata: None,
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
                    capture: Arc::from(mw.tail_capture),
                    metadata: None,
                });
            }

            prev_end = Some(comment_range.end);
        }

        spans.extend(extra);
    }

    /// Scan backward from `first_comment_start` (up to [`SEED_SCAN_CAP`]
    /// lines) using a string-scan fallback to seed the inherited colour.
    ///
    /// The walk goes from NEAREST line to farthest and stops at the first
    /// line that decides the seed: a comment line with a marker (its last
    /// marker wins) or a non-comment line (it resets the inherited colour,
    /// so nothing above it can matter). This is exactly the decision the
    /// historical forward walk produced — its final state is determined
    /// solely by the suffix after its last reset — but the scan stops after
    /// a line or two instead of always walking the whole window.
    fn seed_active<'m>(
        &'m self,
        bytes: &[u8],
        first_comment_start: usize,
    ) -> Option<&'m MarkerWord> {
        if first_comment_start == 0 {
            return None;
        }
        let prefix = &bytes[..first_comment_start];

        // The line containing `first_comment_start` is scanned only for its
        // part before the comment: when the comment does not start at column
        // 0, that prefix is a "line" of its own and a non-comment prefix
        // resets the colour — matching the historical per-line string scan.
        let mut start = memchr::memrchr(b'\n', prefix).map_or(0, |nl| nl + 1);
        let mut end = first_comment_start;
        if start == first_comment_start {
            // Comment begins at column 0 of its own line — begin at the line
            // above, which ends at the newline right before the comment.
            end = start - 1;
            start = memchr::memrchr(b'\n', &bytes[..end]).map_or(0, |nl| nl + 1);
        }

        for _ in 0..=SEED_SCAN_CAP {
            match seed_line(&bytes[start..end], &self.markers) {
                SeedLine::Stop(m) => return Some(m),
                SeedLine::Reset => return None,
                SeedLine::Inherit => {}
            }
            if start == 0 {
                return None; // Buffer head reached — nothing further above.
            }
            end = start - 1;
            start = memchr::memrchr(b'\n', &bytes[..end]).map_or(0, |nl| nl + 1);
        }
        None // Bound reached — anything further above is out of scope.
    }

    /// Byte offset where the seed window must start for a comment at
    /// `first_start`: the start of the nearest line above it that carries a
    /// marker (so the materialised window covers exactly the text the seed
    /// scan needs), or `first_start` itself when no marker is found — a
    /// non-comment reset or the scan bound means the seed is `None`, and the
    /// window does not need to reach above the first comment at all.
    ///
    /// Reads the rows straight from the rope (one `rope.line` at a time), so
    /// the seed scan never materialises the bytes it walks; only the span
    /// returned here is later materialised.
    fn seed_window_top(&self, rope: &ropey::Rope, first_start: usize) -> usize {
        let mut scratch = String::new();
        let first_line = rope.byte_to_line(first_start);
        let line_start = rope.line_to_byte(first_line);
        let mut scanned = 0usize;

        // The comment's own line, up to the comment. When the comment does
        // not start at column 0 this prefix is a "line" of its own — mirrors
        // `seed_active`, where a non-comment prefix resets the seed. The end
        // is floored to a char boundary: `byte_slice` panics mid-char, and
        // the excluded bytes (continuation bytes) can never contain a
        // delimiter or marker word, so the classification is unchanged.
        if line_start < first_start {
            scanned += 1;
            let partial = rope.byte_slice(line_start..floor_char_boundary(rope, first_start));
            match classify_rope_line(partial, &self.markers, &mut scratch) {
                SeedLine::Stop(_) => return line_start,
                // A reset means the seed is `None`; the window does not need
                // to include the resetting line (or anything above it), so
                // start at the first comment itself.
                SeedLine::Reset => return first_start,
                SeedLine::Inherit => {}
            }
        }

        // Lines above, nearest first, bounded by SEED_SCAN_CAP lines total.
        let mut line = first_line;
        while line > 0 && scanned <= SEED_SCAN_CAP {
            line -= 1;
            scanned += 1;
            let ls = rope.line_to_byte(line);
            match classify_rope_line(rope.line(line), &self.markers, &mut scratch) {
                SeedLine::Stop(_) => return ls,
                SeedLine::Reset => return first_start,
                SeedLine::Inherit => {}
            }
        }
        // No marker within the bound (or the buffer head): seed is `None`.
        first_start
    }

    /// Like [`apply`](CommentMarkerPass::apply) but reads source text from a
    /// `ropey::Rope`. Only the bytes required (a window around the comments)
    /// are materialised — no full-document `String` allocation. The seed scan
    /// reads the rows above the first comment straight from the rope, so the
    /// materialised window only reaches up to the nearest marker line (or
    /// starts at the first comment when the seed is `None`).
    ///
    /// The algorithm is identical to `apply`; the `bytes` parameter is replaced
    /// by a window slice extracted from the rope, with absolute byte offsets
    /// translated into window-relative ones.
    pub fn apply_rope(&self, spans: &mut Vec<HighlightSpan>, rope: &ropey::Rope) {
        // Collect and deduplicate comment spans (same logic as `apply`).
        let mut comments: Vec<Range<usize>> = spans
            .iter()
            .filter(|s| s.capture() == "comment" || s.capture().starts_with("comment."))
            .map(|s| s.byte_range.clone())
            .collect();
        comments.sort_by_key(|r| r.start);
        comments.dedup_by(|b, a| {
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

        let first_start = comments[0].start;
        let last_end = comments[comments.len() - 1].end;
        let rope_len = rope.len_bytes();

        // Materialise a window covering the comment spans, plus the text the
        // seed scan needs above the first comment. The seed scan reads the
        // rows above the first comment straight from the rope
        // (`seed_window_top`), so the window only has to reach up to the
        // nearest marker line it found — the old fixed `CAP * 200`-byte
        // (~100 KB) window above the first comment is gone. When no marker
        // is found the seed is `None` and the window starts at the first
        // comment itself.
        // Both edges are snapped outward to char boundaries: the seed window
        // start (and untrusted span ends) can land mid-way through a
        // multi-byte char, and `Rope::byte_slice` panics on non-char-boundary
        // indices.
        let seed_window_start = if self.inheritance && first_start > 0 && first_start <= rope_len {
            self.seed_window_top(rope, first_start)
        } else {
            first_start
        };
        let window_start = floor_char_boundary(rope, seed_window_start.min(rope_len));
        let window_end = ceil_char_boundary(rope, last_end.min(rope_len));

        let window_str: String = rope.byte_slice(window_start..window_end).to_string();
        let window: &[u8] = window_str.as_bytes();

        // Translate an absolute byte index into a window-relative index.
        // Returns `None` when the index falls outside the window.
        let to_win = |abs: usize| -> Option<usize> {
            if abs < window_start || abs > window_end {
                None
            } else {
                Some(abs - window_start)
            }
        };

        // Seed the inherited capture by scanning backward from the first comment.
        let win_first = to_win(first_start).unwrap_or(0);
        let mut active: Option<&MarkerWord> = if self.inheritance && win_first > 0 {
            self.seed_active(window, win_first)
        } else {
            None
        };

        let mut extra: Vec<HighlightSpan> = Vec::new();
        let mut prev_end: Option<usize> = None;

        for comment_range in &comments {
            let consecutive = if let Some(pe) = prev_end {
                if self.inheritance {
                    // is_consecutive reads the gap bytes — translate to window.
                    let win_pe = to_win(pe).unwrap_or(0);
                    let win_ns = to_win(comment_range.start).unwrap_or(0);
                    if win_pe <= win_ns {
                        is_consecutive(window, win_pe, win_ns)
                    } else {
                        false
                    }
                } else {
                    false
                }
            } else {
                false
            };

            if !consecutive {
                active = None;
            }

            let Some(win_cr_start) = to_win(comment_range.start) else {
                prev_end = Some(comment_range.end);
                continue;
            };
            let win_cr_end = to_win(comment_range.end)
                .unwrap_or(window.len())
                .min(window.len());

            let body_win_start = delimiter_skip(window, win_cr_start);
            let body_win_end = win_cr_end;

            if body_win_start >= body_win_end {
                prev_end = Some(comment_range.end);
                continue;
            }

            // scan_markers works in window-relative coords; translate back.
            let found = scan_markers(window, body_win_start, body_win_end, &self.markers);

            let body_start = comment_range.start + (body_win_start - win_cr_start);
            let body_end = comment_range.end;

            if found.is_empty() {
                if let Some(mw) = active {
                    extra.push(HighlightSpan {
                        byte_range: body_start..body_end,
                        capture: Arc::from(mw.tail_capture),
                        metadata: None,
                    });
                }
                prev_end = Some(comment_range.end);
                continue;
            }

            let mut cursor = body_win_start;
            for m in &found {
                // Translate window-relative marker positions to absolute.
                let abs_word_start = window_start + m.word_start;
                let abs_word_end = window_start + m.word_end;
                // `word_start - 1` is a raw byte offset; snap back to the
                // enclosing char boundary when the preceding char is
                // multi-byte.
                let label_start =
                    floor_char_boundary(rope, abs_word_start.saturating_sub(1)).max(body_start);
                let win_cursor_abs = window_start + cursor;
                if let Some(mw) = active
                    && win_cursor_abs < label_start
                {
                    extra.push(HighlightSpan {
                        byte_range: win_cursor_abs..label_start,
                        capture: Arc::from(mw.tail_capture),
                        metadata: None,
                    });
                }
                extra.push(HighlightSpan {
                    byte_range: label_start..abs_word_end,
                    capture: Arc::from(m.marker.label_capture),
                    metadata: None,
                });
                let trail_end = if abs_word_end < body_end {
                    // `word_end + 1` can land mid-char when the char after the
                    // word is multi-byte — snap forward to the char boundary.
                    ceil_char_boundary(rope, abs_word_end + 1)
                } else {
                    abs_word_end
                };
                if trail_end > abs_word_end {
                    extra.push(HighlightSpan {
                        byte_range: abs_word_end..trail_end,
                        capture: Arc::from(m.marker.label_capture),
                        metadata: None,
                    });
                }
                cursor = m.word_end + (trail_end - abs_word_end);
                active = Some(m.marker);
            }
            let win_cursor_abs = window_start + cursor;
            if let Some(mw) = active
                && win_cursor_abs < body_end
            {
                extra.push(HighlightSpan {
                    byte_range: win_cursor_abs..body_end,
                    capture: Arc::from(mw.tail_capture),
                    metadata: None,
                });
            }

            prev_end = Some(comment_range.end);
        }

        spans.extend(extra);
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
                    .is_none_or(|b| !b.is_ascii_alphanumeric());
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

/// What a single line contributes to the seed walk.
enum SeedLine<'m> {
    /// Comment line with at least one marker — `m` (the line's LAST marker)
    /// is the seed and the walk can stop.
    Stop(&'m MarkerWord),
    /// Not a comment line — the inherited colour resets and the walk can
    /// stop, because nothing above the reset can matter.
    Reset,
    /// Comment line without markers — the inherited colour is unchanged and
    /// the walk keeps going up.
    Inherit,
}

/// Classify one line for the seed walk; see [`SeedLine`].
fn seed_line<'m>(line_bytes: &[u8], markers: &'m [MarkerWord]) -> SeedLine<'m> {
    let Some(del_off) = find_comment_delimiter(line_bytes) else {
        return SeedLine::Reset;
    };
    let body_start = delimiter_skip(line_bytes, del_off);
    if body_start < line_bytes.len()
        && let Some(last) = scan_markers(line_bytes, body_start, line_bytes.len(), markers).last()
    {
        SeedLine::Stop(last.marker)
    } else {
        SeedLine::Inherit
    }
}

/// Like [`seed_line`], but reads the line from a rope slice — borrowing the
/// bytes when the slice is contiguous, collecting into `scratch` when it
/// spans chunks.
fn classify_rope_line<'m>(
    slice: ropey::RopeSlice<'_>,
    markers: &'m [MarkerWord],
    scratch: &mut String,
) -> SeedLine<'m> {
    match slice.as_str() {
        Some(s) => seed_line(s.as_bytes(), markers),
        None => {
            scratch.clear();
            scratch.extend(slice.chunks());
            seed_line(scratch.as_bytes(), markers)
        }
    }
}

/// Return `true` when the two comment spans are on directly adjacent lines:
/// the gap contains only space / tab / `\r` and exactly one `\n`. A blank
/// line (two or more newlines) breaks the run, so an unrelated comment that
/// follows a blank line does not inherit the previous comment's marker color.
fn is_consecutive(bytes: &[u8], prev_end: usize, next_start: usize) -> bool {
    if prev_end > next_start || next_start > bytes.len() {
        return false;
    }
    let gap = &bytes[prev_end..next_start];
    let mut newlines = 0usize;
    for &b in gap {
        match b {
            b'\n' => {
                newlines += 1;
                if newlines > 1 {
                    return false;
                }
            }
            b' ' | b'\t' | b'\r' => {}
            _ => return false,
        }
    }
    newlines == 1
}

/// Largest UTF-8 char boundary `<= byte_idx` within `bytes` (clamped to the
/// slice length). The document text is valid UTF-8, so a char boundary is
/// exactly a byte that is not a continuation byte (`0b10xxxxxx`), and
/// `bytes.len()` is always a boundary. Safe (never panics) even for
/// non-UTF-8 input.
fn floor_char_boundary_bytes(bytes: &[u8], byte_idx: usize) -> usize {
    let mut i = byte_idx.min(bytes.len());
    while i > 0 && i < bytes.len() && bytes[i] & 0xC0 == 0x80 {
        i -= 1;
    }
    i
}

/// Smallest UTF-8 char boundary `>= byte_idx` within `bytes` (clamped to the
/// slice length). See [`floor_char_boundary_bytes`].
fn ceil_char_boundary_bytes(bytes: &[u8], byte_idx: usize) -> usize {
    let mut i = byte_idx.min(bytes.len());
    while i < bytes.len() && bytes[i] & 0xC0 == 0x80 {
        i += 1;
    }
    i
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Highlighter;
    use crate::runtime::{Grammar, GrammarLoader, LangSpec, QuerySource};
    use std::sync::{Arc, OnceLock};

    /// Tree-sitter-rust pinned rev for tests; matches what `bonsai.toml` ships.
    const RUST_GIT: &str = "https://github.com/tree-sitter/tree-sitter-rust";
    const RUST_REV: &str = "e86119bdb4968b9799f6a014ca2401c178d54b5f";

    /// Shared rust [`Grammar`], compiled once across all comment-marker tests
    /// in a single process. Persistent under \$XDG_CACHE_HOME so subsequent
    /// `cargo test --ignored` runs are warm.
    fn rust_grammar() -> Arc<Grammar> {
        static G: OnceLock<Arc<Grammar>> = OnceLock::new();
        G.get_or_init(|| {
            let meta = crate::test_support::pinned_manifest_meta();
            let loader = GrammarLoader::user_default(&meta).expect("XDG paths");
            let spec = LangSpec {
                git_url: RUST_GIT.into(),
                git_rev: RUST_REV.into(),
                subpath: None,
                extensions: vec!["rs".into()],
                c_files: vec!["src/parser.c".into(), "src/scanner.c".into()],
                query_source: QuerySource::Helix,
                query_subdir: None,
                source: None,
            };
            Arc::new(Grammar::load("rust", &spec, &loader, &meta).expect("rust grammar"))
        })
        .clone()
    }

    fn rust_comment_spans(src: &[u8]) -> Vec<HighlightSpan> {
        let mut h = Highlighter::new(rust_grammar()).unwrap();
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
    #[ignore = "network + compiler: clones tree-sitter-rust then builds it"]
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
    #[ignore = "network + compiler: clones tree-sitter-rust then builds it"]
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
    #[ignore = "network + compiler: clones tree-sitter-rust then builds it"]
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
    #[ignore = "network + compiler: clones tree-sitter-rust then builds it"]
    fn inheritance_breaks_on_blank_line() {
        // "// TODO foo\n\n// unrelated" — blank line between comments must
        // reset the active marker color; second comment gets no tail.
        let src = b"// TODO foo\n\n// unrelated";
        let ms = marker_spans(src);
        // Second comment starts at byte 13. Anything at or past 13 is inherited
        // tail leaking across the blank line.
        let leaked = ms
            .iter()
            .any(|s| s.capture() == "comment.marker.tail.todo" && s.byte_range.start >= 13);
        assert!(!leaked, "blank line should break inheritance; got {ms:#?}");
    }

    #[test]
    #[ignore = "network + compiler: clones tree-sitter-rust then builds it"]
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
    #[ignore = "network + compiler: clones tree-sitter-rust then builds it"]
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
    #[ignore = "network + compiler: clones tree-sitter-rust then builds it"]
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
    #[ignore = "network + compiler: clones tree-sitter-rust then builds it"]
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
    #[ignore = "network + compiler: clones tree-sitter-rust then builds it"]
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
    #[ignore = "network + compiler: clones tree-sitter-rust then builds it"]
    fn fix_marker_uses_fixme_capture() {
        let src = b"// FIX: broken";
        let ms = marker_spans(src);
        assert!(
            ms.iter().any(|s| s.capture() == "comment.marker.fixme"),
            "FIX should map to comment.marker.fixme; got {ms:#?}"
        );
    }

    #[test]
    #[ignore = "network + compiler: clones tree-sitter-rust then builds it"]
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
    #[ignore = "network + compiler: clones tree-sitter-rust then builds it"]
    fn warn_marker_emits_correct_capture() {
        let src = b"// WARN: danger";
        let ms = marker_spans(src);
        assert!(
            ms.iter().any(|s| s.capture() == "comment.marker.warn"),
            "expected warn label; got {ms:#?}"
        );
    }

    #[test]
    #[ignore = "network + compiler: clones tree-sitter-rust then builds it"]
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
    #[ignore = "network + compiler: clones tree-sitter-rust then builds it"]
    fn default_pass_is_same_as_new() {
        let a = CommentMarkerPass::new();
        let b = CommentMarkerPass::default();
        assert_eq!(a.inheritance, b.inheritance);
        assert_eq!(a.markers.len(), b.markers.len());
    }

    #[test]
    #[ignore = "network + compiler: clones tree-sitter-rust then builds it"]
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
    #[ignore = "network + compiler: clones tree-sitter-rust then builds it"]
    fn scan_markers_word_boundary_right() {
        // "TODOlist" — right boundary fails.
        let bytes = b"// TODOlist";
        let markers = default_markers();
        let found = scan_markers(bytes, 3, bytes.len(), markers);
        assert!(found.is_empty(), "TODOlist should not match");
    }

    #[test]
    #[ignore = "network + compiler: clones tree-sitter-rust then builds it"]
    fn is_consecutive_whitespace_only() {
        let bytes = b"// a\n// b";
        // prev_end=4 (after first comment), next_start=5 (start of second).
        assert!(is_consecutive(bytes, 4, 5));
    }

    #[test]
    #[ignore = "network + compiler: clones tree-sitter-rust then builds it"]
    fn is_consecutive_non_whitespace_between() {
        let bytes = b"// a\nlet x=1;\n// b";
        assert!(!is_consecutive(bytes, 4, 14));
    }

    /// Smoke: `apply_rope` produces at least the same number of marker spans
    /// as `apply` on a small TODO comment — no grammar required (we fabricate
    /// the comment span directly).
    #[test]
    fn apply_rope_todo_emits_marker_spans() {
        let src = "// TODO: refactor";
        let bytes = src.as_bytes();
        let rope = ropey::Rope::from_str(src);

        // Fabricate a comment span covering the full string.
        let comment_span = HighlightSpan {
            byte_range: 0..bytes.len(),
            capture: Arc::from("comment"),
            metadata: None,
        };

        let pass = CommentMarkerPass::new();

        let mut spans_bytes = vec![comment_span.clone()];
        pass.apply(&mut spans_bytes, bytes);

        let mut spans_rope = vec![comment_span];
        pass.apply_rope(&mut spans_rope, &rope);

        let marker_bytes: Vec<_> = spans_bytes
            .iter()
            .filter(|s| s.capture().starts_with("comment.marker"))
            .collect();
        let marker_rope: Vec<_> = spans_rope
            .iter()
            .filter(|s| s.capture().starts_with("comment.marker"))
            .collect();

        assert!(
            !marker_bytes.is_empty(),
            "bytes variant should find markers: {spans_bytes:#?}"
        );
        assert_eq!(
            marker_bytes.len(),
            marker_rope.len(),
            "rope and bytes variants must emit same number of marker spans"
        );
        for (b, r) in marker_bytes.iter().zip(marker_rope.iter()) {
            assert_eq!(b.capture, r.capture, "capture name mismatch");
            assert_eq!(b.byte_range, r.byte_range, "byte_range mismatch");
        }
    }

    /// Regression: label/trail span edges must land on UTF-8 char boundaries.
    /// `label_start = word_start - 1` and `trail_end = word_end + 1` are raw
    /// byte offsets; when the char immediately before/after the marker word is
    /// multi-byte (e.g. 'é'), the emitted span slices the row mid-char, which
    /// panics renderers that `row[range]` the string. Both the `apply` (bytes)
    /// and `apply_rope` paths must snap to the enclosing char boundaries.
    #[test]
    fn marker_span_edges_are_char_boundaries_around_multibyte() {
        // Label side: 'é' (2 bytes) directly before TODO; trail side: 'é'
        // directly after TODO; both at once.
        for src in ["// éTODO: fix", "// TODOé", "// xéTODOé: fix"] {
            let bytes = src.as_bytes();
            let rope = ropey::Rope::from_str(src);
            let comment_span = HighlightSpan {
                byte_range: 0..bytes.len(),
                capture: Arc::from("comment"),
                metadata: None,
            };

            let mut spans_bytes = vec![comment_span.clone()];
            CommentMarkerPass::new().apply(&mut spans_bytes, bytes);

            let mut spans_rope = vec![comment_span];
            CommentMarkerPass::new().apply_rope(&mut spans_rope, &rope);

            for spans in [&spans_bytes, &spans_rope] {
                let markers: Vec<_> = spans
                    .iter()
                    .filter(|s| s.capture().starts_with("comment.marker"))
                    .collect();
                assert!(
                    !markers.is_empty(),
                    "expected marker spans for {src:?}: {spans:#?}"
                );
                for s in markers {
                    assert!(
                        src.is_char_boundary(s.byte_range.start),
                        "{src:?}: span start {} not on a char boundary: {s:?}",
                        s.byte_range.start
                    );
                    assert!(
                        src.is_char_boundary(s.byte_range.end),
                        "{src:?}: span end {} not on a char boundary: {s:?}",
                        s.byte_range.end
                    );
                }
            }
        }
    }

    /// Regression: `seed_active`'s string-scan fallback used to hardcode a
    /// 2-byte delimiter skip (`del_off + 2`) after locating a comment
    /// delimiter, which is correct for `--`/`//`/`/*` but wrong for the
    /// 1-byte `#` delimiter — it eats one byte of the comment body along
    /// with the `#`, so `#TODO: ...` is scanned as `ODO: ...` and the
    /// `TODO` word never matches. A `#TODO:` line sitting just above a
    /// scrolled viewport should still seed the inherited marker state.
    #[test]
    fn seed_active_recognizes_one_byte_hash_delimiter() {
        let src = b"#TODO: fix this\n// next line\n";
        let first_comment_start = src
            .iter()
            .position(|&b| b == b'/')
            .expect("test fixture must contain a `//` comment");

        let pass = CommentMarkerPass::new();
        let active = pass.seed_active(src, first_comment_start);

        assert!(
            active.is_some_and(|m| m.word == "TODO"),
            "seed scan must recognize `#TODO:` (1-byte delimiter) just above \
             the viewport; got {:?}",
            active.map(|m| m.word)
        );
    }

    /// Companion: the 2-byte `//` delimiter path must keep working exactly
    /// as before — this pins the non-regression side of the fix.
    #[test]
    fn seed_active_still_recognizes_two_byte_slash_delimiter() {
        let src = b"// TODO: fix this\n// next line\n";
        let first_comment_start = src.len() - b"// next line\n".len();

        let pass = CommentMarkerPass::new();
        let active = pass.seed_active(src, first_comment_start);

        assert!(
            active.is_some_and(|m| m.word == "TODO"),
            "seed scan must still recognize `// TODO:` (2-byte delimiter); \
             got {:?}",
            active.map(|m| m.word)
        );
    }

    /// Regression: the fixed `CAP * 200` seed-window offset must not panic
    /// when it lands mid-way through a multi-byte char ('€' is 3 bytes, so
    /// `comment_start - 100_000` falls on a non-char-boundary here).
    #[test]
    fn apply_rope_seed_window_start_mid_multibyte_char() {
        let prefix = "€".repeat(34_000); // 102_000 bytes; boundaries at multiples of 3
        let comment = "// TODO x";
        let src = format!("{prefix}{comment}");
        let rope = ropey::Rope::from_str(&src);
        let comment_start = prefix.len(); // 102_000; 102_000 - 100_000 = 2_000 (mid-'€')

        let mut spans = vec![HighlightSpan {
            byte_range: comment_start..src.len(),
            capture: Arc::from("comment"),
            metadata: None,
        }];
        CommentMarkerPass::new().apply_rope(&mut spans, &rope);

        assert!(
            spans.iter().any(|s| s.capture() == "comment.marker.todo"),
            "expected TODO marker span: {spans:#?}"
        );
    }

    /// Regression: the seed scan must stop at the NEAREST preceding comment
    /// with a marker. A walk that keeps going to the top of the window and
    /// returns the farthest marker (or the first marker found from the top)
    /// would surface the FIXME above instead of the nearer TODO.
    #[test]
    fn seed_active_returns_nearest_preceding_marker() {
        let src = b"// FIXME: far above\n// filler comment\n// TODO: near\n// next line\n";
        let first_comment_start = src.len() - b"// next line\n".len();
        let pass = CommentMarkerPass::new();
        let active = pass.seed_active(src, first_comment_start);
        assert_eq!(
            active.map(|m| m.word),
            Some("TODO"),
            "nearest preceding marker must seed the colour, got {:?}",
            active.map(|m| m.word)
        );
    }

    /// A non-comment line between the marker and the comment resets the
    /// colour: the seed must be `None` even though a marker exists further
    /// up — "nearest" is only meaningful across consecutive comment lines.
    #[test]
    fn seed_active_reset_between_marker_and_comment_kills_seed() {
        let src = b"// FIXME: far above\nlet x = 1;\n// next line\n";
        let first_comment_start = src.len() - b"// next line\n".len();
        let pass = CommentMarkerPass::new();
        assert_eq!(
            pass.seed_active(src, first_comment_start).map(|m| m.word),
            None,
            "a non-comment line between the marker and the comment must reset the seed"
        );
    }

    /// Regression: `apply_rope` must emit byte-for-byte the same spans as the
    /// bytes-based `apply` for a buffer whose first comment sits below marker
    /// lines, and the exact spans are pinned. The first comment carries its
    /// own marker, so a broken seed window — one that mis-slices, skips the
    /// comment (window start past the comment), or panics — changes the
    /// emitted spans; the markers above the comment must not add anything.
    #[test]
    fn apply_rope_matches_apply_with_markers_above() {
        let src = "// FIXME: far above\n// TODO: near\n// next TODO line\n";
        let bytes = src.as_bytes();
        let rope = ropey::Rope::from_str(src);
        let first_comment_start = src.len() - "// next TODO line\n".len();
        let comment_span = HighlightSpan {
            byte_range: first_comment_start..src.len(),
            capture: Arc::from("comment"),
            metadata: None,
        };
        // Expected marker spans for the first comment's own TODO: label
        // `41..46` (TODO), trail `46..47` (the space after the word), tail
        // `47..52` (the rest of the line) — pinned exactly.
        let expected = vec![
            comment_span.clone(),
            HighlightSpan {
                byte_range: 41..46,
                capture: Arc::from("comment.marker.todo"),
                metadata: None,
            },
            HighlightSpan {
                byte_range: 46..47,
                capture: Arc::from("comment.marker.todo"),
                metadata: None,
            },
            HighlightSpan {
                byte_range: 47..52,
                capture: Arc::from("comment.marker.tail.todo"),
                metadata: None,
            },
        ];

        let mut spans_bytes = vec![comment_span.clone()];
        CommentMarkerPass::new().apply(&mut spans_bytes, bytes);

        let mut spans_rope = vec![comment_span.clone()];
        CommentMarkerPass::new().apply_rope(&mut spans_rope, &rope);

        assert_eq!(
            spans_bytes, spans_rope,
            "apply and apply_rope must emit identical spans in the same order"
        );
        assert_eq!(spans_rope, expected, "exact spans pinned");
    }
}
