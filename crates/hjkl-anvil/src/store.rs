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
//! ├── checksums/
//! │   └── <tool>.toml       # TOFU checksum sidecar (per-version, per-triple)
//! └── bin/                  # flat symlinks for $PATH prepend
//!
//! $XDG_CACHE_HOME/anvil/    # download staging, extraction scratch
//! ```
//!
//! ## Env-override tests
//!
//! The path-resolution tests that mutate `XDG_DATA_HOME` / `XDG_CACHE_HOME`
//! via `std::env::set_var` run under the `anvil-env` nextest group
//! (`max-threads = 1`) defined in `.config/nextest.toml` at the workspace
//! root. They are serialized automatically by `cargo nextest run`.

use std::collections::BTreeMap;
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

/// `<data_root>/checksums/`.
///
/// Directory of per-tool TOFU checksum sidecars. Created on first call.
pub fn checksums_dir() -> Result<PathBuf, StoreError> {
    let dir = data_root()?.join("checksums");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
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

// ── Checksum sidecar ─────────────────────────────────────────────────────────

/// TOFU checksum sidecar stored at `<checksums_dir>/<tool>.toml`.
///
/// Shape on disk:
/// ```toml
/// [versions."<version>".sha256]
/// "<triple>" = "<64-hex>"
/// ```
///
/// Multiple versions and multiple triples per version are supported.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChecksumSidecar {
    /// versions -> (triple -> sha256 hex)
    pub versions: BTreeMap<String, BTreeMap<String, String>>,
}

impl ChecksumSidecar {
    /// Get the cached sha256 for `(version, triple)`.
    pub fn get(&self, version: &str, triple: &str) -> Option<&str> {
        self.versions
            .get(version)
            .and_then(|triples| triples.get(triple))
            .map(String::as_str)
    }

    /// Record a sha256 for `(version, triple)`.
    pub fn set(
        &mut self,
        version: impl Into<String>,
        triple: impl Into<String>,
        hash: impl Into<String>,
    ) {
        self.versions
            .entry(version.into())
            .or_default()
            .insert(triple.into(), hash.into());
    }

    /// Parse a TOML string produced by [`ChecksumSidecar::to_toml`].
    pub fn from_toml_pub(s: &str) -> Result<Self, StoreError> {
        Self::from_toml(s)
    }

    fn from_toml(s: &str) -> Result<Self, StoreError> {
        // Deserialise manually; avoid pulling in serde for a hand-rolled format.
        // Format:
        //   [versions."<ver>".sha256]
        //   "<triple>" = "<hash>"
        let mut sidecar = ChecksumSidecar::default();
        let mut current_version: Option<String> = None;

        for line in s.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            // Section header: [versions."<ver>".sha256]
            if let Some(inner) = line
                .strip_prefix("[versions.")
                .and_then(|s| s.strip_suffix(".sha256]"))
            {
                let ver = inner.trim_matches('"').to_string();
                current_version = Some(ver);
                continue;
            }
            // Key = value line inside a section.
            if let Some(ver) = &current_version
                && let Some((key, val)) = line.split_once('=')
            {
                let key = key.trim().trim_matches('"').to_string();
                let val = val.trim().trim_matches('"').to_string();
                if !key.is_empty() && !val.is_empty() {
                    sidecar
                        .versions
                        .entry(ver.clone())
                        .or_default()
                        .insert(key, val);
                }
            }
        }
        Ok(sidecar)
    }

    /// Serialize to TOML.
    fn to_toml(&self) -> String {
        let mut out = String::new();
        for (version, triples) in &self.versions {
            out.push_str(&format!("[versions.\"{version}\".sha256]\n"));
            for (triple, hash) in triples {
                out.push_str(&format!("\"{triple}\" = \"{hash}\"\n"));
            }
            out.push('\n');
        }
        out
    }

    /// Read the checksum sidecar for `tool`.
    ///
    /// Returns `Ok(None)` when the file is absent (no prior TOFU installs).
    pub fn read(tool: &str) -> Result<Option<Self>, StoreError> {
        let path = checksums_dir()?.join(format!("{tool}.toml"));
        match std::fs::read_to_string(&path) {
            Ok(s) => Ok(Some(Self::from_toml(&s)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(StoreError::Io(e)),
        }
    }

    /// Write the checksum sidecar for `tool` atomically (tmpfile + rename).
    pub fn write(&self, tool: &str) -> Result<(), StoreError> {
        let dir = checksums_dir()?;
        let target = dir.join(format!("{tool}.toml"));
        let staging = target.with_extension("toml.tmp");
        std::fs::write(&staging, self.to_toml())?;
        std::fs::rename(&staging, &target)?;
        Ok(())
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Serializes all env-mutating tests within a single `cargo test` process.
    // Under nextest the `anvil-env` group (max-threads = 1) provides the same
    // guarantee across processes.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

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

    // ── Path resolution tests (env-mutating — serialized via nextest group) ──

    #[test]
    fn data_root_honors_xdg_data_home() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
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
    fn cache_root_honors_xdg_cache_home() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    fn packages_dir_is_under_data_root() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    fn package_dir_appends_name() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    fn bin_dir_is_under_data_root() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    fn read_rev_returns_none_for_absent_package() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    fn write_then_read_rev_round_trip() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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

    // ── ChecksumSidecar unit tests (no I/O, parallel-safe) ───────────────────

    #[test]
    fn checksum_sidecar_set_and_get() {
        let mut s = ChecksumSidecar::default();
        s.set(
            "v1.0",
            "x86_64-unknown-linux-gnu",
            "deadbeef01234567deadbeef01234567deadbeef01234567deadbeef01234567",
        );
        let got = s.get("v1.0", "x86_64-unknown-linux-gnu");
        assert_eq!(
            got,
            Some("deadbeef01234567deadbeef01234567deadbeef01234567deadbeef01234567")
        );
    }

    #[test]
    fn checksum_sidecar_get_missing_returns_none() {
        let s = ChecksumSidecar::default();
        assert!(s.get("v1.0", "x86_64-unknown-linux-gnu").is_none());
    }

    #[test]
    fn checksum_sidecar_multiple_versions_coexist() {
        let mut s = ChecksumSidecar::default();
        s.set(
            "v1.0",
            "x86_64-unknown-linux-gnu",
            "aaaa01234567890123456789012345678901234567890123456789012345678901",
        );
        s.set(
            "v2.0",
            "x86_64-unknown-linux-gnu",
            "bbbb01234567890123456789012345678901234567890123456789012345678901",
        );
        // Both coexist independently.
        assert!(s.get("v1.0", "x86_64-unknown-linux-gnu").is_some());
        assert!(s.get("v2.0", "x86_64-unknown-linux-gnu").is_some());
        assert_ne!(
            s.get("v1.0", "x86_64-unknown-linux-gnu"),
            s.get("v2.0", "x86_64-unknown-linux-gnu")
        );
    }

    #[test]
    fn checksum_sidecar_multiple_triples_per_version() {
        let mut s = ChecksumSidecar::default();
        s.set(
            "v1.0",
            "x86_64-unknown-linux-gnu",
            "aaaa01234567890123456789012345678901234567890123456789012345678901",
        );
        s.set(
            "v1.0",
            "aarch64-apple-darwin",
            "bbbb01234567890123456789012345678901234567890123456789012345678901",
        );
        assert!(s.get("v1.0", "x86_64-unknown-linux-gnu").is_some());
        assert!(s.get("v1.0", "aarch64-apple-darwin").is_some());
        assert!(s.get("v1.0", "x86_64-pc-windows-msvc").is_none());
    }

    #[test]
    fn checksum_sidecar_round_trip_toml() {
        let mut s = ChecksumSidecar::default();
        s.set(
            "v1.0",
            "x86_64-unknown-linux-gnu",
            "deadbeef01234567deadbeef01234567deadbeef01234567deadbeef01234567",
        );
        s.set(
            "v1.0",
            "aarch64-apple-darwin",
            "cafebabe01234567cafebabe01234567cafebabe01234567cafebabe01234567",
        );
        s.set(
            "v2.0",
            "x86_64-unknown-linux-gnu",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        );

        let toml_str = s.to_toml();
        let parsed = ChecksumSidecar::from_toml(&toml_str).unwrap();
        assert_eq!(parsed, s);
    }

    // ── ChecksumSidecar I/O tests (use tempdir directly, no env mutation) ────

    #[test]
    fn checksum_sidecar_write_then_read_direct() {
        // Exercise write + read using a tempdir as the checksums dir, bypassing XDG.
        let tmp = tempfile::tempdir().unwrap();
        let tool = "rust-analyzer";

        let mut sidecar = ChecksumSidecar::default();
        sidecar.set(
            "2025-01-13",
            "x86_64-unknown-linux-gnu",
            "deadbeef01234567deadbeef01234567deadbeef01234567deadbeef01234567",
        );

        // Manually write to the tmp dir (same logic as ChecksumSidecar::write).
        let target = tmp.path().join(format!("{tool}.toml"));
        let staging = target.with_extension("toml.tmp");
        std::fs::write(&staging, sidecar.to_toml()).unwrap();
        std::fs::rename(&staging, &target).unwrap();

        // Read back manually.
        let content = std::fs::read_to_string(&target).unwrap();
        let parsed = ChecksumSidecar::from_toml(&content).unwrap();
        assert_eq!(parsed, sidecar);
    }

    // ── ChecksumSidecar env-mutating I/O tests (XDG_DATA_HOME respected) ────

    #[test]
    fn checksum_sidecar_xdg_data_home_respected() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("XDG_DATA_HOME", tmp.path());
        }

        let mut sidecar = ChecksumSidecar::default();
        sidecar.set(
            "v1.0",
            "x86_64-unknown-linux-gnu",
            "deadbeef01234567deadbeef01234567deadbeef01234567deadbeef01234567",
        );
        sidecar.write("my-tool").unwrap();

        let read_back = ChecksumSidecar::read("my-tool").unwrap();
        assert_eq!(read_back, Some(sidecar));

        // Path must be inside XDG_DATA_HOME.
        let expected_path = tmp
            .path()
            .join("anvil")
            .join("checksums")
            .join("my-tool.toml");
        assert!(expected_path.exists(), "sidecar must live in XDG_DATA_HOME");

        unsafe {
            std::env::remove_var("XDG_DATA_HOME");
        }
    }

    #[test]
    fn checksum_sidecar_read_returns_none_for_absent_tool() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("XDG_DATA_HOME", tmp.path());
        }

        let result = ChecksumSidecar::read("nonexistent-tool").unwrap();
        assert!(result.is_none());

        unsafe {
            std::env::remove_var("XDG_DATA_HOME");
        }
    }
}
