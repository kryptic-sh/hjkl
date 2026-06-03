//! Subsequence fuzzy scorer for the hjkl editor stack.
//!
//! Provides [`score`] — a single function that returns a relevance score
//! and char-index match positions for a needle/haystack pair. Used by
//! `hjkl-picker` and any other crate that needs fuzzy ranking without
//! pulling in the full picker subsystem.

#![forbid(unsafe_code)]

/// Subsequence-based fuzzy score. Returns `None` when not all needle
/// characters appear (in order) in the haystack.
///
/// On success returns `Some((score, positions))` where `positions` is
/// a list of **char indices** (not byte indices) in `haystack` where
/// each character of `needle` matched, in order. Char indices are used
/// so the renderer can walk `haystack.chars().enumerate()` and check
/// membership directly without any byte-to-char conversion.
///
/// Bonuses:
/// - `+8` per match at a word boundary (start, after `/`, `_`, `-`,
///   `.`, ` `).
/// - `+5` per consecutive match (run of adjacent matches).
/// - `+1` base hit per matched char.
///
/// Penalty: `-len(haystack)/8` so shorter overall paths win on ties.
pub fn score(haystack: &str, needle: &str) -> Option<(i64, Vec<usize>)> {
    if needle.is_empty() {
        return Some((0, Vec::new()));
    }
    // Substring fast path. The greedy subsequence loop below picks the
    // FIRST occurrence of each needle char, so for "main" against
    // "/home/mxaddict/.../main/" it lights up `[m]` in "ho[m]e", `[a]`
    // in "mx[a]ddict", etc. — instead of the contiguous "main" at the
    // tail. When the needle appears literally, return the contiguous
    // run with a heavy bonus so it always outranks scattered matches.
    if let Some(byte_idx) = haystack.find(needle) {
        let start_char = haystack[..byte_idx].chars().count();
        let needle_len = needle.chars().count();
        let positions: Vec<usize> = (start_char..start_char + needle_len).collect();
        let prev_ch = haystack[..byte_idx].chars().last();
        let at_boundary = byte_idx == 0 || matches!(prev_ch, Some('/' | '_' | '-' | '.' | ' '));
        let mut total: i64 = needle_len as i64;
        if at_boundary {
            total += 8;
        }
        total += (needle_len as i64 - 1).max(0) * 5; // consecutive bonus
        total += 100; // ensure contiguous beats scattered subsequence
        total -= haystack.chars().count() as i64 / 8;
        return Some((total, positions));
    }
    let mut needle_chars = needle.chars().peekable();
    let mut total: i64 = 0;
    let mut prev_match = false;
    let mut positions: Vec<usize> = Vec::new();
    let mut prev_ch: Option<char> = None;
    for (ci, ch) in haystack.chars().enumerate() {
        if let Some(&nc) = needle_chars.peek() {
            if ch == nc {
                if prev_match {
                    total += 5;
                }
                let at_boundary = prev_ch
                    .map(|p| matches!(p, '/' | '_' | '-' | '.' | ' '))
                    .unwrap_or(true);
                if at_boundary {
                    total += 8;
                }
                total += 1;
                prev_match = true;
                positions.push(ci);
                needle_chars.next();
            } else {
                prev_match = false;
            }
        }
        prev_ch = Some(ch);
    }
    if needle_chars.peek().is_some() {
        return None;
    }
    total -= haystack.chars().count() as i64 / 8;
    Some((total, positions))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn score_subsequence_match() {
        assert!(score("src/main.rs", "main").is_some());
        assert!(score("src/main.rs", "smr").is_some());
        assert!(score("src/main.rs", "xyz").is_none());
    }

    #[test]
    fn score_word_boundary_beats_mid_word() {
        let (a, _) = score("src/main.rs", "main").unwrap();
        let (b, _) = score("src/domain.rs", "main").unwrap();
        assert!(a > b);
    }

    #[test]
    fn score_shorter_wins_on_ties() {
        let (a, _) = score("a/b/foo.rs", "foo").unwrap();
        let (b, _) = score("a/b/c/d/e/foo.rs", "foo").unwrap();
        assert!(a > b);
    }

    #[test]
    fn score_returns_match_positions() {
        // 'f' is at char index 0, 'b' is at char index 4 in "foo_bar".
        let (_, positions) = score("foo_bar", "fb").unwrap();
        assert_eq!(positions, vec![0, 4]);
    }

    #[test]
    fn score_match_positions_skip_unmatched() {
        // 'h' at index 0, 'w' at index 6 in "hello world".
        let (_, positions) = score("hello world", "hw").unwrap();
        assert_eq!(positions, vec![0, 6]);
    }

    #[test]
    fn substring_match_returns_contiguous_positions() {
        // Regression: greedy subsequence highlighted scattered chars
        // (m in "home", a in "mxaddict", i in "addict", n in "main")
        // instead of the contiguous "main" run at the tail.
        let (_, positions) = score("/home/mxaddict/foo/main/lib.rs", "main").unwrap();
        // "main" starts at char index 19 in this haystack.
        assert_eq!(positions, vec![19, 20, 21, 22]);
    }

    #[test]
    fn substring_match_outranks_scattered_subsequence() {
        // A path where "main" appears contiguously must score higher
        // than one where the chars only appear scattered.
        let (a, _) = score("/p/main.rs", "main").unwrap();
        let (b, _) = score("/m/a/i/extra/n.rs", "main").unwrap();
        assert!(a > b, "contiguous {a} must beat scattered {b}");
    }
}
