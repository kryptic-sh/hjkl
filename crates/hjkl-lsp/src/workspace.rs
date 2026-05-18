//! Workspace root detection via marker files/directories.

use std::path::{Path, PathBuf};

/// Walk ancestors of `start` looking for any of the given `markers` (files or
/// directories). Returns the first ancestor directory that contains a marker,
/// or `None` if the filesystem root is reached without a match.
///
/// `start` may be a file path; the search begins from its parent directory.
pub fn find_root(start: &Path, markers: &[&str]) -> Option<PathBuf> {
    let dir = if start.is_file() || !start.is_dir() {
        start.parent()?
    } else {
        start
    };

    for ancestor in dir.ancestors() {
        for marker in markers {
            if ancestor.join(marker).exists() {
                return Some(ancestor.to_path_buf());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn finds_marker_file_in_ancestor() {
        let root = tempdir().unwrap();
        let sub = root.path().join("a/b/c");
        fs::create_dir_all(&sub).unwrap();
        // Place a Cargo.toml at the root.
        fs::write(root.path().join("Cargo.toml"), "").unwrap();

        let file = sub.join("foo.rs");
        fs::write(&file, "").unwrap();

        let found = find_root(&file, &["Cargo.toml"]).unwrap();
        assert_eq!(found, root.path());
    }

    #[test]
    fn finds_marker_directory_in_ancestor() {
        let root = tempdir().unwrap();
        let sub = root.path().join("src");
        fs::create_dir_all(&sub).unwrap();
        // Place a .git directory at root.
        fs::create_dir(root.path().join(".git")).unwrap();

        let file = sub.join("main.rs");
        fs::write(&file, "").unwrap();

        let found = find_root(&file, &[".git"]).unwrap();
        assert_eq!(found, root.path());
    }

    #[test]
    fn returns_none_when_no_marker_found() {
        let root = tempdir().unwrap();
        let sub = root.path().join("deep/path");
        fs::create_dir_all(&sub).unwrap();
        let file = sub.join("orphan.rs");
        fs::write(&file, "").unwrap();

        // Use a highly unlikely marker name so we don't accidentally match
        // a real ancestor directory.
        let result = find_root(
            &file,
            &[".hjkl-lsp-test-marker-that-does-not-exist-anywhere"],
        );
        assert!(result.is_none());
    }

    #[test]
    fn finds_nearest_marker_first() {
        let outer = tempdir().unwrap();
        let inner = outer.path().join("inner");
        fs::create_dir_all(&inner).unwrap();
        // Both outer and inner have Cargo.toml.
        fs::write(outer.path().join("Cargo.toml"), "").unwrap();
        fs::write(inner.join("Cargo.toml"), "").unwrap();

        let file = inner.join("lib.rs");
        fs::write(&file, "").unwrap();

        let found = find_root(&file, &["Cargo.toml"]).unwrap();
        // Should find the inner one first.
        assert_eq!(found, inner);
    }

    #[test]
    fn start_is_directory() {
        let root = tempdir().unwrap();
        let sub = root.path().join("src");
        fs::create_dir_all(&sub).unwrap();
        fs::write(root.path().join("Cargo.toml"), "").unwrap();

        // Pass a directory as start.
        let found = find_root(&sub, &["Cargo.toml"]).unwrap();
        assert_eq!(found, root.path());
    }
}
