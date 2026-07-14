//! The helix discipline driven end to end (#63 Phase D / #265).
//!
//! Two things are on trial here:
//!
//! 1. **The discipline seam.** A non-vim grammar runs on `hjkl-engine` with no
//!    engine changes — it implements one trait and drives the public API.
//! 2. **Multi-cursor.** `C` adds a caret, insert-mode typing lands at *every*
//!    caret, and the engine keeps them all pointing at the right text. That is
//!    the payoff for #63, and this is the first thing that actually uses it.

use hjkl_buffer::Buffer;
use hjkl_engine::input::{Input, Key};
use hjkl_engine::{CoarseMode, DefaultHost, Editor, Options};
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

fn esc() -> Input {
    Input {
        key: Key::Esc,
        ctrl: false,
        alt: false,
        shift: false,
    }
}

/// Type a run of keys.
fn keys(e: &mut Editor<Buffer, DefaultHost>, s: &str) {
    for c in s.chars() {
        dispatch_input(e, key(c));
    }
}

fn mode(e: &Editor<Buffer, DefaultHost>) -> HelixMode {
    e.discipline()
        .as_any()
        .downcast_ref::<hjkl_helix::HelixState>()
        .unwrap()
        .mode
}

fn text(e: &Editor<Buffer, DefaultHost>) -> String {
    (0..3)
        .filter_map(|r| e.line(r))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The primary selection's anchor.
fn anchor(e: &Editor<Buffer, DefaultHost>) -> hjkl_buffer::Position {
    e.discipline()
        .as_any()
        .downcast_ref::<hjkl_helix::HelixState>()
        .unwrap()
        .anchor
}

// ── The seam ─────────────────────────────────────────────────────────────────

#[test]
fn a_helix_editor_reports_helix_state_not_vim() {
    let e = ed("abc\n");
    assert_eq!(mode(&e), HelixMode::Normal);
    // The engine projects a discipline-agnostic mode for app chrome; it does not
    // know or care that this is helix.
    assert_eq!(e.coarse_mode(), CoarseMode::Normal);
}

#[test]
fn select_mode_projects_as_a_selection_to_app_chrome() {
    let mut e = ed("abcdef\n");
    keys(&mut e, "v");
    assert_eq!(mode(&e), HelixMode::Select);
    assert_eq!(e.coarse_mode(), CoarseMode::Select);
}

#[test]
fn insert_mode_projects_as_insert() {
    let mut e = ed("abc\n");
    keys(&mut e, "i");
    assert_eq!(e.coarse_mode(), CoarseMode::Insert);
}

// ── Motions ──────────────────────────────────────────────────────────────────

#[test]
fn hjkl_moves_the_cursor() {
    let mut e = ed("abc\ndef\n");
    keys(&mut e, "ll");
    assert_eq!(e.cursor(), (0, 2));
    keys(&mut e, "j");
    assert_eq!(e.cursor(), (1, 2));
    keys(&mut e, "h");
    assert_eq!(e.cursor(), (1, 1));
}

#[test]
fn w_selects_the_word_and_its_trailing_space_helix_style() {
    // NOT vim's `w`. Vim moves the caret to the next word's first char (col 4).
    // Helix *selects* "foo " — the word plus the whitespace after it — and leaves
    // the cursor on that trailing space (col 3), with the anchor back at col 0.
    let mut e = ed("foo bar baz\n");
    keys(&mut e, "w");
    assert_eq!(e.cursor(), (0, 3), "the head sits on the trailing space");
    assert_eq!(anchor(&e), hjkl_buffer::Position::new(0, 0));
    // And the selection is the operand: `d` deletes all of "foo ".
    keys(&mut e, "d");
    assert_eq!(e.line(0).as_deref(), Some("bar baz"));
}

// ── Selection-first grammar ──────────────────────────────────────────────────

#[test]
fn normal_mode_motions_replace_the_selection() {
    // The defining difference from Select: the anchor follows the head, so there
    // is never a lingering range.
    let mut e = ed("abcdef\n");
    keys(&mut e, "lll");
    let st = e
        .discipline()
        .as_any()
        .downcast_ref::<hjkl_helix::HelixState>()
        .unwrap();
    let (row, col) = e.cursor();
    assert_eq!(st.anchor, hjkl_buffer::Position::new(row, col));
}

#[test]
fn select_mode_motions_extend_the_selection_and_d_deletes_the_range() {
    // `v` anchors at col 0, `ll` extends the head to col 2, `d` deletes [0..=2].
    let mut e = ed("abcdef\n");
    keys(&mut e, "vlld");
    assert_eq!(e.line(0).as_deref(), Some("def"));
}

#[test]
fn d_on_a_bare_caret_deletes_the_char_under_it() {
    let mut e = ed("abc\n");
    keys(&mut e, "d");
    assert_eq!(e.line(0).as_deref(), Some("bc"));
}

#[test]
fn x_selects_the_line_and_d_deletes_it() {
    let mut e = ed("abcdef\n");
    keys(&mut e, "xd");
    assert_eq!(e.line(0).as_deref(), Some(""));
}

#[test]
fn i_inserts_at_the_selection_start() {
    let mut e = ed("abcdef\n");
    keys(&mut e, "vll"); // select "abc"
    keys(&mut e, "i"); // insert at the START of the selection
    keys(&mut e, "Z");
    assert_eq!(e.line(0).as_deref(), Some("Zabcdef"));
}

// ── Multi-cursor: the point of all of this ───────────────────────────────────

#[test]
fn c_adds_a_cursor_on_the_next_line() {
    let mut e = ed("aaa\nbbb\nccc\n");
    keys(&mut e, "C");
    assert_eq!(e.extra_cursors().len(), 1);
    assert_eq!(e.extra_cursors()[0], hjkl_buffer::Position::new(1, 0));
}

#[test]
fn repeated_c_walks_down_the_file() {
    // Each `C` extends from the LOWEST caret, so carets stack down the file
    // instead of piling up on row 1.
    let mut e = ed("aaa\nbbb\nccc\n");
    keys(&mut e, "CC");
    assert_eq!(
        e.extra_cursors(),
        [
            hjkl_buffer::Position::new(1, 0),
            hjkl_buffer::Position::new(2, 0)
        ]
    );
}

#[test]
fn c_stops_at_the_last_row_instead_of_running_off_the_end() {
    // "aaa\n" is TWO rows — a trailing `\n` produces a trailing empty line, same
    // as every text editor. So the first `C` legitimately lands on that empty
    // row; the second and third find nothing below and add nothing.
    let mut e = ed("aaa\n");
    keys(&mut e, "CCC");
    assert_eq!(
        e.extra_cursors(),
        [hjkl_buffer::Position::new(1, 0)],
        "C must stop at the last row, not keep stacking carets"
    );
}

#[test]
fn typing_in_insert_mode_lands_at_every_cursor() {
    // THE test. Three carets, one keystroke, three edits — and the engine keeps
    // every caret pointing at the right text while the edits cascade.
    let mut e = ed("aaa\nbbb\nccc\n");
    keys(&mut e, "CC"); // carets on rows 0, 1, 2
    keys(&mut e, "i"); // insert mode
    keys(&mut e, "X");
    assert_eq!(text(&e), "Xaaa\nXbbb\nXccc");
}

#[test]
fn multi_cursor_typing_survives_a_multi_char_run() {
    let mut e = ed("aaa\nbbb\n");
    keys(&mut e, "C");
    keys(&mut e, "i");
    keys(&mut e, "XY");
    assert_eq!(e.line(0).as_deref(), Some("XYaaa"));
    assert_eq!(e.line(1).as_deref(), Some("XYbbb"));
}

#[test]
fn several_carets_on_one_line_do_not_smear() {
    // The case a naive top-down fan-out gets wrong.
    let mut e = ed("abcdef\n");
    e.add_cursor(hjkl_buffer::Position::new(0, 2));
    e.add_cursor(hjkl_buffer::Position::new(0, 4));
    keys(&mut e, "i");
    keys(&mut e, "-");
    assert_eq!(e.line(0).as_deref(), Some("-ab-cd-ef"));
}

#[test]
fn d_deletes_under_every_caret() {
    let mut e = ed("axx\nbxx\n");
    keys(&mut e, "C"); // caret on row 0 and row 1, both at col 0
    keys(&mut e, "d");
    assert_eq!(e.line(0).as_deref(), Some("xx"));
    assert_eq!(e.line(1).as_deref(), Some("xx"));
}

#[test]
fn comma_collapses_back_to_one_cursor() {
    let mut e = ed("aaa\nbbb\nccc\n");
    keys(&mut e, "CC");
    assert_eq!(e.extra_cursors().len(), 2);
    keys(&mut e, ",");
    assert!(e.extra_cursors().is_empty());
}

#[test]
fn esc_collapses_multi_cursor() {
    let mut e = ed("aaa\nbbb\n");
    keys(&mut e, "C");
    dispatch_input(&mut e, esc());
    assert!(e.extra_cursors().is_empty());
    assert_eq!(mode(&e), HelixMode::Normal);
}

#[test]
fn a_short_line_below_clamps_the_new_caret_to_its_end() {
    // Column 4 does not exist on "b" — the caret lands at the line's end rather
    // than at a column that is not there.
    let mut e = ed("aaaaaa\nb\n");
    keys(&mut e, "llll"); // col 4
    keys(&mut e, "C");
    assert_eq!(e.extra_cursors()[0], hjkl_buffer::Position::new(1, 1));
}

#[test]
fn insert_then_escape_returns_to_normal_with_carets_intact() {
    let mut e = ed("aaa\nbbb\n");
    keys(&mut e, "C");
    keys(&mut e, "i");
    keys(&mut e, "X");
    // Esc leaves insert; the carets collapse (helix's Esc drops to one selection).
    dispatch_input(&mut e, esc());
    assert_eq!(mode(&e), HelixMode::Normal);
    assert_eq!(e.line(0).as_deref(), Some("Xaaa"));
    assert_eq!(e.line(1).as_deref(), Some("Xbbb"));
}

// ── Undo ─────────────────────────────────────────────────────────────────────

#[test]
fn a_multi_cursor_keystroke_undoes_as_one_step() {
    // One keystroke is one user action, even though it made N edits.
    let mut e = ed("aaa\nbbb\n");
    keys(&mut e, "C");
    keys(&mut e, "i");
    keys(&mut e, "X");
    assert_eq!(e.line(0).as_deref(), Some("Xaaa"));
    e.undo();
    assert_eq!(
        e.line(0).as_deref(),
        Some("aaa"),
        "one undo must revert both edits"
    );
    assert_eq!(e.line(1).as_deref(), Some("bbb"));
}
