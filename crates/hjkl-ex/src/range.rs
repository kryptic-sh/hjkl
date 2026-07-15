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

/// Strip a leading address from `s`, return `(address, remainder)` or `None`.
fn parse_address(s: &str) -> Option<(Address, &str)> {
    let mut chars = s.char_indices();
    let (_, first) = chars.next()?;
    match first {
        '.' => Some((Address::Current, &s[1..])),
        '$' => Some((Address::Last, &s[1..])),
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

/// Resolve a parsed address against the current editor state. Numbers are
/// 1-based and clamped to the buffer; bad marks return an error.
fn resolve_address<H: hjkl_engine::Host>(
    addr: Address,
    editor: &hjkl_engine::Editor<hjkl_buffer::View, H>,
) -> Result<usize, String> {
    let line_count = editor.buffer().row_count();
    // 1-based last line (at least 1 so single-line buffers work)
    let last = line_count.max(1);
    match addr {
        Address::Number(n) => Ok(n.clamp(1, last)),
        Address::Current => Ok(editor.cursor().0 + 1), // cursor is 0-based
        Address::Last => Ok(last),
        Address::Mark(c) => editor
            .mark(c)
            .map(|(r, _)| (r + 1).min(last)) // 0-based → 1-based
            .ok_or_else(|| format!("mark `{c}` not set")),
    }
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

    let Some((start_addr, after_start)) = parse_address(cmd) else {
        return Ok((None, cmd));
    };

    let start = resolve_address(start_addr, editor)?;

    if let Some(after_comma) = after_start.strip_prefix(',') {
        // Expect a second address after the comma. If absent, error.
        if after_comma.is_empty() {
            return Err("missing end address after ','".into());
        }
        let Some((end_addr, rest)) = parse_address(after_comma) else {
            // Something like `5,x` where `x` is not an address character
            return Err(format!("invalid end address in range: `{after_comma}`"));
        };
        let end = resolve_address(end_addr, editor)?;
        let (lo, hi) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        return Ok((Some(LineRange::new(lo, hi)), rest));
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
    let (addr, rest) = parse_address(s).ok_or_else(|| format!("invalid address: `{s}`"))?;
    if !rest.trim().is_empty() {
        return Err(format!("trailing characters after address: `{rest}`"));
    }
    // `0` is a legal destination (place before line 1); resolve_address would
    // clamp it to 1, so special-case it here.
    if let Address::Number(0) = addr {
        return Ok(0);
    }
    resolve_address(addr, editor)
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

    #[test]
    fn line_range_single_start_equals_end() {
        let r = LineRange::single(5);
        assert_eq!(r.start_one_based(), 5);
        assert_eq!(r.end_one_based(), 5);
    }
}
