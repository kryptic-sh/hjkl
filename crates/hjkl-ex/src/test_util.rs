//! Shared test helpers for the crate's unit-test modules.
//!
//! Every `mod tests` in this crate needs the same four things: an editor
//! (empty or seeded from lines) and a way to read the buffer back as strings.
//! They used to be re-declared verbatim in eight files; they live here now.

use hjkl_engine::{DefaultHost, Editor, Options};

/// Editor over an empty buffer with default options.
pub fn make_editor() -> Editor<hjkl_buffer::View, DefaultHost> {
    let buf = hjkl_buffer::View::new();
    let host = DefaultHost::new();
    hjkl_vim::vim_editor(buf, host, Options::default())
}

/// Editor over a buffer seeded with `lines` joined by `\n`, default options.
pub fn make_editor_with_lines(lines: &[&str]) -> Editor<hjkl_buffer::View, DefaultHost> {
    let content = lines.join("\n");
    let buf = hjkl_buffer::View::from_str(&content);
    let host = DefaultHost::new();
    hjkl_vim::vim_editor(buf, host, Options::default())
}

/// Logical line `row` of the editor's buffer, without the trailing newline.
pub fn buf_line(editor: &Editor<hjkl_buffer::View, DefaultHost>, row: usize) -> String {
    hjkl_buffer::rope_line_str(&editor.buffer().rope(), row)
}

/// All logical lines of the editor's buffer, without trailing newlines.
pub fn buf_lines(editor: &Editor<hjkl_buffer::View, DefaultHost>) -> Vec<String> {
    let rope = editor.buffer().rope();
    (0..rope.len_lines())
        .map(|i| hjkl_buffer::rope_line_str(&rope, i))
        .collect()
}
