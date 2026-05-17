use super::*;
use crate::app::CmdLineKind;

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

    // Open the command prompt.
    app.open_command_prompt();
    assert!(app.command_field.is_some());

    // Ctrl-P should recall the most-recent entry ("set nonu").
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

    app.open_cmdline_window(CmdLineKind::Ex);

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

    // Buffer should contain 3 history lines.
    let slot_idx = app.cmdline_win.as_ref().unwrap().slot_idx;
    let line_count = app.slots()[slot_idx].editor.buffer().row_count();
    assert_eq!(line_count, 3, "buffer must have 3 history lines");
}

#[test]
fn q_colon_window_cr_on_history_line_re_executes() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("set nu");
    // last_ex_command is now "set nu"

    app.open_cmdline_window(CmdLineKind::Ex);
    // Cmdline window has 1 line: "set nu". Cursor is on it.
    assert!(app.cmdline_win.is_some());

    // Move cursor to row 0 (the history line).
    let slot_idx = app.cmdline_win.as_ref().unwrap().slot_idx;
    app.slots_mut()[slot_idx].editor.jump_cursor(0, 0);

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

    app.open_cmdline_window(CmdLineKind::Ex);
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
