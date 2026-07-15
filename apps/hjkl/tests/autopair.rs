//! Integration tests for autopair brackets/quotes + auto-close HTML/XML tags
//! + skip-over on duplicate (issue #181).
//!
//! Tests drive the engine directly (no binary spawn) to verify:
//! - Basic pair insertion `(` → `(|)`
//! - Skip-over when typing the close char over an auto-inserted close
//! - Apostrophe prose fallthrough: `don'` stays `don'`
//! - Cursor motion clears the pending-closes stack
//! - Tag autoclose for HTML filetypes
//! - Void element suppression
//! - Self-closing tag suppression
//! - `:set noautopair` disables all pairing
//! - `:set noautoclose-tag` disables tag autoclose
//! - Open-pair-newline: Enter between `{|}` expands to indented block

use hjkl_buffer::View;
use hjkl_engine::{DefaultHost, Editor, Options};
use hjkl_vim::{dispatch_input, insert::step_insert};

// ---------------------------------------------------------------------------
// Test harness helpers
// ---------------------------------------------------------------------------

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

/// Feed keystrokes through the normal/insert dispatcher.
fn feed(ed: &mut Editor<View, DefaultHost>, keys: &str) {
    use hjkl_engine::{Input, Key};
    for ch in keys.chars() {
        let input = match ch {
            '\n' => Input {
                key: Key::Enter,
                ctrl: false,
                alt: false,
                shift: false,
            },
            '\x08' => Input {
                key: Key::Backspace,
                ctrl: false,
                alt: false,
                shift: false,
            },
            '\x1b' => Input {
                key: Key::Esc,
                ctrl: false,
                alt: false,
                shift: false,
            },
            _ => Input {
                key: Key::Char(ch),
                ctrl: false,
                alt: false,
                shift: false,
            },
        };
        dispatch_input(ed, input);
    }
}

/// Feed keystrokes directly to the insert-mode FSM.
fn feed_insert(ed: &mut Editor<View, DefaultHost>, keys: &str) {
    use hjkl_engine::{Input, Key};
    for ch in keys.chars() {
        let input = match ch {
            '\n' => Input {
                key: Key::Enter,
                ctrl: false,
                alt: false,
                shift: false,
            },
            '\x08' => Input {
                key: Key::Backspace,
                ctrl: false,
                alt: false,
                shift: false,
            },
            '\x1b' => Input {
                key: Key::Esc,
                ctrl: false,
                alt: false,
                shift: false,
            },
            _ => Input {
                key: Key::Char(ch),
                ctrl: false,
                alt: false,
                shift: false,
            },
        };
        step_insert(ed, input);
    }
}

/// Feed an arrow key to the insert-mode FSM.
fn feed_arrow_left(ed: &mut Editor<View, DefaultHost>) {
    use hjkl_engine::{Input, Key};
    step_insert(
        ed,
        Input {
            key: Key::Left,
            ctrl: false,
            alt: false,
            shift: false,
        },
    );
}

fn cursor(ed: &Editor<View, DefaultHost>) -> (usize, usize) {
    ed.cursor()
}

// ---------------------------------------------------------------------------
// Basic pair insertion
// ---------------------------------------------------------------------------

/// Helper: content without the trailing newline that `ed.content()` always adds.
fn buf_lines(ed: &Editor<View, DefaultHost>) -> Vec<String> {
    ed.buffer()
        .rope()
        .lines()
        .map(|s| {
            let s = s.to_string();
            s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
        })
        .collect::<Vec<_>>()
}

/// Typing `(` in insert mode inserts `()` and leaves cursor between them.
#[test]
fn basic_pair_paren() {
    let mut ed = editor("", "");
    feed(&mut ed, "i"); // enter insert mode
    feed_insert(&mut ed, "(");
    let lines = buf_lines(&ed);
    assert_eq!(lines, vec!["()"], "expected '()' got {lines:?}");
    let (row, col) = cursor(&ed);
    assert_eq!(
        (row, col),
        (0, 1),
        "cursor should be between ( and ), got ({row},{col})"
    );
}

/// Typing `[` → `[|]`.
#[test]
fn basic_pair_bracket() {
    let mut ed = editor("", "");
    feed(&mut ed, "i");
    feed_insert(&mut ed, "[");
    assert_eq!(buf_lines(&ed), vec!["[]"]);
    assert_eq!(cursor(&ed), (0, 1));
}

/// Typing `{` → `{|}`.
#[test]
fn basic_pair_brace() {
    let mut ed = editor("", "");
    feed(&mut ed, "i");
    feed_insert(&mut ed, "{");
    assert_eq!(buf_lines(&ed), vec!["{}"]);
    assert_eq!(cursor(&ed), (0, 1));
}

/// Typing `"` → `"|"`.
#[test]
fn basic_pair_double_quote() {
    let mut ed = editor("", "");
    feed(&mut ed, "i");
    feed_insert(&mut ed, "\"");
    assert_eq!(buf_lines(&ed), vec!["\"\""]);
    assert_eq!(cursor(&ed), (0, 1));
}

/// Typing backtick → `` `|` ``.
#[test]
fn basic_pair_backtick() {
    let mut ed = editor("", "");
    feed(&mut ed, "i");
    feed_insert(&mut ed, "`");
    assert_eq!(buf_lines(&ed), vec!["``"]);
    assert_eq!(cursor(&ed), (0, 1));
}

// ---------------------------------------------------------------------------
// Skip-over
// ---------------------------------------------------------------------------

/// After `(` auto-inserts `)`, typing `)` skips over it.
#[test]
fn skip_over_paren() {
    let mut ed = editor("", "");
    feed(&mut ed, "i");
    feed_insert(&mut ed, "("); // → "(|)"
    let (_, col_after_open) = cursor(&ed);
    assert_eq!(col_after_open, 1);
    feed_insert(&mut ed, ")"); // skip-over → "()|"
    let (row, col) = cursor(&ed);
    assert_eq!(
        (row, col),
        (0, 2),
        "skip-over should advance past ) to col 2, got ({row},{col})"
    );
    assert_eq!(
        buf_lines(&ed),
        vec!["()"],
        "no duplicate ) should be inserted"
    );
}

/// After `"` auto-inserts `"`, typing `"` skips over the auto-inserted close.
#[test]
fn skip_over_double_quote() {
    let mut ed = editor("", "");
    feed(&mut ed, "i");
    feed_insert(&mut ed, "\""); // → "|""
    feed_insert(&mut ed, "hello"); // type content
    // cursor is now at col 6, auto-close `"` is at col 6
    feed_insert(&mut ed, "\""); // skip-over
    let lines = buf_lines(&ed);
    assert_eq!(lines, vec!["\"hello\""], "no duplicate quote: {lines:?}");
    assert_eq!(cursor(&ed).1, 7, "cursor should be past closing quote");
}

// ---------------------------------------------------------------------------
// Apostrophe prose fallthrough
// ---------------------------------------------------------------------------

/// `don` followed by `'` should NOT auto-pair (prev char is alphabetic).
#[test]
fn apostrophe_no_pair_after_letter() {
    let mut ed = editor("", "");
    feed(&mut ed, "i");
    feed_insert(&mut ed, "don");
    feed_insert(&mut ed, "'");
    let lines = buf_lines(&ed);
    // Should be ["don'"] not ["don''"]
    assert_eq!(
        lines,
        vec!["don'"],
        "apostrophe after letter must not auto-pair: {lines:?}"
    );
}

/// `'` at start of buffer (no prev char) SHOULD auto-pair.
#[test]
fn apostrophe_pairs_at_start() {
    let mut ed = editor("", "");
    feed(&mut ed, "i");
    feed_insert(&mut ed, "'");
    assert_eq!(
        buf_lines(&ed),
        vec!["''"],
        "apostrophe at start should auto-pair"
    );
    assert_eq!(cursor(&ed).1, 1);
}

// ---------------------------------------------------------------------------
// Cursor motion clears pending-closes stack
// ---------------------------------------------------------------------------

/// Type `(` (auto-inserts `)`), then press Left arrow, then type `)`.
/// The `)` should be inserted literally (no skip-over) because the
/// arrow key cleared the pending-closes stack.
#[test]
fn arrow_clears_pending_closes() {
    let mut ed = editor("", "");
    feed(&mut ed, "i");
    feed_insert(&mut ed, "("); // → "(|)"
    feed_arrow_left(&mut ed); // cursor moves to col 0, stack cleared
    feed_insert(&mut ed, ")"); // literal insert, not skip-over
    let lines = buf_lines(&ed);
    // Should be [")()" ] — the `)` was inserted at col 0, then `()` follows
    let text = lines.join("");
    assert!(
        text.contains(")("),
        "literal ) should be inserted, not skipped over; got {text:?}"
    );
}

/// Mode change (Esc → Normal) clears the stack. Re-entering insert with `a`
/// and typing `}` inserts it literally.
#[test]
fn mode_change_clears_pending_closes() {
    let mut ed = editor("", "");
    feed(&mut ed, "i");
    feed_insert(&mut ed, "{"); // → "{|}"
    // Escape to normal mode (clears stack)
    feed(&mut ed, "\x1b");
    // `a` to append (enters insert after cursor)
    feed(&mut ed, "a");
    feed_insert(&mut ed, "}"); // literal `}`, not skip-over
    let lines = buf_lines(&ed);
    let text = lines.join("");
    // We expect "{}}" — the original `{}` plus a literal `}`
    assert!(
        text.contains("}}"),
        "after mode change, }} should be literally inserted; got {text:?}"
    );
}

// ---------------------------------------------------------------------------
// Tag autoclose
// ---------------------------------------------------------------------------

/// `<div>` in html filetype → `<div>|</div>`.
#[test]
fn tag_autoclose_div_html() {
    let mut ed = editor("html", "");
    feed(&mut ed, "i");
    feed_insert(&mut ed, "<div>");
    let lines = buf_lines(&ed);
    assert_eq!(
        lines,
        vec!["<div></div>"],
        "expected tag autoclose: {lines:?}"
    );
    // Cursor should be between `>` and `</div>`, i.e. at col 5
    assert_eq!(cursor(&ed).1, 5, "cursor should be between tags");
}

/// `<span>` in html → autoclose.
#[test]
fn tag_autoclose_span_html() {
    let mut ed = editor("html", "");
    feed(&mut ed, "i");
    feed_insert(&mut ed, "<span>");
    let lines = buf_lines(&ed);
    assert_eq!(lines, vec!["<span></span>"], "{lines:?}");
}

/// Void element `<br>` must NOT get a close tag.
#[test]
fn void_element_no_close() {
    let mut ed = editor("html", "");
    feed(&mut ed, "i");
    feed_insert(&mut ed, "<br>");
    let lines = buf_lines(&ed);
    assert_eq!(
        lines,
        vec!["<br>"],
        "void element must not be auto-closed: {lines:?}"
    );
}

/// Void element `<img>` must NOT get a close tag.
#[test]
fn void_element_img_no_close() {
    let mut ed = editor("html", "");
    feed(&mut ed, "i");
    feed_insert(&mut ed, "<img>");
    let lines = buf_lines(&ed);
    assert_eq!(
        lines,
        vec!["<img>"],
        "void element img must not be auto-closed: {lines:?}"
    );
}

/// Self-closing `<Foo />` must NOT get a close tag.
#[test]
fn self_closing_no_close() {
    let mut ed = editor("jsx", "");
    feed(&mut ed, "i");
    // Type `<Foo />` — the `/` before `>` makes it self-closing
    feed_insert(&mut ed, "<Foo />");
    let lines = buf_lines(&ed);
    assert_eq!(
        lines,
        vec!["<Foo />"],
        "self-closing must not be auto-closed: {lines:?}"
    );
}

/// Non-HTML filetype: `<` should not auto-pair as `<>`.
#[test]
fn lt_no_pair_in_rust() {
    let mut ed = editor("rust", "");
    feed(&mut ed, "i");
    feed_insert(&mut ed, "<");
    let lines = buf_lines(&ed);
    assert_eq!(lines, vec!["<"], "< must not pair in rust: {lines:?}");
}

// ---------------------------------------------------------------------------
// :set noautopair
// ---------------------------------------------------------------------------

/// `:set noautopair` disables bracket pairing.
#[test]
fn set_noautopair_disables() {
    let mut ed = editor("", "");
    let reg = hjkl_ex::default_registry::<DefaultHost>();
    hjkl_ex::try_dispatch(&reg, &mut ed, "set noautopair");
    assert!(
        !ed.settings().autopair,
        "autopair should be off after :set noautopair"
    );
    feed(&mut ed, "i");
    feed_insert(&mut ed, "(");
    let lines = buf_lines(&ed);
    assert_eq!(
        lines,
        vec!["("],
        "with noautopair, only ( should be inserted: {lines:?}"
    );
}

/// `:set noautoclose-tag` disables tag autoclose in HTML.
/// With autoclose-tag off but autopair still on, `<` pairs as `<>` but `>`
/// skip-over fires without tag autoclose.
#[test]
fn set_noautoclose_tag_disables() {
    let mut ed = editor("html", "");
    let reg = hjkl_ex::default_registry::<DefaultHost>();
    hjkl_ex::try_dispatch(&reg, &mut ed, "set noautoclose-tag");
    assert!(!ed.settings().autoclose_tag, "autoclose_tag should be off");
    // Also disable autopair so `<` doesn't pair with `>` in this test.
    ed.settings_mut().autopair = false;
    feed(&mut ed, "i");
    feed_insert(&mut ed, "<div>");
    let lines = buf_lines(&ed);
    assert_eq!(
        lines,
        vec!["<div>"],
        "with noautoclose-tag, no </div> inserted: {lines:?}"
    );
}

// ---------------------------------------------------------------------------
// Open-pair-newline
// ---------------------------------------------------------------------------

/// Enter between `{` and `}` (auto-paired) → expands to indented block.
#[test]
fn open_pair_newline_brace() {
    let mut ed = editor("", "");
    feed(&mut ed, "i");
    feed_insert(&mut ed, "{"); // → "{|}"
    // Cursor is at col 1, between { and }
    feed_insert(&mut ed, "\n"); // open-pair-newline
    let lines = buf_lines(&ed);
    // Expected: ["{", "    ", "}"] (3 lines with default 4-space indent)
    assert_eq!(
        lines.len(),
        3,
        "should have 3 lines after open-pair-newline: {lines:?}"
    );
    assert_eq!(lines[0], "{", "first line should be {{: {lines:?}");
    assert_eq!(lines[2], "}", "third line should be }}: {lines:?}");
    // Cursor should be on the inner (indented) line
    let (row, _) = cursor(&ed);
    assert_eq!(
        row, 1,
        "cursor should be on the middle (indented) line, got row {row}"
    );
}

// ---------------------------------------------------------------------------
// Triple-quote / triple-backtick guard
// ---------------------------------------------------------------------------

/// Typing three backticks must produce exactly three (markdown code-fence),
/// not four. Regression for user report: `` ``` `` left a 4th backtick
/// because the autopair fired on the third keystroke.
#[test]
fn triple_backtick_does_not_autopair_third() {
    let mut ed = editor("", "");
    feed(&mut ed, "i");
    feed_insert(&mut ed, "`"); // 1st → autopair → `|`
    feed_insert(&mut ed, "`"); // 2nd → skip-over → ``|
    feed_insert(&mut ed, "`"); // 3rd → triple-quote guard → bare insert → ```|
    let lines = buf_lines(&ed);
    assert_eq!(
        lines,
        vec!["```"],
        "expected three backticks, got {lines:?}"
    );
    let (row, col) = cursor(&ed);
    assert_eq!(
        (row, col),
        (0, 3),
        "cursor should be after the three backticks"
    );
}

/// Same guard for `"""` (Python triple-quoted strings).
#[test]
fn triple_double_quote_does_not_autopair_third() {
    let mut ed = editor("", "");
    feed(&mut ed, "i");
    feed_insert(&mut ed, "\"");
    feed_insert(&mut ed, "\"");
    feed_insert(&mut ed, "\"");
    assert_eq!(
        buf_lines(&ed),
        vec!["\"\"\""],
        "expected three double-quotes"
    );
    assert_eq!(cursor(&ed), (0, 3));
}

/// Same guard for `\u{27}\u{27}\u{27}` (rare but seen in some templating).
#[test]
fn triple_single_quote_does_not_autopair_third() {
    let mut ed = editor("", "");
    feed(&mut ed, "i");
    feed_insert(&mut ed, "\u{27}");
    feed_insert(&mut ed, "\u{27}");
    feed_insert(&mut ed, "\u{27}");
    assert_eq!(
        buf_lines(&ed),
        vec!["\u{27}\u{27}\u{27}"],
        "expected three single quotes"
    );
    assert_eq!(cursor(&ed), (0, 3));
}

// ---------------------------------------------------------------------------
// Code-fence Enter expansion
// ---------------------------------------------------------------------------

/// Typing ```rust then Enter must produce three lines: opener, blank
/// middle, closer — with the cursor parked on the middle line.
#[test]
fn code_fence_with_lang_expands_on_enter() {
    let mut ed = editor("", "");
    feed(&mut ed, "i");
    feed_insert(&mut ed, "`");
    feed_insert(&mut ed, "`");
    feed_insert(&mut ed, "`");
    feed_insert(&mut ed, "r");
    feed_insert(&mut ed, "u");
    feed_insert(&mut ed, "s");
    feed_insert(&mut ed, "t");
    feed_insert(&mut ed, "\n");
    let lines = buf_lines(&ed);
    assert_eq!(
        lines,
        vec!["```rust".to_string(), String::new(), "```".to_string()],
        "expected three lines with cursor on middle, got {lines:?}"
    );
    let (row, col) = cursor(&ed);
    assert_eq!(
        (row, col),
        (1, 0),
        "cursor must land on the blank middle line"
    );
}

/// Bare ``` (no language tag) on Enter must NOT expand — could be either
/// an opener or a closer and we don't track parity.
#[test]
fn bare_triple_backtick_does_not_expand_on_enter() {
    let mut ed = editor("", "");
    feed(&mut ed, "i");
    feed_insert(&mut ed, "`");
    feed_insert(&mut ed, "`");
    feed_insert(&mut ed, "`");
    feed_insert(&mut ed, "\n");
    let lines = buf_lines(&ed);
    assert_eq!(
        lines,
        vec!["```".to_string(), String::new()],
        "bare fence + Enter must not expand, got {lines:?}"
    );
}

/// Indented fence (e.g. inside a list item) preserves indentation on the
/// opener, blank, and closer lines.
#[test]
fn indented_code_fence_preserves_indent_on_expand() {
    let mut ed = editor("", "");
    feed(&mut ed, "i");
    feed_insert(&mut ed, "    ```rust\n");
    let lines = buf_lines(&ed);
    assert_eq!(
        lines,
        vec![
            "    ```rust".to_string(),
            "    ".to_string(),
            "    ```".to_string(),
        ],
        "expected indented fence pair, got {lines:?}"
    );
    let (row, col) = cursor(&ed);
    assert_eq!(
        (row, col),
        (1, 4),
        "cursor must land on the indented blank middle line"
    );
}
