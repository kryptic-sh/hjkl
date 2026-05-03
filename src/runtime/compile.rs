//! Compile a tree-sitter grammar's C/C++ sources into a shared library.
//!
//! Honors `$CC` / `$CXX` if set, otherwise falls back to `cc` / `c++` on
//! `PATH`. The compiled `<name>.{so|dylib|dll}` is written **in-place
//! inside the source clone** (e.g.
//! `~/.cache/hjkl/grammars/<name>-<rev>/<name>.so`) — sources and their
//! built parser stay together so the cache dir is one self-contained
//! tree per grammar revision. The durable user-data install (the parser
//! that the loader actually picks up across runs) is the
//! [`GrammarLoader`]'s responsibility.
//!
//! `cc-rs` is intentionally avoided: its compiler-discovery path expects
//! build-script environment (OPT_LEVEL, HOST, TARGET, …) we don't have here.
//! For MSVC support down the road we'd reach for it, but Unix compilers are
//! fine driven by hand.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::manifest::LangSpec;

/// Compiles a grammar's C/C++ sources to a shared library inside the
/// grammar's own source-clone directory. Stateless aside from the choice
/// of compiler binary (resolved via `$CC` / `$CXX`).
#[derive(Debug, Clone, Default)]
pub struct GrammarCompiler;

impl GrammarCompiler {
    pub fn new() -> Self {
        Self
    }

    /// Path where the compiled artifact for `(name, source_root)` would
    /// live (whether or not it has been built yet). Matches what
    /// [`Self::compile`] produces.
    pub fn artifact_path(&self, name: &str, source_root: &Path) -> PathBuf {
        source_root.join(format!("{name}{}", shared_lib_ext()))
    }

    /// Compile the grammar at `source_root` into a shared library at
    /// `<source_root>/<name>.<ext>`. Idempotent — returns the existing
    /// artifact path on a hit.
    pub fn compile(&self, name: &str, spec: &LangSpec, source_root: &Path) -> Result<PathBuf> {
        let dest = self.artifact_path(name, source_root);
        if dest.exists() {
            return Ok(dest);
        }

        let staging = source_root.join(format!(
            "{name}.tmp-{}{}",
            std::process::id(),
            shared_lib_ext(),
        ));
        let _ = std::fs::remove_file(&staging);

        match compile_into(spec, source_root, &staging) {
            Ok(()) => {}
            Err(e) => {
                let _ = std::fs::remove_file(&staging);
                return Err(e);
            }
        }

        match std::fs::rename(&staging, &dest) {
            Ok(()) => Ok(dest),
            Err(_) if dest.exists() => {
                let _ = std::fs::remove_file(&staging);
                Ok(dest)
            }
            Err(e) => {
                let _ = std::fs::remove_file(&staging);
                Err(e)
                    .with_context(|| format!("rename {} -> {}", staging.display(), dest.display()))
            }
        }
    }
}

fn compile_into(spec: &LangSpec, source_root: &Path, out_file: &Path) -> Result<()> {
    if spec.c_files.is_empty() {
        bail!("LangSpec has no c_files to compile");
    }

    // Resolve sources + classify C vs C++.
    let mut any_cpp = false;
    let mut sources: Vec<PathBuf> = Vec::with_capacity(spec.c_files.len());
    for f in &spec.c_files {
        let p = source_root.join(f);
        if !p.is_file() {
            bail!("missing source file: {}", p.display());
        }
        if matches!(
            p.extension().and_then(|s| s.to_str()),
            Some("cc" | "cpp" | "cxx" | "C")
        ) {
            any_cpp = true;
        }
        sources.push(p);
    }

    let compiler = pick_compiler(any_cpp);
    let include = source_root.join("src");
    let mut cmd = Command::new(&compiler);
    // Speed > size for parser code; -fPIC required for shared libs on ELF.
    cmd.arg("-O2").arg("-fPIC").arg("-I").arg(&include);
    if any_cpp {
        cmd.arg("-std=c++14");
    } else {
        cmd.arg("-std=c11");
    }
    for src in &sources {
        cmd.arg(src);
    }
    cmd.arg("-shared").arg("-o").arg(out_file);

    let out = cmd
        .output()
        .with_context(|| format!("spawn compiler {compiler}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!(
            "compile failed for {}: {}",
            out_file.display(),
            stderr.trim()
        );
    }
    Ok(())
}

fn pick_compiler(cpp: bool) -> String {
    let env_key = if cpp { "CXX" } else { "CC" };
    if let Some(v) = std::env::var_os(env_key)
        && !v.is_empty()
    {
        return v.to_string_lossy().into_owned();
    }
    if cpp { "c++".into() } else { "cc".into() }
}

pub(super) fn shared_lib_ext() -> &'static str {
    if cfg!(target_os = "macos") {
        ".dylib"
    } else if cfg!(target_os = "windows") {
        ".dll"
    } else {
        ".so"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_spec(rev: &str) -> LangSpec {
        LangSpec {
            git_url: "https://example/repo".into(),
            git_rev: rev.into(),
            subpath: None,
            extensions: vec!["x".into()],
            c_files: vec!["src/parser.c".into()],
            query_dir: "queries".into(),
            source: None,
        }
    }

    #[test]
    fn artifact_path_lives_inside_source_root() {
        let c = GrammarCompiler::new();
        let root = PathBuf::from("/tmp/cache/rust-deadbeef");
        let p = c.artifact_path("rust", &root);
        assert_eq!(p, root.join(format!("rust{}", shared_lib_ext())));
    }

    #[test]
    fn shared_lib_ext_matches_platform() {
        let ext = shared_lib_ext();
        if cfg!(target_os = "linux") {
            assert_eq!(ext, ".so");
        } else if cfg!(target_os = "macos") {
            assert_eq!(ext, ".dylib");
        } else if cfg!(target_os = "windows") {
            assert_eq!(ext, ".dll");
        }
    }

    #[test]
    fn compile_errors_on_missing_source() {
        let tmp = tempfile::tempdir().unwrap();
        let c = GrammarCompiler::new();
        let spec = dummy_spec("deadbeef00000000");
        let bad_root = tmp.path().join("nonexistent");
        let err = c.compile("ghost", &spec, &bad_root).unwrap_err();
        assert!(err.to_string().contains("missing source"), "got: {err:#}");
    }

    /// Real compile against a tiny well-known grammar. `#[ignore]`d so plain
    /// `cargo test` stays offline. Run via:
    /// `cargo test -p hjkl-bonsai -- --ignored`
    #[test]
    #[ignore = "network + compiler: clones tree-sitter-c then builds it"]
    fn compile_real_grammar_end_to_end() {
        use super::super::source::SourceCache;

        let tmp = tempfile::tempdir().unwrap();
        let cache = SourceCache::new(tmp.path().to_path_buf());
        let compiler = GrammarCompiler::new();
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
        let so = compiler.compile("c", &spec, &root).unwrap();
        assert!(so.is_file(), "expected artifact at {}", so.display());
        assert_eq!(so.parent().unwrap(), root);
        let meta = std::fs::metadata(&so).unwrap();
        assert!(meta.len() > 1024, "artifact suspiciously small");

        // Second compile is idempotent.
        let so2 = compiler.compile("c", &spec, &root).unwrap();
        assert_eq!(so, so2);
    }
}
