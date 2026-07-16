//! Query sanitizer and `(#set! @capture ...)` pre-extractor.
//!
//! ## Roles
//!
//! 1. **Sanitizer** ([`sanitize_highlights`]) — strips `(#set! @cap ...)` forms
//!    that stock tree-sitter 0.26 rejects at compile time with
//!    "Invalid arguments to set! predicate". Kept as a fallback.
//!
//! 2. **Pre-extractor** ([`extract_capture_set_directives`]) — scans a raw
//!    query text for `(#set! @cap key val)` forms, maps each to its top-level
//!    pattern index (by counting top-level pattern forms — parenthesized
//!    groups, bracket alternations, and string literals all count), returns a
//!    `Vec<CaptureSetDirective>`, and rewrites the query text with those forms
//!    removed so `Query::new` succeeds.  The [`Highlighter`] stores these
//!    alongside the compiled query and re-applies them at match-iteration time.
//!
//! [`Highlighter`]: crate::Highlighter

/// A single `(#set! @capture key val)` form extracted before query compilation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureSetDirective {
    /// Zero-based index of the top-level pattern this directive belongs to.
    pub pattern_index: usize,
    /// The capture name (without the leading `@`), e.g. `"string.special.url"`.
    pub capture_name: String,
    /// The metadata key, e.g. `"url"`.
    pub key: String,
    /// The metadata value, e.g. `"@string.special.url"`.  `None` means the
    /// value was omitted — consumers should treat that as `Bool(true)`.
    pub value: Option<String>,
}

/// Result of [`extract_capture_set_directives`].
pub struct ExtractResult {
    /// Query text with all `(#set! @cap ...)` forms removed.
    pub rewritten: String,
    /// Pre-extracted directives, ordered by their pattern index.
    pub directives: Vec<CaptureSetDirective>,
}

/// Extract all `(#set! @cap key val)` forms from `src`, rewrite the query text
/// without them, and return both the cleaned text and the extracted directives.
///
/// Pattern indices are assigned by counting top-level pattern forms in the
/// original text (before removal).  A "top-level pattern form" is anything
/// that can stand on its own as a whole pattern per tree-sitter's query
/// grammar: a parenthesized group `(...)`, a bracket alternation `[...]`
/// (counted once for the whole alternation, not once per member — its
/// members sit at nesting depth > 0), or a bare string literal `"..."` (which
/// has no parens at all). `(`/`[`/`)`/`]` share one nesting-depth counter so
/// members nested inside a `[...]` alternation are never mistaken for
/// top-level patterns of their own. The same counting applies to the
/// rewritten text — removing a `(#set! @cap ...)` from *inside* a pattern
/// does not change the pattern count.
pub fn extract_capture_set_directives(src: &str) -> ExtractResult {
    let bytes = src.as_bytes();
    let len = bytes.len();

    // First pass: collect (set_start, set_end, pattern_index, capture, key, val)
    // triples for every capture-form `(#set! @...)`.
    //
    // We walk the full byte stream tracking a stack to know the current pattern
    // index.  A "top-level pattern start" is a `(` at depth 0.

    struct Occurrence {
        start: usize, // byte index of the `(`
        end: usize,   // byte index one past the closing `)`
        pattern_index: usize,
        capture_name: String,
        key: String,
        value: Option<String>,
    }

    let mut occurrences: Vec<Occurrence> = Vec::new();
    let mut pattern_count = 0usize; // number of top-level patterns fully opened so far
    let mut depth = 0usize;
    let mut pos = 0usize;

    while pos < len {
        match bytes[pos] {
            b'"' => {
                // A string literal at depth 0 is itself a complete top-level
                // pattern (e.g. `"if" @kw`), not a member of some enclosing
                // paren group — tree-sitter counts it as one pattern. Nested
                // strings (inside a predicate's arguments, or as a member of
                // a `[...]` alternation) are just skipped, same as before.
                if depth == 0 {
                    pattern_count += 1;
                }
                // Skip string literal.
                pos += 1;
                while pos < len {
                    if bytes[pos] == b'\\' {
                        pos += 2;
                    } else if bytes[pos] == b'"' {
                        pos += 1;
                        break;
                    } else {
                        pos += 1;
                    }
                }
            }
            b';' => {
                // Line comment — skip to end of line.
                while pos < len && bytes[pos] != b'\n' {
                    pos += 1;
                }
            }
            b'(' => {
                // Check if this is a `(#set!` token.
                let needle = b"(#set!";
                if bytes[pos..].starts_with(needle) {
                    let set_start = pos;
                    let after_keyword = pos + needle.len();
                    let first_arg_start = skip_whitespace(bytes, after_keyword);
                    if first_arg_start < len && bytes[first_arg_start] == b'@' {
                        // Capture-form `(#set! @cap key val)` — extract it.
                        let set_end = find_matching_paren(bytes, set_start);
                        if let Some(occ) =
                            parse_capture_set(bytes, set_start, first_arg_start, set_end)
                        {
                            // The pattern_index this directive belongs to is the
                            // index of the enclosing top-level group.  At this
                            // point `pattern_count` = number of top-level groups
                            // *already fully seen*, so the current group is
                            // `pattern_count` (if depth > 0) or a standalone
                            // top-level set! (treat as pattern_count too).
                            let idx = if depth > 0 {
                                pattern_count.saturating_sub(1)
                            } else {
                                pattern_count
                            };
                            occurrences.push(Occurrence {
                                start: set_start,
                                end: set_end,
                                pattern_index: idx,
                                capture_name: occ.0,
                                key: occ.1,
                                value: occ.2,
                            });
                            // Advance past the whole form — don't enter it.
                            pos = set_end;
                            // Do NOT increment depth: we skipped the `(`.
                            continue;
                        }
                    }
                }

                if depth == 0 {
                    pattern_count += 1;
                }
                depth += 1;
                pos += 1;
            }
            b'[' => {
                // A `[...]` alternation is ONE top-level pattern regardless
                // of how many parenthesized members it contains — tracked
                // with the same `depth` counter as `(...)` so members inside
                // it (which sit at `depth > 0`) don't each get miscounted as
                // their own top-level pattern.
                if depth == 0 {
                    pattern_count += 1;
                }
                depth += 1;
                pos += 1;
            }
            b')' | b']' => {
                depth = depth.saturating_sub(1);
                pos += 1;
            }
            _ => {
                pos += 1;
            }
        }
    }

    // Second pass: build rewritten string by splicing out occurrences.
    let mut rewritten = String::with_capacity(len);
    let mut cursor = 0usize;
    for occ in &occurrences {
        rewritten.push_str(&src[cursor..occ.start]);
        cursor = occ.end;
    }
    rewritten.push_str(&src[cursor..]);

    let directives = occurrences
        .into_iter()
        .map(|o| CaptureSetDirective {
            pattern_index: o.pattern_index,
            capture_name: o.capture_name,
            key: o.key,
            value: o.value,
        })
        .collect();

    ExtractResult {
        rewritten,
        directives,
    }
}

/// Parse the internals of a `(#set! @cap key val)` form.
///
/// Returns `(capture_name, key, Option<val>)` on success, `None` if the form
/// is malformed.
fn parse_capture_set(
    bytes: &[u8],
    _set_start: usize,
    first_arg_start: usize,
    set_end: usize,
) -> Option<(String, String, Option<String>)> {
    // bytes[first_arg_start] == b'@' guaranteed by caller.
    let cap_start = first_arg_start + 1; // skip `@`
    let cap_end = scan_identifier(bytes, cap_start);
    let capture_name = std::str::from_utf8(&bytes[cap_start..cap_end])
        .ok()?
        .to_string();

    let key_start = skip_whitespace(bytes, cap_end);
    if key_start >= set_end.saturating_sub(1) {
        return None; // no key
    }
    let (key, after_key) = if bytes[key_start] == b'"' {
        // Quoted key.
        let (s, end) = scan_string(bytes, key_start)?;
        (s, end)
    } else {
        let end = scan_identifier(bytes, key_start);
        if end == key_start {
            return None;
        }
        (
            std::str::from_utf8(&bytes[key_start..end])
                .ok()?
                .to_string(),
            end,
        )
    };

    let val_start = skip_whitespace(bytes, after_key);
    if val_start >= set_end.saturating_sub(1) {
        return Some((capture_name, key, None));
    }
    let val = if bytes[val_start] == b'"' {
        // Quoted value.
        let (s, _) = scan_string(bytes, val_start)?;
        s
    } else if bytes[val_start] == b'@' {
        // Unquoted @-reference value.
        let start = val_start + 1;
        let end = scan_identifier(bytes, start);
        let raw = std::str::from_utf8(&bytes[val_start..end]).ok()?;
        raw.to_string()
    } else {
        let end = scan_unquoted(bytes, val_start, set_end);
        std::str::from_utf8(&bytes[val_start..end])
            .ok()?
            .trim()
            .to_string()
    };

    Some((capture_name, key, Some(val)))
}

/// Scan an identifier (letters, digits, `-`, `_`, `.`) starting at `from`.
fn scan_identifier(bytes: &[u8], from: usize) -> usize {
    let mut i = from;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.' {
            i += 1;
        } else {
            break;
        }
    }
    i
}

/// Scan an unquoted token up to `limit` or whitespace/`)`.
fn scan_unquoted(bytes: &[u8], from: usize, limit: usize) -> usize {
    let mut i = from;
    while i < limit && i < bytes.len() {
        let b = bytes[i];
        if b == b')' || b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' {
            break;
        }
        i += 1;
    }
    i
}

/// Scan a `"..."` string literal starting at `from` (must be `"`).
/// Returns `(contents, one_past_closing_quote)`.
fn scan_string(bytes: &[u8], from: usize) -> Option<(String, usize)> {
    debug_assert_eq!(bytes[from], b'"');
    let mut i = from + 1;
    let mut s = String::new();
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 1;
            if i < bytes.len() {
                s.push(bytes[i] as char);
                i += 1;
            }
        } else if bytes[i] == b'"' {
            i += 1;
            return Some((s, i));
        } else {
            s.push(bytes[i] as char);
            i += 1;
        }
    }
    None // unterminated string
}

// ---------------------------------------------------------------------------
// Legacy sanitizer (kept for compatibility + fallback)
// ---------------------------------------------------------------------------

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
///
/// **Note:** [`extract_capture_set_directives`] is the preferred path — it
/// returns the same rewritten text *plus* the extracted directives.  This
/// function is kept for tests and callers that only need the sanitized text.
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

    // ── extract_capture_set_directives ────────────────────────────────────────

    #[test]
    fn extract_finds_capture_set_directive() {
        let src = r#"((attribute
  (attribute_name) @_attr
  (quoted_attribute_value
    (attribute_value) @string.special.url))
  (#any-of? @_attr "href" "src")
  (#set! @string.special.url url @string.special.url))
(entity) @character.special"#;

        let result = extract_capture_set_directives(src);
        assert_eq!(result.directives.len(), 1, "expected 1 extracted directive");
        let d = &result.directives[0];
        assert_eq!(d.capture_name, "string.special.url");
        assert_eq!(d.key, "url");
        assert_eq!(d.value.as_deref(), Some("@string.special.url"));
        // Pattern index: the whole outer `((...) (#any-of? ...) (#set! ...))` is
        // the first top-level pattern, so index 0.
        assert_eq!(d.pattern_index, 0);

        // Rewritten text must not contain #set!.
        assert!(!result.rewritten.contains("#set!"));
        // It must still contain #any-of?.
        assert!(result.rewritten.contains("#any-of?"));
        // Paren balance must be preserved.
        assert_eq!(paren_balance(&result.rewritten), 0);
    }

    #[test]
    fn extract_preserves_non_capture_set() {
        let src = "((attribute (quoted_attribute_value) @string)\n  (#set! priority 99))\n";
        let result = extract_capture_set_directives(src);
        assert!(
            result.directives.is_empty(),
            "non-capture set must not be extracted"
        );
        // Rewritten text must still contain the literal set.
        assert!(result.rewritten.contains("#set! priority"));
    }

    #[test]
    fn extract_pattern_index_counts_string_and_bracket_patterns_correctly() {
        // Three top-level patterns precede none; the query has exactly three
        // top-level patterns total, and the directive lives in the third:
        //   0: a string-literal pattern ("if" @kw) — has no `(` at all, so
        //      naive "count depth-0 `(`" logic never counts it.
        //   1: a `[...]` bracket alternation containing THREE parenthesized
        //      members — tree-sitter counts the whole alternation as ONE
        //      pattern, but naive depth-0-paren counting (which doesn't
        //      track bracket depth) sees each member's `(` sitting at
        //      "depth 0" and counts three separate patterns for it.
        //   2: the pattern carrying the `#set!` directive.
        //
        // A 2-member bracket would let the missing-string undercount (-1)
        // and the bracket-member overcount (+1) cancel out by coincidence,
        // so this test uses three members to force a real, unambiguous
        // mismatch: naive counting reaches pattern_count = 4 (1 for the
        // first bracket member treated as top-level, 2 for the second, 3
        // for the third, 4 for the outer `(...)`) and assigns the
        // directive index 3 (pattern_count - 1), not the correct 2.
        let src = r#""if" @kw
[
  (foo) @a
  (bar) @b
  (baz) @c
] @alt
((qux) @q
  (#set! @q key val))
"#;

        let result = extract_capture_set_directives(src);
        assert_eq!(result.directives.len(), 1, "expected 1 extracted directive");
        assert_eq!(
            result.directives[0].pattern_index, 2,
            "directive must bind to the third top-level pattern (index 2): \
             0=string literal, 1=bracket alternation, 2=the #set! pattern"
        );
        assert_eq!(result.directives[0].capture_name, "q");
        assert_eq!(result.directives[0].key, "key");
        assert_eq!(result.directives[0].value.as_deref(), Some("val"));

        assert!(!result.rewritten.contains("#set!"));
        assert_eq!(paren_balance(&result.rewritten), 0);
    }

    #[test]
    fn extract_assigns_correct_pattern_index_two_patterns() {
        // Two top-level patterns; directive is in the second.
        let src = "(foo) @foo\n((bar) @bar\n  (#set! @bar key val))\n";
        let result = extract_capture_set_directives(src);
        assert_eq!(result.directives.len(), 1);
        assert_eq!(result.directives[0].pattern_index, 1);
        assert_eq!(result.directives[0].capture_name, "bar");
        assert_eq!(result.directives[0].key, "key");
        assert_eq!(result.directives[0].value.as_deref(), Some("val"));
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
