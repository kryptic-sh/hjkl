//! The helix keymap, judged the way a helix user would judge it: by what the
//! buffer says afterwards.
//!
//! Where multi-cursor is involved these assert the resulting **text**, not caret
//! coordinates — a coordinate can be self-consistently wrong, and the bug this
//! whole slice exists to kill (a secondary selection whose anchor drifted away
//! from its head) shows up as text landing in the wrong place, not as a caret
//! that looks odd.

use hjkl_buffer::{Buffer, Position};
use hjkl_engine::input::{Input, Key};
use hjkl_engine::{DefaultHost, Editor, Options, Sel};
use hjkl_helix::{HelixMode, dispatch_input, helix_editor};

fn ed(content: &str) -> Editor<Buffer, DefaultHost> {
    let mut e = helix_editor(Buffer::new(), DefaultHost::new(), Options::default());
    e.set_content(content);
    e
}

fn key(c: char) -> Input {
    Input {
        key: Key::Char(c),
        ctrl: false,
        alt: false,
        shift: false,
    }
}

fn alt(c: char) -> Input {
    Input {
        key: Key::Char(c),
        ctrl: false,
        alt: true,
        shift: false,
    }
}

fn esc() -> Input {
    Input {
        key: Key::Esc,
        ctrl: false,
        alt: false,
        shift: false,
    }
}

fn keys(e: &mut Editor<Buffer, DefaultHost>, s: &str) {
    for c in s.chars() {
        dispatch_input(e, key(c));
    }
}

/// The whole buffer, rows joined with `\n`.
fn text(e: &Editor<Buffer, DefaultHost>) -> String {
    (0..e.row_count())
        .filter_map(|r| e.line(r))
        .collect::<Vec<_>>()
        .join("\n")
}

fn state(e: &Editor<Buffer, DefaultHost>) -> &hjkl_helix::HelixState {
    e.discipline()
        .as_any()
        .downcast_ref::<hjkl_helix::HelixState>()
        .unwrap()
}

/// The primary selection, reassembled from its two homes.
fn primary(e: &Editor<Buffer, DefaultHost>) -> Sel {
    let (row, col) = e.cursor();
    Sel::new(state(e).anchor, Position::new(row, col))
}

fn pos(row: usize, col: usize) -> Position {
    Position::new(row, col)
}

// ── Counts ───────────────────────────────────────────────────────────────────

#[test]
fn a_count_repeats_a_motion() {
    let mut e = ed("abcdefgh\n");
    keys(&mut e, "3l");
    assert_eq!(e.cursor(), (0, 3));
}

#[test]
fn a_count_repeats_a_vertical_motion() {
    let mut e = ed("a\nb\nc\nd\ne\n");
    keys(&mut e, "3j");
    assert_eq!(e.cursor(), (3, 0));
}

#[test]
fn a_two_digit_count_is_accumulated() {
    let mut e = ed(&format!("{}\n", "x".repeat(30)));
    keys(&mut e, "12l");
    assert_eq!(e.cursor(), (0, 12));
}

#[test]
fn a_counted_word_motion_repeats_the_motion_rather_than_growing_the_selection() {
    // Helix's counted word motions are N *replacements*, not one long selection:
    // each step re-anchors when it starts on a word boundary. `3w` therefore
    // selects the THIRD word, not all three — which is what helix does, and it is
    // the difference between a port and a plausible-looking guess.
    let mut e = ed("one two three four\n");
    keys(&mut e, "3w");
    keys(&mut e, "d");
    assert_eq!(e.line(0).as_deref(), Some("one two four"));
}

#[test]
fn a_count_is_consumed_and_does_not_leak_into_the_next_key() {
    let mut e = ed("abcdefgh\n");
    keys(&mut e, "3l");
    keys(&mut e, "l");
    assert_eq!(e.cursor(), (0, 4), "the second `l` must move exactly one");
}

// ── Word motions ─────────────────────────────────────────────────────────────

#[test]
fn e_selects_the_word_without_its_trailing_space() {
    let mut e = ed("foo bar\n");
    keys(&mut e, "e");
    assert_eq!(e.cursor(), (0, 2), "head on the last char of the word");
    keys(&mut e, "d");
    assert_eq!(e.line(0).as_deref(), Some(" bar"));
}

#[test]
fn b_selects_backwards_to_the_word_start() {
    let mut e = ed("foo bar\n");
    keys(&mut e, "6l"); // on the 'r'
    keys(&mut e, "b");
    assert_eq!(e.cursor(), (0, 4), "head lands on the word's first char");
    // The selection runs backwards from the old cursor to the word start, and it
    // is inclusive of both ends — so it covers all of "bar".
    keys(&mut e, "d");
    assert_eq!(e.line(0).as_deref(), Some("foo "));
}

#[test]
fn w_stops_at_the_end_of_a_line_instead_of_swallowing_it() {
    let mut e = ed("foo bar\nbaz\n");
    keys(&mut e, "w"); // selects "foo "
    keys(&mut e, "w"); // selects "bar" — up to, not across, the line ending
    assert_eq!(e.cursor(), (0, 6));
    keys(&mut e, "d");
    assert_eq!(text(&e), "foo \nbaz\n");
}

#[test]
fn w_from_the_last_word_of_a_line_crosses_onto_the_next() {
    let mut e = ed("foo\nbar\n");
    keys(&mut e, "w"); // "foo" (stops at the line ending)
    keys(&mut e, "w"); // steps over the line ending onto "bar"
    assert_eq!(e.cursor(), (1, 2));
    keys(&mut e, "d");
    assert_eq!(text(&e), "foo\n\n");
}

#[test]
fn long_word_motions_ignore_punctuation() {
    let mut e = ed("foo.bar baz\n");
    keys(&mut e, "W"); // "foo.bar " in one go
    keys(&mut e, "d");
    assert_eq!(e.line(0).as_deref(), Some("baz"));
}

#[test]
fn plain_word_motion_stops_at_punctuation() {
    let mut e = ed("foo.bar baz\n");
    keys(&mut e, "w"); // just "foo"
    keys(&mut e, "d");
    assert_eq!(e.line(0).as_deref(), Some(".bar baz"));
}

// ── Select mode ──────────────────────────────────────────────────────────────

#[test]
fn select_mode_extends_across_word_motions() {
    let mut e = ed("one two three\n");
    keys(&mut e, "v"); // anchor at col 0
    keys(&mut e, "ww"); // extend across two words
    keys(&mut e, "d");
    assert_eq!(e.line(0).as_deref(), Some("three"));
}

#[test]
fn v_toggles_back_out_of_select_mode() {
    let mut e = ed("abcdef\n");
    keys(&mut e, "v");
    assert_eq!(state(&e).mode, HelixMode::Select);
    keys(&mut e, "v");
    assert_eq!(state(&e).mode, HelixMode::Normal);
}

// ── Selection shape ──────────────────────────────────────────────────────────

#[test]
fn x_selects_the_whole_line_including_its_ending_so_d_removes_the_row() {
    let mut e = ed("aaa\nbbb\nccc\n");
    keys(&mut e, "jx"); // select row 1
    keys(&mut e, "d");
    assert_eq!(text(&e), "aaa\nccc\n", "the row goes, not just its text");
}

#[test]
fn repeated_x_grows_the_line_selection_downwards() {
    let mut e = ed("aaa\nbbb\nccc\nddd\n");
    keys(&mut e, "xx"); // rows 0 and 1
    keys(&mut e, "d");
    assert_eq!(text(&e), "ccc\nddd\n");
}

#[test]
fn a_count_on_x_takes_that_many_lines() {
    let mut e = ed("aaa\nbbb\nccc\nddd\n");
    keys(&mut e, "3x");
    keys(&mut e, "d");
    assert_eq!(text(&e), "ddd\n");
}

#[test]
fn capital_x_snaps_a_partial_selection_out_to_line_bounds() {
    let mut e = ed("hello world\nnext\n");
    keys(&mut e, "llvll"); // a few chars in the middle of row 0
    keys(&mut e, "X");
    keys(&mut e, "d");
    assert_eq!(text(&e), "next\n");
}

#[test]
fn percent_selects_the_whole_file() {
    let mut e = ed("aaa\nbbb\nccc\n");
    keys(&mut e, "%");
    keys(&mut e, "d");
    assert_eq!(
        text(&e),
        "",
        "an emptied buffer keeps exactly one blank row"
    );
}

#[test]
fn semicolon_collapses_the_selection_onto_the_cursor() {
    let mut e = ed("foo bar\n");
    keys(&mut e, "w"); // selects "foo "
    keys(&mut e, ";");
    assert!(primary(&e).is_caret());
    keys(&mut e, "d");
    assert_eq!(e.line(0).as_deref(), Some("foobar"), "only the space goes");
}

// ── Goto mode ────────────────────────────────────────────────────────────────

#[test]
fn gg_goes_to_the_start_of_the_file_and_ge_to_the_end() {
    let mut e = ed("aaa\nbbb\nccc");
    keys(&mut e, "jj");
    keys(&mut e, "gg");
    assert_eq!(e.cursor(), (0, 0));
    keys(&mut e, "ge");
    assert_eq!(e.cursor(), (2, 2));
}

#[test]
fn a_count_before_gg_goes_to_that_line() {
    let mut e = ed("aaa\nbbb\nccc\nddd\n");
    keys(&mut e, "3gg");
    assert_eq!(e.cursor(), (2, 0));
}

#[test]
fn capital_g_goes_to_the_counted_line() {
    let mut e = ed("aaa\nbbb\nccc\nddd\n");
    keys(&mut e, "2G");
    assert_eq!(e.cursor(), (1, 0));
}

#[test]
fn gh_and_gl_go_to_the_line_start_and_end() {
    let mut e = ed("hello\n");
    keys(&mut e, "gl");
    assert_eq!(e.cursor(), (0, 4));
    keys(&mut e, "gh");
    assert_eq!(e.cursor(), (0, 0));
}

#[test]
fn gs_goes_to_the_first_non_blank() {
    let mut e = ed("    indented\n");
    keys(&mut e, "gs");
    assert_eq!(e.cursor(), (0, 4));
}

#[test]
fn an_unknown_goto_key_is_swallowed_rather_than_acted_on() {
    let mut e = ed("abc\ndef\n");
    keys(&mut e, "gz"); // not a binding
    assert_eq!(e.cursor(), (0, 0), "and `z` must not run as a bare key");
}

// ── Find char ────────────────────────────────────────────────────────────────

#[test]
fn f_selects_through_the_found_char() {
    let mut e = ed("hello world\n");
    keys(&mut e, "fo"); // select "hello" -> head on the 'o' at col 4
    assert_eq!(e.cursor(), (0, 4));
    keys(&mut e, "d");
    assert_eq!(e.line(0).as_deref(), Some(" world"));
}

#[test]
fn t_stops_one_char_short() {
    let mut e = ed("hello world\n");
    keys(&mut e, "to");
    assert_eq!(e.cursor(), (0, 3));
}

#[test]
fn capital_f_searches_backwards() {
    let mut e = ed("hello world\n");
    keys(&mut e, "gl"); // last char
    keys(&mut e, "Fo");
    assert_eq!(e.cursor(), (0, 7));
}

#[test]
fn a_count_finds_the_nth_match() {
    let mut e = ed("a.b.c.d\n");
    keys(&mut e, "2f.");
    assert_eq!(e.cursor(), (0, 3));
}

#[test]
fn a_find_with_no_match_leaves_the_selection_alone() {
    let mut e = ed("hello\n");
    keys(&mut e, "fz");
    assert_eq!(e.cursor(), (0, 0));
    assert!(primary(&e).is_caret());
}

// ── Change / yank / paste ────────────────────────────────────────────────────

#[test]
fn c_deletes_the_selection_and_lands_in_insert_mode() {
    let mut e = ed("foo bar\n");
    keys(&mut e, "w"); // "foo "
    keys(&mut e, "c");
    assert_eq!(state(&e).mode, HelixMode::Insert);
    keys(&mut e, "XY");
    assert_eq!(e.line(0).as_deref(), Some("XYbar"));
}

#[test]
fn y_then_p_pastes_after_the_selection() {
    let mut e = ed("abc\n");
    keys(&mut e, "vll"); // select "abc"
    keys(&mut e, "y");
    keys(&mut e, "p");
    assert_eq!(e.line(0).as_deref(), Some("abcabc"));
}

#[test]
fn capital_p_pastes_before_the_selection() {
    let mut e = ed("bc\n");
    keys(&mut e, "vl");
    keys(&mut e, "y");
    keys(&mut e, "P");
    assert_eq!(e.line(0).as_deref(), Some("bcbc"));
}

#[test]
fn yanking_a_line_selection_pastes_as_a_whole_line() {
    let mut e = ed("aaa\nbbb\n");
    keys(&mut e, "x"); // whole row 0, line ending included
    keys(&mut e, "y");
    keys(&mut e, "p");
    assert_eq!(text(&e), "aaa\naaa\nbbb\n");
}

#[test]
fn y_leaves_the_selection_alone() {
    let mut e = ed("abc\n");
    keys(&mut e, "vll");
    keys(&mut e, "y");
    assert_eq!(primary(&e), Sel::new(pos(0, 0), pos(0, 2)));
}

// ── Insert entry points ──────────────────────────────────────────────────────

#[test]
fn a_appends_after_the_selection() {
    let mut e = ed("ab\n");
    keys(&mut e, "va"); // Select mode is irrelevant to where `a` lands
    keys(&mut e, "Z");
    assert_eq!(e.line(0).as_deref(), Some("aZb"));
}

#[test]
fn capital_i_inserts_at_the_first_non_blank() {
    let mut e = ed("    code\n");
    keys(&mut e, "gl"); // park at the end
    keys(&mut e, "I");
    keys(&mut e, "X");
    assert_eq!(e.line(0).as_deref(), Some("    Xcode"));
}

#[test]
fn capital_a_appends_at_the_end_of_the_line() {
    let mut e = ed("code\n");
    keys(&mut e, "A");
    keys(&mut e, "!");
    assert_eq!(e.line(0).as_deref(), Some("code!"));
}

#[test]
fn o_opens_a_line_below() {
    let mut e = ed("aaa\nbbb\n");
    keys(&mut e, "o");
    keys(&mut e, "X");
    assert_eq!(text(&e), "aaa\nX\nbbb\n");
}

#[test]
fn capital_o_opens_a_line_above() {
    let mut e = ed("aaa\nbbb\n");
    keys(&mut e, "j"); // on row 1
    keys(&mut e, "O");
    keys(&mut e, "X");
    assert_eq!(text(&e), "aaa\nX\nbbb\n");
}

// ── Undo / redo ──────────────────────────────────────────────────────────────

#[test]
fn u_undoes_and_capital_u_redoes() {
    let mut e = ed("abc\n");
    keys(&mut e, "d"); // delete 'a'
    assert_eq!(e.line(0).as_deref(), Some("bc"));
    keys(&mut e, "u");
    assert_eq!(e.line(0).as_deref(), Some("abc"));
    keys(&mut e, "U");
    assert_eq!(e.line(0).as_deref(), Some("bc"));
}

#[test]
fn a_count_undoes_several_steps() {
    let mut e = ed("abcdef\n");
    keys(&mut e, "ddd"); // three single-char deletes
    assert_eq!(e.line(0).as_deref(), Some("def"));
    keys(&mut e, "3u");
    assert_eq!(e.line(0).as_deref(), Some("abcdef"));
}

// ── In-place rewrites ────────────────────────────────────────────────────────

#[test]
fn r_replaces_every_char_of_the_selection() {
    let mut e = ed("abcd\n");
    keys(&mut e, "vll"); // "abc"
    keys(&mut e, "rx");
    assert_eq!(e.line(0).as_deref(), Some("xxxd"));
}

#[test]
fn r_keeps_the_selection() {
    let mut e = ed("abcd\n");
    keys(&mut e, "vll");
    keys(&mut e, "rx");
    assert_eq!(primary(&e), Sel::new(pos(0, 0), pos(0, 2)));
}

#[test]
fn tilde_switches_the_case_of_the_selection() {
    let mut e = ed("aBc\n");
    keys(&mut e, "vll");
    keys(&mut e, "~");
    assert_eq!(e.line(0).as_deref(), Some("AbC"));
}

#[test]
fn j_joins_the_line_below() {
    let mut e = ed("foo\nbar\n");
    keys(&mut e, "J");
    assert_eq!(e.line(0).as_deref(), Some("foo bar"));
}

#[test]
fn j_joins_every_seam_a_selection_spans() {
    let mut e = ed("a\nb\nc\nd\n");
    keys(&mut e, "xxx"); // rows 0..2
    keys(&mut e, "J");
    assert_eq!(text(&e), "a b c\nd\n");
}

#[test]
fn indent_and_unindent_the_selected_rows() {
    let mut e = ed("aaa\nbbb\nccc\n");
    keys(&mut e, "xx"); // rows 0 and 1
    keys(&mut e, ">");
    assert_eq!(text(&e), "    aaa\n    bbb\nccc\n");
    keys(&mut e, "<");
    assert_eq!(text(&e), "aaa\nbbb\nccc\n");
}

#[test]
fn indent_leaves_blank_rows_blank() {
    let mut e = ed("aaa\n\nccc\n");
    keys(&mut e, "xxx");
    keys(&mut e, ">");
    assert_eq!(text(&e), "    aaa\n\n    ccc\n");
}

// ── Multi-cursor: ranged selections at EVERY cursor ─────────────────────────
//
// The point of the anchor work. Before it, `d` deleted a range at the primary and
// exactly one char at each secondary — silently, and only visible in the text.

#[test]
fn w_then_d_deletes_a_word_at_every_cursor() {
    let mut e = ed("foo one\nfoo two\nfoo three\n");
    keys(&mut e, "CC"); // carets on rows 0, 1, 2
    keys(&mut e, "w"); // each selects "foo "
    keys(&mut e, "d");
    assert_eq!(text(&e), "one\ntwo\nthree\n");
}

#[test]
fn select_mode_extends_every_selection_and_d_deletes_all_of_them() {
    let mut e = ed("abcdef\nabcdef\n");
    keys(&mut e, "C"); // two carets, col 0
    keys(&mut e, "vll"); // extend both to cover "abc"
    keys(&mut e, "d");
    assert_eq!(text(&e), "def\ndef\n");
}

#[test]
fn x_then_d_deletes_a_whole_line_at_every_cursor() {
    let mut e = ed("aaa\nbbb\nccc\nddd\n");
    keys(&mut e, "C"); // carets on rows 0 and 1
    keys(&mut e, "x"); // each selects its whole row
    keys(&mut e, "d");
    assert_eq!(text(&e), "ccc\nddd\n");
}

#[test]
fn c_changes_at_every_cursor() {
    let mut e = ed("foo one\nfoo two\n");
    keys(&mut e, "C");
    keys(&mut e, "w"); // "foo " at both
    keys(&mut e, "c");
    keys(&mut e, "X");
    assert_eq!(text(&e), "Xone\nXtwo\n");
}

#[test]
fn two_cursors_on_one_row_each_delete_their_own_word() {
    // The case a naive fan-out smears: two ranges on the same row, and deleting the
    // first moves the second one's coordinates out from under it.
    let mut e = ed("aaa bbb ccc ddd\n");
    e.add_cursor(pos(0, 8)); // caret on "ccc"
    keys(&mut e, "w"); // primary selects "aaa ", secondary selects "ccc "
    keys(&mut e, "d");
    assert_eq!(e.line(0).as_deref(), Some("bbb ddd"));
}

#[test]
fn a_secondary_selection_keeps_its_anchor_across_an_edit_at_the_primary() {
    // Type at the primary (which shifts everything after it), then delete the
    // secondary's *range*. If the anchor had not moved with the head, the delete
    // would land one char off — visible only in the text.
    let mut e = ed("abc xyz\n");
    e.add_cursor(pos(0, 4)); // caret on the 'x'
    keys(&mut e, "v"); // Select mode
    keys(&mut e, "ll"); // both selections now span 3 chars
    keys(&mut e, "d");
    assert_eq!(e.line(0).as_deref(), Some(" "));
}

#[test]
fn r_replaces_a_ranged_selection_at_every_cursor() {
    let mut e = ed("abc\nabc\n");
    keys(&mut e, "C");
    keys(&mut e, "vl"); // two chars at each
    keys(&mut e, "r-");
    assert_eq!(text(&e), "--c\n--c\n");
}

#[test]
fn y_with_several_cursors_joins_their_text() {
    let mut e = ed("ab\ncd\n");
    keys(&mut e, "C");
    keys(&mut e, "vl"); // "ab" and "cd"
    keys(&mut e, "y");
    let reg = e.registers().read('"').map(|s| s.text.clone()).unwrap();
    assert_eq!(reg, "ab\ncd");
}

#[test]
fn o_opens_a_line_at_every_cursor() {
    let mut e = ed("aaa\nbbb\n");
    keys(&mut e, "C");
    keys(&mut e, "o");
    keys(&mut e, "X");
    assert_eq!(text(&e), "aaa\nX\nbbb\nX\n");
}

#[test]
fn indent_applies_once_to_a_row_two_cursors_share() {
    // Both cursors sit on row 0: the row must be indented once, not twice.
    let mut e = ed("aaa\n");
    e.add_cursor(pos(0, 2));
    keys(&mut e, ">");
    assert_eq!(e.line(0).as_deref(), Some("    aaa"));
}

#[test]
fn alt_c_adds_a_cursor_above() {
    let mut e = ed("aaa\nbbb\nccc\n");
    keys(&mut e, "jj"); // row 2
    dispatch_input(&mut e, alt('C'));
    dispatch_input(&mut e, alt('C'));
    keys(&mut e, "i");
    keys(&mut e, "X");
    assert_eq!(text(&e), "Xaaa\nXbbb\nXccc\n");
}

#[test]
fn rotate_moves_the_primary_to_the_next_selection() {
    let mut e = ed("aaa\nbbb\nccc\n");
    keys(&mut e, "CC"); // primary on row 0, secondaries on rows 1 and 2
    assert_eq!(e.cursor(), (0, 0));
    keys(&mut e, ")");
    assert_eq!(e.cursor(), (1, 0), "the primary is now the row-1 selection");
    keys(&mut e, "(");
    assert_eq!(e.cursor(), (0, 0));
}

#[test]
fn comma_keeps_only_the_primary_and_semicolon_keeps_all_of_them() {
    let mut e = ed("aaa\nbbb\nccc\n");
    keys(&mut e, "CC");
    keys(&mut e, "x"); // every selection is a whole line
    keys(&mut e, ";"); // collapse each, but keep all three
    assert_eq!(e.extra_cursors().len(), 2);
    keys(&mut e, ","); // now drop them
    assert!(e.extra_cursors().is_empty());
}

#[test]
fn undo_after_a_multi_cursor_delete_restores_everything_in_one_step() {
    let mut e = ed("foo one\nfoo two\n");
    keys(&mut e, "C");
    keys(&mut e, "w");
    keys(&mut e, "d");
    assert_eq!(text(&e), "one\ntwo\n");
    keys(&mut e, "u");
    assert_eq!(text(&e), "foo one\nfoo two\n");
    assert!(
        e.extra_cursors().is_empty(),
        "history rewind drops carets it cannot track — it must not keep stale ones"
    );
}

#[test]
fn esc_drops_the_extra_cursors_and_the_selection() {
    let mut e = ed("abc\ndef\n");
    keys(&mut e, "C");
    keys(&mut e, "vl");
    dispatch_input(&mut e, esc());
    assert!(e.extra_cursors().is_empty());
    assert!(primary(&e).is_caret());
    assert_eq!(state(&e).mode, HelixMode::Normal);
}

// ── Multi-byte text ──────────────────────────────────────────────────────────

#[test]
fn a_multi_cursor_edit_lands_correctly_after_a_multi_byte_char() {
    // Char columns, not bytes and not graphemes: an anchor computed in the wrong
    // unit is invisible in ASCII and wrong here.
    let mut e = ed("héllo wörld\nhéllo wörld\n");
    keys(&mut e, "C");
    keys(&mut e, "w"); // selects "héllo " at both cursors
    keys(&mut e, "d");
    assert_eq!(text(&e), "wörld\nwörld\n");
}
