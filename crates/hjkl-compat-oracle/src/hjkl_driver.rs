//! Drive [`hjkl_engine`] with an [`OracleCase`] and return the outcome.
//!
//! Two drivers are provided:
//!
//! - [`run_case`] — in-process driver (fast, no subprocess). Used by default.
//! - [`run_case_via_nvim_api`] — subprocess driver that spawns `hjkl --nvim-api`
//!   and drives it over msgpack-rpc using the same protocol as nvim-rs.
//!   Enabled by setting `HJKL_ORACLE_NVIM_API=1`.

use crate::{OracleCase, test_host::TestHost};
use hjkl_engine::{Editor, Options, VimMode, decode_macro};

/// State snapshot produced after replaying a case's keystrokes through the
/// hjkl engine.
pub struct HjklOutcome {
    /// Full buffer content, lines joined with `\n`. Trailing `\n` is present
    /// when the last logical line of the buffer is empty (mirrors vim's
    /// convention).
    pub buffer: String,
    /// `(row, col)` cursor, 0-based. `col` is a **char index** within the line
    /// (the engine uses char-indexed positions internally; byte-col conversion
    /// is left to callers that need it for comparison against nvim's byte-col).
    pub cursor: (usize, usize),
    /// Lowercase mode name: `"normal"`, `"insert"`, `"visual"`,
    /// `"visual_line"`, or `"visual_block"`.
    pub mode: String,
    /// Contents of the default `"` register after the keystroke sequence.
    pub default_register: String,
}

/// Run `case` through the hjkl engine and return the resulting state.
///
/// # Errors
///
/// Propagates any error from buffer construction. Engine `step` is
/// infallible; failures (unknown key, mode mismatch) surface as state
/// divergence rather than errors.
pub fn run_case(case: &OracleCase) -> anyhow::Result<HjklOutcome> {
    // 1. Build buffer from the initial content.
    let buffer = hjkl_buffer::Buffer::from_str(&case.initial_buffer);

    // 2. Construct editor.
    let mut editor = Editor::new(buffer, TestHost::new(), Options::default());

    // 3. Set initial cursor.
    let (init_row, init_col) = case.initial_cursor;
    editor.jump_cursor(init_row, init_col);

    // 4. Parse and replay keystrokes.
    let inputs = decode_macro(&case.keys);
    for input in inputs {
        hjkl_engine::step(&mut editor, input);
    }

    // 5. Read back state.
    let lines = editor.buffer().lines().to_vec();
    // Reconstruct the buffer string: join with '\n'. If the last line is empty
    // (buffer originally ended with '\n'), the join produces a trailing '\n'.
    let buffer_str = lines.join("\n");

    let cursor = editor.cursor();

    let mode = match editor.vim_mode() {
        VimMode::Normal => "normal",
        VimMode::Insert => "insert",
        VimMode::Visual => "visual",
        VimMode::VisualLine => "visual_line",
        VimMode::VisualBlock => "visual_block",
    }
    .to_string();

    let default_register = editor.yank().to_string();

    Ok(HjklOutcome {
        buffer: buffer_str,
        cursor,
        mode,
        default_register,
    })
}

// ── nvim-api subprocess driver ─────────────────────────────────────────────────

use async_trait::async_trait;
use nvim_rs::{Handler, Neovim, Value, compat::tokio::Compat, create::tokio as create};
use tokio::process::ChildStdin;

/// Noop handler — we only send requests to hjkl.
#[derive(Clone)]
struct NoopHandler;

#[async_trait]
impl Handler for NoopHandler {
    type Writer = Compat<ChildStdin>;
}

/// Run `case` by spawning `hjkl --nvim-api` and driving it over msgpack-rpc.
///
/// This driver validates that the nvim-api surface produces the same results
/// as the in-process driver. Enable via `HJKL_ORACLE_NVIM_API=1`.
///
/// The binary is located via the `HJKL_BIN` environment variable (preferred)
/// or falls back to `target/debug/hjkl` relative to the workspace root
/// (good enough for local `cargo test`; CI should set `HJKL_BIN`).
pub async fn run_case_via_nvim_api(case: &OracleCase) -> anyhow::Result<HjklOutcome> {
    use tokio::process::Command;

    let bin = std::env::var("HJKL_BIN").unwrap_or_else(|_| {
        // Derive from CARGO_MANIFEST_DIR: oracle is two levels below workspace root.
        let manifest = env!("CARGO_MANIFEST_DIR");
        let workspace_root = std::path::Path::new(manifest)
            .parent() // crates/
            .and_then(|p| p.parent()) // workspace root
            .map(|p| p.join("target/debug/hjkl"))
            .unwrap_or_else(|| std::path::PathBuf::from("hjkl"));
        workspace_root.to_string_lossy().into_owned()
    });

    let mut cmd = Command::new(&bin);
    cmd.arg("--nvim-api");
    let (nvim, _io_handle, mut child) = create::new_child_cmd(&mut cmd, NoopHandler).await?;

    let result = run_case_via_nvim_api_inner(&nvim, case).await;

    // Shut down hjkl gracefully.
    let _ = nvim.command("q!").await;
    let _ = child.wait().await;

    result
}

async fn run_case_via_nvim_api_inner(
    nvim: &Neovim<Compat<ChildStdin>>,
    case: &OracleCase,
) -> anyhow::Result<HjklOutcome> {
    // 1. Set buffer content.
    let has_trailing_newline = case.initial_buffer.ends_with('\n');
    let mut lines: Vec<String> = case.initial_buffer.split('\n').map(str::to_owned).collect();
    if has_trailing_newline && lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }

    let cur_buf = nvim.get_current_buf().await?;
    cur_buf.set_lines(0, -1, false, lines).await?;

    // 2. Set initial cursor (nvim: 1-based row, 0-based byte-col).
    let (init_row, init_col) = case.initial_cursor;
    let cur_win = nvim.get_current_win().await?;
    cur_win
        .set_cursor((init_row as i64 + 1, init_col as i64))
        .await?;

    // 3. Apply keystrokes.
    if !case.keys.is_empty() {
        nvim.input(&case.keys).await?;
    }

    // 4. Sync barrier.
    nvim.command("echo 1").await?;

    // 5. Read back buffer.
    let raw_lines = cur_buf.get_lines(0, -1, false).await?;
    let mut buf_str = raw_lines.join("\n");
    if has_trailing_newline {
        buf_str.push('\n');
    }

    // 6. Read back cursor (convert from 1-based row to 0-based).
    let (nvim_row, nvim_col) = cur_win.get_cursor().await?;
    let cursor = ((nvim_row - 1) as usize, nvim_col as usize);

    // 7. Read back mode.
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

    // 8. Read back default register.
    let reg_val = nvim
        .call_function("getreg", vec![Value::from("\"")])
        .await?;
    let default_register = reg_val.as_str().unwrap_or("").to_owned();

    Ok(HjklOutcome {
        buffer: buf_str,
        cursor,
        mode,
        default_register,
    })
}

/// Map nvim's short mode codes to the canonical lowercase strings used by the
/// oracle (same mapping as nvim_driver.rs).
fn nvim_mode_to_string(code: &str) -> String {
    match code {
        "n" => "normal",
        "i" => "insert",
        "v" => "visual",
        "V" => "visual_line",
        "\u{16}" => "visual_block",
        other => return other.to_owned(),
    }
    .to_owned()
}
