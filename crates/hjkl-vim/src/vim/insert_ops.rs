//! Vim FSM: insert ops.
//!
//! Split out of the monolithic `vim.rs` (#267 follow-up).

/// Return the filetype-gated autopair close character for `open`, or `None`
/// when no pairing applies.
///
/// Rules:
/// - `(` → `)`, `[` → `]`, `{` → `}` always.
/// - `"` → `"` and `` ` `` → `` ` `` always, EXCEPT when the previous two
///   characters are the same quote — typing the third `` ` `` of a markdown
///   code-fence or the third `"` of a Python triple-quoted string must
///   emit a bare quote (no close) so the result is `` ``` `` / `"""` and
///   not `` ```` `` / `""""`.
/// - `<` → `>` only for HTML/XML family filetypes.
/// - `'` → `'` unless the character immediately before the cursor is
///   `[A-Za-z]` (prose apostrophe guard — "don't" stays "don't"), AND the
///   same triple-quote guard as `"` / `` ` ``.
pub(crate) fn autopair_close_for(
    ch: char,
    filetype: &str,
    prev_char: Option<char>,
    prev2_char: Option<char>,
) -> Option<char> {
    // Triple-quote guard — applies to ", `, and ' (the three quote chars
    // that get same-char pairing). When the previous two characters are
    // both this same quote, treat the third keystroke as a bare insert so
    // the user lands on `` ``` `` / `"""` / `'''` without a stray fourth
    // quote dangling after the cursor.
    let is_triple_quote_third =
        matches!(ch, '"' | '`' | '\'') && prev_char == Some(ch) && prev2_char == Some(ch);

    match ch {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '"' => {
            if is_triple_quote_third {
                None
            } else {
                Some('"')
            }
        }
        '`' => {
            if is_triple_quote_third {
                None
            } else {
                Some('`')
            }
        }
        '<' => {
            if is_html_filetype(filetype) {
                Some('>')
            } else {
                None
            }
        }
        '\'' => {
            if is_triple_quote_third {
                return None;
            }
            // Prose guard: skip pairing when the previous char is a letter
            // (covers "don't", "it's", etc.).
            if prev_char.map(|c| c.is_ascii_alphabetic()).unwrap_or(false) {
                None
            } else {
                Some('\'')
            }
        }
        _ => None,
    }
}
/// Detect a markdown / doc-comment code-fence opener on the current line.
///
/// Returns `Some(fence)` (the backtick run that should be used as the
/// closing fence) when:
/// - The cursor is at the end of the visible line (`cursor_col` equals the
///   line's char count).
/// - The line, after leading whitespace, begins with 3+ backticks followed
///   by a non-empty language tag matching `[A-Za-z0-9_+-]+` and nothing
///   else (no trailing space, no extra text).
///
/// The language tag requirement is deliberate: a bare ` ``` ` could be
/// either an opener OR a closer, and we don't track fence parity here.
/// Requiring a tag means we only fire when the user is clearly opening a
/// fence (` ```rust `, ` ```ts `, etc.).
pub(crate) fn detect_code_fence_opener(line: &str, cursor_col: usize) -> Option<String> {
    if cursor_col != line.chars().count() {
        return None;
    }
    let trimmed = line.trim_start();
    let backtick_run = trimmed.chars().take_while(|c| *c == '`').count();
    if backtick_run < 3 {
        return None;
    }
    let rest = &trimmed[backtick_run..];
    if rest.is_empty() {
        return None;
    }
    let all_lang_chars = rest
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '+' || c == '-');
    if !all_lang_chars {
        return None;
    }
    Some("`".repeat(backtick_run))
}
/// Filetypes that get HTML/XML-family treatment (`<` pairing + tag autoclose).
pub(crate) fn is_html_filetype(ft: &str) -> bool {
    matches!(
        ft,
        "html" | "xml" | "svg" | "jsx" | "tsx" | "vue" | "svelte"
    )
}
#[cfg(test)]
mod abbrev_tests {
    use crate::vim::{Abbrev, AbbrevKind, AbbrevTrigger, abbrev_kind, try_abbrev_expand};
    use AbbrevKind::{End, Full, NonKw};

    const ISK: &str = "@,48-57,_,192-255"; // default iskeyword

    fn make_abbrev(lhs: &str, rhs: &str) -> Abbrev {
        Abbrev {
            lhs: lhs.to_string(),
            rhs: rhs.to_string(),
            insert: true,
            cmdline: false,
            noremap: false,
        }
    }

    fn expand(
        abbrevs: &[Abbrev],
        before: &str,
        mincol: usize,
        trig: AbbrevTrigger,
    ) -> Option<(usize, String)> {
        try_abbrev_expand(abbrevs, before, mincol, trig, ISK)
    }

    // ── abbrev_type classification ────────────────────────────────────────────

    #[test]
    fn fullid_all_keyword_chars() {
        assert_eq!(abbrev_kind("teh", ISK), Full);
        assert_eq!(abbrev_kind("abc123", ISK), Full);
        assert_eq!(abbrev_kind("_foo", ISK), Full);
    }

    #[test]
    fn endid_ends_with_kw_has_nonkw() {
        assert_eq!(abbrev_kind("#i", ISK), End);
        assert_eq!(abbrev_kind("#include", ISK), End);
    }

    #[test]
    fn nonid_ends_with_nonkw() {
        assert_eq!(abbrev_kind(";;", ISK), NonKw);
        assert_eq!(abbrev_kind("->", ISK), NonKw);
    }

    // ── full-id expansion ─────────────────────────────────────────────────────

    #[test]
    fn fullid_expands_on_space_trigger() {
        let abbrevs = [make_abbrev("teh", "the")];
        let r = expand(&abbrevs, "teh", 0, AbbrevTrigger::NonKeyword(' '));
        assert_eq!(r, Some((3, "the".to_string())));
    }

    #[test]
    fn fullid_expands_on_esc_trigger() {
        let abbrevs = [make_abbrev("teh", "the")];
        let r = expand(&abbrevs, "teh", 0, AbbrevTrigger::Esc);
        assert_eq!(r, Some((3, "the".to_string())));
    }

    #[test]
    fn fullid_expands_on_cr_trigger() {
        let abbrevs = [make_abbrev("teh", "the")];
        let r = expand(&abbrevs, "teh", 0, AbbrevTrigger::Cr);
        assert_eq!(r, Some((3, "the".to_string())));
    }

    #[test]
    fn fullid_expands_on_ctrl_bracket() {
        let abbrevs = [make_abbrev("teh", "the")];
        let r = expand(&abbrevs, "teh", 0, AbbrevTrigger::CtrlBracket);
        assert_eq!(r, Some((3, "the".to_string())));
    }

    #[test]
    fn fullid_does_not_expand_on_keyword_trigger() {
        // Typing a keyword char after "teh" would extend the word — no expand.
        let abbrevs = [make_abbrev("teh", "the")];
        let r = expand(&abbrevs, "teh", 0, AbbrevTrigger::NonKeyword('a'));
        // 'a' is keyword — should not trigger
        assert_eq!(r, None);
    }

    #[test]
    fn fullid_no_expand_when_lhs_not_at_end() {
        let abbrevs = [make_abbrev("teh", "the")];
        // "ateh" — 'a' before is keyword, so skip.
        let r = expand(&abbrevs, "ateh", 0, AbbrevTrigger::NonKeyword(' '));
        assert_eq!(r, None);
    }

    #[test]
    fn fullid_expands_after_nonkw_prefix() {
        let abbrevs = [make_abbrev("teh", "the")];
        // "!teh" — '!' before is non-keyword → expand.
        let r = expand(&abbrevs, "!teh", 0, AbbrevTrigger::NonKeyword(' '));
        assert_eq!(r, Some((3, "the".to_string())));
    }

    #[test]
    fn fullid_single_char_no_expand_after_nonblank_nonkw() {
        let abbrevs = [make_abbrev("a", "b")];
        // "!a" — '!' is non-blank non-keyword before single-char lhs → no expand.
        let r = expand(&abbrevs, "!a", 0, AbbrevTrigger::NonKeyword(' '));
        assert_eq!(r, None);
    }

    #[test]
    fn fullid_single_char_expands_after_space() {
        let abbrevs = [make_abbrev("a", "b")];
        // " a" — space before single-char lhs → expand.
        let r = expand(&abbrevs, " a", 0, AbbrevTrigger::NonKeyword(' '));
        assert_eq!(r, Some((1, "b".to_string())));
    }

    // ── mincol: pre-existing text must not be consumed ────────────────────────

    #[test]
    fn mincol_blocks_consuming_preexisting_text() {
        let abbrevs = [make_abbrev("teh", "the")];
        // "teh" is at cols 0..3, but insert started at col 3 → no match.
        let r = expand(&abbrevs, "teh", 3, AbbrevTrigger::NonKeyword(' '));
        assert_eq!(r, None);
    }

    #[test]
    fn mincol_allows_match_starting_at_mincol() {
        let abbrevs = [make_abbrev("teh", "the")];
        // Existing text "!! " at 0..3, then user typed "teh" → mincol=3.
        // The char before the lhs is ' ' (non-keyword), so full-id expands.
        let r = expand(&abbrevs, "!! teh", 3, AbbrevTrigger::NonKeyword(' '));
        assert_eq!(r, Some((3, "the".to_string())));
    }

    // ── end-id expansion ──────────────────────────────────────────────────────

    #[test]
    fn endid_expands_on_space_trigger() {
        let abbrevs = [make_abbrev("#i", "#include")];
        let r = expand(&abbrevs, "#i", 0, AbbrevTrigger::NonKeyword(' '));
        assert_eq!(r, Some((2, "#include".to_string())));
    }

    #[test]
    fn endid_expands_on_esc_trigger() {
        let abbrevs = [make_abbrev("#i", "#include")];
        let r = expand(&abbrevs, "#i", 0, AbbrevTrigger::Esc);
        assert_eq!(r, Some((2, "#include".to_string())));
    }

    // ── non-id expansion ──────────────────────────────────────────────────────

    #[test]
    fn nonid_expands_on_esc_trigger() {
        let abbrevs = [make_abbrev(";;", "std::endl;")];
        let r = expand(&abbrevs, ";;", 0, AbbrevTrigger::Esc);
        assert_eq!(r, Some((2, "std::endl;".to_string())));
    }

    #[test]
    fn nonid_expands_on_cr_trigger() {
        let abbrevs = [make_abbrev(";;", "std::endl;")];
        let r = expand(&abbrevs, ";;", 0, AbbrevTrigger::Cr);
        assert_eq!(r, Some((2, "std::endl;".to_string())));
    }

    #[test]
    fn nonid_does_not_expand_on_nonkw_trigger() {
        // non-id abbreviations must NOT expand on regular typed chars like space.
        let abbrevs = [make_abbrev(";;", "std::endl;")];
        let r = expand(&abbrevs, ";;", 0, AbbrevTrigger::NonKeyword(' '));
        assert_eq!(r, None);
    }

    #[test]
    fn nonid_expands_on_ctrl_bracket() {
        let abbrevs = [make_abbrev(";;", "std::endl;")];
        let r = expand(&abbrevs, ";;", 0, AbbrevTrigger::CtrlBracket);
        assert_eq!(r, Some((2, "std::endl;".to_string())));
    }

    // ── multiword rhs ─────────────────────────────────────────────────────────

    #[test]
    fn multiword_rhs_expansion() {
        let abbrevs = [make_abbrev("hw", "hello world")];
        let r = expand(&abbrevs, "hw", 0, AbbrevTrigger::NonKeyword(' '));
        assert_eq!(r, Some((2, "hello world".to_string())));
    }

    // ── empty / no match ─────────────────────────────────────────────────────

    #[test]
    fn no_match_returns_none() {
        let abbrevs = [make_abbrev("teh", "the")];
        let r = expand(&abbrevs, "xyz", 0, AbbrevTrigger::NonKeyword(' '));
        assert_eq!(r, None);
    }

    #[test]
    fn empty_abbrevs_returns_none() {
        let r = expand(&[], "teh", 0, AbbrevTrigger::NonKeyword(' '));
        assert_eq!(r, None);
    }

    #[test]
    fn empty_before_text_returns_none() {
        let abbrevs = [make_abbrev("teh", "the")];
        let r = expand(&abbrevs, "", 0, AbbrevTrigger::NonKeyword(' '));
        assert_eq!(r, None);
    }
}
