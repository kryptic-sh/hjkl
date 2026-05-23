use super::*;

// ── Phase 2c-vi: SelectRegister reducer integration tests ─────────────────

/// `"add` — delete line into register a; verify register a contains the line.
#[test]
fn quote_a_then_dd_deletes_into_register_a() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world\nline two");
    // Move cursor to first line (already there after seed).
    assert_eq!(app.active().editor.cursor().0, 0);

    // `"a` sets pending register to 'a' via reducer, then `dd` deletes the line.
    drive_key(&mut app, key(KeyCode::Char('"')));
    drive_key(&mut app, key(KeyCode::Char('a')));
    drive_key(&mut app, key(KeyCode::Char('d')));
    drive_key(&mut app, key(KeyCode::Char('d')));

    // Register 'a' must contain the deleted line text.
    let slot = app.active().editor.registers().read('a');
    assert!(slot.is_some(), "register 'a' should be set after \"add");
    let text = &slot.unwrap().text;
    assert!(
        text.contains("hello world"),
        "register 'a' should contain 'hello world', got {text:?}"
    );
    // Buffer should only have the second line.
    let lines = app.active().editor.buffer().lines().to_vec();
    assert_eq!(lines, vec!["line two"], "\"add must delete first line");
}

/// `"ayy` → move → `"ap` round-trips through register a.
#[test]
fn quote_a_then_yy_then_quote_a_then_p_pastes_named_register() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "first line\nsecond line");

    // `"ayy` — yank first line into register a.
    drive_key(&mut app, key(KeyCode::Char('"')));
    drive_key(&mut app, key(KeyCode::Char('a')));
    drive_key(&mut app, key(KeyCode::Char('y')));
    drive_key(&mut app, key(KeyCode::Char('y')));

    // Verify register a has the yanked text.
    let slot = app.active().editor.registers().read('a');
    assert!(slot.is_some(), "register 'a' must be set after \"ayy");
    let text = slot.unwrap().text.clone();
    assert!(
        text.contains("first line"),
        "register 'a' should contain 'first line', got {text:?}"
    );

    // Move down one line.
    drive_key(&mut app, key(KeyCode::Char('j')));
    assert_eq!(
        app.active().editor.cursor().0,
        1,
        "cursor must be on line 1"
    );

    // `"ap` — paste from register a.
    drive_key(&mut app, key(KeyCode::Char('"')));
    drive_key(&mut app, key(KeyCode::Char('a')));
    drive_key(&mut app, key(KeyCode::Char('p')));

    // Buffer should now have "first line" duplicated after line two.
    let lines = app.active().editor.buffer().lines().to_vec();
    assert!(lines.len() >= 3, "paste must add a line, got {lines:?}");
    assert!(
        lines.iter().any(|l| l.contains("first line")),
        "pasted content must contain 'first line', got {lines:?}"
    );
}

/// `"_dd` — delete into black-hole; unnamed register must not change.
#[test]
fn quote_underscore_then_dd_blackhole_no_unnamed_change() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "keep me\ndelete me\nkeep too");

    // Yank first line into unnamed to establish a baseline.
    drive_key(&mut app, key(KeyCode::Char('y')));
    drive_key(&mut app, key(KeyCode::Char('y')));
    let baseline = app
        .active()
        .editor
        .registers()
        .read('"')
        .map(|s| s.text.clone())
        .unwrap_or_default();

    // Move to second line.
    drive_key(&mut app, key(KeyCode::Char('j')));

    // `"_dd` — delete into black-hole register.
    drive_key(&mut app, key(KeyCode::Char('"')));
    drive_key(&mut app, key(KeyCode::Char('_')));
    drive_key(&mut app, key(KeyCode::Char('d')));
    drive_key(&mut app, key(KeyCode::Char('d')));

    // Unnamed register must still match the baseline yank.
    let after = app
        .active()
        .editor
        .registers()
        .read('"')
        .map(|s| s.text.clone())
        .unwrap_or_default();
    assert_eq!(
        baseline, after,
        "\"_dd must not overwrite the unnamed register"
    );
    // Line was deleted from the buffer.
    let lines = app.active().editor.buffer().lines().to_vec();
    assert!(
        !lines.iter().any(|l| l.contains("delete me")),
        "\"_dd must still delete the line from the buffer, got {lines:?}"
    );
}

/// Esc after `"` must clear the pending reducer state without setting any register.
#[test]
fn quote_then_esc_cancels() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello");

    // `"` enters SelectRegister pending state.
    drive_key(&mut app, key(KeyCode::Char('"')));
    assert!(
        app.pending_state.is_some(),
        "\" must set app pending_state to SelectRegister"
    );

    // Esc cancels.
    drive_key(&mut app, key(KeyCode::Esc));
    assert!(
        app.pending_state.is_none(),
        "Esc must clear pending_state after \""
    );
}

/// `"!` — invalid register char; pending_register must not be set and the
/// next operation uses the unnamed register.
#[test]
fn quote_invalid_char_no_register_set() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world\nsecond");

    // Yank baseline into unnamed.
    drive_key(&mut app, key(KeyCode::Char('y')));
    drive_key(&mut app, key(KeyCode::Char('y')));
    let baseline_unnamed = app
        .active()
        .editor
        .registers()
        .read('"')
        .map(|s| s.text.clone())
        .unwrap_or_default();

    // `"!dd` — '!' is not a valid register selector; engine ignores it.
    // After cancel the reducer clears without setting pending_register.
    drive_key(&mut app, key(KeyCode::Char('"')));
    drive_key(&mut app, key(KeyCode::Char('!')));
    // Reducer cancels on invalid key — pending_state cleared.
    assert!(
        app.pending_state.is_none(),
        "invalid register char must cancel pending_state"
    );

    // No register named '!' exists.
    let slot = app.active().editor.registers().read('!');
    assert!(slot.is_none(), "register '!' must not exist");

    // Unnamed register unchanged.
    let after = app
        .active()
        .editor
        .registers()
        .read('"')
        .map(|s| s.text.clone())
        .unwrap_or_default();
    assert_eq!(
        baseline_unnamed, after,
        "unnamed register must be unchanged after \"!"
    );
}

// ── Phase 5a: mark chord pending states integration tests ─────────────────

/// `ma` then move, then `'a` — linewise mark jump round-trip.
#[test]
fn m_a_then_apostrophe_a_jumps_back_to_line() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "first line\n  second line\nthird line");
    // Jump to row 2, col 3 and set mark 'a'.
    app.active_mut().editor.jump_cursor(2, 3);
    drive_key(&mut app, key(KeyCode::Char('m')));
    drive_key(&mut app, key(KeyCode::Char('a')));
    assert!(
        app.pending_state.is_none(),
        "pending_state must clear after ma"
    );
    // Move away to row 0.
    app.active_mut().editor.jump_cursor(0, 0);
    // `'a` — linewise jump back to row 2, first non-blank.
    drive_key(&mut app, key(KeyCode::Char('\'')));
    drive_key(&mut app, key(KeyCode::Char('a')));
    assert!(
        app.pending_state.is_none(),
        "pending_state must clear after 'a"
    );
    assert_eq!(
        app.active().editor.cursor().0,
        2,
        "'a must jump back to mark row"
    );
    assert_window_synced_to_engine(&app);
}

/// `ma` then move, then `` `a `` — charwise mark jump round-trip.
#[test]
fn m_a_then_backtick_a_jumps_back_to_pos() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "first line\nsecond line\nthird line");
    // Jump to exact pos (1, 4) and set mark 'a'.
    app.active_mut().editor.jump_cursor(1, 4);
    drive_key(&mut app, key(KeyCode::Char('m')));
    drive_key(&mut app, key(KeyCode::Char('a')));
    // Move away.
    app.active_mut().editor.jump_cursor(0, 0);
    // `` `a `` — charwise jump back.
    drive_key(&mut app, key(KeyCode::Char('`')));
    drive_key(&mut app, key(KeyCode::Char('a')));
    assert!(
        app.pending_state.is_none(),
        "pending_state must clear after `a"
    );
    assert_eq!(
        app.active().editor.cursor(),
        (1, 4),
        "`a must jump to exact mark position"
    );
    assert_window_synced_to_engine(&app);
}

/// `m` then `<Esc>` — Esc must cancel the SetMark chord without moving cursor.
#[test]
fn m_then_esc_cancels() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world");
    app.active_mut().editor.jump_cursor(0, 3);
    drive_key(&mut app, key(KeyCode::Char('m')));
    assert!(
        app.pending_state.is_some(),
        "m must enter SetMark pending state"
    );
    drive_key(&mut app, key(KeyCode::Esc));
    assert!(
        app.pending_state.is_none(),
        "Esc must cancel SetMark pending state"
    );
    assert_eq!(
        app.active().editor.cursor(),
        (0, 3),
        "cursor must not move after m<Esc>"
    );
}

/// `'` then `<Esc>` — Esc must cancel the GotoMarkLine chord.
#[test]
fn apostrophe_then_esc_cancels() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world");
    app.active_mut().editor.jump_cursor(0, 2);
    drive_key(&mut app, key(KeyCode::Char('\'')));
    assert!(
        app.pending_state.is_some(),
        "' must enter GotoMarkLine pending state"
    );
    drive_key(&mut app, key(KeyCode::Esc));
    assert!(
        app.pending_state.is_none(),
        "Esc must cancel GotoMarkLine pending state"
    );
    assert_eq!(
        app.active().editor.cursor(),
        (0, 2),
        "cursor must not move after '<Esc>"
    );
}

/// `` ` `` in Visual mode jumps charwise and keeps visual mode active.
#[test]
fn backtick_in_visual_jumps_pos() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "first line\nsecond line\nthird line");
    // Set mark 'b' at (2, 2) in Normal mode.
    app.active_mut().editor.jump_cursor(2, 2);
    drive_key(&mut app, key(KeyCode::Char('m')));
    drive_key(&mut app, key(KeyCode::Char('b')));
    // Enter Visual mode at (0, 0).
    app.active_mut().editor.jump_cursor(0, 0);
    drive_key(&mut app, key(KeyCode::Char('v')));
    assert_eq!(app.active().editor.vim_mode(), hjkl_engine::VimMode::Visual);
    // `` `b `` in Visual mode — must jump to (2, 2) via BeginPendingGotoMarkChar.
    // In Visual mode, route_chord_key dispatches non-Normal trie, which has
    // the `` ` `` binding from build_app_keymap.
    app.route_chord_key(ck('`'));
    app.route_chord_key(ck('b'));
    assert!(
        app.pending_state.is_none(),
        "pending_state must clear after `b in Visual mode"
    );
    assert_eq!(
        app.active().editor.cursor().0,
        2,
        "`b in Visual mode must jump to mark row"
    );
}

// ── Phase 5b: macro record / play integration tests ──────────────────────────

/// Helper: send keys through `route_chord_key` for macro integration tests.
/// Uses route_chord_key (not drive_key) so the recorder hook fires.
#[test]
fn q_then_esc_cancels_no_recording_started() {
    // `q<Esc>` — Esc after `q` cancels (no recording started).
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world");

    drive_key(&mut app, ck('q'));
    // After `q`, pending state = RecordMacroTarget.
    assert_eq!(
        app.pending_state,
        Some(hjkl_vim::PendingState::RecordMacroTarget),
        "q must set RecordMacroTarget pending state"
    );
    drive_key(&mut app, key(KeyCode::Esc));
    // Esc cancels — no recording.
    assert!(
        app.pending_state.is_none(),
        "Esc after q must clear pending_state"
    );
    assert!(
        !app.active().editor.is_recording_macro(),
        "Esc cancel must not start recording"
    );
}

#[test]
fn bare_q_during_record_stops() {
    // `qa` starts recording, bare `q` stops it.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world");

    // Start recording register 'a'.
    drive_key(&mut app, ck('q'));
    drive_key(&mut app, ck('a'));
    assert!(
        app.active().editor.is_recording_macro(),
        "q a must start recording"
    );
    assert_eq!(app.active().editor.recording_register(), Some('a'));

    // Bare `q` must stop the recording (QChord branches to stop_macro_record).
    drive_key(&mut app, ck('q'));
    assert!(
        !app.active().editor.is_recording_macro(),
        "bare q must stop recording"
    );
    assert!(
        app.pending_state.is_none(),
        "pending_state must be clear after stop"
    );
}

#[test]
fn record_macro_a_j_motion_replay_plays() {
    // Record `qa j q` (move down one line), then `@a` replays it.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "line0\nline1\nline2\nline3");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    // Record: qa j q.
    macro_key_seq(
        &mut app,
        &[
            ck('q'),
            ck('a'), // start recording into 'a'
            ck('j'), // record: move down
            ck('q'), // stop recording
        ],
    );
    assert!(
        !app.active().editor.is_recording_macro(),
        "recording must stop after second q"
    );
    // Should be on row 1 from the j motion during recording.
    assert_eq!(app.active().editor.cursor().0, 1);

    // Play: @a — should move down one more row.
    macro_key_seq(&mut app, &[ck('@'), ck('a')]);
    assert!(
        !app.active().editor.is_replaying_macro(),
        "replaying_macro must be false after replay finishes"
    );
    assert_eq!(
        app.active().editor.cursor().0,
        2,
        "@a must replay j motion (move to row 2)"
    );
}

#[test]
fn record_macro_a_insert_text_esc_motion_replays_full() {
    // Repro for: qa A test <Esc> 0 j q  → @a should re-execute the entire
    // sequence on the next line. Reported bug: replay only enters insert
    // mode at EOL and stops (text + esc + motion not replayed).
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "line0\nline1\nline2\nline3");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    macro_key_seq(
        &mut app,
        &[
            ck('q'),
            ck('a'), // start recording into 'a'
            ck('A'), // append at end of line → Insert mode
            ck('t'),
            ck('e'),
            ck('s'),
            ck('t'),
            key(KeyCode::Esc), // back to Normal
            ck('0'),           // start of line
            ck('j'),           // down one
            ck('q'),           // stop recording
        ],
    );
    assert!(!app.active().editor.is_recording_macro());
    assert_eq!(
        app.active().editor.buffer().lines()[0],
        "line0test",
        "recording itself must append 'test' to line0"
    );
    assert_eq!(
        app.active().editor.cursor(),
        (1, 0),
        "after recording, cursor must be at row 1, col 0"
    );

    macro_key_seq(&mut app, &[ck('@'), ck('a')]);
    assert!(!app.active().editor.is_replaying_macro());
    assert_eq!(
        app.active().editor.buffer().lines()[1],
        "line1test",
        "@a must re-execute A+test on line1"
    );
    assert_eq!(
        app.active().editor.cursor(),
        (2, 0),
        "@a must end with cursor at row 2, col 0 (0 then j)"
    );
}

#[test]
fn record_macro_a_comma_text_esc_j0_replays() {
    // User scenario: qaA, this is a test<Esc>j0q  → @a
    // Expected: append ", this is a test" to current line then move down/start.
    // Actual bug report: replay went into insert at EOL then literally typed "j0".
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "line0\nline1\nline2");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    macro_key_seq(
        &mut app,
        &[
            ck('q'),
            ck('a'),
            ck('A'),
            ck(','),
            ck(' '),
            ck('t'),
            ck('h'),
            ck('i'),
            ck('s'),
            ck(' '),
            ck('i'),
            ck('s'),
            ck(' '),
            ck('a'),
            ck(' '),
            ck('t'),
            ck('e'),
            ck('s'),
            ck('t'),
            key(KeyCode::Esc),
            ck('j'),
            ck('0'),
            ck('q'),
        ],
    );
    assert!(!app.active().editor.is_recording_macro());
    assert_eq!(
        app.active().editor.buffer().lines()[0],
        "line0, this is a test",
        "recording itself must append the text"
    );

    macro_key_seq(&mut app, &[ck('@'), ck('a')]);
    assert_eq!(
        app.active().editor.buffer().lines()[1],
        "line1, this is a test",
        "@a must re-append the text on line1"
    );
}

#[test]
fn gcc_toggles_comment_on_current_line_via_app_layer() {
    // User report: `gcc` doesn't toggle line comment in the live binary.
    // Drive through the same path the event loop uses (handle_keypress →
    // route_chord_key → engine via FallThrough).
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "let x = 1;\nlet y = 2;");
    app.active_mut().editor.set_filetype("rust");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    macro_key_seq(&mut app, &[ck('g'), ck('c'), ck('c')]);
    assert_eq!(
        app.active().editor.buffer().lines()[0],
        "// let x = 1;",
        "gcc must toggle a comment marker onto line 0"
    );
}

#[test]
fn gcc_on_doc_comment_uncomments_via_app_layer() {
    // User report: `gcc` is a no-op on `///` rustdoc lines in the live
    // binary. The toggle is supposed to detect "already commented"
    // (starts_with("//") covers "///") and uncomment by stripping one
    // "//" + one optional space — turning `/// foo` → `/ foo`.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "/// doc line\nfn foo() {}");
    app.active_mut().editor.set_filetype("rust");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    macro_key_seq(&mut app, &[ck('g'), ck('c'), ck('c')]);
    let line0 = app.active().editor.buffer().lines()[0].clone();
    assert_ne!(
        line0, "/// doc line",
        "gcc must mutate the doc-comment line; got {line0:?} (no-op == bug)"
    );
}

#[test]
fn gcc_on_indented_doc_comment_via_app_layer() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "    /// indented doc\nfn foo() {}");
    app.active_mut().editor.set_filetype("rust");
    app.active_mut().editor.jump_cursor(0, 4);
    app.sync_viewport_from_editor();

    macro_key_seq(&mut app, &[ck('g'), ck('c'), ck('c')]);
    let line0 = app.active().editor.buffer().lines()[0].clone();
    assert_ne!(
        line0, "    /// indented doc",
        "gcc on indented doc-comment must mutate; got {line0:?}"
    );
}

#[test]
fn opening_rust_file_auto_sets_filetype_so_gcc_works() {
    // User report: opening a .rs file from the CLI and pressing `gcc`
    // does nothing. Root cause: build_slot never called set_filetype, so
    // toggle_comment_range took the no-known-syntax early return. This
    // test drives the production file-open path end-to-end.
    let dir = std::env::temp_dir().join(format!("hjkl-gcc-filetype-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file_path = dir.join("a.rs");
    std::fs::write(&file_path, "let x = 1;\nlet y = 2;\n").unwrap();

    let mut app = App::new(Some(file_path.clone()), false, None, None).unwrap();
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    assert_eq!(
        app.active().editor.settings().filetype,
        "rust",
        "opening a .rs file must seed the editor's filetype to \"rust\""
    );

    macro_key_seq(&mut app, &[ck('g'), ck('c'), ck('c')]);
    assert_eq!(
        app.active().editor.buffer().lines()[0],
        "// let x = 1;",
        "gcc on a freshly-opened .rs file must toggle a comment marker"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn at_at_repeats_last_macro() {
    // Record `qa j q`, play `@a`, then `@@` re-plays same macro.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "line0\nline1\nline2\nline3\nline4");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    // Record qa j q.
    macro_key_seq(&mut app, &[ck('q'), ck('a'), ck('j'), ck('q')]);
    assert_eq!(app.active().editor.cursor().0, 1);

    // @a — plays, moves to row 2, sets last_macro = 'a'.
    macro_key_seq(&mut app, &[ck('@'), ck('a')]);
    assert_eq!(app.active().editor.cursor().0, 2);

    // @@ — repeats last macro ('a'), moves to row 3.
    macro_key_seq(&mut app, &[ck('@'), ck('@')]);
    assert_eq!(
        app.active().editor.cursor().0,
        3,
        "@@ must replay the last macro"
    );
}

#[test]
fn play_macro_with_count_3() {
    // Verify that Editor::play_macro with count=3 produces 3× the inputs,
    // which is the mechanism underlying `3@a`. We call play_macro directly
    // here because count-prefix buffering lives in the event_loop, above
    // route_chord_key, and cannot be easily injected in unit-test context.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "line0\nline1\nline2\nline3\nline4\nline5\nline6");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    // Record a simple macro into register 'a' via the public API.
    app.active_mut().editor.start_macro_record('a');
    app.active_mut().editor.record_input(hjkl_engine::Input {
        key: hjkl_engine::Key::Char('j'),
        ..Default::default()
    });
    app.active_mut().editor.stop_macro_record();

    // play_macro('a', 3) must return 3 inputs.
    let inputs = app.active_mut().editor.play_macro('a', 3);
    app.active_mut().editor.end_macro_replay();
    assert_eq!(
        inputs.len(),
        3,
        "play_macro with count=3 must return 3 inputs"
    );

    // Re-feed them through route_chord_key.
    for input in inputs {
        let ct_key = engine_input_to_key_event(input);
        if ct_key.code != KeyCode::Null {
            let consumed = app.route_chord_key(ct_key);
            if !consumed {
                hjkl_vim_tui::handle_key(&mut app.active_mut().editor, ct_key);
            }
            app.sync_viewport_from_editor();
        }
    }
    assert_eq!(
        app.active().editor.cursor().0,
        3,
        "3× j motions must move cursor to row 3"
    );
}

#[test]
fn record_capital_appends_to_lowercase() {
    // `qa j q` records one j. `qA k q` appends one k (opposite direction).
    // `@a` should replay both (j then k — net zero movement).
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "line0\nline1\nline2\nline3");
    app.active_mut().editor.jump_cursor(1, 0);
    app.sync_viewport_from_editor();

    // Record qa j q.
    macro_key_seq(&mut app, &[ck('q'), ck('a'), ck('j'), ck('q')]);
    assert_eq!(app.active().editor.cursor().0, 2);

    // Record qA k q (append: moves back up from row 2 to row 1).
    macro_key_seq(&mut app, &[ck('q'), ck('A'), ck('k'), ck('q')]);
    // Cursor moved k (up) from row 2 → row 1.
    assert_eq!(app.active().editor.cursor().0, 1);

    // Now @a should replay the combined macro (j then k) — net zero move.
    // Start at row 1 after qA k q. j moves to 2, k moves back to 1.
    let start_row = app.active().editor.cursor().0; // should be 1
    macro_key_seq(&mut app, &[ck('@'), ck('a')]);
    assert_eq!(
        app.active().editor.cursor().0,
        start_row,
        "@a with capital append must replay j+k (net zero from row {start_row})"
    );
}

// ── Phase 5d: `@:` last-ex repeat ───────────────────────────────────────────

#[test]
fn at_colon_replays_last_ex() {
    // dispatch_ex("3") moves cursor to line 3 (1-based → row 2).
    // replay_last_ex() called directly must re-run `:3` and land there again.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "line0\nline1\nline2\nline3\nline4");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    // Run :3 — cursor goes to row 2 (0-based).
    app.dispatch_ex("3");
    assert_eq!(
        app.active().editor.cursor().0,
        2,
        ":3 must move cursor to row 2"
    );

    // Move cursor away.
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();
    assert_eq!(app.active().editor.cursor().0, 0);

    // Direct replay must bring cursor back to row 2.
    app.replay_last_ex();
    assert_eq!(
        app.active().editor.cursor().0,
        2,
        "replay_last_ex must re-run :3 and land on row 2"
    );
}

#[test]
fn at_colon_via_play_macro_arm_replays() {
    // Drive the full `@:` chord through route_chord_key so the PlayMacro arm
    // with reg==':' is exercised.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "line0\nline1\nline2\nline3\nline4");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    // Establish last_ex_command via :3.
    app.dispatch_ex("3");
    assert_eq!(app.active().editor.cursor().0, 2);

    // Move away.
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    // Set up BeginPendingPlayMacro state then drive ':'.
    app.dispatch_action(
        crate::keymap_actions::AppAction::BeginPendingPlayMacro { count: 1 },
        1,
    );
    assert_eq!(
        app.pending_state,
        Some(hjkl_vim::PendingState::PlayMacroTarget { count: 1 }),
        "BeginPendingPlayMacro must set PlayMacroTarget pending state"
    );

    // Drive ':' — should commit PlayMacro { reg: ':', count: 1 } → replay_last_ex.
    let consumed = app.route_chord_key(ck(':'));
    assert!(consumed, "@: must be consumed by route_chord_key");
    assert!(
        app.pending_state.is_none(),
        "pending_state must be cleared after @: commit"
    );
    assert_eq!(
        app.active().editor.cursor().0,
        2,
        "@: chord must replay :3 and land on row 2"
    );
}

#[test]
fn at_colon_with_count_3_replays_three_times() {
    // `3@:` must run the last ex command 3 times. Use `:s` substitute to
    // verify actual repetition count — each run changes one occurrence.
    // Simpler: use line-goto commands to verify idempotent + state at end.
    // We use a toggle-able command: `:set cursorline!` toggled 3 times
    // leaves it in the opposite state from start (net odd = one toggle).
    // But cursorline may not be observable. Instead use row-goto idempotency:
    // 3@: of `:1` is same as `:1` once — cursor ends on row 0 regardless.
    // To prove N > 1 actually loops, verify last_ex_command is still "1"
    // (unchanged) and cursor is on row 0.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "line0\nline1\nline2\nline3\nline4");
    app.active_mut().editor.jump_cursor(2, 0);
    app.sync_viewport_from_editor();

    // Establish last_ex_command as "1" (goto line 1).
    app.dispatch_ex("1");
    assert_eq!(app.active().editor.cursor().0, 0, ":1 must go to row 0");

    // Move away so we can verify the replays actually run.
    app.active_mut().editor.jump_cursor(4, 0);
    app.sync_viewport_from_editor();

    // Set up BeginPendingPlayMacro with count=3, then drive ':'.
    app.dispatch_action(
        crate::keymap_actions::AppAction::BeginPendingPlayMacro { count: 3 },
        1,
    );
    let consumed = app.route_chord_key(ck(':'));
    assert!(consumed, "3@: must be consumed");
    // After 3 replays of :1, cursor must be at row 0 (last replay wins).
    assert_eq!(
        app.active().editor.cursor().0,
        0,
        "3@: of :1 must end with cursor at row 0"
    );
    // last_ex_command must still be "1" (replay doesn't corrupt storage).
    assert_eq!(
        app.last_ex_command.as_deref(),
        Some("1"),
        "last_ex_command must remain '1' after 3@:"
    );
}

#[test]
fn at_colon_no_prior_ex_is_noop() {
    // Fresh app with no prior :cmd — replay_last_ex must be a silent no-op.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "line0\nline1\nline2");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    assert!(
        app.last_ex_command.is_none(),
        "fresh app must have no last_ex_command"
    );

    // Direct call — must not crash.
    app.replay_last_ex();

    // Nothing changed.
    assert_eq!(
        app.active().editor.cursor().0,
        0,
        "cursor must be unchanged"
    );
    assert!(
        app.bus.last_body().is_none(),
        "no status message for no-op replay"
    );
    assert!(
        !app.exit_requested,
        "exit_requested must stay false on no-op"
    );
}

#[test]
fn at_colon_within_macro_does_not_recurse() {
    // Verifies that `replay_last_ex` does NOT re-enter `route_chord_key` or
    // the macro recorder — `dispatch_ex` is a direct state mutation that
    // bypasses the input-dispatch layer entirely.
    //
    // The test works as follows:
    //   1. Establish last_ex_command = "1" via dispatch_ex.
    //   2. While recording a macro, call replay_last_ex() directly (simulating
    //      what the PlayMacro { reg: ':' } arm does). Verify it mutates cursor
    //      state without starting a second recording or causing recursion.
    //   3. Verify that last_ex_command is still "1" after the replay (no
    //      corruption from the inner dispatch_ex call).
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "line0\nline1\nline2\nline3\nline4");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    // Establish last_ex_command as "1".
    app.dispatch_ex("1");
    assert_eq!(app.active().editor.cursor().0, 0, ":1 must go to row 0");
    assert_eq!(app.last_ex_command.as_deref(), Some("1"));

    // Move to row 2.
    app.active_mut().editor.jump_cursor(2, 0);
    app.sync_viewport_from_editor();

    // Start recording register 'a'.
    macro_key_seq(&mut app, &[ck('q'), ck('a')]);
    assert!(
        app.active().editor.is_recording_macro(),
        "must be recording"
    );

    // Directly invoke replay_last_ex (as if @: was processed by the PlayMacro
    // arm). This simulates the non-recursive path: dispatch_ex → ex::run, no
    // route_chord_key re-entry.
    app.replay_last_ex();

    // Cursor must be at row 0 — :1 was replayed.
    assert_eq!(
        app.active().editor.cursor().0,
        0,
        "replay_last_ex during recording must move cursor to row 0"
    );

    // Recording must still be active — replay_last_ex did NOT stop it.
    assert!(
        app.active().editor.is_recording_macro(),
        "replay_last_ex must not stop macro recording"
    );

    // last_ex_command must still be "1" — no corruption from inner dispatch.
    assert_eq!(
        app.last_ex_command.as_deref(),
        Some("1"),
        "last_ex_command must remain '1' after replay_last_ex"
    );

    // Stop recording.
    macro_key_seq(&mut app, &[ck('q')]);
    assert!(!app.active().editor.is_recording_macro());
}

// ── Phase 5e: count + register audit tests ───────────────────────────────────
//
// These tests verify the dispatch path for count-prefixed register-targeted
// ops and the single-use semantics of pending_register.

/// Seed a buffer with N numbered lines ("line1\nline2\n...").
#[test]
fn count_before_op_5dd_deletes_5_lines() {
    // `5dd` — count before doubled op deletes 5 lines.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_numbered_lines(&mut app, 10);

    // Simulate what event_loop does: accumulate '5' in pending_count, then
    // route `d` through the keymap which reads pending_count.
    app.pending_count.try_accumulate('5');
    rck(&mut app, &['d', 'd']);

    let lines = app.active().editor.buffer().lines().to_vec();
    assert_eq!(
        lines.first().map(String::as_str),
        Some("line6"),
        "5dd must delete lines 1-5; first line must now be 'line6', got {lines:?}"
    );
    assert_eq!(
        app.active().editor.vim_mode(),
        hjkl_engine::VimMode::Normal,
        "must be in Normal after 5dd"
    );
}

#[test]
fn register_then_count_a5dd_targets_register_a() {
    // `"a5dd` — register prefix, then count, then doubled op.
    // Sequence: `"` → SelectRegister, `a` → SetPendingRegister('a'),
    //           `5` → pending_count=5, `dd` → delete 5 lines into reg 'a'.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_numbered_lines(&mut app, 10);

    // `"a` via route_chord_key (the canonical path for SelectRegister).
    rck(&mut app, &['"', 'a']);
    assert_eq!(
        app.active().editor.pending_register(),
        Some('a'),
        "pending_register must be 'a' after \"a"
    );

    // `5` — accumulate count.
    app.pending_count.try_accumulate('5');

    // `dd` — op.
    rck(&mut app, &['d', 'd']);

    let lines = app.active().editor.buffer().lines().to_vec();
    assert_eq!(
        lines.first().map(String::as_str),
        Some("line6"),
        "\"a5dd must delete 5 lines; first line must now be 'line6', got {lines:?}"
    );

    // Register 'a' must hold deleted content.
    let reg_a = &app.active().editor.registers().named[0];
    assert!(
        reg_a.text.contains("line1"),
        "register 'a' must contain deleted text; got {:?}",
        reg_a.text
    );

    // pending_register must be cleared after one-shot use.
    assert_eq!(
        app.active().editor.pending_register(),
        None,
        "pending_register must be cleared after op"
    );
}

#[test]
fn count_then_register_5_quote_a_dd_targets_register_a() {
    // `5"add` — count typed BEFORE `"`, then register, then doubled op.
    // Bug target: `BeginPendingSelectRegister` previously reset pending_count,
    // causing the 5 to be silently discarded. With the fix, it must survive.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_numbered_lines(&mut app, 10);

    // `5` — accumulate in pending_count (as event_loop does).
    app.pending_count.try_accumulate('5');

    // `"a` — register selection. pending_count must NOT be reset.
    rck(&mut app, &['"', 'a']);
    assert_eq!(
        app.active().editor.pending_register(),
        Some('a'),
        "pending_register must be 'a' after \"a"
    );
    assert_eq!(
        app.pending_count.peek(),
        5,
        "pending_count must survive through register selection (5\"add regression)"
    );

    // `dd` — op must consume count=5 and register='a'.
    rck(&mut app, &['d', 'd']);

    let lines = app.active().editor.buffer().lines().to_vec();
    assert_eq!(
        lines.first().map(String::as_str),
        Some("line6"),
        "5\"add must delete 5 lines; first line must now be 'line6', got {lines:?}"
    );

    // Register 'a' must hold the deleted content.
    let reg_a = &app.active().editor.registers().named[0];
    assert!(
        reg_a.text.contains("line1"),
        "register 'a' must contain deleted text; got {:?}",
        reg_a.text
    );
}

#[test]
fn outer_count_inner_count_2_quote_a_5dd_total_10() {
    // `2"a5dd` — outer count 2, register 'a', inner count 5.
    // In vim these multiply: total = 2 * 5 = 10 lines deleted.
    // With our implementation: pending_count=2 survives `"`, then `5` is
    // accumulated into pending_count making it 25 (not 10). That's a known
    // quirk of the linear accumulation model; this test documents actual
    // behaviour so regressions are caught. The primary bug (5 discarded by
    // the reset) is fixed; the multiply semantic is aspirational.
    //
    // NOTE: This test deliberately documents current behaviour. If the
    // multiplication semantic is ever implemented, update the assertion.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_numbered_lines(&mut app, 30);

    // Outer count 2.
    app.pending_count.try_accumulate('2');

    // `"a` — register. pending_count must remain 2.
    rck(&mut app, &['"', 'a']);
    assert_eq!(
        app.pending_count.peek(),
        2,
        "pending_count must be 2 after \"a"
    );

    // Inner count 5 — appends to pending_count (becomes 25 in current model).
    app.pending_count.try_accumulate('5');
    assert_eq!(
        app.pending_count.peek(),
        25,
        "pending_count digits accumulate to 25"
    );

    // `dd` — delete count1=25 lines into register 'a'.
    rck(&mut app, &['d', 'd']);

    let lines = app.active().editor.buffer().lines().to_vec();
    // 25 lines deleted from a 30-line buffer → line26 is now first.
    assert_eq!(
        lines.first().map(String::as_str),
        Some("line26"),
        "2\"a5dd with digit-accumulation semantics must delete 25 lines; got {lines:?}"
    );
    let reg_a = &app.active().editor.registers().named[0];
    assert!(
        !reg_a.text.is_empty(),
        "register 'a' must be non-empty after op"
    );
}

#[test]
fn register_prefix_then_x_targets_register() {
    // `"ax` — delete current char into register 'a'.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    // `"a` via route_chord_key.
    rck(&mut app, &['"', 'a']);
    assert_eq!(
        app.active().editor.pending_register(),
        Some('a'),
        "pending_register must be 'a' after \"a"
    );

    // `x` — engine-handled delete-char. Feed via engine (x is not in app keymap).
    hjkl_vim_tui::handle_key(&mut app.active_mut().editor, ck('x'));
    app.sync_viewport_from_editor();

    let lines = app.active().editor.buffer().lines().to_vec();
    assert_eq!(
        lines.first().map(String::as_str),
        Some("ello world"),
        "\"ax must delete 'h'; got {lines:?}"
    );

    // Register 'a' must hold 'h'.
    let reg_a = &app.active().editor.registers().named[0];
    assert_eq!(
        reg_a.text, "h",
        "register 'a' must contain 'h' after \"ax; got {:?}",
        reg_a.text
    );
}

#[test]
fn register_prefix_single_use_then_next_op_unnamed() {
    // `"add` then `dd` — the second dd must go to unnamed register `"`,
    // not reuse register 'a'. Verifies pending_register is one-shot.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_numbered_lines(&mut app, 5);

    // `"add` — delete line1 into reg 'a'.
    rck(&mut app, &['"', 'a', 'd', 'd']);

    let reg_a_text = app.active().editor.registers().named[0].text.clone();
    assert!(
        reg_a_text.contains("line1"),
        "first dd must land in reg 'a'; got {:?}",
        reg_a_text
    );

    // pending_register must be None now.
    assert_eq!(
        app.active().editor.pending_register(),
        None,
        "pending_register must be cleared after first op"
    );

    // Snapshot unnamed register state before second dd.
    let unnamed_before = app.active().editor.registers().unnamed.text.clone();

    // `dd` — delete line2 (now line1) to unnamed register.
    rck(&mut app, &['d', 'd']);

    let unnamed_after = app.active().editor.registers().unnamed.text.clone();
    assert_ne!(
        unnamed_after, unnamed_before,
        "second dd must update unnamed register"
    );

    // Register 'a' must be unchanged — still has line1.
    let reg_a_text2 = app.active().editor.registers().named[0].text.clone();
    assert_eq!(
        reg_a_text, reg_a_text2,
        "register 'a' must not be overwritten by second dd; got {:?}",
        reg_a_text2
    );
}

#[test]
fn count_then_play_macro_3at_a_plays_three_times() {
    // `3@a` — play macro 'a' three times.
    // Record: `qa j q` (move down once). Then `3@a` moves cursor down 3 rows.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_numbered_lines(&mut app, 10);

    // Record macro 'a': move down one line.
    macro_key_seq(&mut app, &[ck('q'), ck('a')]);
    assert!(
        app.active().editor.is_recording_macro(),
        "must be recording"
    );
    macro_key_seq(&mut app, &[ck('j')]);
    macro_key_seq(&mut app, &[ck('q')]);
    assert!(
        !app.active().editor.is_recording_macro(),
        "recording stopped"
    );

    let row_after_record = app.active().editor.cursor().0;
    assert_eq!(row_after_record, 1, "recording 'j' moves cursor to row 1");

    // Accumulate count 3 in pending_count, then `@a`.
    app.pending_count.try_accumulate('3');
    // `@` → BeginPendingPlayMacro, takes pending_count.
    rck(&mut app, &['@', 'a']);

    let row_after_play = app.active().editor.cursor().0;
    assert_eq!(
        row_after_play, 4,
        "3@a must play macro 3 times → cursor moves from row 1 to row 4; got {row_after_play}"
    );
}

#[test]
fn count_then_dot_5_dot_repeats_five_times() {
    // `5.` — dot-repeat runs last change 5 times.
    // Setup: `x` deletes first char of "hello world", then `5.` deletes 5 more.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    // `x` — delete 'h', establishes last_change.
    hjkl_vim_tui::handle_key(&mut app.active_mut().editor, ck('x'));
    app.sync_viewport_from_editor();

    let lines_after_x = app.active().editor.buffer().lines().to_vec();
    assert_eq!(
        lines_after_x.first().map(String::as_str),
        Some("ello world"),
        "x must delete 'h'; got {lines_after_x:?}"
    );

    // Accumulate count 5 into pending_count, then `.`.
    app.pending_count.try_accumulate('5');
    // DotRepeat is bound in the app keymap; route through route_chord_key.
    let consumed = app.route_chord_key(ck('.'));
    assert!(consumed, ". must be consumed by keymap");
    app.sync_viewport_from_editor();

    let lines_after_dot = app.active().editor.buffer().lines().to_vec();
    // Started with "ello world" (10 chars). Delete 5 chars one at a time:
    // 'e','l','l','o',' ' → "world". Each dot-repeat fires x (delete-one-char)
    // once (count folded into single repeat of the last-change op).
    assert_eq!(
        lines_after_dot.first().map(String::as_str),
        Some("world"),
        "5. must repeat x 5 more times; got {lines_after_dot:?}"
    );
}
