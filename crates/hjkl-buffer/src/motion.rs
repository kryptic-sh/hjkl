//! Motion vocabulary helpers.
//!
//! Patch C (0.0.30) relocated the 24 inherent vim motion helpers
//! that lived here onto [`hjkl_engine::motions`] free functions
//! over `&mut hjkl_buffer::View`. Motions don't belong on `View`
//! — they're computed over the buffer, not delegated to it; the
//! relocation is a step toward 0.1.0's full motion-as-trait-bound
//! generic-ification.
//!
//! What stays in this module: [`is_keyword_char`] — the
//! `iskeyword`-spec parser. Keyword classification is data over the
//! `iskeyword` string and a single `char`; it has no buffer
//! dependency, so the engine motions module re-exports it from here.

/// One parsed `iskeyword` token. The classification precedence is
/// exactly that of the original per-token decision tree so both
/// [`is_keyword_char`] and [`KeywordSpec`] agree byte-for-byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token {
    /// `@` — any alphabetic char.
    Alpha,
    /// `N-M` — decimal char-code range, inclusive.
    Range(u32, u32),
    /// bare integer `N` — single char code.
    Code(u32),
    /// single char — literal match.
    Literal(char),
}

impl Token {
    /// Classify one already-trimmed, non-empty token, or `None` for
    /// unrecognized tokens (which the matcher ignores). Precedence
    /// mirrors the original inline parser: `@`, then `N-M` range,
    /// then bare code, then single-char literal.
    fn parse(token: &str) -> Option<Token> {
        if token == "@" {
            return Some(Token::Alpha);
        }
        if let Some((lo, hi)) = token.split_once('-')
            && let (Ok(lo), Ok(hi)) = (lo.parse::<u32>(), hi.parse::<u32>())
        {
            return Some(Token::Range(lo, hi));
        }
        if let Ok(n) = token.parse::<u32>() {
            return Some(Token::Code(n));
        }
        let mut chars = token.chars();
        if let (Some(only), None) = (chars.next(), chars.next()) {
            return Some(Token::Literal(only));
        }
        None
    }

    #[inline]
    fn matches(self, c: char) -> bool {
        match self {
            Token::Alpha => c.is_alphabetic(),
            Token::Range(lo, hi) => (lo..=hi).contains(&(c as u32)),
            Token::Code(n) => c as u32 == n,
            Token::Literal(only) => c == only,
        }
    }
}

/// A vim-style `iskeyword` spec parsed once into its tokens.
///
/// The spec (e.g. `"@,48-57,_,192-255"`) changes only when the
/// option is set, but word motions classify every stepped-over char.
/// Pre-parsing with [`KeywordSpec::parse`] and reusing it via
/// [`KeywordSpec::matches`] avoids re-splitting/re-parsing the spec
/// string on every character. The boolean result is identical to
/// calling [`is_keyword_char`] with the same spec string.
#[derive(Debug, Clone, Default)]
pub struct KeywordSpec {
    tokens: Vec<Token>,
}

impl KeywordSpec {
    /// Parse a comma-separated `iskeyword` spec into a reusable
    /// matcher. Understood forms: `@` (any alphabetic), `_` (literal
    /// underscore), `N-M` (decimal char-code range, inclusive), bare
    /// integer `N` (single char code), single char (literal).
    /// Unknown tokens are dropped.
    pub fn parse(spec: &str) -> Self {
        let tokens = spec
            .split(',')
            .filter_map(|raw| {
                let token = raw.trim();
                if token.is_empty() {
                    None
                } else {
                    Token::parse(token)
                }
            })
            .collect();
        Self { tokens }
    }

    /// True if `c` matches any token in the pre-parsed spec.
    #[inline]
    pub fn matches(&self, c: char) -> bool {
        self.tokens.iter().any(|token| token.matches(c))
    }
}

/// Match `c` against a vim-style `iskeyword` spec. Tokens are
/// comma-separated; understood forms: `@` (any alphabetic),
/// `_` (literal underscore), `N-M` (decimal char-code range, inclusive),
/// bare integer `N` (single char code), single ASCII punctuation char
/// (literal). Unknown tokens are ignored.
///
/// This is a zero-allocation convenience for one-off checks; hot
/// per-character loops should pre-parse once with [`KeywordSpec`].
pub fn is_keyword_char(c: char, spec: &str) -> bool {
    spec.split(',').any(|raw| {
        let token = raw.trim();
        !token.is_empty() && Token::parse(token).is_some_and(|t| t.matches(c))
    })
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
