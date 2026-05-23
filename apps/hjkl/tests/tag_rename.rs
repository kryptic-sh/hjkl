//! Integration tests for auto-rename of matching HTML/XML tag pairs on
//! Esc-from-insert (issue #182).
//!
//! Scenario: cursor in `<test>some text</test>`, run `ci<` (change inside
//! the `<` text-object — though here we just simulate the resulting state),
//! type a new name, press Esc. The matching closing tag should auto-rename
//! so the pair stays in sync.
//!
//! Implementation triggers on `leave_insert_to_normal_bridge` when the
//! filetype is one of the HTML family (html / xml / svg / jsx / tsx / vue /
//! svelte). Pairing uses a stack-based scan so nested same-name tags
//! (`<div><div></div></div>`) resolve correctly.

use hjkl_buffer::Buffer;
use hjkl_engine::{DefaultHost, Editor, Options};
use hjkl_vim::{dispatch_input, insert::step_insert};

fn editor(lang: &str, content: &str) -> Editor<Buffer, DefaultHost> {
    let buf = Buffer::from_str(content);
    let host = DefaultHost::new();
    let opts = Options {
        filetype: lang.to_string(),
        ..Options::default()
    };
    Editor::new(buf, host, opts)
}

fn feed(ed: &mut Editor<Buffer, DefaultHost>, keys: &str) {
    use hjkl_engine::{Input, Key};
    for ch in keys.chars() {
        let input = match ch {
            '\x1b' => Input {
                key: Key::Esc,
                ..Input::default()
            },
            _ => Input {
                key: Key::Char(ch),
                ..Input::default()
            },
        };
        dispatch_input(ed, input);
    }
}

fn feed_insert(ed: &mut Editor<Buffer, DefaultHost>, keys: &str) {
    use hjkl_engine::{Input, Key};
    for ch in keys.chars() {
        let input = match ch {
            '\x1b' => Input {
                key: Key::Esc,
                ..Input::default()
            },
            _ => Input {
                key: Key::Char(ch),
                ..Input::default()
            },
        };
        step_insert(ed, input);
    }
}

fn lines(ed: &Editor<Buffer, DefaultHost>) -> Vec<String> {
    ed.buffer().lines().to_vec()
}

/// Edit the opener name → closer must auto-rename to match.
#[test]
fn edit_opener_renames_closer() {
    // Mid-edit state: opener renamed to "newtag", closer still "test".
    // Cursor inside opener name. Esc-from-insert triggers the sync.
    let mut ed = editor("html", "<newtag>some text</test>");
    ed.jump_cursor(0, 4);
    feed(&mut ed, "i");
    feed_insert(&mut ed, "\x1b");
    assert_eq!(
        lines(&ed),
        vec!["<newtag>some text</newtag>".to_string()],
        "closer must rename to match opener"
    );
}

/// Edit the closer name → opener must auto-rename to match.
#[test]
fn edit_closer_renames_opener() {
    let mut ed = editor("html", "<test>x</done>");
    // Cursor inside closer name (`d` of `done`, col 9).
    ed.jump_cursor(0, 9);
    feed(&mut ed, "i");
    feed_insert(&mut ed, "\x1b");
    assert_eq!(
        lines(&ed),
        vec!["<done>x</done>".to_string()],
        "opener must rename to match closer"
    );
}

/// Nested DIFFERENT-name tags must pair correctly: outer opener renamed
/// → only the OUTER closer renames; the inner `<div>` pair stays intact.
#[test]
fn nested_different_name_pairs_correctly() {
    // Mid-edit: outer opener renamed to "new", outer closer still "test".
    let mut ed = editor("html", "<new><div>x</div></test>");
    ed.jump_cursor(0, 2);
    feed(&mut ed, "i");
    feed_insert(&mut ed, "\x1b");
    assert_eq!(
        lines(&ed),
        vec!["<new><div>x</div></new>".to_string()],
        "stack-based scan must skip over the nested <div>/</div> pair"
    );
}

/// Non-HTML filetype must NOT trigger the rename. Rust generic `Vec<T>`
/// looks like an opener but is not HTML.
#[test]
fn non_html_filetype_does_not_rename() {
    let mut ed = editor("rust", "let v: Vec<T>; let w: Vec<U>;");
    ed.jump_cursor(0, 11);
    feed(&mut ed, "i");
    feed_insert(&mut ed, "\x1b");
    assert_eq!(
        lines(&ed),
        vec!["let v: Vec<T>; let w: Vec<U>;".to_string()],
        "Rust generic must NOT be touched by tag-rename"
    );
}

/// Void HTML elements (`<br>`, `<img>`, …) have no closing pair —
/// editing the void-element name must be a no-op.
#[test]
fn void_element_does_not_attempt_rename() {
    let mut ed = editor("html", "<br>");
    ed.jump_cursor(0, 1);
    feed(&mut ed, "i");
    feed_insert(&mut ed, "\x1b");
    assert_eq!(
        lines(&ed),
        vec!["<br>".to_string()],
        "void element must remain unchanged"
    );
}

/// Self-closing tags (`<Foo />`) have no separate close — no-op.
#[test]
fn self_closing_does_not_attempt_rename() {
    let mut ed = editor("tsx", "<Foo />");
    ed.jump_cursor(0, 1);
    feed(&mut ed, "i");
    feed_insert(&mut ed, "\x1b");
    assert_eq!(lines(&ed), vec!["<Foo />".to_string()]);
}

/// Opener and closer that already match must produce no-op (don't churn).
#[test]
fn already_matching_pair_is_noop() {
    let mut ed = editor("html", "<div>x</div>");
    ed.jump_cursor(0, 2);
    feed(&mut ed, "i");
    feed_insert(&mut ed, "\x1b");
    assert_eq!(lines(&ed), vec!["<div>x</div>".to_string()]);
}

/// Pair spanning multiple lines.
#[test]
fn rename_works_across_lines() {
    let mut ed = editor("html", "<new>\n  inner content\n</old>");
    // Cursor inside opener `<new>` (col 2, on `e`).
    ed.jump_cursor(0, 2);
    feed(&mut ed, "i");
    feed_insert(&mut ed, "\x1b");
    assert_eq!(
        lines(&ed),
        vec![
            "<new>".to_string(),
            "  inner content".to_string(),
            "</new>".to_string(),
        ],
        "rename must reach the closer on a later line"
    );
}
