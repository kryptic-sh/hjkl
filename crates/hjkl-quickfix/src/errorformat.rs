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

use crate::{QfEntry, QfKind};
use std::path::{Path, PathBuf};

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

/// Parse a gcc/clang `path:row:col: level: message` line.
fn parse_gcc_line(line: &str, root: &Path) -> Option<QfEntry> {
    // path : row : col : " level: message"
    let mut parts = line.splitn(4, ':');
    let path = parts.next()?.trim();
    let row: usize = parts.next()?.trim().parse().ok()?;
    let col: usize = parts.next()?.trim().parse().ok()?;
    let tail = parts.next()?.trim();
    if path.is_empty() {
        return None;
    }
    let (kind, message) = match tail.split_once(':') {
        Some((level, msg)) => (kind_from_level(level.trim()), msg.trim().to_string()),
        None => (QfKind::Error, tail.to_string()),
    };
    Some(QfEntry {
        path: resolve(root, path),
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
}
