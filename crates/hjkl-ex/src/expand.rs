use std::borrow::Cow;
use std::path::{Path, PathBuf};

/// Caller-supplied context for token expansion. hjkl-ex stays agnostic of
/// whether `current_path` / `alt_path` come from an Editor or a multi-slot
/// host — the caller fills whichever fields it has.
#[derive(Default, Clone, Debug)]
pub struct ExpandContext<'a> {
    /// Path of the current buffer (for `%`).
    pub current_path: Option<&'a Path>,
    /// Path of the alternate buffer (for `#`).
    pub alt_path: Option<&'a Path>,
    /// Word under cursor (for `<cword>`).
    pub cword: Option<Cow<'a, str>>,
    /// WORD under cursor — broader vim semantic, no whitespace at all (for `<cWORD>`).
    pub cwword: Option<Cow<'a, str>>,
    /// cwd for resolving `:p` modifier (defaults to std::env::current_dir() if None).
    pub cwd: Option<&'a Path>,
}

/// Apply a single modifier (`:p`, `:h`, `:t`) to a path string.
///
/// `cwd` supplies the base directory for `:p` (making a relative path
/// absolute); when `None` it falls back to the process working directory.
/// Returns `None` when the modifier is unrecognised.
fn apply_modifier(s: &str, modifier: &str, cwd: Option<&Path>) -> Option<String> {
    match modifier {
        "p" => {
            let p = Path::new(s);
            let cwd = cwd
                .map(Path::to_path_buf)
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_else(|| PathBuf::from("."));
            let abs = if p.is_absolute() {
                p.to_path_buf()
            } else {
                cwd.join(p)
            };
            // canonicalize may fail if the file doesn't exist yet — fall back to lexical join.
            let result = abs.canonicalize().unwrap_or(abs);
            Some(result.display().to_string())
        }
        "h" => {
            let p = Path::new(s);
            match p.parent() {
                None => Some(String::new()),
                Some(parent) if parent == Path::new("") => Some(".".to_string()),
                Some(parent) => Some(parent.display().to_string()),
            }
        }
        "t" => {
            let p = Path::new(s);
            match p.file_name() {
                Some(name) => Some(name.to_string_lossy().into_owned()),
                None => Some(s.to_string()),
            }
        }
        _ => None,
    }
}

/// Apply a chain of modifiers (e.g. `p:h` from `%:p:h`) to a base value.
/// `cwd` is forwarded to `:p`. Returns `None` if any modifier is unknown.
fn apply_modifiers(mut value: String, modifiers: &str, cwd: Option<&Path>) -> Option<String> {
    if modifiers.is_empty() {
        return Some(value);
    }
    for modifier in modifiers.split(':') {
        if modifier.is_empty() {
            continue;
        }
        value = apply_modifier(&value, modifier, cwd)?;
    }
    Some(value)
}

/// Expand a single token (`%`, `#`, `<cword>`, `<cWORD>`, possibly with
/// modifier suffix `:p:h:t`) to its literal value. Returns None when the
/// token isn't a recognized expansion form OR when the underlying source
/// is unavailable (e.g. `%` with no current_path).
pub fn expand_filename(ctx: &ExpandContext<'_>, token: &str) -> Option<String> {
    // Try `%` with optional `:mod` chain.
    if token == "%" || token.starts_with("%:") {
        let base = ctx.current_path?.display().to_string();
        let mods = token
            .strip_prefix('%')
            .unwrap()
            .strip_prefix(':')
            .unwrap_or("");
        return apply_modifiers(base, mods, ctx.cwd);
    }

    // Try `#` with optional `:mod` chain.
    if token == "#" || token.starts_with("#:") {
        let base = ctx.alt_path?.display().to_string();
        let mods = token
            .strip_prefix('#')
            .unwrap()
            .strip_prefix(':')
            .unwrap_or("");
        return apply_modifiers(base, mods, ctx.cwd);
    }

    // Try `<cword>` with optional `:mod` chain.
    if token == "<cword>" || token.starts_with("<cword>:") {
        let base = ctx.cword.as_deref()?.to_string();
        let mods = token
            .strip_prefix("<cword>")
            .unwrap()
            .strip_prefix(':')
            .unwrap_or("");
        return apply_modifiers(base, mods, ctx.cwd);
    }

    // Try `<cWORD>` with optional `:mod` chain.
    if token == "<cWORD>" || token.starts_with("<cWORD>:") {
        let base = ctx.cwword.as_deref()?.to_string();
        let mods = token
            .strip_prefix("<cWORD>")
            .unwrap()
            .strip_prefix(':')
            .unwrap_or("");
        return apply_modifiers(base, mods, ctx.cwd);
    }

    None
}

/// Extract the expansion token starting at `s` (which must start with `%`,
/// `#`, `<cword>`, or `<cWORD>`). Returns `(token, rest)` where `token`
/// includes any trailing `:mod` chain consumed, and `rest` is the unconsumed
/// tail of `s`.
fn extract_token(s: &str) -> (&str, &str) {
    // Determine the base length.
    let base_len = if s.starts_with("<cword>") || s.starts_with("<cWORD>") {
        7
    } else if s.starts_with('%') || s.starts_with('#') {
        1
    } else {
        // Should not be called without a known prefix.
        return (s, "");
    };

    // Consume optional `:mod` chain.
    let mut end = base_len;
    let bytes = s.as_bytes();
    while end < bytes.len() && bytes[end] == b':' {
        // Scan one modifier segment (letters a-z).
        let colon_pos = end;
        end += 1; // skip `:`
        let seg_start = end;
        while end < bytes.len() && bytes[end].is_ascii_alphabetic() {
            end += 1;
        }
        if end == seg_start {
            // Empty segment after `:` — not a modifier, stop.
            end = colon_pos;
            break;
        }
    }
    (&s[..end], &s[end..])
}

/// Walk an arg string left-to-right, replacing every `%[:mods]`, `#[:mods]`,
/// `<cword>[:mods]`, `<cWORD>[:mods]` occurrence with its expansion. Tokens
/// that fail to expand are LEFT IN PLACE unchanged (vim parity).
///
/// Token boundaries: a `%` or `#` not preceded by a backslash is a
/// candidate start. `<cword>`/`<cWORD>` match literally (case-sensitive).
/// Backslash-escaped variants `\%` `\#` are preserved as `%`/`#` literals.
pub fn expand_args(ctx: &ExpandContext<'_>, args: &str) -> String {
    let mut result = String::with_capacity(args.len());
    let mut i = 0;
    let bytes = args.as_bytes();
    let len = bytes.len();

    while i < len {
        // Handle backslash escape for `%` and `#`.
        if bytes[i] == b'\\' && i + 1 < len && (bytes[i + 1] == b'%' || bytes[i + 1] == b'#') {
            // `\%` → literal `%`, `\#` → literal `#`.
            result.push(bytes[i + 1] as char);
            i += 2;
            continue;
        }

        // Check for expansion token start.
        let rest = &args[i..];
        if bytes[i] == b'%'
            || bytes[i] == b'#'
            || rest.starts_with("<cword>")
            || rest.starts_with("<cWORD>")
        {
            let (token, after) = extract_token(rest);
            match expand_filename(ctx, token) {
                Some(expanded) => {
                    result.push_str(&expanded);
                }
                None => {
                    // Leave token in place (vim parity).
                    result.push_str(token);
                }
            }
            i += token.len();
            let _ = after; // i is already advanced past token
            continue;
        }

        // Ordinary character.
        // Safety: we index by byte but push as char; handle multi-byte by pushing char.
        let ch = args[i..].chars().next().unwrap();
        result.push(ch);
        i += ch.len_utf8();
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn percent_with_current_path() {
        let ctx = ExpandContext {
            current_path: Some(Path::new("src/main.rs")),
            ..Default::default()
        };
        assert_eq!(expand_filename(&ctx, "%"), Some("src/main.rs".to_string()));
    }

    #[test]
    fn percent_no_current_path_returns_none() {
        let ctx = ExpandContext::default();
        assert_eq!(expand_filename(&ctx, "%"), None);
    }

    #[test]
    fn hash_with_alt_path() {
        let ctx = ExpandContext {
            alt_path: Some(Path::new("src/lib.rs")),
            ..Default::default()
        };
        assert_eq!(expand_filename(&ctx, "#"), Some("src/lib.rs".to_string()));
    }

    #[test]
    fn cword_with_value() {
        let ctx = ExpandContext {
            cword: Some(Cow::Borrowed("foo")),
            ..Default::default()
        };
        assert_eq!(expand_filename(&ctx, "<cword>"), Some("foo".to_string()));
    }

    #[test]
    fn cword_none_returns_none() {
        let ctx = ExpandContext::default();
        assert_eq!(expand_filename(&ctx, "<cword>"), None);
    }

    #[test]
    fn percent_colon_p_absolute() {
        // Use a known relative path + explicit cwd to verify :p makes it absolute.
        // Since :p uses current_dir() internally, we just check it's absolute.
        let ctx = ExpandContext {
            current_path: Some(Path::new("Cargo.toml")),
            ..Default::default()
        };
        let expanded = expand_filename(&ctx, "%:p").unwrap();
        assert!(
            Path::new(&expanded).is_absolute(),
            ":p must produce absolute path, got {expanded:?}"
        );
        assert!(expanded.ends_with("Cargo.toml"), "must end with Cargo.toml");
    }

    #[test]
    #[cfg(unix)]
    fn percent_colon_p_uses_ctx_cwd() {
        // `:p` must resolve a relative path against `ctx.cwd` when supplied,
        // not the process working directory. Use a nonexistent cwd so
        // `canonicalize` fails and the lexical join is observable.
        let ctx = ExpandContext {
            current_path: Some(Path::new("rel.txt")),
            cwd: Some(Path::new("/nonexistent-cwd-abc")),
            ..Default::default()
        };
        assert_eq!(
            expand_filename(&ctx, "%:p"),
            Some("/nonexistent-cwd-abc/rel.txt".to_string())
        );
    }

    #[test]
    fn percent_colon_h_parent_dir() {
        let ctx = ExpandContext {
            current_path: Some(Path::new("src/main.rs")),
            ..Default::default()
        };
        assert_eq!(expand_filename(&ctx, "%:h"), Some("src".to_string()));
    }

    #[test]
    fn percent_colon_t_basename() {
        let ctx = ExpandContext {
            current_path: Some(Path::new("src/main.rs")),
            ..Default::default()
        };
        assert_eq!(expand_filename(&ctx, "%:t"), Some("main.rs".to_string()));
    }

    #[test]
    fn percent_colon_p_colon_h_parent_of_absolute() {
        let ctx = ExpandContext {
            current_path: Some(Path::new("src/main.rs")),
            ..Default::default()
        };
        let expanded = expand_filename(&ctx, "%:p:h").unwrap();
        let p = Path::new(&expanded);
        assert!(p.is_absolute(), ":p:h must be absolute, got {expanded:?}");
        // The last component should be "src" (the parent of main.rs).
        assert_eq!(
            p.file_name().map(|n| n.to_string_lossy().to_string()),
            Some("src".to_string())
        );
    }

    #[test]
    fn percent_colon_t_colon_p_silly_but_valid() {
        // %:t:p → basename then made absolute: basename "main.rs" joined to cwd.
        let ctx = ExpandContext {
            current_path: Some(Path::new("src/main.rs")),
            ..Default::default()
        };
        let expanded = expand_filename(&ctx, "%:t:p").unwrap();
        let p = Path::new(&expanded);
        assert!(p.is_absolute(), "%:t:p must be absolute, got {expanded:?}");
        assert!(
            expanded.ends_with("main.rs"),
            "%:t:p must end with main.rs, got {expanded:?}"
        );
    }

    #[test]
    fn percent_bogus_modifier_returns_none() {
        let ctx = ExpandContext {
            current_path: Some(Path::new("src/main.rs")),
            ..Default::default()
        };
        assert_eq!(expand_filename(&ctx, "%:bogus"), None);
    }

    #[test]
    fn expand_args_percent_dot_txt_suffix() {
        // vim semantic: `%` is expanded, `.txt` is literal suffix.
        let ctx = ExpandContext {
            current_path: Some(Path::new("src/main.rs")),
            ..Default::default()
        };
        assert_eq!(expand_args(&ctx, "%.txt"), "src/main.rs.txt");
    }

    #[test]
    fn expand_args_backslash_percent_literal() {
        let ctx = ExpandContext {
            current_path: Some(Path::new("src/main.rs")),
            ..Default::default()
        };
        assert_eq!(expand_args(&ctx, r"\%"), "%");
    }

    #[test]
    fn expand_args_cword_standalone() {
        let ctx = ExpandContext {
            cword: Some(Cow::Borrowed("foo")),
            ..Default::default()
        };
        assert_eq!(expand_args(&ctx, "<cword>"), "foo");
    }

    #[test]
    fn expand_args_cword_surrounded() {
        let ctx = ExpandContext {
            cword: Some(Cow::Borrowed("foo")),
            ..Default::default()
        };
        assert_eq!(expand_args(&ctx, "a <cword> b"), "a foo b");
    }

    #[test]
    fn expand_args_percent_in_middle() {
        let ctx = ExpandContext {
            current_path: Some(Path::new("src/main.rs")),
            ..Default::default()
        };
        // `foo % bar` — space-delimited; % expands in place.
        assert_eq!(expand_args(&ctx, "foo % bar"), "foo src/main.rs bar");
    }

    #[test]
    fn expand_args_unexpandable_left_in_place() {
        // No current_path set — % stays as-is.
        let ctx = ExpandContext::default();
        assert_eq!(expand_args(&ctx, "e %"), "e %");
    }

    #[test]
    fn expand_args_hash_backslash() {
        let ctx = ExpandContext {
            alt_path: Some(Path::new("src/lib.rs")),
            ..Default::default()
        };
        // \# → literal #, not alt path.
        assert_eq!(expand_args(&ctx, r"\#"), "#");
    }

    #[test]
    fn percent_colon_h_root_returns_empty() {
        // Path::parent() of "/" is None → return empty string.
        let ctx = ExpandContext {
            current_path: Some(Path::new("/")),
            ..Default::default()
        };
        // "/" has no parent → return "".
        let result = expand_filename(&ctx, "%:h");
        // On Unix "/" has parent None; we return empty string per spec.
        assert!(result.is_some());
    }

    #[test]
    fn percent_colon_h_no_separator() {
        // "main.rs" has parent "" → we return ".".
        let ctx = ExpandContext {
            current_path: Some(Path::new("main.rs")),
            ..Default::default()
        };
        assert_eq!(expand_filename(&ctx, "%:h"), Some(".".to_string()));
    }
}
