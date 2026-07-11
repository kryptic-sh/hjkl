//! `hjkl-mangler` — external formatter dispatch for hjkl.
//!
//! Wraps `rustfmt`, `prettier`, `gofmt`, `ruff`, `stylua`, `shfmt`, `taplo`
//! and friends behind a single [`Formatter`] trait. The app calls
//! [`formatter_for_path`] to look up a formatter by file extension, then
//! either calls [`Formatter::format`] synchronously (blocking up to 30 s)
//! or submits a [`FormatJob`] to a [`FormatWorker`] for async dispatch.
//!
//! # Range support
//!
//! Formatters that have native range arguments (`prettier`, `stylua`, `ruff`)
//! honour an optional [`RangeSpec`] passed to [`Formatter::format`].  All
//! three tools return the **whole file** with only the in-range region
//! reformatted, so the install path is a simple `set_content_undoable` for
//! every formatter — no diff-splice post-processing needed.
//!
//! Formatters without native range support (`rustfmt`, `gofmt`, `shfmt`,
//! `taplo`) ignore the `range` argument and always reformat the whole file.
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

// ── Range types ───────────────────────────────────────────────────────────────

/// Row range for partial-file formatting (inclusive on both ends).
///
/// Row indices are 0-based and refer to the buffer's line numbering. Both
/// fields are public so callers can construct values directly without a
/// constructor. The range is semantically closed: `start_row..=end_row`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeSpec {
    pub start_row: usize,
    pub end_row: usize,
}

/// Convert a row range (0-based, inclusive) to byte offsets into `source`.
///
/// Returns `(start_byte, end_byte)` where:
/// - `start_byte` is the byte offset of the first byte of `start_row`.
/// - `end_byte` is the byte offset just past the last byte of `end_row`
///   (i.e. the position after `end_row`'s trailing `\n`, or `source.len()`
///   when the row is the last and has no trailing newline).
///
/// Rows beyond the last line are clamped to `source.len()`.
pub fn row_range_to_byte_range(source: &str, range: RangeSpec) -> (usize, usize) {
    let mut start_byte = 0usize;
    let mut end_byte = source.len();
    let mut row = 0usize;
    let mut byte_pos = 0usize;
    let bytes = source.as_bytes();

    while byte_pos <= bytes.len() {
        // Find the end of the current row (exclusive — position after '\n').
        let row_end = bytes[byte_pos..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|nl| byte_pos + nl + 1) // include the '\n'
            .unwrap_or(bytes.len());

        if row == range.start_row {
            start_byte = byte_pos;
        }
        if row == range.end_row {
            end_byte = row_end;
            break;
        }

        row += 1;
        byte_pos = row_end;
        if byte_pos >= bytes.len() {
            break;
        }
    }

    (start_byte, end_byte)
}

/// Maximum time we wait for a formatter subprocess before giving up.
/// 30 s is generous enough for rustfmt on very large files (10k+ LOC)
/// without making a hung formatter feel permanent.
const FORMAT_TIMEOUT: Duration = Duration::from_secs(30);

/// Poll interval inside the wait loop.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Cap on a formatter's stdout/stderr. A runaway formatter that streams more
/// than this is treated as an error rather than buffered into unbounded memory.
const MAX_FORMATTER_OUTPUT: usize = 64 * 1024 * 1024;

/// Deadline for the `<tool> --version` availability probe so a hung binary
/// can't block the calling (often UI) thread forever.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Run `<tool> --version` and wait up to [`PROBE_TIMEOUT`], killing (and
/// reaping) the child on timeout. `Ok(Some(status))` = it exited; `Ok(None)` =
/// it launched but timed out; `Err` = it could not be spawned.
fn probe_status(tool: &str) -> std::io::Result<Option<std::process::ExitStatus>> {
    let mut child = Command::new(tool)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + PROBE_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Read a child pipe to EOF, capping the buffer at `max` bytes. A formatter
/// that streams more than `max` errors out instead of allocating without
/// bound; the pipe is still drained afterward so the child can't deadlock on a
/// full stdout buffer before the caller's timeout fires.
fn read_capped(mut r: impl std::io::Read, max: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let mut buf = Vec::new();
    (&mut r).take(max as u64 + 1).read_to_end(&mut buf)?;
    if buf.len() > max {
        let mut sink = [0u8; 8192];
        while r.read(&mut sink)? > 0 {}
        return Err(std::io::Error::other("formatter output exceeds size limit"));
    }
    Ok(buf)
}

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

/// Formats source by invoking an external subprocess.
///
/// Implementations are expected to:
/// - Spawn the tool as a child process with `cwd = project_root`.
/// - Pipe `source` to the child's stdin.
/// - Read the formatted result from stdout.
/// - Return [`FormatError::Timeout`] if the child does not complete within 30 s.
/// - Return [`FormatError::SyntaxError`] on non-zero exit status.
/// - Return [`FormatError::NotInstalled`] when `spawn` returns `NotFound`.
///
/// When `range` is `Some`, implementations that have native range support
/// (`prettier`, `stylua`, `ruff`) will emit the appropriate range flags.
/// All of these tools return the **whole file** with only in-range lines
/// reformatted — the output is safe to install directly via
/// `set_content_undoable`.
///
/// Implementations without native range support ignore `range` and always
/// reformat the whole file (same as `range = None`).
///
/// **This call blocks the calling thread.** Do not invoke on a UI thread
/// without wrapping in a background task. Async dispatch is tracked in #118.
pub trait Formatter: Send + Sync {
    /// Format `source`. Returns the formatted whole-file content or an error.
    ///
    /// `project_root` is used as the working directory so formatters that
    /// walk up looking for config files (e.g. `prettier`, `rustfmt`) find
    /// the project's config.
    ///
    /// `range` is an optional hint to restrict formatting to a row range.
    /// Formatters with native range support (`prettier`, `stylua`, `ruff`)
    /// honour it; others ignore it and always reformat the whole file.
    fn format(
        &self,
        source: &str,
        project_root: &Path,
        range: Option<RangeSpec>,
    ) -> Result<String, FormatError>;

    /// Human-readable name of the underlying tool (e.g. `"rustfmt"`,
    /// `"prettier"`). Used by callers to probe availability via
    /// [`is_tool_installed`] before spending a worker slot on a job
    /// that would fail with [`FormatError::NotInstalled`].
    fn tool_name(&self) -> &str;
}

/// Return `true` when `tool` resolves on `PATH`. Implemented as a
/// `Command::new(tool).arg("--version")` probe so it works uniformly
/// across rustfmt / prettier / gofmt / ruff / shfmt / stylua / taplo.
///
/// Cheap (~few ms when the binary exists, errors instantly when not).
/// Call from the UI thread before submitting a [`FormatJob`] so the
/// caller can fall back to a dumb local algorithm without burning the
/// worker.
pub fn is_tool_installed(tool: &str) -> bool {
    // Treat "spawn succeeded" as installed, regardless of exit status.
    // `probe_tool` runs `tool --version` and requires exit 0, which is
    // fine for rustfmt/prettier/stylua/ruff but breaks for shells like
    // dash that don't recognise `--version` (Ubuntu CI's `/bin/sh` is
    // dash → `is_tool_installed("sh")` was returning false despite sh
    // being present). The format dispatcher's only real question is
    // "can we launch this binary?" — the worker reports real errors
    // separately. Use `probe_tool` when you also need exit-code
    // diagnostics.
    // Spawn succeeded ⇒ launchable. Uses a timeout so a binary that hangs on
    // `--version` can't block this (often UI-thread) call forever.
    probe_status(tool).is_ok()
}

/// Detailed availability probe. Returns `Ok(())` when the tool runs and
/// exits 0 on `--version`, otherwise an error string describing what
/// went wrong (spawn failure with kind, or non-zero exit code). Use this
/// when the caller wants to surface diagnostics; [`is_tool_installed`]
/// is the convenience wrapper.
pub fn probe_tool(tool: &str) -> Result<(), String> {
    match probe_status(tool) {
        Ok(Some(status)) if status.success() => Ok(()),
        Ok(Some(status)) => Err(format!(
            "spawned but exited {}",
            status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "?".into())
        )),
        Ok(None) => Err(format!("timed out after {}s", PROBE_TIMEOUT.as_secs())),
        Err(e) => Err(format!("spawn failed: {} ({:?})", e, e.kind())),
    }
}

// ── Shared subprocess helper ──────────────────────────────────────────────────

/// A formatter that pipes stdin → stdout with no range support.
///
/// `args[0]` is the program name; `args[1..]` are its arguments.
/// The caller supplies `args` as a `&'static [&'static str]` so no allocation
/// is needed per call. Range is always ignored — the whole file is reformatted.
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
    fn format(
        &self,
        source: &str,
        project_root: &Path,
        _range: Option<RangeSpec>,
    ) -> Result<String, FormatError> {
        run_formatter(self.tool_name, self.args, &[], source, project_root)
    }
    fn tool_name(&self) -> &str {
        self.tool_name
    }
}

/// A prettier formatter that injects the buffer file path and optional byte-range flags.
///
/// Used for `prettier --stdin-filepath <path> [--range-start <s> --range-end <e>]`.
/// When `range` is `Some`, emits `--range-start` and `--range-end` as byte offsets
/// derived from the row range. Prettier returns the **whole file** with only
/// in-range content reformatted, so the output is safe to install directly.
pub struct PrettierFormatter {
    /// The file path to pass as `--stdin-filepath`.
    pub file_path: PathBuf,
}

impl Formatter for PrettierFormatter {
    fn format(
        &self,
        source: &str,
        project_root: &Path,
        range: Option<RangeSpec>,
    ) -> Result<String, FormatError> {
        let path_str = self.file_path.to_string_lossy().into_owned();
        let base_args: &[&str] = &["prettier", "--stdin-filepath"];

        if let Some(range) = range {
            let (start_byte, end_byte) = row_range_to_byte_range(source, range);
            let start_str = start_byte.to_string();
            let end_str = end_byte.to_string();
            run_formatter(
                "prettier",
                base_args,
                &[
                    path_str.as_str(),
                    "--range-start",
                    start_str.as_str(),
                    "--range-end",
                    end_str.as_str(),
                ],
                source,
                project_root,
            )
        } else {
            run_formatter(
                "prettier",
                base_args,
                &[path_str.as_str()],
                source,
                project_root,
            )
        }
    }
    fn tool_name(&self) -> &str {
        "prettier"
    }
}

/// A stylua formatter with native byte-range support.
///
/// When `range` is `Some`, emits `--range-start <s> --range-end <e>` as byte
/// offsets. Stylua returns the **whole file** with only in-range content
/// reformatted. When `range` is `None`, reformats the whole file.
pub struct StyluaFormatter;

impl Formatter for StyluaFormatter {
    fn format(
        &self,
        source: &str,
        project_root: &Path,
        range: Option<RangeSpec>,
    ) -> Result<String, FormatError> {
        let base_args: &[&str] = &["stylua", "-"];

        if let Some(range) = range {
            let (start_byte, end_byte) = row_range_to_byte_range(source, range);
            let start_str = start_byte.to_string();
            let end_str = end_byte.to_string();
            run_formatter(
                "stylua",
                base_args,
                &[
                    "--range-start",
                    start_str.as_str(),
                    "--range-end",
                    end_str.as_str(),
                ],
                source,
                project_root,
            )
        } else {
            run_formatter("stylua", base_args, &[], source, project_root)
        }
    }
    fn tool_name(&self) -> &str {
        "stylua"
    }
}

/// A ruff formatter with native line:col range support.
///
/// When `range` is `Some`, emits `--range <start_line>:<start_col>-<end_line>:<end_col>`
/// using 1-based line numbers. The end line is `end_row + 2` (exclusive) so that the
/// inclusive 0-based `end_row` is fully covered. Ruff returns the **whole file** with
/// only in-range content reformatted. When `range` is `None`, reformats the whole file.
pub struct RuffFormatter;

impl Formatter for RuffFormatter {
    fn format(
        &self,
        source: &str,
        project_root: &Path,
        range: Option<RangeSpec>,
    ) -> Result<String, FormatError> {
        let base_args: &[&str] = &["ruff", "format", "-"];

        if let Some(range) = range {
            // Ruff uses 1-based line numbers. start_row is 0-based, so start
            // line is start_row + 1. The end is exclusive in ruff's semantics,
            // so to cover the inclusive end_row (0-based), end line is end_row + 2.
            let start_line = range.start_row + 1;
            let end_line = range.end_row + 2;
            let range_str = format!("{start_line}:1-{end_line}:1");
            run_formatter(
                "ruff",
                base_args,
                &["--range", range_str.as_str()],
                source,
                project_root,
            )
        } else {
            run_formatter("ruff", base_args, &[], source, project_root)
        }
    }
    fn tool_name(&self) -> &str {
        "ruff"
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
///
/// Range is always ignored — rustfmt has no stable range flag.
pub struct RustFormatter;

impl Formatter for RustFormatter {
    fn format(
        &self,
        source: &str,
        project_root: &Path,
        _range: Option<RangeSpec>,
    ) -> Result<String, FormatError> {
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
    fn tool_name(&self) -> &str {
        "rustfmt"
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
    run_formatter_with_timeout(
        tool_name,
        static_args,
        extra_args,
        source,
        project_root,
        FORMAT_TIMEOUT,
    )
}

/// [`run_formatter`] with an explicit timeout — split out so tests can
/// exercise the timeout path without waiting the full [`FORMAT_TIMEOUT`].
fn run_formatter_with_timeout(
    tool_name: &str,
    static_args: &[&str],
    extra_args: &[&str],
    source: &str,
    project_root: &Path,
    timeout: Duration,
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

    // An empty path as the working directory makes `spawn()` fail with
    // `ErrorKind::NotFound` — which we'd misreport as `NotInstalled`. A bare
    // relative filename (`foo.toml`) yields `Path::parent() == Some("")`, so
    // callers can hand us an empty root; fall back to `.` (the process cwd).
    let cwd: &Path = if project_root.as_os_str().is_empty() {
        Path::new(".")
    } else {
        project_root
    };

    let mut child = match Command::new(program)
        .args(rest)
        .args(extra_args)
        .current_dir(cwd)
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
    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");
    let stdout_handle = std::thread::spawn(move || read_capped(stdout, MAX_FORMATTER_OUTPUT));
    let stderr_handle = std::thread::spawn(move || read_capped(stderr, MAX_FORMATTER_OUTPUT));

    // Write source to stdin in a background thread, then close it so the
    // formatter sees EOF.
    //
    // CRITICAL: the write must not happen on this thread. A hung formatter
    // that never reads stdin leaves `write_all` blocked forever once `source`
    // exceeds the OS pipe buffer (typically 64 KiB on Linux) — and that block
    // would happen *before* the timeout loop below ever starts, wedging the
    // caller permanently.
    //
    // Tolerate `BrokenPipe` — the child may have already errored and closed
    // its end before we finished writing (e.g. rustfmt rejecting a bad flag,
    // shfmt parser hitting an error mid-stream). The real error is in stderr;
    // the reader threads will surface it. Other write errors are logged and
    // the child's exit status decides the outcome.
    let stdin_handle = child.stdin.take().map(|mut stdin| {
        let source = source.as_bytes().to_vec();
        let tool = tool_name.to_owned();
        std::thread::spawn(move || {
            match stdin.write_all(&source) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                    tracing::debug!(
                        tool = %tool,
                        "stdin closed early; child likely errored — reading stderr"
                    );
                }
                Err(e) => {
                    tracing::debug!(tool = %tool, error = %e, "stdin write failed");
                }
            }
            // Drop closes the handle — formatter sees EOF.
        })
    });

    // Poll for child exit with deadline. Reader threads drain pipes the
    // whole time so the child can never block on a full stdout buffer.
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait().map_err(FormatError::Io)? {
            Some(s) => break Some(s),
            None => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    // Reap the killed child — `kill()` alone leaves a zombie
                    // process until the editor exits.
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(POLL_INTERVAL);
            }
        }
    };

    // The child is dead (exited or killed + reaped), so its stdin read end is
    // closed and the writer thread unblocks promptly (EPIPE at worst).
    if let Some(h) = stdin_handle {
        let _ = h.join();
    }

    let Some(status) = status else {
        tracing::warn!(tool = tool_name, "formatter timed out");
        return Err(FormatError::Timeout);
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
/// | Extension | Tool | Native range |
/// |---|---|---|
/// | `.rs` | `rustfmt --emit stdout` | none (whole-file always) |
/// | `.ts .tsx .js .jsx .mjs .cjs .json .md .yaml .yml` | `prettier --stdin-filepath <path>` | `--range-start/--range-end` (bytes) |
/// | `.py` | `ruff format -` | `--range <L:C>-<L:C>` (1-based line:col) |
/// | `.go` | `gofmt` | none |
/// | `.lua` | `stylua -` | `--range-start/--range-end` (bytes) |
/// | `.sh .bash` | `shfmt` | none |
/// | `.toml` | `taplo fmt -` | none |
pub fn formatter_for_path(path: &Path) -> Option<Arc<dyn Formatter>> {
    let ext = path.extension()?.to_str()?;
    match ext {
        "rs" => Some(Arc::new(RustFormatter)),

        // Prettier handles many types; pass the real path so it reads the
        // correct prettier config rules. Supports native byte-range flags.
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "json" | "md" | "yaml" | "yml" => {
            Some(Arc::new(PrettierFormatter {
                file_path: path.to_owned(),
            }))
        }

        // Ruff supports native line:col range formatting.
        "py" => Some(Arc::new(RuffFormatter)),

        "go" => Some(Arc::new(StdinFormatter {
            args: &["gofmt"],
            tool_name: "gofmt",
        })),

        // Stylua supports native byte-range formatting.
        "lua" => Some(Arc::new(StyluaFormatter)),

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
    /// Row range the user operated on. Formatters with native range support
    /// (`prettier`, `stylua`, `ruff`) pass this through as native flags.
    /// Formatters without range support ignore it and reformat the whole file.
    pub range: Option<RangeSpec>,
}

/// Result of a completed (or failed) format job.
pub struct FormatResult {
    /// Which buffer the result is for.
    pub buffer_id: BufferId,
    /// The dirty-gen that was current when the job was submitted.
    pub dirty_gen: u64,
    /// The exact buffer snapshot this result was formatted from. The installer
    /// compares it against the buffer's current content to decide staleness —
    /// `dirty_gen` alone is unreliable because non-content operations (e.g.
    /// fold open/close) also bump it, which would falsely reject valid output.
    pub source: Arc<String>,
    /// Formatted source, or the error that occurred.
    ///
    /// For formatters with native range support, this is the **whole file**
    /// with only the in-range region reformatted. For formatters without range
    /// support, this is the whole file reformatted. In both cases, install
    /// via `set_content_undoable` without any further post-processing.
    pub result: Result<String, FormatError>,
    /// Mirrors [`FormatJob::range`] for informational purposes. The installer
    /// does not need to use this for diff-splicing — the formatter output is
    /// already ready to install directly.
    pub range: Option<RangeSpec>,
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

        let result = job
            .formatter
            .format(&job.source, &job.project_root, job.range);
        let msg = FormatResult {
            buffer_id: job.buffer_id,
            dirty_gen: job.dirty_gen,
            source: job.source.clone(),
            result,
            range: job.range,
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

    #[test]
    fn read_capped_accepts_within_limit_and_rejects_over() {
        // Exactly at the cap is fine.
        let ok = read_capped(std::io::Cursor::new(vec![b'x'; 100]), 100).unwrap();
        assert_eq!(ok.len(), 100);
        // One byte over errors instead of buffering unbounded.
        let err = read_capped(std::io::Cursor::new(vec![b'x'; 101]), 100);
        assert!(err.is_err(), "output over the cap must error");
    }

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
    fn is_tool_installed_returns_true_for_sh() {
        // `sh` is on every POSIX system; can't run mangler tests where it's
        // absent so this is a safe positive assertion.
        assert!(
            is_tool_installed("sh"),
            "sh must resolve on PATH for the probe to function"
        );
    }

    #[test]
    fn is_tool_installed_returns_false_for_missing_tool() {
        assert!(
            !is_tool_installed("hjkl-mangler-definitely-not-a-real-tool-xyz"),
            "probe must return false for a tool not on PATH"
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

    /// Regression: an empty `project_root` (what `Path::parent()` of a bare
    /// relative filename like `foo.toml` yields — `Some("")`) must NOT make the
    /// spawn fail with `NotFound` and get misreported as `NotInstalled`. The
    /// formatter must run against the process cwd instead. Uses `cat` (present
    /// on every POSIX box) as a stdin→stdout echo "formatter".
    #[test]
    #[cfg(unix)]
    fn empty_project_root_does_not_report_not_installed() {
        let fmt = StdinFormatter {
            args: &["cat"],
            tool_name: "cat",
        };
        let out = fmt.format("hello\n", std::path::Path::new(""), None);
        assert!(
            !matches!(out, Err(FormatError::NotInstalled(_))),
            "empty project_root must not be misreported as NotInstalled: {out:?}"
        );
        assert_eq!(out.unwrap(), "hello\n", "cat echoes stdin → stdout");
    }

    /// Regression: a hung formatter that never reads stdin must not defeat
    /// the timeout. `sleep` reads nothing from stdin; with a source larger
    /// than the OS pipe buffer (64 KiB on Linux) the old code blocked in
    /// `write_all` on the calling thread *before* the timeout loop started,
    /// hanging the format worker until the child happened to exit.
    #[test]
    #[cfg(unix)]
    fn hung_formatter_that_ignores_stdin_times_out() {
        let big = "x".repeat(1 << 20); // 1 MiB ≫ pipe buffer
        let start = Instant::now();
        let out = run_formatter_with_timeout(
            "sleep",
            &["sleep", "5"],
            &[],
            &big,
            Path::new("."),
            Duration::from_millis(300),
        );
        assert!(
            matches!(out, Err(FormatError::Timeout)),
            "expected Timeout, got {out:?}"
        );
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "timeout did not fire promptly (took {:?})",
            start.elapsed()
        );
    }

    #[test]
    fn formatter_for_path_picks_formatter_for_yaml() {
        let path = PathBuf::from("ci.yaml");
        assert!(formatter_for_path(&path).is_some());
    }

    // ── row_range_to_byte_range unit tests ────────────────────────────────

    #[test]
    fn prettier_range_flag_emission_byte_offsets() {
        // Source: 12 lines; verify byte offsets for rows 5..=10.
        let mut source = String::new();
        for i in 0..12 {
            source.push_str(&format!("line{i}\n"));
        }

        let range = RangeSpec {
            start_row: 5,
            end_row: 10,
        };
        let (start_byte, end_byte) = row_range_to_byte_range(&source, range);

        // Each "lineN\n" is 6 bytes for N 0..=9 (5 chars + \n).
        // But "line10\n" is 7 bytes. Rows 0..=4 are "line0\n".."line4\n" = 5*6=30 bytes.
        let expected_start: usize = (0..5).map(|i: usize| format!("line{i}\n").len()).sum();
        let expected_end: usize = (0..=10).map(|i: usize| format!("line{i}\n").len()).sum();

        assert_eq!(
            start_byte, expected_start,
            "start_byte must be the offset of row 5"
        );
        assert_eq!(
            end_byte, expected_end,
            "end_byte must be just past the trailing \\n of row 10"
        );
    }

    #[test]
    fn row_range_to_byte_range_single_row() {
        // "abc\ndef\nghi\n" — rows 0="abc\n", 1="def\n", 2="ghi\n"
        let source = "abc\ndef\nghi\n";
        let (s, e) = row_range_to_byte_range(
            source,
            RangeSpec {
                start_row: 1,
                end_row: 1,
            },
        );
        // Row 1 starts at byte 4, ends at byte 8 (past '\n' of "def\n").
        assert_eq!(s, 4);
        assert_eq!(e, 8);
    }

    #[test]
    fn row_range_to_byte_range_whole_file() {
        let source = "abc\ndef\n";
        let (s, e) = row_range_to_byte_range(
            source,
            RangeSpec {
                start_row: 0,
                end_row: 1,
            },
        );
        assert_eq!(s, 0);
        assert_eq!(e, source.len());
    }

    #[test]
    fn row_range_to_byte_range_no_trailing_newline() {
        let source = "abc\ndef";
        let (s, e) = row_range_to_byte_range(
            source,
            RangeSpec {
                start_row: 1,
                end_row: 1,
            },
        );
        // Row 1 starts at byte 4, ends at source.len() (no trailing newline).
        assert_eq!(s, 4);
        assert_eq!(e, source.len());
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
        let result = formatter.format(src, Path::new("/tmp"), None).unwrap();
        // rustfmt adds proper spacing and newlines.
        assert!(result.contains("fn main()"), "expected fn main in output");
        assert!(result.contains("let x = 1;"), "expected spaced assignment");
    }

    #[test]
    #[ignore = "requires prettier on PATH"]
    fn prettier_formats_json() {
        let src = r#"{"a":1,"b":2}"#;
        let formatter = formatter_for_path(Path::new("test.json")).unwrap();
        let result = formatter.format(src, Path::new("/tmp"), None).unwrap();
        // prettier pretty-prints with newlines.
        assert!(
            result.contains('\n'),
            "expected newlines in prettier output"
        );
        assert!(result.contains("\"a\""), "expected key a in output");
    }

    #[test]
    #[ignore = "requires prettier on PATH"]
    fn prettier_range_formats_only_specified_rows() {
        // A two-key JSON object. Format only row 1 ("b":2) — row 0 is already
        // well-formed ("a": 1). Prettier must return the whole file with only
        // the in-range bytes reformatted.
        let src = "{\n  \"a\": 1,\n\"b\":2\n}\n";
        let formatter = formatter_for_path(Path::new("test.json")).unwrap();
        // Row 2 (0-based) is the `"b":2` line.
        let range = RangeSpec {
            start_row: 2,
            end_row: 2,
        };
        let result = formatter
            .format(src, Path::new("/tmp"), Some(range))
            .unwrap();
        // Must still contain "a": 1 (untouched row).
        assert!(result.contains("\"a\""), "row 0 must be preserved");
        // The whole file is returned, not just the range.
        assert!(result.contains('{'), "whole-file output expected");
    }

    #[test]
    #[ignore = "requires gofmt on PATH"]
    fn gofmt_formats_go_source() {
        let src = "package main\nfunc main(){x:=1;_ = x}";
        let formatter = formatter_for_path(Path::new("main.go")).unwrap();
        let result = formatter.format(src, Path::new("/tmp"), None).unwrap();
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
        let result = formatter.format(src, Path::new("/tmp"), None).unwrap();
        assert!(result.contains("echo"), "expected echo in output");
    }

    #[test]
    #[ignore = "requires stylua on PATH"]
    fn stylua_formats_lua() {
        let src = "local x=1;print(x)";
        let formatter = formatter_for_path(Path::new("init.lua")).unwrap();
        let result = formatter.format(src, Path::new("/tmp"), None).unwrap();
        assert!(result.contains("local"), "expected local in output");
    }

    #[test]
    #[ignore = "requires taplo on PATH"]
    fn taplo_formats_toml() {
        let src = "[package]\nname=\"test\"\nversion=\"0.1.0\"";
        let formatter = formatter_for_path(Path::new("Cargo.toml")).unwrap();
        let result = formatter.format(src, Path::new("/tmp"), None).unwrap();
        assert!(result.contains("[package]"), "expected [package] in output");
    }

    #[test]
    #[ignore = "requires ruff on PATH"]
    fn ruff_formats_python() {
        let src = "x=1+2\nprint(x)";
        let formatter = formatter_for_path(Path::new("script.py")).unwrap();
        let result = formatter.format(src, Path::new("/tmp"), None).unwrap();
        assert!(result.contains("x"), "expected x in output");
    }

    // ── FormatWorker unit tests ───────────────────────────────────────────

    /// A formatter that immediately returns its input unchanged.
    struct EchoFormatter;
    impl Formatter for EchoFormatter {
        fn format(
            &self,
            source: &str,
            _root: &Path,
            _range: Option<RangeSpec>,
        ) -> Result<String, FormatError> {
            Ok(source.to_owned())
        }
        fn tool_name(&self) -> &str {
            "echo"
        }
    }

    /// A slow formatter that sleeps briefly so we can test dedup.
    struct SlowFormatter {
        delay: std::time::Duration,
    }
    impl Formatter for SlowFormatter {
        fn format(
            &self,
            source: &str,
            _root: &Path,
            _range: Option<RangeSpec>,
        ) -> Result<String, FormatError> {
            std::thread::sleep(self.delay);
            Ok(source.to_owned())
        }
        fn tool_name(&self) -> &str {
            "slow"
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
            range: None,
        });
        w.submit(FormatJob {
            buffer_id: 2,
            source: Arc::new("b".to_owned()),
            project_root: PathBuf::from("/tmp"),
            formatter: Arc::new(EchoFormatter),
            dirty_gen: 1,
            range: None,
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
            range: None,
        });
        // Immediately replace with "second" before the worker picks it up.
        w.submit(FormatJob {
            buffer_id: 42,
            source: Arc::new("second".to_owned()),
            project_root: PathBuf::from("/tmp"),
            formatter: Arc::new(SlowFormatter { delay: slow }),
            dirty_gen: 2,
            range: None,
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
            range: None,
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
