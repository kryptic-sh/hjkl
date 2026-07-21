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
//! - Patterns are translated from vim's default-magic syntax (and the
//!   `\v` / `\V` / `\m` / `\M` mode switches) into rust-`regex` syntax by
//!   [`crate::search::resolve_case_mode`] before compiling — see that
//!   module for the full transform table. `\1`-`\9` **backreferences in the
//!   pattern** (as opposed to the replacement, which supports them) are not
//!   supported by the rust `regex` crate (no backtracking engine).
//! - The replacement is kept in raw vim notation and expanded per match by
//!   [`expand_replacement`]: capture refs (`&`, `\0`…`\9`), case escapes
//!   (`\u`/`\l`/`\U`/`\L`/`\E`), control chars (`\r`/`\t`/`\n`), and `~` (the
//!   previous replacement). A plain `$` is literal.
//! - Flags: `g` (all), `i`/`I` (case), `c` (confirm), `n` (report count only,
//!   no change), `e` (accepted — hjkl already succeeds on no match),
//!   `p`/`#`/`l` (print the last changed line, optionally with number /
//!   `:list`-style — surfaced by the ex layer), and `&` (reuse the previous
//!   substitute's flags — resolved by the ex layer). A trailing `[count]`
//!   operates on `count` lines from the range's last line.
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
    /// The replacement string in **raw vim notation** (`&`, `~`, `\0`…`\9`,
    /// `\u`/`\U`/`\l`/`\L`/`\E`, `\r`/`\t`/`\n`). Expanded per match by
    /// [`expand_replacement`]. Empty string deletes the match.
    pub replacement: String,
    /// Parsed flags.
    pub flags: SubstFlags,
    /// Optional trailing `[count]` (`:s/a/b/g 3`): operate on `count` lines
    /// starting at the range's last line (vim semantics). `None` = no count.
    pub count: Option<usize>,
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
    /// `n` — report the match count only; do not modify the buffer or move
    /// the cursor. [`apply_substitute`] counts matches and returns without
    /// mutating.
    pub report_only: bool,
    /// `e` — do not treat "pattern not found" as an error. hjkl already
    /// returns success on no match, so this is accepted for compatibility.
    pub no_error: bool,
    /// `p` / `#` / `l` — print the last changed line (optionally with line
    /// number `#` or `:list`-style `l`). Parsed and accepted; the print
    /// itself is surfaced by the ex/host layer.
    pub print: bool,
    /// `#` — print with line number (implies `print`).
    pub print_num: bool,
    /// `l` — print `:list`-style (implies `print`).
    pub print_list: bool,
    /// `&` — reuse the flags from the previous substitute (`:h :s_flags`).
    /// Resolved by the ex handler, which merges the stored `last_substitute`
    /// flags into this command's before applying.
    pub reuse_previous: bool,
}

/// Result of [`apply_substitute`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SubstituteOutcome {
    /// Total number of individual replacements made across all lines.
    pub replacements: usize,
    /// Number of lines that had at least one replacement.
    pub lines_changed: usize,
    /// 0-based row of the last changed line (where the cursor lands). `None`
    /// when nothing changed. Used by the ex layer to print the line for the
    /// `p` / `#` / `l` flags.
    pub last_row: Option<usize>,
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

    // Keep the replacement in raw vim notation; `expand_replacement` resolves
    // capture refs, case escapes, and `~` per match.
    let replacement = raw_replacement.clone();

    let (flags, count) = parse_flags(raw_flags)?;

    Ok(SubstituteCmd {
        pattern,
        replacement,
        flags,
        count,
    })
}

/// Parse a substitute flags+count tail: `[flag-chars][ optional trailing
/// count]`, e.g. `"g"`, `"gi 3"`, `""`. This is the segment after the
/// closing `/` delimiter in `:s/pat/rep/flags`, and — for [B17] bare
/// `:s [flags] [count]` (repeat-last-substitute) — the ENTIRE argument
/// string, since there's no delimiter at all in that form.
///
/// # Errors
///
/// Returns an error on an unrecognized flag character or non-numeric
/// trailing text.
pub fn parse_flags(raw_flags: &str) -> Result<(SubstFlags, Option<usize>), SubstError> {
    let mut flags = SubstFlags::default();
    let mut count: Option<usize> = None;
    let mut chars = raw_flags.chars().peekable();
    while let Some(&ch) = chars.peek() {
        match ch {
            'g' => flags.all = true,
            'i' => flags.ignore_case = true,
            'I' => flags.case_sensitive = true,
            'c' => flags.confirm = true,
            'n' => flags.report_only = true,
            'e' => flags.no_error = true,
            'p' => flags.print = true,
            '#' => {
                flags.print = true;
                flags.print_num = true;
            }
            'l' => {
                flags.print = true;
                flags.print_list = true;
            }
            // `&` — reuse the previous substitute's flags. Resolved by the ex
            // handler (which holds `last_substitute`).
            '&' => flags.reuse_previous = true,
            ' ' | '\t' => {}
            c if c.is_ascii_digit() => break, // trailing count begins
            other => return Err(format!("unknown flag '{other}' in substitute")),
        }
        chars.next();
    }
    // Trailing count: the remainder (after any whitespace) must be a number.
    let rest: String = chars.collect();
    let rest = rest.trim();
    if !rest.is_empty() {
        match rest.parse::<usize>() {
            Ok(n) if n > 0 => count = Some(n),
            _ => return Err(format!("trailing characters in substitute: {rest:?}")),
        }
    }
    Ok((flags, count))
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
/// After a successful substitution the cursor is placed on the first
/// non-blank of the **last line that changed**, matching vim semantics. When
/// no replacements are made the cursor is left unchanged.
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
    ed: &mut Editor<hjkl_buffer::View, H>,
    cmd: &SubstituteCmd,
    line_range: std::ops::RangeInclusive<u32>,
) -> Result<SubstituteOutcome, SubstError> {
    // Resolve pattern.
    let pattern_str: String = match &cmd.pattern {
        Some(p) => p.clone(),
        None => ed
            .last_search()
            .ok_or_else(|| "no previous regular expression".to_string())?,
    };

    // Previous `:s` replacement text (this command is stored only after it
    // succeeds, so this is the *prior* one). Serves double duty: pattern-side
    // magic `~` expands to it, and replacement-side `~` re-expands it per match.
    let prev_replacement = ed.last_substitute_replacement();

    // Case-sensitivity.
    // Per-substitute `/I` (case-sensitive) and `/i` (case-insensitive) flags
    // short-circuit all other resolution — they win over `\c`/`\C` in the
    // pattern (matching vim's documented precedence: flag > inline override).
    let effective_pattern = if cmd.flags.case_sensitive {
        // /I flag: force case-sensitive — run vim_to_rust_regex to strip \c/\C
        // but do NOT add (?i).
        use crate::search::{CaseMode, resolve_case_mode};
        let (stripped, _) = resolve_case_mode(&pattern_str, CaseMode::Sensitive, &prev_replacement);
        stripped
    } else if cmd.flags.ignore_case {
        // /i flag: force case-insensitive — strip \c/\C and prepend (?i).
        use crate::search::{CaseMode, resolve_case_mode};
        let (stripped, _) = resolve_case_mode(&pattern_str, CaseMode::Sensitive, &prev_replacement);
        format!("(?i){stripped}")
    } else {
        // No explicit flag: honour ignorecase + smartcase + inline \c/\C.
        use crate::search::{CaseMode, resolve_case_mode};
        let base = CaseMode::from_options(ed.settings().ignore_case, ed.settings().smartcase);
        let (stripped, mode) = resolve_case_mode(&pattern_str, base, &prev_replacement);
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
    let mut new_lines: Vec<String> = crate::rope_util::rope_to_lines_vec(&rope);
    let mut replacements = 0usize;
    let mut lines_changed = 0usize;
    let mut last_changed_row = 0usize;

    if start <= clamp_end {
        for (row, line) in new_lines[start..=clamp_end].iter_mut().enumerate() {
            let (replaced, n) = do_replace(
                &regex,
                line,
                &cmd.replacement,
                &prev_replacement,
                cmd.flags.all,
            );
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
            last_row: None,
        });
    }

    // `n` flag: report the match count without touching the buffer or cursor.
    // Still refresh `last_search` so `n`/`N` can repeat the pattern.
    if cmd.flags.report_only {
        ed.pop_last_undo();
        ed.set_last_search(Some(pattern_str), true);
        return Ok(SubstituteOutcome {
            replacements,
            lines_changed,
            last_row: None,
        });
    }

    // `last_changed_row` above is a PRE-split row index into `new_lines`: it
    // counts one entry per original row, even though a `\r`/newline in the
    // replacement can turn one entry into several physical rows once joined
    // and re-split by the buffer. Map it into POST-split row space before
    // placing the cursor: earlier rows may have grown (shifting this row's
    // start down), and this row's own replacement may itself have split into
    // multiple physical lines — vim lands on the LAST of those.
    let newlines_before: usize = new_lines[..last_changed_row]
        .iter()
        .map(|l| l.matches('\n').count())
        .sum();
    let newlines_within = new_lines[last_changed_row].matches('\n').count();
    let last_changed_row = last_changed_row + newlines_before + newlines_within;

    // Apply the new content in one shot.
    ed.buffer_mut().replace_all(&new_lines.join("\n"));

    // Cursor lands on the first non-blank of the last changed line (vim). Clamp
    // the row defensively in case of any off-by-one at buffer edges.
    let final_total = crate::types::Query::rope(ed.buffer()).len_lines();
    let cursor_row = last_changed_row.min(final_total.saturating_sub(1));
    let first_non_blank = crate::buf_helpers::buf_line(ed.buffer(), cursor_row)
        .unwrap_or_default()
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .count();
    let line_len = crate::buf_helpers::buf_line(ed.buffer(), cursor_row)
        .unwrap_or_default()
        .chars()
        .count();
    let cursor_col = first_non_blank.min(line_len.saturating_sub(1));
    ed.buffer_mut()
        .set_cursor(hjkl_buffer::Position::new(cursor_row, cursor_col));

    ed.mark_content_dirty();

    // Update last_search so n/N can repeat the same pattern.
    ed.set_last_search(Some(pattern_str), true);

    Ok(SubstituteOutcome {
        replacements,
        lines_changed,
        last_row: Some(cursor_row),
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
    ed: &crate::Editor<hjkl_buffer::View, H>,
    cmd: &SubstituteCmd,
    line_range: std::ops::RangeInclusive<u32>,
) -> Result<Vec<SubstituteMatch>, SubstError> {
    // Resolve pattern — same logic as apply_substitute.
    let pattern_str: String = match &cmd.pattern {
        Some(p) => p.clone(),
        None => ed
            .last_search()
            .ok_or_else(|| "no previous regular expression".to_string())?,
    };

    // Previous `:s` replacement — pattern-side magic `~` expands to it, and
    // replacement-side `~` re-expands it per match (same as apply_substitute).
    let prev_replacement = ed.last_substitute_replacement();

    let effective_pattern = if cmd.flags.case_sensitive {
        use crate::search::{CaseMode, resolve_case_mode};
        let (stripped, _) = resolve_case_mode(&pattern_str, CaseMode::Sensitive, &prev_replacement);
        stripped
    } else if cmd.flags.ignore_case {
        use crate::search::{CaseMode, resolve_case_mode};
        let (stripped, _) = resolve_case_mode(&pattern_str, CaseMode::Sensitive, &prev_replacement);
        format!("(?i){stripped}")
    } else {
        use crate::search::{CaseMode, resolve_case_mode};
        let base = CaseMode::from_options(ed.settings().ignore_case, ed.settings().smartcase);
        let (stripped, mode) = resolve_case_mode(&pattern_str, base, &prev_replacement);
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

    // Expand the raw vim replacement against the match at `m.start()`. Capture
    // against the whole line (not the isolated substring) so anchors /
    // lookaround keep their context and group expansion matches what was found.
    let expand = |line: &str, start: usize| {
        regex
            .captures_at(line, start)
            .map(|caps| expand_replacement(&cmd.replacement, &caps, &prev_replacement))
            .unwrap_or_default()
    };

    if start <= clamp_end {
        for row in start..=clamp_end {
            let line = hjkl_buffer::rope_line_str(&rope, row);
            // Strip trailing newline so byte offsets refer to printable content.
            let line = line.trim_end_matches('\n');

            if cmd.flags.all {
                for m in regex.find_iter(line) {
                    matches.push(SubstituteMatch {
                        row: row as u32,
                        byte_start: m.start() as u32,
                        byte_end: m.end() as u32,
                        replacement: expand(line, m.start()),
                    });
                }
            } else if let Some(m) = regex.find(line) {
                // First match per line only.
                matches.push(SubstituteMatch {
                    row: row as u32,
                    byte_start: m.start() as u32,
                    byte_end: m.end() as u32,
                    replacement: expand(line, m.start()),
                });
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
    ed: &mut crate::Editor<hjkl_buffer::View, H>,
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
    let mut lines_vec: Vec<String> = crate::rope_util::rope_to_lines_vec(&rope);
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
        // Matches are applied high-row-first (reverse document order) so
        // earlier byte offsets stay valid; track the HIGHEST row touched so
        // the cursor lands on the last-in-document-order changed line (vim),
        // not merely the last one processed by this loop.
        last_changed_row = Some(last_changed_row.map_or(row, |lr: usize| lr.max(row)));
    }

    if applied > 0 {
        ed.buffer_mut().replace_all(&lines_vec.join("\n"));
        if let Some(row) = last_changed_row {
            // `row` is a PRE-split index into `lines_vec`: a `\r`/newline in
            // an accepted replacement can turn one entry into several
            // physical rows once joined and re-split by the buffer. Map into
            // POST-split row space the same way `apply_substitute` does.
            let newlines_before: usize = lines_vec[..row]
                .iter()
                .map(|l| l.matches('\n').count())
                .sum();
            let newlines_within = lines_vec[row].matches('\n').count();
            let row = row + newlines_before + newlines_within;
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

/// The persistent (span) case transformation set by `\U` / `\L`, cleared by
/// `\E` / `\e`.
#[derive(Clone, Copy, PartialEq)]
enum SpanCase {
    None,
    /// `\U` — uppercase until `\E`.
    Upper,
    /// `\L` — lowercase until `\E`.
    Lower,
}

/// The one-shot case transformation set by `\u` / `\l`. Takes priority over
/// [`SpanCase`] for exactly the next char, then reverts to whatever span was
/// active — it does NOT clear the span (vim: `\U\l&` on `"hello"` produces
/// `"hELLO"`, not `"hello"` — the lowercase-next-char applies, then the
/// active `\U` span resumes for the rest of the match).
#[derive(Clone, Copy, PartialEq)]
enum OneShotCase {
    Upper,
    Lower,
}

/// Combined case-transformation state threaded through [`expand_into`].
#[derive(Clone, Copy, PartialEq)]
struct CaseState {
    span: SpanCase,
    one_shot: Option<OneShotCase>,
}

impl CaseState {
    fn new() -> Self {
        Self {
            span: SpanCase::None,
            one_shot: None,
        }
    }
}

/// Push `ch` into `out`, applying the active case state. A pending one-shot
/// (`\u`/`\l`) wins for this single char and is then consumed, falling back
/// to the span (`\U`/`\L`) state — which persists — for subsequent chars.
fn push_cased(out: &mut String, case: &mut CaseState, ch: char) {
    let effective = match case.one_shot.take() {
        Some(OneShotCase::Upper) => Some(SpanCase::Upper),
        Some(OneShotCase::Lower) => Some(SpanCase::Lower),
        None => match case.span {
            SpanCase::None => None,
            other => Some(other),
        },
    };
    match effective {
        None => out.push(ch),
        Some(SpanCase::Upper) => out.extend(ch.to_uppercase()),
        Some(SpanCase::Lower) => out.extend(ch.to_lowercase()),
        Some(SpanCase::None) => unreachable!(),
    }
}

/// Expand a raw vim replacement string against a single regex match.
///
/// Handles vim's `:h sub-replace-special` tokens:
/// - `&` / `\0` — whole match; `\1`…`\9` — capture groups; `\&` — literal `&`.
/// - `\r` — line break, `\t` — tab, `\n` — NUL.
/// - `\u`/`\l` — upper/lowercase the next char; `\U`/`\L` … `\E`/`\e` — upper/
///   lowercase a run.
/// - `~` — the previous replacement string (`prev`), re-expanded against this
///   match; `\~` — literal `~`.
/// - `\\` — literal backslash; any other `\x` — literal `x`.
///
/// A plain `$` is literal (unlike the regex crate's `$`-expansion, which this
/// deliberately does not use).
fn expand_replacement(raw: &str, caps: &regex::Captures, prev: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 8);
    expand_into(&mut out, raw, caps, prev, true);
    out
}

fn expand_into(out: &mut String, raw: &str, caps: &regex::Captures, prev: &str, allow_tilde: bool) {
    let mut case = CaseState::new();
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        match c {
            '&' => {
                let g = caps.get(0).map(|m| m.as_str()).unwrap_or("");
                for ch in g.chars() {
                    push_cased(out, &mut case, ch);
                }
            }
            '~' if allow_tilde => {
                // Previous replacement, re-expanded against this match. A `~`
                // nested inside `prev` is treated literally to avoid recursion.
                let mut tmp = String::new();
                expand_into(&mut tmp, prev, caps, "", false);
                for ch in tmp.chars() {
                    push_cased(out, &mut case, ch);
                }
            }
            '\\' => match chars.next() {
                Some('&') => push_cased(out, &mut case, '&'),
                Some('~') => push_cased(out, &mut case, '~'),
                Some('\\') => push_cased(out, &mut case, '\\'),
                // Control chars ignore case state (nothing to case).
                Some('r') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('n') => out.push('\0'),
                Some(d @ '0'..='9') => {
                    let idx = d as usize - '0' as usize;
                    let g = caps.get(idx).map(|m| m.as_str()).unwrap_or("");
                    for ch in g.chars() {
                        push_cased(out, &mut case, ch);
                    }
                }
                Some('u') => case.one_shot = Some(OneShotCase::Upper),
                Some('l') => case.one_shot = Some(OneShotCase::Lower),
                Some('U') => case.span = SpanCase::Upper,
                Some('L') => case.span = SpanCase::Lower,
                Some('e') | Some('E') => case.span = SpanCase::None,
                Some(other) => push_cased(out, &mut case, other),
                None => {} // trailing backslash ignored
            },
            _ => push_cased(out, &mut case, c),
        }
    }
}

/// Replace the first or all occurrences of `regex` in `text`, expanding the
/// raw vim `replacement` (with `prev` for `~`) per match. Returns
/// `(new_text, count)`.
fn do_replace(
    regex: &Regex,
    text: &str,
    replacement: &str,
    prev: &str,
    all: bool,
) -> (String, usize) {
    let matches = regex.find_iter(text).count();
    if matches == 0 {
        return (text.to_string(), 0);
    }
    let rep = |caps: &regex::Captures| expand_replacement(replacement, caps, prev);
    let replaced = if all {
        regex.replace_all(text, rep).into_owned()
    } else {
        regex.replace(text, rep).into_owned()
    };
    let count = if all { matches } else { 1 };
    (replaced, count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{DefaultHost, Options};
    use hjkl_buffer::View;

    fn editor_with(content: &str) -> Editor<View, DefaultHost> {
        let mut e = Editor::new(View::new(), DefaultHost::new(), Options::default());
        e.set_content(content);
        e
    }

    fn buf_line(e: &Editor<View, DefaultHost>, row: usize) -> String {
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

    // The parser stores the replacement in RAW vim notation; expansion (below)
    // resolves `&` / `\1` / `\&` etc. per match.
    #[test]
    fn parse_keeps_replacement_raw() {
        assert_eq!(parse_substitute("/foo/[&]/").unwrap().replacement, "[&]");
        assert_eq!(parse_substitute("/foo/\\&/").unwrap().replacement, "\\&");
        assert_eq!(parse_substitute("/(foo)/\\1/").unwrap().replacement, "\\1");
        assert_eq!(parse_substitute("/(x)/\\9/").unwrap().replacement, "\\9");
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
        assert_eq!(e.last_search(), Some("foo".to_string()));
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
        // Vim default magic: groups need `\(` `\)`; `+` needs `\+`.
        let cmd = parse_substitute("/\\(\\w\\+\\)/<<\\1>>/g").unwrap();
        apply_substitute(&mut e, &cmd, 0..=0).unwrap();
        assert_eq!(buf_line(&e, 0), "<<hello>> <<world>>");
    }

    #[test]
    fn apply_backslash_r_splits_line() {
        // `\r` in the replacement inserts a line break (the split-on-delimiter
        // idiom): `:s/,/\r/g` turns one line into three.
        let mut e = editor_with("a,b,c");
        let cmd = parse_substitute("/,/\\r/g").unwrap();
        apply_substitute(&mut e, &cmd, 0..=0).unwrap();
        assert_eq!(buf_line(&e, 0), "a");
        assert_eq!(buf_line(&e, 1), "b");
        assert_eq!(buf_line(&e, 2), "c");
    }

    /// Audit A5 regression: `:%s/,/\r/` across a multi-row range where an
    /// earlier row's replacement also splits into extra rows. The recorded
    /// "last changed row" must be adjusted into POST-substitution row space
    /// (vim lands on the first non-blank of the last changed line, `d`, real
    /// row 3) — not the PRE-split row index (which would land on `b`, row 1).
    #[test]
    fn apply_backslash_r_multi_row_cursor_lands_on_final_split_row() {
        let mut e = editor_with("a,b\nc,d\n");
        let cmd = parse_substitute("/,/\\r/").unwrap();
        let total = crate::types::Query::rope(e.buffer()).len_lines();
        let out = apply_substitute(&mut e, &cmd, 0..=(total.saturating_sub(1)) as u32).unwrap();
        assert_eq!(buf_line(&e, 0), "a");
        assert_eq!(buf_line(&e, 1), "b");
        assert_eq!(buf_line(&e, 2), "c");
        assert_eq!(buf_line(&e, 3), "d");
        assert_eq!(
            out.last_row,
            Some(3),
            "cursor should land on the last changed line ('d', real row 3) \
             in post-split coordinates, not the pre-split row index"
        );
        assert_eq!(e.buffer().cursor().row, 3);
    }

    /// Single-row case: `\r` splitting one line into several must still put
    /// the cursor on the LAST resulting physical line (vim semantics for a
    /// single `:s` invocation whose replacement itself contains newlines).
    #[test]
    fn apply_backslash_r_single_row_cursor_lands_on_last_split_line() {
        let mut e = editor_with("a,b");
        let cmd = parse_substitute("/,/\\r/").unwrap();
        let out = apply_substitute(&mut e, &cmd, 0..=0).unwrap();
        assert_eq!(buf_line(&e, 0), "a");
        assert_eq!(buf_line(&e, 1), "b");
        assert_eq!(out.last_row, Some(1));
        assert_eq!(e.buffer().cursor().row, 1);
    }

    /// Guard the common (no-newline) multi-row path: cursor still lands on
    /// the last changed row with no coordinate adjustment needed.
    #[test]
    fn apply_no_newline_multi_row_cursor_unaffected() {
        let mut e = editor_with("a\na\na");
        let cmd = parse_substitute("/a/X/").unwrap();
        let out = apply_substitute(&mut e, &cmd, 0..=2).unwrap();
        assert_eq!(buf_line(&e, 0), "X");
        assert_eq!(buf_line(&e, 1), "X");
        assert_eq!(buf_line(&e, 2), "X");
        assert_eq!(out.last_row, Some(2));
        assert_eq!(e.buffer().cursor().row, 2);
    }

    #[test]
    fn apply_backslash_t_inserts_tab() {
        let mut e = editor_with("a,b");
        let cmd = parse_substitute("/,/\\t/").unwrap();
        apply_substitute(&mut e, &cmd, 0..=0).unwrap();
        assert_eq!(buf_line(&e, 0), "a\tb");
    }

    #[test]
    fn apply_literal_dollar_in_replacement() {
        // A literal `$` in the replacement stays literal (vim uses `\1` for
        // groups, so `$5` is not a capture ref).
        let mut e = editor_with("x");
        let cmd = parse_substitute("/x/$5/").unwrap();
        apply_substitute(&mut e, &cmd, 0..=0).unwrap();
        assert_eq!(buf_line(&e, 0), "$5");
    }

    #[test]
    fn apply_backslash_zero_is_whole_match() {
        // `\0` is the whole match (like `&`).
        let mut e = editor_with("foo");
        let cmd = parse_substitute("/foo/[\\0]/").unwrap();
        apply_substitute(&mut e, &cmd, 0..=0).unwrap();
        assert_eq!(buf_line(&e, 0), "[foo]");
    }

    #[test]
    fn apply_group_ref_then_literal_digits() {
        // Braced capture refs let a digit follow a group ref: `\1` then `1`.
        let mut e = editor_with("ab");
        let cmd = parse_substitute("/\\(.\\)/\\11/g").unwrap();
        apply_substitute(&mut e, &cmd, 0..=0).unwrap();
        assert_eq!(buf_line(&e, 0), "a1b1");
    }

    // ── expand_replacement: case escapes + ~ ──────────────────────────────────

    fn expand(raw: &str, pat: &str, text: &str, prev: &str) -> String {
        let re = Regex::new(pat).unwrap();
        let caps = re.captures(text).unwrap();
        expand_replacement(raw, &caps, prev)
    }

    #[test]
    fn expand_case_upper_run_and_end() {
        // `\U…\E` uppercases a run; text after `\E` is unaffected.
        assert_eq!(expand("\\U\\0\\Ex", "foo", "foo", ""), "FOOx");
        assert_eq!(expand("\\L&\\E", "FOO", "FOO", ""), "foo");
    }

    #[test]
    fn expand_case_one_shot() {
        // `\u` / `\l` affect only the next char.
        assert_eq!(expand("\\u\\0", "foo", "foo", ""), "Foo");
        assert_eq!(expand("\\l\\0", "FOO", "FOO", ""), "fOO");
    }

    #[test]
    fn expand_case_applies_to_group() {
        // Case escape applied across a capture group and a following literal.
        assert_eq!(expand("\\U\\1-y\\E", "(f)oo", "foo", ""), "F-Y");
    }

    /// B18: `\u&` on a whole-word match — matches vim's
    /// `:s/\w\+/\u&/` on `"hello world"` → `"Hello world"` (verified
    /// against nvim v0.12.4).
    #[test]
    fn expand_backslash_u_uppercases_first_char_of_group() {
        assert_eq!(expand("\\u\\1", "(\\w+)", "hello world", ""), "Hello");
    }

    /// A one-shot `\u`/`\l` takes priority for exactly the next char, then
    /// FALLS BACK to any active `\U`/`\L` span rather than clearing it —
    /// verified against nvim: `:s/\w\+/\U\l&/` on `"hello"` → `"hELLO"`
    /// (not `"hello"`, and not `"HELLO"`).
    #[test]
    fn expand_one_shot_falls_back_to_active_span() {
        assert_eq!(expand("\\U\\l\\0", "hello", "hello", ""), "hELLO");
        // Same interaction the other way around: `\l\U\1 \2` on
        // "hello world" → "hELLO WORLD" (nvim-verified).
        assert_eq!(
            expand("\\l\\U\\1 \\2", "(\\w+) (\\w+)", "hello world", ""),
            "hELLO WORLD"
        );
    }

    #[test]
    fn expand_literal_dollar_and_amp() {
        assert_eq!(expand("$\\0", "x", "x", ""), "$x");
        assert_eq!(expand("[&]", "foo", "foo", ""), "[foo]");
        assert_eq!(expand("\\&", "foo", "foo", ""), "&");
    }

    #[test]
    fn expand_tilde_uses_previous_replacement() {
        // `~` expands to the previous replacement, re-evaluated against caps.
        assert_eq!(expand("~!", "x", "x", "PREV"), "PREV!");
        assert_eq!(expand("~", "(.)", "a", "[\\1]"), "[a]");
        // `\~` is a literal tilde.
        assert_eq!(expand("\\~", "x", "x", "PREV"), "~");
    }

    // ── `n` flag: report count, no mutation ───────────────────────────────────

    #[test]
    fn apply_report_only_counts_without_mutating() {
        let mut e = editor_with("foo foo foo");
        let cmd = parse_substitute("/foo/bar/gn").unwrap();
        assert!(cmd.flags.report_only);
        let out = apply_substitute(&mut e, &cmd, 0..=0).unwrap();
        assert_eq!(out.replacements, 3);
        // View is untouched.
        assert_eq!(buf_line(&e, 0), "foo foo foo");
    }

    // ── case escapes through the full apply path ──────────────────────────────

    #[test]
    fn apply_upper_run() {
        let mut e = editor_with("hello world");
        let cmd = parse_substitute("/world/\\U&\\E/").unwrap();
        apply_substitute(&mut e, &cmd, 0..=0).unwrap();
        assert_eq!(buf_line(&e, 0), "hello WORLD");
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
        // /\(\w\+\)/<<\1>>/ — the replacement template has a capture group.
        let cmd = parse_substitute("/\\(\\w\\+\\)/<<\\1>>/g").unwrap();
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
        let cmd = parse_substitute("/\\(foo\\|bar\\|baz\\)/X/g").unwrap();
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
        let cmd = parse_substitute("/\\(\\w\\+\\)/<<\\1>>/g").unwrap();
        let matches = collect_substitute_matches(&e, &cmd, 0..=0).unwrap();
        let accepted = vec![true; matches.len()];
        let applied = apply_collected_matches(&mut e, &matches, &accepted);
        assert_eq!(applied, 2);
        assert_eq!(buf_line(&e, 0), "<<hello>> <<world>>");
    }

    // ── V5: magic `~` on the PATTERN side of `:s` and `/`/`?` ─────────────────
    // `apply_substitute` does NOT store `last_substitute` itself (the ex layer
    // does), so these tests set it explicitly to simulate a prior `:s`.

    /// nvim-verified: `:s/foo/BAR/` then `:s/~/baz/` — the second command's
    /// pattern `~` expands to `BAR`, matches the just-inserted `BAR`, → `baz`.
    #[test]
    fn pattern_tilde_expands_to_last_substitute() {
        let mut e = editor_with("foo");
        let first = parse_substitute("/foo/BAR/").unwrap();
        apply_substitute(&mut e, &first, 0..=0).unwrap();
        assert_eq!(buf_line(&e, 0), "BAR");
        e.set_last_substitute(first); // ex layer normally does this

        let second = parse_substitute("/~/baz/").unwrap();
        let out = apply_substitute(&mut e, &second, 0..=0).unwrap();
        assert_eq!(out.replacements, 1, "pattern `~` must match `BAR`");
        assert_eq!(buf_line(&e, 0), "baz");
    }

    /// nvim-verified: `\~` in the pattern is a literal tilde — it matches a real
    /// `~` character and does NOT expand to the last-substitute text.
    #[test]
    fn pattern_escaped_tilde_stays_literal() {
        let mut e = editor_with("a~b");
        // Prior `:s` set the last replacement to BAR; `\~` must ignore it.
        e.set_last_substitute(parse_substitute("/x/BAR/").unwrap());
        let cmd = parse_substitute("/\\~/X/").unwrap();
        let out = apply_substitute(&mut e, &cmd, 0..=0).unwrap();
        assert_eq!(out.replacements, 1, "`\\~` must match the literal tilde");
        assert_eq!(buf_line(&e, 0), "aXb");
    }

    /// No previous substitute → pattern `~` expands to empty (documented
    /// divergence from nvim's `E33`; the empty choice never corrupts text).
    /// Here `:s/a~b/X/` on `"ab"` becomes pattern `ab`, which matches → `X`.
    #[test]
    fn pattern_tilde_no_previous_substitute_expands_empty() {
        let mut e = editor_with("ab");
        assert!(e.last_substitute().is_none());
        let cmd = parse_substitute("/a~b/X/").unwrap();
        let out = apply_substitute(&mut e, &cmd, 0..=0).unwrap();
        assert_eq!(out.replacements, 1, "`~`→empty so pattern is `ab`");
        assert_eq!(buf_line(&e, 0), "X");
    }

    /// A `/` search routes through the SAME `resolve_case_mode` path as the
    /// `:s` LHS (`Editor::push_search_pattern`), so one test covers the shared
    /// path: after a prior `:s/foo/BAR/`, searching `/~` compiles a regex that
    /// matches `BAR`. nvim-verified: `/~` finds the last-substitute text.
    #[test]
    fn search_pattern_tilde_shares_expansion_path() {
        let mut e = editor_with("BAR");
        e.set_last_substitute(parse_substitute("/foo/BAR/").unwrap());
        e.push_search_pattern("~");
        let re = e
            .search_state()
            .pattern
            .as_ref()
            .expect("`/~` must compile to a pattern");
        assert!(re.is_match("BAR"), "search `~` must expand to `BAR`");
        assert!(
            !re.is_match("~"),
            "search `~` must not match a literal tilde"
        );
    }
}
