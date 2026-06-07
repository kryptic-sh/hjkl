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
    // --cmd "set modeline modelines=5": nvim --clean disables modeline by
    // default; re-enable so modeline oracle cases match vim's behaviour.
    let mut cmd = Command::new("nvim");
    cmd.args([
        "--headless",
        "--embed",
        "--clean",
        "-n",
        "--cmd",
        "set modeline modelines=5",
    ]);
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

    // 3b. Apply per-case indent settings so `>>` / `<<` match hjkl's output.
    //     `nvim --clean` defaults to noexpandtab / shiftwidth=8, which diverge
    //     from hjkl's defaults; the corpus pins both sides explicitly.
    //     `startofline` is set whenever indent settings are pinned: nvim
    //     defaults it OFF (cursor keeps its column after `>>`) whereas hjkl
    //     follows traditional vim (cursor → first non-blank), so align nvim
    //     with hjkl for these cases.
    let pins_indent = case.shiftwidth.is_some() || case.expandtab.is_some();
    if let Some(sw) = case.shiftwidth {
        nvim.command(&format!("set shiftwidth={sw} tabstop={sw}"))
            .await?;
    }
    if let Some(et) = case.expandtab {
        nvim.command(if et {
            "set expandtab"
        } else {
            "set noexpandtab"
        })
        .await?;
    }
    if pins_indent {
        nvim.command("set startofline").await?;
    }
    if let Some(tw) = case.textwidth {
        nvim.command(&format!("set textwidth={tw}")).await?;
    }
    if let Some(ai) = case.autoindent {
        nvim.command(if ai {
            "set autoindent"
        } else {
            "set noautoindent"
        })
        .await?;
    }

    // 4. Apply keystrokes.
    //    `nvim_input` reads `<` as the start of a key-notation token (`<Esc>`,
    //    `<C-w>`, ...) and *blocks* waiting for the closing `>` if one never
    //    arrives — so a literal `<` (e.g. the `<<` outdent operator) would hang
    //    the RPC. Escape any `<` that does not open a valid token to `<lt>`.
    if !case.keys.is_empty() {
        nvim.input(&escape_literal_lt(&case.keys)).await?;
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

/// Rewrite any `<` that does NOT open a valid key-notation token (`<Esc>`,
/// `<C-w>`, `<lt>`, ...) into the literal-`<` escape `<lt>`.
///
/// `nvim_input` blocks waiting for a closing `>` when it sees an unterminated
/// `<...>`, so a bare `<` (the `<<` outdent operator) would hang the RPC. A `<`
/// is treated as a token opener only when a matching `>` follows with a
/// plausible key name in between (`[A-Za-z][A-Za-z0-9_-]*`).
fn escape_literal_lt(keys: &str) -> String {
    let bytes = keys.as_bytes();
    let mut out = String::with_capacity(keys.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            if let Some(end) = token_end(&bytes[i..]) {
                out.push_str(&keys[i..i + end]);
                i += end;
                continue;
            }
            out.push_str("<lt>");
            i += 1;
            continue;
        }
        // Copy one full UTF-8 char (keys are ASCII in practice, but be safe).
        let ch = keys[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// If `s` starts with a valid `<Name>` token, return its byte length (including
/// the angle brackets); otherwise `None`. `Name` must start with a letter and
/// contain only `[A-Za-z0-9_-]`.
fn token_end(s: &[u8]) -> Option<usize> {
    debug_assert_eq!(s[0], b'<');
    if s.len() < 3 || !s[1].is_ascii_alphabetic() {
        return None;
    }
    let mut j = 2;
    while j < s.len() {
        match s[j] {
            b'>' => return Some(j + 1),
            c if c.is_ascii_alphanumeric() || c == b'-' || c == b'_' => j += 1,
            _ => return None,
        }
    }
    None
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

#[cfg(test)]
mod escape_tests {
    use super::escape_literal_lt;

    #[test]
    fn literal_double_left_angle_is_escaped() {
        assert_eq!(escape_literal_lt("10<<"), "10<lt><lt>");
        assert_eq!(escape_literal_lt("5<<"), "5<lt><lt>");
    }

    #[test]
    fn right_angle_is_untouched() {
        assert_eq!(escape_literal_lt("3>>"), "3>>");
        assert_eq!(escape_literal_lt("Vj>"), "Vj>");
    }

    #[test]
    fn valid_key_tokens_pass_through() {
        assert_eq!(escape_literal_lt("i x<Esc>"), "i x<Esc>");
        assert_eq!(escape_literal_lt("<C-w>v"), "<C-w>v");
        assert_eq!(escape_literal_lt("a<CR>b"), "a<CR>b");
    }

    #[test]
    fn unterminated_or_invalid_angle_is_escaped() {
        // No closing '>' at all.
        assert_eq!(escape_literal_lt("a<b"), "a<lt>b");
        // '<' followed by a non-letter is never a token opener.
        assert_eq!(escape_literal_lt("<1>"), "<lt>1>");
    }

    #[test]
    fn mixed_token_and_literal() {
        assert_eq!(escape_literal_lt("<Esc>2<<"), "<Esc>2<lt><lt>");
    }
}
