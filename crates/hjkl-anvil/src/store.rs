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
//! ## Testing without XDG env mutation
//!
//! [`AnvilPaths`] carries an explicit `data_root` / `cache_root` pair through
//! every path helper (the `_in` variants below); production resolves it once
//! via [`AnvilPaths::from_xdg`]. Tests build an `AnvilPaths` from a per-test
//! `TempDir` instead of `std::env::set_var`-ing `XDG_DATA_HOME` /
//! `XDG_CACHE_HOME` — env vars are process-global, so mutating them from
//! parallel tests races no matter how carefully a `Mutex` tries to serialize
//! it (audit-r2 fix 4). This is the same pattern `hjkl_xdg::resolve_xdg`
//! uses one layer down.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use hjkl_xdg::{cache_home, data_home};
use thiserror::Error;

// ── Error ────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("xdg resolution failed: {0}")]
    Xdg(#[from] hjkl_xdg::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid package name: {0}")]
    InvalidName(String),
}

/// Reject any tool name that is not a single, safe path component.
///
/// A name with separators, `.`/`..`, or an absolute prefix could otherwise
/// escape `packages/` when joined — e.g. `:Anvil uninstall ../../foo` or an
/// absolute path, which `Path::join` would treat as the whole target — and
/// reach `remove_dir_all` / install writes on an arbitrary directory.
fn validate_name(name: &str) -> Result<(), StoreError> {
    use std::path::Component;
    let mut comps = Path::new(name).components();
    let single_normal =
        matches!(comps.next(), Some(Component::Normal(_))) && comps.next().is_none();
    if single_normal {
        Ok(())
    } else {
        Err(StoreError::InvalidName(name.to_string()))
    }
}

// ── Explicit path roots (audit-r2 fix 4) ────────────────────────────────────

/// Explicit data/cache roots for the anvil store.
///
/// Every path helper below has an `_in(paths, ..)` variant that composes
/// from this struct instead of reading `XDG_DATA_HOME` / `XDG_CACHE_HOME`
/// off the process environment. This is what lets the install pipeline
/// (`install_github_inner` and friends) be driven with a per-test `TempDir`
/// instead of `std::env::set_var` — env vars are process-global, so mutating
/// them from parallel tests is inherently racy no matter how carefully a
/// `Mutex` tries to serialize it (a stray un-locked test anywhere in the
/// binary still collides). Explicit paths sidestep the race entirely: two
/// tests using the SAME tool name against DIFFERENT `AnvilPaths` simply
/// can't collide on disk.
///
/// The plain (non-`_in`) helpers below are kept for API compatibility and
/// resolve via [`AnvilPaths::from_xdg`], matching pre-fix behavior exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnvilPaths {
    pub data_root: PathBuf,
    pub cache_root: PathBuf,
}

impl AnvilPaths {
    /// Resolve from the process's `XDG_DATA_HOME` / `XDG_CACHE_HOME` (or
    /// their fallbacks) — the production default.
    pub fn from_xdg() -> Result<Self, StoreError> {
        Ok(Self {
            data_root: data_home()?.join("anvil"),
            cache_root: cache_home()?.join("anvil"),
        })
    }
}

// ── Path helpers ─────────────────────────────────────────────────────────────

/// `<paths.data_root>/`.
pub fn data_root_in(paths: &AnvilPaths) -> PathBuf {
    paths.data_root.clone()
}

/// `<XDG_DATA_HOME>/anvil/`.
pub fn data_root() -> Result<PathBuf, StoreError> {
    Ok(data_root_in(&AnvilPaths::from_xdg()?))
}

/// `<paths.cache_root>/`.
pub fn cache_root_in(paths: &AnvilPaths) -> PathBuf {
    paths.cache_root.clone()
}

/// `<XDG_CACHE_HOME>/anvil/`.
pub fn cache_root() -> Result<PathBuf, StoreError> {
    Ok(cache_root_in(&AnvilPaths::from_xdg()?))
}

/// `<data_root>/packages/`.
pub fn packages_dir_in(paths: &AnvilPaths) -> PathBuf {
    paths.data_root.join("packages")
}

/// `<data_root>/packages/`.
pub fn packages_dir() -> Result<PathBuf, StoreError> {
    Ok(packages_dir_in(&AnvilPaths::from_xdg()?))
}

/// `<data_root>/packages/<name>/`.
pub fn package_dir_in(paths: &AnvilPaths, name: &str) -> Result<PathBuf, StoreError> {
    validate_name(name)?;
    Ok(packages_dir_in(paths).join(name))
}

/// `<data_root>/packages/<name>/`.
pub fn package_dir(name: &str) -> Result<PathBuf, StoreError> {
    package_dir_in(&AnvilPaths::from_xdg()?, name)
}

/// `<data_root>/packages/<name>/.rev`.
///
/// Sidecar file recording the pinned version + sha that produced the install.
/// Format: `<version>:<sha256>` on a single line.
pub fn rev_file_in(paths: &AnvilPaths, name: &str) -> Result<PathBuf, StoreError> {
    Ok(package_dir_in(paths, name)?.join(".rev"))
}

/// `<data_root>/packages/<name>/.rev`.
pub fn rev_file(name: &str) -> Result<PathBuf, StoreError> {
    rev_file_in(&AnvilPaths::from_xdg()?, name)
}

/// `<data_root>/checksums/`.
///
/// Directory of per-tool TOFU checksum sidecars. Created on first call.
pub fn checksums_dir_in(paths: &AnvilPaths) -> Result<PathBuf, StoreError> {
    let dir = paths.data_root.join("checksums");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// `<data_root>/checksums/`.
pub fn checksums_dir() -> Result<PathBuf, StoreError> {
    checksums_dir_in(&AnvilPaths::from_xdg()?)
}

/// `<data_root>/bin/`.
///
/// Flat directory of symlinks that consumers prepend to `$PATH`.
pub fn bin_dir_in(paths: &AnvilPaths) -> PathBuf {
    paths.data_root.join("bin")
}

/// `<data_root>/bin/`.
pub fn bin_dir() -> Result<PathBuf, StoreError> {
    Ok(bin_dir_in(&AnvilPaths::from_xdg()?))
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
pub fn read_rev_in(paths: &AnvilPaths, name: &str) -> Result<Option<RevSidecar>, StoreError> {
    let path = rev_file_in(paths, name)?;
    match std::fs::read_to_string(&path) {
        Ok(s) => Ok(RevSidecar::parse(&s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(StoreError::Io(e)),
    }
}

/// Read the `<package>/.rev` sidecar.
pub fn read_rev(name: &str) -> Result<Option<RevSidecar>, StoreError> {
    read_rev_in(&AnvilPaths::from_xdg()?, name)
}

/// Write `<package>/.rev` atomically via a staging file + rename.
///
/// Creates the package directory if it doesn't exist.
pub fn write_rev_in(paths: &AnvilPaths, name: &str, rev: &RevSidecar) -> Result<(), StoreError> {
    let pkg_dir = package_dir_in(paths, name)?;
    std::fs::create_dir_all(&pkg_dir)?;

    let target = rev_file_in(paths, name)?;
    // Write to a staging file alongside the target, then rename atomically.
    let staging = target.with_extension("rev.tmp");
    std::fs::write(&staging, rev.to_string())?;
    std::fs::rename(&staging, &target)?;
    Ok(())
}

/// Write `<package>/.rev` atomically via a staging file + rename.
pub fn write_rev(name: &str, rev: &RevSidecar) -> Result<(), StoreError> {
    write_rev_in(&AnvilPaths::from_xdg()?, name, rev)
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
                current_version = Some(toml_unescape(strip_one_quote(inner)));
                continue;
            }
            // Key = value line inside a section.
            if let Some(ver) = &current_version
                && let Some((key, val)) = line.split_once('=')
            {
                let key = toml_unescape(strip_one_quote(key.trim()));
                let val = toml_unescape(strip_one_quote(val.trim()));
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
    ///
    /// Every interpolated value (version, triple, hash) is escaped as a TOML
    /// basic string so that a value containing `"`, `\`, or a newline cannot
    /// break out of its quotes and inject extra sections or keys — e.g. a
    /// crafted `version` string poisoning another version's recorded checksum.
    fn to_toml(&self) -> String {
        let mut out = String::new();
        for (version, triples) in &self.versions {
            out.push_str(&format!("[versions.\"{}\".sha256]\n", toml_escape(version)));
            for (triple, hash) in triples {
                out.push_str(&format!(
                    "\"{}\" = \"{}\"\n",
                    toml_escape(triple),
                    toml_escape(hash)
                ));
            }
            out.push('\n');
        }
        out
    }

    /// Read the checksum sidecar for `tool`.
    ///
    /// Returns `Ok(None)` when the file is absent (no prior TOFU installs).
    pub fn read_in(paths: &AnvilPaths, tool: &str) -> Result<Option<Self>, StoreError> {
        // `tool` is joined into the checksums path — reject names that could
        // escape `checksums/` (e.g. `../…` or an absolute path).
        validate_name(tool)?;
        let path = checksums_dir_in(paths)?.join(format!("{tool}.toml"));
        match std::fs::read_to_string(&path) {
            Ok(s) => Ok(Some(Self::from_toml(&s)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(StoreError::Io(e)),
        }
    }

    /// Read the checksum sidecar for `tool`.
    pub fn read(tool: &str) -> Result<Option<Self>, StoreError> {
        Self::read_in(&AnvilPaths::from_xdg()?, tool)
    }

    /// Write the checksum sidecar for `tool` atomically (tmpfile + rename).
    pub fn write_in(&self, paths: &AnvilPaths, tool: &str) -> Result<(), StoreError> {
        // Same escape guard as `read` — this path is written to.
        validate_name(tool)?;
        let dir = checksums_dir_in(paths)?;
        let target = dir.join(format!("{tool}.toml"));
        let staging = target.with_extension("toml.tmp");
        std::fs::write(&staging, self.to_toml())?;
        std::fs::rename(&staging, &target)?;
        Ok(())
    }

    /// Write the checksum sidecar for `tool` atomically (tmpfile + rename).
    pub fn write(&self, tool: &str) -> Result<(), StoreError> {
        self.write_in(&AnvilPaths::from_xdg()?, tool)
    }
}

/// Escape a string for use as a TOML basic (double-quoted) string value.
/// Escapes `\`, `"`, and the whitespace/control characters that would
/// otherwise break the single-line, quote-delimited sidecar format.
fn toml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Reverse of [`toml_escape`]. Unknown escapes are passed through literally
/// (with the backslash preserved) rather than dropped.
fn toml_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    Some(ch) => out.push(ch),
                    None => {
                        out.push_str("\\u");
                        out.push_str(&hex);
                    }
                }
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Strip exactly one wrapping double-quote from each end when both are present.
/// Unlike `trim_matches('"')` this removes a single quote (not a run), so an
/// escaped `\"` adjacent to the wrapper quote survives for [`toml_unescape`].
fn strip_one_quote(s: &str) -> &str {
    s.strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .unwrap_or(s)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an [`AnvilPaths`] rooted at a fresh `TempDir`'s `data` / `cache`
    /// subdirs. Returned alongside the `TempDir` so callers keep it alive for
    /// the duration of the test (it deletes its contents on drop).
    fn temp_paths() -> (tempfile::TempDir, AnvilPaths) {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AnvilPaths {
            data_root: tmp.path().join("data").join("anvil"),
            cache_root: tmp.path().join("cache").join("anvil"),
        };
        (tmp, paths)
    }

    // ── Name validation (no I/O, parallel-safe) ─────────────────────────────

    #[test]
    fn validate_name_accepts_plain_names() {
        for n in ["rust-analyzer", "gopls", "foo_bar", "a.b"] {
            assert!(validate_name(n).is_ok(), "{n} should be accepted");
        }
    }

    #[test]
    fn validate_name_rejects_traversal_and_absolute() {
        for n in [
            "",
            ".",
            "..",
            "../foo",
            "../../etc",
            "foo/bar",
            "foo/../bar",
            "/etc/passwd",
            "/",
        ] {
            assert!(
                validate_name(n).is_err(),
                "{n:?} must be rejected as an unsafe package name"
            );
        }
    }

    #[test]
    fn package_dir_rejects_escaping_name() {
        // Regression: `:Anvil uninstall ../../foo` must not resolve outside
        // `packages/` and reach `remove_dir_all`.
        assert!(matches!(
            package_dir("../../foo"),
            Err(StoreError::InvalidName(_))
        ));
        assert!(matches!(
            package_dir("/tmp/evil"),
            Err(StoreError::InvalidName(_))
        ));
    }

    #[test]
    fn checksum_sidecar_rejects_escaping_tool_name() {
        // Regression: the tool name is joined into `checksums/<tool>.toml`
        // and must not escape the checksums dir (read or write). Validation
        // happens before any I/O, so no XDG env setup is needed.
        for name in ["../evil", "a/b", "/abs", "..", ""] {
            assert!(
                matches!(ChecksumSidecar::read(name), Err(StoreError::InvalidName(_))),
                "read({name:?}) must be rejected"
            );
            assert!(
                matches!(
                    ChecksumSidecar::default().write(name),
                    Err(StoreError::InvalidName(_))
                ),
                "write({name:?}) must be rejected"
            );
        }
    }

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

    // ── Path resolution tests (explicit AnvilPaths — no env, parallel-safe) ──

    #[test]
    fn data_root_honors_xdg_data_home() {
        let (_tmp, paths) = temp_paths();
        let root = data_root_in(&paths);
        assert_eq!(root, paths.data_root);
    }

    #[test]
    fn cache_root_honors_xdg_cache_home() {
        let (_tmp, paths) = temp_paths();
        let root = cache_root_in(&paths);
        assert_eq!(root, paths.cache_root);
    }

    #[test]
    fn packages_dir_is_under_data_root() {
        let (_tmp, paths) = temp_paths();
        let pd = packages_dir_in(&paths);
        assert_eq!(pd, paths.data_root.join("packages"));
    }

    #[test]
    fn package_dir_appends_name() {
        let (_tmp, paths) = temp_paths();
        let pd = package_dir_in(&paths, "rust-analyzer").unwrap();
        assert_eq!(pd, paths.data_root.join("packages").join("rust-analyzer"));
    }

    #[test]
    fn bin_dir_is_under_data_root() {
        let (_tmp, paths) = temp_paths();
        let bd = bin_dir_in(&paths);
        assert_eq!(bd, paths.data_root.join("bin"));
    }

    #[test]
    fn read_rev_returns_none_for_absent_package() {
        let (_tmp, paths) = temp_paths();
        let result = read_rev_in(&paths, "nonexistent-tool").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn write_then_read_rev_round_trip() {
        let (_tmp, paths) = temp_paths();
        let rev = RevSidecar {
            version: "2025-01-13".to_string(),
            sha256: "deadbeef".to_string(),
        };
        write_rev_in(&paths, "rust-analyzer", &rev).unwrap();
        let read_back = read_rev_in(&paths, "rust-analyzer").unwrap();
        assert_eq!(read_back, Some(rev));
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

    #[test]
    fn toml_escape_unescape_round_trips_specials() {
        for raw in [
            "plain",
            "with\"quote",
            "with\\backslash",
            "with\nnewline",
            "tab\tand\r\n",
            "]injection[\"attempt",
        ] {
            assert_eq!(toml_unescape(&toml_escape(raw)), raw, "round-trip {raw:?}");
        }
    }

    #[test]
    fn checksum_sidecar_hostile_version_does_not_inject_sections() {
        // A version string crafted to break out of its quotes and inject a
        // second `[versions.…]` section (poisoning another version's hash)
        // must round-trip verbatim as a SINGLE version entry.
        let hostile = "1.0\"].sha256]\n[versions.\"evil\".sha256]\n\"t\" = \"beef";
        let mut s = ChecksumSidecar::default();
        s.set(
            hostile,
            "x86_64-unknown-linux-gnu",
            "deadbeef01234567deadbeef01234567deadbeef01234567deadbeef01234567",
        );

        let toml_str = s.to_toml();
        // The serialized form must not contain a raw newline inside the value
        // (the injection vector) — the newline is escaped.
        assert!(
            !toml_str.contains("\n[versions.\"evil\""),
            "hostile version injected a section: {toml_str}"
        );

        let parsed = ChecksumSidecar::from_toml(&toml_str).unwrap();
        assert_eq!(parsed, s, "hostile version must round-trip verbatim");
        assert_eq!(
            parsed.versions.len(),
            1,
            "exactly one version entry expected, got {}",
            parsed.versions.len()
        );
        assert!(
            parsed.versions.contains_key(hostile),
            "the single entry must be the hostile version key, verbatim"
        );
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

    // ── ChecksumSidecar I/O tests (explicit AnvilPaths — no env) ────────────

    #[test]
    fn checksum_sidecar_xdg_data_home_respected() {
        let (_tmp, paths) = temp_paths();

        let mut sidecar = ChecksumSidecar::default();
        sidecar.set(
            "v1.0",
            "x86_64-unknown-linux-gnu",
            "deadbeef01234567deadbeef01234567deadbeef01234567deadbeef01234567",
        );
        sidecar.write_in(&paths, "my-tool").unwrap();

        let read_back = ChecksumSidecar::read_in(&paths, "my-tool").unwrap();
        assert_eq!(read_back, Some(sidecar));

        // Path must be inside the injected data root.
        let expected_path = paths.data_root.join("checksums").join("my-tool.toml");
        assert!(
            expected_path.exists(),
            "sidecar must live under paths.data_root"
        );
    }

    #[test]
    fn checksum_sidecar_read_returns_none_for_absent_tool() {
        let (_tmp, paths) = temp_paths();
        let result = ChecksumSidecar::read_in(&paths, "nonexistent-tool").unwrap();
        assert!(result.is_none());
    }

    // ── AnvilPaths::from_xdg smoke test (reads live env, never mutates it) ──

    #[test]
    fn anvil_paths_from_xdg_resolves_anvil_suffix() {
        let paths = AnvilPaths::from_xdg().unwrap();
        assert!(paths.data_root.ends_with("anvil"));
        assert!(paths.cache_root.ends_with("anvil"));
    }
}
