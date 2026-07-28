//! Pure rope helpers — no editor, host, or vim state involved.
//!
//! These lived in `vim.rs` for historical reasons, which made the
//! mode-agnostic engine core (`substitute.rs`, `editor.rs`) appear to depend
//! on the vim discipline when it only ever wanted rope utilities. Hoisting
//! them here removes the last real engine-core → `vim::` call so `vim.rs` can
//! relocate into `hjkl-vim` (#267 / #265).

/// Return row `r` from a rope as an owned `String`, stripping the
/// trailing `\n` that ropey includes on non-final lines.
pub fn rope_line_to_str(rope: &ropey::Rope, r: usize) -> String {
    let s = rope.line(r).to_string();
    // ropey includes the newline; strip it so callers see bare content.
    if s.ends_with('\n') {
        s[..s.len() - 1].to_string()
    } else {
        s
    }
}

/// Join rows `lo..=hi` from a rope into a single `String` separated by
/// `\n`. Callers must ensure `lo <= hi < rope.len_lines()`.
pub fn rope_row_range_str(rope: &ropey::Rope, lo: usize, hi: usize) -> String {
    let n = rope.len_lines();
    let lo = lo.min(n.saturating_sub(1));
    let hi = hi.min(n.saturating_sub(1));
    if lo > hi {
        return String::new();
    }
    // Use byte-slice to grab the full range in one rope walk.
    let start_byte = rope.line_to_byte(lo);
    // End byte: start of line hi+1, minus the newline separator, or
    // len_bytes() when hi is the last line.
    let end_byte = if hi + 1 < n {
        // line_to_byte(hi+1) points just past line hi's separator; step back
        // one byte to drop it. That byte step is NOT enough on its own:
        // ropey's default `unicode_lines` feature also treats U+0085 (2
        // bytes) and U+2028 / U+2029 (3 bytes) as line breaks, so `- 1` lands
        // *inside* the separator and `byte_slice` panics on a
        // non-char-boundary index. Flooring to the enclosing char start snaps
        // back to the separator's first byte, which is exactly the end of the
        // row's content. `\r\n` is unaffected: `\n` begins a char, so the
        // floor is the identity there and CRLF rows keep their trailing `\r`.
        let step_back = rope.line_to_byte(hi + 1).saturating_sub(1);
        hjkl_buffer::floor_char_boundary(rope, step_back)
    } else {
        rope.len_bytes()
    };
    rope.byte_slice(start_byte..end_byte).to_string()
}

/// Snapshot all rows from a rope as `Vec<String>` (no trailing `\n`).
/// Use only when the caller truly needs mutable per-row access; prefer
/// rope iterators otherwise.
pub fn rope_to_lines_vec(rope: &ropey::Rope) -> Vec<String> {
    let n = rope.len_lines();
    (0..n).map(|r| rope_line_to_str(rope, r)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;

    #[test]
    fn line_to_str_strips_newline() {
        let rope = Rope::from_str("abc\ndef\n");
        assert_eq!(rope_line_to_str(&rope, 0), "abc");
        assert_eq!(rope_line_to_str(&rope, 1), "def");
    }

    #[test]
    fn line_to_str_final_line_without_newline() {
        let rope = Rope::from_str("abc\ndef");
        assert_eq!(rope_line_to_str(&rope, 1), "def");
    }

    #[test]
    fn row_range_str_joins_inclusive() {
        let rope = Rope::from_str("a\nb\nc\n");
        assert_eq!(rope_row_range_str(&rope, 0, 1), "a\nb");
        assert_eq!(rope_row_range_str(&rope, 1, 2), "b\nc");
    }

    #[test]
    fn row_range_str_single_row() {
        let rope = Rope::from_str("a\nb\nc");
        assert_eq!(rope_row_range_str(&rope, 1, 1), "b");
    }

    #[test]
    fn row_range_str_clamps_out_of_bounds() {
        let rope = Rope::from_str("a\nb");
        // hi past the end clamps to the last row rather than panicking.
        assert_eq!(rope_row_range_str(&rope, 0, 99), "a\nb");
    }

    #[test]
    fn row_range_str_empty_when_lo_gt_hi() {
        let rope = Rope::from_str("a\nb\nc");
        assert_eq!(rope_row_range_str(&rope, 2, 1), "");
    }

    #[test]
    fn to_lines_vec_snapshots_all_rows() {
        let rope = Rope::from_str("a\nb\nc");
        assert_eq!(rope_to_lines_vec(&rope), vec!["a", "b", "c"]);
    }

    /// ropey's default `unicode_lines` feature counts more than `\n` as a line
    /// break, and those separators are 2–3 bytes wide. Stepping back a fixed
    /// one byte from the next line's start lands *inside* the separator, and
    /// `byte_slice` panics on a non-char-boundary index. Found by the
    /// `handle_key` fuzz target ("range 6..7").
    #[test]
    fn row_range_str_multibyte_line_separators_do_not_panic() {
        for sep in ["\u{2028}", "\u{2029}", "\u{85}", "\u{0B}", "\u{0C}"] {
            let text = format!("abcdef{sep}ghijkl{sep}mnopqr");
            let rope = Rope::from_str(&text);
            // Every single-row and spanning range must be sliceable.
            for lo in 0..rope.len_lines() {
                for hi in lo..rope.len_lines() {
                    let _ = rope_row_range_str(&rope, lo, hi);
                }
            }
            assert_eq!(
                rope_row_range_str(&rope, 0, 0),
                "abcdef",
                "separator {sep:?} must not leak into the row content"
            );
        }
    }

    /// A CRLF file must keep its existing behaviour: flooring to a char
    /// boundary is a no-op mid-`\r\n` because `\n` starts a char.
    #[test]
    fn row_range_str_crlf_behaviour_unchanged() {
        let rope = Rope::from_str("a\r\nb");
        assert_eq!(rope_row_range_str(&rope, 0, 0), "a\r");
    }
}
