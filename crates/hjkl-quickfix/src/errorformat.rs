//! Minimal `&errorformat`-style parser for `:make` output (#184 phase 2).
//!
//! Recognizes the two common shapes without a full scanf grammar:
//!
//! - **rustc / cargo** — a diagnostic header line (`error[E0425]: msg`,
//!   `warning: msg`) followed by a location line (`  --> path:row:col`). The
//!   header carries the kind + message; the `-->` line carries the location.
//! - **gcc / clang** — a single self-contained line
//!   `path:row:col: level: message`.
//!
//! Anything else is ignored. Locations are resolved against `root` so the host
//! can open them regardless of the cwd.
//!
//! [`parse_errorformat`] is the Phase 5a `&errorformat`-driven parser for
//! `:cexpr` / `:lgetexpr` etc. It translates a comma-separated list of vim
//! errorformat patterns into regexes and matches them line-by-line.

use crate::{QfEntry, QfKind};
use std::path::{Path, PathBuf};

// ---- errorformat parser (Phase 5a) ─────────────────────────────────────────

/// Which capture-group index maps to which quickfix field.
#[derive(Default)]
struct EfmGroupMap {
    /// 1-based capture group index for `%f` (file path), or 0 if not present.
    file: usize,
    /// 1-based capture group index for `%l` (line number), or 0 if not present.
    line: usize,
    /// 1-based capture group index for `%c` (column), or 0 if not present.
    col: usize,
    /// 1-based capture group index for `%m` (message), or 0 if not present.
    msg: usize,
    /// 1-based capture group index for `%t` (type char), or 0 if not present.
    kind: usize,
}

/// Compile one errorformat pattern into a `(Regex, EfmGroupMap)` pair.
/// Returns `None` if the resulting regex is invalid (silently skip).
fn compile_efm_pattern(efm: &str) -> Option<(regex::Regex, EfmGroupMap)> {
    let mut re_src = String::from("^");
    let mut map = EfmGroupMap::default();
    let mut group = 0usize;
    let mut chars = efm.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '%' {
            match chars.next() {
                Some('f') => {
                    group += 1;
                    map.file = group;
                    re_src.push_str("(.+?)");
                }
                Some('l') => {
                    group += 1;
                    map.line = group;
                    re_src.push_str(r"(\d+)");
                }
                Some('c') => {
                    group += 1;
                    map.col = group;
                    re_src.push_str(r"(\d+)");
                }
                Some('m') => {
                    group += 1;
                    map.msg = group;
                    re_src.push_str("(.*)");
                }
                Some('t') => {
                    group += 1;
                    map.kind = group;
                    re_src.push_str("(.)");
                }
                Some('%') => {
                    re_src.push('%');
                }
                Some(other) => {
                    // Unknown specifier: treat literally.
                    re_src.push_str(&regex::escape(&other.to_string()));
                }
                None => {
                    re_src.push('%');
                }
            }
        } else {
            re_src.push_str(&regex::escape(&ch.to_string()));
        }
    }
    re_src.push('$');
    regex::Regex::new(&re_src).ok().map(|r| (r, map))
}

/// Parse `text` using a vim-style comma-separated `efm` list of errorformat
/// patterns. Non-empty lines are tried against each pattern in order; the
/// first match wins and produces one [`QfEntry`]. Lines that match nothing
/// are silently skipped.
///
/// `root` is used to join relative file paths (same as `parse_make_output`).
pub fn parse_errorformat(text: &str, efm: &str, root: &Path) -> Vec<QfEntry> {
    // Compile all patterns up front.
    let compiled: Vec<_> = efm
        .split(',')
        .filter(|p| !p.is_empty())
        .filter_map(compile_efm_pattern)
        .collect();

    let mut out = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        for (re, map) in &compiled {
            if let Some(caps) = re.captures(line) {
                let cap = |idx: usize| -> &str {
                    if idx == 0 {
                        ""
                    } else {
                        caps.get(idx).map(|m| m.as_str()).unwrap_or("")
                    }
                };
                let path_str = cap(map.file);
                let path = if path_str.is_empty() {
                    PathBuf::new()
                } else {
                    let p = Path::new(path_str);
                    if p.is_absolute() {
                        p.to_path_buf()
                    } else {
                        root.join(p)
                    }
                };
                let row = cap(map.line)
                    .parse::<usize>()
                    .unwrap_or(0)
                    .saturating_sub(1);
                let col = cap(map.col).parse::<usize>().unwrap_or(0).saturating_sub(1);
                let message = if map.msg != 0 {
                    cap(map.msg).to_string()
                } else {
                    line.to_string()
                };
                let kind = if map.kind != 0 {
                    match cap(map.kind) {
                        "e" | "E" => QfKind::Error,
                        "w" | "W" => QfKind::Warning,
                        "i" | "I" => QfKind::Info,
                        "n" | "N" => QfKind::Note,
                        _ => QfKind::Error,
                    }
                } else {
                    QfKind::Error
                };
                out.push(QfEntry {
                    path,
                    row,
                    col,
                    kind,
                    message,
                });
                break; // first-match-wins
            }
        }
    }
    out
}

/// Parse build output into quickfix entries. `root` is the directory the build
/// ran in (relative paths are joined onto it).
pub fn parse_make_output(text: &str, root: &Path) -> Vec<QfEntry> {
    let mut out = Vec::new();
    // The most recent rustc/cargo diagnostic header awaiting its `-->` line.
    let mut pending: Option<(QfKind, String)> = None;

    for line in text.lines() {
        let trimmed = line.trim_start();

        // rustc/cargo header: "error[E0425]: msg" / "warning: msg" / "note: …".
        if let Some((kind, msg)) = parse_diag_header(trimmed) {
            pending = Some((kind, msg));
            continue;
        }

        // rustc/cargo location: "--> path:row:col".
        if let Some(rest) = trimmed.strip_prefix("-->") {
            if let Some((path, row, col)) = parse_location(rest.trim()) {
                let (kind, message) = pending.take().unwrap_or((QfKind::Error, String::new()));
                out.push(QfEntry {
                    path: resolve(root, &path),
                    row,
                    col,
                    kind,
                    message,
                });
            }
            continue;
        }

        // gcc/clang: "path:row:col: level: message".
        if let Some(entry) = parse_gcc_line(line, root) {
            out.push(entry);
        }
    }
    out
}

/// Recognize a rustc/cargo diagnostic header. Returns `(kind, message)`.
fn parse_diag_header(s: &str) -> Option<(QfKind, String)> {
    for (prefix, kind) in [
        ("error", QfKind::Error),
        ("warning", QfKind::Warning),
        ("note", QfKind::Note),
        ("help", QfKind::Info),
    ] {
        if let Some(rest) = s.strip_prefix(prefix) {
            // Optional `[E0425]` error code, then a mandatory `: `.
            let rest = rest.strip_prefix(|c| c == '[').map_or(rest, |after| {
                // Skip up to and including the closing `]`.
                after.find(']').map_or(rest, |i| &after[i + 1..])
            });
            if let Some(msg) = rest.strip_prefix(':') {
                return Some((kind, msg.trim().to_string()));
            }
        }
    }
    None
}

/// Parse a trailing `path:row:col` (rustc `-->` payload).
fn parse_location(s: &str) -> Option<(String, usize, usize)> {
    // Split from the right so Windows drive letters / colons in paths survive.
    let mut parts = s.rsplitn(3, ':');
    let col: usize = parts.next()?.trim().parse().ok()?;
    let row: usize = parts.next()?.trim().parse().ok()?;
    let path = parts.next()?.trim().to_string();
    if path.is_empty() {
        return None;
    }
    Some((path, row.saturating_sub(1), col.saturating_sub(1)))
}

/// If `s` starts with a Windows drive prefix (`C:\` / `C:/`), return
/// `("C:", rest)` so the drive's colon isn't mistaken for a field separator;
/// otherwise `("", s)`.
fn split_drive_prefix(s: &str) -> (&str, &str) {
    let b = s.as_bytes();
    if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
    {
        (&s[..2], &s[2..])
    } else {
        ("", s)
    }
}

/// Parse a gcc/clang `path:row:col: level: message` line.
fn parse_gcc_line(line: &str, root: &Path) -> Option<QfEntry> {
    // A leading Windows drive letter (`C:\…`) has a colon that must not be
    // treated as the path/row separator; peel it off and re-attach.
    let (drive, rest) = split_drive_prefix(line);
    // path : row : col : " level: message"
    let mut parts = rest.splitn(4, ':');
    let path_rest = parts.next()?.trim();
    let row: usize = parts.next()?.trim().parse().ok()?;
    let col: usize = parts.next()?.trim().parse().ok()?;
    let tail = parts.next()?.trim();
    let path = format!("{drive}{path_rest}");
    if path.is_empty() {
        return None;
    }
    let (kind, message) = match tail.split_once(':') {
        Some((level, msg)) => (kind_from_level(level.trim()), msg.trim().to_string()),
        None => (QfKind::Error, tail.to_string()),
    };
    Some(QfEntry {
        path: resolve(root, &path),
        row: row.saturating_sub(1),
        col: col.saturating_sub(1),
        kind,
        message,
    })
}

fn kind_from_level(level: &str) -> QfKind {
    match level {
        "error" | "fatal error" => QfKind::Error,
        "warning" => QfKind::Warning,
        "note" => QfKind::Note,
        _ => QfKind::Info,
    }
}

fn resolve(root: &Path, path: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.join(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gcc_windows_drive_letter_path_parses() {
        let root = std::path::Path::new(".");
        let e = parse_gcc_line(r"C:\src\main.rs:10:5: error: boom", root)
            .expect("drive-letter line must parse, not be dropped");
        assert_eq!(e.row, 9);
        assert_eq!(e.col, 4);
        assert!(
            e.path.to_string_lossy().contains("main.rs"),
            "drive-letter path must survive, got {:?}",
            e.path
        );
        assert_eq!(e.message, "boom");
    }

    #[test]
    fn parses_rustc_cargo() {
        let out = "\
error[E0425]: cannot find value `x` in this scope
  --> src/main.rs:3:13
   |
3  |     let y = x;
   |             ^ not found
warning: unused variable: `y`
  --> src/main.rs:3:9
";
        let root = Path::new("/proj");
        let e = parse_make_output(out, root);
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].path, Path::new("/proj/src/main.rs"));
        assert_eq!((e[0].row, e[0].col), (2, 12)); // 0-based
        assert_eq!(e[0].kind, QfKind::Error);
        assert_eq!(e[0].message, "cannot find value `x` in this scope");
        assert_eq!(e[1].kind, QfKind::Warning);
        assert_eq!((e[1].row, e[1].col), (2, 8));
    }

    #[test]
    fn parses_gcc_clang() {
        let out = "src/main.c:10:5: error: 'x' undeclared\n\
                   src/main.c:12:1: warning: control reaches end\n";
        let e = parse_make_output(out, Path::new("/c"));
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].path, Path::new("/c/src/main.c"));
        assert_eq!((e[0].row, e[0].col), (9, 4));
        assert_eq!(e[0].kind, QfKind::Error);
        assert_eq!(e[0].message, "'x' undeclared");
        assert_eq!(e[1].kind, QfKind::Warning);
    }

    #[test]
    fn ignores_noise_and_absolute_paths() {
        let out = "   Compiling foo v0.1.0\n\
                   error: aborting due to previous error\n\
                   --> /abs/path/lib.rs:1:1\n";
        let e = parse_make_output(out, Path::new("/proj"));
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].path, Path::new("/abs/path/lib.rs"));
        assert_eq!(e[0].message, "aborting due to previous error");
    }

    #[test]
    fn empty_output_no_entries() {
        assert!(parse_make_output("", Path::new("/")).is_empty());
    }

    // ---- parse_errorformat tests -----------------------------------------------

    #[test]
    fn efm_line_col_msg_no_file() {
        // %l:%c:%m — no %f → empty path, row/col 0-based
        let e = parse_errorformat("3:2:hello world", "%l:%c:%m", Path::new("/proj"));
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].path, PathBuf::new());
        assert_eq!((e[0].row, e[0].col), (2, 1)); // 3-1=2, 2-1=1
        assert_eq!(e[0].message, "hello world");
        assert_eq!(e[0].kind, QfKind::Error); // default
    }

    #[test]
    fn efm_file_line_col_msg() {
        // %f:%l:%c:%m
        let e = parse_errorformat(
            "src/main.rs:10:5:something failed",
            "%f:%l:%c:%m",
            Path::new("/proj"),
        );
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].path, Path::new("/proj/src/main.rs"));
        assert_eq!((e[0].row, e[0].col), (9, 4));
        assert_eq!(e[0].message, "something failed");
    }

    #[test]
    fn efm_file_line_msg_no_col() {
        // %f:%l:%m — no col → col 0
        let e = parse_errorformat("foo.py:7:oops", "%f:%l:%m", Path::new("/root"));
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].path, Path::new("/root/foo.py"));
        assert_eq!((e[0].row, e[0].col), (6, 0));
        assert_eq!(e[0].message, "oops");
    }

    #[test]
    fn efm_multi_pattern_alternative() {
        // %f:%l:%c:%m,%f:%l:%m — first pattern has col, second doesn't
        let efm = "%f:%l:%c:%m,%f:%l:%m";
        let root = Path::new("/");
        // Line with col matches first pattern
        let e1 = parse_errorformat("a.rs:1:3:err", efm, root);
        assert_eq!(e1.len(), 1);
        assert_eq!((e1[0].row, e1[0].col), (0, 2));

        // Line without col falls through to second pattern
        let e2 = parse_errorformat("b.rs:2:msg only", efm, root);
        assert_eq!(e2.len(), 1);
        assert_eq!((e2[0].row, e2[0].col), (1, 0));
        assert_eq!(e2[0].message, "msg only");
    }

    #[test]
    fn efm_unmatched_lines_skipped() {
        let e = parse_errorformat(
            "this does not match\nnor does this",
            "%f:%l:%c:%m",
            Path::new("/"),
        );
        assert!(e.is_empty(), "non-matching lines should be skipped");
    }

    #[test]
    fn efm_empty_text_no_entries() {
        let e = parse_errorformat("", "%f:%l:%c:%m", Path::new("/"));
        assert!(e.is_empty());
    }

    #[test]
    fn efm_absolute_path_kept_as_is() {
        let e = parse_errorformat("/abs/path.rs:1:1:abs", "%f:%l:%c:%m", Path::new("/proj"));
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].path, Path::new("/abs/path.rs"));
    }

    #[test]
    fn efm_type_field_maps_kind() {
        // %t maps e→Error, w→Warning, i→Info, n→Note
        let efm = "%f:%l:%t:%m";
        let root = Path::new("/");
        for (t, expected) in [
            ("e", QfKind::Error),
            ("w", QfKind::Warning),
            ("i", QfKind::Info),
            ("n", QfKind::Note),
        ] {
            let line = format!("x.rs:1:{t}:msg");
            let e = parse_errorformat(&line, efm, root);
            assert_eq!(e.len(), 1, "kind {t}");
            assert_eq!(e[0].kind, expected, "kind {t}");
        }
    }
}
