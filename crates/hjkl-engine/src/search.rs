//! Engine-owned search state + execution helpers.
//!
//! Patch 0.0.35 step 1 of the 33-method classification rollout
//! (see `DESIGN_33_METHOD_CLASSIFICATION.md`). The pattern, per-row
//! match cache, and `wrapscan` flag previously lived on
//! [`hjkl_buffer::View`] (private `SearchState`). Moving the FSM
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

/// Vim's regex "magic" level — controls which characters are special
/// (regex metacharacters) without a backslash prefix. See `:help magic`.
///
/// Ordering (most → least magic): `VeryMagic > Magic > NoMagic > VeryNoMagic`.
/// A character's inherent level determines its behavior: it is special
/// unescaped when the current level is *at or above* its inherent level, and
/// backslash toggles that (forces the opposite treatment).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MagicLevel {
    /// `\v` — nearly every non-alnum/underscore ASCII character is special
    /// unescaped (groups, quantifiers, alternation, anchors, boundaries).
    VeryMagic,
    /// Default / `\m` — vim's normal mode: `. * [ ] ~` are magic unescaped;
    /// groups/quantifiers/alternation/boundaries need a backslash.
    Magic,
    /// `\M` — only `^ $` are magic unescaped; everything else (including
    /// `. * [ ]`) is literal unless backslashed.
    NoMagic,
    /// `\V` — only `\` is special; every other character is literal unless
    /// backslashed (mirrors `Magic`'s "very magic" meta chars).
    VeryNoMagic,
}

/// Characters whose inherent magic level is "very magic" (`( ) + ? | { } = < >`).
fn very_magic_special(ch: char) -> bool {
    matches!(
        ch,
        '(' | ')' | '+' | '?' | '|' | '{' | '}' | '=' | '<' | '>'
    )
}

/// Characters whose inherent magic level is "magic" (`. * [ ] ~`).
fn magic_special(ch: char) -> bool {
    matches!(ch, '.' | '*' | '[' | ']' | '~')
}

/// Characters whose inherent magic level is "nomagic" (`^ $`).
fn nomagic_special(ch: char) -> bool {
    matches!(ch, '^' | '$')
}

/// `true` when `ch` is a rust-`regex` metacharacter that must be
/// backslash-escaped to appear as a literal.
fn regex_meta(ch: char) -> bool {
    matches!(
        ch,
        '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$'
    )
}

/// Whether `ch` is special-without-a-backslash at the given magic `level`.
fn is_special_unescaped(ch: char, level: MagicLevel) -> bool {
    if very_magic_special(ch) {
        level == MagicLevel::VeryMagic
    } else if magic_special(ch) {
        matches!(level, MagicLevel::VeryMagic | MagicLevel::Magic)
    } else if nomagic_special(ch) {
        level != MagicLevel::VeryNoMagic
    } else {
        false
    }
}

/// Emit `ch`'s regex-special meaning into `out`. `chars` is consumed further
/// only for `{` (counted-repeat body) and `[` (character class) is handled by
/// the caller since it needs to flip a "bracket mode" flag.
///
/// `last_sub` is the previous `:s` replacement string, used to expand the
/// magic `~` (`:h /~`, `:h s/~`).
fn emit_special(
    out: &mut String,
    ch: char,
    chars: &mut std::iter::Peekable<std::str::Chars>,
    last_sub: &str,
) {
    match ch {
        '(' => out.push('('),
        ')' => out.push(')'),
        '+' => out.push('+'),
        '?' => out.push('?'),
        '=' => out.push('?'), // vim `\=` / very-magic `=` — same as `\?`.
        '|' => out.push('|'),
        '<' | '>' => out.push_str(r"\b"),
        '{' => {
            out.push('{');
            emit_counted_repeat(out, chars);
        }
        '}' => out.push('}'), // stray close — harmless as a literal.
        '.' => out.push('.'),
        '*' => out.push('*'),
        ']' => out.push_str(r"\]"), // stray close — harmless as a literal.
        // Magic `~` — expands to the previous `:s` replacement text
        // (`:h /~`, `:h s/~`). Inserted verbatim into the translated
        // (rust-regex) output: like vim, the text is dropped in "as pattern"
        // without re-escaping. Ordinary word replacements (`BAR`) round-trip
        // exactly; a replacement carrying regex metacharacters or vim
        // replacement escapes (`\1`, `&`, `\u`…) is a documented
        // sub-limitation and may not compile. Empty `last_sub` (no prior
        // `:s`) → empty expansion (see `translate_pattern`).
        '~' => out.push_str(last_sub),
        '^' => out.push('^'),
        '$' => out.push('$'),
        _ => out.push(ch),
    }
}

/// Copy a `\{n,m}` / `{n,m}` counted-repeat body through to `out`, closing on
/// either a bare `}` (vim's permissive default-magic form, `\{n,m}`) or an
/// escaped `\}`. Assumes the opening `{` has already been pushed to `out`.
fn emit_counted_repeat(out: &mut String, chars: &mut std::iter::Peekable<std::str::Chars>) {
    loop {
        match chars.next() {
            Some('\\') => {
                if chars.peek() == Some(&'}') {
                    chars.next();
                    out.push('}');
                    return;
                } else if let Some(c2) = chars.next() {
                    out.push(c2);
                } else {
                    return;
                }
            }
            Some('}') => {
                out.push('}');
                return;
            }
            Some(c2) => out.push(c2),
            None => return,
        }
    }
}

/// Emit `ch` as a literal character, escaping it if it happens to be a rust
/// `regex` metacharacter.
fn emit_literal(out: &mut String, ch: char) {
    if regex_meta(ch) {
        out.push('\\');
    }
    out.push(ch);
}

/// Translate a raw vim pattern into rust-`regex` syntax and extract any
/// `\c`/`\C` case override. This is the core of [`resolve_case_mode`].
///
/// Handles vim's default-magic transforms (`\( \) \+ \? \= \|` → group /
/// quantifier / alternation syntax; the inverse — unescaped `( ) + ? | { }`
/// become literals), the `\<` / `\>` word-boundary rewrite (already
/// magic-level-independent), `\{n,m}` counted repeats (including vim's
/// permissive unescaped-closing-brace form), and the `\v` / `\V` / `\m` /
/// `\M` magic-level mode switches (mid-pattern, not just at the start).
///
/// `\1`-`\9` backreferences in the PATTERN (not the replacement) are not
/// supported by the rust `regex` crate (no backtracking engine) — they pass
/// through unchanged, which either fails to compile or fails to match,
/// preserving the pre-fix "silent no-match" behavior rather than corrupting
/// text. See `DIVERGE.md`.
///
/// A simple bracket-depth flag skips translation inside `[...]` character
/// classes, mirroring how vim (and rust-regex) treat class contents mostly
/// literally. A `~` inside `[...]` is therefore a literal class member (as in
/// vim), never a last-substitute expansion.
///
/// ### Magic `~` (last-substitute expansion)
///
/// `last_sub` is the previous `:s` replacement string. Under default magic a
/// bare `~` expands to it (`\~` stays a literal tilde); under `\M`/`\V` the
/// roles swap (`\~` expands, bare `~` is literal) — both fall out of the
/// existing symmetric magic-level logic since `~` is a "magic"-inherent char.
/// The expansion is inserted verbatim into the rust-regex output (see
/// [`emit_special`]). When `last_sub` is empty (no `:s` has run yet) the
/// expansion is empty rather than an error — nvim raises `E33` here, but the
/// empty-string choice is safe (never corrupts the buffer) and matches this
/// repo's "silent no-op over hard error" search convention.
fn translate_pattern(pat: &str, last_sub: &str) -> (String, Option<bool>) {
    let mut out = String::with_capacity(pat.len());
    let mut level = MagicLevel::Magic;
    let mut override_mode: Option<bool> = None;
    let mut chars = pat.chars().peekable();
    let mut in_bracket = false;

    while let Some(ch) = chars.next() {
        if in_bracket {
            out.push(ch);
            if ch == ']' {
                in_bracket = false;
            }
            continue;
        }

        if ch == '\\' {
            match chars.next() {
                Some('c') => override_mode = Some(true),  // \c → insensitive
                Some('C') => override_mode = Some(false), // \C → sensitive
                Some('v') => level = MagicLevel::VeryMagic,
                Some('V') => level = MagicLevel::VeryNoMagic,
                Some('m') => level = MagicLevel::Magic,
                Some('M') => level = MagicLevel::NoMagic,
                Some(d @ '0'..='9') => {
                    // Backreference — unsupported by rust-regex. Pass through
                    // unchanged (keeps prior no-match/error behavior).
                    out.push('\\');
                    out.push(d);
                }
                Some(c2) if very_magic_special(c2) || magic_special(c2) || nomagic_special(c2) => {
                    if is_special_unescaped(c2, level) {
                        // Already special unescaped at this level — backslash
                        // forces the literal reading.
                        emit_literal(&mut out, c2);
                    } else if c2 == '[' {
                        out.push('[');
                        in_bracket = true;
                    } else {
                        emit_special(&mut out, c2, &mut chars, last_sub);
                    }
                }
                Some(other) => {
                    // \d \s \w \b \B \a \A \n \t \r \& \~ \\ etc. — already
                    // valid rust-regex syntax (or handled by the caller) and
                    // identical in vim's default magic. Pass through.
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
            continue;
        }

        if is_special_unescaped(ch, level) {
            if ch == '[' {
                out.push('[');
                in_bracket = true;
            } else {
                emit_special(&mut out, ch, &mut chars, last_sub);
            }
        } else {
            emit_literal(&mut out, ch);
        }
    }

    (out, override_mode)
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
/// ### Magic-mode translation
///
/// As of the default-magic regex fix, this function also translates vim's
/// default-magic (and `\v`/`\V`/`\m`/`\M`-switched) regex syntax into
/// rust-`regex` syntax — see [`translate_pattern`] for the full transform
/// list. `vim_to_rust_regex` is a thin wrapper that discards the case mode.
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
///
/// ### Magic `~` expansion
///
/// `last_sub` is the previous `:s` replacement string (pass `""` when there is
/// no substitute context, e.g. `*`/`#` word search). Callers get it from
/// [`crate::editor::Editor::last_substitute_replacement`]. See
/// [`translate_pattern`] for the expansion + escaping rules.
pub fn resolve_case_mode(pat: &str, base: CaseMode, last_sub: &str) -> (String, CaseMode) {
    let (out, override_mode) = translate_pattern(pat, last_sub);

    let resolved = match override_mode {
        Some(true) => CaseMode::Insensitive,
        Some(false) => CaseMode::Sensitive,
        None => match base {
            CaseMode::Smart => {
                // Any uppercase rune → sensitive. Scan the TRANSLATED
                // pattern so control sequences consumed during translation
                // (`\c` `\C` `\v` `\V` `\m` `\M`) don't spuriously count —
                // matches the pre-existing behavior this function had before
                // magic-mode translation was added.
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
///
/// This thin wrapper passes an empty last-substitute string, so a magic `~`
/// expands to the empty string. Use [`resolve_case_mode`] directly with the
/// editor's last-substitute replacement when `~` expansion matters.
pub fn vim_to_rust_regex(pat: &str) -> String {
    resolve_case_mode(pat, CaseMode::Sensitive, "").0
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
    ///
    /// `get_line` is materialized lazily — only invoked on a cache
    /// miss (never scanned, or the row's gen rolled). A steady-state
    /// warm cache returns the cached runs without allocating the line.
    pub fn matches_for(
        &mut self,
        row: usize,
        dirty_gen: u64,
        get_line: impl FnOnce() -> String,
    ) -> &[(usize, usize)] {
        let Some(ref re) = self.pattern else {
            return &[];
        };
        if self.matches.len() <= row {
            self.matches.resize_with(row + 1, Vec::new);
            self.generations.resize(row + 1, u64::MAX);
        }
        if self.generations[row] != dirty_gen {
            // Shared scanner (`hjkl_buffer::search_match_ranges`) — the same
            // byte-range computation the hlsearch painter and the quickfix
            // dock's match overlay use, so navigation and highlighting can
            // never disagree about where a match is.
            self.matches[row] = hjkl_buffer::search_match_ranges(re, &get_line());
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
    // View's `Search::find_prev` returns the at-or-before match
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
    // Materialize the line lazily — only when the cache misses. A warm
    // steady-state cache skips the per-row allocation entirely.
    state
        .matches_for(row, dirty_gen, || buf.line(row as u32))
        .to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Pos;
    use hjkl_buffer::View;

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
        // vim default magic: `+` is a literal unless backslashed. `\d\+`
        // (digit class, one-or-more quantifier) translates to `\d\+` in
        // rust-regex syntax (identical spelling — `\+` IS rust-regex's own
        // escaped-literal-plus, but since the quantifier here is coming from
        // vim's `\+` we want the rust-regex QUANTIFIER `+`, unescaped).
        assert_eq!(vim_to_rust_regex(r"\d\+"), r"\d+");
        assert_eq!(vim_to_rust_regex(r"^\w\+$"), r"^\w+$");
    }

    /// Mixed: `\<\w\+\>` rewrites to `\b\w+\b` — matches whole words.
    #[test]
    fn mixed_boundary_and_word_class() {
        assert_eq!(vim_to_rust_regex(r"\<\w\+\>"), r"\b\w+\b");
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
        let re = vim_re(r"\<\w\+\>");
        let matches: Vec<_> = re.find_iter("foo bar baz").map(|m| m.as_str()).collect();
        assert_eq!(matches, vec!["foo", "bar", "baz"]);
    }

    #[test]
    fn empty_state_no_match() {
        let mut b = View::from_str("anything");
        let mut s = SearchState::new();
        assert!(!search_forward(&mut b, &mut s, false));
        assert!(!search_backward(&mut b, &mut s, false));
    }

    // ── B8/B9: default-magic + \v/\V/\m/\M translation ───────────────────────

    #[test]
    fn default_magic_groups_and_backref_replacement_side() {
        // \( \) → real groups; the PATTERN side is exercised end-to-end via
        // substitute.rs (replacement-side \1 already worked before this fix).
        assert_eq!(
            vim_to_rust_regex(r"\(hello\) \(world\)"),
            r"(hello) (world)"
        );
    }

    #[test]
    fn default_magic_quantifiers_and_alternation() {
        assert_eq!(vim_to_rust_regex(r"a\+"), r"a+");
        assert_eq!(vim_to_rust_regex(r"a\?"), r"a?");
        assert_eq!(vim_to_rust_regex(r"a\="), r"a?");
        assert_eq!(vim_to_rust_regex(r"a\|b"), r"a|b");
    }

    #[test]
    fn default_magic_counted_repeat_bare_close() {
        // vim allows `\{n,m}` with an UNESCAPED closing brace.
        assert_eq!(vim_to_rust_regex(r"a\{1,2}"), r"a{1,2}");
        // Fully-escaped form also works.
        assert_eq!(vim_to_rust_regex(r"a\{1,2\}"), r"a{1,2}");
    }

    #[test]
    fn default_magic_unescaped_group_chars_are_literal() {
        // The INVERSE: unescaped ( ) + ? | { } are literals in default magic.
        assert_eq!(vim_to_rust_regex("(a)"), r"\(a\)");
        assert_eq!(vim_to_rust_regex("a+b"), r"a\+b");
        assert_eq!(vim_to_rust_regex("a|b"), r"a\|b");
        assert_eq!(vim_to_rust_regex("a?b"), r"a\?b");
    }

    #[test]
    fn default_magic_dot_star_bracket_caret_dollar_stay_magic() {
        assert_eq!(vim_to_rust_regex("a.b"), "a.b");
        assert_eq!(vim_to_rust_regex("a*"), "a*");
        assert_eq!(vim_to_rust_regex("[0-9]"), "[0-9]");
        assert_eq!(vim_to_rust_regex("^foo$"), "^foo$");
    }

    #[test]
    fn magic_tilde_expands_to_last_sub_empty_via_wrapper() {
        // `vim_to_rust_regex` passes an empty last-substitute string, so a bare
        // magic `~` expands to "" (nvim would `E33` with no prior `:s`; we pick
        // the safe empty expansion). `\~` stays a literal tilde.
        assert_eq!(vim_to_rust_regex("a~b"), "ab");
        assert_eq!(vim_to_rust_regex(r"a\~b"), "a~b");
    }

    // ── Magic `~` PATTERN-side expansion (V5) ────────────────────────────────

    /// `~` expands to the supplied last-substitute string; `\~` stays literal.
    /// nvim-verified: after `:s/foo/BAR/`, `/~` matches the text `BAR`.
    #[test]
    fn magic_tilde_expands_to_last_sub() {
        let (out, _) = resolve_case_mode("~", CaseMode::Sensitive, "BAR");
        assert_eq!(out, "BAR");
        // Surrounded by other pattern text.
        let (out, _) = resolve_case_mode("x~y", CaseMode::Sensitive, "BAR");
        assert_eq!(out, "xBARy");
    }

    /// `\~` is a literal tilde and must NOT expand, even with a last-sub set.
    /// nvim-verified: `\~` in a pattern matches a real `~` character.
    #[test]
    fn escaped_tilde_stays_literal_and_does_not_expand() {
        let (out, _) = resolve_case_mode(r"\~", CaseMode::Sensitive, "BAR");
        assert_eq!(out, "~");
        // Compiled: matches a real tilde, not "BAR".
        let re = Regex::new(&out).unwrap();
        assert!(re.is_match("a~b"));
        assert!(!re.is_match("BAR"));
    }

    /// `~` inside a `[...]` class is a literal class member, never an
    /// expansion. nvim-verified: `[~]` matches the tilde character.
    #[test]
    fn tilde_in_bracket_class_is_literal() {
        let (out, _) = resolve_case_mode("[~]", CaseMode::Sensitive, "BAR");
        assert_eq!(out, "[~]");
    }

    /// No previous substitute (empty last-sub) → `~` expands to empty.
    /// Documented divergence from nvim's `E33`; the empty choice never
    /// corrupts the buffer.
    #[test]
    fn magic_tilde_no_previous_sub_expands_empty() {
        let (out, _) = resolve_case_mode("a~b", CaseMode::Sensitive, "");
        assert_eq!(out, "ab");
    }

    #[test]
    fn very_magic_mode_switch_at_start() {
        // \v: groups/quantifiers/alternation/boundaries are magic unescaped.
        assert_eq!(vim_to_rust_regex(r"\v(\w+) (\w+)"), r"(\w+) (\w+)");
        assert_eq!(vim_to_rust_regex(r"\v\d+"), r"\d+");
        assert_eq!(vim_to_rust_regex(r"\v<foo>"), r"\bfoo\b");
        assert_eq!(vim_to_rust_regex(r"\va=b"), r"a?b");
    }

    #[test]
    fn very_magic_mode_escaped_chars_are_literal() {
        // In \v mode, backslash forces the LITERAL reading of an
        // otherwise-special char.
        assert_eq!(vim_to_rust_regex(r"\v\(a\)"), r"\(a\)");
        assert_eq!(vim_to_rust_regex(r"\va\+b"), r"a\+b");
    }

    #[test]
    fn very_nomagic_mode_is_all_literal_except_backslash() {
        // \V: everything literal except `\`-escaped.
        assert_eq!(vim_to_rust_regex(r"\Va.b"), r"a\.b");
        assert_eq!(vim_to_rust_regex(r"\V(a)"), r"\(a\)");
        // Backslash still activates special meaning (mirrors \v).
        assert_eq!(vim_to_rust_regex(r"\Va\.b"), r"a.b");
    }

    #[test]
    fn nomagic_mode_only_caret_dollar_special() {
        // \M: only ^ $ special unescaped; `.` `*` `[` become literal.
        assert_eq!(vim_to_rust_regex(r"\M^a.b$"), r"^a\.b$");
        assert_eq!(vim_to_rust_regex(r"\Ma\.b"), r"a.b");
    }

    #[test]
    fn mode_switch_mid_pattern() {
        // Switching mode partway through the pattern applies from that point on.
        assert_eq!(vim_to_rust_regex(r"(a)\v(b)"), r"\(a\)(b)");
        assert_eq!(vim_to_rust_regex(r"\va\mb+"), r"ab\+");
    }

    #[test]
    fn backreference_in_pattern_passes_through_unchanged() {
        // \1-\9 in the PATTERN aren't supported by rust-regex (no
        // backtracking) — kept as a literal backslash-digit escape so the
        // net effect (no match / compile error) matches pre-fix behavior
        // rather than silently corrupting text. See DIVERGE.md.
        assert_eq!(vim_to_rust_regex(r"\(a\)\1"), r"(a)\1");
    }

    #[test]
    fn character_class_contents_not_translated() {
        // Unescaped `(` `)` inside `[...]` are literal class members in both
        // vim and rust-regex — bracket tracking must not turn them into a
        // group by escaping/unescaping their contents.
        assert_eq!(vim_to_rust_regex("[()]"), "[()]");
    }

    // ── search reveals folds ─────────────────────────────────────────────────

    /// `search_forward` on a buffer with a closed fold hiding the match row:
    /// after finding the match, calling `reveal_row` opens the fold.
    /// (Mirrors what `Editor::search_advance_forward` does.)
    #[test]
    fn search_forward_reveals_fold() {
        use hjkl_buffer::View;

        // View: row 0 = "header", row 1 = "needle", row 2 = "footer"
        // Fold [0..2] closed → row 1 is hidden.
        let mut buf = View::from_str("header\nneedle\nfooter");
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
        use hjkl_buffer::View;

        // row 0 = "footer", row 1 = "needle", row 2 = "header"
        // fold [0..2] closed → row 1 hidden. Start cursor at row 2.
        let mut buf = View::from_str("footer\nneedle\nheader");
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
        let mut b = View::from_str("foo bar foo baz");
        let mut s = SearchState::new();
        s.set_pattern(Some(re("foo")));
        assert!(search_forward(&mut b, &mut s, false));
        assert_eq!(Cursor::cursor(&b), Pos::new(0, 0));
    }

    #[test]
    fn forward_skip_current_walks_past() {
        let mut b = View::from_str("foo bar foo baz");
        let mut s = SearchState::new();
        s.set_pattern(Some(re("foo")));
        search_forward(&mut b, &mut s, false);
        search_forward(&mut b, &mut s, true);
        assert_eq!(Cursor::cursor(&b), Pos::new(0, 8));
    }

    #[test]
    fn forward_wraps_to_top() {
        let mut b = View::from_str("zzz\nfoo");
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
        let b = View::from_str("foo bar");
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
        let (stripped, mode) = resolve_case_mode("foo", CaseMode::Smart, "");
        assert_eq!(stripped, "foo");
        assert_eq!(mode, CaseMode::Insensitive);
    }

    #[test]
    fn resolve_case_mode_no_override_smart_uppercase() {
        let (stripped, mode) = resolve_case_mode("Foo", CaseMode::Smart, "");
        assert_eq!(stripped, "Foo");
        assert_eq!(mode, CaseMode::Sensitive);
    }

    #[test]
    fn resolve_case_mode_lower_c_override() {
        // \c overrides Sensitive → Insensitive; stripped pattern is "Foo"
        let (stripped, mode) = resolve_case_mode(r"\cFoo", CaseMode::Sensitive, "");
        assert_eq!(stripped, "Foo");
        assert_eq!(mode, CaseMode::Insensitive);
    }

    #[test]
    fn resolve_case_mode_upper_c_override() {
        // \C overrides Smart → Sensitive; stripped pattern is "foo"
        let (stripped, mode) = resolve_case_mode(r"foo\C", CaseMode::Smart, "");
        assert_eq!(stripped, "foo");
        assert_eq!(mode, CaseMode::Sensitive);
    }

    #[test]
    fn resolve_case_mode_last_wins() {
        // \c then \C → last-wins → Sensitive; stripped "foo"
        let (stripped, mode) = resolve_case_mode(r"\cfoo\C", CaseMode::Smart, "");
        assert_eq!(stripped, "foo");
        assert_eq!(mode, CaseMode::Sensitive);
    }

    // ── Integration: search with smartcase / \c / \C ─────────────────────────

    fn build_regex_from(pat: &str, ic: bool, smart: bool) -> Regex {
        let base = CaseMode::from_options(ic, smart);
        let (stripped, mode) = resolve_case_mode(pat, base, "");
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
