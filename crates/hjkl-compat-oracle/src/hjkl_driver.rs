//! Drive [`hjkl_engine`] with an [`OracleCase`] and return the outcome.

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
