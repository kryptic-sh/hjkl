use super::*;

// ── Phase 4e: visual-mode operator dispatch via keymap + range-mutation ──────
//
// These tests verify that `d` / `y` / `c` in Visual / VisualLine mode are
// consumed by the app keymap (dispatching `AppAction::VisualOp`) and produce
// the correct buffer / mode state via the range-mutation primitives.

#[test]
fn visual_d_deletes_selection_via_keymap() {
    // Enter Visual, select 5 chars ("hello"), d → " world" remains.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world");
    app.active_editor_mut().jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    // Enter Visual mode via engine FSM.
    hjkl_vim_tui::handle_key(app.active_editor_mut(), ck('v'));
    assert_eq!(
        app.active_editor().vim_mode(),
        hjkl_engine::VimMode::Visual,
        "must be in Visual after v"
    );

    // Extend right 4: cursor on col 4, anchor at col 0 → "hello" selected.
    for _ in 0..4 {
        let consumed = app.route_chord_key(ck('l'));
        assert!(consumed, "l in Visual must be consumed by keymap");
    }

    // Dispatch d via keymap.
    let consumed = app.route_chord_key(ck('d'));
    assert!(
        consumed,
        "d in Visual must be consumed by keymap (VisualOp)"
    );

    // View should have " world" (the chars after the deleted selection).
    let lines = app
        .active_editor()
        .buffer()
        .rope()
        .lines()
        .map(|s| {
            let s = s.to_string();
            s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        lines,
        vec![" world"],
        "vd must delete selected chars; got {lines:?}"
    );

    // Must have returned to Normal mode.
    assert_eq!(
        app.active_editor().vim_mode(),
        hjkl_engine::VimMode::Normal,
        "must exit Visual mode after d"
    );
    assert_window_synced_to_engine(&app);
}

#[test]
fn visual_y_yanks_selection_via_keymap() {
    // Enter Visual, select "hello", y → unnamed register has "hello", buffer unchanged.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world");
    app.active_editor_mut().jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    hjkl_vim_tui::handle_key(app.active_editor_mut(), ck('v'));

    // Extend right 4: covers "hello".
    for _ in 0..4 {
        app.route_chord_key(ck('l'));
    }

    // Dispatch y via keymap.
    let consumed = app.route_chord_key(ck('y'));
    assert!(
        consumed,
        "y in Visual must be consumed by keymap (VisualOp)"
    );

    // View must be unchanged.
    let lines = app
        .active_editor()
        .buffer()
        .rope()
        .lines()
        .map(|s| {
            let s = s.to_string();
            s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        lines,
        vec!["hello world"],
        "vy must not modify the buffer; got {lines:?}"
    );

    // Unnamed register must contain the yanked text.
    let reg = app.active_editor().yank();
    assert!(
        reg.contains("hello"),
        "unnamed register must contain 'hello' after vy; got {reg:?}"
    );

    // Must have returned to Normal mode.
    assert_eq!(
        app.active_editor().vim_mode(),
        hjkl_engine::VimMode::Normal,
        "must exit Visual mode after y"
    );
    assert_window_synced_to_engine(&app);
}

#[test]
fn visual_line_d_deletes_line_via_keymap() {
    // Enter VisualLine (V), d → first line deleted.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "first line\nsecond line");
    app.active_editor_mut().jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    // Enter VisualLine via engine FSM (Shift-V).
    hjkl_vim_tui::handle_key(
        app.active_editor_mut(),
        KeyEvent::new(KeyCode::Char('V'), KeyModifiers::NONE),
    );
    assert_eq!(
        app.active_editor().vim_mode(),
        hjkl_engine::VimMode::VisualLine,
        "must be in VisualLine after V"
    );

    // Dispatch d via keymap.
    let consumed = app.route_chord_key(ck('d'));
    assert!(
        consumed,
        "d in VisualLine must be consumed by keymap (VisualOp)"
    );

    // First line should be gone.
    let lines = app
        .active_editor()
        .buffer()
        .rope()
        .lines()
        .map(|s| {
            let s = s.to_string();
            s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        lines,
        vec!["second line"],
        "Vd must delete first line; got {lines:?}"
    );

    assert_eq!(
        app.active_editor().vim_mode(),
        hjkl_engine::VimMode::Normal,
        "must exit VisualLine mode after d"
    );
    assert_window_synced_to_engine(&app);
}

#[test]
fn visual_c_enters_insert_mode_via_keymap() {
    // Enter Visual, select "hello", c → Insert mode, selection deleted.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world");
    app.active_editor_mut().jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    hjkl_vim_tui::handle_key(app.active_editor_mut(), ck('v'));

    // Extend right 4: covers "hello".
    for _ in 0..4 {
        app.route_chord_key(ck('l'));
    }

    // Dispatch c via keymap.
    let consumed = app.route_chord_key(ck('c'));
    assert!(
        consumed,
        "c in Visual must be consumed by keymap (VisualOp)"
    );

    // Must be in Insert mode.
    assert_eq!(
        app.active_editor().vim_mode(),
        hjkl_engine::VimMode::Insert,
        "vc must enter Insert mode; got {:?}",
        app.active_editor().vim_mode()
    );

    // View should have "hello" deleted, leaving " world".
    let lines = app
        .active_editor()
        .buffer()
        .rope()
        .lines()
        .map(|s| {
            let s = s.to_string();
            s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        lines,
        vec![" world"],
        "vc must delete selected chars; got {lines:?}"
    );

    assert_window_synced_to_engine(&app);
}

#[test]
fn gg_full_sequence_in_normal_mode_via_keymap() {
    // Sanity-check coverage for the previously-working Normal path.
    // Confirms route_chord_key handles Normal mode and that gg still works.
    let mut app = App::new(None, false, None, None).unwrap();
    let lines: Vec<String> = (0..30).map(|i| format!("line{i:02}")).collect();
    seed_buffer(&mut app, &lines.join("\n"));
    app.active_editor_mut().jump_cursor(20, 0);
    app.sync_viewport_from_editor();

    assert_eq!(
        app.active_editor().vim_mode(),
        hjkl_engine::VimMode::Normal,
        "must be in Normal mode"
    );

    use crossterm::event::{KeyCode, KeyEvent as CtKeyEvent, KeyModifiers};
    let g_key = CtKeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE);

    // First `g` — Normal keymap sets pending_state to AfterG.
    let consumed = app.route_chord_key(g_key);
    assert!(consumed, "first g must be consumed");
    assert!(
        matches!(
            app.pending_state,
            Some(hjkl_vim::PendingState::AfterG { .. })
        ),
        "first g must set pending_state to AfterG; got {:?}",
        app.pending_state
    );

    // Second `g` — reducer commits gg.
    let consumed = app.route_chord_key(g_key);
    assert!(consumed, "second g must be consumed");
    assert!(
        app.pending_state.is_none(),
        "after gg the reducer must clear pending_state"
    );
    assert_eq!(
        app.active_editor().cursor().0,
        0,
        "gg must move engine cursor to row 0 from row 20"
    );
    assert_window_synced_to_engine(&app);
}

// ── Phase 4e follow-up regression tests ──────────────────────────────────────

#[test]
fn visual_d_with_named_register_writes_to_register() {
    // "ad on a visual selection → register 'a' contains the deleted text.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world");
    app.active_editor_mut().jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    // "a — set pending register to 'a' via engine FSM.
    hjkl_vim_tui::handle_key(app.active_editor_mut(), ck('"'));
    hjkl_vim_tui::handle_key(app.active_editor_mut(), ck('a'));
    assert_eq!(
        app.active_editor().pending_register(),
        Some('a'),
        "pending_register must be Some('a') after \"a chord"
    );

    // Enter Visual mode.
    hjkl_vim_tui::handle_key(app.active_editor_mut(), ck('v'));
    // Extend right 4 to select "hello".
    for _ in 0..4 {
        app.route_chord_key(ck('l'));
    }

    // d — should use register 'a' from pending_register().
    let consumed = app.route_chord_key(ck('d'));
    assert!(consumed, "d in Visual must be consumed");

    let lines = app
        .active_editor()
        .buffer()
        .rope()
        .lines()
        .map(|s| {
            let s = s.to_string();
            s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        lines,
        vec![" world"],
        "\"ad must delete selection; got {lines:?}"
    );

    // Named register 'a' must contain the deleted text.
    let reg_a = &app.active_editor().registers().named[0]; // 'a' - 'a' = 0
    assert!(
        reg_a.text.contains("hello"),
        "register 'a' must contain 'hello' after \"ad; got {:?}",
        reg_a.text
    );

    assert_eq!(app.active_editor().vim_mode(), hjkl_engine::VimMode::Normal);
    assert_window_synced_to_engine(&app);
}

#[test]
fn visual_line_d_deletes_single_line_via_range_mutation() {
    // Vd on a single-line VisualLine selection. Previously fell to the engine
    // FSM (run_operator_over_range bailed on top==bot Linewise). With the
    // guard fix it flows through delete_range + MotionKind::Linewise.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "only line\nsecond line");
    app.active_editor_mut().jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    // Enter VisualLine.
    hjkl_vim_tui::handle_key(
        app.active_editor_mut(),
        KeyEvent::new(KeyCode::Char('V'), KeyModifiers::NONE),
    );
    assert_eq!(
        app.active_editor().vim_mode(),
        hjkl_engine::VimMode::VisualLine
    );

    // d — single-line VisualLine delete via range-mutation primitive.
    let consumed = app.route_chord_key(ck('d'));
    assert!(consumed, "d in VisualLine must be consumed");

    let lines = app
        .active_editor()
        .buffer()
        .rope()
        .lines()
        .map(|s| {
            let s = s.to_string();
            s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        lines,
        vec!["second line"],
        "Vd on single line must delete it; got {lines:?}"
    );

    assert_eq!(app.active_editor().vim_mode(), hjkl_engine::VimMode::Normal);
    assert_window_synced_to_engine(&app);
}

#[test]
fn visual_block_d_deletes_rectangle_via_range_mutation() {
    // <C-v>lljjd — flows through delete_block primitive. Each affected line
    // has cols 0..=2 removed.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "abcde\nfghij\nklmno");
    app.active_editor_mut().jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    // Enter VisualBlock.
    hjkl_vim_tui::handle_key(
        app.active_editor_mut(),
        KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL),
    );
    assert_eq!(
        app.active_editor().vim_mode(),
        hjkl_engine::VimMode::VisualBlock
    );

    // Extend right 2 (cols 0..=2), down 2 (rows 0..=2).
    for _ in 0..2 {
        app.route_chord_key(ck('l'));
    }
    for _ in 0..2 {
        app.route_chord_key(ck('j'));
    }

    let consumed = app.route_chord_key(ck('d'));
    assert!(consumed, "d in VisualBlock must be consumed");

    let lines = app
        .active_editor()
        .buffer()
        .rope()
        .lines()
        .map(|s| {
            let s = s.to_string();
            s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        lines,
        vec!["de", "ij", "no"],
        "VisualBlock d must remove cols 0..=2 on each row; got {lines:?}"
    );

    assert_eq!(app.active_editor().vim_mode(), hjkl_engine::VimMode::Normal);
    assert_window_synced_to_engine(&app);
}

#[test]
fn visual_block_y_yanks_rectangle_to_register() {
    // <C-v>lj"ay — yank a 2-col block into register 'a'. View unchanged.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "abcde\nfghij\nklmno");
    app.active_editor_mut().jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    // Set pending register 'a'.
    hjkl_vim_tui::handle_key(app.active_editor_mut(), ck('"'));
    hjkl_vim_tui::handle_key(app.active_editor_mut(), ck('a'));

    // Enter VisualBlock.
    hjkl_vim_tui::handle_key(
        app.active_editor_mut(),
        KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL),
    );

    // Extend right 1 (cols 0..=1), down 1 (rows 0..=1).
    app.route_chord_key(ck('l'));
    app.route_chord_key(ck('j'));

    let consumed = app.route_chord_key(ck('y'));
    assert!(consumed, "y in VisualBlock must be consumed");

    // View must be unchanged.
    let lines = app
        .active_editor()
        .buffer()
        .rope()
        .lines()
        .map(|s| {
            let s = s.to_string();
            s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        lines,
        vec!["abcde", "fghij", "klmno"],
        "VisualBlock y must not modify buffer"
    );

    // Register 'a' must contain the yanked block text.
    let reg_a = &app.active_editor().registers().named[0];
    assert!(
        !reg_a.text.is_empty(),
        "register 'a' must be non-empty after block yank"
    );
    assert!(
        reg_a.text.contains("ab") && reg_a.text.contains("fg"),
        "register 'a' must contain block text 'ab'/'fg'; got {:?}",
        reg_a.text
    );

    assert_eq!(app.active_editor().vim_mode(), hjkl_engine::VimMode::Normal);
    assert_window_synced_to_engine(&app);
}
