//! Grammar source acquisition and query-source cache.
//!
//! `SourceCache` clones upstream grammar repos for compilation.
//! `QuerySourceCache` clones (with sparse checkout) the two curated query
//! repos (helix, nvim-treesitter) and resolves `highlights.scm`, expanding
//! `; inherits: foo,bar` chains into a single concatenated file.
//!
//! Strategy mirrors helix's `helix-loader`: shell out to `git`. Avoids
//! dragging in libgit2 and matches the assumption that bonsai consumers have
//! a developer toolchain installed.
//!
//! ⚠️ **Security:** this module **downloads remote code** — it runs `git` to
//! clone the URLs / revisions named in the manifest. That source is then
//! compiled and `dlopen`ed downstream (see [`super::compile`] and
//! [`super::grammar`]), so the manifest's remotes and the transport security
//! of `git` (prefer HTTPS/SSH) are part of the crate's trust boundary. See
//! the crate-root docs for the full model.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
// Grammar names and `; inherits:` targets are joined into cache and query-repo
// paths, so the same "exactly one path segment" guard the anvil store and
// installer use applies here — one implementation, in `hjkl-fs`.
pub use hjkl_fs::is_safe_component;

use super::manifest::{LangSpec, ManifestMeta, QuerySource};
use super::xdg;

/// Populate a private staging directory and publish it to `dest`, with the
/// whole sequence serialised against every other thread *and process* by an
/// exclusive lock on `dest`.
///
/// Both caches in this module live in one shared directory that any number of
/// hjkl processes reach at the same moment — two editors opening different
/// file types on a cold cache, or (how this was found) every test binary
/// `cargo nextest` runs in parallel. Getting that wrong is not theoretical:
/// the caches used to stage every clone at one fixed `<base>/<key>.tmp` and
/// guard it with an in-process mutex, which peers cannot see. The second
/// process to arrive ran `remove_dir_all` on the first one's half-finished
/// clone, and the first one's next `git` invocation died inside a working
/// directory that no longer existed:
///
/// ```text
/// error: cannot open '.git/FETCH_HEAD': No such file or directory
/// fatal: Unable to read current working directory: No such file or directory
/// ```
///
/// Two things close it, and both are needed. The lock is what makes the
/// decide → clone → publish sequence atomic across processes. The
/// pid-suffixed staging name is the belt to its braces: on a filesystem where
/// the advisory lock does not hold (a network mount), two processes still
/// clone into directories they own outright rather than into each other's.
///
/// `populate` is called with the staging path and must fill it; it is only
/// called when `dest` is still missing once the lock is held, so a peer that
/// published while we waited costs nothing but the wait.
fn stage_and_publish<F>(base: &Path, dest: &Path, key: &str, populate: F) -> Result<()>
where
    F: FnOnce(&Path) -> Result<()>,
{
    std::fs::create_dir_all(base)
        .with_context(|| format!("create cache base {}", base.display()))?;

    // `with_lock_exclusive` is io::Result-shaped, so the body's own
    // anyhow::Result comes back nested — hence the two `?`.
    hjkl_fs::with_lock_exclusive(dest, || {
        Ok(stage_and_publish_locked(base, dest, key, populate))
    })
    .with_context(|| format!("lock cache entry {}", dest.display()))?
}

/// The body of [`stage_and_publish`], run while holding the lock on `dest`.
fn stage_and_publish_locked<F>(base: &Path, dest: &Path, key: &str, populate: F) -> Result<()>
where
    F: FnOnce(&Path) -> Result<()>,
{
    // Re-check under the lock: a peer may have published between our caller's
    // miss and our acquiring the lock, and re-cloning on top of it buys
    // nothing.
    if dest.exists() {
        return Ok(());
    }

    let staging = base.join(format!("{key}.tmp-{}", std::process::id()));
    // Only ever our own pid's leftovers, from a run that died mid-clone.
    let _ = std::fs::remove_dir_all(&staging);

    match populate(&staging) {
        Ok(()) => {}
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e);
        }
    }

    super::publish::publish_path(&staging, dest)
        .with_context(|| format!("rename {} -> {}", staging.display(), dest.display()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// SourceCache — grammar compilation trees
// ---------------------------------------------------------------------------

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
    /// same dir (see [`super::compile::GrammarCompiler`]) and then installed
    /// to the durable user-data layer (see [`super::loader::GrammarLoader`]).
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
        // Security: git_rev must be a safe path component — path separators or
        // ".." could escape the cache directory. (M2 audit finding)
        debug_assert!(
            is_safe_component(&spec.git_rev),
            "git_rev contains path separators: {:?}",
            spec.git_rev
        );
        self.base.join(format!("{name}-{}", spec.git_rev))
    }

    /// Resolve `injections.scm` from the grammar source's own `queries/`
    /// directory. Grammar repos (e.g. MDeiml/tree-sitter-markdown) typically
    /// ship `queries/injections.scm` using the standard tree-sitter injection
    /// protocol (`@injection.language` + `@injection.content`).
    ///
    /// This intentionally reads from the **grammar source**, NOT the curated
    /// query repos (helix / nvim-treesitter): those files often use
    /// non-standard predicates (`#set-lang-from-info-string!`) that are
    /// nvim-specific and won't compile with stock tree-sitter.
    ///
    /// Returns `None` when the grammar does not ship `queries/injections.scm`
    /// — normal and not an error. Returns `Err` only on unexpected I/O.
    pub fn injections_path(&self, grammar_source_root: &Path) -> Result<Option<PathBuf>> {
        let injections_path = grammar_source_root.join("queries").join("injections.scm");
        match std::fs::metadata(&injections_path) {
            Ok(_) => Ok(Some(injections_path)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e)
                .with_context(|| format!("stat injections.scm at {}", injections_path.display())),
        }
    }

    /// Clone the grammar source if not already present. Returns the path to
    /// the (possibly nested via `subpath`) grammar directory ready for
    /// compilation.
    ///
    /// Concurrency-safe across threads *and processes*: calls for the same
    /// `(name, rev)` serialise on an exclusive lock on the destination
    /// directory (see [`stage_and_publish`]), so only one clone runs; later
    /// callers re-check `dest.exists()` under the lock and return the
    /// winner's result with no duplicate work. Calls for different grammars
    /// still run in parallel.
    pub fn acquire(&self, name: &str, spec: &LangSpec) -> Result<PathBuf> {
        // `name` is joined into the cache path (`<base>/<name>-<rev>`); reject
        // anything that isn't a single safe component so it can't escape.
        if !is_safe_component(name) {
            bail!("unsafe grammar name {name:?}: must be a single path component");
        }
        if let Some(subpath) = spec.subpath.as_deref()
            && !is_safe_relative_path(subpath)
        {
            bail!("unsafe grammar subpath {subpath:?}");
        }
        let dest = self.source_dir(name, spec);
        if dest.exists() {
            return Ok(grammar_root(&dest, spec));
        }

        let key = format!("{name}-{}", spec.git_rev);
        stage_and_publish(&self.base, &dest, &key, |staging| {
            clone_into(staging, &spec.git_url, &spec.git_rev)
        })?;
        Ok(grammar_root(&dest, spec))
    }
}

/// True if `s` may contain more than one component but still cannot leave the
/// directory it is joined onto: no `..`, no root, no drive prefix.
///
/// Weaker than [`is_safe_component`], deliberately — a query `subpath` is a
/// path into a repo (`queries/rust`), not a single name.
pub fn is_safe_relative_path(s: &str) -> bool {
    !s.is_empty()
        && !Path::new(s).components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn short_rev(rev: &str) -> &str {
    let mut take = rev.len().min(12);
    // Revs are normally ASCII hex, but the manifest is parsed input — back
    // off to a char boundary rather than panicking on a multi-byte rev.
    while !rev.is_char_boundary(take) {
        take -= 1;
    }
    &rev[..take]
}

fn grammar_root(clone_dir: &Path, spec: &LangSpec) -> PathBuf {
    match &spec.subpath {
        Some(s) if !s.is_empty() => clone_dir.join(s),
        _ => clone_dir.to_path_buf(),
    }
}

// ---------------------------------------------------------------------------
// QuerySourceCache — sparse clones of curated query repos
// ---------------------------------------------------------------------------

/// Sparse clones of the helix + nvim-treesitter query repos, shared across
/// all grammar installs. Clone once keyed by `<short-rev>`, reuse for every
/// language.
#[derive(Debug, Clone)]
pub struct QuerySourceCache {
    base: PathBuf,
}

impl QuerySourceCache {
    pub fn new(base: PathBuf) -> Self {
        Self { base }
    }

    pub fn user_default() -> Result<Self> {
        let p = xdg::cache_home()?.join("bonsai/query-sources");
        Ok(Self::new(p))
    }

    /// Ensure the sparse clone for `source` at `rev` is present. Returns the
    /// root of the sparse checkout (the repo root — subdirectories inside are
    /// accessed by callers with the right prefix).
    ///
    /// Concurrency-safe across threads *and processes*: this clone is shared
    /// by every grammar that draws queries from the same Helix /
    /// nvim-treesitter rev, so it is the one path in the cache that unrelated
    /// grammar builds contend on — including builds in *other* hjkl
    /// processes, which the grammar-install lock in
    /// [`super::loader::GrammarLoader::load`] does not cover because that one
    /// is keyed per grammar. [`stage_and_publish`] serialises it on the
    /// destination directory.
    pub fn acquire_source(&self, source: QuerySource, meta: &ManifestMeta) -> Result<PathBuf> {
        let (url, rev) = match source {
            QuerySource::Helix => (meta.helix_repo.as_str(), meta.helix_rev.as_str()),
            QuerySource::NvimTreesitter => (
                meta.nvim_treesitter_repo.as_str(),
                meta.nvim_treesitter_rev.as_str(),
            ),
        };
        let label = match source {
            QuerySource::Helix => "helix",
            QuerySource::NvimTreesitter => "nvim-treesitter",
        };
        let dest = self.base.join(format!("{label}-{rev}"));
        if dest.exists() {
            return Ok(dest);
        }

        let key = format!("{label}-{rev}");
        let sparse_prefix = source.query_prefix();
        stage_and_publish(&self.base, &dest, &key, |staging| {
            sparse_clone_into(staging, url, rev, sparse_prefix)
        })?;
        Ok(dest)
    }

    /// Resolve a fully-expanded `highlights.scm` for `lang_name` from
    /// `source`. `; inherits: foo,bar` chains are expanded by concatenating
    /// ancestor content before descendant content, transitively. The result
    /// is written to a stable path inside the cache and returned.
    pub fn resolve_highlights(
        &self,
        source: QuerySource,
        meta: &ManifestMeta,
        lang_name: &str,
        query_subdir: Option<&str>,
    ) -> Result<PathBuf> {
        // `lang_name` is interpolated into the resolved-query cache filename;
        // reject traversal before it can escape the cache dir.
        if !is_safe_component(lang_name) {
            bail!("unsafe grammar name {lang_name:?}: must be a single path component");
        }
        let repo_root = self.acquire_source(source, meta)?;
        let prefix = source.query_prefix();
        let subdir = query_subdir.unwrap_or(lang_name);
        let resolved_path = self.base.join(format!(
            "{}-{}-{lang_name}.resolved.scm",
            match source {
                QuerySource::Helix => "helix",
                QuerySource::NvimTreesitter => "nvim-treesitter",
            },
            match source {
                QuerySource::Helix => meta.helix_rev.as_str(),
                QuerySource::NvimTreesitter => meta.nvim_treesitter_rev.as_str(),
            },
        ));
        // Already resolved — reuse (idempotent).
        if resolved_path.exists() {
            return Ok(resolved_path);
        }

        let content = resolve_inherits(&repo_root, prefix, subdir, &mut vec![])?;

        // Write via staging + rename so a concurrent resolver that observes
        // `resolved_path.exists()` never reads a truncated/empty query file.
        let staging = self.base.join(format!(
            "{}.tmp-{}",
            resolved_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("resolved.scm"),
            std::process::id(),
        ));
        let _ = std::fs::remove_file(&staging);
        {
            let mut f = std::fs::File::create(&staging)
                .with_context(|| format!("create resolved scm {}", staging.display()))?;
            f.write_all(content.as_bytes())
                .with_context(|| format!("write resolved scm {}", staging.display()))?;
        }
        super::publish::publish_path(&staging, &resolved_path).with_context(|| {
            format!(
                "rename {} -> {}",
                staging.display(),
                resolved_path.display()
            )
        })?;
        Ok(resolved_path)
    }
}

/// Recursively expand `; inherits: a,b,c` directives. `visited` guards
/// against cycles. Returns the fully concatenated query text.
fn resolve_inherits(
    repo_root: &Path,
    prefix: &str,
    lang: &str,
    visited: &mut Vec<String>,
) -> Result<String> {
    // `lang` is joined into the query-repo path (both the requested subdir and
    // every `; inherits:` target recurse through here). Reject traversal so a
    // crafted name / inherits directive can't read `highlights.scm` outside
    // the query repo.
    if !is_safe_component(lang) {
        bail!("unsafe inherits/lang target {lang:?}: must be a single path component");
    }
    if visited.iter().any(|v| v == lang) {
        return Ok(String::new());
    }
    visited.push(lang.to_string());

    let scm_path = repo_root.join(prefix).join(lang).join("highlights.scm");
    if !scm_path.is_file() {
        bail!(
            "highlights.scm not found at {} for lang `{lang}`",
            scm_path.display()
        );
    }
    let raw = std::fs::read_to_string(&scm_path)
        .with_context(|| format!("read {}", scm_path.display()))?;

    // Collect `; inherits: foo,bar` or `; inherits: foo, bar` from first non-
    // empty lines (helix always puts it near the top, but scan all lines to be safe).
    //
    // Two spellings occur in the wild: helix / most nvim-treesitter files use a
    // colon (`; inherits: ecma,jsx`), but a handful of nvim-treesitter files —
    // including `html` (`; inherits html_tags`) — omit it and separate parents
    // with whitespace. Accept both; splitting on comma AND whitespace covers
    // `ecma,jsx` and `html_tags` alike. Missing the no-colon form silently drops
    // html_tags, which is where the default `<script>`→js / `<style>`→css
    // injections live.
    let mut parents: Vec<String> = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        let Some(after) = trimmed
            .strip_prefix(";; inherits")
            .or_else(|| trimmed.strip_prefix("; inherits"))
        else {
            continue;
        };
        // Require a word boundary after `inherits` so `; inheritance ...` (or any
        // stray token) doesn't match. The next char must be `:` or whitespace.
        let rest = match after.strip_prefix(':') {
            Some(r) => r,
            None if after.is_empty() || after.starts_with(char::is_whitespace) => after,
            None => continue,
        };
        for part in rest.split(|c: char| c == ',' || c.is_whitespace()) {
            // helix uses `_typescript` (underscore prefix = "private") and
            // `ecma`. Look them up as-is including the underscore because
            // that IS the directory name.
            let p_raw = part.trim();
            if !p_raw.is_empty() {
                parents.push(p_raw.to_string());
            }
        }
    }

    let mut out = String::new();
    for parent in &parents {
        // Try exact name first, then without leading underscore (private langs).
        let resolved = resolve_inherits(repo_root, prefix, parent, visited)
            .or_else(|_| {
                let stripped = parent.trim_start_matches('_');
                if stripped != parent {
                    resolve_inherits(repo_root, prefix, stripped, visited)
                } else {
                    bail!("no fallback for parent `{parent}`")
                }
            })
            .unwrap_or_default();
        if !resolved.is_empty() {
            out.push_str(&resolved);
            if !out.ends_with('\n') {
                out.push('\n');
            }
        }
    }
    out.push_str(&raw);
    Ok(out)
}

// ---------------------------------------------------------------------------
// git helpers
// ---------------------------------------------------------------------------

/// Reject clone parameters that git would parse as command-line options
/// (argument injection, e.g. a rev of `--upload-pack=<cmd>`). Manifest
/// values are normally trusted, but `Manifest::from_toml_str` is public
/// API — refuse leading-dash values outright.
fn validate_clone_args(url: &str, rev: &str) -> Result<()> {
    if url.is_empty() || url.starts_with('-') {
        bail!("refusing suspicious git url: {url:?}");
    }
    if rev.is_empty() || rev.starts_with('-') {
        bail!("refusing suspicious git rev: {rev:?}");
    }
    if !is_safe_component(rev) {
        bail!("git_rev contains path separators: {rev:?}");
    }
    Ok(())
}

/// Sparse clone: init + enable sparse checkout + fetch single rev + checkout.
/// Only the `sparse_prefix` subtree is materialized on disk.
fn sparse_clone_into(dir: &Path, url: &str, rev: &str, sparse_prefix: &str) -> Result<()> {
    validate_clone_args(url, rev)?;
    std::fs::create_dir_all(dir).with_context(|| format!("create staging {}", dir.display()))?;

    run_git(dir, &["init", "--quiet"])?;
    run_git(dir, &["remote", "add", "origin", url])?;
    run_git(dir, &["sparse-checkout", "init", "--no-cone"])?;
    run_git(dir, &["sparse-checkout", "set", sparse_prefix])?;

    if run_git(dir, &["fetch", "--depth=1", "--quiet", "origin", rev]).is_err() {
        run_git(dir, &["fetch", "--quiet", "origin", rev])
            .with_context(|| format!("fetch {rev} from {url}"))?;
    }

    run_git(dir, &["checkout", "--quiet", "FETCH_HEAD"])
        .with_context(|| format!("checkout {rev}"))?;
    Ok(())
}

/// `git init` + add origin + fetch a single rev + checkout. Tries shallow
/// (`--depth=1`) first, falls back to a full fetch if the server refuses
/// fetching by SHA.
fn clone_into(dir: &Path, url: &str, rev: &str) -> Result<()> {
    validate_clone_args(url, rev)?;
    std::fs::create_dir_all(dir).with_context(|| format!("create staging {}", dir.display()))?;

    run_git(dir, &["init", "--quiet"])?;
    run_git(dir, &["remote", "add", "origin", url])?;

    if run_git(dir, &["fetch", "--depth=1", "--quiet", "origin", rev]).is_err() {
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::manifest::QuerySource;

    fn dummy_spec(rev: &str, subpath: Option<&str>) -> LangSpec {
        LangSpec {
            git_url: "https://example/repo".into(),
            git_rev: rev.into(),
            subpath: subpath.map(String::from),
            extensions: vec!["x".into()],
            c_files: vec!["src/parser.c".into()],
            query_source: QuerySource::Helix,
            query_subdir: None,
            source: None,
        }
    }

    fn dummy_meta() -> ManifestMeta {
        ManifestMeta {
            helix_repo: "https://github.com/helix-editor/helix".into(),
            helix_rev: "aaaa0000bbbb1111cccc2222dddd3333eeee4444".into(),
            nvim_treesitter_repo: "https://github.com/nvim-treesitter/nvim-treesitter".into(),
            nvim_treesitter_rev: "ffff5555aaaa0000bbbb1111cccc2222dddd3333".into(),
        }
    }

    #[test]
    fn short_rev_truncates_to_12() {
        assert_eq!(short_rev("0123456789abcdef"), "0123456789ab");
        assert_eq!(short_rev("abc"), "abc");
    }

    #[test]
    fn short_rev_does_not_panic_on_multibyte_rev() {
        // 12 bytes falls inside the second '€' (3 bytes each) — must back
        // off to a char boundary instead of panicking.
        let rev = "0123456789€€";
        assert_eq!(short_rev(rev), "0123456789");
    }

    #[test]
    fn clone_args_reject_leading_dash() {
        assert!(validate_clone_args("--upload-pack=evil", "deadbeef").is_err());
        assert!(validate_clone_args("https://example/repo", "--upload-pack=evil").is_err());
        assert!(validate_clone_args("", "deadbeef").is_err());
        assert!(validate_clone_args("https://example/repo", "").is_err());
        assert!(validate_clone_args("https://example/repo", "deadbeef").is_ok());
    }

    /// Regression, `grammar tests` CI lane: the caches are reached
    /// concurrently by unrelated *processes* (two editors on a cold cache;
    /// every binary `cargo nextest` runs in parallel), and the staging dance
    /// used to be guarded only by an in-process mutex that peers cannot see.
    ///
    /// Two things must hold, and this asserts both:
    ///
    /// 1. **Mutual exclusion.** `populate` never runs concurrently for one
    ///    `dest` — `peak` is the observable. Without the lock the two threads
    ///    overlap inside the 200ms body and `peak` reaches 2. (The lock in
    ///    `hjkl_fs` is a `flock` plus an in-process wait set, so one process
    ///    with two threads exercises the same gate two processes hit.)
    /// 2. **Private staging.** The two callers are handed *different*
    ///    staging paths, so neither can `remove_dir_all` the other's clone
    ///    even if the lock is not honoured. The old fixed `<key>.tmp` handed
    ///    both the same path, which is what left git running in a directory
    ///    that no longer existed.
    #[test]
    fn stage_and_publish_never_runs_two_populates_at_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};

        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("cache");
        let dest = base.join("nvim-treesitter-deadbeef");

        let live = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let staged = Arc::new(Mutex::new(Vec::new()));

        // Both threads must find `dest` missing when they start, so both
        // actually reach `populate` — a test where the second one short-
        // circuits on the re-check would assert nothing.
        let barrier = Arc::new(std::sync::Barrier::new(2));

        let mut handles = Vec::new();
        for _ in 0..2 {
            let (base, dest) = (base.clone(), dest.clone());
            let (live, peak, staged, barrier) = (
                Arc::clone(&live),
                Arc::clone(&peak),
                Arc::clone(&staged),
                Arc::clone(&barrier),
            );
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                stage_and_publish(&base, &dest, "nvim-treesitter-deadbeef", |staging| {
                    staged.lock().unwrap().push(staging.to_path_buf());
                    let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    std::fs::create_dir_all(staging)?;
                    std::fs::write(staging.join("marker"), b"ok")?;
                    live.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                })
            }));
        }
        for h in handles {
            h.join().unwrap().expect("both publishes succeed");
        }

        assert_eq!(
            peak.load(Ordering::SeqCst),
            1,
            "two populates ran against one destination at the same time"
        );
        let staged = staged.lock().unwrap();
        assert_eq!(staged.len(), 1, "loser must short-circuit on the re-check");
        assert!(
            !staged[0].ends_with("nvim-treesitter-deadbeef.tmp"),
            "staging must be process-private, got {:?}",
            staged[0]
        );
        assert_eq!(std::fs::read(dest.join("marker")).unwrap(), b"ok");
        assert!(!staged[0].exists(), "staging must not survive the publish");
    }

    #[test]
    fn source_dir_format_includes_full_rev() {
        let cache = SourceCache::new(PathBuf::from("/tmp/cache"));
        let spec = dummy_spec("0123456789abcdef00112233", None);
        assert_eq!(
            cache.source_dir("rust", &spec),
            PathBuf::from("/tmp/cache/rust-0123456789abcdef00112233")
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

    #[test]
    fn inherits_chain_resolved_into_single_file() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        // Fake helix layout: runtime/queries/<lang>/highlights.scm
        let prefix = "runtime/queries";
        let ecma_dir = repo.join(prefix).join("ecma");
        let ts_dir = repo.join(prefix).join("typescript");
        std::fs::create_dir_all(&ecma_dir).unwrap();
        std::fs::create_dir_all(&ts_dir).unwrap();
        std::fs::write(ecma_dir.join("highlights.scm"), "(injection.foo)\n").unwrap();
        std::fs::write(
            ts_dir.join("highlights.scm"),
            "; inherits: ecma\n(typescript.bar)\n",
        )
        .unwrap();

        let mut visited = vec![];
        let result = resolve_inherits(&repo, prefix, "typescript", &mut visited).unwrap();
        assert!(
            result.contains("(injection.foo)"),
            "parent ecma content missing: {result}"
        );
        assert!(
            result.contains("(typescript.bar)"),
            "child typescript content missing: {result}"
        );
        // Parent must appear before child.
        let parent_pos = result.find("(injection.foo)").unwrap();
        let child_pos = result.find("(typescript.bar)").unwrap();
        assert!(parent_pos < child_pos, "parent must precede child");
    }

    #[test]
    fn inherits_no_colon_whitespace_separated_resolved() {
        // nvim-treesitter's `html` query writes `; inherits html_tags` (no colon,
        // whitespace-separated). The default `<script>`→js injection lives in
        // html_tags, so dropping this chain kills script highlighting.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let prefix = "queries";
        let tags_dir = repo.join(prefix).join("html_tags");
        let html_dir = repo.join(prefix).join("html");
        std::fs::create_dir_all(&tags_dir).unwrap();
        std::fs::create_dir_all(&html_dir).unwrap();
        std::fs::write(tags_dir.join("injections.scm"), "(script.js)\n").unwrap();
        std::fs::write(
            html_dir.join("injections.scm"),
            "; inherits html_tags\n(html.py)\n",
        )
        .unwrap();

        // resolve_inherits reads `highlights.scm`; exercise the parser directly
        // by feeding a highlights file with the same modeline.
        std::fs::write(tags_dir.join("highlights.scm"), "(script.js)\n").unwrap();
        std::fs::write(
            html_dir.join("highlights.scm"),
            "; inherits html_tags\n(html.py)\n",
        )
        .unwrap();

        let mut visited = vec![];
        let result = resolve_inherits(&repo, prefix, "html", &mut visited).unwrap();
        assert!(
            result.contains("(script.js)"),
            "html_tags parent not chained without colon: {result}"
        );
        assert!(result.contains("(html.py)"), "html child missing: {result}");
    }

    #[test]
    fn inherits_word_boundary_not_matched_by_prefix() {
        // `; inheritance` must NOT be parsed as an inherits directive.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let prefix = "queries";
        let dir = repo.join(prefix).join("lang");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("highlights.scm"),
            "; inheritance notes here\n(lang.id)\n",
        )
        .unwrap();

        let mut visited = vec![];
        // Must resolve fine (no bogus parent lookup) and keep own content.
        let result = resolve_inherits(&repo, prefix, "lang", &mut visited).unwrap();
        assert!(
            result.contains("(lang.id)"),
            "own content missing: {result}"
        );
    }

    // The predicate's own case table lives with the implementation, in
    // `hjkl_fs::path`. What is asserted here is this crate's use of it: an
    // unsafe grammar name must be refused before any clone or I/O happens.
    #[test]
    fn acquire_rejects_unsafe_name() {
        let cache = SourceCache::new(PathBuf::from("/tmp/cache"));
        let spec = dummy_spec("0123456789abcdef00112233", None);
        // Must fail before any clone/IO — no network touched.
        assert!(cache.acquire("../evil", &spec).is_err());
        assert!(cache.acquire("a/b", &spec).is_err());
        assert!(cache.acquire("..", &spec).is_err());
    }

    #[test]
    fn resolve_inherits_rejects_traversal_target() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let prefix = "runtime/queries";
        std::fs::create_dir_all(repo.join(prefix).join("rust")).unwrap();
        std::fs::write(
            repo.join(prefix).join("rust").join("highlights.scm"),
            "(rust.id)\n",
        )
        .unwrap();

        // A directly-requested traversal target must error, not read outside.
        let mut visited = vec![];
        assert!(resolve_inherits(&repo, prefix, "../../../etc", &mut visited).is_err());
    }

    #[test]
    fn resolve_inherits_skips_traversal_parent() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let prefix = "runtime/queries";
        let ts_dir = repo.join(prefix).join("typescript");
        std::fs::create_dir_all(&ts_dir).unwrap();
        // A hostile `; inherits:` directive pointing outside the query subtree.
        std::fs::write(
            ts_dir.join("highlights.scm"),
            "; inherits: ../secret\n(typescript.bar)\n",
        )
        .unwrap();
        // Plant a file at the escape target: repo/prefix/../secret/highlights.scm.
        // Without the traversal guard, `../secret` would resolve and its body
        // would be concatenated into the result.
        let secret_dir = repo.join(prefix).parent().unwrap().join("secret");
        std::fs::create_dir_all(&secret_dir).unwrap();
        std::fs::write(secret_dir.join("highlights.scm"), "(SECRET_LEAKED)\n").unwrap();

        let mut visited = vec![];
        let result = resolve_inherits(&repo, prefix, "typescript", &mut visited).unwrap();
        // The traversal parent is skipped; only the child's own content remains.
        assert!(
            result.contains("(typescript.bar)"),
            "child missing: {result}"
        );
        assert!(
            !result.contains("SECRET_LEAKED"),
            "traversal target file must NOT be read: {result}"
        );
    }

    #[test]
    fn query_source_helix_picks_helix_layout() {
        let tmp = tempfile::tempdir().unwrap();
        // Build a minimal fake helix sparse-clone layout.
        let cache_base = tmp.path().join("query-sources");
        let meta = dummy_meta();
        let label = format!("helix-{}", meta.helix_rev);
        let repo = cache_base.join(&label);
        let qs_dir = repo.join("runtime/queries/rust");
        std::fs::create_dir_all(&qs_dir).unwrap();
        std::fs::write(qs_dir.join("highlights.scm"), "(rust.id) @variable\n").unwrap();

        let qsc = QuerySourceCache::new(cache_base);
        // Pre-seed so acquire_source is skipped (no network in tests).
        let resolved = qsc
            .resolve_highlights(QuerySource::Helix, &meta, "rust", None)
            .unwrap();
        let content = std::fs::read_to_string(&resolved).unwrap();
        assert!(content.contains("(rust.id)"), "got: {content}");
    }

    #[test]
    fn query_source_nvim_used_when_helix_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_base = tmp.path().join("query-sources");
        let meta = dummy_meta();
        let label = format!("nvim-treesitter-{}", meta.nvim_treesitter_rev);
        let repo = cache_base.join(&label);
        let qs_dir = repo.join("queries/go");
        std::fs::create_dir_all(&qs_dir).unwrap();
        std::fs::write(qs_dir.join("highlights.scm"), "(go.func) @function\n").unwrap();

        let qsc = QuerySourceCache::new(cache_base);
        let resolved = qsc
            .resolve_highlights(QuerySource::NvimTreesitter, &meta, "go", None)
            .unwrap();
        let content = std::fs::read_to_string(&resolved).unwrap();
        assert!(content.contains("(go.func)"), "got: {content}");
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
            query_source: QuerySource::Helix,
            query_subdir: None,
            source: None,
        };
        let root = cache.acquire("c", &spec).unwrap();
        assert!(root.join("src/parser.c").is_file());
        let root2 = cache.acquire("c", &spec).unwrap();
        assert_eq!(root, root2);
    }
}
