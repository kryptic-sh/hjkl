//! Built-in predicate and directive implementations.
//!
//! All builtins are parser-agnostic — no editor-specific semantics.
//! Register them via [`register_builtins`] (called by
//! [`PredicateRegistry::with_builtins`]).

use std::collections::HashMap;
use std::sync::Mutex;

use crate::predicate::{
    Directive, MatchContext, MatchMetadata, MetaValue, PredicateArg, PredicateRegistry,
};

// ---------------------------------------------------------------------------
// Lua pattern → Rust regex translator (used by LuaMatchPredicate)
// ---------------------------------------------------------------------------

/// Expand a Lua character-class letter (`a`, `d`, `l`, `u`, `s`, `w`, `p`,
/// `x`, `c`) to its POSIX name for use *inside* a `[...]` regex class.
/// Returns `None` for unknown or uppercase (negated) classes.
fn lua_class_inside_brackets(ch: char) -> Option<&'static str> {
    match ch {
        'a' => Some("[:alpha:]"),
        'd' => Some("[:digit:]"),
        'l' => Some("[:lower:]"),
        'u' => Some("[:upper:]"),
        's' => Some("[:space:]"),
        'w' => Some("[:alnum:]"),
        'p' => Some("[:punct:]"),
        'x' => Some("[:xdigit:]"),
        'c' => Some("[:cntrl:]"),
        _ => None,
    }
}

/// Expand a Lua character-class letter to a standalone regex character class
/// (with surrounding `[...]` brackets). Uppercase variants produce the
/// negated form.
fn lua_class_standalone(ch: char) -> Option<&'static str> {
    match ch {
        'a' => Some("[[:alpha:]]"),
        'A' => Some("[^[:alpha:]]"),
        'd' => Some("[[:digit:]]"),
        'D' => Some("[^[:digit:]]"),
        'l' => Some("[[:lower:]]"),
        'L' => Some("[^[:lower:]]"),
        'u' => Some("[[:upper:]]"),
        'U' => Some("[^[:upper:]]"),
        's' => Some("[[:space:]]"),
        'S' => Some("[^[:space:]]"),
        'w' => Some("[[:alnum:]]"),
        'W' => Some("[^[:alnum:]]"),
        'p' => Some("[[:punct:]]"),
        'P' => Some("[^[:punct:]]"),
        'x' => Some("[[:xdigit:]]"),
        'X' => Some("[^[:xdigit:]]"),
        'c' => Some("[[:cntrl:]]"),
        'C' => Some("[^[:cntrl:]]"),
        _ => None,
    }
}

/// Translate a Lua pattern string to a Rust `regex`-compatible string.
///
/// Supports the subset of Lua patterns used in tree-sitter `highlights.scm`
/// files:
/// - `%a/%d/%l/%u/%s/%w/%p/%x/%c` character classes (and their uppercase negations)
/// - `[...]` character classes with embedded Lua classes (`%a` etc.)
/// - `^`, `$`, `.`, `*`, `+`, `?` — same meaning as regex
/// - `%X` (escaped punctuation) — becomes the literal character
///
/// Unsupported: `%-` (lazy repetition, use `*?` manually), `%b` (balanced
/// match). These produce `Err` so the caller can fall back to permissive.
fn lua_pattern_to_regex(pat: &str) -> Result<String, String> {
    // Regex metacharacters that have no special meaning in Lua patterns and
    // must therefore be escaped when they appear literally.
    const REGEX_ONLY_META: &str = "{}|\\";

    let mut out = String::with_capacity(pat.len() * 2);
    let chars: Vec<char> = pat.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        match chars[i] {
            '%' => {
                i += 1;
                if i >= chars.len() {
                    return Err("trailing % in Lua pattern".into());
                }
                let next = chars[i];
                if next == 'b' {
                    return Err("Lua balanced match (%b) not supported in regex translation".into());
                }
                // Try standalone class expansion first (covers %a..%c + uppercase).
                if let Some(cls) = lua_class_standalone(next) {
                    out.push_str(cls);
                } else {
                    // Literal escape: %( %[ %* etc. → just the char, escaped for regex.
                    if ".+*?[]{}()|\\^$".contains(next) {
                        out.push('\\');
                    }
                    out.push(next);
                }
            }
            '[' => {
                // Parse Lua character class, translating embedded %x classes.
                out.push('[');
                i += 1;
                // Optional negation.
                if i < chars.len() && chars[i] == '^' {
                    out.push('^');
                    i += 1;
                }
                // Body: collect until the closing `]`.
                while i < chars.len() && chars[i] != ']' {
                    if chars[i] == '%' {
                        i += 1;
                        if i >= chars.len() {
                            break;
                        }
                        let cls_ch = chars[i];
                        if let Some(posix) = lua_class_inside_brackets(cls_ch) {
                            out.push_str(posix);
                        } else if let Some(posix) =
                            lua_class_inside_brackets(cls_ch.to_ascii_lowercase())
                        {
                            // Uppercase Lua class inside [...]: e.g. %A — can't negate
                            // a POSIX class inside a set elegantly; expand via standalone.
                            // Just embed the standalone form (with its own brackets stripped).
                            // e.g. %A in [%A] → [^[:alpha:]] — handled by wrapping.
                            // We can't do this cleanly inside [...], so fall back: treat
                            // the uppercase as the lowercase for now (conservative).
                            out.push_str(posix);
                        } else {
                            // Literal escape inside class.
                            if "\\^]-".contains(cls_ch) {
                                out.push('\\');
                            }
                            out.push(cls_ch);
                        }
                    } else {
                        let c = chars[i];
                        // Inside regex character classes, `\` must be escaped.
                        if c == '\\' {
                            out.push_str("\\\\");
                        } else {
                            out.push(c);
                        }
                    }
                    i += 1;
                }
                out.push(']');
            }
            // Lazy quantifier in Lua; not commonly used in TS queries.
            // Map to `*?` (lazy) in regex.  If it appears outside a valid
            // repetition context the regex crate will error and we treat
            // the predicate as unsupported (permissive fallback).
            '-' => out.push_str("*?"),
            // Pass through characters with the same meaning in both Lua and regex.
            '^' | '$' | '.' | '*' | '+' | '?' | '(' | ')' => {
                out.push(chars[i]);
            }
            // Regex-only metacharacters — escape them.
            c if REGEX_ONLY_META.contains(c) => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
        i += 1;
    }

    Ok(out)
}

// ---------------------------------------------------------------------------
// Predicates
// ---------------------------------------------------------------------------

/// `(#contains? @cap "substr" ...)` — true when the text of the first capture
/// contains any one of the given string arguments.
#[derive(Debug)]
pub struct ContainsPredicate;

impl crate::predicate::Predicate for ContainsPredicate {
    fn name(&self) -> &str {
        "contains?"
    }

    fn eval(&self, ctx: &MatchContext<'_>) -> bool {
        // First arg must be a Capture; remaining args are the substrings.
        let Some(PredicateArg::Capture(cap_idx)) = ctx.args.first() else {
            return true; // malformed — don't filter
        };
        let text = match ctx.capture_text(*cap_idx) {
            Some(t) => t,
            None => return false,
        };
        ctx.args[1..].iter().any(|arg| {
            if let PredicateArg::Str(s) = arg {
                text.contains(*s)
            } else {
                false
            }
        })
    }
}

/// `(#has-ancestor? @cap "kind" ...)` — true if any ancestor of the first
/// capture's node has a `kind()` matching one of the string args.
#[derive(Debug)]
pub struct HasAncestorPredicate;

impl crate::predicate::Predicate for HasAncestorPredicate {
    fn name(&self) -> &str {
        "has-ancestor?"
    }

    fn eval(&self, ctx: &MatchContext<'_>) -> bool {
        let Some(PredicateArg::Capture(cap_idx)) = ctx.args.first() else {
            return true;
        };
        let node = match ctx.first_capture(*cap_idx) {
            Some(n) => n,
            None => return false,
        };
        let kinds: Vec<&str> = ctx.args[1..]
            .iter()
            .filter_map(|a| {
                if let PredicateArg::Str(s) = a {
                    Some(*s)
                } else {
                    None
                }
            })
            .collect();
        if kinds.is_empty() {
            return true;
        }
        let mut cur = node.parent();
        while let Some(parent) = cur {
            if kinds.contains(&parent.kind()) {
                return true;
            }
            cur = parent.parent();
        }
        false
    }
}

/// `(#has-parent? @cap "kind" ...)` — true if the direct parent of the first
/// capture's node has a `kind()` matching one of the string args.
#[derive(Debug)]
pub struct HasParentPredicate;

impl crate::predicate::Predicate for HasParentPredicate {
    fn name(&self) -> &str {
        "has-parent?"
    }

    fn eval(&self, ctx: &MatchContext<'_>) -> bool {
        let Some(PredicateArg::Capture(cap_idx)) = ctx.args.first() else {
            return true;
        };
        let node = match ctx.first_capture(*cap_idx) {
            Some(n) => n,
            None => return false,
        };
        let kinds: Vec<&str> = ctx.args[1..]
            .iter()
            .filter_map(|a| {
                if let PredicateArg::Str(s) = a {
                    Some(*s)
                } else {
                    None
                }
            })
            .collect();
        if kinds.is_empty() {
            return true;
        }
        match node.parent() {
            Some(p) => kinds.contains(&p.kind()),
            None => false,
        }
    }
}

/// `(#lua-match? @cap pattern)` — true when the text of the first capture
/// matches the Lua pattern string. The Lua pattern is translated to a Rust
/// regex before matching. On translation failure (unsupported construct like
/// `%b`) the predicate returns `true` (permissive: don't veto the match).
///
/// This predicate is an nvim-treesitter extension used in many highlights.scm
/// files to further filter captures by text content.
#[derive(Debug)]
pub struct LuaMatchPredicate {
    /// Whether to negate the match (for `not-lua-match?`).
    negate: bool,
    /// Compiled-regex cache keyed by the raw Lua pattern string. `None`
    /// records a translation or compilation failure so we don't keep
    /// retrying — `eval` returns `!negate` (permissive) for those.
    ///
    /// Tree-sitter calls this predicate once per match per render frame.
    /// Without this cache, `regex::Regex::new` rebuilt the NFA + DFA on
    /// every call — ~43 % of per-keystroke main-thread CPU in profiling
    /// on a moderately-sized file. Patterns are static (drawn from the
    /// grammar's `highlights.scm`), so the cache is small and bounded.
    cache: Mutex<HashMap<String, Option<regex::Regex>>>,
}

impl LuaMatchPredicate {
    pub fn new(negate: bool) -> Self {
        Self {
            negate,
            cache: Mutex::new(HashMap::new()),
        }
    }
}

impl crate::predicate::Predicate for LuaMatchPredicate {
    fn name(&self) -> &str {
        if self.negate {
            "not-lua-match?"
        } else {
            "lua-match?"
        }
    }

    fn eval(&self, ctx: &MatchContext<'_>) -> bool {
        // Args: @capture, "pattern"
        let (Some(PredicateArg::Capture(cap_idx)), Some(PredicateArg::Str(pattern))) =
            (ctx.args.first(), ctx.args.get(1))
        else {
            return true; // malformed — don't filter
        };
        let text = match ctx.capture_text(*cap_idx) {
            Some(t) => t,
            None => return false,
        };

        // Resolve the regex through the cache. `Regex::clone` is cheap
        // (internal `Arc`); we release the lock before running the
        // potentially-expensive `is_match`.
        let pattern: &str = pattern;
        let re_opt: Option<regex::Regex> = {
            let mut cache = self.cache.lock().unwrap();
            if let Some(opt) = cache.get(pattern) {
                opt.clone()
            } else {
                let built = lua_pattern_to_regex(pattern)
                    .ok()
                    .and_then(|src| regex::Regex::new(&src).ok());
                cache.insert(pattern.to_string(), built.clone());
                built
            }
        };
        let matched = match re_opt {
            Some(re) => re.is_match(text),
            // Translation or compile failure — permissive: don't veto.
            None => return !self.negate,
        };
        if self.negate { !matched } else { matched }
    }
}

// ---------------------------------------------------------------------------
// Directives
// ---------------------------------------------------------------------------

/// `(#set! ...)` directive — handles both forms:
///
/// - Literal: `(#set! "key" "val")` / `(#set! key "val")` → `meta.pattern[key] = Str(val)`.
/// - Capture-target: `(#set! @cap key val)` → `meta.per_capture[cap][key] = Str(val)`.
/// - Val omitted (`(#set! key)`): sets `Bool(true)`.
///
/// The capture-target form is handled via pre-extracted directives in the
/// [`Highlighter`] — this struct handles only the literal forms that survive
/// `property_settings()` parsing.
///
/// [`Highlighter`]: crate::Highlighter
#[derive(Debug)]
pub struct SetDirective;

impl Directive for SetDirective {
    fn name(&self) -> &str {
        "set!"
    }

    fn apply(&self, ctx: &MatchContext<'_>, meta: &mut MatchMetadata) {
        match ctx.args {
            // (#set! @cap key val) or (#set! @cap key)
            [
                PredicateArg::Capture(cap_idx),
                PredicateArg::Str(key),
                rest @ ..,
            ] => {
                let value = match rest.first() {
                    Some(PredicateArg::Str(v)) => MetaValue::Str(v.to_string()),
                    _ => MetaValue::Bool(true),
                };
                meta.capture_mut(*cap_idx).insert(key.to_string(), value);
            }
            // (#set! "key" "val") or (#set! key "val")
            [PredicateArg::Str(key), rest @ ..] => {
                let value = match rest.first() {
                    Some(PredicateArg::Str(v)) => MetaValue::Str(v.to_string()),
                    _ => MetaValue::Bool(true),
                };
                meta.pattern.insert(key.to_string(), value);
            }
            _ => {}
        }
    }
}

/// `(#offset! @cap row_start col_start row_end col_end)` — writes a synthetic
/// range string into `meta.per_capture[cap]["range"]`.
#[derive(Debug)]
pub struct OffsetDirective;

impl Directive for OffsetDirective {
    fn name(&self) -> &str {
        "offset!"
    }

    fn apply(&self, ctx: &MatchContext<'_>, meta: &mut MatchMetadata) {
        // Expected: @cap, row_start, col_start, row_end, col_end (all strings or ints)
        let Some(PredicateArg::Capture(cap_idx)) = ctx.args.first() else {
            return;
        };
        let nums: Vec<&str> = ctx.args[1..]
            .iter()
            .filter_map(|a| {
                if let PredicateArg::Str(s) = a {
                    Some(*s)
                } else {
                    None
                }
            })
            .collect();
        if nums.len() < 4 {
            return;
        }
        let range_str = format!("{},{}-{},{}", nums[0], nums[1], nums[2], nums[3]);
        meta.capture_mut(*cap_idx)
            .insert("range".to_string(), MetaValue::Str(range_str));
    }
}

/// `(#trim! @cap)` — records `meta.per_capture[cap]["trim"] = Bool(true)`.
/// Consumers apply the actual whitespace trimming when emitting output.
#[derive(Debug)]
pub struct TrimDirective;

impl Directive for TrimDirective {
    fn name(&self) -> &str {
        "trim!"
    }

    fn apply(&self, ctx: &MatchContext<'_>, meta: &mut MatchMetadata) {
        let Some(PredicateArg::Capture(cap_idx)) = ctx.args.first() else {
            return;
        };
        meta.capture_mut(*cap_idx)
            .insert("trim".to_string(), MetaValue::Bool(true));
    }
}

/// `(#gsub! @cap "pattern" "replacement")` — records the substitution under
/// `meta.per_capture[cap]["gsub"] = Str("pattern\u{1}replacement")`.
///
/// The separator `\u{1}` (ASCII SOH) is chosen because it cannot appear in
/// valid tree-sitter query string literals. Consumers split on it to recover
/// pattern and replacement.
#[derive(Debug)]
pub struct GsubDirective;

impl Directive for GsubDirective {
    fn name(&self) -> &str {
        "gsub!"
    }

    fn apply(&self, ctx: &MatchContext<'_>, meta: &mut MatchMetadata) {
        let (Some(PredicateArg::Capture(cap_idx)), Some(PredicateArg::Str(pattern)), rest) =
            (ctx.args.first(), ctx.args.get(1), &ctx.args[2..])
        else {
            return;
        };
        let replacement = match rest.first() {
            Some(PredicateArg::Str(r)) => *r,
            _ => "",
        };
        let encoded = format!("{}\u{1}{}", pattern, replacement);
        meta.capture_mut(*cap_idx)
            .insert("gsub".to_string(), MetaValue::Str(encoded));
    }
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register all builtin predicates and directives into `registry`.
pub fn register_builtins(registry: &mut PredicateRegistry) {
    registry.register_predicate(Box::new(ContainsPredicate));
    registry.register_predicate(Box::new(HasAncestorPredicate));
    registry.register_predicate(Box::new(HasParentPredicate));
    registry.register_predicate(Box::new(LuaMatchPredicate::new(false)));
    registry.register_predicate(Box::new(LuaMatchPredicate::new(true)));
    registry.register_directive(Box::new(SetDirective));
    registry.register_directive(Box::new(OffsetDirective));
    registry.register_directive(Box::new(TrimDirective));
    registry.register_directive(Box::new(GsubDirective));
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::predicate::{
        Directive, MatchContext, MatchMetadata, MetaValue, Predicate, PredicateArg,
    };

    // ── helpers ──────────────────────────────────────────────────────────────

    /// Load the html grammar from the bonsai data dir if it exists, so we can
    /// write tests that need real `Node` values. Returns `None` when bonsai
    /// hasn't installed the html grammar yet — tests using this should be
    /// marked `#[ignore]` so they are explicit opt-ins.
    fn load_html_grammar() -> Option<crate::runtime::Grammar> {
        let base = std::env::var_os("XDG_DATA_HOME")
            .filter(|v| !v.is_empty())
            .map(std::path::PathBuf::from)
            .or_else(|| dirs::home_dir().map(|h| h.join(".local/share")))?;
        let so = base.join("bonsai/grammars/html.so");
        if !so.exists() {
            return None;
        }
        crate::runtime::Grammar::load_from_path("html", &so).ok()
    }

    /// Parse `source` with `language` and return the root node + the source
    /// bytes, together with the tree (kept alive for node borrows).
    fn parse(language: &tree_sitter::Language, source: &[u8]) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(language).expect("set language");
        parser.parse(source, None).expect("parse")
    }

    // ── SetDirective — literal pattern form ──────────────────────────────────

    #[test]
    fn set_directive_literal_key_val() {
        let d = SetDirective;
        let args = [PredicateArg::Str("priority"), PredicateArg::Str("99")];
        let ctx = MatchContext {
            pattern_index: 0,
            captures: &[],
            source: b"",
            args: &args,
            capture_names: &[],
        };
        let mut meta = MatchMetadata::default();
        d.apply(&ctx, &mut meta);
        assert_eq!(
            meta.pattern.get("priority"),
            Some(&MetaValue::Str("99".into()))
        );
    }

    #[test]
    fn set_directive_literal_key_only_sets_bool_true() {
        let d = SetDirective;
        let args = [PredicateArg::Str("injection.combined")];
        let ctx = MatchContext {
            pattern_index: 0,
            captures: &[],
            source: b"",
            args: &args,
            capture_names: &[],
        };
        let mut meta = MatchMetadata::default();
        d.apply(&ctx, &mut meta);
        assert_eq!(
            meta.pattern.get("injection.combined"),
            Some(&MetaValue::Bool(true))
        );
    }

    // ── SetDirective — capture target form ───────────────────────────────────

    #[test]
    fn set_directive_capture_target() {
        let d = SetDirective;
        // (#set! @0 "url" "@string.special.url")
        let args = [
            PredicateArg::Capture(0),
            PredicateArg::Str("url"),
            PredicateArg::Str("@string.special.url"),
        ];
        let ctx = MatchContext {
            pattern_index: 0,
            captures: &[],
            source: b"",
            args: &args,
            capture_names: &[],
        };
        let mut meta = MatchMetadata::default();
        d.apply(&ctx, &mut meta);
        assert_eq!(
            meta.per_capture.get(&0).and_then(|m| m.get("url")),
            Some(&MetaValue::Str("@string.special.url".into()))
        );
    }

    // ── OffsetDirective ───────────────────────────────────────────────────────

    #[test]
    fn offset_directive_writes_range() {
        let d = OffsetDirective;
        let args = [
            PredicateArg::Capture(1),
            PredicateArg::Str("0"),
            PredicateArg::Str("1"),
            PredicateArg::Str("0"),
            PredicateArg::Str("-1"),
        ];
        let ctx = MatchContext {
            pattern_index: 0,
            captures: &[],
            source: b"",
            args: &args,
            capture_names: &[],
        };
        let mut meta = MatchMetadata::default();
        d.apply(&ctx, &mut meta);
        assert_eq!(
            meta.per_capture.get(&1).and_then(|m| m.get("range")),
            Some(&MetaValue::Str("0,1-0,-1".into()))
        );
    }

    // ── TrimDirective ─────────────────────────────────────────────────────────

    #[test]
    fn trim_directive_sets_flag() {
        let d = TrimDirective;
        let args = [PredicateArg::Capture(2)];
        let ctx = MatchContext {
            pattern_index: 0,
            captures: &[],
            source: b"",
            args: &args,
            capture_names: &[],
        };
        let mut meta = MatchMetadata::default();
        d.apply(&ctx, &mut meta);
        assert_eq!(
            meta.per_capture.get(&2).and_then(|m| m.get("trim")),
            Some(&MetaValue::Bool(true))
        );
    }

    // ── GsubDirective ─────────────────────────────────────────────────────────

    #[test]
    fn gsub_directive_encodes_pattern_and_replacement() {
        let d = GsubDirective;
        let args = [
            PredicateArg::Capture(0),
            PredicateArg::Str("foo"),
            PredicateArg::Str("bar"),
        ];
        let ctx = MatchContext {
            pattern_index: 0,
            captures: &[],
            source: b"",
            args: &args,
            capture_names: &[],
        };
        let mut meta = MatchMetadata::default();
        d.apply(&ctx, &mut meta);
        let encoded = meta
            .per_capture
            .get(&0)
            .and_then(|m| m.get("gsub"))
            .unwrap();
        if let MetaValue::Str(s) = encoded {
            let parts: Vec<&str> = s.splitn(2, '\u{1}').collect();
            assert_eq!(parts[0], "foo");
            assert_eq!(parts[1], "bar");
        } else {
            panic!("expected Str, got {encoded:?}");
        }
    }

    // ── ContainsPredicate — via real parse ────────────────────────────────────

    #[test]
    #[ignore = "needs cached html grammar — run after hjkl installs html"]
    fn contains_predicate_match() {
        let grammar = match load_html_grammar() {
            Some(g) => g,
            None => {
                eprintln!("html grammar not available; skipping test");
                return;
            }
        };
        let source = b"<a href=\"https://example.com\">link</a>";
        let tree = parse(grammar.language(), source);
        let root = tree.root_node();

        // Find the attribute_value node containing the URL.
        let url_node = find_node_by_kind(&root, "attribute_value");
        let url_node = match url_node {
            Some(n) => n,
            None => {
                eprintln!("could not find attribute_value node; skipping");
                return;
            }
        };

        let captures = vec![(0u32, url_node)];
        let args = [PredicateArg::Capture(0), PredicateArg::Str("example")];
        let ctx = MatchContext {
            pattern_index: 0,
            captures: &captures,
            source,
            args: &args,
            capture_names: &["string.special.url".to_string()],
        };

        let pred = ContainsPredicate;
        assert!(pred.eval(&ctx), "should match: text contains 'example'");
    }

    #[ignore = "needs cached html grammar — run after hjkl installs html"]
    #[test]
    fn contains_predicate_no_match() {
        let grammar = match load_html_grammar() {
            Some(g) => g,
            None => {
                eprintln!("html grammar not available; skipping test");
                return;
            }
        };
        let source = b"<a href=\"https://example.com\">link</a>";
        let tree = parse(grammar.language(), source);
        let root = tree.root_node();

        let url_node = find_node_by_kind(&root, "attribute_value");
        let url_node = match url_node {
            Some(n) => n,
            None => {
                eprintln!("could not find attribute_value node; skipping");
                return;
            }
        };

        let captures = vec![(0u32, url_node)];
        let args = [
            PredicateArg::Capture(0),
            PredicateArg::Str("NO_SUCH_STRING"),
        ];
        let ctx = MatchContext {
            pattern_index: 0,
            captures: &captures,
            source,
            args: &args,
            capture_names: &["string.special.url".to_string()],
        };

        let pred = ContainsPredicate;
        assert!(
            !pred.eval(&ctx),
            "should not match: text does not contain the needle"
        );
    }

    // ── HasAncestorPredicate ──────────────────────────────────────────────────

    #[test]
    #[ignore = "needs cached html grammar — run after hjkl installs html"]
    fn has_ancestor_predicate_true() {
        let grammar = match load_html_grammar() {
            Some(g) => g,
            None => {
                eprintln!("html grammar not available; skipping test");
                return;
            }
        };
        let source = b"<a href=\"https://example.com\">link</a>";
        let tree = parse(grammar.language(), source);
        let root = tree.root_node();

        // attribute_value is nested inside attribute > start_tag > element > document
        let node = find_node_by_kind(&root, "attribute_value");
        let node = match node {
            Some(n) => n,
            None => {
                eprintln!("could not find attribute_value; skipping");
                return;
            }
        };

        let captures = vec![(0u32, node)];
        let args = [PredicateArg::Capture(0), PredicateArg::Str("attribute")];
        let ctx = MatchContext {
            pattern_index: 0,
            captures: &captures,
            source,
            args: &args,
            capture_names: &[],
        };

        let pred = HasAncestorPredicate;
        assert!(
            pred.eval(&ctx),
            "attribute_value should have ancestor 'attribute'"
        );
    }

    #[test]
    #[ignore = "needs cached html grammar — run after hjkl installs html"]
    fn has_ancestor_predicate_false() {
        let grammar = match load_html_grammar() {
            Some(g) => g,
            None => {
                eprintln!("html grammar not available; skipping test");
                return;
            }
        };
        let source = b"<a href=\"https://example.com\">link</a>";
        let tree = parse(grammar.language(), source);
        let root = tree.root_node();

        let node = find_node_by_kind(&root, "attribute_value");
        let node = match node {
            Some(n) => n,
            None => {
                eprintln!("could not find attribute_value; skipping");
                return;
            }
        };

        let captures = vec![(0u32, node)];
        let args = [
            PredicateArg::Capture(0),
            PredicateArg::Str("no_such_kind_xyzzy"),
        ];
        let ctx = MatchContext {
            pattern_index: 0,
            captures: &captures,
            source,
            args: &args,
            capture_names: &[],
        };

        let pred = HasAncestorPredicate;
        assert!(!pred.eval(&ctx), "should not find ancestor of that kind");
    }

    // ── HasParentPredicate ────────────────────────────────────────────────────

    #[test]
    #[ignore = "needs cached html grammar — run after hjkl installs html"]
    fn has_parent_predicate_true() {
        let grammar = match load_html_grammar() {
            Some(g) => g,
            None => {
                eprintln!("html grammar not available; skipping test");
                return;
            }
        };
        let source = b"<a href=\"https://example.com\">link</a>";
        let tree = parse(grammar.language(), source);
        let root = tree.root_node();

        // attribute_value's direct parent should be "attribute"
        let node = find_node_by_kind(&root, "attribute_value");
        let node = match node {
            Some(n) => n,
            None => {
                eprintln!("could not find attribute_value; skipping");
                return;
            }
        };
        let parent_kind = node.parent().map(|p| p.kind()).unwrap_or("");

        let captures = vec![(0u32, node)];
        let args = [PredicateArg::Capture(0), PredicateArg::Str(parent_kind)];
        let ctx = MatchContext {
            pattern_index: 0,
            captures: &captures,
            source,
            args: &args,
            capture_names: &[],
        };

        let pred = HasParentPredicate;
        assert!(pred.eval(&ctx), "should find direct parent '{parent_kind}'");
    }

    // ── lua_pattern_to_regex ──────────────────────────────────────────────────

    fn lua_re(pat: &str) -> String {
        lua_pattern_to_regex(pat).expect("translation should succeed")
    }

    #[test]
    fn lua_pattern_ascii_ranges_pass_through() {
        // Pure ASCII ranges need no translation.
        assert_eq!(lua_re("^[A-Z]"), "^[A-Z]");
        assert_eq!(lua_re("^[a-z0-9_]"), "^[a-z0-9_]");
    }

    #[test]
    fn lua_pattern_class_standalone() {
        assert_eq!(lua_re("%a"), "[[:alpha:]]");
        assert_eq!(lua_re("%d"), "[[:digit:]]");
        assert_eq!(lua_re("%u"), "[[:upper:]]");
        assert_eq!(lua_re("%l"), "[[:lower:]]");
        assert_eq!(lua_re("%s"), "[[:space:]]");
        assert_eq!(lua_re("%w"), "[[:alnum:]]");
    }

    #[test]
    fn lua_pattern_class_inside_brackets() {
        assert_eq!(lua_re("[%a_]"), "[[:alpha:]_]");
        assert_eq!(lua_re("[%a%d_]"), "[[:alpha:][:digit:]_]");
        assert_eq!(lua_re("[^%a]"), "[^[:alpha:]]");
    }

    #[test]
    fn lua_pattern_escaped_punct() {
        assert_eq!(lua_re("%."), "\\.");
        assert_eq!(lua_re("%["), "\\[");
        assert_eq!(lua_re("%%"), "%");
    }

    #[test]
    fn lua_pattern_anchors_and_quantifiers_pass_through() {
        assert_eq!(lua_re("^foo$"), "^foo$");
        assert_eq!(lua_re("foo*"), "foo*");
        assert_eq!(lua_re("foo+"), "foo+");
        assert_eq!(lua_re("foo?"), "foo?");
    }

    // ── LuaMatchPredicate — no-grammar needed ────────────────────────────────

    #[test]
    fn lua_match_predicate_pure_ascii_no_grammar_needed() {
        // We can test via regex compilation + is_match without a real grammar node.
        let pattern = "^[A-Z]";
        let regex_src = lua_pattern_to_regex(pattern).unwrap();
        let re = regex::Regex::new(&regex_src).unwrap();
        assert!(re.is_match("Foo"), "Foo starts with uppercase");
        assert!(!re.is_match("foo"), "foo does not start with uppercase");
    }

    #[test]
    fn lua_match_predicate_lua_alpha_class() {
        let pattern = "^[%a_]";
        let regex_src = lua_pattern_to_regex(pattern).unwrap();
        let re = regex::Regex::new(&regex_src).unwrap();
        assert!(re.is_match("hello"), "letter start");
        assert!(re.is_match("_private"), "underscore start");
        assert!(!re.is_match("123"), "digit start should not match");
    }

    // ── helper ────────────────────────────────────────────────────────────────

    fn find_node_by_kind<'tree>(
        node: &tree_sitter::Node<'tree>,
        kind: &str,
    ) -> Option<tree_sitter::Node<'tree>> {
        if node.kind() == kind {
            return Some(*node);
        }
        let mut c = node.walk();
        for child in node.children(&mut c) {
            if let Some(found) = find_node_by_kind(&child, kind) {
                return Some(found);
            }
        }
        None
    }
}
