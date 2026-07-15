/// A parsed line range. 1-based, inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineRange {
    start: usize,
    end: usize,
}

impl LineRange {
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    pub fn start_one_based(&self) -> usize {
        self.start
    }

    pub fn end_one_based(&self) -> usize {
        self.end
    }

    pub fn single(line: usize) -> Self {
        Self {
            start: line,
            end: line,
        }
    }
}

// ---- address parsing -------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum Address {
    Number(usize), // 1-based, as the user typed
    Current,
    Last,
    Mark(char),
}

/// Strip a leading base address from `s`, return `(address, remainder)` or
/// `None`. Does NOT consume a trailing offset (`+3`, `-2`, ...) — that's
/// [`parse_offset_chain`]'s job. A leading `+`/`-` (no explicit base) is
/// treated as an implicit `.` (current line) base and left unconsumed so the
/// offset parser picks it up.
fn parse_base_address(s: &str) -> Option<(Address, &str)> {
    let mut chars = s.char_indices();
    let (_, first) = chars.next()?;
    match first {
        '.' => Some((Address::Current, &s[1..])),
        '$' => Some((Address::Last, &s[1..])),
        '+' | '-' => Some((Address::Current, s)), // implicit `.`; leave sign for offset parsing
        '\'' => {
            // The mark char may be multibyte (e.g. `:'é`); slice at its real
            // byte boundary rather than a hard-coded `2` to avoid a
            // char-boundary panic that would crash the editor.
            let (mark_start, mark) = chars.next()?;
            Some((Address::Mark(mark), &s[mark_start + mark.len_utf8()..]))
        }
        '0'..='9' => {
            let mut end = 1;
            for (i, c) in s.char_indices().skip(1) {
                if c.is_ascii_digit() {
                    end = i + c.len_utf8();
                } else {
                    break;
                }
            }
            let n: usize = s[..end].parse().ok()?;
            Some((Address::Number(n), &s[end..]))
        }
        _ => None,
    }
}

/// Consume a chain of `+N` / `-N` / bare `+` / `-` offset terms from the
/// front of `s` (vim allows repeats like `++`, `+-`, `.+3-1`), summing them.
/// Bare `+`/`-` (no digits) count as 1. Returns `(total_offset, remainder)`;
/// `(0, s)` when `s` has no leading offset term.
fn parse_offset_chain(mut s: &str) -> (i64, &str) {
    let mut total: i64 = 0;
    loop {
        let sign: i64 = match s.chars().next() {
            Some('+') => 1,
            Some('-') => -1,
            _ => break,
        };
        let after_sign = &s[1..];
        let mut end = 0;
        for (i, c) in after_sign.char_indices() {
            if c.is_ascii_digit() {
                end = i + c.len_utf8();
            } else {
                break;
            }
        }
        let magnitude: i64 = if end == 0 {
            1
        } else {
            match after_sign[..end].parse() {
                Ok(v) => v,
                Err(_) => break,
            }
        };
        total += sign * magnitude;
        s = &after_sign[end..];
    }
    (total, s)
}

/// Strip a leading address (base + optional offset chain) from `s`. Returns
/// `(base_address, offset, remainder)` or `None` when `s` doesn't start with
/// a valid address character.
fn parse_address(s: &str) -> Option<(Address, i64, &str)> {
    let (base, rest) = parse_base_address(s)?;
    let (offset, rest) = parse_offset_chain(rest);
    Some((base, offset, rest))
}

/// Resolve a parsed address (base + offset) against the current editor
/// state. Numbers are 1-based; the final `base + offset` is clamped to the
/// buffer. Bad marks return an error.
fn resolve_address<H: hjkl_engine::Host>(
    addr: Address,
    offset: i64,
    editor: &hjkl_engine::Editor<hjkl_buffer::View, H>,
) -> Result<usize, String> {
    let line_count = editor.buffer().row_count();
    // 1-based last line (at least 1 so single-line buffers work)
    let last = line_count.max(1);
    let base: i64 = match addr {
        Address::Number(n) => n as i64,
        Address::Current => editor.cursor().0 as i64 + 1, // cursor is 0-based
        Address::Last => last as i64,
        Address::Mark(c) => editor
            .mark(c)
            .map(|(r, _)| r as i64 + 1) // 0-based → 1-based
            .ok_or_else(|| format!("mark `{c}` not set"))?,
    };
    Ok((base + offset).clamp(1, last as i64) as usize)
}

// ---- public API ------------------------------------------------------------

/// Parse a leading range prefix from `cmd`. Supports:
/// - `5`        → single line 5
/// - `5,10`     → 5 through 10
/// - `.,$`      → current line through last line
/// - `'a,'b`    → mark a through mark b
/// - `%`        → whole buffer (1 through line_count)
///
/// Returns `(parsed_range, remainder)`. `parsed_range` is `None` when the
/// command starts with a non-range character (typical case for `:w`, `:e`).
pub fn parse_range<'a, H: hjkl_engine::Host>(
    cmd: &'a str,
    editor: &hjkl_engine::Editor<hjkl_buffer::View, H>,
) -> Result<(Option<LineRange>, &'a str), String> {
    // `%` — whole buffer
    if let Some(rest) = cmd.strip_prefix('%') {
        let line_count = editor.buffer().row_count().max(1);
        return Ok((Some(LineRange::new(1, line_count)), rest));
    }

    let Some((start_addr, start_offset, after_start)) = parse_address(cmd) else {
        return Ok((None, cmd));
    };

    let start = resolve_address(start_addr, start_offset, editor)?;

    if let Some(after_comma) = after_start.strip_prefix(',') {
        // Expect a second address after the comma. If absent, error.
        if after_comma.is_empty() {
            return Err("missing end address after ','".into());
        }
        let Some((end_addr, end_offset, rest)) = parse_address(after_comma) else {
            // Something like `5,x` where `x` is not an address character
            return Err(format!("invalid end address in range: `{after_comma}`"));
        };
        let end = resolve_address(end_addr, end_offset, editor)?;
        if start > end {
            // Vim parity: a backward range (`:8,3d`) is rejected outright in
            // non-interactive use — real vim only offers to swap it via an
            // interactive "OK to swap" prompt, which hjkl (headless/keystroke
            // driven) has no equivalent of, so the plain error is the correct
            // match here. Previously this silently swapped to `(end, start)`,
            // which let e.g. `:8,3d` delete lines 3-8 instead of erroring.
            return Err("E493: Backwards range given".into());
        }
        return Ok((Some(LineRange::new(start, end)), rest));
    }

    Ok((Some(LineRange::single(start)), after_start))
}

/// Parse a single destination address for `:move` / `:copy` — `$`, `.`, a
/// number, a mark, or `0` (meaning "before the first line"). Returns a 1-based
/// line number, or `0` for the top-of-buffer destination. Trailing text after
/// the address is an error.
pub fn parse_dest_address<H: hjkl_engine::Host>(
    s: &str,
    editor: &hjkl_engine::Editor<hjkl_buffer::View, H>,
) -> Result<usize, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("expected a destination address".into());
    }
    let (addr, offset, rest) = parse_address(s).ok_or_else(|| format!("invalid address: `{s}`"))?;
    if !rest.trim().is_empty() {
        return Err(format!("trailing characters after address: `{rest}`"));
    }
    // `0` is a legal destination (place before line 1); resolve_address would
    // clamp it to 1, so special-case it here.
    if let (Address::Number(0), 0) = (addr, offset) {
        return Ok(0);
    }
    resolve_address(addr, offset, editor)
}

// ---- tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use hjkl_engine::{DefaultHost, Editor, Options};

    fn make_editor_with_lines(lines: &[&str]) -> Editor<hjkl_buffer::View, DefaultHost> {
        use hjkl_buffer::View;
        let content = lines.join("\n");
        let buf = View::from_str(&content);
        let host = DefaultHost::new();
        hjkl_vim::vim_editor(buf, host, Options::default())
    }

    fn make_editor() -> Editor<hjkl_buffer::View, DefaultHost> {
        make_editor_with_lines(&["line1", "line2", "line3", "line4", "line5"])
    }

    // Helper: parse range on a 5-line editor, check start/end (1-based).
    fn parse(cmd: &str) -> Result<(Option<(usize, usize)>, String), String> {
        let e = make_editor();
        parse_range(cmd, &e).map(|(r, rest)| (r.map(|lr| (lr.start, lr.end)), rest.to_owned()))
    }

    #[test]
    fn multibyte_mark_does_not_panic() {
        // Regression: `&s[2..]` assumed a 1-byte mark; a multibyte mark char
        // (`:'é`) sliced mid-char and crashed the editor. Must resolve
        // gracefully (unknown mark → error) rather than panic.
        let _ = parse("'é");
        let _ = parse("'あx");
        let _ = parse("'😀,5");
    }

    #[test]
    fn bare_number() {
        let (r, rest) = parse("5").unwrap();
        assert_eq!(r, Some((5, 5)));
        assert_eq!(rest, "");
    }

    #[test]
    fn comma_separated() {
        let (r, rest) = parse("5,10").unwrap();
        // editor has 5 lines so 10 is clamped to 5
        assert_eq!(r, Some((5, 5)));
        assert_eq!(rest, "");
    }

    #[test]
    fn comma_separated_within_range() {
        let (r, rest) = parse("2,4").unwrap();
        assert_eq!(r, Some((2, 4)));
        assert_eq!(rest, "");
    }

    #[test]
    fn percent_whole_buffer() {
        let (r, rest) = parse("%").unwrap();
        assert_eq!(r, Some((1, 5)));
        assert_eq!(rest, "");
    }

    #[test]
    fn dot_dollar() {
        // cursor starts at row 0 (1-based: 1), last line is 5
        let (r, rest) = parse(".,$").unwrap();
        assert_eq!(r, Some((1, 5)));
        assert_eq!(rest, "");
    }

    #[test]
    fn mark_range() {
        use hjkl_buffer::View;
        use hjkl_engine::{DefaultHost, Options};
        let buf = View::from_str("a\nb\nc\nd\ne");
        let host = DefaultHost::new();
        let mut editor = hjkl_vim::vim_editor(buf, host, Options::default());
        // marks are 0-based internally; 1-based in range results
        editor.set_mark('a', (0, 0)); // line 1
        editor.set_mark('b', (2, 0)); // line 3
        let (r, rest) = parse_range("'a,'b", &editor).unwrap();
        assert_eq!(r, Some(LineRange::new(1, 3)));
        assert_eq!(rest, "");
    }

    #[test]
    fn range_followed_by_command() {
        let (r, rest) = parse("5,10w").unwrap();
        // 10 clamped to 5 (5-line buffer)
        assert_eq!(r, Some((5, 5)));
        assert_eq!(rest, "w");
    }

    #[test]
    fn range_2_4_followed_by_command() {
        let (r, rest) = parse("2,4w").unwrap();
        assert_eq!(r, Some((2, 4)));
        assert_eq!(rest, "w");
    }

    #[test]
    fn no_range() {
        let (r, rest) = parse("w").unwrap();
        assert_eq!(r, None);
        assert_eq!(rest, "w");
    }

    #[test]
    fn invalid_end_address() {
        let result = parse("5,x");
        assert!(result.is_err(), "expected error for invalid end address");
    }

    #[test]
    fn mark_not_set_returns_error() {
        let result = parse("'z");
        assert!(result.is_err());
    }

    // ---- audit A3: backward ranges error (E493) instead of silently swapping

    #[test]
    fn backward_numeric_range_returns_e493() {
        // `:4,2` — start (4) > end (2) — must error, not silently become
        // `(2, 4)`.
        let result = parse("4,2");
        let err = result.expect_err("backward range must be rejected, not swapped");
        assert!(
            err.contains("E493"),
            "expected E493 backwards-range error, got: {err}"
        );
    }

    #[test]
    fn backward_numeric_range_does_not_produce_swapped_range() {
        // Belt-and-suspenders on top of the message check: confirm the `Err`
        // path is actually taken (no `Ok((Some(range), _))` sneaking through
        // with the swapped bounds).
        let e = make_editor();
        let result = parse_range("4,2", &e);
        assert!(
            result.is_err(),
            "backward range must not resolve to a swapped LineRange, got: {result:?}"
        );
    }

    #[test]
    fn backward_mark_range_returns_e493() {
        use hjkl_buffer::View;
        use hjkl_engine::{DefaultHost, Options};
        let buf = View::from_str("a\nb\nc\nd\ne");
        let host = DefaultHost::new();
        let mut editor = hjkl_vim::vim_editor(buf, host, Options::default());
        // mark 'a' on line 4, mark 'b' on line 2 — 'a,'b is backward.
        editor.set_mark('a', (3, 0)); // line 4
        editor.set_mark('b', (1, 0)); // line 2
        let result = parse_range("'a,'b", &editor);
        let err = result.expect_err("backward mark range must be rejected, not swapped");
        assert!(
            err.contains("E493"),
            "expected E493 backwards-range error, got: {err}"
        );
    }

    #[test]
    fn forward_range_still_works_after_e493_change() {
        // Forward ranges (start <= end) must be completely unaffected.
        let (r, rest) = parse("2,4").unwrap();
        assert_eq!(r, Some((2, 4)));
        assert_eq!(rest, "");
    }

    #[test]
    fn equal_range_is_fine() {
        // start == end is not "backward" — must parse fine, not error.
        let (r, rest) = parse("5,5").unwrap();
        assert_eq!(r, Some((5, 5)));
        assert_eq!(rest, "");
    }

    #[test]
    fn line_range_single_start_equals_end() {
        let r = LineRange::single(5);
        assert_eq!(r.start_one_based(), 5);
        assert_eq!(r.end_one_based(), 5);
    }

    // ---- audit A4: `+N` / `-N` cursor-relative offset addresses -----------

    /// Parse `cmd` on a 5-line editor whose cursor starts on 1-based
    /// `cursor_line`. Returns `(start, end)` (1-based) like `parse`.
    fn parse_from_line(cmd: &str, cursor_line: usize) -> Result<(usize, usize), String> {
        let mut e = make_editor();
        e.set_cursor_quiet(cursor_line - 1, 0);
        let (r, _rest) = parse_range(cmd, &e)?;
        Ok(r.map(|lr| (lr.start_one_based(), lr.end_one_based()))
            .expect("expected a parsed range"))
    }

    #[test]
    fn plus_n_is_cursor_relative_not_absolute() {
        // Audit A4: `:+3` from cursor line 1 must land on line 4 (cursor + 3),
        // NOT absolute line 3 (the old bug: it fell through to the bare
        // line-number fallback, whose `usize::from_str` silently accepts a
        // leading `+`).
        let (start, end) = parse_from_line("+3", 1).unwrap();
        assert_eq!((start, end), (4, 4));
    }

    #[test]
    fn minus_n_is_cursor_relative() {
        // `:-2` from cursor line 5 → line 3.
        let (start, end) = parse_from_line("-2", 5).unwrap();
        assert_eq!((start, end), (3, 3));
    }

    #[test]
    fn bare_plus_is_cursor_plus_one() {
        let (start, end) = parse_from_line("+", 2).unwrap();
        assert_eq!((start, end), (3, 3));
    }

    #[test]
    fn bare_minus_is_cursor_minus_one() {
        let (start, end) = parse_from_line("-", 4).unwrap();
        assert_eq!((start, end), (3, 3));
    }

    #[test]
    fn dot_plus_n_offset() {
        // `.+2` — explicit current-line base with an offset.
        let (start, end) = parse_from_line(".+2", 2).unwrap();
        assert_eq!((start, end), (4, 4));
    }

    #[test]
    fn plus_offset_clamps_to_last_line() {
        // `:+999` on a 5-line buffer clamps to the last line, it doesn't error.
        let (start, end) = parse_from_line("+999", 1).unwrap();
        assert_eq!((start, end), (5, 5));
    }

    #[test]
    fn minus_offset_clamps_to_first_line() {
        // `:-999` on a 5-line buffer clamps to line 1.
        let (start, end) = parse_from_line("-999", 5).unwrap();
        assert_eq!((start, end), (1, 1));
    }

    #[test]
    fn repeated_sign_offsets_accumulate() {
        // vim allows repeated offset terms: `++` = +2, `+-` = 0.
        let (start, end) = parse_from_line("++", 1).unwrap();
        assert_eq!((start, end), (3, 3));
        let (start, end) = parse_from_line("+-", 3).unwrap();
        assert_eq!((start, end), (3, 3));
    }

    #[test]
    fn existing_address_tests_unaffected_by_offset_support() {
        // Sanity: plain numbers, `.`, `$`, and marks with no offset still
        // resolve exactly as before (offset chain parses to 0 and is a
        // no-op).
        let (r, rest) = parse("5").unwrap();
        assert_eq!(r, Some((5, 5)));
        assert_eq!(rest, "");
        let (r, rest) = parse("%").unwrap();
        assert_eq!(r, Some((1, 5)));
        assert_eq!(rest, "");
    }
}
