//! `hjkl-mangler` — external formatter dispatch for hjkl.
//!
//! Wraps `rustfmt`, `prettier`, `gofmt`, `ruff`, `stylua`, `shfmt`, `taplo`
//! and friends behind a single [`Formatter`] trait. The app calls
//! [`formatter_for_path`] to look up a formatter by file extension, then
//! calls [`Formatter::format`] synchronously (blocking up to 2 s).
//!
//! # Timeout
//!
//! [`Formatter::format`] blocks the calling thread for at most 2 seconds.
//! The implementation polls [`std::process::Child::try_wait`] in a tight
//! spin-loop with 5 ms sleeps. This is intentionally simple; async invocation
//! is tracked in #118.
//!
//! # Adding a formatter
//!
//! 1. Implement [`Formatter`] (or reuse [`StdinFormatter`]).
//! 2. Add an entry to [`formatter_for_path`].

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Maximum time we wait for a formatter subprocess before giving up.
const FORMAT_TIMEOUT: Duration = Duration::from_secs(2);

/// Poll interval inside the wait loop.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors returned by [`Formatter::format`].
#[derive(Debug)]
pub enum FormatError {
    /// Tool is not installed / not on `PATH`. Carries the tool name.
    NotInstalled(String),
    /// Formatter exceeded the 2-second timeout.
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
            FormatError::Timeout => write!(f, "formatter timed out (>2 s)"),
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
/// - Return [`FormatError::Timeout`] if the child does not complete within 2 s.
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

/// Spawn the formatter, pipe `source` to stdin, wait up to 2 s, return stdout.
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

    // Write source to stdin, then close it so the formatter sees EOF.
    // Tolerate `BrokenPipe` — the child may have already errored and closed
    // its end before we finished writing (e.g. rustfmt rejecting a bad flag,
    // shfmt parser hitting an error mid-stream). The real error is in stderr;
    // we'll surface it from `wait_with_output` below.
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

    // Poll until done or timeout.
    let deadline = Instant::now() + FORMAT_TIMEOUT;
    loop {
        match child.try_wait().map_err(FormatError::Io)? {
            Some(status) => {
                // Child finished — collect output.
                let output = child.wait_with_output().map_err(FormatError::Io)?;
                if status.success() {
                    let formatted = String::from_utf8(output.stdout).map_err(|e| {
                        FormatError::Io(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            e.to_string(),
                        ))
                    })?;
                    tracing::debug!(tool = tool_name, "formatter succeeded");
                    return Ok(formatted);
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
                    tracing::debug!(tool = tool_name, %stderr, "formatter failed");
                    return Err(FormatError::SyntaxError(stderr));
                }
            }
            None => {
                // Still running.
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    tracing::warn!(tool = tool_name, "formatter timed out");
                    return Err(FormatError::Timeout);
                }
                std::thread::sleep(POLL_INTERVAL);
            }
        }
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
}
