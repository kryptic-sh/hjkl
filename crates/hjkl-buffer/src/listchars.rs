//! Invisible-character rendering configuration.
//!
//! [`ListChars`] holds the glyph substitutions used when
//! `:set list` is active. Mirrors vim's `listchars` option.

use unicode_width::UnicodeWidthChar;

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

    // Guard against a zero tabstop from the host — `col % 0` panics.
    let tabstop = tabstop.max(1);

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
                // Same per-char width formula the renderer advances by
                // (`hjkl-buffer-tui::render::paint_row` / `display_width`) and
                // that `wrap.rs` sums. It must be the real `unicode-width`
                // table, not a hand-rolled CJK range list: an approximation
                // silently mis-counts emoji (width 2) and combining marks
                // (width 0), so `col` — and therefore tab-stop expansion —
                // drifts from where the renderer actually paints. `None`
                // (control chars) maps to 1 because the renderer substitutes
                // a width-1 Control Pictures glyph for them.
                col += other.width().unwrap_or(1);
            }
        }
    }

    // Append eol marker
    if let Some(eol) = lc.eol {
        out.push(eol);
    }

    std::borrow::Cow::Owned(out)
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

    /// Regression: `tabstop == 0` used to hit `col % 0` and panic when the
    /// line contained a tab. Zero is normalised to 1.
    #[test]
    fn apply_listchars_tabstop_zero_does_not_panic() {
        let lc = ListChars::parse("tab:>-").unwrap();
        let result = apply_listchars("\tx", &lc, true, 0);
        assert_eq!(result.as_ref(), ">x");
    }

    /// Regression: column accounting used a hand-rolled CJK range list that
    /// missed emoji, counting U+1F600 as 1 cell. `unicode-width` (what the
    /// renderer paints with) says 2, so the following tab must start at
    /// column 2 and expand to 2 cells, not 3.
    #[test]
    fn apply_listchars_emoji_counts_as_width_two() {
        let lc = ListChars::parse("tab:>-").unwrap();
        let result = apply_listchars("\u{1F600}\tx", &lc, true, 4);
        assert_eq!(result.as_ref(), "\u{1F600}>-x");
    }

    /// Regression: combining marks are zero-width in `unicode-width` but the
    /// old approximation counted them as 1, pushing the tab stop one cell off.
    #[test]
    fn apply_listchars_combining_mark_counts_as_width_zero() {
        let lc = ListChars::parse("tab:>-").unwrap();
        // 'a' (1) + U+0301 combining acute (0) → tab starts at column 1.
        let result = apply_listchars("a\u{0301}\tx", &lc, true, 4);
        assert_eq!(result.as_ref(), "a\u{0301}>--x");
    }

    /// A CJK char the old hand-rolled `is_wide` already covered stays width 2
    /// under the real table — the switch is not a regression for CJK.
    #[test]
    fn apply_listchars_cjk_still_counts_as_width_two() {
        let lc = ListChars::parse("tab:>-").unwrap();
        let result = apply_listchars("\u{4e2d}\tx", &lc, true, 4);
        assert_eq!(result.as_ref(), "\u{4e2d}>-x");
    }

    /// Plain ASCII is width 1 per char — the overwhelmingly common path is
    /// untouched by the width-table switch.
    #[test]
    fn apply_listchars_ascii_width_unchanged() {
        let lc = ListChars::parse("tab:>-").unwrap();
        let result = apply_listchars("ab\tx", &lc, true, 4);
        assert_eq!(result.as_ref(), "ab>-x");
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
