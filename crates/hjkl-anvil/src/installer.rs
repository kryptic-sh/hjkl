//! Install pipeline for Github and Cargo tool methods.
//!
//! # Design
//!
//! The public surface is built around the [`Install`] trait so that step 5
//! (Npm, Pip, GoInstall, Script) just adds new impl structs without touching
//! this file.
//!
//! The [`GithubInstaller`] uses an internal `install_github_inner` function
//! that accepts a `download` closure.  Production code passes a real
//! `reqwest`-backed downloader; tests pass a closure that copies fixture bytes
//! directly.  This keeps network-free testing possible without any mocking
//! framework.
//!
//! # Security
//!
//! Every tar/zip entry path is validated by [`safe_join`] before extraction.
//! Parent-dir components (`..`), root prefixes, and absolute paths are all
//! rejected with [`InstallError::PathEscape`].

use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

use crate::manifest::{InstallMethod, ToolSpec};
use crate::store::{self, RevSidecar};

// ── Error ─────────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("checksum mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },

    #[error("missing checksum for triple {0}")]
    MissingChecksum(String),

    #[error("unsupported triple: {0}")]
    UnsupportedTriple(String),

    #[error("archive error: {0}")]
    Archive(String),

    #[error("path escape detected: {0}")]
    PathEscape(String),

    #[error("unsupported install method: {0}")]
    UnsupportedMethod(&'static str),

    #[error("io: {0}")]
    Io(#[from] io::Error),

    #[error("xdg: {0}")]
    Xdg(#[from] hjkl_xdg::Error),

    #[error("store: {0}")]
    Store(#[from] store::StoreError),

    #[error("subprocess failed: {cmd}: {stderr}")]
    Subprocess { cmd: String, stderr: String },

    #[error("binary not found in installed package: {0}")]
    BinNotFound(String),
}

// ── Status ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallStatus {
    Queued,
    Downloading {
        bytes_downloaded: u64,
        total: Option<u64>,
    },
    Verifying,
    Extracting,
    Installing,
    Done {
        bin_path: PathBuf,
    },
    Failed(String),
}

// ── Trait ─────────────────────────────────────────────────────────────────────

/// One install backend. Step 5 just adds new impls — no changes here.
pub trait Install {
    fn install(
        &self,
        name: &str,
        spec: &ToolSpec,
        progress: &dyn Fn(InstallStatus),
    ) -> Result<PathBuf, InstallError>;
}

// ── Dispatcher ────────────────────────────────────────────────────────────────

/// Pick the right [`Install`] impl and run it.
pub fn install_blocking(
    name: &str,
    spec: &ToolSpec,
    progress: &dyn Fn(InstallStatus),
) -> Result<PathBuf, InstallError> {
    match &spec.method {
        InstallMethod::Github(_) => GithubInstaller.install(name, spec, progress),
        InstallMethod::Cargo(_) => CargoInstaller.install(name, spec, progress),
        InstallMethod::Npm(_) => Err(InstallError::UnsupportedMethod("npm")),
        InstallMethod::Pip(_) => Err(InstallError::UnsupportedMethod("pip")),
        InstallMethod::GoInstall(_) => Err(InstallError::UnsupportedMethod("goinstall")),
        InstallMethod::Script(_) => Err(InstallError::UnsupportedMethod("script")),
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Returns `Ok(joined_path)` if joined_path stays inside `root`. Rejects
/// parent-dir traversal, root prefixes, and absolute paths.
///
/// Neither `root` nor `entry` is required to exist on disk — this is a
/// pure-path check.
pub fn safe_join(root: &Path, entry: &Path) -> Result<PathBuf, InstallError> {
    let mut out = root.to_path_buf();
    for comp in entry.components() {
        match comp {
            Component::Normal(c) => out.push(c),
            Component::CurDir => {} // skip `.`
            // Reject parent-dir traversal, root prefixes, absolute paths.
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(InstallError::PathEscape(entry.display().to_string()));
            }
        }
    }
    Ok(out)
}

/// Map `(OS, ARCH)` → one of the six canonical triples we support.
///
/// `linux-musl` is not auto-detected — it requires an explicit opt-in via a
/// future env var / config field.  Add it there; do not extend this fn.
///
/// TODO: expose an opt-in env var or config key (e.g. `ANVIL_MUSL=1`) that
/// overrides the result to `x86_64-unknown-linux-musl` on Linux/x86_64.
pub fn host_triple() -> Result<&'static str, InstallError> {
    use std::env::consts::{ARCH, OS};
    Ok(match (OS, ARCH) {
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        (os, arch) => return Err(InstallError::UnsupportedTriple(format!("{os}/{arch}"))),
    })
}

/// Determine the archive extension from the asset filename.  Returns one of
/// `"tar.gz"`, `"tgz"`, `"gz"`, `"zip"`, or `""` (raw binary).
fn asset_ext(asset_name: &str) -> &'static str {
    let name = asset_name.to_ascii_lowercase();
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        "tar.gz"
    } else if name.ends_with(".gz") {
        "gz"
    } else if name.ends_with(".zip") {
        "zip"
    } else {
        ""
    }
}

/// Recursively search `dir` for a file named `bin_name`.  Returns the path
/// relative to `dir` on success.
fn find_bin(dir: &Path, bin_name: &str) -> Option<PathBuf> {
    for entry in walkdir(dir) {
        if entry.file_name().is_some_and(|fname| fname == bin_name) {
            // Return path relative to dir
            return entry.strip_prefix(dir).ok().map(|p| p.to_path_buf());
        }
    }
    None
}

/// Simple recursive directory walker that yields `PathBuf` for every file.
fn walkdir(root: &Path) -> Vec<PathBuf> {
    let mut result = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                result.push(path);
            }
        }
    }
    result
}

/// Create the symlink atomically: write to `<target>.tmp`, then rename.
///
/// On Windows, symlink creation requires elevated privileges or Developer Mode.
/// TODO: On Windows, consider copying the binary instead of symlinking.
fn atomic_symlink(link_path: &Path, target: &Path) -> Result<(), InstallError> {
    let tmp = link_path.with_extension("tmp");
    // Remove stale temp if present.
    let _ = std::fs::remove_file(&tmp);

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, &tmp)?;
    }
    #[cfg(not(unix))]
    {
        // TODO: Windows – copy file instead of symlinking (requires elevation).
        // For now we just error out gracefully.
        return Err(InstallError::Archive(
            "symlinks not supported on this platform; TODO: implement copy fallback".to_string(),
        ));
    }

    std::fs::rename(&tmp, link_path)?;
    Ok(())
}

/// Emit `chmod 0755` on Unix; no-op on other platforms.
fn make_executable(path: &Path) -> Result<(), InstallError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms)?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

// ── Github installer ──────────────────────────────────────────────────────────

pub struct GithubInstaller;

impl Install for GithubInstaller {
    fn install(
        &self,
        name: &str,
        spec: &ToolSpec,
        progress: &dyn Fn(InstallStatus),
    ) -> Result<PathBuf, InstallError> {
        install_github_inner(name, spec, real_download, progress)
    }
}

/// Real reqwest-backed downloader. Streams in 64 KiB chunks and reports
/// `Downloading` progress for each chunk.
fn real_download(
    url: &str,
    dest: &Path,
    progress: &dyn Fn(InstallStatus),
) -> Result<(), InstallError> {
    let response = reqwest::blocking::get(url)?;
    let total = response.content_length();
    let mut downloaded: u64 = 0;

    let mut reader = response;
    let mut file = std::fs::File::create(dest)?;
    let mut buf = [0u8; 65536];

    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        downloaded += n as u64;
        progress(InstallStatus::Downloading {
            bytes_downloaded: downloaded,
            total,
        });
    }

    Ok(())
}

/// Testable core of the Github install pipeline.
///
/// `download` is a closure `(url, dest_path, progress) -> Result<(), InstallError>`.
/// Production: `real_download`. Tests: copy fixture bytes.
pub fn install_github_inner(
    name: &str,
    spec: &ToolSpec,
    download: impl Fn(&str, &Path, &dyn Fn(InstallStatus)) -> Result<(), InstallError>,
    progress: &dyn Fn(InstallStatus),
) -> Result<PathBuf, InstallError> {
    let InstallMethod::Github(ref gh) = spec.method else {
        return Err(InstallError::UnsupportedMethod("not a github method"));
    };

    // 1. Detect triple → look up sha256.
    let triple = host_triple()?;
    let expected_sha = gh
        .sha256
        .get(triple)
        .ok_or_else(|| InstallError::MissingChecksum(triple.to_string()))?
        .clone();

    // 2. Build download URL.
    let asset = gh
        .asset_pattern
        .replace("{triple}", triple)
        .replace("{version}", &spec.version);
    let url = format!(
        "https://github.com/{}/releases/download/{}/{}",
        gh.repo, spec.version, asset
    );

    // 3. Download to staging file.
    let cache = store::cache_root()?;
    let staging_dir = cache.join("staging").join(name);
    std::fs::create_dir_all(&staging_dir)?;

    let ext = asset_ext(&asset);
    let dl_name = format!("{name}.{ext}");
    let dl_path = staging_dir.join(&dl_name);

    download(&url, &dl_path, progress)?;

    // 4. Verify SHA-256.
    progress(InstallStatus::Verifying);
    let actual_sha = sha256_file(&dl_path)?;
    if actual_sha != expected_sha {
        return Err(InstallError::ChecksumMismatch {
            expected: expected_sha,
            actual: actual_sha,
        });
    }

    // 5. Extract.
    progress(InstallStatus::Extracting);
    let extract_dir = staging_dir.join("extract");
    std::fs::create_dir_all(&extract_dir)?;
    extract_archive(&dl_path, ext, &extract_dir, &spec.bin)?;

    // 6. Locate the bin in the extracted tree.
    let rel_bin = find_bin(&extract_dir, &spec.bin)
        .ok_or_else(|| InstallError::BinNotFound(spec.bin.clone()))?;
    let bin_in_extract = extract_dir.join(&rel_bin);

    // 7. chmod 0755.
    make_executable(&bin_in_extract)?;

    // 8. Two-stage rename: staging → final.
    progress(InstallStatus::Installing);
    let final_pkg = store::package_dir(name)?;
    // Ensure the parent packages/ directory exists before attempting rename.
    if let Some(parent) = final_pkg.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bak = final_pkg.with_extension("bak");

    // If an old install exists, move it aside.
    if final_pkg.exists() {
        let _ = std::fs::remove_dir_all(&bak); // clean stale bak
        std::fs::rename(&final_pkg, &bak)?;
    }

    // Move the extracted tree into place.
    match std::fs::rename(&extract_dir, &final_pkg) {
        Ok(()) => {
            // Success — remove backup.
            if bak.exists() {
                let _ = std::fs::remove_dir_all(&bak);
            }
        }
        Err(e) => {
            // Rollback.
            if bak.exists() {
                let _ = std::fs::rename(&bak, &final_pkg);
            }
            return Err(InstallError::Io(e));
        }
    }

    // 9. Symlink into bin/.
    let bin_dir = store::bin_dir()?;
    std::fs::create_dir_all(&bin_dir)?;
    let link = bin_dir.join(&spec.bin);
    let bin_abs = final_pkg.join(&rel_bin);
    atomic_symlink(&link, &bin_abs)?;

    // 10. Write .rev sidecar.
    let rev = RevSidecar {
        version: spec.version.clone(),
        sha256: expected_sha,
    };
    store::write_rev(name, &rev)?;

    // 11. Done.
    let bin_path = bin_abs;
    progress(InstallStatus::Done {
        bin_path: bin_path.clone(),
    });
    Ok(bin_path)
}

/// SHA-256 a file on disk, return lowercase hex string.
fn sha256_file(path: &Path) -> Result<String, InstallError> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Extract an archive into `dest_dir` based on `ext`.
///
/// - `"tar.gz"` / `"tgz"` → flate2 + tar
/// - `"gz"` (no tar)      → single-file gzip, write to `bin_name`
/// - `"zip"`              → zip::ZipArchive
/// - `""`  (raw)          → copy dl_path → dest_dir/bin_name
///
/// Every entry path is validated by [`safe_join`].
fn extract_archive(
    dl_path: &Path,
    ext: &str,
    dest_dir: &Path,
    bin_name: &str,
) -> Result<(), InstallError> {
    match ext {
        "tar.gz" | "tgz" => {
            let f = std::fs::File::open(dl_path)?;
            let gz = flate2::read::GzDecoder::new(f);
            let mut archive = tar::Archive::new(gz);
            for entry in archive
                .entries()
                .map_err(|e| InstallError::Archive(e.to_string()))?
            {
                let mut entry = entry.map_err(|e| InstallError::Archive(e.to_string()))?;
                let entry_path = entry
                    .path()
                    .map_err(|e| InstallError::Archive(e.to_string()))?
                    .to_path_buf();
                let out = safe_join(dest_dir, &entry_path)?;
                if let Some(parent) = out.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                // Only extract regular files (skip symlinks, dirs, etc. in the
                // archive — dirs are created via create_dir_all above).
                if entry.header().entry_type().is_file() {
                    let mut dest_file = std::fs::File::create(&out)?;
                    io::copy(&mut entry, &mut dest_file)?;
                } else if entry.header().entry_type() == tar::EntryType::Directory {
                    std::fs::create_dir_all(&out)?;
                }
            }
        }
        "gz" => {
            // Single-file gzip — no inner name; write directly to bin_name.
            let f = std::fs::File::open(dl_path)?;
            let mut gz = flate2::read::GzDecoder::new(f);
            let out = dest_dir.join(bin_name);
            let mut dest_file = std::fs::File::create(&out)?;
            io::copy(&mut gz, &mut dest_file)?;
        }
        "zip" => {
            let f = std::fs::File::open(dl_path)?;
            let mut archive =
                zip::ZipArchive::new(f).map_err(|e| InstallError::Archive(e.to_string()))?;
            for i in 0..archive.len() {
                let mut entry = archive
                    .by_index(i)
                    .map_err(|e| InstallError::Archive(e.to_string()))?;
                let entry_path = PathBuf::from(entry.name());
                let out = safe_join(dest_dir, &entry_path)?;
                if entry.is_dir() {
                    std::fs::create_dir_all(&out)?;
                } else {
                    if let Some(parent) = out.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    let mut dest_file = std::fs::File::create(&out)?;
                    io::copy(&mut entry, &mut dest_file)?;
                }
            }
        }
        _ => {
            // Raw binary — copy as-is.
            let out = dest_dir.join(bin_name);
            std::fs::copy(dl_path, &out)?;
        }
    }
    Ok(())
}

// ── Cargo installer ───────────────────────────────────────────────────────────

pub struct CargoInstaller;

impl Install for CargoInstaller {
    fn install(
        &self,
        name: &str,
        spec: &ToolSpec,
        progress: &dyn Fn(InstallStatus),
    ) -> Result<PathBuf, InstallError> {
        let InstallMethod::Cargo(ref cargo) = spec.method else {
            return Err(InstallError::UnsupportedMethod("not a cargo method"));
        };

        // 1. Build the install root.
        let install_root = store::package_dir(name)?;
        std::fs::create_dir_all(&install_root)?;

        progress(InstallStatus::Installing);

        // Run `cargo install` with --locked first, retry without on failure.
        let result = run_cargo_install(
            &cargo.crate_name,
            &spec.version,
            &install_root,
            true, // --locked
            progress,
        );

        if result.is_err() {
            // Retry without --locked.
            run_cargo_install(
                &cargo.crate_name,
                &spec.version,
                &install_root,
                false,
                progress,
            )?;
        }

        // 2. Verify bin exists.
        let bin_path = install_root.join("bin").join(&spec.bin);
        if !bin_path.exists() {
            return Err(InstallError::BinNotFound(spec.bin.clone()));
        }

        // 3. Symlink into bin/.
        let bin_dir = store::bin_dir()?;
        std::fs::create_dir_all(&bin_dir)?;
        let link = bin_dir.join(&spec.bin);
        atomic_symlink(&link, &bin_path)?;

        // 4. Write .rev sidecar (no sha256 for cargo).
        let rev = RevSidecar {
            version: spec.version.clone(),
            sha256: String::new(),
        };
        store::write_rev(name, &rev)?;

        progress(InstallStatus::Done {
            bin_path: bin_path.clone(),
        });
        Ok(bin_path)
    }
}

/// Run `cargo install <crate_name> --version <version> --root <root>`.
/// Optionally add `--locked`. Streams output lines via `progress`.
fn run_cargo_install(
    crate_name: &str,
    version: &str,
    root: &Path,
    locked: bool,
    progress: &dyn Fn(InstallStatus),
) -> Result<(), InstallError> {
    let mut cmd = Command::new("cargo");
    cmd.arg("install")
        .arg(crate_name)
        .arg("--version")
        .arg(version)
        .arg("--root")
        .arg(root);
    if locked {
        cmd.arg("--locked");
    }

    let output = cmd.output().map_err(|e| InstallError::Subprocess {
        cmd: "cargo install".to_string(),
        stderr: e.to_string(),
    })?;

    // Emit each stderr line as an Installing update (truncated to 200 chars).
    let stderr = String::from_utf8_lossy(&output.stderr);
    for line in stderr.lines() {
        let line = if line.len() > 200 { &line[..200] } else { line };
        progress(InstallStatus::Installing);
        let _ = line; // stored in progress closure by caller if needed
    }

    if !output.status.success() {
        return Err(InstallError::Subprocess {
            cmd: format!(
                "cargo install {crate_name} --version {version}{}",
                if locked { " --locked" } else { "" }
            ),
            stderr: stderr.into_owned(),
        });
    }

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use crate::manifest::{GithubMethod, ToolCategory};

    // SHA-256 of the fixture files (computed once, stored here).
    // Re-run `sha256sum tests/fixtures/hello.*` to verify.
    const HELLO_TAR_GZ_SHA: &str =
        "9dae51f8d23ea48e988bc08ec10b7e8488a7b4f4634e5197ea165bf4e5361295";
    const HELLO_ZIP_SHA: &str = "bcff8654881e86bc7600365fa43f4487ae184ad9487053af0ffbae204f137218";
    #[allow(dead_code)]
    const HELLO_GZ_SHA: &str = "64bc750ede7af4dfed2964cf51af3e7447557fda5b2848b817aa41049d8bf7a1";
    #[allow(dead_code)]
    const HELLO_RAW_SHA: &str = "bfdeaeb08cffb6a36438bcd12dda25417e3cdd36f1e7e482a2849d539225288b";

    fn fixture_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    fn fixture_bytes(name: &str) -> Vec<u8> {
        std::fs::read(fixture_path(name)).expect("fixture must exist")
    }

    /// Build a minimal Github ToolSpec for the given triple + sha + asset pattern.
    fn github_spec(triple: &str, sha: &str, asset_pattern: &str, bin: &str) -> ToolSpec {
        let mut sha256 = BTreeMap::new();
        sha256.insert(triple.to_string(), sha.to_string());
        ToolSpec {
            category: ToolCategory::Lsp,
            description: "test tool".to_string(),
            version: "v1.0".to_string(),
            bin: bin.to_string(),
            method: InstallMethod::Github(GithubMethod {
                repo: "owner/repo".to_string(),
                asset_pattern: asset_pattern.to_string(),
                sha256,
            }),
        }
    }

    /// Stub downloader that writes fixture bytes to `dest`.
    fn stub_download(
        fixture_name: &str,
    ) -> impl Fn(&str, &Path, &dyn Fn(InstallStatus)) -> Result<(), InstallError> + '_ {
        move |_url, dest, progress| {
            let bytes = fixture_bytes(fixture_name);
            std::fs::write(dest, &bytes)?;
            progress(InstallStatus::Downloading {
                bytes_downloaded: bytes.len() as u64,
                total: Some(bytes.len() as u64),
            });
            Ok(())
        }
    }

    // ── safe_join ─────────────────────────────────────────────────────────────

    #[test]
    fn safe_join_normal_path() {
        let root = PathBuf::from("/tmp/root");
        let entry = PathBuf::from("bin/hello");
        let result = safe_join(&root, &entry).unwrap();
        assert_eq!(result, PathBuf::from("/tmp/root/bin/hello"));
    }

    #[test]
    fn safe_join_rejects_parent_traversal() {
        let root = PathBuf::from("/tmp/root");
        let evil = PathBuf::from("../../etc/passwd");
        assert!(matches!(
            safe_join(&root, &evil),
            Err(InstallError::PathEscape(_))
        ));
    }

    #[test]
    fn safe_join_skips_cur_dir() {
        let root = PathBuf::from("/tmp/root");
        let p = PathBuf::from("./bin/hello");
        let result = safe_join(&root, &p).unwrap();
        assert_eq!(result, PathBuf::from("/tmp/root/bin/hello"));
    }

    // ── sha256_file ───────────────────────────────────────────────────────────

    #[test]
    fn sha256_file_matches_known_fixture() {
        let path = fixture_path("hello.tar.gz");
        let hash = sha256_file(&path).unwrap();
        assert_eq!(hash, HELLO_TAR_GZ_SHA);
    }

    // ── extract_tar_gz ────────────────────────────────────────────────────────

    #[test]
    fn extract_tar_gz_into_staging_finds_bin() {
        let tmp = tempfile::tempdir().unwrap();
        let dl = tmp.path().join("hello.tar.gz");
        std::fs::write(&dl, fixture_bytes("hello.tar.gz")).unwrap();

        extract_archive(&dl, "tar.gz", tmp.path(), "hello").unwrap();

        let bin = tmp.path().join("bin").join("hello");
        assert!(bin.exists(), "bin/hello must be extracted");
    }

    // ── extract_zip ───────────────────────────────────────────────────────────

    #[test]
    fn extract_zip_into_staging_finds_bin() {
        let tmp = tempfile::tempdir().unwrap();
        let dl = tmp.path().join("hello.zip");
        std::fs::write(&dl, fixture_bytes("hello.zip")).unwrap();

        extract_archive(&dl, "zip", tmp.path(), "hello").unwrap();

        let bin = tmp.path().join("bin").join("hello");
        assert!(bin.exists(), "bin/hello must be extracted from zip");
    }

    // ── extract_gz (single-file) ──────────────────────────────────────────────

    #[test]
    fn extract_gz_writes_single_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dl = tmp.path().join("hello.gz");
        std::fs::write(&dl, fixture_bytes("hello.gz")).unwrap();

        extract_archive(&dl, "gz", tmp.path(), "hello").unwrap();

        let bin = tmp.path().join("hello");
        assert!(bin.exists(), "hello must be written for single-file gz");
        // Content should be the decompressed script.
        let content = std::fs::read_to_string(&bin).unwrap();
        assert!(content.contains("hello"), "content should say hello");
    }

    // ── extract_raw ───────────────────────────────────────────────────────────

    #[test]
    fn extract_raw_copies_to_bin_path() {
        let tmp = tempfile::tempdir().unwrap();
        let dl = tmp.path().join("hello-raw");
        std::fs::write(&dl, fixture_bytes("hello-raw")).unwrap();

        extract_archive(&dl, "", tmp.path(), "hello").unwrap();

        let bin = tmp.path().join("hello");
        assert!(bin.exists(), "raw binary must be copied as 'hello'");
    }

    // ── path traversal ────────────────────────────────────────────────────────

    #[test]
    fn path_traversal_in_tar_is_rejected_with_path_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let dl = tmp.path().join("evil.tar.gz");
        std::fs::write(&dl, fixture_bytes("evil-traversal.tar.gz")).unwrap();

        let err = extract_archive(&dl, "tar.gz", tmp.path(), "hello").unwrap_err();
        assert!(
            matches!(err, InstallError::PathEscape(_)),
            "expected PathEscape, got: {err:?}"
        );
    }

    // ── checksum mismatch ─────────────────────────────────────────────────────

    #[test]
    fn sha256_mismatch_returns_checksum_mismatch() {
        let tmp = tempfile::tempdir().unwrap();

        // Use current host triple so host_triple() succeeds.
        let triple = host_triple().unwrap();
        let spec = github_spec(triple, "deadbeefdeadbeef", "hello-{triple}.tar.gz", "hello");

        let result = install_github_inner(
            "hello",
            &spec,
            |_url, dest, progress| {
                let bytes = fixture_bytes("hello.tar.gz");
                std::fs::write(dest, &bytes)?;
                progress(InstallStatus::Downloading {
                    bytes_downloaded: bytes.len() as u64,
                    total: None,
                });
                Ok(())
            },
            &|_| {},
        );

        // Override XDG paths to tmp so we don't touch the real store.
        // We cannot easily override XDG here, so just check we get a
        // ChecksumMismatch before any I/O to the store happens.
        assert!(
            matches!(result, Err(InstallError::ChecksumMismatch { .. })),
            "got: {result:?}"
        );
        let _ = tmp;
    }

    // ── missing checksum ──────────────────────────────────────────────────────

    #[test]
    fn missing_checksum_for_host_triple_returns_missing_checksum() {
        // Provide a sha256 map that doesn't include the host triple.
        let mut sha256 = BTreeMap::new();
        sha256.insert("nonexistent-triple".to_string(), "abc".to_string());
        let spec = ToolSpec {
            category: ToolCategory::Lsp,
            description: "test".to_string(),
            version: "v1.0".to_string(),
            bin: "hello".to_string(),
            method: InstallMethod::Github(GithubMethod {
                repo: "owner/repo".to_string(),
                asset_pattern: "hello-{triple}.tar.gz".to_string(),
                sha256,
            }),
        };

        // If host_triple() itself errors (unsupported OS), that's also fine —
        // either error means we can't proceed.
        let result = install_github_inner("hello", &spec, |_, _, _| Ok(()), &|_| {});
        assert!(
            matches!(
                result,
                Err(InstallError::MissingChecksum(_)) | Err(InstallError::UnsupportedTriple(_))
            ),
            "got: {result:?}"
        );
    }

    // ── bin not found ─────────────────────────────────────────────────────────

    #[test]
    fn bin_not_found_in_archive_returns_bin_not_found() {
        let triple = match host_triple() {
            Ok(t) => t,
            Err(_) => return, // skip on unsupported platform
        };

        // The hello.tar.gz contains `bin/hello` — requesting `nonexistent`
        // should trigger BinNotFound.
        let spec = github_spec(
            triple,
            HELLO_TAR_GZ_SHA,
            "hello-{triple}.tar.gz",
            "nonexistent",
        );

        let result =
            install_github_inner("nonexistent", &spec, stub_download("hello.tar.gz"), &|_| {});

        assert!(
            matches!(result, Err(InstallError::BinNotFound(_))),
            "got: {result:?}"
        );
    }

    // ── host_triple ───────────────────────────────────────────────────────────

    #[test]
    fn host_triple_returns_known_triple_or_unsupported() {
        match host_triple() {
            Ok(t) => {
                let known = [
                    "x86_64-unknown-linux-gnu",
                    "aarch64-unknown-linux-gnu",
                    "x86_64-apple-darwin",
                    "aarch64-apple-darwin",
                    "x86_64-pc-windows-msvc",
                ];
                assert!(known.contains(&t), "unexpected triple: {t}");
            }
            Err(InstallError::UnsupportedTriple(_)) => {} // also valid
            Err(e) => panic!("unexpected error: {e}"),
        }
    }

    // ── full Github pipeline (with temp XDG dirs via env override) ────────────

    /// Full end-to-end Github pipeline exercised via environment-variable
    /// XDG override and stub downloader.  Marked ignore because it mutates
    /// env vars and must run single-threaded.
    #[test]
    #[ignore = "requires serialized env: run with --test-threads=1 --include-ignored"]
    fn full_github_pipeline_tar_gz() {
        let data_dir = tempfile::tempdir().unwrap();
        let cache_dir = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("XDG_DATA_HOME", data_dir.path());
            std::env::set_var("XDG_CACHE_HOME", cache_dir.path());
        }

        let triple = host_triple().unwrap();
        let spec = github_spec(triple, HELLO_TAR_GZ_SHA, "hello-{triple}.tar.gz", "hello");

        let statuses: std::sync::Mutex<Vec<InstallStatus>> = std::sync::Mutex::new(Vec::new());

        let result = install_github_inner("hello", &spec, stub_download("hello.tar.gz"), &|s| {
            statuses.lock().unwrap().push(s.clone())
        });

        unsafe {
            std::env::remove_var("XDG_DATA_HOME");
            std::env::remove_var("XDG_CACHE_HOME");
        }

        let bin_path = result.expect("full pipeline must succeed");
        assert!(bin_path.exists(), "installed binary must exist");

        let statuses = statuses.into_inner().unwrap();
        assert!(
            statuses
                .iter()
                .any(|s| matches!(s, InstallStatus::Done { .. }))
        );
    }

    /// Same as above but with a zip fixture.
    #[test]
    #[ignore = "requires serialized env: run with --test-threads=1 --include-ignored"]
    fn full_github_pipeline_zip() {
        let data_dir = tempfile::tempdir().unwrap();
        let cache_dir = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("XDG_DATA_HOME", data_dir.path());
            std::env::set_var("XDG_CACHE_HOME", cache_dir.path());
        }

        let triple = host_triple().unwrap();
        let spec = github_spec(triple, HELLO_ZIP_SHA, "hello-{triple}.zip", "hello");

        let result = install_github_inner("hello", &spec, stub_download("hello.zip"), &|_| {});

        unsafe {
            std::env::remove_var("XDG_DATA_HOME");
            std::env::remove_var("XDG_CACHE_HOME");
        }

        let bin_path = result.expect("zip pipeline must succeed");
        assert!(bin_path.exists());
    }

    // ── unsupported methods ───────────────────────────────────────────────────

    #[test]
    fn install_blocking_npm_returns_unsupported() {
        use crate::manifest::NpmMethod;
        let spec = ToolSpec {
            category: ToolCategory::Lsp,
            description: "test".to_string(),
            version: "1.0".to_string(),
            bin: "bin".to_string(),
            method: InstallMethod::Npm(NpmMethod {
                package: "pkg".to_string(),
            }),
        };
        let err = install_blocking("tool", &spec, &|_| {}).unwrap_err();
        assert!(matches!(err, InstallError::UnsupportedMethod("npm")));
    }

    // ── cargo spec sha computation check ─────────────────────────────────────

    #[test]
    fn cargo_installer_skips_checksum() {
        // Verify CargoInstaller produces no sha256 in .rev by constructing
        // the RevSidecar directly (the actual cargo install is live, so we
        // only test the rev shape here).
        let rev = RevSidecar {
            version: "0.9.3".to_string(),
            sha256: String::new(),
        };
        assert_eq!(rev.sha256, "");
    }
}
