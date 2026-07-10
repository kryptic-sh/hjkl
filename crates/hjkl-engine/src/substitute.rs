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
//! - The `c` (confirm) flag triggers interactive replacement. Each match
//!   is presented one-by-one; the user chooses y/n/a/q/l. See
//!   [`collect_substitute_matches`] and [`apply_collected_matches`].
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
    /// `c` — confirm mode. When set, [`apply_substitute`] skips all matches
    /// and the caller must use [`collect_substitute_matches`] +
    /// [`apply_collected_matches`] for interactive replacement.
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
    // Per-substitute `/I` (case-sensitive) and `/i` (case-insensitive) flags
    // short-circuit all other resolution — they win over `\c`/`\C` in the
    // pattern (matching vim's documented precedence: flag > inline override).
    let effective_pattern = if cmd.flags.case_sensitive {
        // /I flag: force case-sensitive — run vim_to_rust_regex to strip \c/\C
        // but do NOT add (?i).
        use crate::search::{CaseMode, resolve_case_mode};
        let (stripped, _) = resolve_case_mode(&pattern_str, CaseMode::Sensitive);
        stripped
    } else if cmd.flags.ignore_case {
        // /i flag: force case-insensitive — strip \c/\C and prepend (?i).
        use crate::search::{CaseMode, resolve_case_mode};
        let (stripped, _) = resolve_case_mode(&pattern_str, CaseMode::Sensitive);
        format!("(?i){stripped}")
    } else {
        // No explicit flag: honour ignorecase + smartcase + inline \c/\C.
        use crate::search::{CaseMode, resolve_case_mode};
        let base = CaseMode::from_options(ed.settings().ignore_case, ed.settings().smartcase);
        let (stripped, mode) = resolve_case_mode(&pattern_str, base);
        if mode == CaseMode::Insensitive {
            format!("(?i){stripped}")
        } else {
            stripped
        }
    };

    let regex = Regex::new(&effective_pattern).map_err(|e| format!("bad pattern: {e}"))?;

    ed.push_undo();

    let start = *line_range.start() as usize;
    let end = *line_range.end() as usize;
    let rope = crate::types::Query::rope(ed.buffer());
    let total = rope.len_lines();

    let clamp_end = end.min(total.saturating_sub(1));
    let mut new_lines: Vec<String> = crate::vim::rope_to_lines_vec(&rope);
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

/// A single candidate match discovered by [`collect_substitute_matches`].
///
/// Positions are 0-based byte offsets within their line. The `replacement`
/// field already has all capture-group references expanded (e.g. `$1`) to
/// their literal values so the caller can display it and apply without
/// running the regex again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubstituteMatch {
    /// 0-based row index in the buffer.
    pub row: u32,
    /// Byte offset of the first byte of the match within that row's text.
    pub byte_start: u32,
    /// Byte offset one past the last byte of the match (exclusive).
    pub byte_end: u32,
    /// The literal replacement string (captures expanded).
    pub replacement: String,
}

/// Collect all candidate matches for a `:s/pat/rep/[gc]` command without
/// mutating the buffer.
///
/// Uses the same pattern-resolution and case-sensitivity logic as
/// [`apply_substitute`]. The returned vec is in document order (low row +
/// low byte first). Each entry's `replacement` has capture groups already
/// expanded so the caller can display it without re-running the regex.
///
/// # Errors
///
/// Returns an error when pattern resolution fails or the regex is invalid.
pub fn collect_substitute_matches<H: crate::types::Host>(
    ed: &crate::Editor<hjkl_buffer::Buffer, H>,
    cmd: &SubstituteCmd,
    line_range: std::ops::RangeInclusive<u32>,
) -> Result<Vec<SubstituteMatch>, SubstError> {
    // Resolve pattern — same logic as apply_substitute.
    let pattern_str: String = match &cmd.pattern {
        Some(p) => p.clone(),
        None => ed
            .last_search()
            .map(str::to_owned)
            .ok_or_else(|| "no previous regular expression".to_string())?,
    };

    let effective_pattern = if cmd.flags.case_sensitive {
        use crate::search::{CaseMode, resolve_case_mode};
        let (stripped, _) = resolve_case_mode(&pattern_str, CaseMode::Sensitive);
        stripped
    } else if cmd.flags.ignore_case {
        use crate::search::{CaseMode, resolve_case_mode};
        let (stripped, _) = resolve_case_mode(&pattern_str, CaseMode::Sensitive);
        format!("(?i){stripped}")
    } else {
        use crate::search::{CaseMode, resolve_case_mode};
        let base = CaseMode::from_options(ed.settings().ignore_case, ed.settings().smartcase);
        let (stripped, mode) = resolve_case_mode(&pattern_str, base);
        if mode == CaseMode::Insensitive {
            format!("(?i){stripped}")
        } else {
            stripped
        }
    };

    let regex = Regex::new(&effective_pattern).map_err(|e| format!("bad pattern: {e}"))?;

    let start = *line_range.start() as usize;
    let end = *line_range.end() as usize;
    let rope = crate::types::Query::rope(ed.buffer());
    let total = rope.len_lines();
    let clamp_end = end.min(total.saturating_sub(1));

    let mut matches: Vec<SubstituteMatch> = Vec::new();

    if start <= clamp_end {
        for row in start..=clamp_end {
            let line = hjkl_buffer::rope_line_str(&rope, row);
            // Strip trailing newline so byte offsets refer to printable content.
            let line = line.trim_end_matches('\n');

            if cmd.flags.all {
                for m in regex.find_iter(line) {
                    // Expand capture groups into the literal replacement text.
                    let replacement = regex
                        .captures(m.as_str())
                        .map(|caps| {
                            let mut rep = String::new();
                            caps.expand(&cmd.replacement, &mut rep);
                            rep
                        })
                        .unwrap_or_else(|| cmd.replacement.clone());

                    matches.push(SubstituteMatch {
                        row: row as u32,
                        byte_start: m.start() as u32,
                        byte_end: m.end() as u32,
                        replacement,
                    });
                }
            } else {
                // First match per line only.
                if let Some(m) = regex.find(line) {
                    let replacement = regex
                        .captures(m.as_str())
                        .map(|caps| {
                            let mut rep = String::new();
                            caps.expand(&cmd.replacement, &mut rep);
                            rep
                        })
                        .unwrap_or_else(|| cmd.replacement.clone());

                    matches.push(SubstituteMatch {
                        row: row as u32,
                        byte_start: m.start() as u32,
                        byte_end: m.end() as u32,
                        replacement,
                    });
                }
            }
        }
    }

    Ok(matches)
}

/// Apply a subset of matches collected by [`collect_substitute_matches`].
///
/// Applies the matches in REVERSE document order (high row → low row, and
/// within a row high byte → low byte) so earlier byte offsets remain valid
/// after each replacement. Only matches for which the corresponding
/// `accepted` entry is `true` are written; all others are skipped.
///
/// Returns the number of replacements actually applied.
///
/// # Panics
///
/// Panics when `accepted.len() != matches.len()`.
pub fn apply_collected_matches<H: crate::types::Host>(
    ed: &mut crate::Editor<hjkl_buffer::Buffer, H>,
    matches: &[SubstituteMatch],
    accepted: &[bool],
) -> usize {
    assert_eq!(
        matches.len(),
        accepted.len(),
        "apply_collected_matches: accepted.len() must equal matches.len()"
    );

    // Collect accepted matches and sort reverse — high row first, high
    // byte_start first within the same row.
    let mut to_apply: Vec<&SubstituteMatch> = matches
        .iter()
        .zip(accepted.iter())
        .filter_map(|(m, &ok)| if ok { Some(m) } else { None })
        .collect();

    if to_apply.is_empty() {
        return 0;
    }

    to_apply.sort_unstable_by(|a, b| b.row.cmp(&a.row).then(b.byte_start.cmp(&a.byte_start)));

    let rope = crate::types::Query::rope(ed.buffer());
    let mut lines_vec: Vec<String> = crate::vim::rope_to_lines_vec(&rope);
    let mut applied = 0usize;
    let mut last_changed_row: Option<usize> = None;

    for sm in &to_apply {
        let row = sm.row as usize;
        if row >= lines_vec.len() {
            continue;
        }
        let line = &lines_vec[row];
        let bs = sm.byte_start as usize;
        let be = sm.byte_end as usize;
        if be > line.len() || bs > be {
            continue;
        }
        // Stale matches (buffer changed between collect and apply) can land
        // mid-char on multibyte text; skip instead of panicking on the slice.
        if !line.is_char_boundary(bs) || !line.is_char_boundary(be) {
            continue;
        }
        // Splice the replacement in.
        let mut new_line = String::with_capacity(line.len() + sm.replacement.len());
        new_line.push_str(&line[..bs]);
        new_line.push_str(&sm.replacement);
        new_line.push_str(&line[be..]);
        lines_vec[row] = new_line;
        applied += 1;
        last_changed_row = Some(row);
    }

    if applied > 0 {
        ed.buffer_mut().replace_all(&lines_vec.join("\n"));
        if let Some(row) = last_changed_row {
            ed.buffer_mut()
                .set_cursor(hjkl_buffer::Position::new(row, 0));
        }
        ed.mark_content_dirty();
    }

    applied
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

    fn buf_line(e: &Editor<Buffer, DefaultHost>, row: usize) -> String {
        hjkl_buffer::rope_line_str(&e.buffer().rope(), row)
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
        assert_eq!(buf_line(&e, 0), "bar foo");
    }

    #[test]
    fn apply_single_line_global() {
        let mut e = editor_with("foo foo foo");
        let cmd = parse_substitute("/foo/bar/g").unwrap();
        let out = apply_substitute(&mut e, &cmd, 0..=0).unwrap();
        assert_eq!(out.replacements, 3);
        assert_eq!(out.lines_changed, 1);
        assert_eq!(buf_line(&e, 0), "bar bar bar");
    }

    #[test]
    fn apply_multi_line_range() {
        let mut e = editor_with("foo\nfoo foo\nbar");
        let cmd = parse_substitute("/foo/xyz/g").unwrap();
        let out = apply_substitute(&mut e, &cmd, 0..=2).unwrap();
        assert_eq!(out.replacements, 3);
        assert_eq!(out.lines_changed, 2);
        assert_eq!(buf_line(&e, 0), "xyz");
        assert_eq!(buf_line(&e, 1), "xyz xyz");
        assert_eq!(buf_line(&e, 2), "bar");
    }

    #[test]
    fn apply_no_match_returns_zero() {
        let mut e = editor_with("hello");
        let original = buf_line(&e, 0);
        let cmd = parse_substitute("/xyz/abc/").unwrap();
        let out = apply_substitute(&mut e, &cmd, 0..=0).unwrap();
        assert_eq!(out.replacements, 0);
        assert_eq!(out.lines_changed, 0);
        assert_eq!(buf_line(&e, 0), original);
    }

    #[test]
    fn apply_case_insensitive_flag() {
        let mut e = editor_with("Foo FOO foo");
        let cmd = parse_substitute("/foo/bar/gi").unwrap();
        let out = apply_substitute(&mut e, &cmd, 0..=0).unwrap();
        assert_eq!(out.replacements, 3);
        assert_eq!(buf_line(&e, 0), "bar bar bar");
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
        assert_eq!(buf_line(&e, 0), "Foo bar");
    }

    #[test]
    fn apply_empty_pattern_reuses_last_search() {
        let mut e = editor_with("hello world");
        e.set_last_search(Some("world".to_string()), true);
        let cmd = parse_substitute("//planet/").unwrap();
        let out = apply_substitute(&mut e, &cmd, 0..=0).unwrap();
        assert_eq!(out.replacements, 1);
        assert_eq!(buf_line(&e, 0), "hello planet");
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
        assert_eq!(buf_line(&e, 0), "hello ");
    }

    #[test]
    fn apply_undo_reverts_in_one_step() {
        let mut e = editor_with("foo");
        let cmd = parse_substitute("/foo/bar/").unwrap();
        apply_substitute(&mut e, &cmd, 0..=0).unwrap();
        assert_eq!(buf_line(&e, 0), "bar");
        e.undo();
        assert_eq!(buf_line(&e, 0), "foo");
    }

    #[test]
    fn apply_ampersand_in_replacement() {
        let mut e = editor_with("foo");
        let cmd = parse_substitute("/foo/[&]/").unwrap();
        apply_substitute(&mut e, &cmd, 0..=0).unwrap();
        assert_eq!(buf_line(&e, 0), "[foo]");
    }

    #[test]
    fn apply_capture_group_reference() {
        let mut e = editor_with("hello world");
        let cmd = parse_substitute("/(\\w+)/<<\\1>>/g").unwrap();
        apply_substitute(&mut e, &cmd, 0..=0).unwrap();
        assert_eq!(buf_line(&e, 0), "<<hello>> <<world>>");
    }

    // ── smartcase + \c/\C tests ───────────────────────────────────────────────

    /// `:s/foo/bar/` on `"Foo"` — ignorecase+smartcase on by default, all-
    /// lowercase pattern → Insensitive → matches `Foo` → becomes `bar`.
    #[test]
    fn substitute_respects_smartcase() {
        let mut e = editor_with("Foo");
        // Default Options has ignorecase=true, smartcase=true.
        let cmd = parse_substitute("/foo/bar/").unwrap();
        let out = apply_substitute(&mut e, &cmd, 0..=0).unwrap();
        assert_eq!(out.replacements, 1);
        assert_eq!(buf_line(&e, 0), "bar");
    }

    /// `:s/Foo/bar/i` — `/i` flag overrides smartcase (mixed pattern would
    /// normally be Sensitive) → case-insensitive → matches `"foo"`.
    #[test]
    fn substitute_i_flag_overrides_c() {
        let mut e = editor_with("foo");
        // /i forces insensitive regardless of pattern case or smartcase.
        let cmd = parse_substitute("/Foo/bar/i").unwrap();
        let out = apply_substitute(&mut e, &cmd, 0..=0).unwrap();
        assert_eq!(out.replacements, 1, "expected match on 'foo' with /i flag");
        assert_eq!(buf_line(&e, 0), "bar");
    }

    /// `\c` inline override in a pattern with no `/i`/`/I` flag — forces
    /// insensitive even though `Foo` has uppercase (smartcase trip).
    #[test]
    fn substitute_lower_c_inline_overrides_smartcase() {
        let mut e = editor_with("FOO");
        // \cFoo — override wins, Insensitive → matches "FOO"
        let cmd = parse_substitute("/\\cFoo/bar/").unwrap();
        let out = apply_substitute(&mut e, &cmd, 0..=0).unwrap();
        assert_eq!(out.replacements, 1);
        assert_eq!(buf_line(&e, 0), "bar");
    }

    // ── collect_substitute_matches tests ────────────────────────────────────

    #[test]
    fn collect_substitute_matches_finds_all_occurrences() {
        let e = editor_with("foo bar foo");
        let cmd = parse_substitute("/foo/baz/g").unwrap();
        let matches = collect_substitute_matches(&e, &cmd, 0..=0).unwrap();
        assert_eq!(matches.len(), 2, "expected 2 matches for /g flag");
        assert_eq!(matches[0].byte_start, 0);
        assert_eq!(matches[0].byte_end, 3);
        assert_eq!(matches[1].byte_start, 8);
        assert_eq!(matches[1].byte_end, 11);
        assert_eq!(matches[0].replacement, "baz");
        assert_eq!(matches[1].replacement, "baz");
    }

    #[test]
    fn collect_substitute_matches_respects_g_flag() {
        // Without /g only the first match per line.
        let e = editor_with("foo foo foo");
        let cmd = parse_substitute("/foo/baz/").unwrap();
        let matches = collect_substitute_matches(&e, &cmd, 0..=0).unwrap();
        assert_eq!(matches.len(), 1, "expected 1 match without /g");
        assert_eq!(matches[0].byte_start, 0);
    }

    #[test]
    fn collect_substitute_matches_respects_range() {
        let e = editor_with("foo\nfoo\nfoo\nfoo\nfoo");
        let cmd = parse_substitute("/foo/bar/g").unwrap();
        // Only rows 1 and 2 (0-based) — should return 2 matches, not 5.
        let matches = collect_substitute_matches(&e, &cmd, 1..=2).unwrap();
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].row, 1);
        assert_eq!(matches[1].row, 2);
    }

    #[test]
    fn collect_substitute_matches_expands_template() {
        let e = editor_with("hello world");
        // /(\\w+)/<<\\1>>/ — the replacement template has a capture group.
        let cmd = parse_substitute("/(\\w+)/<<\\1>>/g").unwrap();
        let matches = collect_substitute_matches(&e, &cmd, 0..=0).unwrap();
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].replacement, "<<hello>>");
        assert_eq!(matches[1].replacement, "<<world>>");
    }

    // ── apply_collected_matches tests ───────────────────────────────────────

    #[test]
    fn apply_collected_matches_reverse_order_preserves_offsets() {
        // Three matches at byte offsets 0..3, 4..7, 8..11.
        // Applying in forward order would shift byte offsets; reverse must
        // keep the final buffer consistent.
        let mut e = editor_with("foo bar baz");
        let cmd = parse_substitute("/(foo|bar|baz)/X/g").unwrap();
        let matches = collect_substitute_matches(&e, &cmd, 0..=0).unwrap();
        assert_eq!(matches.len(), 3);
        let accepted = vec![true; 3];
        let applied = apply_collected_matches(&mut e, &matches, &accepted);
        assert_eq!(applied, 3);
        assert_eq!(buf_line(&e, 0), "X X X");
    }

    #[test]
    fn apply_collected_matches_subset_only() {
        // 3 matches; accept only first and third.
        let mut e = editor_with("foo bar foo");
        let cmd = parse_substitute("/foo/ZZZ/g").unwrap();
        let matches = collect_substitute_matches(&e, &cmd, 0..=0).unwrap();
        assert_eq!(matches.len(), 2, "expected 2 foo matches");
        // Accept only the first (index 0), skip the second (index 1).
        let accepted = vec![true, false];
        let applied = apply_collected_matches(&mut e, &matches, &accepted);
        assert_eq!(applied, 1);
        // First "foo" replaced; second "foo" untouched.
        assert_eq!(buf_line(&e, 0), "ZZZ bar foo");
    }

    #[test]
    fn apply_collected_matches_zero_accepted() {
        let mut e = editor_with("foo bar foo");
        let cmd = parse_substitute("/foo/ZZZ/g").unwrap();
        let matches = collect_substitute_matches(&e, &cmd, 0..=0).unwrap();
        let accepted = vec![false; matches.len()];
        let applied = apply_collected_matches(&mut e, &matches, &accepted);
        assert_eq!(applied, 0);
        assert_eq!(buf_line(&e, 0), "foo bar foo");
    }

    #[test]
    fn apply_collected_matches_expands_template() {
        let mut e = editor_with("hello world");
        let cmd = parse_substitute("/(\\w+)/<<\\1>>/g").unwrap();
        let matches = collect_substitute_matches(&e, &cmd, 0..=0).unwrap();
        let accepted = vec![true; matches.len()];
        let applied = apply_collected_matches(&mut e, &matches, &accepted);
        assert_eq!(applied, 2);
        assert_eq!(buf_line(&e, 0), "<<hello>> <<world>>");
    }
}
