//! `:[range]!cmd` shell filter — Phase 8a.
//!
//! Without a range: runs the command and shows stdout as an Info toast.
//! With a range: pipes the rows through the command and replaces them with
//! stdout. Ported verbatim from `hjkl_editor::ex::apply_shell_filter`.

use crate::{effect::ExEffect, range::LineRange};
use hjkl_engine::Host;

/// `:[range]!cmd` — pipe range through shell command, or run bare.
///
/// Called from `try_dispatch` via the special-case `!` prefix check
/// (before `split_name_args`).
pub(crate) fn shell_filter_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    cmd: &str,
    range: Option<LineRange>,
) -> ExEffect {
    if cmd.is_empty() {
        return ExEffect::Error(":! needs a shell command".into());
    }
    use std::io::Write as IoWrite;
    use std::process::{Command, Stdio};

    if range.is_none() {
        // Bare `:!cmd` — run, no buffer change, surface stdout via Info.
        let output = Command::new("sh").arg("-c").arg(cmd).output();
        return match output {
            Ok(out) if out.status.success() => {
                let stdout = String::from_utf8_lossy(&out.stdout).trim_end().to_string();
                if stdout.is_empty() {
                    ExEffect::Info(format!("`{cmd}` exited 0"))
                } else {
                    ExEffect::Info(stdout)
                }
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let trimmed = stderr.trim();
                let label = if trimmed.is_empty() {
                    "no stderr".to_string()
                } else {
                    trimmed.to_string()
                };
                ExEffect::Error(format!(
                    "command exited {} ({label})",
                    out.status
                        .code()
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "?".into())
                ))
            }
            Err(e) => ExEffect::Error(format!("cannot run `{cmd}`: {e}")),
        };
    }

    // Range supplied — pipe the rows through the command.
    let r = range.unwrap();
    let mut all_lines: Vec<String> = editor.buffer().lines().to_vec();
    let total = all_lines.len();
    if total == 0 {
        return ExEffect::Ok;
    }
    // Convert 1-based inclusive range to 0-based inclusive.
    let start = r.start_one_based().saturating_sub(1);
    let bot = (r.end_one_based().saturating_sub(1)).min(total - 1);
    if start > bot {
        return ExEffect::Ok;
    }
    let payload = all_lines[start..=bot].join("\n");
    let mut child = match Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return ExEffect::Error(format!("cannot spawn `{cmd}`: {e}")),
    };
    if let Some(stdin) = child.stdin.as_mut() {
        match stdin.write_all(payload.as_bytes()) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
            Err(e) => return ExEffect::Error(format!("cannot write to `{cmd}`: {e}")),
        }
    }
    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => return ExEffect::Error(format!("`{cmd}` failed: {e}")),
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let trimmed = stderr.trim();
        let label = if trimmed.is_empty() {
            "no stderr".to_string()
        } else {
            trimmed.to_string()
        };
        return ExEffect::Error(format!(
            "command exited {} ({label})",
            output
                .status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "?".into())
        ));
    }
    let stdout = match String::from_utf8(output.stdout) {
        Ok(s) => s,
        Err(_) => return ExEffect::Error("filter output was not UTF-8".into()),
    };
    let trimmed = stdout.strip_suffix('\n').unwrap_or(&stdout);
    let new_rows: Vec<String> = trimmed.split('\n').map(String::from).collect();

    editor.push_undo();
    let after: Vec<String> = all_lines.split_off(bot + 1);
    all_lines.truncate(start);
    all_lines.extend(new_rows);
    all_lines.extend(after);
    editor.restore(all_lines, (start, 0));
    editor.mark_content_dirty();
    ExEffect::Ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use hjkl_engine::{DefaultHost, Editor, Options};

    fn make_editor_with_lines(lines: &[&str]) -> Editor<hjkl_buffer::Buffer, DefaultHost> {
        let content = lines.join("\n");
        let buf = hjkl_buffer::Buffer::from_str(&content);
        let host = DefaultHost::new();
        Editor::new(buf, host, Options::default())
    }

    fn sh_available() -> bool {
        std::process::Command::new("sh")
            .arg("-c")
            .arg("exit 0")
            .output()
            .is_ok()
    }

    #[test]
    fn shell_no_range_returns_info() {
        if !sh_available() {
            return;
        }
        let mut editor = make_editor_with_lines(&["hello"]);
        let result = shell_filter_handler(&mut editor, "echo hello", None);
        match result {
            ExEffect::Info(msg) => assert!(msg.contains("hello"), "got: {msg}"),
            other => panic!("expected Info(_), got {other:?}"),
        }
    }

    #[test]
    fn shell_empty_cmd_returns_error() {
        let mut editor = make_editor_with_lines(&["hello"]);
        let result = shell_filter_handler(&mut editor, "", None);
        assert!(matches!(result, ExEffect::Error(_)), "got: {result:?}");
    }

    #[cfg(unix)]
    #[test]
    fn shell_range_filter_sorts_lines() {
        if !sh_available() {
            return;
        }
        let mut editor = make_editor_with_lines(&["banana", "apple", "cherry"]);
        let range = LineRange::new(1, 3);
        let result = shell_filter_handler(&mut editor, "sort", Some(range));
        assert_eq!(result, ExEffect::Ok, "got: {result:?}");
        let lines = editor.buffer().lines().to_vec();
        assert_eq!(lines[0], "apple");
        assert_eq!(lines[1], "banana");
        assert_eq!(lines[2], "cherry");
    }
}
