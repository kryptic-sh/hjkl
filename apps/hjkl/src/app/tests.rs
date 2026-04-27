use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}
fn ctrl_key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn type_str(app: &mut App, text: &str) {
    for c in text.chars() {
        app.handle_command_field_key(key(KeyCode::Char(c)));
    }
}

// ── Command palette (`:`) tests ─────────────────────────────────────────

#[test]
fn palette_open_and_submit_runs_dispatch_and_closes() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.open_command_prompt();
    assert!(app.command_field.is_some());
    type_str(&mut app, "wq");
    assert_eq!(app.command_field.as_ref().unwrap().text(), "wq");
    app.handle_command_field_key(key(KeyCode::Enter));
    assert!(app.command_field.is_none());
    assert!(app.exit_requested);
}

#[test]
fn palette_esc_in_insert_drops_to_normal_then_motions_apply() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.open_command_prompt();
    type_str(&mut app, "abc");
    app.handle_command_field_key(key(KeyCode::Esc));
    assert!(app.command_field.is_some());
    let f = app.command_field.as_ref().unwrap();
    assert_eq!(f.vim_mode(), VimMode::Normal);
    assert_eq!(f.text(), "abc");
    app.handle_command_field_key(key(KeyCode::Char('b')));
    app.handle_command_field_key(key(KeyCode::Char('d')));
    app.handle_command_field_key(key(KeyCode::Char('w')));
    let f = app.command_field.as_ref().unwrap();
    assert_eq!(f.text(), "");
    app.handle_command_field_key(key(KeyCode::Esc));
    assert!(app.command_field.is_none());
}

#[test]
fn palette_ctrl_c_cancels_without_quitting_app() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.open_command_prompt();
    type_str(&mut app, "wq");
    let cc = ctrl_key('c');
    if app.command_field.is_some()
        && cc.code == KeyCode::Char('c')
        && cc.modifiers.contains(KeyModifiers::CONTROL)
    {
        app.command_field = None;
    }
    assert!(app.command_field.is_none());
    assert!(!app.exit_requested);
}

// ── Search prompt (`/` `?`) tests ───────────────────────────────────────

fn type_search(app: &mut App, text: &str) {
    for c in text.chars() {
        app.handle_search_field_key(key(KeyCode::Char(c)));
    }
}

fn seed_buffer(app: &mut App, content: &str) {
    BufferEdit::replace_all(app.active_mut().editor.buffer_mut(), content);
}

#[test]
fn search_open_and_type_drives_live_preview() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "foo bar foo baz");
    app.open_search_prompt(SearchDir::Forward);
    assert!(app.search_field.is_some());
    type_search(&mut app, "foo");
    assert_eq!(app.search_field.as_ref().unwrap().text(), "foo");
    assert!(app.active().editor.search_state().pattern.is_some());
}

#[test]
fn search_more_typing_updates_pattern() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "foobar foozle");
    app.open_search_prompt(SearchDir::Forward);
    type_search(&mut app, "foo");
    let p1 = app
        .active()
        .editor
        .search_state()
        .pattern
        .as_ref()
        .unwrap()
        .as_str()
        .to_string();
    type_search(&mut app, "z");
    let p2 = app
        .active()
        .editor
        .search_state()
        .pattern
        .as_ref()
        .unwrap()
        .as_str()
        .to_string();
    assert_ne!(p1, p2, "pattern must update on further typing");
}

#[test]
fn search_motion_in_normal_edits_prompt_and_updates_highlight() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "alpha beta\ngamma");
    app.open_search_prompt(SearchDir::Forward);
    type_search(&mut app, "alpha beta");
    app.handle_search_field_key(key(KeyCode::Esc));
    assert_eq!(
        app.search_field.as_ref().unwrap().vim_mode(),
        VimMode::Normal
    );
    app.handle_search_field_key(key(KeyCode::Char('b')));
    app.handle_search_field_key(key(KeyCode::Char('d')));
    app.handle_search_field_key(key(KeyCode::Char('b')));
    let new_text = app.search_field.as_ref().unwrap().text();
    assert!(
        new_text.len() < "alpha beta".len(),
        "prompt text shrank: {new_text:?}"
    );
}

#[test]
fn search_enter_commits_and_advances_cursor() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "alpha\nbeta\nfoo here\ndone");
    app.open_search_prompt(SearchDir::Forward);
    type_search(&mut app, "foo");
    app.handle_search_field_key(key(KeyCode::Enter));
    assert!(app.search_field.is_none());
    let (row, col) = app.active().editor.cursor();
    assert_eq!(row, 2);
    assert_eq!(col, 0);
    assert_eq!(app.active().editor.last_search(), Some("foo"));
    assert!(app.active().editor.last_search_forward());
}

#[test]
fn search_esc_twice_cancels_and_clears_when_no_prior_search() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "abc def");
    app.open_search_prompt(SearchDir::Forward);
    type_search(&mut app, "abc");
    app.handle_search_field_key(key(KeyCode::Esc));
    assert!(app.search_field.is_some());
    app.handle_search_field_key(key(KeyCode::Esc));
    assert!(app.search_field.is_none());
    assert!(app.active().editor.search_state().pattern.is_none());
}

#[test]
fn search_backward_prompt_uses_question_dir() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "foo here\nbar there\nfoo again");
    app.active_mut().editor.goto_line(3);
    app.open_search_prompt(SearchDir::Backward);
    type_search(&mut app, "foo");
    app.handle_search_field_key(key(KeyCode::Enter));
    let (row, _) = app.active().editor.cursor();
    assert_eq!(row, 0);
    assert!(!app.active().editor.last_search_forward());
}

// ── App::new tests ──────────────────────────────────────────────────────

#[test]
fn app_new_no_file() {
    let app = App::new(None, false, None, None).unwrap();
    assert!(!app.active().dirty);
    assert!(!app.active().is_new_file);
    assert!(app.active().filename.is_none());
    assert!(!app.active().editor.is_readonly());
}

#[test]
fn app_new_readonly_flag() {
    let app = App::new(None, true, None, None).unwrap();
    assert!(app.active().editor.is_readonly());
}

#[test]
fn app_new_not_found_sets_is_new_file() {
    let path = std::path::PathBuf::from("/tmp/hjkl_phase5_nonexistent_abc123.txt");
    let _ = std::fs::remove_file(&path);
    let app = App::new(Some(path), false, None, None).unwrap();
    assert!(app.active().is_new_file);
    assert!(!app.active().dirty);
}

#[test]
fn app_new_goto_line_clamps() {
    let app = App::new(None, false, Some(999), None).unwrap();
    let (row, _col) = app.active().editor.cursor();
    assert_eq!(row, 0);
}

#[test]
fn do_save_readonly_blocked() {
    let mut app = App::new(None, true, None, None).unwrap();
    app.active_mut().filename = Some(std::path::PathBuf::from("/tmp/hjkl_phase5_ro_test.txt"));
    app.do_save(None);
    let msg = app.status_message.unwrap_or_default();
    assert!(
        msg.contains("E45"),
        "expected E45 readonly error, got: {msg}"
    );
}

#[test]
fn do_save_no_filename_e32() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.do_save(None);
    let msg = app.status_message.unwrap_or_default();
    assert!(msg.contains("E32"), "expected E32, got: {msg}");
}

// ── :e tests ────────────────────────────────────────────────────────────

#[test]
fn edit_percent_reloads_current_file() {
    let path = std::env::temp_dir().join("hjkl_edit_percent_reload.txt");
    std::fs::write(&path, "first\nsecond\n").unwrap();
    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
    std::fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();
    app.dispatch_ex("e %");
    let lines = app.active().editor.buffer().lines();
    assert_eq!(lines, vec!["alpha", "beta", "gamma"]);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn edit_no_arg_reloads_current_file() {
    let path = std::env::temp_dir().join("hjkl_edit_noarg_reload.txt");
    std::fs::write(&path, "v1\n").unwrap();
    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
    std::fs::write(&path, "v2\n").unwrap();
    app.dispatch_ex("e");
    assert_eq!(app.active().editor.buffer().lines(), vec!["v2".to_string()]);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn edit_blocks_dirty_buffer_without_force() {
    let path = std::env::temp_dir().join("hjkl_edit_dirty_block.txt");
    std::fs::write(&path, "orig\n").unwrap();
    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
    app.active_mut().dirty = true;
    app.dispatch_ex("e %");
    let msg = app.status_message.unwrap_or_default();
    assert!(msg.contains("E37"), "expected E37, got: {msg}");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn edit_force_reloads_dirty_buffer() {
    let path = std::env::temp_dir().join("hjkl_edit_force.txt");
    std::fs::write(&path, "disk\n").unwrap();
    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
    app.active_mut().dirty = true;
    app.dispatch_ex("e!");
    assert_eq!(
        app.active().editor.buffer().lines(),
        vec!["disk".to_string()]
    );
    assert!(!app.active().dirty);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn undo_to_saved_state_clears_dirty() {
    let path = std::env::temp_dir().join("hjkl_undo_clears_dirty.txt");
    std::fs::write(&path, "alpha\nbravo\n").unwrap();
    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
    assert!(!app.active().dirty);
    app.active_mut().editor.handle_key(key(KeyCode::Char('i')));
    app.active_mut().editor.handle_key(key(KeyCode::Char('X')));
    if app.active_mut().editor.take_dirty() {
        app.active_mut().refresh_dirty_against_saved();
    }
    assert!(app.active().dirty, "edit should mark dirty");
    app.active_mut().editor.handle_key(key(KeyCode::Esc));
    app.active_mut().editor.handle_key(key(KeyCode::Char('u')));
    if app.active_mut().editor.take_dirty() {
        app.active_mut().refresh_dirty_against_saved();
    }
    assert!(
        !app.active().dirty,
        "undo to saved state should clear dirty"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn esc_on_empty_command_prompt_dismisses() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.open_command_prompt();
    assert!(app.command_field.is_some());
    app.handle_command_field_key(key(KeyCode::Esc));
    assert!(
        app.command_field.is_none(),
        "empty : prompt should close on Esc"
    );
}

#[test]
fn esc_on_nonempty_command_drops_to_normal_then_closes() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.open_command_prompt();
    app.handle_command_field_key(key(KeyCode::Char('w')));
    app.handle_command_field_key(key(KeyCode::Esc));
    assert!(app.command_field.is_some());
    assert_eq!(
        app.command_field.as_ref().unwrap().vim_mode(),
        VimMode::Normal
    );
    app.handle_command_field_key(key(KeyCode::Esc));
    assert!(app.command_field.is_none());
}

#[test]
fn esc_on_empty_search_prompt_dismisses() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.open_search_prompt(SearchDir::Forward);
    assert!(app.search_field.is_some());
    app.handle_search_field_key(key(KeyCode::Esc));
    assert!(
        app.search_field.is_none(),
        "empty / prompt should close on Esc"
    );
}

#[test]
fn edit_no_arg_no_filename_e32() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("e");
    let msg = app.status_message.unwrap_or_default();
    assert!(msg.contains("E32"), "expected E32, got: {msg}");
}

// ── Phase C: multi-buffer tests ─────────────────────────────────────────

#[test]
fn edit_new_path_appends_slot_and_switches() {
    let path_a = std::env::temp_dir().join("hjkl_phc_a.txt");
    let path_b = std::env::temp_dir().join("hjkl_phc_b.txt");
    std::fs::write(&path_a, "alpha\n").unwrap();
    std::fs::write(&path_b, "beta\n").unwrap();
    let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
    assert_eq!(app.slots.len(), 1);
    app.dispatch_ex(&format!("e {}", path_b.display()));
    assert_eq!(app.slots.len(), 2);
    assert_eq!(app.active, 1);
    assert_eq!(
        app.active().editor.buffer().lines(),
        vec!["beta".to_string()]
    );
    let _ = std::fs::remove_file(&path_a);
    let _ = std::fs::remove_file(&path_b);
}

#[test]
fn edit_existing_path_switches_to_open_slot() {
    let path_a = std::env::temp_dir().join("hjkl_phc_switch_a.txt");
    let path_b = std::env::temp_dir().join("hjkl_phc_switch_b.txt");
    std::fs::write(&path_a, "alpha\n").unwrap();
    std::fs::write(&path_b, "beta\n").unwrap();
    let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
    app.dispatch_ex(&format!("e {}", path_b.display()));
    assert_eq!(app.active, 1);
    // Re-open path_a → switch back, no third slot.
    app.dispatch_ex(&format!("e {}", path_a.display()));
    assert_eq!(app.slots.len(), 2);
    assert_eq!(app.active, 0);
    let _ = std::fs::remove_file(&path_a);
    let _ = std::fs::remove_file(&path_b);
}

#[test]
fn edit_other_open_path_does_not_block_on_dirty() {
    let path_a = std::env::temp_dir().join("hjkl_phc_dirty_a.txt");
    let path_b = std::env::temp_dir().join("hjkl_phc_dirty_b.txt");
    std::fs::write(&path_a, "a\n").unwrap();
    std::fs::write(&path_b, "b\n").unwrap();
    let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
    app.active_mut().dirty = true;
    // Switching to a *different* file must not be gated on the
    // current slot's dirty flag — the slot isn't being destroyed.
    app.dispatch_ex(&format!("e {}", path_b.display()));
    assert_eq!(app.active, 1);
    assert!(app.slots[0].dirty, "slot 0 should remain dirty");
    let _ = std::fs::remove_file(&path_a);
    let _ = std::fs::remove_file(&path_b);
}

#[test]
fn bnext_bprev_cycle_active() {
    let path_a = std::env::temp_dir().join("hjkl_phc_cycle_a.txt");
    let path_b = std::env::temp_dir().join("hjkl_phc_cycle_b.txt");
    let path_c = std::env::temp_dir().join("hjkl_phc_cycle_c.txt");
    for p in [&path_a, &path_b, &path_c] {
        std::fs::write(p, "x\n").unwrap();
    }
    let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
    app.dispatch_ex(&format!("e {}", path_b.display()));
    app.dispatch_ex(&format!("e {}", path_c.display()));
    assert_eq!(app.active, 2);
    app.dispatch_ex("bn");
    assert_eq!(app.active, 0, "wrap forward to 0");
    app.dispatch_ex("bn");
    assert_eq!(app.active, 1);
    app.dispatch_ex("bp");
    assert_eq!(app.active, 0);
    app.dispatch_ex("bp");
    assert_eq!(app.active, 2, "wrap backward to last");
    for p in [&path_a, &path_b, &path_c] {
        let _ = std::fs::remove_file(p);
    }
}

#[test]
fn bnext_no_op_with_single_slot() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("bn");
    assert_eq!(app.active, 0);
    assert_eq!(app.slots.len(), 1);
}

#[test]
fn bdelete_blocks_dirty_without_force() {
    let path_a = std::env::temp_dir().join("hjkl_phc_bd_a.txt");
    let path_b = std::env::temp_dir().join("hjkl_phc_bd_b.txt");
    std::fs::write(&path_a, "a\n").unwrap();
    std::fs::write(&path_b, "b\n").unwrap();
    let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
    app.dispatch_ex(&format!("e {}", path_b.display()));
    app.active_mut().dirty = true;
    app.dispatch_ex("bd");
    let msg = app.status_message.clone().unwrap_or_default();
    assert!(msg.contains("E89"), "expected E89, got: {msg}");
    assert_eq!(app.slots.len(), 2);
    let _ = std::fs::remove_file(&path_a);
    let _ = std::fs::remove_file(&path_b);
}

#[test]
fn bdelete_force_removes_dirty_slot() {
    let path_a = std::env::temp_dir().join("hjkl_phc_bdforce_a.txt");
    let path_b = std::env::temp_dir().join("hjkl_phc_bdforce_b.txt");
    std::fs::write(&path_a, "a\n").unwrap();
    std::fs::write(&path_b, "b\n").unwrap();
    let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
    app.dispatch_ex(&format!("e {}", path_b.display()));
    app.active_mut().dirty = true;
    app.dispatch_ex("bd!");
    assert_eq!(app.slots.len(), 1);
    assert_eq!(app.active, 0);
    assert_eq!(app.active().editor.buffer().lines(), vec!["a".to_string()]);
    let _ = std::fs::remove_file(&path_a);
    let _ = std::fs::remove_file(&path_b);
}

#[test]
fn bdelete_on_last_slot_resets_to_no_name() {
    let path = std::env::temp_dir().join("hjkl_phc_bd_last.txt");
    std::fs::write(&path, "content\n").unwrap();
    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
    app.dispatch_ex("bd");
    assert_eq!(app.slots.len(), 1);
    assert!(app.active().filename.is_none());
    let lines = app.active().editor.buffer().lines();
    assert!(
        lines.is_empty() || (lines.len() == 1 && lines[0].is_empty()),
        "expected empty scratch buffer, got: {lines:?}"
    );
    let _ = std::fs::remove_file(&path);
}

// ── Alt-buffer (D2) tests ───────────────────────────────────────────────

#[test]
fn buffer_alt_swaps_with_prev_active() {
    let path_a = std::env::temp_dir().join("hjkl_d2_alt_a.txt");
    let path_b = std::env::temp_dir().join("hjkl_d2_alt_b.txt");
    let path_c = std::env::temp_dir().join("hjkl_d2_alt_c.txt");
    for p in [&path_a, &path_b, &path_c] {
        std::fs::write(p, "x\n").unwrap();
    }
    let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
    app.dispatch_ex(&format!("e {}", path_b.display())); // active=1, prev=0
    app.dispatch_ex(&format!("e {}", path_c.display())); // active=2, prev=1
    assert_eq!(app.active, 2);
    assert_eq!(app.prev_active, Some(1));

    // First alt: go back to 1, prev becomes 2.
    app.buffer_alt();
    assert_eq!(app.active, 1);
    assert_eq!(app.prev_active, Some(2));

    // Second alt: go back to 2.
    app.buffer_alt();
    assert_eq!(app.active, 2);

    for p in [&path_a, &path_b, &path_c] {
        let _ = std::fs::remove_file(p);
    }
}

#[test]
fn buffer_alt_with_single_slot_no_op_with_message() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert_eq!(app.slots.len(), 1);
    app.buffer_alt();
    assert_eq!(app.active, 0);
    let msg = app.status_message.clone().unwrap_or_default();
    assert!(
        msg.contains("only one buffer"),
        "expected 'only one buffer' message, got: {msg}"
    );
}

#[test]
fn bd_clears_prev_active() {
    let path_a = std::env::temp_dir().join("hjkl_d2_bd_a.txt");
    let path_b = std::env::temp_dir().join("hjkl_d2_bd_b.txt");
    std::fs::write(&path_a, "a\n").unwrap();
    std::fs::write(&path_b, "b\n").unwrap();
    let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
    app.dispatch_ex(&format!("e {}", path_b.display())); // active=1, prev=0
    assert_eq!(app.prev_active, Some(0));
    // Force-close the active slot (b.txt).
    app.dispatch_ex("bd!");
    // prev_active must be reset so the stale index is gone.
    assert!(
        app.prev_active.is_none(),
        "prev_active should be None after bd!"
    );
    let _ = std::fs::remove_file(&path_a);
    let _ = std::fs::remove_file(&path_b);
}

// ── Buffer picker (D4) source tests ────────────────────────────────────

#[test]
fn buffer_source_new_produces_n_entries() {
    let path_a = std::env::temp_dir().join("hjkl_d4_src_a.txt");
    let path_b = std::env::temp_dir().join("hjkl_d4_src_b.txt");
    std::fs::write(&path_a, "a\n").unwrap();
    std::fs::write(&path_b, "b\n").unwrap();
    let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
    app.dispatch_ex(&format!("e {}", path_b.display()));
    assert_eq!(app.slots.len(), 2);

    let source = crate::picker::BufferSource::new(
        &app.slots,
        |s| {
            s.filename
                .as_ref()
                .and_then(|p| p.to_str())
                .unwrap_or("[No Name]")
                .to_owned()
        },
        |s| s.dirty,
    );
    // Build a Picker from the source — it calls enumerate internally.
    let mut picker = crate::picker::BufferPicker::new(source);
    picker.refresh();
    assert_eq!(picker.total(), 2, "expected 2 entries");
    assert!(picker.scan_done(), "scan_done must be set");
    let _ = std::fs::remove_file(&path_a);
    let _ = std::fs::remove_file(&path_b);
}

#[test]
fn buffer_source_select_returns_switch_buffer() {
    use crate::picker::{BufferSource, PickerAction, PickerSource};
    let path = std::env::temp_dir().join("hjkl_d4_sel.txt");
    std::fs::write(&path, "x\n").unwrap();
    let app = App::new(Some(path.clone()), false, None, None).unwrap();
    let source = BufferSource::new(
        &app.slots,
        |s| {
            s.filename
                .as_ref()
                .and_then(|p| p.to_str())
                .unwrap_or("[No Name]")
                .to_owned()
        },
        |s| s.dirty,
    );
    let entry = crate::picker::BufferEntry {
        idx: 0,
        name: "foo".into(),
        dirty: false,
    };
    match source.select(&entry) {
        PickerAction::SwitchBuffer(i) => assert_eq!(i, 0),
        _ => panic!("expected SwitchBuffer(0)"),
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn open_extra_adds_slot_and_leaves_active_zero() {
    let path_a = std::env::temp_dir().join("hjkl_open_extra_a.txt");
    let path_b = std::env::temp_dir().join("hjkl_open_extra_b.txt");
    std::fs::write(&path_a, "first\n").unwrap();
    std::fs::write(&path_b, "second\n").unwrap();
    let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
    assert_eq!(app.slots.len(), 1);
    assert_eq!(app.active, 0);
    app.open_extra(path_b.clone()).unwrap();
    assert_eq!(app.slots.len(), 2, "extra slot should have been added");
    assert_eq!(app.active, 0, "active must stay at 0 after open_extra");
    assert_eq!(
        app.slots[0].editor.buffer().lines(),
        vec!["first".to_string()]
    );
    assert_eq!(
        app.slots[1].editor.buffer().lines(),
        vec!["second".to_string()]
    );
    let _ = std::fs::remove_file(&path_a);
    let _ = std::fs::remove_file(&path_b);
}

#[test]
fn ls_lists_all_buffers_with_active_marker() {
    let path_a = std::env::temp_dir().join("hjkl_phc_ls_a.txt");
    let path_b = std::env::temp_dir().join("hjkl_phc_ls_b.txt");
    std::fs::write(&path_a, "a\n").unwrap();
    std::fs::write(&path_b, "b\n").unwrap();
    let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
    app.dispatch_ex(&format!("e {}", path_b.display()));
    app.dispatch_ex("ls");
    let msg = app.status_message.clone().unwrap_or_default();
    assert!(msg.contains("1: "), "expected slot 1 entry, got: {msg}");
    assert!(
        msg.contains("2:%"),
        "active marker missing on slot 2: {msg}"
    );
    let _ = std::fs::remove_file(&path_a);
    let _ = std::fs::remove_file(&path_b);
}

// ── Phase E: multi-buffer ex-command parity tests ──────────────────────

#[test]
fn b_num_switches_by_index() {
    let path_a = std::env::temp_dir().join("hjkl_phe_bnum_a.txt");
    let path_b = std::env::temp_dir().join("hjkl_phe_bnum_b.txt");
    let path_c = std::env::temp_dir().join("hjkl_phe_bnum_c.txt");
    for p in [&path_a, &path_b, &path_c] {
        std::fs::write(p, "x\n").unwrap();
    }
    let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
    app.dispatch_ex(&format!("e {}", path_b.display()));
    app.dispatch_ex(&format!("e {}", path_c.display()));
    assert_eq!(app.slots.len(), 3);
    app.dispatch_ex("b 2");
    assert_eq!(app.active, 1, "`:b 2` should switch to index 1");
    for p in [&path_a, &path_b, &path_c] {
        let _ = std::fs::remove_file(p);
    }
}

#[test]
fn b_num_out_of_range_errors() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert_eq!(app.slots.len(), 1);
    app.dispatch_ex("b 5");
    let msg = app.status_message.clone().unwrap_or_default();
    assert!(msg.contains("E86"), "expected E86, got: {msg}");
}

#[test]
fn b_name_substring_switches() {
    let path_foo = std::env::temp_dir().join("hjkl_phe_bname_foo.txt");
    let path_bar = std::env::temp_dir().join("hjkl_phe_bname_bar.txt");
    std::fs::write(&path_foo, "foo\n").unwrap();
    std::fs::write(&path_bar, "bar\n").unwrap();
    let mut app = App::new(Some(path_foo.clone()), false, None, None).unwrap();
    app.dispatch_ex(&format!("e {}", path_bar.display()));
    assert_eq!(app.active, 1);
    // Switch to the foo slot by substring
    app.dispatch_ex("b foo");
    assert_eq!(app.active, 0, "`:b foo` should switch to foo's slot");
    let _ = std::fs::remove_file(&path_foo);
    let _ = std::fs::remove_file(&path_bar);
}

#[test]
fn b_name_ambiguous_errors() {
    let path_a = std::env::temp_dir().join("hjkl_phe_bamb_foo_a.txt");
    let path_b = std::env::temp_dir().join("hjkl_phe_bamb_foo_b.txt");
    std::fs::write(&path_a, "a\n").unwrap();
    std::fs::write(&path_b, "b\n").unwrap();
    let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
    app.dispatch_ex(&format!("e {}", path_b.display()));
    // Both filenames contain "foo" — ambiguous
    app.dispatch_ex("b foo");
    let msg = app.status_message.clone().unwrap_or_default();
    assert!(
        msg.contains("E93"),
        "expected E93 ambiguous error, got: {msg}"
    );
    let _ = std::fs::remove_file(&path_a);
    let _ = std::fs::remove_file(&path_b);
}

#[test]
fn bfirst_blast_jump_to_ends() {
    let path_a = std::env::temp_dir().join("hjkl_phe_bfl_a.txt");
    let path_b = std::env::temp_dir().join("hjkl_phe_bfl_b.txt");
    let path_c = std::env::temp_dir().join("hjkl_phe_bfl_c.txt");
    for p in [&path_a, &path_b, &path_c] {
        std::fs::write(p, "x\n").unwrap();
    }
    let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
    app.dispatch_ex(&format!("e {}", path_b.display()));
    app.dispatch_ex(&format!("e {}", path_c.display()));
    assert_eq!(app.slots.len(), 3);
    // Start in middle
    app.dispatch_ex("b 2");
    assert_eq!(app.active, 1);
    app.dispatch_ex("bfirst");
    assert_eq!(app.active, 0, "`:bfirst` should go to slot 0");
    app.dispatch_ex("blast");
    assert_eq!(app.active, 2, "`:blast` should go to last slot");
    for p in [&path_a, &path_b, &path_c] {
        let _ = std::fs::remove_file(p);
    }
}

#[test]
fn wa_writes_dirty_named_slots() {
    let path_a = std::env::temp_dir().join("hjkl_phe_wa_a.txt");
    let path_b = std::env::temp_dir().join("hjkl_phe_wa_b.txt");
    std::fs::write(&path_a, "original a\n").unwrap();
    std::fs::write(&path_b, "original b\n").unwrap();
    let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
    app.dispatch_ex(&format!("e {}", path_b.display()));
    // Mark both slots dirty with new content
    app.slots[0].dirty = true;
    BufferEdit::replace_all(app.slots[0].editor.buffer_mut(), "edited a");
    app.slots[1].dirty = true;
    BufferEdit::replace_all(app.slots[1].editor.buffer_mut(), "edited b");
    app.dispatch_ex("wa");
    assert!(!app.slots[0].dirty, "slot 0 should be clean after :wa");
    assert!(!app.slots[1].dirty, "slot 1 should be clean after :wa");
    let contents_a = std::fs::read_to_string(&path_a).unwrap_or_default();
    let contents_b = std::fs::read_to_string(&path_b).unwrap_or_default();
    assert!(
        contents_a.contains("edited a"),
        "file a should contain edited content, got: {contents_a}"
    );
    assert!(
        contents_b.contains("edited b"),
        "file b should contain edited content, got: {contents_b}"
    );
    let _ = std::fs::remove_file(&path_a);
    let _ = std::fs::remove_file(&path_b);
}

#[test]
fn qa_blocks_when_any_slot_dirty() {
    let path_a = std::env::temp_dir().join("hjkl_phe_qa_dirty_a.txt");
    let path_b = std::env::temp_dir().join("hjkl_phe_qa_dirty_b.txt");
    std::fs::write(&path_a, "a\n").unwrap();
    std::fs::write(&path_b, "b\n").unwrap();
    let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
    app.dispatch_ex(&format!("e {}", path_b.display()));
    app.slots[0].dirty = true;
    app.dispatch_ex("qa");
    assert!(
        !app.exit_requested,
        ":qa should not exit when dirty slot exists"
    );
    let msg = app.status_message.clone().unwrap_or_default();
    assert!(msg.contains("E37"), "expected E37, got: {msg}");
    let _ = std::fs::remove_file(&path_a);
    let _ = std::fs::remove_file(&path_b);
}

#[test]
fn qa_force_exits_with_dirty() {
    let path_a = std::env::temp_dir().join("hjkl_phe_qa_force_a.txt");
    std::fs::write(&path_a, "a\n").unwrap();
    let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
    app.slots[0].dirty = true;
    app.dispatch_ex("qa!");
    assert!(app.exit_requested, ":qa! should exit even when dirty");
    let _ = std::fs::remove_file(&path_a);
}

#[test]
fn q_on_multi_slot_closes_slot_not_app() {
    let path_a = std::env::temp_dir().join("hjkl_phe_q_multi_a.txt");
    let path_b = std::env::temp_dir().join("hjkl_phe_q_multi_b.txt");
    std::fs::write(&path_a, "a\n").unwrap();
    std::fs::write(&path_b, "b\n").unwrap();
    let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
    app.dispatch_ex(&format!("e {}", path_b.display()));
    assert_eq!(app.slots.len(), 2);
    app.dispatch_ex("q!");
    assert_eq!(
        app.slots.len(),
        1,
        "`:q!` with 2 slots should close active slot"
    );
    assert!(
        !app.exit_requested,
        "app should remain open after closing one slot"
    );
    let _ = std::fs::remove_file(&path_a);
    let _ = std::fs::remove_file(&path_b);
}

#[test]
fn q_on_last_slot_quits_app() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert_eq!(app.slots.len(), 1);
    assert!(!app.active().dirty);
    app.dispatch_ex("q");
    assert!(app.exit_requested, "`:q` on clean last slot should exit");
}
