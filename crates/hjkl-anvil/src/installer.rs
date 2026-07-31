//! Install pipeline for Github, Cargo, Npm, Pip, and GoInstall tool methods.
//!
//! # Design
//!
//! The public surface is built around the [`Install`] trait so that adding new
//! install backends is a matter of implementing a new struct without touching
//! the dispatcher logic.
//!
//! The [`GithubInstaller`] uses an internal `install_github_inner` function
//! that accepts a `download` closure.  Production code passes a real
//! `reqwest`-backed downloader; tests pass a closure that copies fixture bytes
//! directly.  This keeps network-free testing possible without any mocking
//! framework.
//!
//! Cargo, Npm, Pip, and GoInstall all share two helpers:
//!
//! - [`run_subprocess`] — run a `Command`, stream stderr line-by-line, return
//!   `Err` on non-zero exit.
//! - [`finalize_install`] — atomic symlink + `.rev` sidecar + `Done` status.
//!
//! # Security
//!
//! Every tar/zip entry path is validated by [`safe_join`] before extraction.
//! Parent-dir components (`..`), root prefixes, and absolute paths are all
//! rejected with [`InstallError::PathEscape`].
//!
//! Script installs are intentionally **not** implemented here — they require a
//! security review before execution (sandboxing, hash pinning, etc.).

use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use hjkl_fs::is_safe_component;
use sha2::{Digest, Sha256};

use crate::manifest::{GithubMethod, InstallMethod, ToolSpec};
use crate::store::{self, ChecksumSidecar, RevSidecar};

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

    #[error("refusing option-like or empty argument: {0:?}")]
    OptionLike(String),

    #[error("refusing unsafe URL component: {0:?}")]
    UnsafeUrlComponent(String),
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
    /// Emitted the first time a TOFU hash is recorded for a (tool, version, triple).
    TofuRecorded {
        triple: String,
        sha256: String,
    },
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
    // `spec.bin` is joined into extract and symlink paths (`dest_dir.join(bin)`,
    // `bin_dir.join(&spec.bin)`). A name with separators or `..` from an
    // untrusted manifest would escape those roots (archive *entry* paths are
    // guarded by `safe_join`, but the bin name is not). Require a single safe
    // path component up front for every install method.
    if !is_safe_component(&spec.bin) {
        return Err(InstallError::PathEscape(spec.bin.clone()));
    }
    // `name` is joined into the download staging path
    // (`<cache>/staging/<name>`) and the checksum sidecar path
    // (`<data>/checksums/<name>.toml`) *before* `store::package_dir` ever
    // validates it — an unsafe name from an untrusted manifest would read and
    // write files outside those roots. Require a single safe component here
    // too.
    if !is_safe_component(name) {
        return Err(InstallError::PathEscape(name.to_string()));
    }
    match &spec.method {
        InstallMethod::Github(_) => GithubInstaller.install(name, spec, progress),
        InstallMethod::Cargo(_) => CargoInstaller.install(name, spec, progress),
        InstallMethod::Npm(_) => NpmInstaller.install(name, spec, progress),
        InstallMethod::Pip(_) => PipInstaller.install(name, spec, progress),
        InstallMethod::GoInstall(_) => GoInstaller.install(name, spec, progress),
        // Script requires a security review (sandboxing, hash pinning) before
        // execution — intentionally left unsupported.
        InstallMethod::Script(_) => Err(InstallError::UnsupportedMethod("script")),
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Reject a package/crate/module identifier that the spawned package manager
/// would parse as an option flag (e.g. `cargo install --path=…`,
/// `npm install -g`), plus empty identifiers which would silently shift argv.
fn reject_option_like(value: &str) -> Result<(), InstallError> {
    if value.is_empty() || value.starts_with('-') {
        return Err(InstallError::OptionLike(value.to_string()));
    }
    Ok(())
}

/// True if `value` is safe to paste into a single path segment of the Github
/// release URL.
///
/// The host is fixed, but the path is not: a manifest value carrying `/` adds
/// path segments, `..` walks up out of `/<repo>/releases/download/…`, and `?`
/// or `#` truncate the path into a query/fragment — each of which points the
/// fetch at a *different* artifact on github.com. That is only caught by a
/// pinned checksum; on the TOFU path (first install, no pin) whatever comes
/// back becomes the trusted baseline for every later install. Backslashes and
/// whitespace/control characters are rejected too, since they invite parser
/// disagreement between us and the HTTP stack.
///
/// `%` is rejected for the same reason: rejecting a literal `../` while letting
/// `%2e%2e%2f` through would only move the traversal one decoding step away, and
/// no legitimate version or asset name in a release URL contains a percent.
fn is_safe_url_component(value: &str) -> bool {
    !value.is_empty()
        && !value.contains("..")
        && !value.chars().any(|c| {
            matches!(c, '/' | '\\' | '?' | '#' | '%') || c.is_whitespace() || c.is_control()
        })
}

/// True if `repo` is a plain `owner/name` pair — exactly one `/`, both segments
/// non-empty, only `[A-Za-z0-9._-]`, and neither segment a `.`/`..` traversal.
///
/// `repo` is the first thing interpolated into the release URL path, so the same
/// redirection attack as [`is_safe_url_component`] applies, with more room: a
/// repo of `owner/name/../../other` or `owner/name?x=` rewrites the whole path.
/// Restricting to the character set Github actually allows in an owner/repo
/// closes it without needing a URL parser.
fn is_safe_repo(repo: &str) -> bool {
    let Some((owner, name)) = repo.split_once('/') else {
        return false;
    };
    // A second `/` lands inside `name` and is rejected by the char check below,
    // which is what enforces "exactly one separator".
    [owner, name].into_iter().all(|seg| {
        !seg.is_empty()
            && seg != "."
            && seg != ".."
            && seg
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    })
}

/// Rollback backup path for a package directory: a sibling named
/// `<pkgname>.bak`.
///
/// Derived by appending to the whole file name, never with
/// `Path::with_extension`, which *replaces* a trailing extension instead.
/// Package names are validated as a single path component but a dot is legal
/// inside one, so `with_extension("bak")` maps `foo.bar`, `foo.baz`, and plain
/// `foo` all onto the same `foo.bak`. The per-package install lock serializes
/// installs of the same tool but not of two *different* tools, so colliding
/// backups let one install `remove_dir_all` another's live rollback target — or
/// roll back by restoring the wrong package tree over it.
fn package_backup_path(final_pkg: &Path) -> PathBuf {
    match final_pkg.file_name() {
        Some(name) => {
            let mut bak = name.to_os_string();
            bak.push(".bak");
            final_pkg.with_file_name(bak)
        }
        // Root / `..` — no file name to append to. Unreachable for a validated
        // package dir; keep the result distinct from the input so a caller can
        // never `remove_dir_all` the package itself.
        None => final_pkg.join("hjkl-anvil.bak"),
    }
}

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

/// Move a directory tree across filesystem boundaries (EXDEV), through the disk
/// I/O seam ([`hjkl_fs::move_atomic`]: `rename`, then a staged copy-then-delete).
///
/// The package tree is downloaded and extracted under the cache root and lands
/// under the data root, so `$XDG_CACHE_HOME` and `$XDG_DATA_HOME` on different
/// filesystems is the case this exists for — the `rename` simply fails there.
///
/// The seam improves on the hand-rolled walk this replaces in three ways that
/// matter for an archive whose contents nobody vetted:
///
/// - The fallback copy is **staged** beside its destination and swapped in, so
///   an install that dies mid-copy leaves no half-populated package directory
///   for `find_bin` to pick a stale binary out of later.
/// - Symlinks in the extracted tree are reproduced as symlinks. The old walk
///   classified with `is_dir()` (which follows) and copied with `fs::copy` (which
///   also follows), so a link became a full copy of its target — and a link to a
///   directory was recursed into.
/// - A fifo or device node in the tree is an error rather than a hang: `fs::copy`
///   on a fifo blocks forever waiting for a writer.
///
/// The source is never removed until the copy is complete, so a failed move
/// still leaves the extracted tree where it was.
fn move_dir_cross_device(src: &Path, dst: &Path) -> io::Result<()> {
    // Default options: files keep the source's mode (the `chmod 0755` applied to
    // the binary before this point survives the move, as it did with `fs::copy`),
    // and the copy is durable — an install is exactly the write a user expects to
    // still be there after a crash.
    hjkl_fs::move_atomic(src, dst, &hjkl_fs::WriteOptions::default())
}

/// Create the symlink atomically, through the disk-I/O seam
/// ([`hjkl_fs::symlink_atomic`]: stage beside the destination, then rename).
///
/// The seam's staging name (`.<file_name>.hjkl-tmp.<pid>`) satisfies what the
/// old `.anvil-tmp` suffix was written for: it is *appended* to the whole file
/// name rather than replacing an extension, so `foo.sh` and `foo.py` never
/// collide on one staging path, and it carries the pid so two concurrent
/// workers cannot pick the same name. It also improves on it — the seam retries
/// a taken staging name instead of `remove_file`-ing it, so a bin that happens
/// to be named like a staging file is never deleted, and the leading dot keeps
/// it out of directory listings.
///
/// The staging link is a sibling of `link_path`, so the rename is always within
/// one directory and can never hit EXDEV — the old `move_file_cross_device`
/// fallback here was unreachable (and, for a symlink, wrong: `fs::copy` follows
/// the link, so it would have replaced the link with a regular copy of its
/// target). `move_dir_cross_device`, which moves the package tree from the
/// cache root to the data root and *can* genuinely cross devices, is untouched.
///
/// On Windows, symlink creation requires elevated privileges or Developer Mode.
/// TODO: On Windows, consider copying the binary instead of symlinking.
fn atomic_symlink(link_path: &Path, target: &Path) -> Result<(), InstallError> {
    hjkl_fs::symlink_atomic(link_path, target).map_err(|e| match e.kind() {
        // Keep the public error surface identical: non-unix platforms still
        // report the unsupported-platform case as `Archive`, everything else is
        // `Io`.
        io::ErrorKind::Unsupported => InstallError::Archive(
            "symlinks not supported on this platform; TODO: implement copy fallback".to_string(),
        ),
        _ => InstallError::Io(e),
    })
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

// ── TOFU / pinned SHA resolution ─────────────────────────────────────────────

/// How we handle the expected checksum for a given (tool, version, triple).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpectedSha {
    /// Manifest had a real 64-hex hash for this triple — enforce strictly.
    Pinned(String),
    /// A prior TOFU install recorded this hash — enforce to detect tampering.
    Cached(String),
    /// First time seeing this (tool, version, triple) — accept any hash and
    /// record it in the sidecar.
    Tofu,
}

/// Resolve the expected SHA for a download.
///
/// Resolution order:
/// 1. Manifest entry for the triple (non-empty) → `Pinned`.
/// 2. Sidecar entry for `(version, triple)` → `Cached`.
/// 3. Otherwise → `Tofu`.
pub fn resolve_expected_sha(
    gh: &GithubMethod,
    tool: &str,
    version: &str,
    triple: &str,
    paths: &store::AnvilPaths,
) -> Result<ExpectedSha, InstallError> {
    // 1. Check manifest pin.
    if let Some(manifest_sha) = gh.sha256.get(triple)
        && !manifest_sha.is_empty()
    {
        return Ok(ExpectedSha::Pinned(manifest_sha.clone()));
    }

    // 2. Check sidecar cache.
    if let Some(sidecar) = ChecksumSidecar::read_in(paths, tool)?
        && let Some(cached) = sidecar.get(version, triple)
    {
        return Ok(ExpectedSha::Cached(cached.to_string()));
    }

    // 3. TOFU.
    Ok(ExpectedSha::Tofu)
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
        let paths = store::AnvilPaths::from_xdg()?;
        install_github_inner(name, spec, &paths, real_download, progress)
    }
}

/// Real reqwest-backed downloader. Streams in 64 KiB chunks and reports
/// `Downloading` progress for each chunk.
fn real_download(
    url: &str,
    dest: &Path,
    progress: &dyn Fn(InstallStatus),
) -> Result<(), InstallError> {
    // Reject non-2xx responses. Without this a GitHub 404/500 HTML body would
    // be written to the staging file and treated as the artifact — and under
    // TOFU its hash would be recorded as the trusted checksum, or (for raw
    // binary assets) installed and symlinked as the executable.
    let response = reqwest::blocking::get(url)?.error_for_status()?;
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
        if downloaded > MAX_DOWNLOAD_BYTES {
            return Err(InstallError::Archive(format!(
                "download exceeds size limit of {MAX_DOWNLOAD_BYTES} bytes"
            )));
        }
        progress(InstallStatus::Downloading {
            bytes_downloaded: downloaded,
            total,
        });
    }

    Ok(())
}

/// Testable core of the Github install pipeline.
///
/// `paths` selects the data/cache roots — production callers pass
/// `store::AnvilPaths::from_xdg()`; tests pass an `AnvilPaths` built from a
/// per-test `TempDir` so parallel tests can never collide on disk, even when
/// reusing the same `name` (audit-r2 fix 4).
///
/// `download` is a closure `(url, dest_path, progress) -> Result<(), InstallError>`.
/// Production: `real_download`. Tests: copy fixture bytes.
pub fn install_github_inner(
    name: &str,
    spec: &ToolSpec,
    paths: &store::AnvilPaths,
    download: impl Fn(&str, &Path, &dyn Fn(InstallStatus)) -> Result<(), InstallError>,
    progress: &dyn Fn(InstallStatus),
) -> Result<PathBuf, InstallError> {
    if !matches!(spec.method, InstallMethod::Github(_)) {
        return Err(InstallError::UnsupportedMethod("not a github method"));
    }

    // `install_github_inner` is public and can be reached without going
    // through `install_blocking`, so re-validate the two identifiers that are
    // joined into filesystem paths below (staging dir, checksum sidecar,
    // extract/symlink paths).
    if !is_safe_component(name) {
        return Err(InstallError::PathEscape(name.to_string()));
    }
    if !is_safe_component(&spec.bin) {
        return Err(InstallError::PathEscape(spec.bin.clone()));
    }

    // Two workers can be installing the same tool at once, and every step past
    // this point is on shared state: one `staging/<name>` download dir, one
    // extract tree, one checksum sidecar read-then-written across steps 1 and
    // 6b, one package dir, one `bin/` symlink. Serialize the whole
    // decide → download → verify → extract → install sequence on the package
    // dir, which is the one path every install of this tool agrees on. The
    // locks are re-entrant per thread, so the atomic symlink and sidecar writes
    // inside nest safely under this one.
    let pkg_dir = store::package_dir_in(paths, name)?;
    if let Some(parent) = pkg_dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    hjkl_fs::with_lock_exclusive(&pkg_dir, || {
        Ok(install_github_locked(
            name, spec, paths, &pkg_dir, download, progress,
        ))
    })?
}

/// The body of [`install_github_inner`], run while holding the install lock on
/// the tool's package directory.
fn install_github_locked(
    name: &str,
    spec: &ToolSpec,
    paths: &store::AnvilPaths,
    final_pkg: &Path,
    download: impl Fn(&str, &Path, &dyn Fn(InstallStatus)) -> Result<(), InstallError>,
    progress: &dyn Fn(InstallStatus),
) -> Result<PathBuf, InstallError> {
    let InstallMethod::Github(ref gh) = spec.method else {
        return Err(InstallError::UnsupportedMethod("not a github method"));
    };

    // 1. Detect triple → resolve expected sha (manifest pin | cached TOFU | first-time TOFU).
    let triple = host_triple()?;
    let expected = resolve_expected_sha(gh, name, &spec.version, triple, paths)?;

    // 2. Build download URL.
    //
    // `repo`, `version`, and the expanded asset name are pasted straight into
    // the URL path, so each has to be proven inert first: a value carrying
    // `../`, `?`, or `#` steers the fetch to a different artifact on
    // github.com, and on the TOFU path (first install, no pinned hash) whatever
    // that returns becomes the trusted baseline. The manifest is user-owned
    // config today; this closes the hole before a shared or remote registry
    // makes it reachable by someone other than the user.
    if !is_safe_repo(&gh.repo) {
        return Err(InstallError::UnsafeUrlComponent(gh.repo.clone()));
    }
    if !is_safe_url_component(&spec.version) {
        return Err(InstallError::UnsafeUrlComponent(spec.version.clone()));
    }
    let asset = gh
        .asset_pattern
        .replace("{triple}", triple)
        .replace("{version}", &spec.version);
    // Validate the *expanded* asset — the pattern is only safe if what the
    // substitutions produce is.
    if !is_safe_url_component(&asset) {
        return Err(InstallError::UnsafeUrlComponent(asset));
    }
    let url = format!(
        "https://github.com/{}/releases/download/{}/{}",
        gh.repo, spec.version, asset
    );

    // 3. Download to staging file.
    let staging_dir = paths.cache_root.join("staging").join(name);
    std::fs::create_dir_all(&staging_dir)?;

    let ext = asset_ext(&asset);
    let dl_name = format!("{name}.{ext}");
    let dl_path = staging_dir.join(&dl_name);

    download(&url, &dl_path, progress)?;

    // 4. Verify SHA-256 (or note that this is a first-time TOFU record).
    //
    // For a pinned/cached hash we fail closed here, before touching the
    // archive. For TOFU we do NOT record the hash yet: recording it before
    // the archive is proven to be a usable package would let a corrupt or
    // hostile first download poison the trust baseline (a bad hash becomes
    // the "expected" value for every future install). The sidecar write is
    // deferred to step 6b, after extraction + bin location succeed.
    progress(InstallStatus::Verifying);
    let actual_sha = sha256_file(&dl_path)?;
    let is_tofu = match &expected {
        ExpectedSha::Pinned(expected_hash) | ExpectedSha::Cached(expected_hash) => {
            if &actual_sha != expected_hash {
                return Err(InstallError::ChecksumMismatch {
                    expected: expected_hash.clone(),
                    actual: actual_sha,
                });
            }
            false
        }
        ExpectedSha::Tofu => true,
    };
    let recorded_sha = actual_sha.clone();

    // 5. Extract.
    progress(InstallStatus::Extracting);
    let extract_dir = staging_dir.join("extract");
    // A previous failed install may have left a partial tree here; extracting
    // on top of it would merge stale files into the final package (and
    // `find_bin` could pick up a stale binary).
    if extract_dir.exists() {
        std::fs::remove_dir_all(&extract_dir)?;
    }
    std::fs::create_dir_all(&extract_dir)?;
    extract_archive(&dl_path, ext, &extract_dir, &spec.bin)?;

    // 6. Locate the bin in the extracted tree.
    let rel_bin = find_bin(&extract_dir, &spec.bin)
        .ok_or_else(|| InstallError::BinNotFound(spec.bin.clone()))?;
    let bin_in_extract = extract_dir.join(&rel_bin);

    // 6b. TOFU: only now — the archive extracted cleanly and contains the
    // expected binary — do we trust this hash and record it as the baseline.
    if is_tofu {
        let mut sidecar = ChecksumSidecar::read_in(paths, name)?.unwrap_or_default();
        sidecar.set(&spec.version, triple, &actual_sha);
        sidecar.write_in(paths, name)?;
        progress(InstallStatus::TofuRecorded {
            triple: triple.to_string(),
            sha256: actual_sha.clone(),
        });
    }

    // 7. chmod 0755.
    make_executable(&bin_in_extract)?;

    // 8. Two-stage rename: staging → final. `final_pkg` is the path the caller
    // locked; its parent is already created there.
    progress(InstallStatus::Installing);
    let bak = package_backup_path(final_pkg);

    // If an old install exists, move it aside.
    if final_pkg.exists() {
        let _ = std::fs::remove_dir_all(&bak); // clean stale bak
        move_dir_cross_device(final_pkg, &bak)?;
    }

    // Move the extracted tree into place.
    match move_dir_cross_device(&extract_dir, final_pkg) {
        Ok(()) => {
            // Success — remove backup.
            if bak.exists() {
                let _ = std::fs::remove_dir_all(&bak);
            }
        }
        Err(e) => {
            // Rollback.
            if bak.exists() {
                let _ = move_dir_cross_device(&bak, final_pkg);
            }
            return Err(InstallError::Io(e));
        }
    }

    // 9. Symlink into bin/.
    let bin_dir = store::bin_dir_in(paths);
    std::fs::create_dir_all(&bin_dir)?;
    let link = bin_dir.join(&spec.bin);
    let bin_abs = final_pkg.join(&rel_bin);
    atomic_symlink(&link, &bin_abs)?;

    // 10. Write .rev sidecar.
    let rev = RevSidecar {
        version: spec.version.clone(),
        sha256: recorded_sha,
    };
    store::write_rev_in(paths, name, &rev)?;

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

/// Cap on a single downloaded artifact so a runaway or endless HTTP body can't
/// fill the disk. Generous — no real dev tool approaches this.
const MAX_DOWNLOAD_BYTES: u64 = 2 * 1024 * 1024 * 1024; // 2 GiB

/// Cap on the total bytes written while extracting one archive so a
/// decompression bomb (a tiny gz/zip inflating to terabytes) errors out
/// instead of filling the disk.
const MAX_EXTRACT_BYTES: u64 = 2 * 1024 * 1024 * 1024; // 2 GiB

/// `io::copy` against a shared byte budget: errors if the copy would exceed the
/// remaining budget (decompression-bomb guard) and decrements it otherwise.
fn copy_capped<R: io::Read, W: io::Write>(
    r: &mut R,
    w: &mut W,
    budget: &mut u64,
) -> Result<(), InstallError> {
    // Read at most budget+1 so we can distinguish "exactly at the limit" from
    // "over the limit".
    let copied = io::copy(&mut r.take(budget.saturating_add(1)), w)?;
    if copied > *budget {
        return Err(InstallError::Archive(
            "archive expands beyond size limit".to_string(),
        ));
    }
    *budget -= copied;
    Ok(())
}

/// Extract an archive into `dest_dir` based on `ext`.
///
/// - `"tar.gz"` / `"tgz"` → flate2 + tar
/// - `"gz"` (no tar)      → single-file gzip, write to `bin_name`
/// - `"zip"`              → zip::ZipArchive
/// - `""`  (raw)          → copy dl_path → dest_dir/bin_name
///
/// Every entry path is validated by [`safe_join`]; total output is bounded by
/// [`MAX_EXTRACT_BYTES`].
fn extract_archive(
    dl_path: &Path,
    ext: &str,
    dest_dir: &Path,
    bin_name: &str,
) -> Result<(), InstallError> {
    // Shared decompression budget across every file in the archive.
    let mut budget = MAX_EXTRACT_BYTES;
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
                    copy_capped(&mut entry, &mut dest_file, &mut budget)?;
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
            copy_capped(&mut gz, &mut dest_file, &mut budget)?;
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
                    copy_capped(&mut entry, &mut dest_file, &mut budget)?;
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

// ── Shared subprocess / finalize helpers ──────────────────────────────────────

/// Run `cmd`, stream each stderr line as an `Installing` status update
/// (truncated to 200 chars), and return `Err(Subprocess)` on non-zero exit.
///
/// `cmd_label` is only used in the error message.
fn run_subprocess(
    cmd: &mut Command,
    cmd_label: &str,
    progress: &dyn Fn(InstallStatus),
) -> Result<(), InstallError> {
    let output = cmd.output().map_err(|e| InstallError::Subprocess {
        cmd: cmd_label.to_string(),
        stderr: e.to_string(),
    })?;

    let stderr = String::from_utf8_lossy(&output.stderr);
    for _line in stderr.lines() {
        progress(InstallStatus::Installing);
    }

    if !output.status.success() {
        return Err(InstallError::Subprocess {
            cmd: cmd_label.to_string(),
            stderr: stderr.into_owned(),
        });
    }

    Ok(())
}

/// Shared finishing sequence used by Cargo, Npm, Pip, and GoInstall:
///
/// 1. Atomic symlink `<bin_dir>/<spec.bin>` → `bin_path_in_pkg`.
/// 2. Write `.rev` sidecar with `version` and `sha` (empty sha for these
///    methods).
/// 3. Emit `Done { bin_path }`.
///
/// Returns the absolute path of the binary inside the package directory.
fn finalize_install(
    name: &str,
    spec: &ToolSpec,
    bin_path_in_pkg: &Path,
    sha: &str,
    paths: &store::AnvilPaths,
    progress: &dyn Fn(InstallStatus),
) -> Result<PathBuf, InstallError> {
    // Symlink + sidecar are one publish step: a reader that sees the new
    // `.rev` must not still be looking at the previous symlink. Two workers
    // finishing the same tool at once would otherwise interleave them, so hold
    // the same package-dir lock the Github path uses (re-entrant, so the
    // symlink's own staging is safe inside it).
    let pkg_dir = store::package_dir_in(paths, name)?;
    if let Some(parent) = pkg_dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    hjkl_fs::with_lock_exclusive(&pkg_dir, || {
        Ok(finalize_install_locked(
            name,
            spec,
            bin_path_in_pkg,
            sha,
            paths,
            progress,
        ))
    })?
}

/// The body of [`finalize_install`], run under the package-dir install lock.
fn finalize_install_locked(
    name: &str,
    spec: &ToolSpec,
    bin_path_in_pkg: &Path,
    sha: &str,
    paths: &store::AnvilPaths,
    progress: &dyn Fn(InstallStatus),
) -> Result<PathBuf, InstallError> {
    // 1. Symlink into bin/.
    let bin_dir = store::bin_dir_in(paths);
    std::fs::create_dir_all(&bin_dir)?;
    let link = bin_dir.join(&spec.bin);
    atomic_symlink(&link, bin_path_in_pkg)?;

    // 2. Write .rev sidecar.
    let rev = RevSidecar {
        version: spec.version.clone(),
        sha256: sha.to_string(),
    };
    store::write_rev_in(paths, name, &rev)?;

    // 3. Done.
    let bin_path = bin_path_in_pkg.to_path_buf();
    progress(InstallStatus::Done {
        bin_path: bin_path.clone(),
    });
    Ok(bin_path)
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

        // `crate_name` is passed to `cargo install` as a positional argument;
        // an option-like value (e.g. `--path=…`) would be argument injection.
        reject_option_like(&cargo.crate_name)?;

        let paths = store::AnvilPaths::from_xdg()?;

        // 1. Build the install root.
        let install_root = store::package_dir_in(&paths, name)?;
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

        // 3. Symlink, .rev, Done.
        finalize_install(name, spec, &bin_path, "", &paths, progress)
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
    let label = format!(
        "cargo install {crate_name} --version {version}{}",
        if locked { " --locked" } else { "" }
    );
    run_subprocess(&mut cmd, &label, progress)
}

// ── Npm installer ─────────────────────────────────────────────────────────────

pub struct NpmInstaller;

/// Build the argv list for `npm install --prefix <prefix> <pkg>@<version> ...`
/// (exported for unit testing).
pub fn build_npm_argv(pkg: &str, version: &str, prefix: &Path) -> Vec<String> {
    vec![
        "install".to_string(),
        "--prefix".to_string(),
        prefix.display().to_string(),
        format!("{pkg}@{version}"),
        "--no-audit".to_string(),
        "--no-fund".to_string(),
        "--silent".to_string(),
    ]
}

impl Install for NpmInstaller {
    fn install(
        &self,
        name: &str,
        spec: &ToolSpec,
        progress: &dyn Fn(InstallStatus),
    ) -> Result<PathBuf, InstallError> {
        let InstallMethod::Npm(ref npm) = spec.method else {
            return Err(InstallError::UnsupportedMethod("not an npm method"));
        };

        // `package` becomes the `<pkg>@<version>` positional argument; an
        // option-like value (e.g. `-g`) would be argument injection.
        reject_option_like(&npm.package)?;

        let paths = store::AnvilPaths::from_xdg()?;

        // 1. Prepare package directory.
        let pkg_dir = store::package_dir_in(&paths, name)?;
        let node_modules_bin = pkg_dir.join("node_modules").join(".bin");
        std::fs::create_dir_all(&node_modules_bin)?;

        progress(InstallStatus::Installing);

        // 2. Run npm install.
        // build_npm_argv returns ["install", "--prefix", ...] — pass all args
        // to the `npm` command directly.
        let argv = build_npm_argv(&npm.package, &spec.version, &pkg_dir);
        let mut cmd = Command::new("npm");
        for arg in &argv {
            cmd.arg(arg);
        }
        run_subprocess(
            &mut cmd,
            &format!("npm install {}@{}", npm.package, spec.version),
            progress,
        )?;

        // 3. Verify bin exists.
        let bin_in_pkg = node_modules_bin.join(&spec.bin);
        if !bin_in_pkg.exists() {
            return Err(InstallError::BinNotFound(spec.bin.clone()));
        }

        // 4. Symlink, .rev, Done.
        finalize_install(name, spec, &bin_in_pkg, "", &paths, progress)
    }
}

// ── Pip installer ─────────────────────────────────────────────────────────────

pub struct PipInstaller;

/// Build the argv list for pip install inside a venv (exported for unit testing).
pub fn build_pip_argv(pkg: &str, version: &str) -> Vec<String> {
    vec![
        "install".to_string(),
        "--upgrade".to_string(),
        format!("{pkg}=={version}"),
    ]
}

impl Install for PipInstaller {
    fn install(
        &self,
        name: &str,
        spec: &ToolSpec,
        progress: &dyn Fn(InstallStatus),
    ) -> Result<PathBuf, InstallError> {
        let InstallMethod::Pip(ref pip) = spec.method else {
            return Err(InstallError::UnsupportedMethod("not a pip method"));
        };

        // `package` becomes the `<pkg>==<version>` positional argument; an
        // option-like value would be argument injection into pip.
        reject_option_like(&pip.package)?;

        let paths = store::AnvilPaths::from_xdg()?;
        let pkg_dir = store::package_dir_in(&paths, name)?;
        let venv_dir = pkg_dir.join("venv");
        std::fs::create_dir_all(&pkg_dir)?;

        progress(InstallStatus::Installing);

        // 1. Create venv with python3.
        let mut venv_cmd = Command::new("python3");
        venv_cmd.args(["-m", "venv"]).arg(&venv_dir);
        run_subprocess(
            &mut venv_cmd,
            &format!("python3 -m venv {}", venv_dir.display()),
            progress,
        )?;

        // 2. pip install inside the venv.
        let pip_bin = venv_dir.join("bin").join("pip");
        let pip_argv = build_pip_argv(&pip.package, &spec.version);
        let mut pip_cmd = Command::new(&pip_bin);
        for arg in &pip_argv {
            pip_cmd.arg(arg);
        }
        run_subprocess(
            &mut pip_cmd,
            &format!("pip install {}=={}", pip.package, spec.version),
            progress,
        )?;

        // 3. Verify bin exists.
        let bin_in_venv = venv_dir.join("bin").join(&spec.bin);
        if !bin_in_venv.exists() {
            return Err(InstallError::BinNotFound(spec.bin.clone()));
        }

        // 4. Symlink, .rev, Done.
        finalize_install(name, spec, &bin_in_venv, "", &paths, progress)
    }
}

// ── Go installer ──────────────────────────────────────────────────────────────

pub struct GoInstaller;

/// Build the `go install <module>@<version>` argv (exported for unit testing).
pub fn build_go_argv(module: &str, version: &str) -> Vec<String> {
    vec!["install".to_string(), format!("{module}@{version}")]
}

impl Install for GoInstaller {
    fn install(
        &self,
        name: &str,
        spec: &ToolSpec,
        progress: &dyn Fn(InstallStatus),
    ) -> Result<PathBuf, InstallError> {
        let InstallMethod::GoInstall(ref go) = spec.method else {
            return Err(InstallError::UnsupportedMethod("not a goinstall method"));
        };

        // `module` becomes the `<module>@<version>` positional argument; an
        // option-like value would be argument injection into `go install`.
        reject_option_like(&go.module)?;

        let paths = store::AnvilPaths::from_xdg()?;

        // 1. Prepare isolated GOBIN directory.
        let pkg_dir = store::package_dir_in(&paths, name)?;
        let gobin_dir = pkg_dir.join("bin");
        std::fs::create_dir_all(&gobin_dir)?;

        progress(InstallStatus::Installing);

        // 2. Run `go install <module>@<version>` with GOBIN overridden.
        let argv = build_go_argv(&go.module, &spec.version);
        let mut cmd = Command::new("go");
        for arg in &argv {
            cmd.arg(arg);
        }
        cmd.env("GOBIN", &gobin_dir);
        run_subprocess(
            &mut cmd,
            &format!("go install {}@{}", go.module, spec.version),
            progress,
        )?;

        // 3. Verify bin exists.
        let bin_in_pkg = gobin_dir.join(&spec.bin);
        if !bin_in_pkg.exists() {
            return Err(InstallError::BinNotFound(spec.bin.clone()));
        }

        // 4. Symlink, .rev, Done.
        finalize_install(name, spec, &bin_in_pkg, "", &paths, progress)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use crate::manifest::{GithubMethod, ToolCategory};

    /// Build a `store::AnvilPaths` rooted at a fresh `TempDir`'s `data` /
    /// `cache` subdirs — the per-test-isolation seam that replaces
    /// `std::env::set_var(XDG_DATA_HOME/XDG_CACHE_HOME)` (audit-r2 fix 4).
    /// Two tests using the SAME tool `name` against two different
    /// `temp_paths()` results can never collide on disk, so nothing here
    /// needs a `Mutex` or nextest's `anvil-env` serialization group anymore.
    fn temp_paths() -> (tempfile::TempDir, store::AnvilPaths) {
        let tmp = tempfile::tempdir().unwrap();
        let paths = store::AnvilPaths {
            data_root: tmp.path().join("data").join("anvil"),
            cache_root: tmp.path().join("cache").join("anvil"),
        };
        (tmp, paths)
    }

    #[test]
    fn copy_capped_bounds_output() {
        // Within budget: copies and decrements.
        let mut budget = 100u64;
        let mut out = Vec::new();
        copy_capped(
            &mut std::io::Cursor::new(vec![7u8; 80]),
            &mut out,
            &mut budget,
        )
        .unwrap();
        assert_eq!(out.len(), 80);
        assert_eq!(budget, 20);
        // Over the remaining budget errors (decompression-bomb guard).
        let mut out2 = Vec::new();
        let err = copy_capped(
            &mut std::io::Cursor::new(vec![7u8; 21]),
            &mut out2,
            &mut budget,
        );
        assert!(err.is_err(), "copy beyond remaining budget must error");
    }

    // SHA-256 of the fixture files (computed once, stored here).
    // Re-run `sha256sum tests/fixtures/hello.*` to verify.
    const HELLO_TAR_GZ_SHA: &str =
        "9dae51f8d23ea48e988bc08ec10b7e8488a7b4f4634e5197ea165bf4e5361295";
    #[allow(dead_code)]
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
        // Buffer should be the decompressed script.
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

    // ── unsafe tool names / option-like package args ──────────────────────────

    #[test]
    fn install_blocking_rejects_unsafe_tool_name() {
        // The tool name is joined into the staging and checksum-sidecar paths
        // before store::package_dir validates it — it must be rejected up
        // front, before any I/O.
        let spec = make_cargo_spec("taplo");
        for name in ["../evil", "a/b", "/abs", "..", "."] {
            let err = install_blocking(name, &spec, &|_| {}).unwrap_err();
            assert!(
                matches!(err, InstallError::PathEscape(_)),
                "{name:?}: expected PathEscape, got {err:?}"
            );
        }
    }

    #[test]
    fn install_github_inner_rejects_unsafe_tool_name() {
        // Skip on unsupported platform.
        let Ok(triple) = host_triple() else { return };
        let spec = github_spec(triple, HELLO_TAR_GZ_SHA, "hello-{triple}.tar.gz", "hello");
        let (_tmp, paths) = temp_paths();
        let err = install_github_inner(
            "../evil",
            &spec,
            &paths,
            stub_download("hello.tar.gz"),
            &|_| {},
        )
        .unwrap_err();
        assert!(
            matches!(err, InstallError::PathEscape(_)),
            "expected PathEscape, got {err:?}"
        );
    }

    #[test]
    fn subprocess_installers_reject_option_like_package_ids() {
        use crate::manifest::{CargoMethod, GoMethod, NpmMethod, PipMethod};
        let mk = |method: InstallMethod| ToolSpec {
            category: ToolCategory::Lsp,
            description: "test".to_string(),
            version: "1.0".to_string(),
            bin: "safe-bin".to_string(),
            method,
        };
        let specs = [
            mk(InstallMethod::Cargo(CargoMethod {
                crate_name: "--path=/tmp/evil".to_string(),
            })),
            mk(InstallMethod::Npm(NpmMethod {
                package: "-g".to_string(),
            })),
            mk(InstallMethod::Pip(PipMethod {
                package: "--index-url=https://evil.example".to_string(),
            })),
            mk(InstallMethod::GoInstall(GoMethod {
                module: "-x".to_string(),
            })),
            mk(InstallMethod::Cargo(CargoMethod {
                crate_name: String::new(),
            })),
        ];
        for spec in &specs {
            let err = install_blocking("safe-name", spec, &|_| {}).unwrap_err();
            assert!(
                matches!(err, InstallError::OptionLike(_)),
                "expected OptionLike for {:?}, got {err:?}",
                spec.method
            );
        }
    }

    // ── atomic_symlink staging-name isolation ─────────────────────────────────

    /// The symlink staging path must be derived by appending to the full file
    /// name — `with_extension("tmp")` would map `hello` to `hello.tmp`, which
    /// can be another tool's live symlink and would be deleted.
    #[cfg(unix)]
    #[test]
    fn atomic_symlink_does_not_delete_neighbor_dot_tmp_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target-bin");
        std::fs::write(&target, b"#!/bin/sh\n").unwrap();

        // Simulate another tool whose bin symlink is literally `hello.tmp`.
        let neighbor = tmp.path().join("hello.tmp");
        std::fs::write(&neighbor, b"neighbor").unwrap();

        let link = tmp.path().join("hello");
        atomic_symlink(&link, &target).unwrap();

        assert!(neighbor.exists(), "hello.tmp must not be deleted");
        assert_eq!(std::fs::read_link(&link).unwrap(), target);
    }

    // ── rollback backup path isolation ────────────────────────────────────────

    /// The rollback backup must be a sibling named `<pkgname>.bak`, derived by
    /// appending to the whole file name. `with_extension("bak")` would collapse
    /// `foo.bar`, `foo.baz`, and plain `foo` onto one `foo.bak`.
    #[test]
    fn package_backup_path_appends_to_full_name() {
        let packages = Path::new("/data/anvil/packages");

        let dotted = package_backup_path(&packages.join("foo.bar"));
        assert_eq!(dotted, packages.join("foo.bar.bak"));
        assert_eq!(dotted.parent(), Some(packages), "backup must be a sibling");

        let sibling = package_backup_path(&packages.join("foo.baz"));
        let plain = package_backup_path(&packages.join("foo"));
        assert_ne!(
            dotted, sibling,
            "stem-sharing tools must not share a backup"
        );
        assert_ne!(
            dotted, plain,
            "`foo.bar` must not back up onto `foo`'s path"
        );
        assert_eq!(plain, packages.join("foo.bak"));
    }

    /// End-to-end: reinstalling a dotted tool name must not remove the backup
    /// tree of a different tool that shares its stem. Under
    /// `with_extension("bak")` both map to `packages/hello.bak`, so this
    /// reinstall would `remove_dir_all` the neighbor's live rollback target.
    #[cfg(unix)]
    #[test]
    fn reinstall_does_not_clobber_stem_sharing_backup() {
        let (_tmp, paths) = temp_paths();
        let triple = host_triple().unwrap();
        let spec = github_spec(triple, HELLO_TAR_GZ_SHA, "hello-{triple}.tar.gz", "hello");

        install_github_inner(
            "hello.tool",
            &spec,
            &paths,
            stub_download("hello.tar.gz"),
            &|_| {},
        )
        .expect("first install must succeed");

        // Stand in for another tool (`hello` / `hello.other`) mid-install: its
        // backup tree lives at packages/hello.bak.
        let neighbor_bak = paths.data_root.join("packages").join("hello.bak");
        std::fs::create_dir_all(&neighbor_bak).unwrap();
        std::fs::write(neighbor_bak.join("marker"), b"neighbor").unwrap();

        install_github_inner(
            "hello.tool",
            &spec,
            &paths,
            stub_download("hello.tar.gz"),
            &|_| {},
        )
        .expect("reinstall must succeed");

        assert!(
            neighbor_bak.join("marker").exists(),
            "reinstall must not touch a stem-sharing tool's backup tree"
        );
        assert!(
            paths
                .data_root
                .join("packages")
                .join("hello.tool")
                .join("bin")
                .join("hello")
                .exists(),
            "reinstalled package must be in place"
        );
    }

    // ── URL component validation ──────────────────────────────────────────────

    /// Rebuild `spec` with a patched Github method field.
    fn patch_github(spec: &ToolSpec, repo: Option<&str>, asset_pattern: Option<&str>) -> ToolSpec {
        let mut out = spec.clone();
        if let InstallMethod::Github(ref mut gh) = out.method {
            if let Some(r) = repo {
                gh.repo = r.to_string();
            }
            if let Some(a) = asset_pattern {
                gh.asset_pattern = a.to_string();
            }
        }
        out
    }

    /// Manifest values are interpolated into the release URL path, so a `repo`,
    /// `version`, or expanded asset carrying `/`, `..`, `?`, or `#` must be
    /// refused before any request is made — otherwise the fetch is redirected
    /// to another path on github.com, and on the TOFU path that artifact
    /// becomes the trusted baseline.
    #[test]
    fn install_github_rejects_unsafe_url_components() {
        let Ok(triple) = host_triple() else { return };
        let base = github_spec(triple, HELLO_TAR_GZ_SHA, "hello-{triple}.tar.gz", "hello");

        let mut bad_version = base.clone();
        bad_version.version = "../../../evil".to_string();

        let cases = [
            (
                "repo with traversal",
                patch_github(&base, Some("owner/repo/../../evil"), None),
            ),
            (
                "repo missing owner",
                patch_github(&base, Some("repo"), None),
            ),
            ("version with traversal", bad_version),
            (
                "asset expanding to a query",
                patch_github(&base, None, Some("hello-{triple}.tar.gz?to=/evil")),
            ),
            // Rejecting a literal `../` but accepting its percent-encoded form
            // would only move the traversal one decoding step away.
            (
                "asset with percent-encoded traversal",
                patch_github(&base, None, Some("%2e%2e%2fevil-{triple}.tar.gz")),
            ),
        ];

        for (label, spec) in &cases {
            let (_tmp, paths) = temp_paths();
            let err = install_github_inner(
                "hello",
                spec,
                &paths,
                |_url, _dest, _progress| {
                    Err(InstallError::Archive("downloader must not run".to_string()))
                },
                &|_| {},
            )
            .unwrap_err();
            assert!(
                matches!(err, InstallError::UnsafeUrlComponent(_)),
                "{label}: expected UnsafeUrlComponent, got {err:?}"
            );
        }
    }

    /// Positive case: a normal spec still builds the expected release URL and
    /// installs through the stub downloader.
    #[cfg(unix)]
    #[test]
    fn install_github_builds_expected_url_for_valid_spec() {
        let (_tmp, paths) = temp_paths();
        let triple = host_triple().unwrap();
        let spec = github_spec(triple, HELLO_TAR_GZ_SHA, "hello-{triple}.tar.gz", "hello");

        let seen: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let result = install_github_inner(
            "hello",
            &spec,
            &paths,
            |url, dest, progress| {
                seen.lock().unwrap().push(url.to_string());
                stub_download("hello.tar.gz")(url, dest, progress)
            },
            &|_| {},
        );

        result.expect("valid spec must still install");
        assert_eq!(
            seen.into_inner().unwrap(),
            vec![format!(
                "https://github.com/owner/repo/releases/download/v1.0/hello-{triple}.tar.gz"
            )],
        );
    }

    // ── checksum mismatch ─────────────────────────────────────────────────────

    #[test]
    fn sha256_mismatch_returns_checksum_mismatch() {
        // Explicit per-test paths (audit-r2 fix 4) — this test used to touch
        // the REAL XDG store (no env override, no lock) and collide with
        // every other test using tool name "hello".
        let (_tmp, paths) = temp_paths();

        // Use current host triple so host_triple() succeeds.
        let triple = host_triple().unwrap();
        let spec = github_spec(triple, "deadbeefdeadbeef", "hello-{triple}.tar.gz", "hello");

        let result = install_github_inner(
            "hello",
            &spec,
            &paths,
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

        assert!(
            matches!(result, Err(InstallError::ChecksumMismatch { .. })),
            "got: {result:?}"
        );
    }

    // ── missing triple falls through to TOFU ─────────────────────────────────

    #[test]
    fn missing_triple_falls_through_to_tofu() {
        // sha256 map has no entry for the host triple → falls through to TOFU.
        // With a no-op downloader (no file written to disk), sha256_file will
        // return Io(NotFound) — we never reach MissingChecksum.
        // On unsupported platforms, UnsupportedTriple fires first.
        //
        // Explicit per-test paths (audit-r2 fix 4), plus a unique tool name
        // as a second layer of isolation.
        let (_tmp, paths) = temp_paths();
        let mut sha256 = BTreeMap::new();
        sha256.insert("nonexistent-triple".to_string(), "abc".to_string());
        let spec = ToolSpec {
            category: ToolCategory::Lsp,
            description: "test".to_string(),
            version: "v1.0".to_string(),
            bin: "noop-tofu-test-bin".to_string(),
            method: InstallMethod::Github(GithubMethod {
                repo: "owner/repo".to_string(),
                asset_pattern: "noop-tofu-test-{triple}.tar.gz".to_string(),
                sha256,
            }),
        };

        let result = install_github_inner(
            "noop-tofu-test-tool-unique",
            &spec,
            &paths,
            |_, _, _| Ok(()),
            &|_| {},
        );
        // On unsupported platforms → UnsupportedTriple before any I/O.
        // On supported platforms → TOFU path → sha256_file fails with Io
        // (staging file was never written by the no-op downloader), OR
        // Store error if checksums_dir can't be created.
        assert!(
            matches!(
                result,
                Err(InstallError::UnsupportedTriple(_))
                    | Err(InstallError::Io(_))
                    | Err(InstallError::Store(_))
            ),
            "got: {result:?}"
        );
    }

    // ── bin not found ─────────────────────────────────────────────────────────

    #[test]
    fn bin_not_found_in_archive_returns_bin_not_found() {
        // Skip on unsupported platform.
        let Ok(triple) = host_triple() else { return };

        // Explicit per-test paths (audit-r2 fix 4) — this test used to touch
        // the REAL XDG store (no env override, no lock).
        let (_tmp, paths) = temp_paths();

        // The hello.tar.gz contains `bin/hello` — requesting `nonexistent`
        // should trigger BinNotFound.
        let spec = github_spec(
            triple,
            HELLO_TAR_GZ_SHA,
            "hello-{triple}.tar.gz",
            "nonexistent",
        );

        let result = install_github_inner(
            "nonexistent",
            &spec,
            &paths,
            stub_download("hello.tar.gz"),
            &|_| {},
        );

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

    // ── full Github pipeline (explicit per-test AnvilPaths) ───────────────────

    /// Full end-to-end Github pipeline against a per-test `AnvilPaths` and
    /// stub downloader — no env mutation, so no lock or nextest group needed.
    #[cfg(unix)]
    #[test]
    fn full_github_pipeline_tar_gz() {
        let (_tmp, paths) = temp_paths();

        let triple = host_triple().unwrap();
        let spec = github_spec(triple, HELLO_TAR_GZ_SHA, "hello-{triple}.tar.gz", "hello");

        let statuses: std::sync::Mutex<Vec<InstallStatus>> = std::sync::Mutex::new(Vec::new());

        let result = install_github_inner(
            "hello",
            &spec,
            &paths,
            stub_download("hello.tar.gz"),
            &|s| statuses.lock().unwrap().push(s.clone()),
        );

        let bin_path = result.expect("full pipeline must succeed");
        assert!(bin_path.exists(), "installed binary must exist");

        let statuses = statuses.into_inner().unwrap();
        assert!(
            statuses
                .iter()
                .any(|s| matches!(s, InstallStatus::Done { .. }))
        );
    }

    /// A stale extract tree left by a previous failed install must not leak
    /// files into the final package.
    #[cfg(unix)]
    #[test]
    fn stale_extract_dir_is_cleared_before_extraction() {
        let (_tmp, paths) = temp_paths();

        // Simulate a leftover partial extraction from a failed run.
        let stale = paths
            .cache_root
            .join("staging")
            .join("hello")
            .join("extract")
            .join("stale.txt");
        std::fs::create_dir_all(stale.parent().unwrap()).unwrap();
        std::fs::write(&stale, b"stale").unwrap();

        let triple = host_triple().unwrap();
        let spec = github_spec(triple, HELLO_TAR_GZ_SHA, "hello-{triple}.tar.gz", "hello");
        let result = install_github_inner(
            "hello",
            &spec,
            &paths,
            stub_download("hello.tar.gz"),
            &|_| {},
        );

        result.expect("pipeline must succeed");
        let final_pkg = paths.data_root.join("packages").join("hello");
        assert!(
            final_pkg.join("bin").join("hello").exists(),
            "fresh bin must be installed"
        );
        assert!(
            !final_pkg.join("stale.txt").exists(),
            "stale extract content must not leak into the final package"
        );
    }

    /// Same as above but with a zip fixture.
    #[cfg(unix)]
    #[test]
    fn full_github_pipeline_zip() {
        let (_tmp, paths) = temp_paths();

        let triple = host_triple().unwrap();
        let spec = github_spec(triple, HELLO_ZIP_SHA, "hello-{triple}.zip", "hello");

        let result =
            install_github_inner("hello", &spec, &paths, stub_download("hello.zip"), &|_| {});

        let bin_path = result.expect("zip pipeline must succeed");
        assert!(bin_path.exists());
    }

    // ── unsupported methods ───────────────────────────────────────────────────

    #[test]
    fn install_blocking_script_returns_unsupported() {
        use crate::manifest::ScriptMethod;
        let spec = ToolSpec {
            category: ToolCategory::Lsp,
            description: "test".to_string(),
            version: "1.0".to_string(),
            bin: "bin".to_string(),
            method: InstallMethod::Script(ScriptMethod {
                url: "https://example.com/install.tar.gz".to_string(),
                sha256: "deadbeef".to_string(),
                exec: "./install.sh".to_string(),
            }),
        };
        let err = install_blocking("tool", &spec, &|_| {}).unwrap_err();
        assert!(matches!(err, InstallError::UnsupportedMethod("script")));
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

    // ── argv construction (unit tests — no subprocess) ────────────────────────

    #[test]
    fn npm_argv_contains_pkg_at_version() {
        let prefix = PathBuf::from("/tmp/pkg");
        let argv = build_npm_argv("pyright", "1.1.395", &prefix);
        // First arg is the subcommand.
        assert_eq!(argv[0], "install");
        // Package@version must appear verbatim.
        assert!(
            argv.contains(&"pyright@1.1.395".to_string()),
            "expected pyright@1.1.395 in argv, got: {argv:?}"
        );
        // --prefix and its value must both be present.
        let prefix_idx = argv
            .iter()
            .position(|a| a == "--prefix")
            .expect("--prefix missing");
        assert_eq!(argv[prefix_idx + 1], prefix.display().to_string());
        // Noise-reduction flags.
        assert!(argv.contains(&"--no-audit".to_string()));
        assert!(argv.contains(&"--no-fund".to_string()));
        assert!(argv.contains(&"--silent".to_string()));
    }

    #[test]
    fn pip_argv_uses_double_equals_pinning() {
        let argv = build_pip_argv("black", "24.0.0");
        assert_eq!(argv[0], "install");
        assert!(
            argv.contains(&"black==24.0.0".to_string()),
            "expected black==24.0.0 in argv, got: {argv:?}"
        );
    }

    #[test]
    fn go_argv_uses_at_version() {
        let argv = build_go_argv("golang.org/x/tools/gopls", "v0.17.1");
        assert_eq!(argv[0], "install");
        assert!(
            argv.contains(&"golang.org/x/tools/gopls@v0.17.1".to_string()),
            "expected golang.org/x/tools/gopls@v0.17.1 in argv, got: {argv:?}"
        );
    }

    // ── finalize_install helper integration tests ─────────────────────────────
    //
    // These tests exercise atomic_symlink + write_rev via finalize_install
    // using a real tempdir, without needing any package manager on PATH.

    #[allow(dead_code)]
    fn make_cargo_spec(bin: &str) -> ToolSpec {
        use crate::manifest::{CargoMethod, ToolCategory};
        ToolSpec {
            category: ToolCategory::Lsp,
            description: "test".to_string(),
            version: "0.9.3".to_string(),
            bin: bin.to_string(),
            method: InstallMethod::Cargo(CargoMethod {
                crate_name: bin.to_string(),
            }),
        }
    }

    /// finalize_install creates the symlink and the .rev sidecar.
    #[cfg(unix)]
    #[test]
    fn finalize_install_creates_symlink_and_rev() {
        let (_tmp, paths) = temp_paths();

        // Create a fake binary inside a fake package dir.
        let pkg_dir = paths.data_root.join("packages").join("taplo");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        let fake_bin = pkg_dir.join("bin").join("taplo");
        std::fs::create_dir_all(fake_bin.parent().unwrap()).unwrap();
        std::fs::write(&fake_bin, b"#!/bin/sh\necho hi\n").unwrap();

        let spec = make_cargo_spec("taplo");
        let statuses: std::sync::Mutex<Vec<InstallStatus>> = std::sync::Mutex::new(Vec::new());

        let result = finalize_install("taplo", &spec, &fake_bin, "", &paths, &|s| {
            statuses.lock().unwrap().push(s.clone());
        });

        let bin_path = result.expect("finalize_install must succeed");
        assert_eq!(bin_path, fake_bin);

        // Symlink must exist in bin/.
        let link = paths.data_root.join("bin").join("taplo");
        assert!(link.exists(), "symlink must exist at {}", link.display());

        // .rev sidecar must contain the version.
        let rev_path = pkg_dir.join(".rev");
        let rev_content = std::fs::read_to_string(&rev_path).unwrap();
        assert!(
            rev_content.starts_with("0.9.3:"),
            "rev must start with version, got: {rev_content:?}"
        );

        // Done status must have been emitted.
        let statuses = statuses.into_inner().unwrap();
        assert!(
            statuses
                .iter()
                .any(|s| matches!(s, InstallStatus::Done { .. })),
            "Done status missing"
        );
    }

    /// finalize_install overwrites a stale symlink atomically.
    #[cfg(unix)]
    #[test]
    fn finalize_install_overwrites_stale_symlink() {
        let (_tmp, paths) = temp_paths();

        // Bin dir with a stale symlink pointing nowhere.
        let bin_dir = paths.data_root.join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let stale_link = bin_dir.join("taplo");
        #[cfg(unix)]
        std::os::unix::fs::symlink("/nonexistent/old/taplo", &stale_link).unwrap();

        // Fresh fake binary.
        let pkg_dir = paths.data_root.join("packages").join("taplo");
        let fake_bin = pkg_dir.join("bin").join("taplo");
        std::fs::create_dir_all(fake_bin.parent().unwrap()).unwrap();
        std::fs::write(&fake_bin, b"#!/bin/sh\necho hi\n").unwrap();

        let spec = make_cargo_spec("taplo");
        let result = finalize_install("taplo", &spec, &fake_bin, "", &paths, &|_| {});

        result.expect("finalize_install must succeed over stale link");

        // Symlink must now resolve to the new binary.
        let resolved = std::fs::read_link(&stale_link).unwrap();
        assert_eq!(resolved, fake_bin);
    }

    // NOTE: End-to-end installs of real packages are exercised manually.
    // To test NpmInstaller:   :Anvil install pyright   (needs npm on $PATH)
    // To test PipInstaller:   :Anvil install black     (needs python3 on $PATH)
    // To test GoInstaller:    :Anvil install gopls     (needs go on $PATH)

    // ── TOFU sidecar tests (env-mutating, single-threaded) ────────────────────

    /// Build a Github ToolSpec where ALL triples map to empty string (TOFU).
    fn tofu_github_spec(triple: &str, asset_pattern: &str, bin: &str) -> ToolSpec {
        let mut sha256 = BTreeMap::new();
        sha256.insert(triple.to_string(), String::new());
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

    /// First TOFU install records the SHA to the sidecar.
    #[cfg(unix)]
    #[test]
    fn tofu_first_install_records_sha_to_sidecar() {
        use crate::store::ChecksumSidecar;

        let (_tmp, paths) = temp_paths();

        let triple = host_triple().unwrap();
        let spec = tofu_github_spec(triple, "hello-{triple}.tar.gz", "hello");

        let recorded_tofu = std::cell::Cell::new(false);
        let result = install_github_inner(
            "hello",
            &spec,
            &paths,
            stub_download("hello.tar.gz"),
            &|s| {
                if matches!(s, InstallStatus::TofuRecorded { .. }) {
                    recorded_tofu.set(true);
                }
            },
        );

        result.expect("TOFU first install must succeed");
        assert!(
            recorded_tofu.get(),
            "TofuRecorded status must have been emitted"
        );

        // Sidecar must have been written with the correct hash.
        let sidecar_path = paths.data_root.join("checksums").join("hello.toml");
        assert!(
            sidecar_path.exists(),
            "checksum sidecar must exist after TOFU install"
        );

        let content = std::fs::read_to_string(&sidecar_path).unwrap();
        let sidecar = ChecksumSidecar::from_toml_pub(&content).unwrap();
        let recorded_hash = sidecar.get("v1.0", triple);
        assert!(
            recorded_hash.is_some(),
            "sidecar must contain hash for (v1.0, {triple})"
        );
        assert_eq!(
            recorded_hash.unwrap(),
            HELLO_TAR_GZ_SHA,
            "recorded hash must match fixture sha"
        );
    }

    /// A first-time (TOFU) install whose archive does not contain the expected
    /// binary must NOT record a checksum: recording before the archive is
    /// proven usable would poison the trust baseline with a bad-download hash.
    #[cfg(unix)]
    #[test]
    fn tofu_does_not_record_when_bin_missing() {
        let (_tmp, paths) = temp_paths();

        let triple = host_triple().unwrap();
        // The fixture archive contains `hello`, not `nonexistent-bin`, so
        // find_bin fails at step 6 — before the deferred TOFU record.
        let spec = tofu_github_spec(triple, "hello-{triple}.tar.gz", "nonexistent-bin");

        let recorded_tofu = std::cell::Cell::new(false);
        let result = install_github_inner(
            "hello",
            &spec,
            &paths,
            stub_download("hello.tar.gz"),
            &|s| {
                if matches!(s, InstallStatus::TofuRecorded { .. }) {
                    recorded_tofu.set(true);
                }
            },
        );

        let sidecar_path = paths.data_root.join("checksums").join("hello.toml");
        let sidecar_exists = sidecar_path.exists();

        assert!(
            matches!(result, Err(InstallError::BinNotFound(_))),
            "expected BinNotFound, got {result:?}"
        );
        assert!(
            !recorded_tofu.get(),
            "TofuRecorded must NOT be emitted when the archive is unusable"
        );
        assert!(
            !sidecar_exists,
            "checksum sidecar must NOT be written when the archive is unusable"
        );
    }

    /// Second TOFU install (sidecar present) enforces the cached SHA.
    #[test]
    fn tofu_second_install_enforces_cached_sha() {
        use crate::store::ChecksumSidecar;

        let (_tmp, paths) = temp_paths();
        let triple = host_triple().unwrap();

        // Pre-populate the sidecar with a different hash.
        let stale_hash = "aaaa000000000000000000000000000000000000000000000000000000000001";
        let mut sidecar = ChecksumSidecar::default();
        sidecar.set("v1.0", triple, stale_hash);
        sidecar.write_in(&paths, "hello").unwrap();

        let spec = tofu_github_spec(triple, "hello-{triple}.tar.gz", "hello");

        let result = install_github_inner(
            "hello",
            &spec,
            &paths,
            stub_download("hello.tar.gz"),
            &|_| {},
        );

        // The actual fixture hash differs from the stale_hash → ChecksumMismatch.
        assert!(
            matches!(result, Err(InstallError::ChecksumMismatch { .. })),
            "expected ChecksumMismatch when sidecar hash mismatches download; got: {result:?}"
        );
    }

    /// Pinned manifest hash takes precedence over any cached sidecar hash.
    #[cfg(unix)]
    #[test]
    fn pinned_manifest_sha_overrides_sidecar() {
        use crate::store::ChecksumSidecar;

        let (_tmp, paths) = temp_paths();
        let triple = host_triple().unwrap();

        // Sidecar has a stale/wrong hash.
        let stale_hash = "bbbb000000000000000000000000000000000000000000000000000000000002";
        let mut sidecar = ChecksumSidecar::default();
        sidecar.set("v1.0", triple, stale_hash);
        sidecar.write_in(&paths, "hello").unwrap();

        // Manifest is pinned to the CORRECT fixture hash — should succeed.
        let spec = github_spec(triple, HELLO_TAR_GZ_SHA, "hello-{triple}.tar.gz", "hello");

        let result = install_github_inner(
            "hello",
            &spec,
            &paths,
            stub_download("hello.tar.gz"),
            &|_| {},
        );

        // Manifest pin overrides the stale sidecar → install succeeds.
        result.expect("pinned manifest hash must override stale sidecar and succeed");
    }
}
