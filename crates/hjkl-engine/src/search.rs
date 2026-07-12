//! Engine-owned search state + execution helpers.
//!
//! Patch 0.0.35 step 1 of the 33-method classification rollout
//! (see `DESIGN_33_METHOD_CLASSIFICATION.md`). The pattern, per-row
//! match cache, and `wrapscan` flag previously lived on
//! [`hjkl_buffer::Buffer`] (private `SearchState`). Moving the FSM
//! state out of the buffer keeps multi-window hosts from sharing the
//! "current search" across panes that happen to share content.
//!
//! The buffer keeps `Search::find_next` / `Search::find_prev` (the
//! SPEC trait surface — pure observers, caller owns the regex). This
//! module composes those primitives with the Editor-owned
//! [`SearchState`] to drive `n` / `N` / `*` / `#` / `/` / `?`.
//!
//! 0.0.37: the buffer-inherent `search_forward` / `search_backward`
//! / `search_matches` / `set_search_pattern` / `search_pattern` /
//! `set_search_wrap` / `search_wraps` accessors are removed. Search
//! state lives on `Editor::search_state`, the rendering path
//! (`BufferView`) takes the active `&Regex` as a parameter, and the
//! `Search` trait impl always wraps (engine controls non-wrap
//! semantics).

use regex::Regex;

use crate::types::{Cursor, Query, Search};
use hjkl_vim_types::Operator;

/// Active `/` or `?` search prompt. Text mutations drive the textarea's
/// live search pattern so matches highlight as the user types.
#[derive(Debug, Clone)]
pub struct SearchPrompt {
    pub text: String,
    pub cursor: usize,
    pub forward: bool,
    /// Operator-pending search (`d/pat`, `c/pat`, `y/pat`): the operator, its
    /// count, and the cursor position where the operator started. `None` for a
    /// plain `/` / `?` search. On commit the operator runs over the (exclusive,
    /// charwise) range from `origin` to the match.
    pub operator: Option<(Operator, usize, (usize, usize))>,
}

/// Case-sensitivity policy derived from `:set ignorecase` / `:set smartcase`.
///
/// Use [`CaseMode::from_options`] to build from two booleans, then pass to
/// [`resolve_case_mode`] together with the raw pattern string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseMode {
    /// Always case-sensitive regardless of the pattern.
    Sensitive,
    /// Always case-insensitive regardless of the pattern.
    Insensitive,
    /// Case-insensitive unless the pattern contains an uppercase rune
    /// (vim's `smartcase` behaviour).
    Smart,
}

impl CaseMode {
    /// Build a `CaseMode` from the two option booleans.
    ///
    /// | `ignorecase` | `smartcase` | Result        |
    /// |---|---|---|
    /// | `false` | `*`   | `Sensitive`   |
    /// | `true`  | `false` | `Insensitive` |
    /// | `true`  | `true`  | `Smart`       |
    pub fn from_options(ignorecase: bool, smartcase: bool) -> Self {
        if !ignorecase {
            CaseMode::Sensitive
        } else if smartcase {
            CaseMode::Smart
        } else {
            CaseMode::Insensitive
        }
    }
}

/// Strip `\c` / `\C` overrides from `pat`, resolve the effective
/// [`CaseMode`], and return the cleaned pattern together with the
/// resolved mode.
///
/// ### Override rules (mirrors vim)
///
/// - `\c` anywhere in `pat` forces case-insensitive.
/// - `\C` anywhere in `pat` forces case-sensitive.
/// - When both appear the **last** one wins.
/// - Both are stripped from the returned pattern.
///
/// ### Smart-case detection
///
/// When `base` is [`CaseMode::Smart`] and no `\c`/`\C` override was
/// found, the pattern is scanned for uppercase Unicode letters. Any
/// uppercase letter → `Sensitive`; otherwise → `Insensitive`.
///
/// ### Per-substitute flag interaction
///
/// The `:s/…/…/i` and `:s/…/…/I` flags are handled in
/// `apply_substitute` **before** calling this function (they
/// short-circuit entirely). This function is not involved.
pub fn resolve_case_mode(pat: &str, base: CaseMode) -> (String, CaseMode) {
    let mut out = String::with_capacity(pat.len());
    let mut chars = pat.chars().peekable();
    // None = no override seen yet; Some(true) = \c (insensitive); Some(false) = \C (sensitive).
    let mut override_mode: Option<bool> = None;

    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.peek() {
                Some('c') => {
                    chars.next();
                    override_mode = Some(true); // \c → insensitive
                }
                Some('C') => {
                    chars.next();
                    override_mode = Some(false); // \C → sensitive
                }
                Some('<') => {
                    chars.next();
                    out.push_str(r"\b");
                }
                Some('>') => {
                    chars.next();
                    out.push_str(r"\b");
                }
                _ => {
                    out.push('\\');
                    if let Some(next) = chars.next() {
                        out.push(next);
                    }
                }
            }
        } else {
            out.push(ch);
        }
    }

    let resolved = match override_mode {
        Some(true) => CaseMode::Insensitive,
        Some(false) => CaseMode::Sensitive,
        None => match base {
            CaseMode::Smart => {
                // Any uppercase rune → sensitive.
                if out.chars().any(|c| c.is_uppercase()) {
                    CaseMode::Sensitive
                } else {
                    CaseMode::Insensitive
                }
            }
            other => other,
        },
    };

    (out, resolved)
}

/// Rewrite vim-style word-boundary escapes to Rust `regex`-compatible form
/// **and** strip `\c`/`\C` case overrides.
///
/// The `regex` crate supports `\b` (symmetric word boundary) but not the
/// vim/PCRE `\<` (word-boundary start) or `\>` (word-boundary end) variants.
/// This function performs a single-pass rewrite:
///
/// - `\<` → `\b`
/// - `\>` → `\b`
/// - `\c` / `\C` stripped (case override — handled by [`resolve_case_mode`])
/// - `\\<` / `\\>` (literal double-backslash followed by `<`/`>`) are left
///   untouched — only the unescaped form transforms.
/// - All other syntax (`\b`, `\B`, `\d`, anchors, …) passes through unchanged.
///
/// Call this on the raw user-typed pattern string **before** passing to
/// `regex::Regex::new`. Keep the original string for display / history.
///
/// Prefer [`resolve_case_mode`] when you also need to apply case semantics;
/// that function performs the same boundary rewrite internally.
pub fn vim_to_rust_regex(pat: &str) -> String {
    resolve_case_mode(pat, CaseMode::Sensitive).0
}

/// Per-row match cache keyed against the buffer's `dirty_gen`. Live
/// alongside the active pattern so re-running `n` doesn't re-scan
/// rows the buffer hasn't touched.
#[derive(Debug, Clone, Default)]
pub struct SearchState {
    /// Active pattern, if any. `None` clears highlighting and makes
    /// `n` / `N` no-op until the next `/` / `?` commit.
    pub pattern: Option<Regex>,
    /// `true` for `/`, `false` for `?` — drives `n` vs `N` direction.
    /// Mirrors `vim.last_search_forward`; consolidated so future
    /// patches can drop the duplicate.
    pub forward: bool,
    /// `matches[row]` is the `(byte_start, byte_end)` runs cached on
    /// `row`, captured at `gen[row]`. Length grows lazily.
    pub matches: Vec<Vec<(usize, usize)>>,
    /// Per-row generation tag. When the buffer's `dirty_gen` for a
    /// row diverges, the row gets re-scanned on next access.
    pub generations: Vec<u64>,
    /// Wrap past buffer ends. Mirrors `Settings::wrapscan`.
    pub wrap_around: bool,
}

impl SearchState {
    /// Empty state — no pattern, forward direction, wraps.
    pub fn new() -> Self {
        Self {
            pattern: None,
            forward: true,
            matches: Vec::new(),
            generations: Vec::new(),
            wrap_around: true,
        }
    }

    /// Replace the active pattern. Drops the cached match runs so
    /// the next access re-scans against the new regex.
    pub fn set_pattern(&mut self, re: Option<Regex>) {
        self.pattern = re;
        self.matches.clear();
        self.generations.clear();
    }

    /// Refresh `matches[row]` if either the row's gen has rolled or
    /// we never scanned it. Returns the cached slice.
    pub fn matches_for(&mut self, row: usize, line: &str, dirty_gen: u64) -> &[(usize, usize)] {
        let Some(ref re) = self.pattern else {
            return &[];
        };
        if self.matches.len() <= row {
            self.matches.resize_with(row + 1, Vec::new);
            self.generations.resize(row + 1, u64::MAX);
        }
        if self.generations[row] != dirty_gen {
            self.matches[row] = re.find_iter(line).map(|m| (m.start(), m.end())).collect();
            self.generations[row] = dirty_gen;
        }
        &self.matches[row]
    }
}

/// Move the cursor to the next match starting from (or just after,
/// when `skip_current = true`) the cursor. Wraps end-of-buffer to
/// row 0 when `state.wrap_around`. Returns `true` when a match was
/// found.
///
/// Pure observe + cursor mutation — no auto-scroll. The Editor's
/// post-step `ensure_cursor_in_scrolloff` reapplies viewport
/// follow.
pub fn search_forward<B: Cursor + Query + Search>(
    buf: &mut B,
    state: &mut SearchState,
    skip_current: bool,
) -> bool {
    let Some(re) = state.pattern.clone() else {
        return false;
    };
    let cursor = buf.cursor();
    let total = buf.line_count();
    if total == 0 {
        return false;
    }
    // To "skip the current cell", advance `from` one byte past the
    // cursor before asking `find_next` for the at-or-after match.
    // `pos_at_byte` clamps overflow to end-of-buffer so this is
    // safe even when the cursor sits at the trailing edge.
    let from = if skip_current {
        let from_byte = buf.byte_offset(cursor);
        buf.pos_at_byte(from_byte.saturating_add(1))
    } else {
        cursor
    };
    if let Some(range) = buf.find_next(from, &re) {
        // Honour engine wrap policy explicitly. The buffer impl uses
        // its own (deprecated) wrap flag; for new search state the
        // engine SearchState is the source of truth.
        if !state.wrap_around && range.start.line < cursor.line {
            return false;
        }
        Cursor::set_cursor(buf, range.start);
        return true;
    }
    false
}

/// Symmetric counterpart of [`search_forward`].
pub fn search_backward<B: Cursor + Query + Search>(
    buf: &mut B,
    state: &mut SearchState,
    skip_current: bool,
) -> bool {
    let Some(re) = state.pattern.clone() else {
        return false;
    };
    let cursor = buf.cursor();
    let total = buf.line_count();
    if total == 0 {
        return false;
    }
    // Buffer's `Search::find_prev` returns the at-or-before match
    // for the anchor `from`. For `skip_current`, we want the
    // rightmost match whose start is *strictly before* the cursor.
    // Strategy: query find_prev(cursor); if the returned match
    // covers/starts-at the cursor, step the anchor back one byte
    // past that match's start and re-query so the next find_prev
    // skips it. Otherwise the at-or-before match is already strictly
    // before the cursor and we accept it.
    let initial = buf.find_prev(cursor, &re);
    let range = if skip_current {
        match initial {
            Some(m) if m.start == cursor => {
                // Cursor sits exactly on a match start (typical post-
                // commit state). Step past and re-query.
                let cb = buf.byte_offset(m.start);
                if cb == 0 {
                    // No earlier byte — fall through to wrap.
                    None
                } else {
                    let anchor = buf.pos_at_byte(cb.saturating_sub(1));
                    buf.find_prev(anchor, &re)
                }
            }
            other => other,
        }
    } else {
        initial
    };
    if let Some(range) = range {
        if !state.wrap_around && range.start.line > cursor.line {
            return false;
        }
        Cursor::set_cursor(buf, range.start);
        return true;
    }
    false
}

/// Match positions on `row` as `(byte_start, byte_end)`. Used by
/// the engine's highlight pipeline. Reads through the cache so a
/// steady-state buffer doesn't re-scan every frame.
pub fn search_matches<B: Query>(
    buf: &B,
    state: &mut SearchState,
    dirty_gen: u64,
    row: usize,
) -> Vec<(usize, usize)> {
    if state.pattern.is_none() {
        return Vec::new();
    }
    let line_count = buf.line_count() as usize;
    if row >= line_count {
        return Vec::new();
    }
    let line = buf.line(row as u32);
    state.matches_for(row, &line, dirty_gen).to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Pos;
    use hjkl_buffer::Buffer;

    fn re(pat: &str) -> Regex {
        Regex::new(pat).unwrap()
    }

    fn vim_re(pat: &str) -> Regex {
        Regex::new(&vim_to_rust_regex(pat)).unwrap()
    }

    // ── vim_to_rust_regex unit tests ─────────────────────────────────────────

    /// `\<` and `\>` both rewrite to `\b`.
    #[test]
    fn vim_boundary_rewrites_to_b() {
        assert_eq!(vim_to_rust_regex(r"\<foo\>"), r"\bfoo\b");
        assert_eq!(vim_to_rust_regex(r"\<"), r"\b");
        assert_eq!(vim_to_rust_regex(r"\>"), r"\b");
    }

    /// A literal double-backslash before `<`/`>` must not be consumed.
    /// `\\<` in the source string is two chars: `\` `\`; the rewriter sees
    /// the first `\` followed by `\`, emits `\\`, then `<` is plain text.
    #[test]
    fn escaped_backslash_left_alone() {
        // Input: \\< (three chars in source: '\', '\', '<')
        // Expected output: \\< (the first \ escapes the second, < is literal)
        let input = r"\\<";
        let output = vim_to_rust_regex(input);
        assert_eq!(output, r"\\<");
    }

    /// Other escape sequences (`\b`, `\B`, `\d`, `\w`, anchors) pass through.
    #[test]
    fn other_escapes_unchanged() {
        assert_eq!(vim_to_rust_regex(r"\b"), r"\b");
        assert_eq!(vim_to_rust_regex(r"\B"), r"\B");
        assert_eq!(vim_to_rust_regex(r"\d+"), r"\d+");
        assert_eq!(vim_to_rust_regex(r"^\w+$"), r"^\w+$");
    }

    /// Mixed: `\<\w+\>` rewrites to `\b\w+\b` — matches whole words.
    #[test]
    fn mixed_boundary_and_word_class() {
        assert_eq!(vim_to_rust_regex(r"\<\w+\>"), r"\b\w+\b");
    }

    // ── Integration: compiled vim patterns match correctly ───────────────────

    /// `/foo\<bar\>` — `bar` as a standalone word is matched, `foobar` is not.
    #[test]
    fn vim_boundary_matches_standalone_word_not_suffix() {
        let re = vim_re(r"foo\<bar\>");
        // "foobar" — `bar` follows directly after `foo` with no word boundary:
        // the `\b` between `foo` and `bar` fails here.
        assert!(!re.is_match("foobar"));
        // "foo bar" — word boundary between `foo ` and `bar`:
        // pattern `foo\bbar\b` does not match because `foo` is not adjacent.
        // Use a pattern that directly tests the intent: `bar` as a whole word.
        let re2 = vim_re(r"\<bar\>");
        assert!(re2.is_match("foo bar baz"));
        assert!(!re2.is_match("foobar"));
    }

    /// `\<word` matches `word` at start-of-word but not mid-word.
    #[test]
    fn vim_boundary_start_only() {
        let re = vim_re(r"\<word");
        assert!(re.is_match("word here"));
        assert!(re.is_match("some word here"));
        assert!(!re.is_match("sword"));
        assert!(!re.is_match("aword"));
    }

    /// `word\>` matches `word` at end-of-word but not when followed by more.
    #[test]
    fn vim_boundary_end_only() {
        let re = vim_re(r"word\>");
        assert!(re.is_match("some word"));
        assert!(re.is_match("word"));
        assert!(!re.is_match("words"));
        assert!(!re.is_match("wordsmith"));
    }

    /// Existing `\b` continues to work (sanity check — no double-transform).
    #[test]
    fn existing_b_boundary_unchanged() {
        let re = vim_re(r"\bfoo\b");
        assert!(re.is_match("foo"));
        assert!(re.is_match("a foo b"));
        assert!(!re.is_match("foobar"));
        assert!(!re.is_match("afoo"));
    }

    /// Mixed: `\<\w+\>` matches whole words only.
    #[test]
    fn vim_whole_word_pattern() {
        let re = vim_re(r"\<\w+\>");
        let matches: Vec<_> = re.find_iter("foo bar baz").map(|m| m.as_str()).collect();
        assert_eq!(matches, vec!["foo", "bar", "baz"]);
    }

    #[test]
    fn empty_state_no_match() {
        let mut b = Buffer::from_str("anything");
        let mut s = SearchState::new();
        assert!(!search_forward(&mut b, &mut s, false));
        assert!(!search_backward(&mut b, &mut s, false));
    }

    // ── search reveals folds ─────────────────────────────────────────────────

    /// `search_forward` on a buffer with a closed fold hiding the match row:
    /// after finding the match, calling `reveal_row` opens the fold.
    /// (Mirrors what `Editor::search_advance_forward` does.)
    #[test]
    fn search_forward_reveals_fold() {
        use hjkl_buffer::Buffer;

        // Buffer: row 0 = "header", row 1 = "needle", row 2 = "footer"
        // Fold [0..2] closed → row 1 is hidden.
        let mut buf = Buffer::from_str("header\nneedle\nfooter");
        buf.add_fold(0, 2, true);
        assert!(buf.is_row_hidden(1), "row 1 must be hidden before search");

        let mut state = SearchState::new();
        state.set_pattern(Some(re("needle")));

        // Use search_forward directly on the buffer.
        let found = search_forward(&mut buf, &mut state, false);
        assert!(found, "search_forward must find 'needle'");

        // After search_forward, cursor is on row 1. Reveal as Editor does.
        let row = crate::types::Cursor::cursor(&buf).line as usize;
        buf.reveal_row(row);
        assert!(
            !buf.is_row_hidden(1),
            "row 1 must be revealed after search finds it there"
        );
    }

    /// `search_backward` similarly: finding a match then calling reveal_row opens folds.
    #[test]
    fn search_backward_reveals_fold() {
        use hjkl_buffer::Buffer;

        // row 0 = "footer", row 1 = "needle", row 2 = "header"
        // fold [0..2] closed → row 1 hidden. Start cursor at row 2.
        let mut buf = Buffer::from_str("footer\nneedle\nheader");
        buf.add_fold(0, 2, true);
        crate::types::Cursor::set_cursor(&mut buf, crate::types::Pos::new(2, 0));
        assert!(buf.is_row_hidden(1), "row 1 must be hidden before search");

        let mut state = SearchState::new();
        state.set_pattern(Some(re("needle")));

        let found = search_backward(&mut buf, &mut state, false);
        assert!(found, "search_backward must find 'needle'");

        let row = crate::types::Cursor::cursor(&buf).line as usize;
        buf.reveal_row(row);
        assert!(
            !buf.is_row_hidden(1),
            "row 1 must be revealed after backward search finds it"
        );
    }

    #[test]
    fn forward_finds_first_match() {
        let mut b = Buffer::from_str("foo bar foo baz");
        let mut s = SearchState::new();
        s.set_pattern(Some(re("foo")));
        assert!(search_forward(&mut b, &mut s, false));
        assert_eq!(Cursor::cursor(&b), Pos::new(0, 0));
    }

    #[test]
    fn forward_skip_current_walks_past() {
        let mut b = Buffer::from_str("foo bar foo baz");
        let mut s = SearchState::new();
        s.set_pattern(Some(re("foo")));
        search_forward(&mut b, &mut s, false);
        search_forward(&mut b, &mut s, true);
        assert_eq!(Cursor::cursor(&b), Pos::new(0, 8));
    }

    #[test]
    fn forward_wraps_to_top() {
        let mut b = Buffer::from_str("zzz\nfoo");
        // 0.0.37: wrap policy lives entirely on `SearchState::wrap_around`;
        // the buffer-side `set_search_wrap` accessor is gone. Trait
        // `find_next` always wraps; the engine search free function
        // honours `s.wrap_around` directly.
        Cursor::set_cursor(&mut b, Pos::new(1, 2));
        let mut s = SearchState::new();
        s.set_pattern(Some(re("zzz")));
        s.wrap_around = true;
        assert!(search_forward(&mut b, &mut s, true));
        assert_eq!(Cursor::cursor(&b), Pos::new(0, 0));
    }

    #[test]
    fn search_matches_caches_against_dirty_gen() {
        let b = Buffer::from_str("foo bar");
        let mut s = SearchState::new();
        s.set_pattern(Some(re("bar")));
        let dgen = b.dirty_gen();
        let initial = search_matches(&b, &mut s, dgen, 0);
        assert_eq!(initial, vec![(4, 7)]);
    }

    // ── CaseMode::from_options matrix ────────────────────────────────────────

    #[test]
    fn case_mode_from_options_matrix() {
        // ic=false, smart=* → Sensitive
        assert_eq!(CaseMode::from_options(false, false), CaseMode::Sensitive);
        assert_eq!(CaseMode::from_options(false, true), CaseMode::Sensitive);
        // ic=true, smart=false → Insensitive
        assert_eq!(CaseMode::from_options(true, false), CaseMode::Insensitive);
        // ic=true, smart=true → Smart
        assert_eq!(CaseMode::from_options(true, true), CaseMode::Smart);
    }

    // ── resolve_case_mode unit tests ─────────────────────────────────────────

    #[test]
    fn resolve_case_mode_no_override_smart_lowercase() {
        let (stripped, mode) = resolve_case_mode("foo", CaseMode::Smart);
        assert_eq!(stripped, "foo");
        assert_eq!(mode, CaseMode::Insensitive);
    }

    #[test]
    fn resolve_case_mode_no_override_smart_uppercase() {
        let (stripped, mode) = resolve_case_mode("Foo", CaseMode::Smart);
        assert_eq!(stripped, "Foo");
        assert_eq!(mode, CaseMode::Sensitive);
    }

    #[test]
    fn resolve_case_mode_lower_c_override() {
        // \c overrides Sensitive → Insensitive; stripped pattern is "Foo"
        let (stripped, mode) = resolve_case_mode(r"\cFoo", CaseMode::Sensitive);
        assert_eq!(stripped, "Foo");
        assert_eq!(mode, CaseMode::Insensitive);
    }

    #[test]
    fn resolve_case_mode_upper_c_override() {
        // \C overrides Smart → Sensitive; stripped pattern is "foo"
        let (stripped, mode) = resolve_case_mode(r"foo\C", CaseMode::Smart);
        assert_eq!(stripped, "foo");
        assert_eq!(mode, CaseMode::Sensitive);
    }

    #[test]
    fn resolve_case_mode_last_wins() {
        // \c then \C → last-wins → Sensitive; stripped "foo"
        let (stripped, mode) = resolve_case_mode(r"\cfoo\C", CaseMode::Smart);
        assert_eq!(stripped, "foo");
        assert_eq!(mode, CaseMode::Sensitive);
    }

    // ── Integration: search with smartcase / \c / \C ─────────────────────────

    fn build_regex_from(pat: &str, ic: bool, smart: bool) -> Regex {
        let base = CaseMode::from_options(ic, smart);
        let (stripped, mode) = resolve_case_mode(pat, base);
        let src = if mode == CaseMode::Insensitive {
            format!("(?i){stripped}")
        } else {
            stripped
        };
        Regex::new(&src).unwrap()
    }

    #[test]
    fn search_finds_capital_with_smartcase_lowercase_pattern() {
        // ic=true, smart=true, pattern "foo" → Insensitive → matches "FOO"
        let re = build_regex_from("foo", true, true);
        assert!(re.is_match("FOO"), "expected match on 'FOO'");
        assert!(re.is_match("foo"), "expected match on 'foo'");
    }

    #[test]
    fn search_skips_capital_with_smartcase_mixed_pattern() {
        // ic=true, smart=true, pattern "Foo" → Sensitive → does NOT match "FOO"
        let re = build_regex_from("Foo", true, true);
        assert!(!re.is_match("FOO"), "must not match 'FOO' (case-sensitive)");
        assert!(re.is_match("Foo"), "must match exact 'Foo'");
    }

    #[test]
    fn search_lower_c_override_finds_capital() {
        // \cFoo + Sensitive base → Insensitive override → matches "FOO"
        let re = build_regex_from(r"\cFoo", false, false);
        assert!(re.is_match("FOO"), "\\c override must match 'FOO'");
        assert!(re.is_match("foo"), "\\c override must match 'foo'");
    }

    #[test]
    fn vim_to_rust_regex_strips_case_overrides() {
        // vim_to_rust_regex is now a thin wrapper; \c and \C are stripped
        assert_eq!(vim_to_rust_regex(r"\cfoo"), "foo");
        assert_eq!(vim_to_rust_regex(r"foo\C"), "foo");
        assert_eq!(vim_to_rust_regex(r"\<bar\>"), r"\bbar\b");
    }

    /// `*` on word "foo" emits the pattern `\bfoo\b` (all lowercase). Under
    /// smartcase that resolves to Insensitive → should match "FOO". This test
    /// simulates the word_at_cursor_search pattern-build path.
    #[test]
    fn star_search_finds_lowercase_when_smartcase_lower_word() {
        // word_at_cursor_search escapes the word then wraps \b..\b.
        // "foo" is all-lowercase after word-extraction → Smart → Insensitive.
        let pat = r"\bfoo\b";
        let re = build_regex_from(pat, true, true);
        // Case-insensitive → matches "FOO foo Foo".
        let text = "FOO foo Foo";
        let hits: Vec<_> = re.find_iter(text).map(|m| m.as_str()).collect();
        assert!(
            hits.contains(&"FOO"),
            "smartcase lower-word * must match FOO: {hits:?}"
        );
        assert!(
            hits.contains(&"foo"),
            "smartcase lower-word * must match foo: {hits:?}"
        );
    }
}
