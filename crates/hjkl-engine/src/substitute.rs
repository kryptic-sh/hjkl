//! Public substitute command parser and applicator.
//!
//! Exposes [`parse_substitute`] and [`apply_substitute`] for the
//! `:[range]s/pattern/replacement/[flags]` ex command.
//!
//! ## Vim compatibility notes (v1 limitations)
//!
//! - Delimiter is **always `/`**. Alternate delimiters (`s|x|y|`,
//!   `s#x#y#`) are not supported. The parser returns an error when the
//!   first character after the keyword is not `/`.
//! - The `c` (confirm) flag is **parsed but silently ignored**. No
//!   interactive replacement. See vim's `:help :s_c` for what a full
//!   implementation looks like.
//! - The `\v` very-magic mode is not supported. The regex crate uses
//!   ERE syntax by default. Most ERE patterns work, but vim-specific
//!   extensions (`\<`, `\>`, `\s`, `\+`) may not. Use POSIX ERE
//!   equivalents or the `regex` crate's syntax.
//! - Capture-group references use vim notation (`\1`…`\9`, `&`); the
//!   parser translates them to `$1`…`$9`, `$0` for the `regex` crate.
//!
//! See vim's `:help :substitute` for the full spec.

use regex::Regex;

use crate::Editor;

/// Error type returned by [`parse_substitute`] and [`apply_substitute`].
pub type SubstError = String;

/// Parsed `:s/pattern/replacement/flags` command.
///
/// Produced by [`parse_substitute`]. Pass to [`apply_substitute`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubstituteCmd {
    /// The literal pattern string. `None` means "reuse `last_search`
    /// from the editor" (the user typed `:s//replacement/`).
    pub pattern: Option<String>,
    /// The replacement string in vim notation (`&`, `\1`…`\9`).
    /// Empty string deletes the match.
    pub replacement: String,
    /// Parsed flags.
    pub flags: SubstFlags,
}

/// Flags for the substitute command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SubstFlags {
    /// `g` — replace all occurrences on each line (default: first only).
    pub all: bool,
    /// `i` — case-insensitive (overrides editor `ignorecase`).
    pub ignore_case: bool,
    /// `I` — case-sensitive (overrides editor `ignorecase`).
    pub case_sensitive: bool,
    /// `c` — confirm mode. **Parsed but ignored in v1.** Behaves as if
    /// not set; all matches are replaced without prompting. This is a
    /// known divergence from vim. See vim's `:help :s_c`.
    pub confirm: bool,
}

/// Result of [`apply_substitute`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SubstituteOutcome {
    /// Total number of individual replacements made across all lines.
    pub replacements: usize,
    /// Number of lines that had at least one replacement.
    pub lines_changed: usize,
}

/// Parse the tail of a substitute command (everything after the leading
/// `s` / `substitute` keyword).
///
/// # Examples
///
/// ```
/// use hjkl_engine::substitute::parse_substitute;
///
/// let cmd = parse_substitute("/foo/bar/gi").unwrap();
/// assert_eq!(cmd.pattern.as_deref(), Some("foo"));
/// assert_eq!(cmd.replacement, "bar");
/// assert!(cmd.flags.all);
/// assert!(cmd.flags.ignore_case);
///
/// // Empty pattern — reuse last_search.
/// let cmd = parse_substitute("//bar/").unwrap();
/// assert!(cmd.pattern.is_none());
/// assert_eq!(cmd.replacement, "bar");
/// ```
///
/// # Errors
///
/// Returns an error when:
/// - `s` is not followed by `/` (no delimiter or alternate delimiter).
/// - The flag string contains an unknown character.
/// - The separator `/` is absent (less than two fields).
pub fn parse_substitute(s: &str) -> Result<SubstituteCmd, SubstError> {
    // Require leading `/`. Alternate delimiters are out of scope for v1.
    let rest = s
        .strip_prefix('/')
        .ok_or_else(|| format!("substitute: expected '/' delimiter, got {s:?}"))?;

    // Split on unescaped `/`, collecting at most 3 segments:
    // [pattern, replacement, flags?]
    let parts = split_on_slash(rest);

    if parts.len() < 2 {
        return Err("substitute needs /pattern/replacement/".into());
    }

    let raw_pattern = &parts[0];
    let raw_replacement = &parts[1];
    let raw_flags = parts.get(2).map(String::as_str).unwrap_or("");

    // Empty pattern → reuse last_search.
    let pattern = if raw_pattern.is_empty() {
        None
    } else {
        Some(raw_pattern.clone())
    };

    // Translate vim replacement notation to regex crate notation.
    let replacement = translate_replacement(raw_replacement);

    let mut flags = SubstFlags::default();
    for ch in raw_flags.chars() {
        match ch {
            'g' => flags.all = true,
            'i' => flags.ignore_case = true,
            'I' => flags.case_sensitive = true,
            'c' => flags.confirm = true, // parsed, silently ignored
            other => return Err(format!("unknown flag '{other}' in substitute")),
        }
    }

    Ok(SubstituteCmd {
        pattern,
        replacement,
        flags,
    })
}

/// Apply a parsed substitute command to `line_range` (0-based inclusive)
/// in the editor's buffer.
///
/// # Pattern resolution
///
/// If `cmd.pattern` is `None` (user typed `:s//rep/`), the editor's
/// `last_search()` is used. Returns an error with `"no previous regular
/// expression"` when both are empty.
///
/// # Case-sensitivity precedence
///
/// `flags.case_sensitive` wins over `flags.ignore_case`, which wins over
/// the editor's `settings().ignore_case`.
///
/// # Cursor
///
/// After a successful substitution the cursor is placed at column 0 of the
/// **last line that changed**, matching vim semantics. When no replacements
/// are made the cursor is left unchanged.
///
/// # Undo
///
/// One undo snapshot is pushed before the first edit. If no replacements
/// occur the snapshot is popped so the undo stack stays clean.
///
/// # Errors
///
/// Returns an error when pattern resolution fails or the regex is invalid.
pub fn apply_substitute<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    cmd: &SubstituteCmd,
    line_range: std::ops::RangeInclusive<u32>,
) -> Result<SubstituteOutcome, SubstError> {
    // Resolve pattern.
    let pattern_str: String = match &cmd.pattern {
        Some(p) => p.clone(),
        None => ed
            .last_search()
            .map(str::to_owned)
            .ok_or_else(|| "no previous regular expression".to_string())?,
    };

    // Case-sensitivity.
    let case_insensitive = if cmd.flags.case_sensitive {
        false
    } else if cmd.flags.ignore_case {
        true
    } else {
        ed.settings().ignore_case
    };

    let effective_pattern = if case_insensitive {
        format!("(?i){pattern_str}")
    } else {
        pattern_str.clone()
    };

    let regex = Regex::new(&effective_pattern).map_err(|e| format!("bad pattern: {e}"))?;

    ed.push_undo();

    let start = *line_range.start() as usize;
    let end = *line_range.end() as usize;
    let total = ed.buffer().lines().len();

    let clamp_end = end.min(total.saturating_sub(1));
    let mut new_lines: Vec<String> = ed.buffer().lines().to_vec();
    let mut replacements = 0usize;
    let mut lines_changed = 0usize;
    let mut last_changed_row = 0usize;

    if start <= clamp_end {
        for (row, line) in new_lines[start..=clamp_end].iter_mut().enumerate() {
            let (replaced, n) = do_replace(&regex, line, &cmd.replacement, cmd.flags.all);
            if n > 0 {
                *line = replaced;
                replacements += n;
                lines_changed += 1;
                last_changed_row = start + row;
            }
        }
    }

    if replacements == 0 {
        ed.pop_last_undo();
        return Ok(SubstituteOutcome {
            replacements: 0,
            lines_changed: 0,
        });
    }

    // Apply the new content in one shot.
    ed.buffer_mut().replace_all(&new_lines.join("\n"));

    // Cursor lands on the start of the last changed line.
    ed.buffer_mut()
        .set_cursor(hjkl_buffer::Position::new(last_changed_row, 0));

    ed.mark_content_dirty();

    // Update last_search so n/N can repeat the same pattern.
    ed.set_last_search(Some(pattern_str), true);

    Ok(SubstituteOutcome {
        replacements,
        lines_changed,
    })
}

/// Split `s` on unescaped `/`. Each `\/` in `s` becomes a literal `/`
/// in the output segment. Other `\x` sequences pass through unchanged
/// (so regex escape syntax survives).
///
/// Returns at most 3 segments: `[pattern, replacement, flags]`. Anything
/// after the third `/` is absorbed into the flags segment.
fn split_on_slash(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some(&'/') => {
                    // Escaped delimiter → literal slash in this segment.
                    cur.push('/');
                    chars.next();
                }
                Some(_) => {
                    // Any other escape: preserve both chars so regex
                    // syntax (\d, \s, \1, \n …) survives.
                    let next = chars.next().unwrap();
                    cur.push('\\');
                    cur.push(next);
                }
                None => cur.push('\\'),
            }
        } else if c == '/' {
            if out.len() < 2 {
                out.push(std::mem::take(&mut cur));
            } else {
                // Third delimiter found: treat rest as flags.
                // Everything up to this point was the replacement;
                // collect the flags into `cur` and break.
                cur.push(c);
                // Keep going to collect remaining chars as flags.
                // (Actually we already consumed the `/`, so just let
                // the outer loop continue accumulating into cur.)
            }
        } else {
            cur.push(c);
        }
    }
    out.push(cur);
    out
}

/// Translate vim-style replacement tokens to regex-crate syntax.
///
/// - `&` → `$0` (whole match)
/// - `\&` → literal `&`
/// - `\1`…`\9` → `$1`…`$9` (capture groups)
/// - `\\` → `\` (literal backslash)
/// - Any other `\x` → `x` (drop the backslash)
fn translate_replacement(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&' {
            out.push_str("$0");
        } else if c == '\\' {
            match chars.next() {
                Some('&') => out.push('&'),   // \& → literal &
                Some('\\') => out.push('\\'), // \\ → literal \
                Some(d @ '1'..='9') => {
                    out.push('$');
                    out.push(d);
                }
                Some(other) => out.push(other), // drop backslash
                None => {}                      // trailing \ ignored
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Replace first or all occurrences of `regex` in `text` using the
/// already-translated `replacement` string. Returns `(new_text, count)`.
fn do_replace(regex: &Regex, text: &str, replacement: &str, all: bool) -> (String, usize) {
    let matches = regex.find_iter(text).count();
    if matches == 0 {
        return (text.to_string(), 0);
    }
    let replaced = if all {
        regex.replace_all(text, replacement).into_owned()
    } else {
        regex.replace(text, replacement).into_owned()
    };
    let count = if all { matches } else { 1 };
    (replaced, count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{DefaultHost, Options};
    use hjkl_buffer::Buffer;

    fn editor_with(content: &str) -> Editor<Buffer, DefaultHost> {
        let mut e = Editor::new(Buffer::new(), DefaultHost::new(), Options::default());
        e.set_content(content);
        e
    }

    // ── Parser tests ─────────────────────────────────────────────────

    #[test]
    fn parse_basic() {
        let cmd = parse_substitute("/foo/bar/").unwrap();
        assert_eq!(cmd.pattern.as_deref(), Some("foo"));
        assert_eq!(cmd.replacement, "bar");
        assert!(!cmd.flags.all);
    }

    #[test]
    fn parse_trailing_slash_optional() {
        let cmd = parse_substitute("/foo/bar").unwrap();
        assert_eq!(cmd.pattern.as_deref(), Some("foo"));
        assert_eq!(cmd.replacement, "bar");
    }

    #[test]
    fn parse_global_flag() {
        let cmd = parse_substitute("/x/y/g").unwrap();
        assert!(cmd.flags.all);
    }

    #[test]
    fn parse_ignore_case_flag() {
        let cmd = parse_substitute("/x/y/i").unwrap();
        assert!(cmd.flags.ignore_case);
    }

    #[test]
    fn parse_case_sensitive_flag() {
        let cmd = parse_substitute("/x/y/I").unwrap();
        assert!(cmd.flags.case_sensitive);
    }

    #[test]
    fn parse_confirm_flag_accepted() {
        let cmd = parse_substitute("/x/y/c").unwrap();
        assert!(cmd.flags.confirm);
    }

    #[test]
    fn parse_multi_flags() {
        let cmd = parse_substitute("/x/y/gi").unwrap();
        assert!(cmd.flags.all);
        assert!(cmd.flags.ignore_case);
    }

    #[test]
    fn parse_unknown_flag_errors() {
        let err = parse_substitute("/x/y/z").unwrap_err();
        assert!(err.to_string().contains("unknown flag 'z'"), "{err}");
    }

    #[test]
    fn parse_empty_pattern_is_none() {
        let cmd = parse_substitute("//bar/").unwrap();
        assert!(cmd.pattern.is_none());
        assert_eq!(cmd.replacement, "bar");
    }

    #[test]
    fn parse_empty_replacement_ok() {
        let cmd = parse_substitute("/foo//").unwrap();
        assert_eq!(cmd.pattern.as_deref(), Some("foo"));
        assert_eq!(cmd.replacement, "");
    }

    #[test]
    fn parse_escaped_slash_in_pattern() {
        let cmd = parse_substitute("/a\\/b/c/").unwrap();
        assert_eq!(cmd.pattern.as_deref(), Some("a/b"));
    }

    #[test]
    fn parse_escaped_slash_in_replacement() {
        let cmd = parse_substitute("/a/b\\/c/").unwrap();
        // Replacement is already translated; literal / survives.
        assert_eq!(cmd.replacement, "b/c");
    }

    #[test]
    fn parse_ampersand_becomes_dollar_zero() {
        let cmd = parse_substitute("/foo/[&]/").unwrap();
        assert_eq!(cmd.replacement, "[$0]");
    }

    #[test]
    fn parse_escaped_ampersand_is_literal() {
        let cmd = parse_substitute("/foo/\\&/").unwrap();
        assert_eq!(cmd.replacement, "&");
    }

    #[test]
    fn parse_group_ref_translates() {
        let cmd = parse_substitute("/(foo)/\\1/").unwrap();
        assert_eq!(cmd.replacement, "$1");
    }

    #[test]
    fn parse_group_ref_nine() {
        let cmd = parse_substitute("/(x)/\\9/").unwrap();
        assert_eq!(cmd.replacement, "$9");
    }

    #[test]
    fn parse_wrong_delimiter_errors() {
        let err = parse_substitute("|foo|bar|").unwrap_err();
        assert!(err.to_string().contains("'/'"), "{err}");
    }

    #[test]
    fn parse_too_few_fields_errors() {
        let err = parse_substitute("/foo").unwrap_err();
        assert!(
            err.to_string().contains("needs /pattern/replacement"),
            "{err}"
        );
    }

    // ── Apply tests ──────────────────────────────────────────────────

    #[test]
    fn apply_single_line_first_only() {
        let mut e = editor_with("foo foo");
        let cmd = parse_substitute("/foo/bar/").unwrap();
        let out = apply_substitute(&mut e, &cmd, 0..=0).unwrap();
        assert_eq!(out.replacements, 1);
        assert_eq!(out.lines_changed, 1);
        assert_eq!(e.buffer().lines()[0], "bar foo");
    }

    #[test]
    fn apply_single_line_global() {
        let mut e = editor_with("foo foo foo");
        let cmd = parse_substitute("/foo/bar/g").unwrap();
        let out = apply_substitute(&mut e, &cmd, 0..=0).unwrap();
        assert_eq!(out.replacements, 3);
        assert_eq!(out.lines_changed, 1);
        assert_eq!(e.buffer().lines()[0], "bar bar bar");
    }

    #[test]
    fn apply_multi_line_range() {
        let mut e = editor_with("foo\nfoo foo\nbar");
        let cmd = parse_substitute("/foo/xyz/g").unwrap();
        let out = apply_substitute(&mut e, &cmd, 0..=2).unwrap();
        assert_eq!(out.replacements, 3);
        assert_eq!(out.lines_changed, 2);
        assert_eq!(e.buffer().lines()[0], "xyz");
        assert_eq!(e.buffer().lines()[1], "xyz xyz");
        assert_eq!(e.buffer().lines()[2], "bar");
    }

    #[test]
    fn apply_no_match_returns_zero() {
        let mut e = editor_with("hello");
        let original = e.buffer().lines()[0].to_string();
        let cmd = parse_substitute("/xyz/abc/").unwrap();
        let out = apply_substitute(&mut e, &cmd, 0..=0).unwrap();
        assert_eq!(out.replacements, 0);
        assert_eq!(out.lines_changed, 0);
        assert_eq!(e.buffer().lines()[0], original);
    }

    #[test]
    fn apply_case_insensitive_flag() {
        let mut e = editor_with("Foo FOO foo");
        let cmd = parse_substitute("/foo/bar/gi").unwrap();
        let out = apply_substitute(&mut e, &cmd, 0..=0).unwrap();
        assert_eq!(out.replacements, 3);
        assert_eq!(e.buffer().lines()[0], "bar bar bar");
    }

    #[test]
    fn apply_case_sensitive_flag_overrides_editor_setting() {
        let mut e = editor_with("Foo foo");
        // Enable ignorecase on the editor.
        e.settings_mut().ignore_case = true;
        // `I` (capital) forces case-sensitive.
        let cmd = parse_substitute("/foo/bar/I").unwrap();
        let out = apply_substitute(&mut e, &cmd, 0..=0).unwrap();
        // Only the lowercase "foo" matches.
        assert_eq!(out.replacements, 1);
        assert_eq!(e.buffer().lines()[0], "Foo bar");
    }

    #[test]
    fn apply_empty_pattern_reuses_last_search() {
        let mut e = editor_with("hello world");
        e.set_last_search(Some("world".to_string()), true);
        let cmd = parse_substitute("//planet/").unwrap();
        let out = apply_substitute(&mut e, &cmd, 0..=0).unwrap();
        assert_eq!(out.replacements, 1);
        assert_eq!(e.buffer().lines()[0], "hello planet");
    }

    #[test]
    fn apply_empty_pattern_no_last_search_errors() {
        let mut e = editor_with("hello");
        let cmd = parse_substitute("//bar/").unwrap();
        let err = apply_substitute(&mut e, &cmd, 0..=0).unwrap_err();
        assert!(
            err.to_string().contains("no previous regular expression"),
            "{err}"
        );
    }

    #[test]
    fn apply_updates_last_search() {
        let mut e = editor_with("foo");
        let cmd = parse_substitute("/foo/bar/").unwrap();
        apply_substitute(&mut e, &cmd, 0..=0).unwrap();
        assert_eq!(e.last_search(), Some("foo"));
    }

    #[test]
    fn apply_empty_replacement_deletes_match() {
        let mut e = editor_with("hello world");
        let cmd = parse_substitute("/world//").unwrap();
        let out = apply_substitute(&mut e, &cmd, 0..=0).unwrap();
        assert_eq!(out.replacements, 1);
        assert_eq!(e.buffer().lines()[0], "hello ");
    }

    #[test]
    fn apply_undo_reverts_in_one_step() {
        let mut e = editor_with("foo");
        let cmd = parse_substitute("/foo/bar/").unwrap();
        apply_substitute(&mut e, &cmd, 0..=0).unwrap();
        assert_eq!(e.buffer().lines()[0], "bar");
        e.undo();
        assert_eq!(e.buffer().lines()[0], "foo");
    }

    #[test]
    fn apply_ampersand_in_replacement() {
        let mut e = editor_with("foo");
        let cmd = parse_substitute("/foo/[&]/").unwrap();
        apply_substitute(&mut e, &cmd, 0..=0).unwrap();
        assert_eq!(e.buffer().lines()[0], "[foo]");
    }

    #[test]
    fn apply_capture_group_reference() {
        let mut e = editor_with("hello world");
        let cmd = parse_substitute("/(\\w+)/<<\\1>>/g").unwrap();
        apply_substitute(&mut e, &cmd, 0..=0).unwrap();
        assert_eq!(e.buffer().lines()[0], "<<hello>> <<world>>");
    }
}
