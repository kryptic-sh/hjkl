//! Integration tests for auto-continue line comments (issue #180).
//!
//! Tests drive the engine directly (no binary spawn) to verify that
//! `<Enter>` in insert mode, `o`, and `O` in normal mode continue comment
//! prefixes, and that `<Backspace>` on a blank-after-prefix line strips the
//! whole prefix in one stroke.
//!
//! No temp files needed — all state lives in-memory.

mod common;

use hjkl_buffer::View;
use hjkl_engine::{DefaultHost, Editor, Options};

/// Build an editor pre-loaded with `content` and the given language.
/// The editor starts in Normal mode (default).
fn editor(lang: &str, content: &str) -> Editor<View, DefaultHost> {
    let buf = View::from_str(content);
    let host = DefaultHost::new();
    let opts = Options {
        filetype: lang.to_string(),
        formatoptions: "ro".to_string(),
        ..Options::default()
    };
    hjkl_vim::vim_editor(buf, host, opts)
}

/// Return the full buffer text as a String.
fn buf_text(ed: &Editor<View, DefaultHost>) -> String {
    ed.content()
}

// ---------------------------------------------------------------------------
// Insert-mode <Enter> continuation
// ---------------------------------------------------------------------------

/// `// foo<Enter>` on a Rust file → next line starts with `// `.
#[test]
fn enter_continues_rust_line_comment() {
    let mut ed = editor("rust", "// foo\n");
    // Position cursor at end of first line (after "// foo").
    // Enter normal mode, then 'A' to append at end, then Enter.
    common::feed(&mut ed, "A"); // enter insert mode at end of line
    common::feed_insert(&mut ed, "\n"); // press Enter
    // The new line should start with "// ".
    let text = buf_text(&ed);
    let lines: Vec<&str> = text.lines().collect();
    // Line 0 = "// foo", Line 1 = "// " (continued), possibly more.
    assert!(
        lines.len() >= 2,
        "expected at least 2 lines, got: {lines:?}"
    );
    assert!(
        lines[1].starts_with("// "),
        "line 2 should start with '// ', got: {:?}",
        lines[1]
    );
}

/// `/// outer doc<Enter>` → next line starts with `/// `.
#[test]
fn enter_continues_rust_outer_doc_comment() {
    let mut ed = editor("rust", "/// outer doc\n");
    common::feed(&mut ed, "A");
    common::feed_insert(&mut ed, "\n");
    let text = buf_text(&ed);
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines.get(1).is_some_and(|l| l.starts_with("/// ")),
        "line 2 should start with '/// ', got: {lines:?}"
    );
}

/// `//! inner doc<Enter>` → next line starts with `//! `.
#[test]
fn enter_continues_rust_inner_doc_comment() {
    let mut ed = editor("rust", "//! inner\n");
    common::feed(&mut ed, "A");
    common::feed_insert(&mut ed, "\n");
    let text = buf_text(&ed);
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines.get(1).is_some_and(|l| l.starts_with("//! ")),
        "line 2 should start with '//! ', got: {lines:?}"
    );
}

/// Python `# comment<Enter>` → next line starts with `# `.
#[test]
fn enter_continues_python_comment() {
    let mut ed = editor("python", "# comment\n");
    common::feed(&mut ed, "A");
    common::feed_insert(&mut ed, "\n");
    let text = buf_text(&ed);
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines.get(1).is_some_and(|l| l.starts_with("# ")),
        "line 2 should start with '# ', got: {lines:?}"
    );
}

/// Non-comment line: Enter must NOT insert a comment prefix.
#[test]
fn enter_does_not_continue_non_comment() {
    let mut ed = editor("rust", "let x = 1;\n");
    common::feed(&mut ed, "A");
    common::feed_insert(&mut ed, "\n");
    let text = buf_text(&ed);
    let lines: Vec<&str> = text.lines().collect();
    let line2 = lines.get(1).copied().unwrap_or("");
    assert!(
        !line2.starts_with("// "),
        "non-comment Enter should not add //, got: {line2:?}"
    );
}

/// With `formatoptions-=r`, Enter must NOT continue the comment.
#[test]
fn enter_no_continuation_when_r_cleared() {
    let mut ed = editor("rust", "// foo\n");
    ed.settings_mut().formatoptions = "o".to_string(); // r cleared
    common::feed(&mut ed, "A");
    common::feed_insert(&mut ed, "\n");
    let text = buf_text(&ed);
    let lines: Vec<&str> = text.lines().collect();
    let line2 = lines.get(1).copied().unwrap_or("");
    assert!(
        !line2.starts_with("// "),
        "r cleared → should not continue comment, got: {line2:?}"
    );
}

// ---------------------------------------------------------------------------
// Normal-mode `o` continuation
// ---------------------------------------------------------------------------

/// `o` on `// foo` opens `// ` below.
#[test]
fn open_below_continues_rust_comment() {
    let mut ed = editor("rust", "// foo\n");
    // Cursor is at row 0. Press 'o' (normal mode).
    common::feed(&mut ed, "o");
    // Now in insert mode. Check that the new line starts with `// `.
    let (row, _) = ed.cursor();
    assert_eq!(row, 1, "cursor should be on line 2 after 'o'");
    let text = buf_text(&ed);
    let lines: Vec<&str> = text.lines().collect();
    let line2 = lines.get(1).copied().unwrap_or("");
    assert!(
        line2.starts_with("// "),
        "open-below should start with '// ', got: {line2:?}"
    );
}

/// With `formatoptions-=o`, `o` must NOT continue the comment.
#[test]
fn open_below_no_continuation_when_o_cleared() {
    let mut ed = editor("rust", "// foo\n");
    ed.settings_mut().formatoptions = "r".to_string(); // o cleared
    common::feed(&mut ed, "o");
    let text = buf_text(&ed);
    let lines: Vec<&str> = text.lines().collect();
    let line2 = lines.get(1).copied().unwrap_or("");
    assert!(
        !line2.starts_with("// "),
        "o cleared → should not continue comment, got: {line2:?}"
    );
}

// ---------------------------------------------------------------------------
// Normal-mode `O` continuation
// ---------------------------------------------------------------------------

/// `O` on `// foo` opens `// ` above.
#[test]
fn open_above_continues_rust_comment() {
    let mut ed = editor("rust", "// foo\n");
    common::feed(&mut ed, "O");
    // After O, cursor is on the new line above (row 0), in insert mode.
    let text = buf_text(&ed);
    let lines: Vec<&str> = text.lines().collect();
    let line1 = lines.first().copied().unwrap_or("");
    assert!(
        line1.starts_with("// "),
        "open-above should start with '// ', got: {line1:?}"
    );
}

// ---------------------------------------------------------------------------
// Backspace strips whole prefix
// ---------------------------------------------------------------------------

/// Backspace on `// ` (just the prefix, cursor at end) removes all of it.
#[test]
fn backspace_strips_comment_prefix_at_end() {
    // View: "// \n" (the continued line with just the prefix).
    let mut ed = editor("rust", "// \n");
    // Go to end of line 0 (col 3, after the trailing space).
    common::feed(&mut ed, "A"); // enter insert, move to end
    // The line content is "// " and cursor is at col 3.
    // Pressing Backspace should remove the whole "// " prefix.
    common::feed_insert(&mut ed, "\x08");
    let text = buf_text(&ed);
    let line1 = text.lines().next().unwrap_or("");
    assert!(
        line1.is_empty(),
        "backspace should have stripped '// ', line is: {line1:?}"
    );
}
