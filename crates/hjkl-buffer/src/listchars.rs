//! Invisible-character rendering configuration.
//!
//! [`ListChars`] holds the glyph substitutions used when
//! `:set list` is active. Mirrors vim's `listchars` option.

/// Invisibles rendering configuration. Matches vim's `:set listchars`.
///
/// When `:set list` is on, the render layer substitutes whitespace characters
/// with the glyphs configured here. `None` fields mean "no substitution /
/// not rendered".
///
/// Default matches vim's built-in default: `tab:^I,eol:$`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListChars {
    /// Leading char of a tab expansion (required). E.g. `>` in `tab:>-`.
    pub tab_lead: char,
    /// Fill char repeated to next tabstop. `None` = single-glyph tab (no fill).
    pub tab_fill: Option<char>,
    /// Substitution for regular spaces. `None` = no substitution (vim default).
    pub space: Option<char>,
    /// Substitution for trailing whitespace. `None` = falls back to `space` or no render.
    pub trail: Option<char>,
    /// Marker appended after the last char on each line. `None` = no marker.
    pub eol: Option<char>,
    /// Substitution for non-breaking spaces (`\u{00a0}`). `None` = no substitution.
    pub nbsp: Option<char>,
    /// Char shown at the right edge when a line extends beyond the viewport
    /// (no-wrap mode). `None` = no marker.
    /// TODO: deferred — requires viewport edge integration.
    pub extends: Option<char>,
    /// Char shown at the left edge when the viewport is scrolled right past
    /// the line start. `None` = no marker.
    /// TODO: deferred — requires viewport edge integration.
    pub precedes: Option<char>,
}

impl Default for ListChars {
    fn default() -> Self {
        // vim built-in default: tab:^I,eol:$
        Self {
            tab_lead: '^',
            tab_fill: Some('I'),
            space: None,
            trail: None,
            eol: Some('$'),
            nbsp: None,
            extends: None,
            precedes: None,
        }
    }
}

impl ListChars {
    /// Parse a vim-style `listchars` value string.
    ///
    /// Accepts comma-separated `key:value` pairs where value is one or two
    /// chars (UTF-8). `tab` is the only key that may have two chars
    /// (`tab:lead_fill`); all others take exactly one char.
    ///
    /// Returns `Err(String)` with a diagnostic on unknown keys or bad values.
    pub fn parse(s: &str) -> Result<Self, String> {
        // Start from a blank slate (all None). The `tab` key is required
        // for the resulting value to be valid; if the caller omits it the
        // existing tab_lead/tab_fill remain at the blank-slate defaults
        // (`^` + `I`) which matches vim's initial default.
        let mut lc = Self {
            tab_lead: '^',
            tab_fill: Some('I'),
            space: None,
            trail: None,
            eol: None,
            nbsp: None,
            extends: None,
            precedes: None,
        };
        for raw_part in s.split(',') {
            // Only trim leading whitespace (not trailing — a trailing space
            // is a valid single-char value, e.g. `tab:→ ` where space is
            // the fill char).
            let part = raw_part.trim_start();
            if part.is_empty() {
                continue;
            }
            let (key, val) = part
                .split_once(':')
                .ok_or_else(|| format!("listchars: missing `:` in `{part}`"))?;
            let chars: Vec<char> = val.chars().collect();
            match key {
                "tab" => match chars.len() {
                    1 => {
                        lc.tab_lead = chars[0];
                        lc.tab_fill = None;
                    }
                    2 => {
                        lc.tab_lead = chars[0];
                        lc.tab_fill = Some(chars[1]);
                    }
                    n => {
                        return Err(format!(
                            "listchars: `tab` value must be 1 or 2 chars, got {n}"
                        ));
                    }
                },
                "space" => lc.space = Some(one_char(key, &chars)?),
                "trail" => lc.trail = Some(one_char(key, &chars)?),
                "eol" => lc.eol = Some(one_char(key, &chars)?),
                "nbsp" => lc.nbsp = Some(one_char(key, &chars)?),
                "extends" => lc.extends = Some(one_char(key, &chars)?),
                "precedes" => lc.precedes = Some(one_char(key, &chars)?),
                other => {
                    return Err(format!("listchars: unknown key `{other}`"));
                }
            }
        }
        Ok(lc)
    }

    /// Canonical string form for `:set listchars?`.
    ///
    /// Emits only the fields that are set (non-None), always in the order:
    /// `tab`, `space`, `trail`, `eol`, `nbsp`, `extends`, `precedes`.
    pub fn to_canonical_string(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        // tab is always present
        if let Some(fill) = self.tab_fill {
            parts.push(format!("tab:{}{}", self.tab_lead, fill));
        } else {
            parts.push(format!("tab:{}", self.tab_lead));
        }
        if let Some(ch) = self.space {
            parts.push(format!("space:{ch}"));
        }
        if let Some(ch) = self.trail {
            parts.push(format!("trail:{ch}"));
        }
        if let Some(ch) = self.eol {
            parts.push(format!("eol:{ch}"));
        }
        if let Some(ch) = self.nbsp {
            parts.push(format!("nbsp:{ch}"));
        }
        if let Some(ch) = self.extends {
            parts.push(format!("extends:{ch}"));
        }
        if let Some(ch) = self.precedes {
            parts.push(format!("precedes:{ch}"));
        }
        parts.join(",")
    }
}

/// Extract exactly one char from `chars`, returning an error if count != 1.
fn one_char(key: &str, chars: &[char]) -> Result<char, String> {
    match chars.len() {
        1 => Ok(chars[0]),
        n => Err(format!(
            "listchars: `{key}` value must be exactly 1 char, got {n}"
        )),
    }
}

/// Apply listchars substitutions to a line string.
///
/// When `list` is false, returns `Cow::Borrowed(line)` with no allocation.
/// When `list` is true, walks the line and substitutes:
/// - `\t` → `tab_lead` + `tab_fill` × (tabstop - col % tabstop - 1)
/// - trailing spaces → `trail` glyph (if `Some`), else `space` glyph (if `Some`)
/// - end-of-line → `eol` glyph (if `Some`) appended after all chars
/// - `\u{00a0}` → `nbsp` glyph (if `Some`)
/// - regular spaces → `space` glyph (if `Some`)
///
/// Note: extends/precedes (viewport edge markers) are deferred — handled by
/// the renderer at the cell-paint level, not pre-processed here.
pub fn apply_listchars<'a>(
    line: &'a str,
    lc: &ListChars,
    list: bool,
    tabstop: usize,
) -> std::borrow::Cow<'a, str> {
    if !list {
        return std::borrow::Cow::Borrowed(line);
    }

    // Find the index of the first trailing whitespace char.
    // "trailing whitespace" = spaces/tabs at the end of the line that would
    // be rendered with `trail` glyph.
    let trimmed_end = line.trim_end_matches([' ', '\t']).len();

    let mut out = String::with_capacity(line.len() + 8);
    let mut col: usize = 0; // visible column counter (for tab expansion)

    for (byte_idx, ch) in line.char_indices() {
        let is_trailing = byte_idx >= trimmed_end;
        match ch {
            '\t' => {
                let spaces = tabstop - (col % tabstop);
                // tab_lead is always the first cell
                out.push(lc.tab_lead);
                col += 1;
                // fill remaining cells
                let fill_count = spaces.saturating_sub(1);
                if let Some(fill) = lc.tab_fill {
                    for _ in 0..fill_count {
                        out.push(fill);
                        col += 1;
                    }
                } else {
                    // single-glyph tab: pad with spaces to honour tabstop
                    for _ in 0..fill_count {
                        out.push(' ');
                        col += 1;
                    }
                }
            }
            ' ' => {
                let sub = if is_trailing {
                    lc.trail.or(lc.space).unwrap_or(' ')
                } else {
                    lc.space.unwrap_or(' ')
                };
                out.push(sub);
                col += 1;
            }
            '\u{00a0}' => {
                out.push(lc.nbsp.unwrap_or('\u{00a0}'));
                col += 1;
            }
            other => {
                out.push(other);
                col += unicode_width(other);
            }
        }
    }

    // Append eol marker
    if let Some(eol) = lc.eol {
        out.push(eol);
    }

    std::borrow::Cow::Owned(out)
}

/// Unicode display width for a char (1 for most, 2 for CJK wide chars, 0 for controls).
#[inline]
fn unicode_width(ch: char) -> usize {
    // Use a simple approximation: CJK wide = 2, everything else = 1.
    // This avoids adding unicode-width as a direct dep here; buffer-tui
    // uses the real UnicodeWidthChar for rendering.
    if is_wide(ch) { 2 } else { 1 }
}

/// Very small is_wide predicate covering the most common CJK blocks.
#[inline]
fn is_wide(ch: char) -> bool {
    matches!(ch,
        '\u{1100}'..='\u{115F}'   // Hangul Jamo
        | '\u{2E80}'..='\u{303E}' // CJK Radicals
        | '\u{3041}'..='\u{33BF}' // Hiragana/Katakana/CJK
        | '\u{33FF}'..='\u{A4CF}' // CJK Unified
        | '\u{A960}'..='\u{A97F}' // Hangul extension
        | '\u{AC00}'..='\u{D7FF}' // Hangul Syllables
        | '\u{F900}'..='\u{FAFF}' // CJK Compatibility
        | '\u{FE10}'..='\u{FE1F}' // Vertical forms
        | '\u{FE30}'..='\u{FE6F}' // CJK Compatibility forms
        | '\u{FF00}'..='\u{FF60}' // Fullwidth
        | '\u{FFE0}'..='\u{FFE6}' // Fullwidth signs
        | '\u{1B000}'..='\u{1B0FF}' // Kana Supplement
        | '\u{1F004}'              // Mahjong tile
        | '\u{1F0CF}'              // Playing card
        | '\u{1F200}'..='\u{1F2FF}' // Enclosed CJK
        | '\u{20000}'..='\u{2A6DF}' // CJK Unified Ext B
        | '\u{2A700}'..='\u{2CEAF}' // CJK Unified Ext C/D/E
        | '\u{2CEB0}'..='\u{2EBEF}' // CJK Unified Ext F
        | '\u{30000}'..='\u{3134F}' // CJK Unified Ext G
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;

    // ---- ListChars::parse tests ----

    #[test]
    fn listchars_parse_basic() {
        let lc = ListChars::parse("tab:>-,eol:$").unwrap();
        assert_eq!(lc.tab_lead, '>');
        assert_eq!(lc.tab_fill, Some('-'));
        assert_eq!(lc.eol, Some('$'));
        assert_eq!(lc.space, None);
        assert_eq!(lc.trail, None);
    }

    #[test]
    fn listchars_parse_all_keys() {
        let lc =
            ListChars::parse("tab:>-,space:·,trail:~,eol:¶,nbsp:_,extends:>,precedes:<").unwrap();
        assert_eq!(lc.tab_lead, '>');
        assert_eq!(lc.tab_fill, Some('-'));
        assert_eq!(lc.space, Some('·'));
        assert_eq!(lc.trail, Some('~'));
        assert_eq!(lc.eol, Some('¶'));
        assert_eq!(lc.nbsp, Some('_'));
        assert_eq!(lc.extends, Some('>'));
        assert_eq!(lc.precedes, Some('<'));
    }

    #[test]
    fn listchars_parse_utf8() {
        let lc = ListChars::parse("tab:→ ,eol:¬").unwrap();
        assert_eq!(lc.tab_lead, '→');
        assert_eq!(lc.tab_fill, Some(' '));
        assert_eq!(lc.eol, Some('¬'));
    }

    #[test]
    fn listchars_parse_invalid_no_colon() {
        assert!(ListChars::parse("tab").is_err());
    }

    #[test]
    fn listchars_parse_invalid_three_char_tab() {
        assert!(ListChars::parse("tab:abc").is_err());
    }

    #[test]
    fn listchars_parse_invalid_unknown_key() {
        assert!(ListChars::parse("bogus:x").is_err());
    }

    #[test]
    fn listchars_parse_invalid_returns_err() {
        // All three error cases from the spec
        assert!(ListChars::parse("tab").is_err(), "no colon");
        assert!(ListChars::parse("tab:abc").is_err(), "3-char tab value");
        assert!(ListChars::parse("bogus:x").is_err(), "unknown key");
    }

    #[test]
    fn listchars_to_string_roundtrip() {
        let s = "tab:>-,space:·,trail:~,eol:¶,nbsp:_,extends:>,precedes:<";
        let lc1 = ListChars::parse(s).unwrap();
        let canonical = lc1.to_canonical_string();
        let lc2 = ListChars::parse(&canonical).unwrap();
        assert_eq!(lc1, lc2);
    }

    #[test]
    fn listchars_default_matches_vim() {
        let lc = ListChars::default();
        assert_eq!(lc.tab_lead, '^');
        assert_eq!(lc.tab_fill, Some('I'));
        assert_eq!(lc.eol, Some('$'));
        assert_eq!(lc.space, None);
        assert_eq!(lc.trail, None);
        assert_eq!(lc.nbsp, None);
    }

    // ---- apply_listchars tests ----

    #[test]
    fn apply_listchars_off_returns_borrowed() {
        let lc = ListChars::default();
        let result = apply_listchars("hello world", &lc, false, 4);
        assert!(
            matches!(result, Cow::Borrowed(_)),
            "expected Borrowed when list=false"
        );
    }

    #[test]
    fn apply_listchars_tab_expansion() {
        // tab:>- at col 0 with tabstop=4 → ">---foo"
        let lc = ListChars::parse("tab:>-,eol:$").unwrap();
        let result = apply_listchars("\tfoo", &lc, true, 4);
        // tab at col 0 → 4 wide: '>' + '-' + '-' + '-', then "foo", then '$'
        assert_eq!(result.as_ref(), ">---foo$");
    }

    #[test]
    fn apply_listchars_trail_substitution() {
        let lc = ListChars::parse("tab:>-,trail:·").unwrap();
        // eol=None so no eol marker; space=None so interior spaces stay as ' '
        let result = apply_listchars("foo   ", &lc, true, 4);
        assert_eq!(result.as_ref(), "foo···");
    }

    #[test]
    fn apply_listchars_eol_appended() {
        let lc = ListChars::parse("tab:>-,eol:¶").unwrap();
        let result = apply_listchars("foo", &lc, true, 4);
        assert_eq!(result.as_ref(), "foo¶");
    }

    #[test]
    fn apply_listchars_nbsp_substitution() {
        let lc = ListChars::parse("tab:>-,nbsp:_").unwrap();
        let result = apply_listchars("a\u{00a0}b", &lc, true, 4);
        assert_eq!(result.as_ref(), "a_b");
    }

    #[test]
    fn apply_listchars_combined() {
        let lc = ListChars::parse("tab:>-,space:·,trail:~,eol:¶,nbsp:_").unwrap();
        // line: tab, space, 'x', nbsp, trailing space
        let input = "\t x\u{00a0} ";
        let result = apply_listchars(input, &lc, true, 4);
        // tab at col 0 with tabstop=4 → ">---"
        // interior space → '·'
        // 'x' → 'x'
        // nbsp → '_'
        // trailing space → '~'
        // eol → '¶'
        assert_eq!(result.as_ref(), ">---·x_~¶");
    }
}
