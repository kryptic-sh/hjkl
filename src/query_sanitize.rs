#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QuerySanitizeReport {
    pub changed: bool,
    /// Number of `(#set! @...)` directives removed.
    pub removed_lines: usize,
}

/// Remove every `(#set! @<capture> ...)` subexpression from `src` using
/// paren-balanced scanning.  Only forms whose first non-whitespace argument
/// starts with `@` are excised; forms like `(#set! "priority" 99)` or
/// `(#set! priority 99)` are left intact.
///
/// String literals (`"..."` with `\` escapes) are tracked so a `)` inside a
/// string never fools the paren counter.
pub fn sanitize_highlights(src: &str) -> (String, QuerySanitizeReport) {
    let bytes = src.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(len);
    let mut pos = 0usize;
    let mut removed = 0usize;

    while pos < len {
        // Look for the literal token `(#set!`.
        if let Some(rel) = find_set_at(bytes, pos) {
            let set_start = pos + rel;

            // Copy everything before this occurrence verbatim.
            out.push_str(&src[pos..set_start]);

            // Peek past `(#set!` to find the first non-whitespace argument.
            let after_keyword = set_start + 6; // len("(#set!") == 6
            let first_arg_start = skip_whitespace(bytes, after_keyword);

            let is_capture_form = first_arg_start < len && bytes[first_arg_start] == b'@';

            if is_capture_form {
                // Scan to the matching `)` of this whole `(#set! ...)` form.
                // We already consumed the opening `(`.
                let end = find_matching_paren(bytes, set_start);
                pos = end; // skip past the closing `)`
                removed += 1;
            } else {
                // Not a capture form — emit the `(#set!` token and continue.
                out.push_str("(#set!");
                pos = after_keyword;
            }
        } else {
            // No more `(#set!` in the remaining input — copy the rest verbatim.
            out.push_str(&src[pos..]);
            pos = len;
        }
    }

    // Match the original contract: trim trailing newlines from output so
    // callers that compare against `src.trim_end_matches('\n')` get a stable
    // result regardless of whether any directives were removed.
    let out = out.trim_end_matches('\n').to_string();
    let normalized_in = src.trim_end_matches('\n');
    let changed = out != normalized_in;
    (
        out,
        QuerySanitizeReport {
            changed,
            removed_lines: removed,
        },
    )
}

// ── helpers ──────────────────────────────────────────────────────────────────

/// Search `bytes[from..]` for the byte sequence `(#set!`.
/// Returns the *relative* offset from `from` if found, or `None`.
fn find_set_at(bytes: &[u8], from: usize) -> Option<usize> {
    let needle = b"(#set!";
    bytes[from..]
        .windows(needle.len())
        .position(|w| w == needle)
}

/// Return the index of the first byte in `bytes[from..]` that is not ASCII
/// whitespace, or `bytes.len()` if none.
fn skip_whitespace(bytes: &[u8], from: usize) -> usize {
    let mut i = from;
    while i < bytes.len()
        && (bytes[i] == b' ' || bytes[i] == b'\t' || bytes[i] == b'\n' || bytes[i] == b'\r')
    {
        i += 1;
    }
    i
}

/// Given that `bytes[start]` is the `(` that opens a `(#set! ...)` form,
/// scan forward tracking paren depth and string literals, and return the index
/// *one past* the matching `)`.
fn find_matching_paren(bytes: &[u8], start: usize) -> usize {
    debug_assert_eq!(bytes[start], b'(');
    let mut depth = 0usize;
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                // Skip string literal, handling `\"` escapes.
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i += 2; // skip escape sequence
                    } else if bytes[i] == b'"' {
                        i += 1;
                        break;
                    } else {
                        i += 1;
                    }
                }
            }
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                if depth == 0 {
                    // Should not happen if outer caller opened with `(`.
                    return i + 1;
                }
                depth -= 1;
                i += 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => {
                i += 1;
            }
        }
    }
    // Unterminated — return end of input.
    bytes.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── existing tests (updated for new logic) ────────────────────────────────

    #[test]
    fn removes_capture_set_directive_lines() {
        // Standalone form on its own line — must still be excised.
        let src = "(tag_name) @tag\n(#set! @string.special.url url @string.special.url)\n";
        let (san, report) = sanitize_highlights(src);
        // The directive itself is gone; surrounding content preserved.
        assert!(
            !san.contains("#set!"),
            "directive should be removed: {san:?}"
        );
        assert!(
            san.contains("(tag_name) @tag"),
            "surrounding content must remain"
        );
        assert!(report.changed);
        assert_eq!(report.removed_lines, 1);
        // Paren balance: count `(` minus `)` outside strings must be zero.
        assert_eq!(
            paren_balance(&san),
            0,
            "parens unbalanced after sanitize: {san:?}"
        );
    }

    #[test]
    fn keeps_regular_set_directive_lines() {
        let src = "((attribute (quoted_attribute_value) @string)\n  (#set! priority 99))\n";
        let (san, report) = sanitize_highlights(src);
        assert_eq!(san, src.trim_end_matches('\n'));
        assert!(!report.changed);
        assert_eq!(report.removed_lines, 0);
    }

    // ── new regression test: multi-line html_tags pattern ────────────────────

    #[test]
    fn html_tags_multiline_pattern_paren_balanced() {
        // Verbatim from nvim-treesitter html_tags/highlights.scm — this is the
        // pattern that previously caused "Invalid syntax" because the whole last
        // line was dropped, removing BOTH the `#set!` closing `)` and the outer
        // `)` that closes the enclosing predicate group.
        let src = r#"((attribute
  (attribute_name) @_attr
  (quoted_attribute_value
    (attribute_value) @string.special.url))
  (#any-of? @_attr "href" "src")
  (#set! @string.special.url url @string.special.url))
(entity) @character.special"#;

        let (san, report) = sanitize_highlights(src);

        // The `#set!` directive must be gone.
        assert!(
            !san.contains("#set!"),
            "directive should be removed: {san:?}"
        );

        // The `#any-of?` predicate must remain.
        assert!(
            san.contains("(#any-of? @_attr \"href\" \"src\")"),
            "any-of predicate must remain: {san:?}"
        );

        // Parens must be balanced.
        assert_eq!(
            paren_balance(&san),
            0,
            "parens unbalanced after sanitize: {san:?}"
        );

        assert!(report.changed);
        assert_eq!(report.removed_lines, 1);
    }

    // ── new: literal first-arg form is preserved ─────────────────────────────

    #[test]
    fn keeps_literal_priority_set_directive() {
        let src = "((node) @foo\n  (#set! \"priority\" 99))\n";
        let (san, report) = sanitize_highlights(src);
        assert_eq!(san, src.trim_end_matches('\n'));
        assert!(!report.changed);
        assert_eq!(report.removed_lines, 0);
    }

    // ── new: string literal containing `)` must not confuse the parser ───────

    #[test]
    fn string_with_paren_inside_excised_correctly() {
        // The `)` inside `"weird)stuff"` is part of the string — it must NOT
        // be treated as the closing paren of the `#set!` form.
        let src = "before (#set! @x \"weird)stuff\") after";
        let (san, report) = sanitize_highlights(src);

        assert!(
            !san.contains("#set!"),
            "directive should be removed: {san:?}"
        );
        assert!(san.contains("before"), "prefix must remain: {san:?}");
        assert!(san.contains("after"), "suffix must remain: {san:?}");
        assert_eq!(paren_balance(&san), 0, "parens unbalanced: {san:?}");
        assert!(report.changed);
        assert_eq!(report.removed_lines, 1);
    }

    // ── helper ───────────────────────────────────────────────────────────────

    /// Count open-parens minus close-parens, ignoring characters inside `"..."`.
    fn paren_balance(s: &str) -> i64 {
        let mut balance = 0i64;
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '"' => {
                    // skip until closing `"`, honoring `\` escapes
                    loop {
                        match chars.next() {
                            None | Some('"') => break,
                            Some('\\') => {
                                chars.next();
                            }
                            _ => {}
                        }
                    }
                }
                '(' => balance += 1,
                ')' => balance -= 1,
                _ => {}
            }
        }
        balance
    }
}
