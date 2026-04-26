//! Motion vocabulary helpers.
//!
//! Patch C (0.0.30) relocated the 24 inherent vim motion helpers
//! that lived here onto [`hjkl_engine::motions`] free functions
//! over `&mut hjkl_buffer::Buffer`. Per [SPEC.md], "motions don't
//! belong on `Buffer` — they're computed over the buffer, not
//! delegated to it"; the relocation is a step toward 0.1.0's full
//! motion-as-trait-bound generic-ification.
//!
//! What stays in this module: [`is_keyword_char`] — the
//! `iskeyword`-spec parser. Keyword classification is data over the
//! `iskeyword` string and a single `char`; it has no buffer
//! dependency, so the engine motions module re-exports it from here.
//!
//! [SPEC.md]: https://github.com/kryptic-sh/hjkl/blob/main/crates/hjkl-engine/SPEC.md

/// Match `c` against a vim-style `iskeyword` spec. Tokens are
/// comma-separated; understood forms: `@` (any alphabetic),
/// `_` (literal underscore), `N-M` (decimal char-code range, inclusive),
/// bare integer `N` (single char code), single ASCII punctuation char
/// (literal). Unknown tokens are ignored.
pub fn is_keyword_char(c: char, spec: &str) -> bool {
    for raw in spec.split(',') {
        let token = raw.trim();
        if token.is_empty() {
            continue;
        }
        if token == "@" {
            if c.is_alphabetic() {
                return true;
            }
            continue;
        }
        if let Some((lo, hi)) = token.split_once('-')
            && let (Ok(lo), Ok(hi)) = (lo.parse::<u32>(), hi.parse::<u32>())
        {
            if (lo..=hi).contains(&(c as u32)) {
                return true;
            }
            continue;
        }
        if let Ok(n) = token.parse::<u32>() {
            if c as u32 == n {
                return true;
            }
            continue;
        }
        let mut chars = token.chars();
        if let (Some(only), None) = (chars.next(), chars.next())
            && c == only
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iskeyword_alphabetic_via_at() {
        assert!(is_keyword_char('a', "@"));
        assert!(is_keyword_char('Z', "@"));
        assert!(!is_keyword_char('1', "@"));
    }

    #[test]
    fn iskeyword_numeric_range() {
        assert!(is_keyword_char('0', "48-57"));
        assert!(is_keyword_char('9', "48-57"));
        assert!(!is_keyword_char('a', "48-57"));
    }

    #[test]
    fn iskeyword_literal_punctuation() {
        assert!(is_keyword_char('_', "_"));
        assert!(!is_keyword_char('.', "_"));
    }

    #[test]
    fn iskeyword_default_spec() {
        // Matches vim default `@,48-57,_,192-255` and engine's
        // `Settings::default()`.
        let spec = "@,48-57,_,192-255";
        assert!(is_keyword_char('a', spec));
        assert!(is_keyword_char('5', spec));
        assert!(is_keyword_char('_', spec));
        assert!(!is_keyword_char(' ', spec));
        assert!(!is_keyword_char('.', spec));
    }
}
