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

/// Rewrite vim-style word-boundary escapes to Rust `regex`-compatible form.
///
/// The `regex` crate supports `\b` (symmetric word boundary) but not the
/// vim/PCRE `\<` (word-boundary start) or `\>` (word-boundary end) variants.
/// This function performs a single-pass rewrite:
///
/// - `\<` → `\b`
/// - `\>` → `\b`
/// - `\\<` / `\\>` (literal double-backslash followed by `<`/`>`) are left
///   untouched — only the unescaped form transforms.
/// - All other syntax (`\b`, `\B`, `\d`, anchors, …) passes through unchanged.
///
/// Call this on the raw user-typed pattern string **before** passing to
/// `regex::Regex::new`. Keep the original string for display / history.
pub fn vim_to_rust_regex(pat: &str) -> String {
    let mut out = String::with_capacity(pat.len());
    let mut chars = pat.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.peek() {
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
    out
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
}
