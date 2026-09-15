//! Process-global execution policy for non-TUI / RPC modes.
//!
//! Interactive TUI keeps full vim parity (shell-out, unrestricted paths). The
//! non-TUI entry points (`--embed`, `--nvim-api`, `--headless`) may take
//! commands from a remote or automated caller that is not the local user, so
//! they can tighten this policy at startup. Mirrors the one-shot global pattern
//! used by the clipboard-disable path (`host::disable_clipboard_for_rpc`).
//!
//! Flags are set once, before any editor is built, and only ever flip from the
//! permissive default to the restrictive state — never back — so a plain
//! `Relaxed` atomic is sufficient.

use std::sync::atomic::{AtomicBool, Ordering};

/// When `true`, shell-out commands (`:!cmd`, `:[range]!cmd`, `:r !cmd`, and the
/// engine range filter) are refused. Default `false` (allowed, as in vim).
static SHELL_DISABLED: AtomicBool = AtomicBool::new(false);

/// Refuse shell-out for the rest of the process. Call once at RPC/headless
/// startup, before building any editor.
pub fn disable_shell() {
    SHELL_DISABLED.store(true, Ordering::Relaxed);
}

/// True if shell-out has been disabled for this process.
pub fn shell_disabled() -> bool {
    SHELL_DISABLED.load(Ordering::Relaxed)
}

/// Build the platform shell invocation for a user-typed shell-out command —
/// the one builder every shell-out site uses, so they all run the same shell.
/// Callers still check [`shell_disabled`] first.
///
/// Unix runs `sh -c <command>`. Windows runs `%COMSPEC% /S /C "<command>"`
/// (`cmd.exe` when `COMSPEC` is unset), vim's `shell` / `shellcmdflag` /
/// `shellxquote` defaults there: there is no `sh` on a stock Windows `PATH`.
/// The command goes to cmd.exe as one raw, quote-wrapped argument, and `/S`
/// makes cmd.exe strip exactly those outer quotes and run the rest verbatim.
/// std's default argument escaping would backslash-escape inner quotes, which
/// cmd.exe does not understand — `echo "a b"` printed `\"a b\"` and a quoted
/// program path failed to run.
pub fn shell_command(command: &str) -> std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let shell = std::env::var_os("COMSPEC").unwrap_or_else(|| "cmd.exe".into());
        let mut cmd = std::process::Command::new(shell);
        cmd.arg("/S").arg("/C").raw_arg(format!("\"{command}\""));
        cmd
    }
    #[cfg(not(windows))]
    {
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c").arg(command);
        cmd
    }
}

/// Split a shell filter's `stdout` into the rows that replace the filtered
/// range, where `input` is the range's rows joined by `\n` as fed to the
/// command. Every range filter uses it, so they all agree on line endings.
///
/// A trailing newline ends the last row rather than starting an empty one, so
/// empty output replaces the range with no rows (vim: `:%!true` deletes them).
///
/// Line endings follow the input. When no input row ended in `\r`, each output
/// row's trailing `\r` is dropped: tools run through cmd.exe emit CRLF, which
/// would otherwise turn an LF buffer's rows into CRLF ones. When the input had
/// CRLF rows, the output rows are kept byte-for-byte, so a CRLF buffer stays
/// CRLF through a filter.
pub fn filter_output_rows(input: &str, stdout: &str) -> Vec<String> {
    if stdout.is_empty() {
        return Vec::new();
    }
    let input_has_cr = input.split('\n').any(|row| row.ends_with('\r'));
    let body = stdout.strip_suffix('\n').unwrap_or(stdout);
    body.split('\n')
        .map(|row| {
            if input_has_cr {
                row
            } else {
                row.strip_suffix('\r').unwrap_or(row)
            }
        })
        .map(String::from)
        .collect()
}

/// When `true`, file I/O paths are confined to the current working directory
/// subtree: absolute paths and paths containing a `..` component are refused.
/// Default `false` (unrestricted, as in vim). The RPC entry points enable this
/// so a remote/automated caller cannot read or write arbitrary filesystem
/// locations via `:w`/`:e`/`:r`.
static FS_RESTRICTED: AtomicBool = AtomicBool::new(false);

/// Confine file I/O to the working-directory subtree for the rest of the
/// process. Call once at RPC startup, before building any editor.
pub fn restrict_fs() {
    FS_RESTRICTED.store(true, Ordering::Relaxed);
}

/// True if filesystem access has been confined for this process.
pub fn fs_restricted() -> bool {
    FS_RESTRICTED.load(Ordering::Relaxed)
}

/// True if `path` would escape a confined working directory: it is absolute, or
/// contains a parent-dir (`..`), root, or prefix component.
pub fn path_escapes(path: &std::path::Path) -> bool {
    use std::path::Component;
    path.components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    })
}

/// `Err` with a uniform message when `path` is refused under a confined
/// filesystem policy; `Ok(())` when access is allowed (policy off, or the path
/// stays within the working directory).
pub fn check_fs_path(path: &std::path::Path) -> Result<(), String> {
    if fs_restricted() && path_escapes(path) {
        return Err(format!(
            "path {} is outside the working directory (blocked in RPC mode)",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(command: &str) -> String {
        let out = shell_command(command)
            .output()
            .expect("the platform shell must spawn");
        assert!(out.status.success(), "`{command}` failed: {out:?}");
        String::from_utf8(out.stdout)
            .unwrap()
            .trim_end()
            .to_string()
    }

    #[test]
    fn shell_command_runs_a_command() {
        assert_eq!(run("echo hjkl"), "hjkl");
    }

    /// Inner quotes reach the shell exactly as typed: cmd.exe's `echo` prints
    /// them, sh's `echo` consumes them. Either way the doubled space survives.
    #[test]
    fn shell_command_passes_inner_quotes_verbatim() {
        let expected = if cfg!(windows) { "\"a  b\"" } else { "a  b" };
        assert_eq!(run(r#"echo "a  b""#), expected);
    }

    fn rows(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn filter_output_rows_strips_cr_when_input_is_lf() {
        assert_eq!(filter_output_rows("b\na", "a\r\nb\r\n"), rows(&["a", "b"]));
    }

    #[test]
    fn filter_output_rows_keeps_cr_when_input_is_crlf() {
        assert_eq!(
            filter_output_rows("b\r\na\r", "a\r\nb\r\n"),
            rows(&["a\r", "b\r"])
        );
    }

    #[test]
    fn filter_output_rows_trailing_newline_ends_the_last_row() {
        assert_eq!(filter_output_rows("x", "a\n"), rows(&["a"]));
        assert_eq!(filter_output_rows("x", "a"), rows(&["a"]));
        assert_eq!(filter_output_rows("x", "a\n\n"), rows(&["a", ""]));
        assert_eq!(filter_output_rows("x", "\n"), rows(&[""]));
    }

    #[test]
    fn filter_output_rows_empty_output_is_no_rows() {
        assert!(filter_output_rows("x", "").is_empty());
    }

    #[test]
    fn shell_command_runs_pipelines() {
        let command = if cfg!(windows) {
            "echo ab| findstr b"
        } else {
            "echo ab | grep b"
        };
        assert_eq!(run(command), "ab");
    }
}
