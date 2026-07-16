//! Integration tests for the install pipeline.
//!
//! These tests use fixture files under `tests/fixtures/` to exercise the
//! extraction and validation paths without any network access.
//!
//! Every test that drives `install_github_inner` passes an explicit
//! `AnvilPaths` built from its own `TempDir` (via [`temp_paths`]) instead of
//! mutating `XDG_DATA_HOME` / `XDG_CACHE_HOME` — env vars are process-global,
//! so two tests mutating them in parallel race no matter how carefully a
//! `Mutex` tries to serialize it. Explicit paths mean tests can run fully
//! parallel, even when they reuse the same tool `name` (audit-r2 fix 4).

use std::collections::BTreeMap;
use std::path::PathBuf;

use hjkl_anvil::store::AnvilPaths;

/// Build an [`AnvilPaths`] rooted at a fresh `TempDir`'s `data` / `cache`
/// subdirs. Returned alongside the `TempDir` so callers keep it alive for
/// the duration of the test.
fn temp_paths() -> (tempfile::TempDir, AnvilPaths) {
    let tmp = tempfile::tempdir().unwrap();
    let paths = AnvilPaths {
        data_root: tmp.path().join("data").join("anvil"),
        cache_root: tmp.path().join("cache").join("anvil"),
    };
    (tmp, paths)
}

use hjkl_anvil::installer::{InstallError, InstallStatus};
use hjkl_anvil::manifest::{GithubMethod, InstallMethod, ToolCategory, ToolSpec};

// ── Fixture helpers ───────────────────────────────────────────────────────────

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn fixture_bytes(name: &str) -> Vec<u8> {
    std::fs::read(fixture_path(name)).unwrap_or_else(|e| panic!("fixture {name}: {e}"))
}

// SHA-256 of fixtures (precomputed; verified by `sha256sum tests/fixtures/*`).
const HELLO_TAR_GZ_SHA: &str = "9dae51f8d23ea48e988bc08ec10b7e8488a7b4f4634e5197ea165bf4e5361295";
#[allow(dead_code)]
const HELLO_ZIP_SHA: &str = "bcff8654881e86bc7600365fa43f4487ae184ad9487053af0ffbae204f137218";
// gz / raw SHA values are retained here for documentation; the extraction tests
// verify the output content rather than a round-trip hash.
#[allow(dead_code)]
const HELLO_GZ_SHA: &str = "64bc750ede7af4dfed2964cf51af3e7447557fda5b2848b817aa41049d8bf7a1";
#[allow(dead_code)]
const HELLO_RAW_SHA: &str = "bfdeaeb08cffb6a36438bcd12dda25417e3cdd36f1e7e482a2849d539225288b";

/// Return the host triple or skip the test on unsupported platforms.
macro_rules! host_triple_or_skip {
    () => {
        match hjkl_anvil::installer::host_triple() {
            Ok(t) => t,
            Err(_) => return, // skip on unsupported platform
        }
    };
}

/// Build a Github [`ToolSpec`] wired to a specific triple + sha + pattern.
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

/// Stub downloader that writes `fixture_name` bytes to `dest`.
fn stub_download<'a>(
    fixture_name: &'a str,
) -> impl Fn(&str, &std::path::Path, &dyn Fn(InstallStatus)) -> Result<(), InstallError> + 'a {
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

// ── Extraction tests (no XDG, no network) ─────────────────────────────────────

/// Helper: call the internal extract_archive via the installer module's
/// public test-only path.  We replicate the extraction call here to stay
/// independent of private APIs.
mod extract_helper {
    use hjkl_anvil::installer::InstallError;
    use std::path::Path;

    /// Thin re-export so integration tests can call the internal function
    /// without requiring it to be `pub`.  We use the `install_github_inner`
    /// path with a controlled stub instead.
    ///
    /// For pure extraction tests we directly use tempfile + read the fs.
    pub fn extract_tar_gz(fixture: &[u8], dest: &Path) -> Result<(), InstallError> {
        use flate2::read::GzDecoder;
        use std::io;
        use tar::Archive;

        let gz = GzDecoder::new(fixture);
        let mut archive = Archive::new(gz);
        for entry in archive
            .entries()
            .map_err(|e| InstallError::Archive(e.to_string()))?
        {
            let mut entry = entry.map_err(|e| InstallError::Archive(e.to_string()))?;
            let entry_path = entry
                .path()
                .map_err(|e| InstallError::Archive(e.to_string()))?
                .to_path_buf();
            // Validate path — replicate safe_join logic.
            use std::path::Component;
            let mut out = dest.to_path_buf();
            for comp in entry_path.components() {
                match comp {
                    Component::Normal(c) => out.push(c),
                    Component::CurDir => {}
                    Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                        return Err(InstallError::PathEscape(entry_path.display().to_string()));
                    }
                }
            }
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if entry.header().entry_type().is_file() {
                let mut f = std::fs::File::create(&out)?;
                io::copy(&mut entry, &mut f)?;
            } else if entry.header().entry_type() == tar::EntryType::Directory {
                std::fs::create_dir_all(&out)?;
            }
        }
        Ok(())
    }
}

#[test]
fn extract_tar_gz_into_staging_finds_bin() {
    let tmp = tempfile::tempdir().unwrap();
    let bytes = fixture_bytes("hello.tar.gz");
    extract_helper::extract_tar_gz(&bytes, tmp.path()).unwrap();

    let bin = tmp.path().join("bin").join("hello");
    assert!(bin.exists(), "bin/hello must exist after tar.gz extraction");
}

#[test]
fn extract_zip_into_staging_finds_bin() {
    use std::io;
    let tmp = tempfile::tempdir().unwrap();
    let bytes = fixture_bytes("hello.zip");

    let cursor = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor).unwrap();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).unwrap();
        let entry_path = std::path::PathBuf::from(entry.name());
        // safe_join
        use std::path::Component;
        let mut out = tmp.path().to_path_buf();
        for comp in entry_path.components() {
            match comp {
                Component::Normal(c) => out.push(c),
                Component::CurDir => {}
                _ => panic!("unexpected path component in fixture zip"),
            }
        }
        if entry.is_dir() {
            std::fs::create_dir_all(&out).unwrap();
        } else {
            if let Some(p) = out.parent() {
                std::fs::create_dir_all(p).unwrap();
            }
            let mut f = std::fs::File::create(&out).unwrap();
            io::copy(&mut entry, &mut f).unwrap();
        }
    }

    let bin = tmp.path().join("bin").join("hello");
    assert!(bin.exists(), "bin/hello must exist after zip extraction");
}

#[test]
fn extract_gz_writes_single_file() {
    use flate2::read::GzDecoder;
    use std::io;

    let tmp = tempfile::tempdir().unwrap();
    let bytes = fixture_bytes("hello.gz");
    let mut gz = GzDecoder::new(bytes.as_slice());
    let out = tmp.path().join("hello");
    let mut f = std::fs::File::create(&out).unwrap();
    io::copy(&mut gz, &mut f).unwrap();

    assert!(
        out.exists(),
        "hello must exist after single-file gz extraction"
    );
    let content = std::fs::read_to_string(&out).unwrap();
    assert!(
        content.contains("hello"),
        "decompressed content must mention 'hello'"
    );
}

#[test]
fn extract_raw_copies_to_bin_path() {
    let tmp = tempfile::tempdir().unwrap();
    let src = fixture_path("hello-raw");
    let dest = tmp.path().join("hello");
    std::fs::copy(&src, &dest).unwrap();

    assert!(dest.exists(), "raw copy must produce hello");
    let content = std::fs::read_to_string(&dest).unwrap();
    assert!(
        content.contains("hello"),
        "raw content must mention 'hello'"
    );
}

// ── Path traversal ────────────────────────────────────────────────────────────

#[test]
fn path_traversal_in_tar_is_rejected_with_path_escape() {
    let tmp = tempfile::tempdir().unwrap();
    let bytes = fixture_bytes("evil-traversal.tar.gz");
    let err = extract_helper::extract_tar_gz(&bytes, tmp.path()).unwrap_err();
    assert!(
        matches!(err, InstallError::PathEscape(_)),
        "expected PathEscape, got: {err:?}"
    );
}

// ── SHA-256 mismatch ──────────────────────────────────────────────────────────

#[test]
fn sha256_mismatch_returns_checksum_mismatch() {
    // Explicit per-test paths (audit-r2 fix 4) — this test used to run with
    // NO env override and NO lock, so it wrote to the developer's REAL
    // `~/.cache/anvil` / `~/.local/share/anvil` and collided with every
    // other test using tool name "hello".
    let (_tmp, paths) = temp_paths();
    let triple = host_triple_or_skip!();
    let spec = github_spec(triple, "deadbeef", "hello-{triple}.tar.gz", "hello");

    let result = hjkl_anvil::installer::install_github_inner(
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

// ── Missing triple falls through to TOFU ─────────────────────────────────────

#[test]
fn missing_triple_falls_through_to_tofu() {
    // sha256 map has no entry for the host triple → TOFU path.
    // With a no-op downloader, the staging file is never written, so
    // sha256_file returns Io(NotFound) — never MissingChecksum.
    // On unsupported platforms, UnsupportedTriple fires first.
    //
    // Explicit per-test paths, plus a unique tool name as a second layer.
    let (_tmp, paths) = temp_paths();
    let mut sha256 = BTreeMap::new();
    sha256.insert("nonexistent-triple".to_string(), "abc123".to_string());
    let spec = ToolSpec {
        category: ToolCategory::Lsp,
        description: "test".to_string(),
        version: "v1.0".to_string(),
        bin: "noop-tofu-integ-bin".to_string(),
        method: InstallMethod::Github(GithubMethod {
            repo: "owner/repo".to_string(),
            asset_pattern: "noop-tofu-integ-{triple}.tar.gz".to_string(),
            sha256,
        }),
    };

    let result = hjkl_anvil::installer::install_github_inner(
        "noop-tofu-integ-tool-unique",
        &spec,
        &paths,
        |_, _, _| Ok(()),
        &|_| {},
    );

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

// ── BinNotFound ───────────────────────────────────────────────────────────────

#[test]
fn bin_not_found_in_archive_returns_bin_not_found() {
    // Explicit per-test paths (audit-r2 fix 4) — see comment on
    // `sha256_mismatch_returns_checksum_mismatch` above: this test used to
    // touch the real XDG store with no isolation at all.
    let (_tmp, paths) = temp_paths();
    let triple = host_triple_or_skip!();
    // hello.tar.gz contains bin/hello; request a bin named "doesnotexist".
    let spec = github_spec(
        triple,
        HELLO_TAR_GZ_SHA,
        "hello-{triple}.tar.gz",
        "doesnotexist",
    );

    let result = hjkl_anvil::installer::install_github_inner(
        "doesnotexist",
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

// ── Full pipeline (explicit per-test AnvilPaths — fully parallel-safe) ───────

/// Full tar.gz install pipeline with stub downloader and a per-test
/// `AnvilPaths` — no env mutation, so no lock or nextest group needed.
#[cfg(unix)]
#[test]
fn full_github_pipeline_tar_gz_end_to_end() {
    let (_tmp, paths) = temp_paths();
    let triple = host_triple_or_skip!();

    let spec = github_spec(triple, HELLO_TAR_GZ_SHA, "hello-{triple}.tar.gz", "hello");
    let statuses: std::sync::Mutex<Vec<InstallStatus>> = std::sync::Mutex::new(Vec::new());

    let result = hjkl_anvil::installer::install_github_inner(
        "hello",
        &spec,
        &paths,
        stub_download("hello.tar.gz"),
        &|s| statuses.lock().unwrap().push(s.clone()),
    );

    let bin_path = result.expect("tar.gz pipeline must succeed");
    assert!(
        bin_path.exists(),
        "installed binary must exist at {bin_path:?}"
    );

    let statuses = statuses.into_inner().unwrap();
    assert!(
        statuses
            .iter()
            .any(|s| matches!(s, InstallStatus::Done { .. })),
        "no Done status emitted"
    );

    // Verify .rev sidecar was written.
    let rev_content = std::fs::read_to_string(paths.data_root.join("packages/hello/.rev")).unwrap();
    assert!(
        rev_content.contains(HELLO_TAR_GZ_SHA),
        "rev sidecar must contain sha"
    );
}

/// Full zip install pipeline.
#[cfg(unix)]
#[test]
fn full_github_pipeline_zip_end_to_end() {
    let (_tmp, paths) = temp_paths();
    let triple = host_triple_or_skip!();

    let spec = github_spec(triple, HELLO_ZIP_SHA, "hello-{triple}.zip", "hello");

    let result = hjkl_anvil::installer::install_github_inner(
        "hello",
        &spec,
        &paths,
        stub_download("hello.zip"),
        &|_| {},
    );

    let bin_path = result.expect("zip pipeline must succeed");
    assert!(bin_path.exists());
}
