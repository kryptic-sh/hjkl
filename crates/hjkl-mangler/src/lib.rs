//! `hjkl-mangler` — external formatter dispatch for hjkl.
//!
//! Wraps `rustfmt`, `prettier`, `gofmt`, `ruff`, `stylua`, `shfmt`, `taplo`
//! and friends behind a single [`Formatter`] trait. The app calls
//! [`formatter_for_path`] to look up a formatter by file extension, then
//! either calls [`Formatter::format`] synchronously (blocking up to 30 s)
//! or submits a [`FormatJob`] to a [`FormatWorker`] for async dispatch.
//!
//! # Timeout
//!
//! [`Formatter::format`] blocks the calling thread for at most 30 seconds.
//! The implementation polls [`std::process::Child::try_wait`] in a tight
//! spin-loop with 5 ms sleeps. This is intentionally simple.
//!
//! # Async dispatch
//!
//! [`FormatWorker`] moves formatter invocations off the UI thread. Construct
//! one via [`FormatWorker::spawn`], submit jobs via [`FormatWorker::submit`],
//! and drain results each event-loop tick via [`FormatWorker::try_recv`].
//! Per-buffer deduplication ensures that repeated `=` presses while a slow
//! formatter is still running replace the pending job rather than enqueue N.
//!
//! # Adding a formatter
//!
//! 1. Implement [`Formatter`] (or reuse [`StdinFormatter`]).
//! 2. Add an entry to [`formatter_for_path`].

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Maximum time we wait for a formatter subprocess before giving up.
/// 30 s is generous enough for rustfmt on very large files (10k+ LOC)
/// without making a hung formatter feel permanent.
const FORMAT_TIMEOUT: Duration = Duration::from_secs(30);

/// Poll interval inside the wait loop.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors returned by [`Formatter::format`].
#[derive(Debug)]
pub enum FormatError {
    /// Tool is not installed / not on `PATH`. Carries the tool name.
    NotInstalled(String),
    /// Formatter exceeded the [`FORMAT_TIMEOUT`].
    Timeout,
    /// Formatter exited with non-zero status. Carries captured stderr text.
    SyntaxError(String),
    /// I/O error while spawning or communicating with the subprocess.
    Io(std::io::Error),
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FormatError::NotInstalled(name) => write!(f, "{name}: not installed"),
            FormatError::Timeout => write!(f, "formatter timed out (>30 s)"),
            FormatError::SyntaxError(msg) => write!(f, "formatter error: {msg}"),
            FormatError::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for FormatError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FormatError::Io(e) => Some(e),
            _ => None,
        }
    }
}

// ── Formatter trait ───────────────────────────────────────────────────────────

/// Formats whole-file source by invoking an external subprocess.
///
/// Implementations are expected to:
/// - Spawn the tool as a child process with `cwd = project_root`.
/// - Pipe `source` to the child's stdin.
/// - Read the formatted result from stdout.
/// - Return [`FormatError::Timeout`] if the child does not complete within 30 s.
/// - Return [`FormatError::SyntaxError`] on non-zero exit status.
/// - Return [`FormatError::NotInstalled`] when `spawn` returns `NotFound`.
///
/// **This call blocks the calling thread.** Do not invoke on a UI thread
/// without wrapping in a background task. Async dispatch is tracked in #118.
pub trait Formatter: Send + Sync {
    /// Format whole-file `source`. Returns formatted bytes or an error.
    ///
    /// `project_root` is used as the working directory so formatters that
    /// walk up looking for config files (e.g. `prettier`, `rustfmt`) find
    /// the project's config.
    fn format(&self, source: &str, project_root: &Path) -> Result<String, FormatError>;
}

// ── Shared subprocess helper ──────────────────────────────────────────────────

/// A formatter that pipes stdin → stdout.
///
/// `args[0]` is the program name; `args[1..]` are its arguments.
/// The caller supplies `args` as a `&'static [&'static str]` so no allocation
/// is needed per call.
///
/// For prettier-style `--stdin-filepath`, the path must be supplied at
/// format-call time. Use [`PrettierFormatter`] instead which accepts the
/// buffer path dynamically.
pub struct StdinFormatter {
    /// Program + static args (e.g. `["rustfmt", "--emit", "stdout"]`).
    pub args: &'static [&'static str],
    /// Human-readable tool name for error messages.
    pub tool_name: &'static str,
}

impl Formatter for StdinFormatter {
    fn format(&self, source: &str, project_root: &Path) -> Result<String, FormatError> {
        run_formatter(self.tool_name, self.args, &[], source, project_root)
    }
}

/// A formatter that injects the buffer file path as an extra argument.
///
/// Used for `prettier --stdin-filepath <path>` and similar tools where the
/// path affects which config is applied.
pub struct FormatterWithPath {
    /// Base program + args (e.g. `["prettier", "--stdin-filepath"]`).
    pub base_args: &'static [&'static str],
    /// The file path to append as the last argument.
    pub file_path: std::path::PathBuf,
    /// Human-readable tool name.
    pub tool_name: &'static str,
}

impl Formatter for FormatterWithPath {
    fn format(&self, source: &str, project_root: &Path) -> Result<String, FormatError> {
        let path_arg = self.file_path.to_string_lossy().into_owned();
        run_formatter(
            self.tool_name,
            self.base_args,
            &[path_arg.as_str()],
            source,
            project_root,
        )
    }
}

/// Rust-specific formatter — invokes `rustfmt` with `--edition` set from the
/// project's `Cargo.toml`. Necessary because `rustfmt` reading from stdin
/// can't auto-discover `rustfmt.toml` (no file path → no config search root)
/// and defaults to edition 2015, which rejects modern syntax (let chains,
/// async closures, etc).
///
/// Resolution order for the edition:
/// 1. `[package].edition` in the nearest `Cargo.toml` (walks up `project_root`).
/// 2. `[workspace.package].edition` if no package edition.
/// 3. Defaults to `2024` — the current stable edition.
pub struct RustFormatter;

impl Formatter for RustFormatter {
    fn format(&self, source: &str, project_root: &Path) -> Result<String, FormatError> {
        let edition = detect_rust_edition(project_root).unwrap_or_else(|| "2024".to_string());
        let edition_arg = format!("--edition={edition}");
        run_formatter(
            "rustfmt",
            &["rustfmt", "--emit", "stdout"],
            &[edition_arg.as_str()],
            source,
            project_root,
        )
    }
}

/// Walk up from `start` looking for `Cargo.toml` and parse `[package].edition`
/// (or `[workspace.package].edition`). Returns `None` when no `Cargo.toml` is
/// found or the file has no edition field.
///
/// Tiny TOML parser — only looks for `edition = "20XX"` under the right table
/// header. Avoids pulling in the `toml` crate just for this one field.
fn detect_rust_edition(start: &Path) -> Option<String> {
    let mut cur = start.to_owned();
    loop {
        let manifest = cur.join("Cargo.toml");
        if manifest.is_file()
            && let Ok(text) = std::fs::read_to_string(&manifest)
            && let Some(ed) = parse_edition_from_cargo_toml(&text)
        {
            return Some(ed);
        }
        if !cur.pop() {
            return None;
        }
    }
}

fn parse_edition_from_cargo_toml(text: &str) -> Option<String> {
    let mut current_table = String::new();
    let mut package_edition: Option<String> = None;
    let mut workspace_package_edition: Option<String> = None;

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            current_table = rest.trim().to_string();
            continue;
        }
        // Match `edition = "..."` ONLY. Reject `edition.workspace = true`
        // (the workspace-inheritance shorthand) — that would parse as
        // edition=true and rustfmt rejects "Invalid value for --edition".
        if let Some(rest) = line.strip_prefix("edition")
            && rest.starts_with(|c: char| c.is_whitespace() || c == '=')
            && let Some(eq_idx) = rest.find('=')
        {
            let val = rest[eq_idx + 1..]
                .trim()
                .trim_matches(|c| c == '"' || c == '\'');
            match current_table.as_str() {
                "package" => package_edition = Some(val.to_string()),
                "workspace.package" => workspace_package_edition = Some(val.to_string()),
                _ => {}
            }
        }
    }

    package_edition.or(workspace_package_edition)
}

#[cfg(test)]
mod edition_tests {
    use super::*;

    #[test]
    fn parse_edition_finds_package_edition() {
        let toml = r#"
[package]
name = "x"
edition = "2024"
"#;
        assert_eq!(parse_edition_from_cargo_toml(toml).as_deref(), Some("2024"));
    }

    #[test]
    fn parse_edition_finds_workspace_package_edition() {
        let toml = r#"
[workspace.package]
edition = "2021"
"#;
        assert_eq!(parse_edition_from_cargo_toml(toml).as_deref(), Some("2021"));
    }

    #[test]
    fn parse_edition_returns_none_when_missing() {
        let toml = r#"
[package]
name = "x"
"#;
        assert_eq!(parse_edition_from_cargo_toml(toml), None);
    }

    #[test]
    fn parse_edition_handles_single_quotes() {
        let toml = r#"
[package]
edition = '2024'
"#;
        assert_eq!(parse_edition_from_cargo_toml(toml).as_deref(), Some("2024"));
    }

    #[test]
    fn parse_edition_skips_edition_in_other_tables() {
        // `edition` under `[dependencies.foo]` etc must NOT be picked up.
        let toml = r#"
[dependencies.foo]
edition = "2021"

[package]
name = "x"
edition = "2024"
"#;
        assert_eq!(parse_edition_from_cargo_toml(toml).as_deref(), Some("2024"));
    }

    /// Regression: `edition.workspace = true` is the workspace-inheritance
    /// shorthand (used by every member crate in a workspace) and must NOT be
    /// matched as `edition = true` — rustfmt rejects "Invalid value for --edition".
    #[test]
    fn parse_edition_ignores_dot_workspace_shorthand() {
        let toml = r#"
[package]
name = "x"
edition.workspace = true
"#;
        assert_eq!(
            parse_edition_from_cargo_toml(toml),
            None,
            "edition.workspace shorthand must NOT match the bare `edition = ...` rule"
        );
    }

    /// And when both a member crate's Cargo.toml has the shorthand AND a
    /// workspace root has a real `[workspace.package].edition`, the walk-up
    /// must skip the shorthand-only file and find the real value above.
    #[test]
    fn parse_edition_workspace_inheritance_resolves_to_workspace_edition() {
        let member = r#"
[package]
name = "member"
edition.workspace = true
"#;
        let root = r#"
[workspace.package]
edition = "2024"
"#;
        assert_eq!(parse_edition_from_cargo_toml(member), None);
        assert_eq!(parse_edition_from_cargo_toml(root).as_deref(), Some("2024"));
    }
}

// ── Core subprocess runner ────────────────────────────────────────────────────

/// Spawn the formatter, pipe `source` to stdin, wait up to 30 s, return stdout.
///
/// `static_args` are the compile-time arg list (including argv[0] = program).
/// `extra_args` are appended after `static_args` (e.g. the file path for
/// prettier). Both slices may be empty.
///
/// # Errors
///
/// - [`FormatError::NotInstalled`] — `spawn` returns `ErrorKind::NotFound`.
/// - [`FormatError::Io`] — any other I/O error.
/// - [`FormatError::Timeout`] — child still running after [`FORMAT_TIMEOUT`].
/// - [`FormatError::SyntaxError`] — child exits with non-zero status; stderr
///   is captured and included in the error.
fn run_formatter(
    tool_name: &str,
    static_args: &[&str],
    extra_args: &[&str],
    source: &str,
    project_root: &Path,
) -> Result<String, FormatError> {
    let (program, rest) = match static_args {
        [] => {
            return Err(FormatError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "formatter args must not be empty",
            )));
        }
        [prog, rest @ ..] => (*prog, rest),
    };

    tracing::debug!(tool = tool_name, ?project_root, "spawning formatter");

    let mut child = match Command::new(program)
        .args(rest)
        .args(extra_args)
        .current_dir(project_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(FormatError::NotInstalled(tool_name.to_owned()));
        }
        Err(e) => return Err(FormatError::Io(e)),
    };

    // Drain stdout / stderr concurrently in background threads.
    //
    // CRITICAL: without this, a formatter whose output (or the partial
    // output before it errors) exceeds the OS pipe buffer (typically
    // 64 KiB on Linux) deadlocks — the child blocks writing stdout, we
    // block in `try_wait` waiting for the child to exit, neither side
    // moves. Read pipes in dedicated threads so they always drain.
    use std::io::Read as _;
    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");
    let stdout_handle = std::thread::spawn(move || -> std::io::Result<Vec<u8>> {
        let mut buf = Vec::new();
        let mut s = stdout;
        s.read_to_end(&mut buf)?;
        Ok(buf)
    });
    let stderr_handle = std::thread::spawn(move || -> std::io::Result<Vec<u8>> {
        let mut buf = Vec::new();
        let mut s = stderr;
        s.read_to_end(&mut buf)?;
        Ok(buf)
    });

    // Write source to stdin, then close it so the formatter sees EOF.
    // Tolerate `BrokenPipe` — the child may have already errored and closed
    // its end before we finished writing (e.g. rustfmt rejecting a bad flag,
    // shfmt parser hitting an error mid-stream). The real error is in stderr;
    // the reader threads will surface it.
    if let Some(mut stdin) = child.stdin.take() {
        match stdin.write_all(source.as_bytes()) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                tracing::debug!(
                    tool = tool_name,
                    "stdin closed early; child likely errored — reading stderr"
                );
            }
            Err(e) => return Err(FormatError::Io(e)),
        }
        // Drop closes the handle — formatter sees EOF.
    }

    // Poll for child exit with deadline. Reader threads drain pipes the
    // whole time so the child can never block on a full stdout buffer.
    let deadline = Instant::now() + FORMAT_TIMEOUT;
    let status = loop {
        match child.try_wait().map_err(FormatError::Io)? {
            Some(s) => break s,
            None => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    tracing::warn!(tool = tool_name, "formatter timed out");
                    return Err(FormatError::Timeout);
                }
                std::thread::sleep(POLL_INTERVAL);
            }
        }
    };

    // Child exited — join reader threads to get the bytes.
    let stdout_bytes = stdout_handle
        .join()
        .expect("stdout reader thread panicked")
        .map_err(FormatError::Io)?;
    let stderr_bytes = stderr_handle
        .join()
        .expect("stderr reader thread panicked")
        .map_err(FormatError::Io)?;

    if status.success() {
        let formatted = String::from_utf8(stdout_bytes).map_err(|e| {
            FormatError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                e.to_string(),
            ))
        })?;
        tracing::debug!(tool = tool_name, "formatter succeeded");
        Ok(formatted)
    } else {
        let stderr = String::from_utf8_lossy(&stderr_bytes).into_owned();
        tracing::debug!(tool = tool_name, %stderr, "formatter failed");
        Err(FormatError::SyntaxError(stderr))
    }
}

// ── Built-in formatter registry ───────────────────────────────────────────────

/// Look up a formatter for `path` based on its file extension.
///
/// Returns `None` when no built-in formatter is registered for the extension.
/// The returned [`Formatter`] spawns the external tool on each call to
/// [`Formatter::format`]; it does **not** verify the tool is installed until
/// format time.
///
/// | Extension | Tool |
/// |---|---|
/// | `.rs` | `rustfmt --emit stdout` |
/// | `.ts .tsx .js .jsx .mjs .cjs .json .md .yaml .yml` | `prettier --stdin-filepath <path>` |
/// | `.py` | `ruff format -` |
/// | `.go` | `gofmt` |
/// | `.lua` | `stylua -` |
/// | `.sh .bash` | `shfmt` |
/// | `.toml` | `taplo fmt -` |
pub fn formatter_for_path(path: &Path) -> Option<Arc<dyn Formatter>> {
    let ext = path.extension()?.to_str()?;
    match ext {
        "rs" => Some(Arc::new(RustFormatter)),

        // Prettier handles many types; pass the real path so it reads the
        // correct prettier config rules.
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "json" | "md" | "yaml" | "yml" => {
            Some(Arc::new(FormatterWithPath {
                base_args: &["prettier", "--stdin-filepath"],
                file_path: path.to_owned(),
                tool_name: "prettier",
            }))
        }

        "py" => Some(Arc::new(StdinFormatter {
            args: &["ruff", "format", "-"],
            tool_name: "ruff",
        })),

        "go" => Some(Arc::new(StdinFormatter {
            args: &["gofmt"],
            tool_name: "gofmt",
        })),

        "lua" => Some(Arc::new(StdinFormatter {
            args: &["stylua", "-"],
            tool_name: "stylua",
        })),

        "sh" | "bash" => Some(Arc::new(StdinFormatter {
            args: &["shfmt"],
            tool_name: "shfmt",
        })),

        "toml" => Some(Arc::new(StdinFormatter {
            args: &["taplo", "fmt", "-"],
            tool_name: "taplo",
        })),

        _ => None,
    }
}

// ── Async worker ─────────────────────────────────────────────────────────────

/// Stable identifier for an open buffer. Matches the `BufferId` type used by
/// `hjkl-engine` consumers; declared here independently so `hjkl-mangler`
/// does not need to depend on the engine crate.
pub type BufferId = u64;

/// A format job submitted to [`FormatWorker`].
pub struct FormatJob {
    /// The buffer this job targets.
    pub buffer_id: BufferId,
    /// Snapshot of the buffer source at submission time.
    pub source: Arc<String>,
    /// Project root used as the formatter's working directory.
    pub project_root: PathBuf,
    /// Formatter to run.
    pub formatter: Arc<dyn Formatter>,
    /// Buffer dirty-generation at submission time. The install path drops
    /// results whose `dirty_gen` is older than the current buffer gen so
    /// that interleaved typing does not install stale formatted output.
    pub dirty_gen: u64,
}

/// Result of a completed (or failed) format job.
pub struct FormatResult {
    /// Which buffer the result is for.
    pub buffer_id: BufferId,
    /// The dirty-gen that was current when the job was submitted.
    pub dirty_gen: u64,
    /// Formatted source, or the error that occurred.
    pub result: Result<String, FormatError>,
}

/// Internal shared state between the submitter (main thread) and the worker.
struct Pending {
    /// One pending job per buffer_id. Submitting a new job for buffer A
    /// replaces the existing entry — the key dedup invariant.
    jobs: HashMap<BufferId, FormatJob>,
    /// When `true` the worker thread should exit.
    quit: bool,
}

impl Pending {
    fn new() -> Self {
        Self {
            jobs: HashMap::new(),
            quit: false,
        }
    }

    fn has_work(&self) -> bool {
        self.quit || !self.jobs.is_empty()
    }
}

/// Background worker that runs [`Formatter::format`] off the UI thread.
///
/// - Spawn with [`FormatWorker::spawn`].
/// - Submit jobs with [`FormatWorker::submit`]. Per-buffer dedup: a new
///   submit for buffer A while one is already pending replaces it.
/// - Drain results with [`FormatWorker::try_recv`] each event-loop tick.
pub struct FormatWorker {
    pending: Arc<(Mutex<Pending>, Condvar)>,
    rx: std::sync::mpsc::Receiver<FormatResult>,
    handle: Option<JoinHandle<()>>,
}

impl FormatWorker {
    /// Spawn the background worker thread.
    pub fn spawn() -> Self {
        let pending = Arc::new((Mutex::new(Pending::new()), Condvar::new()));
        let (tx, rx) = std::sync::mpsc::channel();
        let pending_for_thread = Arc::clone(&pending);
        let handle = std::thread::Builder::new()
            .name("hjkl-mangler".into())
            .spawn(move || format_worker_loop(pending_for_thread, tx))
            .expect("spawn format worker");
        Self {
            pending,
            rx,
            handle: Some(handle),
        }
    }

    /// Submit a format job. If a job for the same `buffer_id` is already
    /// pending it is replaced (latest wins). Returns immediately.
    pub fn submit(&self, job: FormatJob) {
        let (lock, cvar) = &*self.pending;
        let mut p = lock.lock().expect("format pending mutex poisoned");
        p.jobs.insert(job.buffer_id, job);
        cvar.notify_one();
    }

    /// Non-blocking drain: return the next completed [`FormatResult`] if one
    /// is available, `None` otherwise. Call once per event-loop tick.
    pub fn try_recv(&self) -> Option<FormatResult> {
        self.rx.try_recv().ok()
    }
}

impl Drop for FormatWorker {
    fn drop(&mut self) {
        {
            let (lock, cvar) = &*self.pending;
            if let Ok(mut p) = lock.lock() {
                p.quit = true;
                cvar.notify_one();
            }
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn format_worker_loop(
    pending: Arc<(Mutex<Pending>, Condvar)>,
    tx: std::sync::mpsc::Sender<FormatResult>,
) {
    loop {
        // Wait until there is work or a quit signal.
        let job = {
            let (lock, cvar) = &*pending;
            let mut p = lock.lock().expect("format pending mutex poisoned");
            while !p.has_work() {
                p = cvar.wait(p).expect("format pending cvar poisoned");
            }
            if p.quit {
                return;
            }
            // Take one arbitrary job from the map.
            let key = *p.jobs.keys().next().expect("has_work implies non-empty");
            p.jobs.remove(&key).expect("key just found")
        };

        let result = job.formatter.format(&job.source, &job.project_root);
        let msg = FormatResult {
            buffer_id: job.buffer_id,
            dirty_gen: job.dirty_gen,
            result,
        };
        // Channel closed only when the receiver (App) has been dropped — exit.
        if tx.send(msg).is_err() {
            return;
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // ── formatter_for_path dispatch ──────────────────────────────────────

    #[test]
    fn formatter_for_path_picks_rustfmt_for_rs() {
        let path = PathBuf::from("foo.rs");
        assert!(
            formatter_for_path(&path).is_some(),
            "expected Some(formatter) for .rs"
        );
    }

    #[test]
    fn formatter_for_path_returns_none_for_unknown_ext() {
        let path = PathBuf::from("foo.xyz");
        assert!(
            formatter_for_path(&path).is_none(),
            "expected None for unknown extension .xyz"
        );
    }

    #[test]
    fn formatter_for_path_picks_formatter_for_ts() {
        let path = PathBuf::from("index.ts");
        assert!(formatter_for_path(&path).is_some());
    }

    #[test]
    fn formatter_for_path_picks_formatter_for_py() {
        let path = PathBuf::from("script.py");
        assert!(formatter_for_path(&path).is_some());
    }

    #[test]
    fn formatter_for_path_picks_formatter_for_go() {
        let path = PathBuf::from("main.go");
        assert!(formatter_for_path(&path).is_some());
    }

    #[test]
    fn formatter_for_path_picks_formatter_for_lua() {
        let path = PathBuf::from("init.lua");
        assert!(formatter_for_path(&path).is_some());
    }

    #[test]
    fn formatter_for_path_picks_formatter_for_sh() {
        let path = PathBuf::from("run.sh");
        assert!(formatter_for_path(&path).is_some());
    }

    #[test]
    fn formatter_for_path_picks_formatter_for_toml() {
        let path = PathBuf::from("config.toml");
        assert!(formatter_for_path(&path).is_some());
    }

    #[test]
    fn formatter_for_path_picks_formatter_for_json() {
        let path = PathBuf::from("package.json");
        assert!(formatter_for_path(&path).is_some());
    }

    #[test]
    fn formatter_for_path_picks_formatter_for_yaml() {
        let path = PathBuf::from("ci.yaml");
        assert!(formatter_for_path(&path).is_some());
    }

    // ── Subprocess tests (require tools installed) ────────────────────────
    //
    // Run these with:
    //   cargo test --package hjkl-mangler -- --ignored
    //
    // Each test requires the corresponding tool on PATH.

    #[test]
    #[ignore = "requires rustfmt on PATH"]
    fn rustfmt_formats_simple_function() {
        let src = "fn main(){let x=1;}";
        let formatter = formatter_for_path(Path::new("foo.rs")).unwrap();
        let result = formatter.format(src, Path::new("/tmp")).unwrap();
        // rustfmt adds proper spacing and newlines.
        assert!(result.contains("fn main()"), "expected fn main in output");
        assert!(result.contains("let x = 1;"), "expected spaced assignment");
    }

    #[test]
    #[ignore = "requires prettier on PATH"]
    fn prettier_formats_json() {
        let src = r#"{"a":1,"b":2}"#;
        let formatter = formatter_for_path(Path::new("test.json")).unwrap();
        let result = formatter.format(src, Path::new("/tmp")).unwrap();
        // prettier pretty-prints with newlines.
        assert!(
            result.contains('\n'),
            "expected newlines in prettier output"
        );
        assert!(result.contains("\"a\""), "expected key a in output");
    }

    #[test]
    #[ignore = "requires gofmt on PATH"]
    fn gofmt_formats_go_source() {
        let src = "package main\nfunc main(){x:=1;_ = x}";
        let formatter = formatter_for_path(Path::new("main.go")).unwrap();
        let result = formatter.format(src, Path::new("/tmp")).unwrap();
        assert!(
            result.contains("func main()"),
            "expected func main in output"
        );
    }

    #[test]
    #[ignore = "requires shfmt on PATH"]
    fn shfmt_formats_shell_script() {
        let src = "#!/bin/sh\nif [ 1 -eq 1 ];then echo hi;fi";
        let formatter = formatter_for_path(Path::new("run.sh")).unwrap();
        let result = formatter.format(src, Path::new("/tmp")).unwrap();
        assert!(result.contains("echo"), "expected echo in output");
    }

    #[test]
    #[ignore = "requires stylua on PATH"]
    fn stylua_formats_lua() {
        let src = "local x=1;print(x)";
        let formatter = formatter_for_path(Path::new("init.lua")).unwrap();
        let result = formatter.format(src, Path::new("/tmp")).unwrap();
        assert!(result.contains("local"), "expected local in output");
    }

    #[test]
    #[ignore = "requires taplo on PATH"]
    fn taplo_formats_toml() {
        let src = "[package]\nname=\"test\"\nversion=\"0.1.0\"";
        let formatter = formatter_for_path(Path::new("Cargo.toml")).unwrap();
        let result = formatter.format(src, Path::new("/tmp")).unwrap();
        assert!(result.contains("[package]"), "expected [package] in output");
    }

    #[test]
    #[ignore = "requires ruff on PATH"]
    fn ruff_formats_python() {
        let src = "x=1+2\nprint(x)";
        let formatter = formatter_for_path(Path::new("script.py")).unwrap();
        let result = formatter.format(src, Path::new("/tmp")).unwrap();
        assert!(result.contains("x"), "expected x in output");
    }

    // ── FormatWorker unit tests ───────────────────────────────────────────

    /// A formatter that immediately returns its input unchanged.
    struct EchoFormatter;
    impl Formatter for EchoFormatter {
        fn format(&self, source: &str, _root: &Path) -> Result<String, FormatError> {
            Ok(source.to_owned())
        }
    }

    /// A slow formatter that sleeps briefly so we can test dedup.
    struct SlowFormatter {
        delay: std::time::Duration,
    }
    impl Formatter for SlowFormatter {
        fn format(&self, source: &str, _root: &Path) -> Result<String, FormatError> {
            std::thread::sleep(self.delay);
            Ok(source.to_owned())
        }
    }

    #[test]
    fn worker_drop_joins_cleanly() {
        let w = FormatWorker::spawn();
        drop(w);
        // If the worker thread panics or hangs, the test will either
        // panic itself or time out — either way a test failure.
    }

    #[test]
    fn submit_keeps_jobs_for_different_buffer_ids() {
        // Use a slow formatter so both jobs stay pending long enough to verify.
        let w = FormatWorker::spawn();
        // Submit two jobs for different buffers very quickly.
        w.submit(FormatJob {
            buffer_id: 1,
            source: Arc::new("a".to_owned()),
            project_root: PathBuf::from("/tmp"),
            formatter: Arc::new(EchoFormatter),
            dirty_gen: 1,
        });
        w.submit(FormatJob {
            buffer_id: 2,
            source: Arc::new("b".to_owned()),
            project_root: PathBuf::from("/tmp"),
            formatter: Arc::new(EchoFormatter),
            dirty_gen: 1,
        });
        // Drain both results (order not guaranteed).
        let mut saw_1 = false;
        let mut saw_2 = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(r) = w.try_recv() {
                match r.buffer_id {
                    1 => saw_1 = true,
                    2 => saw_2 = true,
                    _ => panic!("unexpected buffer_id {}", r.buffer_id),
                }
            }
            if saw_1 && saw_2 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for both buffer results"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn submit_replaces_pending_for_same_buffer_id() {
        // Use a slow formatter so the first job is still pending when the
        // second submit arrives.
        let slow = std::time::Duration::from_millis(80);
        let w = FormatWorker::spawn();

        // First job for buffer 42 — content "first".
        w.submit(FormatJob {
            buffer_id: 42,
            source: Arc::new("first".to_owned()),
            project_root: PathBuf::from("/tmp"),
            formatter: Arc::new(SlowFormatter { delay: slow }),
            dirty_gen: 1,
        });
        // Immediately replace with "second" before the worker picks it up.
        w.submit(FormatJob {
            buffer_id: 42,
            source: Arc::new("second".to_owned()),
            project_root: PathBuf::from("/tmp"),
            formatter: Arc::new(SlowFormatter { delay: slow }),
            dirty_gen: 2,
        });

        // Drain results for up to 5 s; we expect exactly one result and it
        // should be dirty_gen=2 ("second") OR dirty_gen=1 ("first") if the
        // worker had already taken the first job before the replace.
        // Either way we must get at most 2 results (one per job that
        // actually ran), and the LAST one must be dirty_gen=2.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut results = Vec::new();
        loop {
            if let Some(r) = w.try_recv() {
                results.push(r);
            }
            // The key dedup invariant: we never get more than 2 results
            // (one if the second replaced before worker saw the first, two
            // if the worker had already dequeued the first job by the time
            // the second submit landed).
            assert!(results.len() <= 2, "got more than 2 results — dedup failed");
            if !results.is_empty() && std::time::Instant::now() > deadline {
                break;
            }
            // Give both jobs enough time to complete if both ran.
            if std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(5));
            } else {
                break;
            }
        }
        assert!(!results.is_empty(), "expected at least one result");
        // The last result must be the second (latest) job.
        let last = results.last().unwrap();
        assert_eq!(
            last.dirty_gen, 2,
            "expected last result to be the second (dirty_gen=2) job"
        );
    }

    #[test]
    #[ignore = "requires rustfmt on PATH"]
    fn worker_formats_rust_async() {
        let w = FormatWorker::spawn();
        let src = "fn main(){let x=1;}";
        w.submit(FormatJob {
            buffer_id: 99,
            source: Arc::new(src.to_owned()),
            project_root: PathBuf::from("/tmp"),
            formatter: formatter_for_path(Path::new("foo.rs")).unwrap(),
            dirty_gen: 7,
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if let Some(r) = w.try_recv() {
                let formatted = r.result.expect("rustfmt should succeed");
                assert!(formatted.contains("fn main()"), "expected fn main");
                assert!(formatted.contains("let x = 1;"), "expected spaced assign");
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for rustfmt result"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}
