//! Surgical single-key TOML write-back, powered by `toml_edit`.
//!
//! Some settings (window/dock sizes, anything interactively resized) need to
//! persist to the user's config file so they survive across sessions. A
//! naive `write_default`-style "serialize the whole struct and overwrite the
//! file" would blow away the user's comments and formatting on every
//! resize — the config file is meant to stay human-owned. [`write_key_at`]
//! instead edits exactly one key in place via `toml_edit`'s format-preserving
//! document model: every other byte of the file (comments, blank lines,
//! key order, quoting style) is left untouched.
//!
//! Missing parent tables are created as needed; a missing file is created
//! fresh with just the one key. Never call [`write_default`](crate::write_default)
//! and this function on the same file expecting both to coexist gracefully —
//! `write_default` is a full-overwrite tool for scaffolding, this is a
//! targeted patch tool for runtime persistence.

use std::path::Path;

use crate::error::ConfigError;

/// Set `dotted_path` (e.g. `"explorer.width"`) to `value` in the TOML file at
/// `path`, preserving every other byte of the file.
///
/// - `dotted_path` is split on `.`; all segments but the last are treated as
///   (and created as, if missing) tables. A dotted path with no `.` sets a
///   top-level key.
/// - If `path` does not exist, a fresh document is created containing only
///   the resulting key (plus its parent tables).
/// - If a path segment exists but is not a table (e.g. `explorer` is a
///   string), returns [`ConfigError::Invalid`] rather than clobbering it.
///
/// # Errors
///
/// [`ConfigError::Io`] on read failure (other than "file does not exist"),
/// [`ConfigError::Invalid`] when the existing file isn't valid TOML or a
/// path segment collides with a non-table value, [`ConfigError::Write`] on
/// write failure.
pub fn write_key_at(
    path: &Path,
    dotted_path: &str,
    value: impl Into<toml_edit::Value>,
) -> Result<(), ConfigError> {
    let existing = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(ConfigError::Io {
                path: path.to_path_buf(),
                source: e,
            });
        }
    };
    let mut doc = existing
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| ConfigError::Invalid {
            path: path.to_path_buf(),
            message: format!("existing config is not valid TOML: {e}"),
        })?;

    let segments: Vec<&str> = dotted_path.split('.').collect();
    let (last, parents) = segments
        .split_last()
        .expect("dotted_path must have at least one segment");

    let mut table: &mut toml_edit::Table = doc.as_table_mut();
    for seg in parents {
        let item = table
            .entry(seg)
            .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
        table = item.as_table_mut().ok_or_else(|| ConfigError::Invalid {
            path: path.to_path_buf(),
            message: format!("`{seg}` (in `{dotted_path}`) exists but is not a table"),
        })?;
    }
    table.insert(last, toml_edit::Item::Value(value.into()));

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ConfigError::Write {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }
    std::fs::write(path, doc.to_string()).map_err(|e| ConfigError::Write {
        path: path.to_path_buf(),
        source: e,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_file_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("config.toml");
        write_key_at(&path, "explorer.width", 42i64).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[explorer]"));
        assert!(text.contains("width = 42"));
    }

    #[test]
    fn preserves_unrelated_content_and_comments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "# leader comment\n[editor]\nleader = \" \"\n\n[explorer]\nwidth = 36\n",
        )
        .unwrap();
        write_key_at(&path, "explorer.width", 50i64).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# leader comment"));
        assert!(text.contains("leader = \" \""));
        assert!(text.contains("width = 50"));
        assert!(!text.contains("width = 36"));
    }

    #[test]
    fn creates_missing_parent_table() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[editor]\nleader = \" \"\n").unwrap();
        write_key_at(&path, "panel.height", 12i64).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[panel]"));
        assert!(text.contains("height = 12"));
        assert!(text.contains("[editor]"));
    }

    #[test]
    fn overwrites_existing_key_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[explorer]\nwidth = 20\n").unwrap();
        write_key_at(&path, "explorer.width", 60i64).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let width_lines: Vec<&str> = text.lines().filter(|l| l.contains("width")).collect();
        assert_eq!(width_lines.len(), 1, "must not duplicate the key");
        assert!(width_lines[0].contains("60"));
    }

    #[test]
    fn errors_when_segment_is_not_a_table() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "explorer = \"oops\"\n").unwrap();
        let err = write_key_at(&path, "explorer.width", 40i64).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }));
    }

    #[test]
    fn invalid_existing_toml_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "not [ valid toml").unwrap();
        let err = write_key_at(&path, "explorer.width", 40i64).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }));
    }
}
