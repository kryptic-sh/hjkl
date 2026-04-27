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
}
