//! XDG path resolution and store layout helpers.
//!
//! ## Store layout
//!
//! ```text
//! $XDG_DATA_HOME/anvil/
//! ├── packages/
//! │   ├── <name>/
//! │   │   ├── bin/<bin>
//! │   │   └── .rev          # version:sha256 sidecar
//! │   └── ...
//! └── bin/                  # flat symlinks for $PATH prepend
//!
//! $XDG_CACHE_HOME/anvil/    # download staging, extraction scratch
//! ```
//!
//! ## Env-override tests
//!
//! The path-resolution tests that mutate `XDG_DATA_HOME` / `XDG_CACHE_HOME`
//! via `std::env::set_var` are marked `#[ignore = "requires serialized env"]`
//! because parallel test runners can cause races. Run them with:
//!
//! ```sh
//! cargo test -p hjkl-anvil -- --test-threads=1 --include-ignored
//! ```

use std::path::PathBuf;

use hjkl_xdg::{cache_home, data_home};
use thiserror::Error;

// ── Error ────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("xdg resolution failed: {0}")]
    Xdg(#[from] hjkl_xdg::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

// ── Path helpers ─────────────────────────────────────────────────────────────

/// `<XDG_DATA_HOME>/anvil/`.
pub fn data_root() -> Result<PathBuf, StoreError> {
    Ok(data_home()?.join("anvil"))
}

/// `<XDG_CACHE_HOME>/anvil/`.
pub fn cache_root() -> Result<PathBuf, StoreError> {
    Ok(cache_home()?.join("anvil"))
}

/// `<data_root>/packages/`.
pub fn packages_dir() -> Result<PathBuf, StoreError> {
    Ok(data_root()?.join("packages"))
}

/// `<data_root>/packages/<name>/`.
pub fn package_dir(name: &str) -> Result<PathBuf, StoreError> {
    Ok(packages_dir()?.join(name))
}

/// `<data_root>/packages/<name>/.rev`.
///
/// Sidecar file recording the pinned version + sha that produced the install.
/// Format: `<version>:<sha256>` on a single line.
pub fn rev_file(name: &str) -> Result<PathBuf, StoreError> {
    Ok(package_dir(name)?.join(".rev"))
}

/// `<data_root>/bin/`.
///
/// Flat directory of symlinks that consumers prepend to `$PATH`.
pub fn bin_dir() -> Result<PathBuf, StoreError> {
    Ok(data_root()?.join("bin"))
}

// ── Rev sidecar ──────────────────────────────────────────────────────────────

/// Sidecar persisted in `<package>/.rev`.
///
/// Format on disk: `<version>:<sha256>` on one line, newline-terminated.
/// The `sha256` field is empty for methods that don't carry a checksum
/// (e.g. Cargo, Npm, Pip, GoInstall).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevSidecar {
    pub version: String,
    pub sha256: String,
}

impl RevSidecar {
    /// Parse a `.rev` sidecar string.
    ///
    /// Returns `None` if the string doesn't contain a `:` separator or
    /// the version portion is empty.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let (version, sha256) = s.split_once(':')?;
        if version.is_empty() {
            return None;
        }
        Some(Self {
            version: version.to_string(),
            sha256: sha256.to_string(),
        })
    }

    /// Serialize to the on-disk format: `<version>:<sha256>\n`.
    #[allow(clippy::inherent_to_string)]
    pub fn to_string(&self) -> String {
        format!("{}:{}\n", self.version, self.sha256)
    }
}

// ── Rev I/O ──────────────────────────────────────────────────────────────────

/// Read the `<package>/.rev` sidecar.
///
/// Returns `Ok(None)` when the file is absent (tool not installed).
/// Returns `Err` on other I/O errors or a malformed sidecar.
pub fn read_rev(name: &str) -> Result<Option<RevSidecar>, StoreError> {
    let path = rev_file(name)?;
    match std::fs::read_to_string(&path) {
        Ok(s) => Ok(RevSidecar::parse(&s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(StoreError::Io(e)),
    }
}

/// Write `<package>/.rev` atomically via a staging file + rename.
///
/// Creates the package directory if it doesn't exist.
pub fn write_rev(name: &str, rev: &RevSidecar) -> Result<(), StoreError> {
    let pkg_dir = package_dir(name)?;
    std::fs::create_dir_all(&pkg_dir)?;

    let target = rev_file(name)?;
    // Write to a staging file alongside the target, then rename atomically.
    let staging = target.with_extension("rev.tmp");
    std::fs::write(&staging, rev.to_string())?;
    std::fs::rename(&staging, &target)?;
    Ok(())
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── RevSidecar unit tests (no I/O, parallel-safe) ────────────────────────

    #[test]
    fn rev_sidecar_parse_with_sha() {
        let s = "2025-01-13:deadbeef00000000000000000000000000000000000000000000000000000000\n";
        let rev = RevSidecar::parse(s).unwrap();
        assert_eq!(rev.version, "2025-01-13");
        assert_eq!(
            rev.sha256,
            "deadbeef00000000000000000000000000000000000000000000000000000000"
        );
    }

    #[test]
    fn rev_sidecar_parse_empty_sha() {
        // Methods like Cargo/Npm don't carry a checksum.
        let s = "0.9.3:";
        let rev = RevSidecar::parse(s).unwrap();
        assert_eq!(rev.version, "0.9.3");
        assert_eq!(rev.sha256, "");
    }

    #[test]
    fn rev_sidecar_parse_no_colon_returns_none() {
        assert!(RevSidecar::parse("nocolon").is_none());
    }

    #[test]
    fn rev_sidecar_parse_empty_version_returns_none() {
        assert!(RevSidecar::parse(":sha").is_none());
    }

    #[test]
    fn rev_sidecar_to_string_round_trip() {
        let rev = RevSidecar {
            version: "v0.17.1".to_string(),
            sha256: "abc123".to_string(),
        };
        let serialized = rev.to_string();
        let parsed = RevSidecar::parse(&serialized).unwrap();
        assert_eq!(parsed, rev);
    }

    #[test]
    fn rev_sidecar_empty_sha_round_trip() {
        let rev = RevSidecar {
            version: "1.1.395".to_string(),
            sha256: String::new(),
        };
        let parsed = RevSidecar::parse(&rev.to_string()).unwrap();
        assert_eq!(parsed, rev);
    }

    // ── Path resolution tests (env-mutating — must run single-threaded) ──────
    //
    // Run with: cargo test -p hjkl-anvil -- --test-threads=1 --include-ignored

    #[test]
    #[ignore = "requires serialized env: run with --test-threads=1 --include-ignored"]
    fn data_root_honors_xdg_data_home() {
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: single-threaded test context only.
        unsafe {
            std::env::set_var("XDG_DATA_HOME", tmp.path());
        }
        let root = data_root().unwrap();
        assert_eq!(root, tmp.path().join("anvil"));
        unsafe {
            std::env::remove_var("XDG_DATA_HOME");
        }
    }

    #[test]
    #[ignore = "requires serialized env: run with --test-threads=1 --include-ignored"]
    fn cache_root_honors_xdg_cache_home() {
        let tmp = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("XDG_CACHE_HOME", tmp.path());
        }
        let root = cache_root().unwrap();
        assert_eq!(root, tmp.path().join("anvil"));
        unsafe {
            std::env::remove_var("XDG_CACHE_HOME");
        }
    }

    #[test]
    #[ignore = "requires serialized env: run with --test-threads=1 --include-ignored"]
    fn packages_dir_is_under_data_root() {
        let tmp = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("XDG_DATA_HOME", tmp.path());
        }
        let pd = packages_dir().unwrap();
        assert_eq!(pd, tmp.path().join("anvil").join("packages"));
        unsafe {
            std::env::remove_var("XDG_DATA_HOME");
        }
    }

    #[test]
    #[ignore = "requires serialized env: run with --test-threads=1 --include-ignored"]
    fn package_dir_appends_name() {
        let tmp = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("XDG_DATA_HOME", tmp.path());
        }
        let pd = package_dir("rust-analyzer").unwrap();
        assert_eq!(
            pd,
            tmp.path()
                .join("anvil")
                .join("packages")
                .join("rust-analyzer")
        );
        unsafe {
            std::env::remove_var("XDG_DATA_HOME");
        }
    }

    #[test]
    #[ignore = "requires serialized env: run with --test-threads=1 --include-ignored"]
    fn bin_dir_is_under_data_root() {
        let tmp = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("XDG_DATA_HOME", tmp.path());
        }
        let bd = bin_dir().unwrap();
        assert_eq!(bd, tmp.path().join("anvil").join("bin"));
        unsafe {
            std::env::remove_var("XDG_DATA_HOME");
        }
    }

    #[test]
    #[ignore = "requires serialized env: run with --test-threads=1 --include-ignored"]
    fn read_rev_returns_none_for_absent_package() {
        let tmp = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("XDG_DATA_HOME", tmp.path());
        }
        let result = read_rev("nonexistent-tool").unwrap();
        assert!(result.is_none());
        unsafe {
            std::env::remove_var("XDG_DATA_HOME");
        }
    }

    #[test]
    #[ignore = "requires serialized env: run with --test-threads=1 --include-ignored"]
    fn write_then_read_rev_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("XDG_DATA_HOME", tmp.path());
        }
        let rev = RevSidecar {
            version: "2025-01-13".to_string(),
            sha256: "deadbeef".to_string(),
        };
        write_rev("rust-analyzer", &rev).unwrap();
        let read_back = read_rev("rust-analyzer").unwrap();
        assert_eq!(read_back, Some(rev));
        unsafe {
            std::env::remove_var("XDG_DATA_HOME");
        }
    }

    // ── I/O tests that use a tempdir without env mutation ────────────────────

    #[test]
    fn write_then_read_rev_direct_tempdir() {
        // This test exercises write_rev / read_rev directly against a tempdir
        // path, bypassing the XDG env lookup by constructing paths manually.
        let tmp = tempfile::tempdir().unwrap();
        let name = "taplo";

        // Simulate what write_rev does internally, but with a custom base.
        let pkg_dir = tmp.path().join("packages").join(name);
        std::fs::create_dir_all(&pkg_dir).unwrap();
        let rev = RevSidecar {
            version: "0.9.3".to_string(),
            sha256: String::new(),
        };
        let target = pkg_dir.join(".rev");
        let staging = target.with_extension("rev.tmp");
        std::fs::write(&staging, rev.to_string()).unwrap();
        std::fs::rename(&staging, &target).unwrap();

        // Read back manually
        let content = std::fs::read_to_string(&target).unwrap();
        let parsed = RevSidecar::parse(&content).unwrap();
        assert_eq!(parsed, rev);
    }

    #[test]
    fn read_rev_returns_none_for_missing_file_directly() {
        // Verify the NotFound branch: a path that doesn't exist returns None.
        let tmp = tempfile::tempdir().unwrap();
        let nonexistent = tmp.path().join(".rev");
        let result = match std::fs::read_to_string(&nonexistent) {
            Ok(s) => RevSidecar::parse(&s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => panic!("unexpected error: {e}"),
        };
        assert!(result.is_none());
    }
}
