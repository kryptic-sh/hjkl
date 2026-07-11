use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    #[error("no home directory available — XDG resolution failed (no $HOME on this platform)")]
    NoHomeDir,

    #[error("io error reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("io error writing {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("parse error in {path} at line {line}, column {col}: {message}\n  | {snippet}")]
    Parse {
        path: PathBuf,
        line: usize,
        col: usize,
        message: String,
        snippet: String,
    },

    /// Schema-level error: TOML parsed, but didn't match the target type
    /// (unknown field, wrong type, missing required field after merge,
    /// invalid bundled defaults). No span info — the structure is post-parse.
    #[error("invalid config in {path}: {message}")]
    Invalid { path: PathBuf, message: String },
}

/// Map a byte offset into `src` to a `(line, col, line_text)` triple.
///
/// `line` and `col` are 1-indexed; `col` counts characters, not bytes, so
/// multibyte text earlier in the line doesn't skew it. `line_text` is the
/// full line containing the offset, with no trailing newline. Used to
/// enrich `toml::de::Error` span info into human-readable parse errors.
pub(crate) fn locate(src: &str, byte_offset: usize) -> (usize, usize, String) {
    let mut clamped = byte_offset.min(src.len());
    // Defensive: snap to the previous char boundary so the column slice
    // below can't panic if a span ever lands mid-character.
    while !src.is_char_boundary(clamped) {
        clamped -= 1;
    }
    let mut line_start = 0usize;
    let mut line_no = 1usize;
    for (i, ch) in src.char_indices() {
        if i >= clamped {
            break;
        }
        if ch == '\n' {
            line_no += 1;
            line_start = i + ch.len_utf8();
        }
    }
    let line_end = src[line_start..]
        .find('\n')
        .map(|n| line_start + n)
        .unwrap_or(src.len());
    let line_text = src[line_start..line_end].to_string();
    let col = src[line_start..clamped].chars().count() + 1;
    (line_no, col, line_text)
}

#[cfg(test)]
mod tests {
    use super::locate;

    #[test]
    fn locate_first_line() {
        let (l, c, txt) = locate("hello\nworld\n", 0);
        assert_eq!((l, c), (1, 1));
        assert_eq!(txt, "hello");
    }

    #[test]
    fn locate_second_line_mid() {
        let (l, c, txt) = locate("hello\nworld\n", 8);
        assert_eq!((l, c), (2, 3));
        assert_eq!(txt, "world");
    }

    #[test]
    fn locate_clamps_past_end() {
        let (l, c, _) = locate("a", 999);
        assert_eq!((l, c), (1, 2));
    }

    #[test]
    fn locate_multibyte_before_offset_counts_chars_not_bytes() {
        // 'é' is 2 bytes but 1 char — the column must be char-based.
        let src = "é = x";
        let x_offset = src.find('x').unwrap();
        let (l, c, txt) = locate(src, x_offset);
        assert_eq!((l, c), (1, 5));
        assert_eq!(txt, "é = x");
    }

    #[test]
    fn locate_mid_char_offset_snaps_to_boundary() {
        // Offset 1 lands inside the 2-byte 'é' — must not panic and must
        // report the column of the char containing the offset.
        let (l, c, txt) = locate("é = 1", 1);
        assert_eq!((l, c), (1, 1));
        assert_eq!(txt, "é = 1");
    }

    #[test]
    fn locate_unicode_offset() {
        let src = "α = 1\nβ = 2";
        let beta_offset = src.find('β').unwrap();
        let (l, c, txt) = locate(src, beta_offset);
        assert_eq!(l, 2);
        assert_eq!(c, 1);
        assert_eq!(txt, "β = 2");
    }
}
