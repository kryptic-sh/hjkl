//! Shared search-match scanning.
//!
//! One canonical "pattern → match ranges within a line" implementation for
//! every consumer that needs to know WHERE a search pattern matches:
//!
//! - `hjkl-engine`'s `SearchState` per-row match cache (`n`/`N` navigation),
//! - `hjkl-buffer-tui`'s hlsearch painting (`BufferView::row_search_ranges`),
//! - the app's quickfix-dock match overlay (highlighting the `:grep` pattern
//!   inside each entry's message text).
//!
//! All three previously (or would otherwise) hand-roll the same
//! `find_iter → (start, end)` expression; keeping it here — the lowest crate
//! they all already depend on — guarantees they can never disagree about
//! what counts as a match.

/// Byte ranges (`(start, end)`, half-open) of every non-overlapping match of
/// `re` in `line`, in order. Empty when nothing matches.
///
/// BYTE offsets, matching `regex::Match` — callers that paint per-cell
/// (charwise columns) convert at their own boundary (see
/// `BufferView::row_search_ranges` in `hjkl-buffer-tui`); callers that feed
/// byte-ranged spans (`hjkl_buffer::Span`, engine syntax spans) use them
/// directly.
pub fn search_match_ranges(re: &regex::Regex, line: &str) -> Vec<(usize, usize)> {
    re.find_iter(line).map(|m| (m.start(), m.end())).collect()
}

#[cfg(test)]
mod tests {
    use super::search_match_ranges;

    #[test]
    fn ranges_are_byte_offsets_in_order() {
        let re = regex::Regex::new("ab").unwrap();
        assert_eq!(search_match_ranges(&re, "ab cd ab"), vec![(0, 2), (6, 8)]);
    }

    #[test]
    fn no_match_is_empty() {
        let re = regex::Regex::new("zzz").unwrap();
        assert!(search_match_ranges(&re, "ab cd").is_empty());
    }

    #[test]
    fn multibyte_prefix_yields_byte_not_char_offsets() {
        // "é" is 2 bytes — a match after it must report BYTE offsets.
        let re = regex::Regex::new("x").unwrap();
        assert_eq!(search_match_ranges(&re, "éx"), vec![(2, 3)]);
    }
}
