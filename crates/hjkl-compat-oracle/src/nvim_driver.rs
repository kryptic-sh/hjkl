//! Drive a headless neovim process with an [`OracleCase`] and return the
//! outcome.

use crate::OracleCase;
use async_trait::async_trait;
use nvim_rs::{Handler, Neovim, Value, compat::tokio::Compat, create::tokio as create};
use tokio::process::ChildStdin;

/// State snapshot produced after replaying a case's keystrokes through neovim.
pub struct NvimOutcome {
    /// Full buffer content joined with `\n`. Trailing `\n` re-applied when the
    /// original `initial_buffer` ended with `\n` (nvim strips trailing empty
    /// lines from `get_lines`).
    pub buffer: String,
    /// `(row, col)` cursor, 0-based row, byte-col (mirrors nvim's encoding).
    pub cursor: (usize, usize),
    /// Lowercase mode name matching [`crate::hjkl_driver::HjklOutcome::mode`].
    pub mode: String,
    /// Contents of the default `"` register.
    pub default_register: String,
}

// ── Noop handler ──────────────────────────────────────────────────────────────

/// Noop [`Handler`] — we never handle incoming requests or notifications from
/// the embedded nvim instance; we only send requests.
#[derive(Clone)]
struct NoopHandler;

#[async_trait]
impl Handler for NoopHandler {
    type Writer = Compat<ChildStdin>;
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Returns `true` when `nvim` is on `PATH` and exits cleanly with `--version`.
pub fn nvim_available() -> bool {
    std::process::Command::new("nvim")
        .arg("--version")
        .output()
        .is_ok()
}

/// Run `case` through a freshly-spawned headless neovim and return the state.
///
/// # Errors
///
/// Returns an error if nvim fails to spawn, if any RPC call fails, or if the
/// mode / register values cannot be extracted from the msgpack response.
pub async fn run_case(case: &OracleCase) -> anyhow::Result<NvimOutcome> {
    use tokio::process::Command;

    // 1. Spawn nvim in headless embedded mode.
    let mut cmd = Command::new("nvim");
    cmd.args(["--headless", "--embed", "--clean", "-n"]);
    let (nvim, _io_handle, mut child) = create::new_child_cmd(&mut cmd, NoopHandler).await?;

    let result = run_case_inner(&nvim, case).await;

    // Cleanly quit nvim; ignore shutdown errors.
    let _ = nvim.command("qa!").await;
    let _ = child.wait().await;

    result
}

async fn run_case_inner(
    nvim: &Neovim<Compat<ChildStdin>>,
    case: &OracleCase,
) -> anyhow::Result<NvimOutcome> {
    // 2. Set buffer content.
    //    nvim expects lines WITHOUT a trailing empty entry even when the content
    //    ends with '\n' (the implicit final newline is part of every vim buffer).
    let has_trailing_newline = case.initial_buffer.ends_with('\n');
    let mut lines: Vec<String> = case.initial_buffer.split('\n').map(str::to_owned).collect();
    // split('\n') on "hello\n" yields ["hello", ""] — drop the trailing empty.
    if has_trailing_newline && lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }

    let cur_buf = nvim.get_current_buf().await?;
    cur_buf.set_lines(0, -1, false, lines.clone()).await?;

    // 3. Set initial cursor (nvim: 1-based row, 0-based byte-col).
    let (init_row, init_col) = case.initial_cursor;
    let cur_win = nvim.get_current_win().await?;
    cur_win
        .set_cursor((init_row as i64 + 1, init_col as i64))
        .await?;

    // 4. Apply keystrokes.
    if !case.keys.is_empty() {
        nvim.input(&case.keys).await?;
    }

    // 5. Synchronisation barrier — a round-trip ensures the previous input is
    //    fully processed before we read back state.
    nvim.command("echo 1").await?;

    // 6. Read back buffer.
    let raw_lines = cur_buf.get_lines(0, -1, false).await?;
    let mut buf_str = raw_lines.join("\n");
    // Re-apply the trailing newline that the original buffer had.
    if has_trailing_newline {
        buf_str.push('\n');
    }

    // 7. Read back cursor (convert from 1-based row to 0-based).
    let (nvim_row, nvim_col) = cur_win.get_cursor().await?;
    let cursor = ((nvim_row - 1) as usize, nvim_col as usize);

    // 8. Read back mode.
    let mode_pairs = nvim.get_mode().await?;
    let mode_char = mode_pairs
        .into_iter()
        .find_map(|(k, v)| {
            if k == Value::from("mode") {
                v.as_str().map(str::to_owned)
            } else {
                None
            }
        })
        .unwrap_or_default();
    let mode = nvim_mode_to_string(&mode_char);

    // 9. Read back default register.
    let reg_val = nvim
        .call_function("getreg", vec![Value::from("\"")])
        .await?;
    let default_register = reg_val.as_str().unwrap_or("").to_owned();

    Ok(NvimOutcome {
        buffer: buf_str,
        cursor,
        mode,
        default_register,
    })
}

/// Map nvim's short mode codes to the canonical lowercase strings used by the
/// oracle. Unknown codes are passed through so mismatches surface in the diff.
fn nvim_mode_to_string(code: &str) -> String {
    match code {
        "n" => "normal",
        "i" => "insert",
        "v" => "visual",
        "V" => "visual_line",
        "\u{16}" => "visual_block", // Ctrl-V
        other => return other.to_owned(),
    }
    .to_owned()
}
