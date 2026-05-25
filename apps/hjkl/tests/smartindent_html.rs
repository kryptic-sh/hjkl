//! Integration tests for the dumb smartindent's HTML opening-tag bump.
//!
//! When the cursor sits at the end of a line whose final non-whitespace
//! token is an HTML opening tag (`<head>`, `<div class="foo">`, etc.),
//! pressing Enter must indent the new line one `shiftwidth` deeper —
//! same shape as the existing `{` / `(` / `[` bump. Gated on the HTML
//! family of filetypes (html, xml, svg, jsx, tsx, vue, svelte) so that
//! Rust generics like `Vec<T>` don't trigger an unwanted bump.
//!
//! Self-closing tags (`<br />`) and void elements (`<br>`, `<img>`)
//! are explicitly skipped — they don't open a block.

use hjkl_buffer::Buffer;
use hjkl_engine::{DefaultHost, Editor, Options};
use hjkl_vim::{dispatch_input, insert::step_insert};

fn editor(lang: &str, content: &str) -> Editor<Buffer, DefaultHost> {
    let buf = Buffer::from_str(content);
    let host = DefaultHost::new();
    let opts = Options {
        filetype: lang.to_string(),
        formatoptions: "ro".to_string(),
        autoindent: true,
        smartindent: true,
        expandtab: true,
        tabstop: 4,
        shiftwidth: 4,
        softtabstop: 4,
        ..Options::default()
    };
    Editor::new(buf, host, opts)
}

fn feed(ed: &mut Editor<Buffer, DefaultHost>, keys: &str) {
    use hjkl_engine::{Input, Key};
    for ch in keys.chars() {
        let input = match ch {
            '\n' => Input {
                key: Key::Enter,
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
            '\n' => Input {
                key: Key::Enter,
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
    ed.buffer()
        .rope()
        .lines()
        .map(|s| {
            let s = s.to_string();
            s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
        })
        .collect::<Vec<_>>()
}

#[test]
fn html_opening_tag_bumps_indent_on_enter() {
    let mut ed = editor("html", "<head>");
    feed(&mut ed, "A"); // Insert at end-of-line.
    feed_insert(&mut ed, "\n");
    let l = lines(&ed);
    assert_eq!(
        l,
        vec!["<head>".to_string(), "    ".to_string()],
        "Enter after <head> must produce a 4-space-indented new line; got {l:?}"
    );
}

#[test]
fn html_opening_tag_with_attributes_bumps_indent() {
    let mut ed = editor("html", "<div class=\"foo\">");
    feed(&mut ed, "A");
    feed_insert(&mut ed, "\n");
    let l = lines(&ed);
    assert_eq!(
        l,
        vec!["<div class=\"foo\">".to_string(), "    ".to_string()],
        "Enter after a tag with attributes must still bump; got {l:?}"
    );
}

#[test]
fn html_self_closing_tag_does_not_bump_indent() {
    let mut ed = editor("html", "<br />");
    feed(&mut ed, "A");
    feed_insert(&mut ed, "\n");
    let l = lines(&ed);
    assert_eq!(
        l,
        vec!["<br />".to_string(), String::new()],
        "self-closing `<br />` must NOT bump indent; got {l:?}"
    );
}

#[test]
fn html_void_element_does_not_bump_indent() {
    let mut ed = editor("html", "<br>");
    feed(&mut ed, "A");
    feed_insert(&mut ed, "\n");
    let l = lines(&ed);
    assert_eq!(
        l,
        vec!["<br>".to_string(), String::new()],
        "void element `<br>` must NOT bump indent; got {l:?}"
    );
}

#[test]
fn html_closing_tag_does_not_bump_indent() {
    let mut ed = editor("html", "</head>");
    feed(&mut ed, "A");
    feed_insert(&mut ed, "\n");
    let l = lines(&ed);
    assert_eq!(
        l,
        vec!["</head>".to_string(), String::new()],
        "closing tag `</head>` must NOT bump indent; got {l:?}"
    );
}

#[test]
fn nested_html_bump_preserves_existing_indent() {
    let mut ed = editor("html", "    <head>");
    feed(&mut ed, "A");
    feed_insert(&mut ed, "\n");
    let l = lines(&ed);
    assert_eq!(
        l,
        vec!["    <head>".to_string(), "        ".to_string()],
        "nested tag must add a level on top of existing indent; got {l:?}"
    );
}

#[test]
fn non_html_filetype_does_not_bump_after_generic() {
    // Rust generics like `Vec<T>` must NOT trigger the html-tag bump.
    let mut ed = editor("rust", "let v: Vec<T>");
    feed(&mut ed, "A");
    feed_insert(&mut ed, "\n");
    let l = lines(&ed);
    assert_eq!(
        l,
        vec!["let v: Vec<T>".to_string(), String::new()],
        "Rust generic `<T>` must NOT trigger the html-tag indent bump; got {l:?}"
    );
}

#[test]
fn html_open_brace_still_bumps_indent() {
    // Regression — language-agnostic `{` bump still fires inside html
    // filetype (e.g. inline `<style>` JS-like CSS rules).
    let mut ed = editor("html", "  body {");
    feed(&mut ed, "A");
    feed_insert(&mut ed, "\n");
    let l = lines(&ed);
    assert_eq!(
        l,
        vec!["  body {".to_string(), "      ".to_string()],
        "open-brace bump must still work inside html filetype; got {l:?}"
    );
}
