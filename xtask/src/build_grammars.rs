//! `cargo xtask build-grammars` — clone + compile every grammar in
//! `bonsai.toml` and lay them out flat as `<out>/<name>.<ext>` so the
//! `GrammarLoader` system/user lookup picks them up directly.
//!
//! Distro maintainers run this once per release per architecture and ship
//! the resulting directory at `/usr/share/hjkl/runtime/grammars/`.
//!
//! The output directory only contains the compiled artifacts. Source
//! clones and the compile cache live in `--work-dir` (default:
//! `$XDG_CACHE_HOME/hjkl/build-grammars/`) so a packager can ship the
//! out dir as-is without any cleanup step.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use hjkl_bonsai::runtime::{GrammarCompiler, LangSpec, Manifest, SourceCache};

const MANIFEST: &str = include_str!("../../bonsai.toml");

pub fn run(args: &[String]) -> Result<()> {
    let opts = Options::parse(args)?;
    let manifest = Manifest::from_toml_str(MANIFEST).context("parse embedded bonsai.toml")?;

    std::fs::create_dir_all(&opts.out_dir)
        .with_context(|| format!("create out dir {}", opts.out_dir.display()))?;
    // Work dir defaults to a persistent location *outside* out_dir so the
    // output stays clean (only .so/.dylib/.dll files) and incremental
    // rebuilds still hit the cache. Override with --work-dir for hermetic
    // builds (e.g. CI: `--work-dir $(mktemp -d)`).
    let work_dir = match opts.work_dir.clone() {
        Some(d) => d,
        None => default_work_dir()?,
    };
    let sources = SourceCache::new(work_dir.join("sources"));
    let compiler = GrammarCompiler::new(work_dir.join("cache"));

    let total = manifest.iter().count();
    let mut built = 0usize;
    let mut copied = 0usize;
    let mut skipped = 0usize;
    let mut failures: Vec<(String, anyhow::Error)> = Vec::new();

    for (name, spec) in manifest.iter() {
        if !opts.want(name) {
            skipped += 1;
            continue;
        }
        match build_one(name, spec, &sources, &compiler, &opts.out_dir) {
            Ok(BuildKind::Built) => {
                built += 1;
                copied += 1;
                println!("  built  {name}");
            }
            Ok(BuildKind::Cached) => {
                copied += 1;
                println!("  cached {name}");
            }
            Err(e) => {
                failures.push((name.to_string(), e));
                println!("  FAIL   {name}");
            }
        }
    }

    println!();
    println!(
        "summary: {copied} laid out (of {total}), {built} built fresh, {skipped} skipped, {} failed",
        failures.len()
    );
    if !failures.is_empty() {
        println!();
        println!("failures:");
        for (name, e) in &failures {
            println!("  {name}: {e:#}");
        }
        bail!("{} grammar(s) failed to build", failures.len());
    }
    Ok(())
}

enum BuildKind {
    Built,
    Cached,
}

fn build_one(
    name: &str,
    spec: &LangSpec,
    sources: &SourceCache,
    compiler: &GrammarCompiler,
    out_dir: &Path,
) -> Result<BuildKind> {
    let dest = out_dir.join(format!("{name}{}", shared_lib_ext()));
    let cached_artifact = compiler.artifact_path(name, spec);
    let already_built = cached_artifact.is_file();

    let source_root = sources
        .acquire(name, spec)
        .with_context(|| format!("acquire source for {name}"))?;
    let so = compiler
        .compile(name, spec, &source_root)
        .with_context(|| format!("compile {name}"))?;
    std::fs::copy(&so, &dest)
        .with_context(|| format!("copy {} -> {}", so.display(), dest.display()))?;

    if already_built {
        Ok(BuildKind::Cached)
    } else {
        Ok(BuildKind::Built)
    }
}

fn default_work_dir() -> Result<PathBuf> {
    let base = if let Some(p) = std::env::var_os("XDG_CACHE_HOME")
        && !p.is_empty()
    {
        PathBuf::from(p)
    } else {
        let home = std::env::var_os("HOME").context("HOME not set")?;
        PathBuf::from(home).join(".cache")
    };
    Ok(base.join("hjkl/build-grammars"))
}

fn shared_lib_ext() -> &'static str {
    if cfg!(target_os = "macos") {
        ".dylib"
    } else if cfg!(target_os = "windows") {
        ".dll"
    } else {
        ".so"
    }
}

#[derive(Debug, Default)]
struct Options {
    out_dir: PathBuf,
    work_dir: Option<PathBuf>,
    only: Option<HashSet<String>>,
    exclude: HashSet<String>,
}

impl Options {
    fn want(&self, name: &str) -> bool {
        if self.exclude.contains(name) {
            return false;
        }
        match &self.only {
            Some(set) => set.contains(name),
            None => true,
        }
    }

    fn parse(args: &[String]) -> Result<Self> {
        let mut opts = Options {
            out_dir: PathBuf::from("build/grammars"),
            ..Default::default()
        };
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--out" | "-o" => {
                    let v = args
                        .get(i + 1)
                        .with_context(|| format!("{} requires a value", args[i]))?;
                    opts.out_dir = PathBuf::from(v);
                    i += 2;
                }
                "--work-dir" => {
                    let v = args
                        .get(i + 1)
                        .with_context(|| format!("{} requires a value", args[i]))?;
                    opts.work_dir = Some(PathBuf::from(v));
                    i += 2;
                }
                "--only" => {
                    let v = args
                        .get(i + 1)
                        .with_context(|| format!("{} requires a value", args[i]))?;
                    opts.only = Some(v.split(',').map(|s| s.trim().to_string()).collect());
                    i += 2;
                }
                "--exclude" => {
                    let v = args
                        .get(i + 1)
                        .with_context(|| format!("{} requires a value", args[i]))?;
                    opts.exclude = v.split(',').map(|s| s.trim().to_string()).collect();
                    i += 2;
                }
                "-h" | "--help" => {
                    println!("{HELP}");
                    std::process::exit(0);
                }
                other => bail!("unknown argument: {other} (try --help)"),
            }
        }
        Ok(opts)
    }
}

const HELP: &str = "\
cargo xtask build-grammars [OPTIONS]

Compile every grammar in bonsai.toml into <out>/<name>.{so|dylib|dll}.
Distro maintainers ship the resulting dir at
/usr/share/hjkl/runtime/grammars/.

The output directory only ever contains the compiled .so/.dylib/.dll
files — sources and the compile cache live elsewhere so packagers can
ship out_dir as-is.

Options:
  -o, --out <dir>      Output directory (default: build/grammars/).
                       Only ever contains <name>.{so,dylib,dll} files.
      --work-dir <dir> Where to clone sources / cache compiled artifacts.
                       Defaults to $XDG_CACHE_HOME/hjkl/build-grammars/
                       (persistent across runs). Pass a fresh tempdir for
                       hermetic CI builds.
      --only <list>    Comma-separated allowlist of language names.
      --exclude <list> Comma-separated denylist of language names.
  -h, --help           Show this help.

Examples:
  cargo xtask build-grammars
  cargo xtask build-grammars --out /tmp/grammars --only rust,python,go
  cargo xtask build-grammars --exclude angular
";
