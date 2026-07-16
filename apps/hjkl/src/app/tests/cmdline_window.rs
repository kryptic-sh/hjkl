use super::*;
use crate::app::{CmdLineKind, SearchDir};

// ── Phase 1: history ring tests ─────────────────────────────────────────────

#[test]
fn ex_history_records_dispatched_commands() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("set nu");
    app.dispatch_ex("set nonu");
    assert_eq!(app.ex_history.len(), 2);
    assert_eq!(app.ex_history[0], "set nu");
    assert_eq!(app.ex_history[1], "set nonu");
}

#[test]
fn ex_history_skips_immediate_duplicate() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("set nu");
    app.dispatch_ex("set nu");
    assert_eq!(
        app.ex_history.len(),
        1,
        "consecutive duplicate must not be pushed"
    );
}

#[test]
fn ex_history_caps_at_100() {
    let mut app = App::new(None, false, None, None).unwrap();
    for i in 0..105usize {
        app.dispatch_ex(&format!("set ts={i}"));
    }
    assert_eq!(
        app.ex_history.len(),
        100,
        "history must be capped at 100 entries"
    );
    // Oldest 5 dropped — first entry should be "set ts=5".
    assert_eq!(
        app.ex_history[0], "set ts=5",
        "oldest entries must be dropped first"
    );
}

// ── Phase 2: prompt Ctrl-P / Ctrl-N recall ──────────────────────────────────

#[test]
fn prompt_ctrl_p_recalls_previous() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("set nu");
    app.dispatch_ex("set nonu");

    // Open the command prompt. A bare `:` shows NO completion popup, so
    // <C-p> goes straight to history recall (no popup to navigate first).
    app.open_command_prompt();
    assert!(app.command_field.is_some());
    assert!(
        app.completion.is_none(),
        "empty `:` prompt must not show a popup"
    );

    // Now Ctrl-P should recall the most-recent entry ("set nonu").
    app.handle_command_field_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('p'),
        crossterm::event::KeyModifiers::CONTROL,
    ));
    let text = app.command_field.as_ref().unwrap().text();
    assert_eq!(text, "set nonu", "Ctrl-P must recall the most recent entry");
}

#[test]
fn prompt_ctrl_n_after_p_advances() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("set nu");
    app.dispatch_ex("set nonu");
    app.dispatch_ex("set ts=4");

    app.open_command_prompt();
    // Empty prompt shows no popup, so history nav is immediately active.
    assert!(
        app.completion.is_none(),
        "empty `:` prompt must not show a popup"
    );

    let ctrl_p = crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('p'),
        crossterm::event::KeyModifiers::CONTROL,
    );
    let ctrl_n = crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('n'),
        crossterm::event::KeyModifiers::CONTROL,
    );

    // Ctrl-P Ctrl-P → idx 1 (second from end = "set nonu").
    app.handle_command_field_key(ctrl_p);
    app.handle_command_field_key(ctrl_p);
    let text = app.command_field.as_ref().unwrap().text();
    assert_eq!(text, "set nonu", "two Ctrl-P should be 2nd from end");

    // Ctrl-N → idx 2 (most recent = "set ts=4").
    app.handle_command_field_key(ctrl_n);
    let text = app.command_field.as_ref().unwrap().text();
    assert_eq!(text, "set ts=4", "Ctrl-N after two Ctrl-P must go forward");
}

// ── Phase 3: command-line window ────────────────────────────────────────────

#[test]
fn q_colon_opens_cmdline_window() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("set nu");
    app.dispatch_ex("set nonu");
    app.dispatch_ex("set ts=4");

    let wins_before = app.windows.iter().filter(|w| w.is_some()).count();
    let slots_before = app.slots().len();

    app.open_cmdline_window(CmdLineKind::Ex, None);

    let wins_after = app.windows.iter().filter(|w| w.is_some()).count();
    let slots_after = app.slots().len();

    assert_eq!(wins_after, wins_before + 1, "one new window expected");
    assert_eq!(slots_after, slots_before + 1, "one new slot expected");
    assert!(app.cmdline_win.is_some(), "cmdline_win must be Some");
    assert_eq!(
        app.cmdline_win.as_ref().unwrap().kind,
        CmdLineKind::Ex,
        "kind must be Ex"
    );
    assert!(
        app.is_cmdline_win_focused(),
        "cmdline window must be focused"
    );

    // View should contain 3 history lines.
    let slot_idx = app.cmdline_win.as_ref().unwrap().slot_idx;
    let line_count = app.slots()[slot_idx].buffer().row_count();
    assert_eq!(line_count, 3, "buffer must have 3 history lines");
}

#[test]
fn q_colon_window_cr_on_history_line_re_executes() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("set nu");
    // last_ex_command is now "set nu"

    app.open_cmdline_window(CmdLineKind::Ex, None);
    // Cmdline window has 1 line: "set nu". Cursor is on it.
    assert!(app.cmdline_win.is_some());

    // Move cursor to row 0 (the history line). The cmdline window is
    // focused after `open_cmdline_window` (#151 Stage 2b: cursor lives on
    // the window's own editor, not the slot).
    app.active_editor_mut().jump_cursor(0, 0);

    let wins_before = app.windows.iter().filter(|w| w.is_some()).count();
    app.commit_cmdline_window();

    // Window must be closed.
    assert!(app.cmdline_win.is_none(), "cmdline_win must be cleared");
    let wins_after = app.windows.iter().filter(|w| w.is_some()).count();
    assert_eq!(wins_after, wins_before - 1, "window must have been removed");

    // The command was re-dispatched — last_ex_command should be "set nu".
    assert_eq!(
        app.last_ex_command.as_deref(),
        Some("set nu"),
        "command must have been re-dispatched"
    );
}

#[test]
fn q_colon_window_quit_without_execute() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("set nu");
    // Clear last_ex_command so we can detect a re-dispatch.
    app.last_ex_command = None;

    app.open_cmdline_window(CmdLineKind::Ex, None);
    assert!(app.cmdline_win.is_some());

    let wins_before = app.windows.iter().filter(|w| w.is_some()).count();

    // Close via dispatch_ex("q").
    app.dispatch_ex("q");

    // Window closed, no new dispatch.
    assert!(app.cmdline_win.is_none(), "cmdline_win must be cleared");
    let wins_after = app.windows.iter().filter(|w| w.is_some()).count();
    assert_eq!(wins_after, wins_before - 1, "window must have been closed");
    assert!(
        app.last_ex_command.is_none() || app.last_ex_command.as_deref() == Some("q"),
        "no ex command other than q itself must have been dispatched"
    );
    // App must not have exited.
    assert!(
        !app.exit_requested,
        "app must not exit when cmdline window is closed via :q"
    );
}

// ── Phase 4: <C-f> mid-prompt switch (issue #132) ───────────────────────────

/// Helper: send <C-f> to the command prompt.
fn ctrl_f_cmd(app: &mut App) {
    app.handle_command_field_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('f'),
        crossterm::event::KeyModifiers::CONTROL,
    ));
}

/// Helper: send <C-f> to the search prompt.
fn ctrl_f_search(app: &mut App) {
    app.handle_search_field_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('f'),
        crossterm::event::KeyModifiers::CONTROL,
    ));
}

#[test]
fn c_f_from_ex_prompt_opens_q_colon_with_inprogress_text() {
    let mut app = App::new(None, false, None, None).unwrap();
    // Seed some history so the window has prior rows.
    app.dispatch_ex("set nu");

    // Open `:` prompt, type some text.
    app.open_command_prompt();
    type_str(&mut app, "s/foo/b");
    assert_eq!(app.command_field.as_ref().unwrap().text(), "s/foo/b");

    // Press <C-f>.
    ctrl_f_cmd(&mut app);

    // Prompt must be closed.
    assert!(
        app.command_field.is_none(),
        "command_field must be closed after <C-f>"
    );
    // Cmdline window must have opened.
    assert!(
        app.cmdline_win.is_some(),
        "cmdline_win must be Some after <C-f>"
    );
    assert_eq!(
        app.cmdline_win.as_ref().unwrap().kind,
        CmdLineKind::Ex,
        "kind must be Ex"
    );

    let slot_idx = app.cmdline_win.as_ref().unwrap().slot_idx;
    let buffer = app.slots()[slot_idx].buffer();
    // View: 1 history line + 1 prefill line = 2 rows.
    assert_eq!(
        buffer.row_count(),
        2,
        "buffer must have 1 history + 1 prefill line"
    );
    // Last line must be the in-progress text.
    let last_row = buffer.row_count() - 1;
    let last_line = hjkl_buffer::rope_line_str(&buffer.rope(), last_row);
    assert_eq!(
        last_line, "s/foo/b",
        "trailing line must hold the in-progress text"
    );

    // Cursor must be at the last row, col == text length (cursor was at end).
    // The cmdline window is focused, so its own editor is the cursor's
    // source of truth (#151 Stage 2b).
    let (cur_row, cur_col) = app.active_editor().cursor();
    assert_eq!(cur_row, last_row, "cursor must be on the trailing line");
    assert_eq!(
        cur_col,
        "s/foo/b".len(),
        "cursor col must match prompt cursor col"
    );
}

#[test]
fn c_f_from_search_forward_prompt_opens_q_slash() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "foo bar baz");

    app.open_search_prompt(SearchDir::Forward);
    type_search(&mut app, "foo");
    assert_eq!(app.search_field.as_ref().unwrap().text(), "foo");

    ctrl_f_search(&mut app);

    assert!(
        app.search_field.is_none(),
        "search_field must be closed after <C-f>"
    );
    assert!(app.cmdline_win.is_some(), "cmdline_win must open");
    assert_eq!(
        app.cmdline_win.as_ref().unwrap().kind,
        CmdLineKind::SearchForward,
        "kind must be SearchForward for / prompt"
    );

    let slot_idx = app.cmdline_win.as_ref().unwrap().slot_idx;
    let buffer = app.slots()[slot_idx].buffer();
    let last_row = buffer.row_count() - 1;
    let last_line = hjkl_buffer::rope_line_str(&buffer.rope(), last_row);
    assert_eq!(last_line, "foo", "trailing line must be the search text");
}

#[test]
fn c_f_from_search_backward_prompt_opens_q_question() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "bar baz foo");

    app.open_search_prompt(SearchDir::Backward);
    type_search(&mut app, "bar");

    ctrl_f_search(&mut app);

    assert!(
        app.search_field.is_none(),
        "search_field must be closed after <C-f>"
    );
    assert!(app.cmdline_win.is_some(), "cmdline_win must open");
    assert_eq!(
        app.cmdline_win.as_ref().unwrap().kind,
        CmdLineKind::SearchBackward,
        "kind must be SearchBackward for ? prompt"
    );

    let slot_idx = app.cmdline_win.as_ref().unwrap().slot_idx;
    let buffer = app.slots()[slot_idx].buffer();
    let last_row = buffer.row_count() - 1;
    let last_line = hjkl_buffer::rope_line_str(&buffer.rope(), last_row);
    assert_eq!(last_line, "bar", "trailing line must be the search text");
}

#[test]
fn c_f_empty_ex_prompt_opens_q_colon_with_empty_trailing_line() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("set nu"); // one history entry

    app.open_command_prompt();
    // No typing — prompt is empty.
    assert_eq!(app.command_field.as_ref().unwrap().text(), "");

    ctrl_f_cmd(&mut app);

    assert!(app.command_field.is_none());
    assert!(app.cmdline_win.is_some());

    let slot_idx = app.cmdline_win.as_ref().unwrap().slot_idx;
    let buffer = app.slots()[slot_idx].buffer();
    // 1 history + 1 empty prefill = 2 rows.
    assert_eq!(
        buffer.row_count(),
        2,
        "empty prefill still adds a trailing line"
    );
    let last_row = buffer.row_count() - 1;
    let last_line = hjkl_buffer::rope_line_str(&buffer.rope(), last_row);
    assert_eq!(last_line, "", "trailing line is empty for empty prompt");
}

#[test]
fn c_f_does_not_write_inprogress_text_to_history() {
    let mut app = App::new(None, false, None, None).unwrap();
    let history_before = app.ex_history.len();

    app.open_command_prompt();
    type_str(&mut app, "set ts=99");
    ctrl_f_cmd(&mut app);

    // History must not have grown — <C-f> aborts the prompt without committing.
    assert_eq!(
        app.ex_history.len(),
        history_before,
        "<C-f> must not push in-progress text to ex_history"
    );
}

#[test]
fn c_f_then_ctrl_c_returns_to_normal_without_reopening_prompt() {
    let mut app = App::new(None, false, None, None).unwrap();

    app.open_command_prompt();
    type_str(&mut app, "set ic");
    ctrl_f_cmd(&mut app);

    // Cmdline window is open; now <C-c> from handle_keypress.
    // Simulate the <C-c> path that close_cmdline_window handles.
    assert!(app.is_cmdline_win_focused());
    app.close_cmdline_window();

    // Must be in normal mode — no command_field, no cmdline_win.
    assert!(
        app.command_field.is_none(),
        "command_field must not re-open"
    );
    assert!(app.cmdline_win.is_none(), "cmdline_win must be closed");
    assert!(!app.exit_requested, "app must not exit");
}
