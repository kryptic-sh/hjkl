//! Regression coverage for audit findings D1 / D4: `toggle_comment_range`
//! and `filter_range` used to rebuild the ENTIRE document as a
//! `Vec<String>` + rejoin on every call — O(document size) for a
//! range-scoped edit. Both now splice through a single bounded
//! `hjkl_buffer::Edit::Replace` (`Editor::splice_row_range`), which is
//! O(edit size).
//!
//! These tests pin content + cursor to the exact values the OLD
//! whole-document `Vec<String>` reconstruction produced, across the row
//! positions that exercise the boundary-newline edge cases the fix has
//! to get exactly right: a range in the middle of the buffer, the first
//! row, and the last row of a buffer with no trailing newline (vim's
//! "the last line never has a trailing newline" invariant) — plus, for
//! `filter_range`, ranges whose replacement has a different row count
//! than the original range (a filter can add, remove, or keep lines).

use hjkl_engine::{DefaultHost, Editor, Options};

fn rust_editor(content: &str) -> Editor<hjkl_buffer::View, DefaultHost> {
    let opts = Options {
        filetype: "rust".to_string(),
        ..Options::default()
    };
    let mut e = Editor::new(hjkl_buffer::View::new(), DefaultHost::new(), opts);
    e.set_content(content);
    e
}

fn plain_editor(content: &str) -> Editor<hjkl_buffer::View, DefaultHost> {
    let mut e = Editor::new(
        hjkl_buffer::View::new(),
        DefaultHost::new(),
        Options::default(),
    );
    e.set_content(content);
    e
}

fn content_of(e: &Editor<hjkl_buffer::View, DefaultHost>) -> String {
    e.buffer().as_string()
}

// ── toggle_comment_range ──────────────────────────────────────────────────

#[test]
fn comment_single_line_add() {
    let mut e = rust_editor("let x = 1;");
    e.toggle_comment_range(0, 0);
    assert_eq!(content_of(&e), "// let x = 1;");
    assert_eq!(e.cursor(), (0, 0));
}

#[test]
fn comment_single_line_remove() {
    let mut e = rust_editor("// let x = 1;");
    e.toggle_comment_range(0, 0);
    assert_eq!(content_of(&e), "let x = 1;");
    assert_eq!(e.cursor(), (0, 0));
}

#[test]
fn comment_multi_line_range_middle_of_buffer() {
    // Rows 0 and 4 must be untouched — only the [1, 3] range is rewritten.
    let mut e = rust_editor("let z0 = 0;\nlet a = 1;\nlet b = 2;\nlet c = 3;\nlet z4 = 4;");
    e.toggle_comment_range(1, 3);
    assert_eq!(
        content_of(&e),
        "let z0 = 0;\n// let a = 1;\n// let b = 2;\n// let c = 3;\nlet z4 = 4;"
    );
    assert_eq!(e.cursor(), (1, 0));
}

#[test]
fn comment_first_line_of_buffer_with_trailing_newline() {
    let mut e = rust_editor("let a = 1;\nlet b = 2;\nlet c = 3;\n");
    e.toggle_comment_range(0, 0);
    assert_eq!(content_of(&e), "// let a = 1;\nlet b = 2;\nlet c = 3;\n");
    assert_eq!(e.cursor(), (0, 0));
}

#[test]
fn comment_last_line_of_buffer_with_no_trailing_newline() {
    // No trailing '\n' — this is vim's canonical "file without a final
    // newline" shape. Toggling the true last row must not grow one.
    let mut e = rust_editor("let a = 1;\nlet b = 2;\nlet c = 3;");
    e.toggle_comment_range(2, 2);
    assert_eq!(content_of(&e), "let a = 1;\nlet b = 2;\n// let c = 3;");
    assert_eq!(e.cursor(), (2, 0));
}

#[test]
fn comment_whole_buffer_no_trailing_newline() {
    let mut e = rust_editor("let a = 1;\nlet b = 2;");
    e.toggle_comment_range(0, 1);
    assert_eq!(content_of(&e), "// let a = 1;\n// let b = 2;");
    assert_eq!(e.cursor(), (0, 0));
}

// ── filter_range ────────────────────────────────────────────────────────

#[test]
fn filter_identity_single_line() {
    let mut e = plain_editor("alpha");
    let result = e.filter_range(0, 0, "cat", None);
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(content_of(&e), "alpha");
    assert_eq!(e.cursor(), (0, 0));
}

#[test]
fn filter_multiline_reorders_without_touching_row_count() {
    let mut e = plain_editor("banana\napple\ncherry");
    let result = e.filter_range(0, 2, "sort", None);
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(content_of(&e), "apple\nbanana\ncherry");
    assert_eq!(e.cursor(), (0, 0));
}

#[test]
fn filter_first_line_only_leaves_rest_of_buffer_byte_identical() {
    let mut e = plain_editor("charlie\nalpha\nbravo\n");
    let result = e.filter_range(0, 0, "tr a-z A-Z", None);
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(content_of(&e), "CHARLIE\nalpha\nbravo\n");
    assert_eq!(e.cursor(), (0, 0));
}

#[test]
fn filter_last_line_no_trailing_newline_stays_trailing_newline_free() {
    let mut e = plain_editor("alpha\nbravo\ncharlie");
    let result = e.filter_range(2, 2, "tr a-z A-Z", None);
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(content_of(&e), "alpha\nbravo\nCHARLIE");
    assert_eq!(e.cursor(), (2, 0));
}

/// Filter output with FEWER lines than the input range (`true` emits
/// nothing) — the range must be spliced out entirely, not left as a
/// blank-line gap. Range sits in the middle of the buffer.
#[test]
fn filter_empty_output_deletes_range_in_middle_of_buffer() {
    let mut e = plain_editor("one\ntwo\nthree\nfour\n");
    let result = e.filter_range(1, 2, "true", None);
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(content_of(&e), "one\nfour\n");
    assert_eq!(
        e.cursor(),
        (1, 0),
        "row 1 now holds \"four\" — still a valid row"
    );
}

/// Same as above but the deleted range runs to the buffer's last row
/// (no trailing newline) — must not leave a dangling trailing newline
/// on the row that becomes the new last row.
#[test]
fn filter_empty_output_deletes_range_at_end_of_buffer() {
    let mut e = plain_editor("one\ntwo\nthree");
    let result = e.filter_range(1, 2, "true", None);
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(content_of(&e), "one");
    assert_eq!(e.cursor(), (0, 0));
}

/// Filter output with MORE lines than the input range (each line
/// doubled) — spliced into the middle of a larger buffer.
#[test]
fn filter_output_grows_line_count_in_middle_of_buffer() {
    let mut e = plain_editor("x\na\nb\ny\n");
    let result = e.filter_range(1, 2, "awk '{print;print}'", None);
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(content_of(&e), "x\na\na\nb\nb\ny\n");
    assert_eq!(e.cursor(), (1, 0));
}

/// Filter output with more lines than input, replacing the WHOLE buffer
/// (no trailing newline before or after).
#[test]
fn filter_output_grows_line_count_over_whole_buffer() {
    let mut e = plain_editor("a\nb");
    let result = e.filter_range(0, 1, "awk '{print;print}'", None);
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(content_of(&e), "a\na\nb\nb");
    assert_eq!(e.cursor(), (0, 0));
}

#[test]
fn filter_error_leaves_buffer_untouched() {
    let mut e = plain_editor("line1\nline2");
    let before = content_of(&e);
    let result = e.filter_range(0, 1, "__hjkl_no_such_cmd_xyz__", None);
    assert!(result.is_err());
    assert_eq!(content_of(&e), before);
}

// ── Perf-shaped guard (audit D1) ──────────────────────────────────────────

/// `gcc` on ONE line of a large buffer must be fast — the whole point of
/// the fix. Before it, `toggle_comment_range` rebuilt the entire document
/// as a `Vec<String>` (one heap allocation per line) on every call, so
/// this would scale with document size instead of edit size. 50k rows is
/// comfortably enough to make an O(document) implementation visible while
/// staying well under a CI-safe budget for the O(edit) one.
#[test]
fn toggle_comment_on_one_line_of_large_buffer_is_fast() {
    let content = (0..50_000)
        .map(|i| format!("let v{i} = {i};"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut e = rust_editor(&content);

    let start = std::time::Instant::now();
    e.toggle_comment_range(25_000, 25_000);
    let elapsed = start.elapsed();

    assert_eq!(
        hjkl_buffer::rope_line_str(&e.buffer().rope(), 25_000),
        "// let v25000 = 25000;"
    );
    assert!(
        elapsed.as_millis() < 100,
        "gcc on one line of a 50k-line buffer took {elapsed:?}; \
         budget 100ms — an O(document) implementation would blow this"
    );
}
