//! `Event::Paste` (bracketed terminal paste) — audit R2 fix 3.
//!
//! Two gaps in `App::handle_paste`:
//!   (a) pasting while the completion popup is open inserted text but never
//!       updated/dismissed the popup (stale prefix filtering, popup floating
//!       over changed text);
//!   (b) pasted text was not recorded into an active macro recording, so
//!       `@q` replay silently dropped the paste.

use super::*;

/// (a) A paste while the completion popup is open must dismiss it — vim
/// dismisses the popup on paste-like input, and a pasted blob essentially
/// never matches the in-flight prefix anyway.
#[test]
fn paste_dismisses_open_completion_popup() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "fn foo");
    hjkl_vim_tui::handle_key(app.active_editor_mut(), key(KeyCode::Char('i')));
    app.sync_after_engine_mutation();
    assert_eq!(app.active_editor().vim_mode(), VimMode::Insert);

    let items = vec![make_completion_item("hello"), make_completion_item("world")];
    app.completion = Some(crate::completion::Completion::new(0, 0, items));
    assert!(app.completion.is_some(), "precondition: popup must be open");

    app.handle_paste("pasted text".to_string());

    assert!(
        app.completion.is_none(),
        "paste must dismiss the open completion popup"
    );
    // The paste itself must still have gone through.
    let line = hjkl_buffer::rope_line_str(&app.active_editor().buffer().rope(), 0);
    assert!(
        line.contains("pasted text"),
        "paste must still insert the text, got: {line:?}"
    );
}

/// A paste while no popup is open is an unaffected no-op for this fix
/// (`self.completion` stays `None`).
#[test]
fn paste_with_no_popup_open_is_unaffected() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "");
    hjkl_vim_tui::handle_key(app.active_editor_mut(), key(KeyCode::Char('i')));
    app.sync_after_engine_mutation();

    app.handle_paste("hi".to_string());

    assert!(app.completion.is_none());
    let line = hjkl_buffer::rope_line_str(&app.active_editor().buffer().rope(), 0);
    assert_eq!(line, "hi");
}

/// (b) A paste that happens WHILE a macro is recording must be captured, so
/// replaying the macro reproduces the paste instead of silently dropping it.
#[test]
fn record_macro_with_paste_replays_the_paste() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "line0\nline1\nline2");
    app.active_editor_mut().jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    // Record: qa i <paste "XX"> <Esc> j 0 q
    macro_key_seq(&mut app, &[ck('q'), ck('a')]);
    assert!(app.active_editor().is_recording_macro());
    macro_key_seq(&mut app, &[ck('i')]);
    assert_eq!(app.active_editor().vim_mode(), VimMode::Insert);

    app.handle_paste("XX".to_string());

    macro_key_seq(&mut app, &[key(KeyCode::Esc), ck('j'), ck('0'), ck('q')]);
    assert!(
        !app.active_editor().is_recording_macro(),
        "recording must stop after the second q"
    );

    // Recording itself must have inserted the paste on line0.
    assert_eq!(
        hjkl_buffer::rope_line_str(&app.active_editor().buffer().rope(), 0),
        "XXline0",
        "recording itself must apply the paste"
    );
    assert_eq!(app.active_editor().cursor(), (1, 0));

    // Replay: @a — must reproduce the paste on line1.
    macro_key_seq(&mut app, &[ck('@'), ck('a')]);
    assert!(!app.active_editor().is_replaying_macro());
    assert_eq!(
        hjkl_buffer::rope_line_str(&app.active_editor().buffer().rope(), 1),
        "XXline1",
        "@a replay must reproduce the paste that happened during recording"
    );
}

/// A paste while NOT recording must not be captured into any register (no
/// recording active means there is nothing to append to — this is mostly a
/// guard against the recorder hook firing unconditionally).
#[test]
fn paste_outside_recording_does_not_start_a_recording() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "");
    hjkl_vim_tui::handle_key(app.active_editor_mut(), key(KeyCode::Char('i')));
    app.sync_after_engine_mutation();

    app.handle_paste("hi".to_string());

    assert!(!app.active_editor().is_recording_macro());
}
