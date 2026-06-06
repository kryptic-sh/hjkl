//! Trash-directory helper — safe "delete" for the oil-style explorer.
//!
//! Deleted filesystem entries are moved to `<XDG_CACHE_HOME>/hjkl/trash/`
//! rather than permanently removed, matching the swap-dir pattern from
//! [`crate::swap`].  Only *path resolution* lives here — no file I/O beyond
//! creating the directory and probing for existing entries.

use std::path::{Path, PathBuf};

// ── Directory helper ──────────────────────────────────────────────────────────

/// Return (and auto-create) `<XDG_CACHE_HOME>/hjkl/trash/`.
///
/// Resolution mirrors [`crate::swap::swap_dir`]:
/// - reads `$XDG_CACHE_HOME` via `hjkl_xdg::cache_dir`.
/// - appends `trash/`.
/// - creates the directory if absent (`create_dir_all`).
pub fn trash_dir() -> std::io::Result<PathBuf> {
    let base = hjkl_xdg::cache_dir("hjkl")
        .map_err(|e| std::io::Error::other(format!("xdg cache_dir: {e}")))?;
    let dir = base.join("trash");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

// ── Unique destination path ───────────────────────────────────────────────────

/// Return a unique, non-existing path inside [`trash_dir`] for `original`.
///
/// The returned path has the form `<trash>/<stem>.<ext>.<n>` where `<n>` is
/// the lowest non-negative integer that does not collide with an existing
/// entry in the trash directory.  The counter is determined by scanning
/// existing directory entries — no randomness or time-based values are used
/// so the result is deterministic for a given trash-directory state.
///
/// **Does NOT move anything** — the caller is responsible for the actual
/// filesystem operation.
pub fn trash_path(original: &Path) -> std::io::Result<PathBuf> {
    let dir = trash_dir()?;

    // Build a base name from the original file name (fall back to "unnamed").
    let base_name = original
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unnamed".to_string());

    // Probe with counter 0, 1, 2, … until we find a name that does not exist.
    let mut counter: u64 = 0;
    loop {
        let candidate_name = format!("{base_name}.{counter}");
        let candidate = dir.join(&candidate_name);
        if !candidate.exists() {
            return Ok(candidate);
        }
        counter += 1;
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Override `XDG_CACHE_HOME` to a temp directory for isolation.
    ///
    /// Returns the `TempDir` guard — keep it in scope for the duration of the
    /// test so the directory is not deleted while we use it.
    fn isolated_trash_dir() -> (tempfile::TempDir, PathBuf) {
        let td = tempfile::tempdir().unwrap();
        // Point XDG_CACHE_HOME at the temp dir so trash_dir() resolves inside it.
        // SAFETY: Tests run in a single-threaded nextest process (one test per
        // process) so mutating this env var cannot race with other threads.
        unsafe {
            std::env::set_var("XDG_CACHE_HOME", td.path());
        }
        let expected = td.path().join("hjkl").join("trash");
        (td, expected)
    }

    #[test]
    fn trash_dir_creates_directory() {
        let (_td, expected) = isolated_trash_dir();
        let got = trash_dir().expect("trash_dir must succeed");
        assert_eq!(
            got, expected,
            "trash_dir must match XDG_CACHE_HOME/hjkl/trash"
        );
        assert!(got.is_dir(), "trash_dir must create the directory");
    }

    #[test]
    fn trash_path_is_unique_across_two_calls_for_same_name() {
        let (_td, _expected) = isolated_trash_dir();
        let fake_original = Path::new("/some/project/foo.rs");

        // First call — nothing in trash yet, should get counter 0.
        let first = trash_path(fake_original).expect("first trash_path must succeed");
        assert!(
            first.to_string_lossy().ends_with(".0"),
            "first trash_path must end with .0, got {first:?}"
        );

        // Simulate the file being moved there: create a dummy entry.
        std::fs::write(&first, b"dummy content").expect("creating dummy trash entry must succeed");

        // Second call — counter 0 is taken, must get counter 1.
        let second = trash_path(fake_original).expect("second trash_path must succeed");
        assert!(
            second.to_string_lossy().ends_with(".1"),
            "second trash_path must end with .1, got {second:?}"
        );
        assert_ne!(first, second, "trash_path must return distinct paths");
    }

    #[test]
    fn trash_path_does_not_collide_with_gaps() {
        let (_td, _expected) = isolated_trash_dir();
        let fake_original = Path::new("/home/user/gap_test.txt");

        // Pre-populate counters 0 and 2 (leaving 1 absent).
        let dir = trash_dir().unwrap();
        std::fs::write(dir.join("gap_test.txt.0"), b"x").unwrap();
        std::fs::write(dir.join("gap_test.txt.2"), b"x").unwrap();

        // First free slot is 1 (lexicographic probe, not a sorted scan —
        // the implementation probes 0, 1, 2 in order; 0 is taken, 1 is free).
        let got = trash_path(fake_original).unwrap();
        assert!(
            got.to_string_lossy().ends_with(".1"),
            "expected counter 1 (first gap), got {got:?}"
        );
    }

    #[test]
    fn trash_dir_is_idempotent() {
        let (_td, _expected) = isolated_trash_dir();
        // Calling twice must not error even though the dir already exists.
        let a = trash_dir().unwrap();
        let b = trash_dir().unwrap();
        assert_eq!(a, b);
    }
}
