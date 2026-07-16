//! B5: `U` (undo line, `:h U`) — pinned as unit tests rather than oracle
//! corpus cases.
//!
//! The oracle's nvim comparison seeds each case's buffer via
//! `nvim_buf_set_lines` (an RPC call against an already-running nvim
//! instance), which real nvim's undo system treats as a genuine change to
//! the buffer — so `b_u_line_ptr` (the line `U` restores) gets set to the
//! buffer's state *before* that seed call (i.e. nvim's default single empty
//! line), not to the seeded content. `U` then "restores" the line to empty,
//! which is a test-harness artifact of the RPC-based seeding, not real vim
//! behaviour: opening an actual file (verified directly via
//! `nvim --headless <file> -c 'normal! ...'`, which does NOT go through this
//! RPC path) behaves exactly as pinned below. hjkl's own buffer construction
//! (`hjkl_buffer::View::from_str`) never routes through `Editor::mutate_edit`
//! either, so it doesn't have this artifact — these unit tests are the
//! faithful ground truth, cross-checked against real nvim via a real file.

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

/// Replay `keys` (vim macro notation, same decoder the oracle uses) through
/// `dispatch_input`.
fn press(e: &mut Editor, keys: &str) {
    for input in hjkl_engine::decode_macro(keys) {
        hjkl_vim::dispatch_input(e, input);
    }
}

/// `xxxU` deletes "hel" then restores the whole line — verified against
/// `nvim --headless hello_world.txt -c 'normal! xxxU'` → "hello world",
/// cursor (0, 0).
#[test]
fn undo_line_restores_all_changes_on_the_line() {
    let mut e = editor_with("hello world\n");
    press(&mut e, "xxxU");
    assert_eq!(e.content(), "hello world\n\n");
    assert_eq!(e.cursor(), (0, 0));
}

/// `U` is itself a change and toggles: a second `U` redoes what the first
/// one undid, landing back at the fully-deleted state — verified against
/// `nvim --headless hello_world.txt -c 'normal! xxxUU'` → "lo world".
#[test]
fn undo_line_toggles_on_second_press() {
    let mut e = editor_with("hello world\n");
    press(&mut e, "xxxUU");
    assert_eq!(e.content(), "lo world\n\n");
}

/// `U` with nothing changed on the current line is a no-op — verified
/// against `nvim --headless hello.txt -c 'normal! U'` → "hello" unchanged.
#[test]
fn undo_line_is_noop_when_nothing_changed() {
    let mut e = editor_with("hello\n");
    press(&mut e, "U");
    assert_eq!(e.content(), "hello\n\n");
    assert_eq!(e.cursor(), (0, 0));
}

/// `U` targets the line where the latest change was made, not the line the
/// cursor currently sits on — moving to a different line without editing it
/// doesn't retarget `U`, and the cursor jumps back to the restored line.
/// Verified against `nvim --headless f.txt -c 'normal! xxxjU'` →
/// "hello world\nfoo bar\n", cursor (0, 0).
#[test]
fn undo_line_targets_changed_line_not_cursor_line() {
    let mut e = editor_with("hello world\nfoo bar\n");
    press(&mut e, "xxxjU");
    assert_eq!(e.content(), "hello world\nfoo bar\n\n");
    assert_eq!(e.cursor(), (0, 0));
}

/// A change on a DIFFERENT row resets the tracked line: `U` after editing
/// row 1 must restore row 1, not the earlier edit on row 0.
#[test]
fn undo_line_resets_tracked_row_on_edit_elsewhere() {
    let mut e = editor_with("hello\nworld\n");
    press(&mut e, "x"); // row 0: "ello"
    press(&mut e, "jx"); // row 1: "orld" — different row, retargets U
    press(&mut e, "U");
    assert_eq!(e.content(), "ello\nworld\n\n");
    assert_eq!(e.cursor(), (1, 0));
}
