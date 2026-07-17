//! B16: `ip`/`ap` text objects on blank-line runs — the two cases that fully
//! empty (or reduce to a single unterminated line) the buffer, pinned here
//! rather than in the oracle corpus.
//!
//! `hjkl_driver.rs`'s buffer reconstruction (`lines.join("\n")` over the raw
//! rope) doesn't re-append a trailing newline the way `nvim_driver.rs` does
//! when the result collapses to a single line — this reproduces even for a
//! plain `dd` emptying a one-line buffer, so it's a pre-existing driver gap
//! unrelated to ip/ap. `Editor::content()` (the real API every caller
//! actually uses) already returns the correct, nvim-matching string in both
//! cases below — confirmed with a throwaway probe before writing this file.

use hjkl_engine::Editor;

fn editor_with(content: &str) -> Editor {
    let opts = hjkl_engine::Options::default();
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::new(),
        hjkl_engine::DefaultHost::new(),
        opts,
    );
    e.set_content(content);
    e
}

fn press(e: &mut Editor, keys: &str) {
    for input in hjkl_engine::decode_macro(keys) {
        hjkl_vim::dispatch_input(e, input);
    }
}

/// `dap` on a blank run at the buffer start consumes the run AND the
/// following paragraph, emptying the buffer entirely — verified against
/// `nvim --headless f.txt -c 'normal! dap'` on `"\n\n\nb\n"` → empty file
/// (0 lines, 0 bytes; re-read back as a single empty line, `"\n"`).
#[test]
fn dap_from_blank_run_at_buffer_start_empties_buffer() {
    let mut e = editor_with("\n\n\nb\n");
    press(&mut e, "dap");
    assert_eq!(e.content(), "\n");
}

/// `dip` on a blank run touching EOF (no trailing paragraph) still deletes
/// just the run, leaving the preceding paragraph intact — verified against
/// `nvim --headless f.txt -c 'normal! jdip'` on `"a\n\n\n\n"` → `"a\n"`.
#[test]
fn dip_from_blank_run_at_eof_leaves_preceding_paragraph() {
    let mut e = editor_with("a\n\n\n\n");
    press(&mut e, "jdip");
    assert_eq!(e.content(), "a\n");
}
