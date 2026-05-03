//! Grammar source acquisition.
//!
//! Clones the upstream grammar repo for a [`LangSpec`] into a content-addressed
//! directory keyed by `<name>-<short-rev>`. Idempotent: a second call with the
//! same spec is a no-op once the directory exists.
//!
//! Strategy mirrors helix's `helix-loader`: shell out to `git`. Avoids dragging
//! in libgit2 just for clone+checkout, and matches the assumption that bonsai
//! consumers have a developer toolchain installed.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::manifest::LangSpec;
use super::xdg;

/// Cache of cloned grammar source trees.
#[derive(Debug, Clone)]
pub struct SourceCache {
    base: PathBuf,
}

impl SourceCache {
    /// Wrap an arbitrary base directory. Sources land at
    /// `<base>/<name>-<short-rev>/`. Useful for tests.
    pub fn new(base: PathBuf) -> Self {
        Self { base }
    }

    /// User-default cache rooted at `$XDG_CACHE_HOME/bonsai/grammars/`,
    /// falling back to `~/.cache/bonsai/grammars/` on every platform.
    /// macOS / Windows do *not* use their platform-native cache dirs —
    /// bonsai stores grammar source caches uniformly across platforms.
    ///
    /// Each cloned grammar lives under `<base>/<name>-<short-rev>/`. The
    /// compiled `<name>.{so|dylib|dll}` is built **in-place** inside the
    /// same dir (see [`GrammarCompiler`]) and then installed to the
    /// durable user-data layer (see [`GrammarLoader`]).
    pub fn user_default() -> Result<Self> {
        let p = xdg::cache_home()?.join("bonsai/grammars");
        Ok(Self::new(p))
    }

    /// Root directory of this cache. Created on first acquire.
    pub fn base(&self) -> &Path {
        &self.base
    }

    /// Path where the source tree for `(name, spec)` would live (whether or
    /// not it has been cloned yet).
    pub fn source_dir(&self, name: &str, spec: &LangSpec) -> PathBuf {
        self.base
            .join(format!("{name}-{}", short_rev(&spec.git_rev)))
    }

    /// Clone the grammar source if not already present. Returns the path to
    /// the (possibly nested via `subpath`) grammar directory ready for
    /// compilation.
    ///
    /// Idempotent — a second call is a cheap directory existence check. The
    /// clone is staged in a sibling `<name>-<rev>.tmp/` directory and moved
    /// atomically on success so a partial clone never aliases the final path.
    pub fn acquire(&self, name: &str, spec: &LangSpec) -> Result<PathBuf> {
        let dest = self.source_dir(name, spec);
        if dest.exists() {
            return Ok(grammar_root(&dest, spec));
        }
        std::fs::create_dir_all(&self.base)
            .with_context(|| format!("create cache base {}", self.base.display()))?;

        let staging = self.base.join(format!(
            "{name}-{}.tmp-{}",
            short_rev(&spec.git_rev),
            std::process::id()
        ));
        // Best-effort cleanup of any prior aborted staging dir for this pid.
        let _ = std::fs::remove_dir_all(&staging);

        match clone_into(&staging, &spec.git_url, &spec.git_rev) {
            Ok(()) => {}
            Err(e) => {
                let _ = std::fs::remove_dir_all(&staging);
                return Err(e);
            }
        }

        // Atomic move into place. If we lose a race against a concurrent
        // acquire, treat the now-existing dest as success and drop staging.
        match std::fs::rename(&staging, &dest) {
            Ok(()) => Ok(grammar_root(&dest, spec)),
            Err(_) if dest.exists() => {
                let _ = std::fs::remove_dir_all(&staging);
                Ok(grammar_root(&dest, spec))
            }
            Err(e) => {
                let _ = std::fs::remove_dir_all(&staging);
                Err(e)
                    .with_context(|| format!("rename {} -> {}", staging.display(), dest.display()))
            }
        }
    }
}

fn short_rev(rev: &str) -> &str {
    let take = rev.len().min(12);
    &rev[..take]
}

fn grammar_root(clone_dir: &Path, spec: &LangSpec) -> PathBuf {
    match &spec.subpath {
        Some(s) if !s.is_empty() => clone_dir.join(s),
        _ => clone_dir.to_path_buf(),
    }
}

/// `git init` + add origin + fetch a single rev + checkout. Tries shallow
/// (`--depth=1`) first, falls back to a full fetch if the server refuses
/// fetching by SHA (some hosts don't allow `uploadpack.allowAnySHA1InWant`).
fn clone_into(dir: &Path, url: &str, rev: &str) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("create staging {}", dir.display()))?;

    run_git(dir, &["init", "--quiet"])?;
    run_git(dir, &["remote", "add", "origin", url])?;

    if run_git(dir, &["fetch", "--depth=1", "--quiet", "origin", rev]).is_err() {
        // Fallback: full fetch (no depth) — slower, but works against hosts
        // that disallow fetching arbitrary SHAs shallowly.
        run_git(dir, &["fetch", "--quiet", "origin", rev])
            .with_context(|| format!("fetch {rev} from {url}"))?;
    }

    run_git(dir, &["checkout", "--quiet", "FETCH_HEAD"])
        .with_context(|| format!("checkout {rev}"))?;
    Ok(())
}

fn run_git(cwd: &Path, args: &[&str]) -> Result<()> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("spawn git {}", args.join(" ")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!(
            "git {} failed in {}: {}",
            args.join(" "),
            cwd.display(),
            stderr.trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_spec(rev: &str, subpath: Option<&str>) -> LangSpec {
        LangSpec {
            git_url: "https://example/repo".into(),
            git_rev: rev.into(),
            subpath: subpath.map(String::from),
            extensions: vec!["x".into()],
            c_files: vec!["src/parser.c".into()],
            query_dir: "queries".into(),
            source: None,
        }
    }

    #[test]
    fn short_rev_truncates_to_12() {
        assert_eq!(short_rev("0123456789abcdef"), "0123456789ab");
        assert_eq!(short_rev("abc"), "abc");
    }

    #[test]
    fn source_dir_format_includes_short_rev() {
        let cache = SourceCache::new(PathBuf::from("/tmp/cache"));
        let spec = dummy_spec("0123456789abcdef00112233", None);
        assert_eq!(
            cache.source_dir("rust", &spec),
            PathBuf::from("/tmp/cache/rust-0123456789ab")
        );
    }

    #[test]
    fn grammar_root_honors_subpath() {
        let clone = PathBuf::from("/tmp/cache/typescript-deadbeef0000");
        let spec = dummy_spec("deadbeef00000000", Some("typescript"));
        assert_eq!(grammar_root(&clone, &spec), clone.join("typescript"));
    }

    #[test]
    fn grammar_root_no_subpath_returns_clone_dir() {
        let clone = PathBuf::from("/tmp/cache/rust-deadbeef0000");
        let spec = dummy_spec("deadbeef00000000", None);
        assert_eq!(grammar_root(&clone, &spec), clone);
    }

    /// Real network test against a tiny well-known repo. Kept `#[ignore]`d
    /// so plain `cargo test` stays offline; run with
    /// `cargo test -p hjkl-bonsai -- --ignored` for manual smoke-testing.
    #[test]
    #[ignore = "network: clones from github"]
    fn acquire_clones_real_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = SourceCache::new(tmp.path().to_path_buf());
        let spec = LangSpec {
            git_url: "https://github.com/tree-sitter/tree-sitter-c".into(),
            git_rev: "2a265d69a4caf57108a73ad2ed1e6922dd2f998c".into(),
            subpath: None,
            extensions: vec!["c".into()],
            c_files: vec!["src/parser.c".into()],
            query_dir: "queries".into(),
            source: None,
        };
        let root = cache.acquire("c", &spec).unwrap();
        assert!(root.join("src/parser.c").is_file());
        // Second acquire must be idempotent.
        let root2 = cache.acquire("c", &spec).unwrap();
        assert_eq!(root, root2);
    }
}
