//! Regression tests: text-object column conventions on multibyte lines,
//! and count-prefix arithmetic saturation.
//!
//! Pre-0.33.5, `word_text_object` / `quote_text_object` returned BYTE
//! columns while the operator pipeline (`cut_vim_range` / the visual
//! extend path) consumed CHAR columns — `diw` / `di"` on lines holding
//! multibyte text deleted the wrong span (`di"` could eat most of the
//! line). Both resolvers now speak char columns end to end.

use hjkl_engine::Editor;
use hjkl_engine::types::{DefaultHost, Options};
use hjkl_engine::vim::Operator;
use hjkl_vim::VimEditorExt;

fn ed_with(content: &str) -> Editor<hjkl_buffer::Buffer, DefaultHost> {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        DefaultHost::new(),
        Options::default(),
    );
    e.set_content(content);
    e
}

fn line0(e: &Editor<hjkl_buffer::Buffer, DefaultHost>) -> String {
    hjkl_buffer::rope_line_str(&e.buffer().rope(), 0)
}

/// `diw` on an ASCII word — baseline, unchanged by the fix.
#[test]
fn diw_ascii_word_baseline() {
    let mut e = ed_with("hello world");
    e.jump_cursor(0, 6);
    e.apply_op_text_obj(Operator::Delete, 'w', true, 1);
    assert_eq!(line0(&e), "hello ");
}

/// `diw` with multibyte chars earlier on the line and in the word itself.
/// Pre-fix: byte columns fed into the char-column cut left "héllo w".
#[test]
fn diw_multibyte_word() {
    let mut e = ed_with("héllo wörld");
    e.jump_cursor(0, 6); // char col 6 = 'w'
    e.apply_op_text_obj(Operator::Delete, 'w', true, 1);
    assert_eq!(line0(&e), "héllo ");
}

/// `daw` must absorb the leading space (no trailing ws after the word),
/// with multibyte text on both sides of the boundary.
#[test]
fn daw_multibyte_absorbs_leading_ws() {
    let mut e = ed_with("héllo wörld");
    e.jump_cursor(0, 6);
    e.apply_op_text_obj(Operator::Delete, 'w', false, 1);
    assert_eq!(line0(&e), "héllo");
}

/// `di"` with a multibyte first pair and the cursor inside the second pair.
/// Pre-fix: the char-indexed cursor was compared against byte-indexed quote
/// positions, selecting the FIRST pair and cutting nearly the whole line.
#[test]
fn di_quote_multibyte_second_pair() {
    let mut e = ed_with("\"日日日\" \"x\"");
    e.jump_cursor(0, 7); // char col 7 = 'x'
    e.apply_op_text_obj(Operator::Delete, '"', true, 1);
    assert_eq!(line0(&e), "\"日日日\" \"\"");
}

/// `di"` inside a multibyte quoted span deletes exactly its content.
#[test]
fn di_quote_multibyte_content() {
    let mut e = ed_with("\"日日日\" \"x\"");
    e.jump_cursor(0, 2); // char col 2 = second 日
    e.apply_op_text_obj(Operator::Delete, '"', true, 1);
    assert_eq!(line0(&e), "\"\" \"x\"");
}

/// Typing an absurdly long digit prefix must clamp at vim's documented
/// count ceiling of 999,999,999 (`:h count`), not saturate toward
/// `usize::MAX` — an effectively-infinite count would freeze every
/// `0..count` apply loop (DoS via `<20 nines>x` / `999999999dw`).
#[test]
fn count_prefix_digits_clamp_at_vim_max() {
    let mut e = ed_with("one\ntwo\nthree");
    for _ in 0..40 {
        e.accumulate_count_digit(9);
    }
    assert_eq!(e.count(), hjkl_engine::vim::MAX_COUNT);
    assert_eq!(e.count(), 999_999_999);
    // A count-consuming command with the clamped prefix must not panic.
    let count = e.take_count();
    assert_eq!(count, 999_999_999);
    e.goto_percent(count);
}

/// The direct count setter (used by the hjkl-vim reducer to thread counts
/// through pending chords) must apply the same vim ceiling.
#[test]
fn set_count_clamps_at_vim_max() {
    let mut e = ed_with("one\ntwo\nthree");
    e.set_count(usize::MAX);
    assert_eq!(e.count(), 999_999_999);
}

/// An operator apply loop driven by a pathological count must stay bounded:
/// `<20 nines>dw` on a short buffer has to finish (each word-motion step
/// breaks once it can no longer advance, and the count itself is capped at
/// 999,999,999), leaving an emptied line — not an effective UI freeze.
#[test]
fn pathological_count_apply_loop_is_bounded() {
    let mut e = ed_with("one two three");
    for _ in 0..20 {
        e.accumulate_count_digit(9);
    }
    let count = e.take_count();
    assert_eq!(count, 999_999_999);
    e.apply_op_motion(Operator::Delete, 'w', count);
    assert_eq!(line0(&e), "");
}

/// Folded operator counts (`count1 × count2` from `999999999d999999999w`
/// style streams) enter the engine pre-multiplied and can exceed a single
/// prefix's cap; the engine must re-clamp them internally and complete.
#[test]
fn folded_operator_count_product_is_bounded() {
    let mut e = ed_with("one two three");
    e.apply_op_motion(Operator::Delete, 'w', usize::MAX);
    assert_eq!(line0(&e), "");
}
