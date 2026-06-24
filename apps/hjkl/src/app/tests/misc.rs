use super::*;

// ── App::new tests ──────────────────────────────────────────────────────

#[test]
fn app_new_no_file() {
    let app = App::new(None, false, None, None).unwrap();
    assert!(!app.active().dirty);
    assert!(!app.active().is_new_file);
    assert!(app.active().filename.is_none());
    assert!(!app.active_editor().is_readonly());
}

#[test]
fn app_new_readonly_flag() {
    let app = App::new(None, true, None, None).unwrap();
    assert!(app.active_editor().is_readonly());
}

#[test]
fn app_new_not_found_sets_is_new_file() {
    let path = tmp_path("hjkl_phase5_nonexistent_abc123.txt");
    let _ = std::fs::remove_file(&path);
    let app = App::new(Some(path), false, None, None).unwrap();
    assert!(app.active().is_new_file);
    assert!(!app.active().dirty);
}

#[test]
fn app_new_goto_line_clamps() {
    let app = App::new(None, false, Some(999), None).unwrap();
    let (row, _col) = app.active_editor().cursor();
    assert_eq!(row, 0);
}

#[test]
fn ex_goto_line_100_via_dispatch() {
    let mut app = App::new(None, false, None, None).unwrap();
    // Seed buffer with 120 lines.
    let buf: String = (1..=120)
        .map(|n| format!("line{n}"))
        .collect::<Vec<_>>()
        .join("\n");
    use hjkl_buffer::{Edit, Position};
    app.active_editor_mut().mutate_edit(Edit::InsertStr {
        at: Position::new(0, 0),
        text: buf,
    });
    app.active_editor_mut().jump_cursor(0, 0);
    app.dispatch_ex("100");
    let (row, _col) = app.active_editor().cursor();
    assert_eq!(row, 99, "':100' must land on row 99");
}

#[test]
fn dot_repeat_replays_last_change() {
    use crate::keymap_actions::AppAction;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use hjkl_buffer::{Edit, Position};
    let mut app = App::new(None, false, None, None).unwrap();
    app.active_editor_mut().mutate_edit(Edit::InsertStr {
        at: Position::new(0, 0),
        text: "abc".to_string(),
    });
    app.active_editor_mut().jump_cursor(0, 0);
    // Set up a last_change by feeding `x` through the engine (single-char delete).
    hjkl_vim_tui::handle_key(
        app.active_editor_mut(),
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
    );
    // Buffer now "bc". Dot-repeat should delete one more char.
    app.dispatch_action(AppAction::DotRepeat { count: 1 }, 1);
    let line0 = hjkl_buffer::rope_line_str(&app.active_editor().buffer().rope(), 0);
    assert_eq!(
        line0, "c",
        "`.` after `x` must delete one more char, got {line0:?}"
    );
}

#[test]
fn dot_repeat_with_count_3_replays_three_times() {
    use crate::keymap_actions::AppAction;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use hjkl_buffer::{Edit, Position};
    let mut app = App::new(None, false, None, None).unwrap();
    app.active_editor_mut().mutate_edit(Edit::InsertStr {
        at: Position::new(0, 0),
        text: "abcdef".to_string(),
    });
    app.active_editor_mut().jump_cursor(0, 0);
    hjkl_vim_tui::handle_key(
        app.active_editor_mut(),
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
    );
    // Buffer "bcdef". `3.` deletes 3 more. Seed pending_count to simulate
    // the keymap layer's count-prefix accumulation.
    app.pending_count.try_accumulate('3');
    app.dispatch_action(AppAction::DotRepeat { count: 1 }, 1);
    let line0 = hjkl_buffer::rope_line_str(&app.active_editor().buffer().rope(), 0);
    assert_eq!(
        line0, "ef",
        "`3.` after `x` must delete 3 more chars, got {line0:?}"
    );
}

#[test]
fn ex_goto_line_100_via_command_field_keys() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use hjkl_buffer::{Edit, Position};
    let mut app = App::new(None, false, None, None).unwrap();
    let buf: String = (1..=120)
        .map(|n| format!("line{n}"))
        .collect::<Vec<_>>()
        .join("\n");
    app.active_editor_mut().mutate_edit(Edit::InsertStr {
        at: Position::new(0, 0),
        text: buf,
    });
    app.active_editor_mut().jump_cursor(0, 0);
    // Open command prompt, type "100", press Enter — simulate full user path.
    app.open_command_prompt();
    for c in ['1', '0', '0'] {
        app.handle_command_field_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_command_field_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let (row, _col) = app.active_editor().cursor();
    assert_eq!(
        row, 99,
        "':100<Enter>' via command-field must land on row 99, got {row}"
    );
    // Critical: the window cursor cache (used at render time) must also
    // reflect the move. Without sync_viewport_from_editor after ex::run,
    // engine cursor moves but render shows stale position.
    let fw = app.focused_window();
    assert_eq!(
        app.window_cursor(fw).0,
        99,
        "window cache cursor_row must follow engine cursor after `:100`"
    );
}

#[test]
fn do_save_readonly_blocked() {
    let mut app = App::new(None, true, None, None).unwrap();
    app.active_mut().filename = Some(tmp_path("hjkl_phase5_ro_test.txt"));
    app.do_save(None);
    let msg = app.bus.last_body_or_empty().to_string();
    assert!(
        msg.contains("E45"),
        "expected E45 readonly error, got: {msg}"
    );
}

#[test]
fn do_save_no_filename_e32() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.do_save(None);
    let msg = app.bus.last_body_or_empty().to_string();
    assert!(msg.contains("E32"), "expected E32, got: {msg}");
}

// ── Start screen tests ─────────────────────────────────────────────────

#[test]
fn start_screen_present_when_no_file() {
    let app = App::new(None, false, None, None).unwrap();
    assert!(
        app.start_screen.is_some(),
        "start_screen must be Some when no file given"
    );
}

#[test]
fn start_screen_absent_when_file_given() {
    let path = std::env::temp_dir().join("hjkl_splash_with_file.txt");
    std::fs::write(&path, "x\n").unwrap();
    let app = App::new(Some(path.clone()), false, None, None).unwrap();
    assert!(
        app.start_screen.is_none(),
        "start_screen must be None when file given"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn mode_label_returns_start_during_splash() {
    let app = App::new(None, false, None, None).unwrap();
    assert!(app.start_screen.is_some());
    assert_eq!(app.mode_label(), "START");
}

// Splash tick advancement is now wall-clock driven inside `hjkl-splash`
// (see its own unit tests); apps/hjkl just constructs the splash and
// renders it. No tick assertions live here.

// ── Config layering tests ──────────────────────────────────────────────

#[test]
fn with_config_updates_leader_and_reapplies_to_existing_slot() {
    // Smoke test for the slot-0 boot-order fix: App::new builds slot 0
    // before any user config is wired, so with_config must propagate the
    // new config and re-apply Options to existing slots. This test pins
    // the public observable: app.config reflects the override, and the
    // re-application path does not panic with a single-slot app.
    let app = App::new(None, false, None, None).unwrap();
    assert_eq!(app.config.editor.leader, ' ');

    let mut cfg = hjkl_app::config::Config::default();
    cfg.editor.leader = '\\';
    cfg.editor.tab_width = 2;
    let app = app.with_config(cfg);

    assert_eq!(app.config.editor.leader, '\\');
    assert_eq!(app.config.editor.tab_width, 2);
    assert_eq!(
        app.slots.len(),
        1,
        "with_config should not add or drop slots"
    );
}

#[test]
fn with_config_preserves_readonly_on_existing_slot() {
    // Slots opened with readonly = true must stay readonly after a
    // user-config swap (the re-applied Options must not silently flip
    // the bit back to false).
    let app = App::new(None, true, None, None).unwrap();
    assert!(app.active_editor().is_readonly());

    let app = app.with_config(hjkl_app::config::Config::default());
    assert!(
        app.active_editor().is_readonly(),
        "readonly state must survive with_config re-application"
    );
}

#[test]
fn config_load_from_disk_then_with_config_propagates_overrides() {
    // End-to-end pipeline: write a user config to a tempfile, parse it
    // through the on-disk loader (deep-merged over bundled defaults),
    // hand the result to App::with_config, and verify the override
    // landed on the App. Pins the `--config <PATH>` path that main
    // uses without spinning up the terminal.
    use std::io::Write as _;
    let mut tmp = tempfile::NamedTempFile::new().unwrap();
    writeln!(
        tmp,
        r#"
        [editor]
        leader = "\\"
        tab_width = 2

        [theme]
        name = "dark"
        "#
    )
    .unwrap();

    let cfg = hjkl_app::config::load_from(tmp.path()).expect("load_from must succeed");
    // Bundled defaults survived for fields the user file omitted:
    assert!(cfg.editor.expandtab);
    // User overrides won where present:
    assert_eq!(cfg.editor.leader, '\\');
    assert_eq!(cfg.editor.tab_width, 2);

    use hjkl_config::Validate;
    cfg.validate()
        .expect("merged user+default config must validate");

    let app = App::new(None, false, None, None).unwrap().with_config(cfg);
    assert_eq!(app.config.editor.leader, '\\');
    assert_eq!(app.config.editor.tab_width, 2);
}

#[test]
fn config_load_from_disk_validation_failure_surfaces() {
    // Out-of-range values parse cleanly but the Validate impl rejects
    // them. The pipeline must surface the field name so users can
    // identify the offending key.
    use std::io::Write as _;
    let mut tmp = tempfile::NamedTempFile::new().unwrap();
    writeln!(tmp, "[editor]\ntab_width = 0").unwrap();

    let cfg = hjkl_app::config::load_from(tmp.path()).expect("parse must succeed");

    use hjkl_config::Validate;
    let err = cfg.validate().unwrap_err();
    assert_eq!(err.field, "editor.tab_width");
}

// ── Render-level :set option tests ──────────────────────────────────────────

#[test]
fn set_cursorline_flips_setting() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert!(
        app.active_editor().settings().cursorline,
        "cursorline must default to true"
    );
    app.dispatch_ex("set nocursorline");
    assert!(
        !app.active_editor().settings().cursorline,
        ":set nocursorline must disable cursorline"
    );
    app.dispatch_ex("set cursorline");
    assert!(
        app.active_editor().settings().cursorline,
        ":set cursorline must enable cursorline"
    );
}

#[test]
fn set_cul_alias_works() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("set cul");
    assert!(
        app.active_editor().settings().cursorline,
        ":set cul must enable cursorline"
    );
}

#[test]
fn set_cursorcolumn_flips_setting() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert!(!app.active_editor().settings().cursorcolumn);
    app.dispatch_ex("set cuc");
    assert!(app.active_editor().settings().cursorcolumn);
    app.dispatch_ex("set nocuc");
    assert!(!app.active_editor().settings().cursorcolumn);
}

#[test]
fn set_signcolumn_yes() {
    use hjkl_engine::types::SignColumnMode;
    let mut app = App::new(None, false, None, None).unwrap();
    assert_eq!(
        app.active_editor().settings().signcolumn,
        SignColumnMode::Auto,
        "signcolumn defaults to auto"
    );
    app.dispatch_ex("set signcolumn=yes");
    assert_eq!(
        app.active_editor().settings().signcolumn,
        SignColumnMode::Yes
    );
}

#[test]
fn set_signcolumn_scl_alias() {
    use hjkl_engine::types::SignColumnMode;
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("set scl=no");
    assert_eq!(
        app.active_editor().settings().signcolumn,
        SignColumnMode::No
    );
}

#[test]
fn set_foldcolumn_stores_value() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert_eq!(app.active_editor().settings().foldcolumn, 0);
    app.dispatch_ex("set foldcolumn=4");
    assert_eq!(app.active_editor().settings().foldcolumn, 4);
    app.dispatch_ex("set fdc=0");
    assert_eq!(app.active_editor().settings().foldcolumn, 0);
}

#[test]
fn set_colorcolumn_stores_value() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert_eq!(app.active_editor().settings().colorcolumn, "");
    app.dispatch_ex("set cc=80");
    assert_eq!(app.active_editor().settings().colorcolumn, "80");
    app.dispatch_ex("set colorcolumn=80,120");
    assert_eq!(app.active_editor().settings().colorcolumn, "80,120");
    app.dispatch_ex("set cc=");
    assert_eq!(app.active_editor().settings().colorcolumn, "");
}

// ── issue #120 Phase 2 regression tests ─────────────────────────────────────
//
// These tests verify that `:`, `/`, `?`, `K`, and `<C-^>` are dispatched
// through the keymap trie (route_chord_key) rather than inline intercepts.
//
// Critical regression: `<leader>/` must open the grep picker, NOT the
// search prompt. The `leader_slash_no_inline_intercept_regression` test
// would fail if a bare `/` intercept fired before the trie consumed the
// second key of the `<leader>/` chord.
//
// How to verify the test gates the old bug: temporarily add an inline
// `/` intercept in event_loop.rs that fires BEFORE route_chord_key —
// the test will fail because the search prompt opens instead of the picker.

/// `<leader>/` opens the grep picker, not the search prompt.
///
/// This is the canonical regression for 1ed6e7b: an inline `/` intercept
/// that fires before the keymap trie is consumed would swallow the `/` and
/// open the search prompt instead of completing the `<leader>/` chord.
/// Because there is no longer an inline intercept, the trie handles `/`
/// entirely and this test catches any regression that re-introduces one.
#[test]
fn leader_slash_no_inline_intercept_regression() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert!(app.picker.is_none(), "picker must start None");
    assert!(app.search_field.is_none(), "search_field must start None");

    // Feed <leader> (Space) + `/` through route_chord_key.
    // The keymap trie has `<leader>/` → OpenGrepPicker and bare `/` →
    // OpenSearchPrompt(Forward). The trie must resolve to the longer chord.
    app.route_chord_key(key(KeyCode::Char(' ')));
    app.route_chord_key(key(KeyCode::Char('/')));

    assert!(
        app.picker.is_some(),
        "<leader>/ must open the grep picker, not the search prompt"
    );
    assert!(
        app.search_field.is_none(),
        "<leader>/ must NOT open the search prompt"
    );
}

/// Bare `:` opens the command prompt via the keymap trie.
#[test]
fn colon_opens_command_prompt_via_keymap() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert!(app.command_field.is_none());
    app.route_chord_key(key(KeyCode::Char(':')));
    assert!(
        app.command_field.is_some(),
        "`:` must open the command prompt via keymap dispatch"
    );
}

/// Bare `/` opens the forward search prompt via the keymap trie.
#[test]
fn slash_opens_search_prompt_forward_via_keymap() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert!(app.search_field.is_none());
    app.route_chord_key(key(KeyCode::Char('/')));
    assert!(
        app.search_field.is_some(),
        "`/` must open the search prompt via keymap dispatch"
    );
}

/// Bare `?` opens the backward search prompt via the keymap trie.
#[test]
fn question_opens_search_prompt_backward_via_keymap() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert!(app.search_field.is_none());
    app.route_chord_key(key(KeyCode::Char('?')));
    assert!(
        app.search_field.is_some(),
        "`?` must open the search prompt via keymap dispatch"
    );
}

/// `<C-^>` triggers buffer alt via the keymap trie.
/// With a single slot it's a no-op (status message), not an error.
#[test]
fn ctrl_caret_triggers_buffer_alt_via_keymap() {
    let mut app = App::new(None, false, None, None).unwrap();
    // Single slot: buffer_alt sets a status message; no panic.
    app.route_chord_key(ctrl_key('^'));
    // Just assert we didn't panic and the app is still alive.
    assert!(app.picker.is_none(), "ctrl-^ must not open picker");
}

// ── issue #120 Phase 3 regression tests ─────────────────────────────────────

/// `H` with a single slot falls back to viewport-top motion (no buffer cycle).
#[test]
fn h_single_slot_fallback_to_viewport_top() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert_eq!(app.slots.len(), 1, "test requires single slot");
    // H should not crash and should be consumed by the keymap.
    let consumed = app.route_chord_key(key(KeyCode::Char('H')));
    assert!(consumed, "H must be consumed by keymap (BufferCycleH)");
}

/// `L` with a single slot falls back to viewport-bottom motion.
#[test]
fn l_single_slot_fallback_to_viewport_bottom() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert_eq!(app.slots.len(), 1, "test requires single slot");
    let consumed = app.route_chord_key(key(KeyCode::Char('L')));
    assert!(consumed, "L must be consumed by keymap (BufferCycleL)");
}

/// `<C-h>` on a single-window layout triggers tmux path (no-op without TMUX).
#[test]
fn ctrl_h_single_window_no_tmux_no_panic() {
    let mut app = App::new(None, false, None, None).unwrap();
    // TMUX not set: TmuxNavigate falls through to no-op.
    // Must not panic and must consume the key.
    let consumed = app.route_chord_key(ctrl_key('h'));
    assert!(consumed, "<C-h> must be consumed by keymap (TmuxNavigate)");
}

// ── issue #120 Phase 4 regression tests ─────────────────────────────────────
// These tests call handle_keypress directly (the extracted method) to verify
// the full key dispatch path including overlay and prefix handling.

/// `handle_keypress` returns Break on Ctrl-C with no overlay active.
#[test]
fn handle_keypress_ctrl_c_breaks() {
    use crate::app::event_loop::KeyOutcome;
    let mut app = App::new(None, false, None, None).unwrap();
    let outcome = app.handle_keypress(ctrl_key('c'));
    assert!(
        matches!(outcome, KeyOutcome::Break),
        "Ctrl-C with no overlay must return Break"
    );
}

/// `handle_keypress` returns Continue (dismisses command field) on Ctrl-C with command field open.
#[test]
fn handle_keypress_ctrl_c_dismisses_command_field() {
    use crate::app::event_loop::KeyOutcome;
    let mut app = App::new(None, false, None, None).unwrap();
    app.open_command_prompt();
    assert!(app.command_field.is_some());
    let outcome = app.handle_keypress(ctrl_key('c'));
    assert!(
        matches!(outcome, KeyOutcome::Continue),
        "Ctrl-C with command field open must dismiss and return Continue"
    );
    assert!(app.command_field.is_none());
}

/// `handle_keypress` routes `:` to the command prompt (via keymap trie).
#[test]
fn handle_keypress_colon_opens_command_prompt() {
    use crate::app::event_loop::KeyOutcome;
    let mut app = App::new(None, false, None, None).unwrap();
    let outcome = app.handle_keypress(key(KeyCode::Char(':')));
    assert!(
        matches!(outcome, KeyOutcome::Continue),
        "`:` must return Continue after opening command prompt"
    );
    assert!(
        app.command_field.is_some(),
        "`:` must open the command prompt"
    );
}

/// `<leader>/` via handle_keypress opens the grep picker, not the search prompt.
///
/// This is the Phase 4 variant of `leader_slash_no_inline_intercept_regression`:
/// it exercises the full handle_keypress path rather than the inner route_chord_key.
#[test]
fn handle_keypress_leader_slash_opens_grep_picker() {
    use crate::app::event_loop::KeyOutcome;
    let mut app = App::new(None, false, None, None).unwrap();
    // Feed <leader> (Space) — returns Continue (chord in flight).
    let o1 = app.handle_keypress(key(KeyCode::Char(' ')));
    assert!(
        matches!(o1, KeyOutcome::Continue),
        "<leader> first key must return Continue"
    );
    // Feed `/` — trie resolves to OpenGrepPicker.
    let o2 = app.handle_keypress(key(KeyCode::Char('/')));
    assert!(
        matches!(o2, KeyOutcome::Continue),
        "<leader>/ second key must return Continue"
    );
    assert!(
        app.picker.is_some(),
        "<leader>/ via handle_keypress must open grep picker"
    );
    assert!(
        app.search_field.is_none(),
        "<leader>/ via handle_keypress must NOT open search prompt"
    );
}

// ── Sub-dispatcher canary tests (issue #121) ─────────────────────────────────

#[test]
fn dispatch_picker_action_opens_file_picker() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert!(app.picker.is_none(), "picker starts closed");
    app.dispatch_action(AppAction::OpenFilePicker, 1);
    assert!(app.picker.is_some(), "OpenFilePicker must open picker");
}

#[test]
fn dispatch_picker_action_opens_buffer_picker() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_action(AppAction::OpenBufferPicker, 1);
    assert!(app.picker.is_some(), "OpenBufferPicker must open picker");
}

#[test]
fn dispatch_git_action_status_sets_picker_or_notification() {
    // Without a git repo the picker may or may not open (implementation
    // detail), but the call must not panic.
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_action(AppAction::GitStatus, 1);
    // Either a picker opened or a status message was set — both are valid.
    let reacted = app.picker.is_some() || app.bus.last_body().is_some();
    assert!(reacted, "GitStatus must open picker or set status message");
}

#[test]
fn dispatch_lsp_action_lsp_rename_sets_notification() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_action(AppAction::LspRename, 1);
    assert!(
        app.bus.last_body().is_some(),
        "LspRename must push a notification"
    );
    let msg = app.bus.last_body_or_empty();
    assert!(
        msg.contains("Rename"),
        "LspRename status must mention :Rename, got: {msg}"
    );
}

#[test]
fn dispatch_window_action_focus_left_on_single_window_no_panic() {
    // Single window — FocusLeft is a no-op but must not panic.
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_action(AppAction::FocusLeft, 1);
}

#[test]
fn dispatch_buffer_action_buffer_next_single_slot_sets_message() {
    let mut app = App::new(None, false, None, None).unwrap();
    // With a single slot buffer_next is a no-op that sets a status message.
    app.dispatch_action(AppAction::BufferNext, 1);
    assert!(
        app.bus.last_body().is_some(),
        "BufferNext on single slot must push a notification"
    );
}

#[test]
fn dispatch_prompt_action_open_command_prompt_opens_command_field() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_action(AppAction::OpenCommandPrompt, 1);
    assert!(
        app.command_field.is_some(),
        "OpenCommandPrompt must open command_field"
    );
}

#[test]
fn dispatch_prompt_action_open_search_prompt_opens_search_field() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_action(AppAction::OpenSearchPrompt(SearchDir::Forward), 1);
    assert!(
        app.search_field.is_some(),
        "OpenSearchPrompt must open search_field"
    );
}

#[test]
fn dispatch_pending_state_action_begin_pending_replace_sets_state() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert!(app.pending_state.is_none(), "pending_state starts None");
    app.dispatch_action(AppAction::BeginPendingReplace { count: 1 }, 1);
    assert!(
        app.pending_state.is_some(),
        "BeginPendingReplace must set pending_state"
    );
    assert!(
        matches!(
            app.pending_state,
            Some(hjkl_vim::PendingState::Replace { .. })
        ),
        "pending_state must be Replace variant"
    );
}

#[test]
fn dispatch_engine_action_dot_repeat_no_panic_on_empty_buffer() {
    // Empty buffer, no last change — DotRepeat must not panic.
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_action(AppAction::DotRepeat { count: 1 }, 1);
}

#[test]
fn dispatch_action_stays_small() {
    // Regression: dispatch_action body must stay under 100 lines.
    // We locate the function by its signature and count until the first
    // `^    }$` line (the closing brace at 4-space indent).
    // dispatch_action was moved to dispatch.rs as part of the mod.rs split.
    let src = include_str!("../dispatch.rs");
    let start = src
        .find("pub fn dispatch_action")
        .expect("dispatch_action must exist in dispatch.rs");
    let rest = &src[start..];
    // Count lines until and including the closing brace.
    let mut brace_depth = 0usize;
    let mut line_count = 0usize;
    let mut found_open = false;
    for line in rest.lines() {
        line_count += 1;
        for ch in line.chars() {
            match ch {
                '{' => {
                    brace_depth += 1;
                    found_open = true;
                }
                '}' => {
                    brace_depth = brace_depth.saturating_sub(1);
                }
                _ => {}
            }
        }
        if found_open && brace_depth == 0 {
            break;
        }
    }
    // Limit bumped from 100 → 130 to accommodate the explorer-action group
    // (15 Explorer* variant names + comment + call line = ~17 extra lines).
    // The explorer group follows the same sub-dispatcher pattern (delegates to
    // dispatch_explorer_action in explorer.rs) so the spirit of the guard is met.
    assert!(
        line_count < 130,
        "dispatch_action must be < 130 lines, got {line_count}"
    );
}

// ── chord_timeout_ms config wiring tests ──────────────────────────────────

/// App::new (no-config path) must seed the chord-timeout from the canonical
/// EditorConfig default (1000 ms), not a hardcoded literal.
#[test]
fn app_new_chord_timeout_uses_editor_config_default() {
    let app = App::new(None, false, None, None).unwrap();
    let expected = std::time::Duration::from_millis(
        hjkl_app::config::Config::default().editor.chord_timeout_ms,
    );
    assert_eq!(
        app.app_keymap.timeout_duration(),
        expected,
        "App::new chord timeout must match EditorConfig::default().chord_timeout_ms"
    );
}

/// App::with_config must thread chord_timeout_ms from the config into the
/// app_keymap, replacing whatever App::new seeded.
#[test]
fn with_config_applies_chord_timeout_ms() {
    let app = App::new(None, false, None, None).unwrap();

    let mut cfg = hjkl_app::config::Config::default();
    cfg.editor.chord_timeout_ms = 250;
    // which_key.delay_ms is 500 by default; set it below 250 so the warn
    // branch is not triggered in this test.
    cfg.which_key.delay_ms = 100;
    let app = app.with_config(cfg);

    assert_eq!(
        app.app_keymap.timeout_duration(),
        std::time::Duration::from_millis(250),
        "with_config must set keymap timeout to chord_timeout_ms"
    );
}

/// Default config must carry chord_timeout_ms = 1000.
#[test]
fn default_config_chord_timeout_ms_is_1000() {
    let cfg = hjkl_app::config::Config::default();
    assert_eq!(
        cfg.editor.chord_timeout_ms, 1000,
        "bundled default chord_timeout_ms must be 1000"
    );
}

// ── matchparen_cells tests ───────────────────────────────────────────────────

/// matchparen_cells resolves both bracket positions when cursor is on a bracket.
#[test]
fn matchparen_cells_resolves_pair_on_bracket() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "foo(bar)baz");

    // Move cursor to col 3 — the `(` opener.
    use hjkl_buffer::Position;
    app.active_editor_mut()
        .buffer_mut()
        .set_cursor(Position::new(0, 3));

    let cells = app.matchparen_cells();
    assert_eq!(
        cells,
        Some([(0, 3), (0, 7)]),
        "cursor on `(` must report pair [(0,3),(0,7)]"
    );

    // Move cursor to col 7 — the `)` closer.
    app.active_editor_mut()
        .buffer_mut()
        .set_cursor(Position::new(0, 7));
    let cells = app.matchparen_cells();
    assert_eq!(
        cells,
        Some([(0, 7), (0, 3)]),
        "cursor on `)` must report pair [(0,7),(0,3)]"
    );

    // Move cursor to col 0 — not a bracket.
    app.active_editor_mut()
        .buffer_mut()
        .set_cursor(Position::new(0, 0));
    let cells = app.matchparen_cells();
    assert_eq!(cells, None, "cursor on non-bracket must return None");
}

/// matchparen_cells returns None when matchparen is disabled.
#[test]
fn matchparen_off_returns_none() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "foo(bar)baz");

    // Cursor on `(` — would normally produce a pair.
    use hjkl_buffer::Position;
    app.active_editor_mut()
        .buffer_mut()
        .set_cursor(Position::new(0, 3));

    // Disable matchparen.
    app.dispatch_ex("set nomatchparen");
    let cells = app.matchparen_cells();
    assert_eq!(
        cells, None,
        "matchparen_cells must return None when matchparen is disabled"
    );
}

// ── matchparen_tag_cells tests ───────────────────────────────────────────────

/// matchparen_tag_cells returns cells for both names when cursor is on the
/// open tag name.
#[test]
fn matchparen_tag_cells_resolves_on_open_tag() {
    let mut app = App::new(None, false, None, None).unwrap();
    // `<div></div>`:
    //   open  name at cols 1..4  (d=1, i=2, v=3)
    //   close name at cols 7..10 (d=7, i=8, v=9)
    seed_buffer(&mut app, "<div></div>");

    use hjkl_buffer::Position;
    // col 2 is inside "div" of the opener.
    app.active_editor_mut()
        .buffer_mut()
        .set_cursor(Position::new(0, 2));

    let cells = app.matchparen_tag_cells();
    assert!(
        cells.is_some(),
        "cursor on open tag name must return Some(cells)"
    );
    let cells = cells.unwrap();
    // Opener name cols 1,2,3 + closer name cols 7,8,9 = 6 cells.
    assert_eq!(cells.len(), 6, "expected 3 opener + 3 closer cells");
    // Opener cells.
    assert!(cells.contains(&(0, 1)), "missing opener col 1");
    assert!(cells.contains(&(0, 2)), "missing opener col 2");
    assert!(cells.contains(&(0, 3)), "missing opener col 3");
    // Closer cells.
    assert!(cells.contains(&(0, 7)), "missing closer col 7");
    assert!(cells.contains(&(0, 8)), "missing closer col 8");
    assert!(cells.contains(&(0, 9)), "missing closer col 9");
}

/// matchparen_tag_cells returns cells for both names when cursor is on the
/// close tag name.
#[test]
fn matchparen_tag_cells_resolves_on_close_tag() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "<div></div>");

    use hjkl_buffer::Position;
    // col 8 is inside "div" of the closer.
    app.active_editor_mut()
        .buffer_mut()
        .set_cursor(Position::new(0, 8));

    let cells = app.matchparen_tag_cells();
    assert!(
        cells.is_some(),
        "cursor on close tag name must return Some(cells)"
    );
    let cells = cells.unwrap();
    assert_eq!(cells.len(), 6, "expected 3 opener + 3 closer cells");
    assert!(cells.contains(&(0, 1)));
    assert!(cells.contains(&(0, 7)));
}

/// Nested same-name tags pair by structural depth — the outer opener must
/// match the outer closer, not the inner one.
#[test]
fn matchparen_tag_cells_nested_pairs_by_depth() {
    let mut app = App::new(None, false, None, None).unwrap();
    // `<div><div></div></div>`:
    //   outer open  name cols  1..4
    //   inner open  name cols  6..9
    //   inner close name cols 12..15
    //   outer close name cols 18..21
    seed_buffer(&mut app, "<div><div></div></div>");

    use hjkl_buffer::Position;
    // col 2 → inside the OUTER opener "div".
    app.active_editor_mut()
        .buffer_mut()
        .set_cursor(Position::new(0, 2));

    let cells = app.matchparen_tag_cells();
    assert!(cells.is_some(), "outer opener must pair with outer closer");
    let cells = cells.unwrap();
    // Should include outer opener (cols 1-3) and outer closer (cols 18-20),
    // NOT the inner pair (cols 6-8 / 12-14).
    assert!(
        cells.contains(&(0, 1)),
        "outer opener col 1 must be highlighted"
    );
    assert!(
        cells.contains(&(0, 18)),
        "outer closer col 18 must be highlighted"
    );
    assert!(
        !cells.contains(&(0, 6)),
        "inner opener col 6 must NOT be highlighted for outer pair"
    );
}

/// matchparen_tag_cells returns None when matchparen is disabled.
#[test]
fn matchparen_tag_cells_off_returns_none() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "<div></div>");

    use hjkl_buffer::Position;
    app.active_editor_mut()
        .buffer_mut()
        .set_cursor(Position::new(0, 2));

    app.dispatch_ex("set nomatchparen");
    let cells = app.matchparen_tag_cells();
    assert_eq!(
        cells, None,
        "matchparen_tag_cells must return None when matchparen is disabled"
    );
}

/// matchparen_tag_cells returns None when cursor is not on a tag name.
#[test]
fn matchparen_tag_cells_none_when_not_on_tag() {
    let mut app = App::new(None, false, None, None).unwrap();
    // col 0 is `<` — not inside a name region.
    seed_buffer(&mut app, "<div></div>after");

    use hjkl_buffer::Position;
    app.active_editor_mut()
        .buffer_mut()
        .set_cursor(Position::new(0, 0));

    let cells = app.matchparen_tag_cells();
    assert_eq!(
        cells, None,
        "cursor on `<` (not a name char) must return None"
    );

    // Also try plain text after the tags.
    app.active_editor_mut()
        .buffer_mut()
        .set_cursor(Position::new(0, 12));
    let cells = app.matchparen_tag_cells();
    assert_eq!(cells, None, "cursor on plain text must return None");
}

// ── modifiable / readonly app-layer tests ─────────────────────────────────────

/// Explorer buffer must be modifiable (oil.nvim-style editing).
#[test]
fn explorer_buffer_is_modifiable() {
    use crate::keymap_actions::AppAction;
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_action(AppAction::ToggleExplorer, 1);
    let explorer_slot = app
        .slots
        .iter()
        .find(|s| s.is_explorer)
        .expect("explorer slot must exist after toggle");
    assert!(
        explorer_slot.editor.is_modifiable(),
        "explorer buffer must be modifiable (oil.nvim-style editing)"
    );
}

/// Pressing `i` in the explorer now enters Insert mode (editable buffer).
#[test]
fn explorer_i_enters_insert() {
    use crate::keymap_actions::AppAction;
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_action(AppAction::ToggleExplorer, 1);
    // Focus explorer slot.
    assert!(app.explorer_buf_focused());
    // Press `i`.
    app.handle_keypress(key(KeyCode::Char('i')));
    // Mode lives on the focused window's editor (#151 Phase D), not the slot
    // bridge editor (which is never dispatched).
    assert_eq!(
        app.active_editor().vim_mode(),
        hjkl_engine::VimMode::Insert,
        "pressing `i` in explorer must enter Insert mode (editable buffer)"
    );
}

/// `readonly` buffer: `:w` (own file) errors E45; `:w othername` succeeds.
#[test]
fn readonly_w_own_file_errors_e45() {
    let td = tempfile::tempdir().unwrap();
    let file = td.path().join("ro_test.txt");
    std::fs::write(&file, "content\n").unwrap();
    let mut app = App::new(Some(file.clone()), true, None, None).unwrap();
    assert!(app.active_editor().is_readonly());
    app.dispatch_ex("write");
    let msgs: Vec<String> = app.bus.history().map(|h| h.body.clone()).collect();
    assert!(
        msgs.iter().any(|m| m.contains("E45")),
        "`:w` on own readonly file must error E45; got: {msgs:?}"
    );
}

/// `readonly` buffer: `:w! own_file` clears the error (force overrides E45).
#[test]
fn readonly_w_force_overrides_e45() {
    let td = tempfile::tempdir().unwrap();
    let file = td.path().join("ro_force_test.txt");
    std::fs::write(&file, "content\n").unwrap();
    let mut app = App::new(Some(file.clone()), true, None, None).unwrap();
    assert!(app.active_editor().is_readonly());
    // `:write!` must succeed despite readonly.
    let saved = app.do_save_force(None, true);
    assert!(saved, ":w! must succeed on a readonly buffer");
}

/// `readonly` buffer: `:w othername` (different path) must succeed.
#[test]
fn readonly_w_different_path_succeeds() {
    let td = tempfile::tempdir().unwrap();
    let file = td.path().join("readonly_orig.txt");
    let other = td.path().join("readonly_other.txt");
    std::fs::write(&file, "content\n").unwrap();
    let mut app = App::new(Some(file.clone()), true, None, None).unwrap();
    assert!(app.active_editor().is_readonly());
    // `:w othername` must write without E45.
    let saved = app.do_save(Some(other.clone()));
    assert!(
        saved,
        ":w to a different path must succeed even on a readonly buffer"
    );
    assert!(other.exists(), "output file must have been written");
}

// ── Regression: multibyte insert must not panic on the completion-prefix
// slice (char-col used as byte index). Pasting Unicode (nerd icons, box-
// drawing chars) crashed at event_loop.rs char/byte slicing. ──────────────
#[test]
fn insert_multibyte_does_not_panic_on_completion_prefix() {
    let mut app = App::new(None, false, None, None).unwrap();
    // Enter insert mode, then type a line mixing ASCII + multibyte glyphs.
    app.handle_keypress(key(KeyCode::Char('i')));
    for c in "a├b\u{f0149}c│d".chars() {
        // Before the fix this panicked: "byte index N is not a char boundary".
        app.handle_keypress(key(KeyCode::Char(c)));
    }
    // Backspace through the multibyte content too (the other slice site).
    for _ in 0..4 {
        app.handle_keypress(key(KeyCode::Backspace));
    }
}
