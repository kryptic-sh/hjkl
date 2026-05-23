//! Comment-prefix table for auto-continuation (`formatoptions=ro`).
//!
//! Maps a language name (e.g. `"rust"`, `"python"`) to the list of line-comment
//! prefixes that should be continued when the user presses `<Enter>` / `o` / `O`
//! on a comment line. Prefixes are ordered **longest-first** so that `///` is
//! matched before `//`.
//!
//! The table is intentionally pure-Rust with no grammar / tree-sitter dependency
//! so it can be imported by `hjkl-engine` in a future refactor, and is already
//! available here for the `gc` toggle (#187).
//!
//! # Example
//!
//! ```
//! use hjkl_lang::comment::comment_prefixes;
//!
//! let prefixes = comment_prefixes("rust");
//! // Each prefix includes a trailing space as the canonical continuation form.
//! assert!(prefixes.contains(&"/// "));
//! assert!(prefixes.contains(&"//! "));
//! assert!(prefixes.contains(&"// "));
//! ```

/// Return the ordered (longest-first) list of line-comment prefixes for
/// `lang`, or an empty slice for unrecognised languages.
///
/// The returned prefixes include one trailing space (e.g. `"// "`) because
/// that is the canonical continuation form — the cursor lands after the space.
pub fn comment_prefixes(lang: &str) -> &'static [&'static str] {
    match lang {
        // Rust: outer doc (`///`), inner doc (`//!`), plain (`//`).
        // Longer first so the matcher catches `///` before `//`.
        "rust" => &["/// ", "//! ", "// "],
        // C / C++
        "c" | "cpp" => &["// "],
        // Python / Shell / TOML / YAML
        "python" | "sh" | "bash" | "zsh" | "fish" | "toml" | "yaml" => &["# "],
        // Lua
        "lua" => &["-- "],
        // SQL
        "sql" => &["-- "],
        // Vimscript / Vim
        "vim" | "viml" => &["\" "],
        _ => &[],
    }
}

/// Detect the comment prefix active on `line`.
///
/// Strips leading whitespace first, then tries each prefix in the order
/// returned by [`comment_prefixes`] (longest-first). Returns
/// `Some((indent, prefix))` where `indent` is the leading whitespace and
/// `prefix` is the matched comment prefix (with trailing space already
/// included). Returns `None` when the line is not a comment.
pub fn detect_comment_prefix<'a>(lang: &str, line: &'a str) -> Option<(&'a str, &'static str)> {
    let indent_end = line
        .char_indices()
        .find(|(_, c)| *c != ' ' && *c != '\t')
        .map(|(i, _)| i)
        .unwrap_or(line.len());
    let indent = &line[..indent_end];
    let rest = &line[indent_end..];
    for &prefix in comment_prefixes(lang) {
        if rest.starts_with(prefix) {
            return Some((indent, prefix));
        }
        // Also match the prefix without trailing space (e.g. a line that is
        // exactly `//` with nothing after it).
        let bare = prefix.trim_end_matches(' ');
        if rest == bare || rest.starts_with(&format!("{bare} ")) {
            return Some((indent, prefix));
        }
    }
    None
}

/// Return the comment-string markers for `lang` as `(start, end)`.
///
/// For line-comment languages `end` is `None` — the toggle inserts `start`
/// (plus one space) before the non-blank text and removes it on uncomment.
///
/// For block-comment languages (HTML, CSS, …) `end` is `Some(end_marker)` —
/// each line is wrapped individually as `start text end`.
///
/// Returns `None` for unrecognised languages. The caller should fall back to
/// an empty prefix (no-op toggle) when `None` is returned.
///
/// The markers here deliberately do NOT include trailing/leading spaces —
/// the toggle algorithm adds/removes exactly one space separator.
///
/// | Language group                           | start  | end     |
/// |------------------------------------------|--------|---------|
/// | Rust, C, C++, JS, TS, Go, Java, Swift    | `//`   | —       |
/// | Python, Shell, TOML, YAML, Make, Ruby, Perl | `#` | —       |
/// | Lua, SQL, Haskell, Ada                   | `--`   | —       |
/// | Vim / Vimscript                          | `"`    | —       |
/// | LaTeX, MATLAB                            | `%`    | —       |
/// | HTML, XML, SVG                           | ``  | `` |
/// | CSS, SCSS                                | `/*`   | `*/`    |
pub fn commentstring_for_lang(lang: &str) -> Option<(&'static str, Option<&'static str>)> {
    match lang {
        // Rust — plain `//` for gc toggle (not the doc-comment variants).
        "rust" => Some(("//", None)),
        // C / C++ / JavaScript / TypeScript / Go / Java / Swift
        "c" | "cpp" | "javascript" | "js" | "typescript" | "ts" | "go" | "java" | "swift" => {
            Some(("//", None))
        }
        // Python / Shell variants / TOML / YAML / Makefile / Ruby / Perl
        "python" | "sh" | "bash" | "zsh" | "fish" | "toml" | "yaml" | "make" | "makefile"
        | "ruby" | "perl" => Some(("#", None)),
        // Lua / SQL / Haskell / Ada
        "lua" | "sql" | "haskell" | "ada" => Some(("--", None)),
        // Vim / Vimscript
        "vim" | "viml" => Some(("\"", None)),
        // LaTeX / MATLAB
        "latex" | "tex" | "matlab" => Some(("%", None)),
        // HTML / XML / SVG — block style, wrap each line individually.
        "html" | "xml" | "svg" => Some(("<!--", Some("-->"))),
        // CSS / SCSS — block style.
        "css" | "scss" => Some(("/*", Some("*/"))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_prefixes_longest_first() {
        let p = comment_prefixes("rust");
        assert_eq!(p[0], "/// ");
        assert_eq!(p[1], "//! ");
        assert_eq!(p[2], "// ");
    }

    #[test]
    fn python_prefix() {
        assert_eq!(comment_prefixes("python"), &["# "]);
    }

    #[test]
    fn lua_prefix() {
        assert_eq!(comment_prefixes("lua"), &["-- "]);
    }

    #[test]
    fn unknown_lang_empty() {
        assert!(comment_prefixes("nolang").is_empty());
    }

    #[test]
    fn detect_rust_doc_comment() {
        let (indent, prefix) = detect_comment_prefix("rust", "/// foo").unwrap();
        assert_eq!(indent, "");
        assert_eq!(prefix, "/// ");
    }

    #[test]
    fn detect_rust_inner_doc_comment() {
        let (indent, prefix) = detect_comment_prefix("rust", "//! crate docs").unwrap();
        assert_eq!(indent, "");
        assert_eq!(prefix, "//! ");
    }

    #[test]
    fn detect_rust_line_comment_with_indent() {
        let (indent, prefix) = detect_comment_prefix("rust", "    // foo").unwrap();
        assert_eq!(indent, "    ");
        assert_eq!(prefix, "// ");
    }

    #[test]
    fn detect_non_comment_returns_none() {
        assert!(detect_comment_prefix("rust", "let x = 1;").is_none());
    }

    #[test]
    fn detect_python_hash() {
        let (indent, prefix) = detect_comment_prefix("python", "# hello").unwrap();
        assert_eq!(indent, "");
        assert_eq!(prefix, "# ");
    }

    #[test]
    fn detect_bare_slash_slash() {
        // A line that is exactly `//` with nothing after it should still match.
        let result = detect_comment_prefix("rust", "//");
        assert!(result.is_some());
    }

    // ── commentstring_for_lang ───────────────────────────────────────────────

    #[test]
    fn commentstring_rust_is_slash_slash() {
        let (start, end) = commentstring_for_lang("rust").unwrap();
        assert_eq!(start, "//");
        assert!(end.is_none());
    }

    #[test]
    fn commentstring_python_is_hash() {
        let (start, end) = commentstring_for_lang("python").unwrap();
        assert_eq!(start, "#");
        assert!(end.is_none());
    }

    #[test]
    fn commentstring_lua_is_dash_dash() {
        let (start, end) = commentstring_for_lang("lua").unwrap();
        assert_eq!(start, "--");
        assert!(end.is_none());
    }

    #[test]
    fn commentstring_html_is_block() {
        let (start, end) = commentstring_for_lang("html").unwrap();
        assert_eq!(start, "<!--");
        assert_eq!(end, Some("-->"));
    }

    #[test]
    fn commentstring_css_is_block() {
        let (start, end) = commentstring_for_lang("css").unwrap();
        assert_eq!(start, "/*");
        assert_eq!(end, Some("*/"));
    }

    #[test]
    fn commentstring_unknown_lang_returns_none() {
        assert!(commentstring_for_lang("brainfuck").is_none());
    }
}
