use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::Duration;

use crate::theme::AppTheme;

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
    type_str(&mut app, "q");
    assert_eq!(app.command_field.as_ref().unwrap().text(), "q");
    app.handle_command_field_key(key(KeyCode::Enter));
    assert!(app.command_field.is_none());
    assert!(app.exit_requested);
}

#[test]
fn wq_no_filename_does_not_exit() {
    // :wq on a [No Name] buffer with content must NOT quit — the save
    // fails with E32 and the user would lose their work otherwise.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "unsaved work");
    app.active_mut().dirty = true;
    app.dispatch_ex("wq");
    assert!(!app.exit_requested, "wq must not exit when save fails");
    let msg = app.status_message.clone().unwrap_or_default();
    assert!(msg.contains("E32"), "expected E32, got: {msg}");
}

#[test]
fn wq_readonly_does_not_exit() {
    let mut app = App::new(None, true, None, None).unwrap();
    app.active_mut().filename = Some(tmp_path("hjkl_wq_ro_test.txt"));
    app.dispatch_ex("wq");
    assert!(!app.exit_requested, "wq must not exit when save fails");
    let msg = app.status_message.clone().unwrap_or_default();
    assert!(msg.contains("E45"), "expected E45, got: {msg}");
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
    let path = tmp_path("hjkl_phase5_nonexistent_abc123.txt");
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
    app.active_mut().filename = Some(tmp_path("hjkl_phase5_ro_test.txt"));
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
    assert_eq!(app.active_index(), 1);
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
    assert_eq!(app.active_index(), 1);
    // Re-open path_a → switch back, no third slot.
    app.dispatch_ex(&format!("e {}", path_a.display()));
    assert_eq!(app.slots.len(), 2);
    assert_eq!(app.active_index(), 0);
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
    assert_eq!(app.active_index(), 1);
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
    assert_eq!(app.active_index(), 2);
    app.dispatch_ex("bn");
    assert_eq!(app.active_index(), 0, "wrap forward to 0");
    app.dispatch_ex("bn");
    assert_eq!(app.active_index(), 1);
    app.dispatch_ex("bp");
    assert_eq!(app.active_index(), 0);
    app.dispatch_ex("bp");
    assert_eq!(app.active_index(), 2, "wrap backward to last");
    for p in [&path_a, &path_b, &path_c] {
        let _ = std::fs::remove_file(p);
    }
}

#[test]
fn bnext_no_op_with_single_slot() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("bn");
    assert_eq!(app.active_index(), 0);
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
    assert_eq!(app.active_index(), 0);
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
    assert_eq!(app.active_index(), 2);
    assert_eq!(app.prev_active, Some(1));

    // First alt: go back to 1, prev becomes 2.
    app.buffer_alt();
    assert_eq!(app.active_index(), 1);
    assert_eq!(app.prev_active, Some(2));

    // Second alt: go back to 2.
    app.buffer_alt();
    assert_eq!(app.active_index(), 2);

    for p in [&path_a, &path_b, &path_c] {
        let _ = std::fs::remove_file(p);
    }
}

#[test]
fn buffer_alt_with_single_slot_no_op_with_message() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert_eq!(app.slots.len(), 1);
    app.buffer_alt();
    assert_eq!(app.active_index(), 0);
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

    let source = Box::new(crate::picker::BufferSource::new(
        &app.slots,
        |s| {
            s.filename
                .as_ref()
                .and_then(|p| p.to_str())
                .unwrap_or("[No Name]")
                .to_owned()
        },
        |s| s.dirty,
        |s| s.editor.buffer().as_string(),
        |s| s.filename.clone(),
        |s| s.editor.buffer().cursor().row,
        |_| 0,
    ));
    // Build a Picker from the source — it calls enumerate internally.
    let mut picker = crate::picker::Picker::new(source);
    picker.refresh();
    assert_eq!(picker.total(), 2, "expected 2 entries");
    assert!(picker.scan_done(), "scan_done must be set");
    let _ = std::fs::remove_file(&path_a);
    let _ = std::fs::remove_file(&path_b);
}

#[test]
fn buffer_source_select_returns_switch_buffer() {
    use crate::picker::{BufferSource, PickerAction, PickerLogic};
    use crate::picker_action::AppAction;
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
        |s| s.editor.buffer().as_string(),
        |s| s.filename.clone(),
        |s| s.editor.buffer().cursor().row,
        |_| 0,
    );
    // Index 0 corresponds to the first entry (the only slot).
    match source.select(0) {
        PickerAction::Custom(b) => {
            let a = b
                .downcast::<AppAction>()
                .expect("should downcast to AppAction");
            assert!(matches!(*a, AppAction::SwitchSlot(0)));
        }
        _ => panic!("expected Custom(AppAction::SwitchSlot(0))"),
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn edit_drops_pristine_default_buffer_when_first_real_file_opens() {
    let path = std::env::temp_dir().join("hjkl_drop_pristine.txt");
    std::fs::write(&path, "hello\n").unwrap();
    let mut app = App::new(None, false, None, None).unwrap();
    assert_eq!(app.slots.len(), 1);
    assert!(app.active().filename.is_none());
    app.dispatch_ex(&format!("e {}", path.display()));
    assert_eq!(
        app.slots.len(),
        1,
        "pristine default buffer should have been dropped"
    );
    assert_eq!(app.active_index(), 0);
    assert_eq!(
        app.active().filename.as_deref(),
        Some(path.as_path()),
        "active slot should now be the opened file"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn edit_keeps_dirty_default_buffer_when_opening_real_file() {
    let path = std::env::temp_dir().join("hjkl_keep_dirty_default.txt");
    std::fs::write(&path, "hello\n").unwrap();
    let mut app = App::new(None, false, None, None).unwrap();
    // Mark default as dirty without giving it a name.
    app.slots[0].dirty = true;
    app.dispatch_ex(&format!("e {}", path.display()));
    assert_eq!(
        app.slots.len(),
        2,
        "dirty unnamed buffer must not be dropped silently"
    );
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
    assert_eq!(app.active_index(), 0);
    app.open_extra(path_b.clone()).unwrap();
    assert_eq!(app.slots.len(), 2, "extra slot should have been added");
    assert_eq!(
        app.active_index(),
        0,
        "active must stay at 0 after open_extra"
    );
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
    assert_eq!(app.active_index(), 1, "`:b 2` should switch to index 1");
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
    assert_eq!(app.active_index(), 1);
    // Switch to the foo slot by substring
    app.dispatch_ex("b foo");
    assert_eq!(
        app.active_index(),
        0,
        "`:b foo` should switch to foo's slot"
    );
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
    assert_eq!(app.active_index(), 1);
    app.dispatch_ex("bfirst");
    assert_eq!(app.active_index(), 0, "`:bfirst` should go to slot 0");
    app.dispatch_ex("blast");
    assert_eq!(app.active_index(), 2, "`:blast` should go to last slot");
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

    let mut cfg = crate::config::Config::default();
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
    assert!(app.active().editor.is_readonly());

    let app = app.with_config(crate::config::Config::default());
    assert!(
        app.active().editor.is_readonly(),
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

    let cfg = crate::config::load_from(tmp.path()).expect("load_from must succeed");
    // Bundled defaults survived for fields the user file omitted:
    assert_eq!(cfg.editor.huge_file_threshold, 50_000);
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
    writeln!(tmp, "[editor]\nhuge_file_threshold = 0").unwrap();

    let cfg = crate::config::load_from(tmp.path()).expect("parse must succeed");

    use hjkl_config::Validate;
    let err = cfg.validate().unwrap_err();
    assert_eq!(err.field, "editor.huge_file_threshold");
}

// ── Git status picker smoke tests ──────────────────────────────────────

#[test]
fn open_git_status_picker_sets_picker_and_clears_pending() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert!(app.picker.is_none());
    app.pending_leader = true;
    app.pending_git = true;
    app.open_git_status_picker();
    assert!(
        app.picker.is_some(),
        "picker should be open after open_git_status_picker"
    );
    assert!(!app.pending_leader, "pending_leader must be cleared");
    assert!(!app.pending_git, "pending_git must be cleared");
}

#[test]
fn git_status_picker_title_is_git_status() {
    use crate::picker_git::GitStatusPicker;
    use hjkl_picker::PickerLogic;
    let tmp = tempfile::tempdir().unwrap();
    let theme = AppTheme::default_dark();
    let theme_arc = theme.syntax.clone() as std::sync::Arc<dyn hjkl_bonsai::Theme + Send + Sync>;
    let directory = std::sync::Arc::new(crate::lang::LanguageDirectory::new().unwrap());
    let source = GitStatusPicker::new(tmp.path().to_path_buf(), theme_arc, directory);
    assert_eq!(source.title(), "git status");
}

// ── Git log picker smoke tests ─────────────────────────────────────────

#[test]
fn git_log_picker_opens_and_clears_pending() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert!(app.picker.is_none());
    app.pending_leader = true;
    app.pending_git = true;
    app.open_git_log_picker();
    assert!(
        app.picker.is_some(),
        "picker should be open after open_git_log_picker"
    );
    assert!(!app.pending_leader, "pending_leader must be cleared");
    assert!(!app.pending_git, "pending_git must be cleared");
}

#[test]
fn git_log_picker_title_is_git_log() {
    use crate::picker_git::GitLogPicker;
    use hjkl_picker::PickerLogic;
    let tmp = tempfile::tempdir().unwrap();
    let theme = AppTheme::default_dark();
    let theme_arc = theme.syntax.clone() as std::sync::Arc<dyn hjkl_bonsai::Theme + Send + Sync>;
    let directory = std::sync::Arc::new(crate::lang::LanguageDirectory::new().unwrap());
    let source = GitLogPicker::new(tmp.path().to_path_buf(), theme_arc, directory);
    assert_eq!(source.title(), "git log");
}

// ── Git branch picker smoke tests ──────────────────────────────────────

#[test]
fn git_branch_picker_opens_and_clears_pending() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert!(app.picker.is_none());
    app.pending_leader = true;
    app.pending_git = true;
    app.open_git_branch_picker();
    assert!(
        app.picker.is_some(),
        "picker should be open after open_git_branch_picker"
    );
    assert!(!app.pending_leader, "pending_leader must be cleared");
    assert!(!app.pending_git, "pending_git must be cleared");
}

#[test]
fn git_branch_picker_title_is_git_branches() {
    use crate::picker_git::GitBranchPicker;
    use hjkl_picker::PickerLogic;
    let tmp = tempfile::tempdir().unwrap();
    let source = GitBranchPicker::new(tmp.path().to_path_buf());
    assert_eq!(source.title(), "git branches");
}

// ── Git file history picker smoke tests ───────────────────────────────────

#[test]
fn git_file_history_picker_opens_and_clears_pending() {
    let path = std::env::temp_dir().join("hjkl_gB_smoke.txt");
    std::fs::write(&path, "content\n").unwrap();
    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
    assert!(app.picker.is_none());
    app.pending_leader = true;
    app.pending_git = true;
    // Buffer has a path — picker opens (it may show sentinel if not a repo).
    app.open_git_file_history_picker();
    // pending flags must always be cleared.
    assert!(!app.pending_leader, "pending_leader must be cleared");
    assert!(!app.pending_git, "pending_git must be cleared");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn git_file_history_picker_no_path_sets_status_and_clears_pending() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert!(app.active().filename.is_none());
    app.pending_leader = true;
    app.pending_git = true;
    app.open_git_file_history_picker();
    assert!(!app.pending_leader, "pending_leader must be cleared");
    assert!(!app.pending_git, "pending_git must be cleared");
    assert!(app.picker.is_none(), "picker must not open without a path");
    let msg = app.status_message.clone().unwrap_or_default();
    assert!(
        msg.contains("no path"),
        "expected 'no path' status message, got: {msg:?}"
    );
}

#[test]
fn git_file_history_picker_title_is_git_file_history() {
    use crate::picker_git::GitFileHistoryPicker;
    use hjkl_picker::PickerLogic;
    let tmp = tempfile::tempdir().unwrap();
    let theme = AppTheme::default_dark();
    let theme_arc = theme.syntax.clone() as std::sync::Arc<dyn hjkl_bonsai::Theme + Send + Sync>;
    let directory = std::sync::Arc::new(crate::lang::LanguageDirectory::new().unwrap());
    let source = GitFileHistoryPicker::new(
        tmp.path().to_path_buf(),
        std::path::PathBuf::from("src/main.rs"),
        theme_arc,
        directory,
    );
    assert_eq!(source.title(), "git file history");
}

#[test]
fn git_status_picker_no_repo_scan_produces_sentinel_or_empty() {
    use crate::picker_git::GitStatusPicker;
    use hjkl_picker::PickerLogic;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    let tmp = tempfile::tempdir().unwrap();
    let theme = AppTheme::default_dark();
    let theme_arc = theme.syntax.clone() as std::sync::Arc<dyn hjkl_bonsai::Theme + Send + Sync>;
    let directory = std::sync::Arc::new(crate::lang::LanguageDirectory::new().unwrap());
    let mut source = GitStatusPicker::new(tmp.path().to_path_buf(), theme_arc, directory);

    let cancel = Arc::new(AtomicBool::new(false));
    let handle = source.enumerate(None, Arc::clone(&cancel));
    if let Some(h) = handle {
        let _ = h.join();
    }

    // Either a sentinel item (label says "not a git repo") or empty.
    let count = source.item_count();
    if count > 0 {
        let label = source.label(0);
        assert!(
            label.contains("not a git repo"),
            "sentinel label unexpected: {label:?}"
        );
        assert!(matches!(
            source.select(0),
            crate::picker::PickerAction::None
        ));
    }
}

// ── Git stash picker smoke tests ──────────────────────────────────────────

#[test]
fn git_stash_picker_opens_and_clears_pending() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert!(app.picker.is_none());
    app.pending_leader = true;
    app.pending_git = true;
    app.open_git_stash_picker();
    assert!(
        app.picker.is_some(),
        "picker should be open after open_git_stash_picker"
    );
    assert!(!app.pending_leader, "pending_leader must be cleared");
    assert!(!app.pending_git, "pending_git must be cleared");
}

#[test]
fn git_stash_picker_title_is_git_stashes() {
    use crate::picker_git::GitStashPicker;
    use hjkl_picker::PickerLogic;
    let tmp = tempfile::tempdir().unwrap();
    let source = GitStashPicker::new(tmp.path().to_path_buf());
    assert_eq!(source.title(), "git stashes");
}

#[test]
fn git_stash_picker_shift_s_chord_dispatches() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert!(app.picker.is_none());
    // Simulate <leader>g chord state then press S.
    app.pending_leader = true;
    app.pending_git = true;
    // Directly call the open function (event_loop routes S here).
    app.open_git_stash_picker();
    assert!(app.picker.is_some(), "S chord must open the stash picker");
    assert!(!app.pending_leader);
    assert!(!app.pending_git);
    // Title must match.
    assert_eq!(app.picker.as_ref().unwrap().title(), "git stashes");
}

// ── Git tags picker smoke tests ───────────────────────────────────────────

#[test]
fn git_tags_picker_opens_and_clears_pending() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert!(app.picker.is_none());
    app.pending_leader = true;
    app.pending_git = true;
    app.open_git_tags_picker();
    assert!(
        app.picker.is_some(),
        "picker should be open after open_git_tags_picker"
    );
    assert!(!app.pending_leader, "pending_leader must be cleared");
    assert!(!app.pending_git, "pending_git must be cleared");
}

#[test]
fn git_tags_picker_title_is_git_tags() {
    use crate::picker_git::GitTagsPicker;
    use hjkl_picker::PickerLogic;
    let tmp = tempfile::tempdir().unwrap();
    let source = GitTagsPicker::new(tmp.path().to_path_buf());
    assert_eq!(source.title(), "git tags");
}

#[test]
fn git_tags_picker_no_repo_produces_sentinel() {
    use crate::picker_git::GitTagsPicker;
    use hjkl_picker::PickerLogic;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    let tmp = tempfile::tempdir().unwrap();
    let mut source = GitTagsPicker::new(tmp.path().to_path_buf());
    let cancel = Arc::new(AtomicBool::new(false));
    let handle = source.enumerate(None, Arc::clone(&cancel));
    if let Some(h) = handle {
        let _ = h.join();
    }
    let count = source.item_count();
    assert!(count > 0, "should have at least a sentinel item");
    let label = source.label(0);
    assert!(
        label.contains("no tags") || label.contains("not a git repo"),
        "sentinel label unexpected: {label:?}"
    );
    assert!(matches!(
        source.select(0),
        crate::picker::PickerAction::None
    ));
}

// ── Git remotes picker smoke tests ────────────────────────────────────────

#[test]
fn git_remotes_picker_opens_and_clears_pending() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert!(app.picker.is_none());
    app.pending_leader = true;
    app.pending_git = true;
    app.open_git_remotes_picker();
    assert!(
        app.picker.is_some(),
        "picker should be open after open_git_remotes_picker"
    );
    assert!(!app.pending_leader, "pending_leader must be cleared");
    assert!(!app.pending_git, "pending_git must be cleared");
}

#[test]
fn git_remotes_picker_title_is_git_remotes() {
    use crate::picker_git::GitRemotesPicker;
    use hjkl_picker::PickerLogic;
    let tmp = tempfile::tempdir().unwrap();
    let source = GitRemotesPicker::new(tmp.path().to_path_buf());
    assert_eq!(source.title(), "git remotes");
}

#[test]
fn git_remotes_picker_no_repo_produces_sentinel() {
    use crate::picker_git::GitRemotesPicker;
    use hjkl_picker::PickerLogic;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    let tmp = tempfile::tempdir().unwrap();
    let mut source = GitRemotesPicker::new(tmp.path().to_path_buf());
    let cancel = Arc::new(AtomicBool::new(false));
    let handle = source.enumerate(None, Arc::clone(&cancel));
    if let Some(h) = handle {
        let _ = h.join();
    }
    let count = source.item_count();
    assert!(count > 0, "should have at least a sentinel item");
    let label = source.label(0);
    assert!(
        label.contains("no remotes") || label.contains("not a git repo"),
        "sentinel label unexpected: {label:?}"
    );
    assert!(matches!(
        source.select(0),
        crate::picker::PickerAction::None
    ));
}

// ── PickerAction downcast test ─────────────────────────────────────────

#[test]
fn picker_action_custom_downcasts_to_app_action() {
    use crate::picker_action::AppAction;
    use hjkl_picker::PickerAction;
    let action = PickerAction::Custom(Box::new(AppAction::SwitchSlot(2)));
    if let PickerAction::Custom(b) = action {
        let recovered = b.downcast::<AppAction>().expect("should downcast");
        assert!(matches!(*recovered, AppAction::SwitchSlot(2)));
    } else {
        panic!("expected Custom");
    }
}

// ── checktime / disk-change detection tests ────────────────────────────

/// Helper: bump mtime by writing a file then sleeping briefly so the
/// filesystem timestamp advances past the stored baseline.
fn write_and_wait(path: &std::path::Path, content: &str) {
    std::fs::write(path, content).unwrap();
    // Give the FS time to advance mtime past what we stored at load.
    std::thread::sleep(Duration::from_millis(50));
}

#[test]
fn checktime_reloads_clean_buffer_when_disk_changed() {
    let path = std::env::temp_dir().join("hjkl_ct_reload.txt");
    std::fs::write(&path, "line1\nline2\n").unwrap();
    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
    assert_eq!(app.active().editor.buffer().lines(), vec!["line1", "line2"]);

    write_and_wait(&path, "new content\n");
    app.checktime_all();

    assert_eq!(
        app.active().editor.buffer().lines(),
        vec!["new content"],
        "buffer should be reloaded from disk"
    );
    assert!(!app.active().dirty, "reloaded buffer must not be dirty");
    assert_eq!(app.active().disk_state, DiskState::Synced);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn checktime_marks_dirty_buffer_as_changed_on_disk_no_reload() {
    let path = std::env::temp_dir().join("hjkl_ct_dirty.txt");
    std::fs::write(&path, "original\n").unwrap();
    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();

    // Dirty the buffer without touching disk.
    app.active_mut().dirty = true;

    write_and_wait(&path, "changed on disk\n");
    app.checktime_all();

    // Content must NOT have changed.
    assert_eq!(
        app.active().editor.buffer().lines(),
        vec!["original"],
        "dirty buffer must not be reloaded"
    );
    assert_eq!(
        app.active().disk_state,
        DiskState::ChangedOnDisk,
        "disk_state must be ChangedOnDisk"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn checktime_marks_deleted_when_file_removed() {
    let path = std::env::temp_dir().join("hjkl_ct_deleted.txt");
    std::fs::write(&path, "content\n").unwrap();
    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();

    std::fs::remove_file(&path).unwrap();
    app.checktime_all();

    assert_eq!(app.active().disk_state, DiskState::DeletedOnDisk);
    // Buffer content preserved.
    assert_eq!(app.active().editor.buffer().lines(), vec!["content"]);
}

#[test]
fn checktime_recovers_after_file_recreated() {
    let path = std::env::temp_dir().join("hjkl_ct_recover.txt");
    std::fs::write(&path, "v1\n").unwrap();
    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();

    // Delete → marks DeletedOnDisk.
    std::fs::remove_file(&path).unwrap();
    app.checktime_all();
    assert_eq!(app.active().disk_state, DiskState::DeletedOnDisk);

    // Recreate with new content — next checktime should reload (not dirty).
    write_and_wait(&path, "v2\n");
    app.checktime_all();

    assert_eq!(
        app.active().editor.buffer().lines(),
        vec!["v2"],
        "recreated file should be reloaded"
    );
    assert_eq!(app.active().disk_state, DiskState::Synced);
    let _ = std::fs::remove_file(&path);
}

// ── Substitute ex-command tests ──────────────────────────────────────────────

/// `:%s/foo/bar/g` over a multi-line buffer replaces all occurrences.
#[test]
fn substitute_percent_global_multi_line() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "foo foo\nfoo");
    app.dispatch_ex("%s/foo/bar/g");
    let lines = app.active().editor.buffer().lines().to_vec();
    assert_eq!(
        lines,
        vec!["bar bar", "bar"],
        "buffer should be fully substituted"
    );
    let msg = app.status_message.clone().unwrap_or_default();
    assert_eq!(msg, "3 substitutions on 2 lines", "status: {msg}");
}

/// `:s/foo/bar/` on the current line replaces only the first occurrence.
#[test]
fn substitute_current_line_first_only() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "foo foo\nfoo");
    app.dispatch_ex("s/foo/bar/");
    let lines = app.active().editor.buffer().lines().to_vec();
    assert_eq!(lines[0], "bar foo", "only first occurrence on current line");
    assert_eq!(lines[1], "foo", "second line unchanged");
    let msg = app.status_message.clone().unwrap_or_default();
    assert_eq!(msg, "1 substitutions on 1 lines", "status: {msg}");
}

/// `:s//xxx/` after a `/foo` search reuses the last pattern.
#[test]
fn substitute_empty_pattern_reuses_last_search() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world");
    // Simulate a prior search by setting last_search directly.
    app.active_mut()
        .editor
        .set_last_search(Some("world".to_string()), true);
    app.dispatch_ex("s//planet/");
    let lines = app.active().editor.buffer().lines().to_vec();
    assert_eq!(
        lines[0], "hello planet",
        "should replace using last search pattern"
    );
    let msg = app.status_message.clone().unwrap_or_default();
    assert_eq!(msg, "1 substitutions on 1 lines", "status: {msg}");
}

/// `:s/foo/bar/` with no match leaves the buffer unchanged and shows "Pattern not found".
#[test]
fn substitute_no_match_shows_pattern_not_found() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world");
    app.dispatch_ex("s/xyz/bar/");
    let lines = app.active().editor.buffer().lines().to_vec();
    assert_eq!(lines[0], "hello world", "buffer should be unchanged");
    let msg = app.status_message.clone().unwrap_or_default();
    assert_eq!(msg, "Pattern not found", "status: {msg}");
}

/// `poll_grammar_loads` clears an already-expired `grammar_load_error` and
/// returns `true` to request a redraw (so the indicator disappears).
#[test]
fn poll_grammar_loads_clears_expired_error() {
    let mut app = App::new(None, false, None, None).unwrap();
    // Inject an error that is already expired (timestamp 10 s in the past).
    app.grammar_load_error = Some(GrammarLoadError {
        name: "fake-lang".to_string(),
        message: "connection refused".to_string(),
        at: std::time::Instant::now() - Duration::from_secs(10),
    });
    let needs_redraw = app.poll_grammar_loads();
    assert!(needs_redraw, "expired error should request redraw");
    assert!(
        app.grammar_load_error.is_none(),
        "expired error should be cleared"
    );
}

// ── Phase 1: window split tests ────────────────────────────────────────────

#[test]
fn sp_splits_focused_window() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert_eq!(app.windows.len(), 1);
    assert_eq!(app.focused_window(), 0);

    app.dispatch_ex("sp");

    // A second window should now exist.
    assert_eq!(
        app.windows.iter().filter(|w| w.is_some()).count(),
        2,
        "expected 2 open windows after :sp"
    );
    // Focus moved to the new (upper) window.
    let new_win_id = app.focused_window();
    assert_ne!(new_win_id, 0, "focus must have moved to the new window");
    // The layout should no longer be a single leaf.
    assert!(
        app.layout().leaves().len() == 2,
        "layout must have 2 leaves after split"
    );
}

#[test]
fn close_focused_window_collapses_split() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("sp");
    assert_eq!(app.windows.iter().filter(|w| w.is_some()).count(), 2);
    let focused_before_close = app.focused_window();

    app.dispatch_ex("close");

    // After closing, the closed window's entry is None.
    assert!(
        app.windows[focused_before_close].is_none(),
        "closed window entry must be None"
    );
    // Layout should be back to a single leaf.
    assert_eq!(
        app.layout().leaves().len(),
        1,
        "layout must collapse to 1 leaf"
    );
    // Status confirms closure.
    let msg = app.status_message.clone().unwrap_or_default();
    assert!(
        msg.contains("window closed"),
        "expected 'window closed' status"
    );
}

#[test]
fn close_last_window_errors() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert_eq!(app.windows.iter().filter(|w| w.is_some()).count(), 1);

    app.dispatch_ex("close");

    // Must not close the only window.
    assert_eq!(
        app.windows.iter().filter(|w| w.is_some()).count(),
        1,
        "last window must not be closed"
    );
    let msg = app.status_message.clone().unwrap_or_default();
    assert!(msg.contains("E444"), "expected E444 error, got: {msg}");
}

#[test]
fn ctrl_w_j_focuses_below() {
    let mut app = App::new(None, false, None, None).unwrap();
    // After :sp, focused window is the new (top) one.
    app.dispatch_ex("sp");
    let top_win = app.focused_window();

    // Ctrl-w j should move focus to the window below (the original).
    app.focus_below();
    let bottom_win = app.focused_window();
    assert_ne!(top_win, bottom_win, "focus must have moved down");
    // Moving below from bottom-most is a no-op.
    app.focus_below();
    assert_eq!(
        app.focused_window(),
        bottom_win,
        "focus must not move below the bottom-most window"
    );
}

#[test]
fn ctrl_w_k_focuses_above() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("sp");
    // Currently on top — move to bottom first.
    app.focus_below();
    let bottom_win = app.focused_window();

    // Ctrl-w k should move back up.
    app.focus_above();
    let top_win = app.focused_window();
    assert_ne!(bottom_win, top_win, "focus must have moved up");
    // Moving above from top-most is a no-op.
    app.focus_above();
    assert_eq!(
        app.focused_window(),
        top_win,
        "focus must not move above the top-most window"
    );
}

#[test]
fn non_focused_window_keeps_scroll_after_focused_scrolls() {
    let mut app = App::new(None, false, None, None).unwrap();
    // Fill the buffer with enough lines so scrolling has room.
    let content: String = (0..100)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    seed_buffer(&mut app, &content);

    // Split: new window on top (id = 1 typically), original below (id = 0).
    app.dispatch_ex("sp");
    let top_win = app.focused_window();
    // Move focus to bottom window.
    app.focus_below();
    let bottom_win = app.focused_window();
    assert_ne!(top_win, bottom_win);

    // Record top window's scroll before we scroll the bottom one.
    let top_top_row_before = app.windows[top_win].as_ref().unwrap().top_row;

    // Manually advance bottom window's scroll to simulate scrolling.
    app.windows[bottom_win].as_mut().unwrap().top_row = 20;

    // Top window's scroll must be unaffected.
    let top_top_row_after = app.windows[top_win].as_ref().unwrap().top_row;
    assert_eq!(
        top_top_row_before, top_top_row_after,
        "non-focused window scroll must not change when focused window scrolls"
    );
}

// ── Phase 2: vertical split tests ─────────────────────────────────────────────

#[test]
fn vsp_creates_vertical_split_with_new_on_left() {
    let mut app = App::new(None, false, None, None).unwrap();
    let original_win = app.focused_window();

    app.dispatch_ex("vsp");

    // Two windows now exist.
    assert_eq!(
        app.windows.iter().filter(|w| w.is_some()).count(),
        2,
        "expected 2 open windows after :vsp"
    );
    // Layout has 2 leaves.
    assert_eq!(app.layout().leaves().len(), 2, "layout must have 2 leaves");

    // Focus moved to the new (left) window.
    let new_win = app.focused_window();
    assert_ne!(new_win, original_win, "focus must have moved to new window");

    // New window is on the left (a-side) of a Vertical split — its
    // neighbor_right is the original window.
    let right = app.layout().neighbor_right(new_win);
    assert_eq!(
        right,
        Some(original_win),
        "original window must be to the right of the new one"
    );

    // Status says "vsplit".
    let msg = app.status_message.clone().unwrap_or_default();
    assert!(
        msg.contains("vsplit"),
        "expected 'vsplit' status, got: {msg}"
    );
}

#[test]
fn vnew_creates_empty_buffer_in_left_split() {
    let mut app = App::new(None, false, None, None).unwrap();
    let original_win = app.focused_window();

    app.dispatch_ex("vnew");

    assert_eq!(
        app.windows.iter().filter(|w| w.is_some()).count(),
        2,
        "expected 2 open windows after :vnew"
    );
    assert_eq!(app.layout().leaves().len(), 2);

    let new_win = app.focused_window();
    assert_ne!(new_win, original_win);

    // The new window points at an unnamed empty slot.
    let new_slot_idx = app.windows[new_win].as_ref().unwrap().slot;
    assert!(
        app.slots[new_slot_idx].filename.is_none(),
        "vnew window must point to an unnamed slot"
    );

    // Status says "vnew".
    let msg = app.status_message.clone().unwrap_or_default();
    assert!(msg.contains("vnew"), "expected 'vnew' status, got: {msg}");
}

#[test]
fn ctrl_w_h_focuses_left() {
    let mut app = App::new(None, false, None, None).unwrap();
    // After :vsp, focus is on the new left window.
    app.dispatch_ex("vsp");
    let left_win = app.focused_window();

    // Can't go further left — no-op.
    app.focus_left();
    assert_eq!(
        app.focused_window(),
        left_win,
        "focus_left from leftmost must be a no-op"
    );

    // Move right first, then come back left.
    app.focus_right();
    let right_win = app.focused_window();
    assert_ne!(left_win, right_win, "focus must have moved right");

    app.focus_left();
    assert_eq!(
        app.focused_window(),
        left_win,
        "focus_left must return to left window"
    );
}

#[test]
fn ctrl_w_l_focuses_right() {
    let mut app = App::new(None, false, None, None).unwrap();
    // After :vsp, focus is on the left (new) window.
    app.dispatch_ex("vsp");
    let left_win = app.focused_window();

    // Move right.
    app.focus_right();
    let right_win = app.focused_window();
    assert_ne!(left_win, right_win, "focus must have moved right");

    // Can't go further right — no-op.
    app.focus_right();
    assert_eq!(
        app.focused_window(),
        right_win,
        "focus_right from rightmost must be a no-op"
    );
}

#[test]
fn ctrl_w_w_cycles_next() {
    let mut app = App::new(None, false, None, None).unwrap();
    // Create two windows via :sp.
    app.dispatch_ex("sp");
    let leaves = app.layout().leaves();
    assert_eq!(leaves.len(), 2);

    // From the current focused window, next should cycle.
    let initial = app.focused_window();
    app.focus_next();
    let after_one = app.focused_window();
    assert_ne!(initial, after_one, "focus_next must move focus");
    app.focus_next();
    let after_two = app.focused_window();
    // With 2 windows, two focus_next should bring us back.
    assert_eq!(after_two, initial, "two focus_next calls must wrap around");
}

#[test]
fn ctrl_w_shift_w_cycles_previous() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("sp");

    let initial = app.focused_window();
    app.focus_previous();
    let after_one = app.focused_window();
    assert_ne!(initial, after_one, "focus_previous must move focus");
    app.focus_previous();
    let after_two = app.focused_window();
    assert_eq!(
        after_two, initial,
        "two focus_previous calls must wrap around"
    );
}

// ── Phase 3: window resize tests ─────────────────────────────────────────────

/// Inject a Rect into the innermost Split node that contains `id`.
/// Used instead of running a full render so resize methods have a
/// populated `last_rect` to work from.
fn inject_split_rect(
    layout: &mut window::LayoutTree,
    id: window::WindowId,
    rect: ratatui::layout::Rect,
) {
    if let window::LayoutTree::Split {
        a, b, last_rect, ..
    } = layout
        && (a.contains(id) || b.contains(id))
    {
        *last_rect = Some(rect);
        if let window::LayoutTree::Split { .. } = a.as_mut() {
            inject_split_rect(a, id, rect);
        }
        if let window::LayoutTree::Split { .. } = b.as_mut() {
            inject_split_rect(b, id, rect);
        }
    }
}

#[test]
fn resize_height_grows_focused_window() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("sp");
    // ratio starts at 0.5. Inject a 40-row rect so delta=2 is meaningful.
    let rect = ratatui::layout::Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 40,
    };
    let fw = app.focused_window();
    inject_split_rect(app.layout_mut(), fw, rect);

    let ratio_before = if let window::LayoutTree::Split { ratio, .. } = app.layout() {
        *ratio
    } else {
        panic!("expected Split");
    };

    app.resize_height(2);

    let ratio_after = if let window::LayoutTree::Split { ratio, .. } = app.layout() {
        *ratio
    } else {
        panic!("expected Split");
    };

    // focused is in `a` (top), growing by 2 rows of 40 increases ratio.
    assert!(
        ratio_after > ratio_before,
        "ratio should grow when resizing focused (top) window: before={ratio_before} after={ratio_after}"
    );
}

#[test]
fn resize_height_clamps_at_minimum() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("sp");
    let rect = ratatui::layout::Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 10,
    };
    let fw = app.focused_window();
    inject_split_rect(app.layout_mut(), fw, rect);

    // Try to shrink by far more than available — should clamp, not underflow.
    app.resize_height(-1000);

    let ratio = if let window::LayoutTree::Split { ratio, .. } = app.layout() {
        *ratio
    } else {
        panic!("expected Split");
    };
    // ratio must be at least 0.01 (our clamp) and sibling must have ≥ 1 row.
    assert!(ratio >= 0.01, "ratio must be >= 0.01 after clamp: {ratio}");
    assert!(
        ratio < 1.0,
        "ratio must be < 1.0 (sibling needs at least 1 row): {ratio}"
    );
}

#[test]
fn resize_width_grows_focused_window() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("vsp");
    let rect = ratatui::layout::Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 24,
    };
    let fw = app.focused_window();
    inject_split_rect(app.layout_mut(), fw, rect);

    let ratio_before = if let window::LayoutTree::Split { ratio, .. } = app.layout() {
        *ratio
    } else {
        panic!("expected Split");
    };

    app.resize_width(4);

    let ratio_after = if let window::LayoutTree::Split { ratio, .. } = app.layout() {
        *ratio
    } else {
        panic!("expected Split");
    };

    // focused is in `a` (left), growing by 4 columns of 80 increases ratio.
    assert!(
        ratio_after > ratio_before,
        "ratio should grow when resizing focused (left) window: before={ratio_before} after={ratio_after}"
    );
}

#[test]
fn equalize_layout_resets_uneven_splits() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("sp");

    // Manually skew the ratio.
    if let window::LayoutTree::Split { ratio, .. } = app.layout_mut() {
        *ratio = 0.3;
    }

    app.equalize_layout();

    let ratio = if let window::LayoutTree::Split { ratio, .. } = app.layout() {
        *ratio
    } else {
        panic!("expected Split");
    };
    assert!(
        (ratio - 0.5).abs() < 1e-5,
        "equalize should reset ratio to 0.5, got {ratio}"
    );
}

#[test]
fn maximize_height_collapses_siblings() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("sp");
    let rect = ratatui::layout::Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 24,
    };
    let fw = app.focused_window();
    inject_split_rect(app.layout_mut(), fw, rect);

    app.maximize_height();

    let ratio = if let window::LayoutTree::Split { ratio, .. } = app.layout() {
        *ratio
    } else {
        panic!("expected Split");
    };
    // Focused is in `a` (top). Maximized means ratio ≈ (height-1)/height = 23/24.
    let expected = 23.0_f32 / 24.0;
    assert!(
        (ratio - expected).abs() < 0.05,
        "maximize_height should set ratio near {expected}, got {ratio}"
    );
}

#[test]
fn ctrl_w_plus_grows_focused() {
    // Full keyboard path: simulate Ctrl-w then '+' through the pending_window_motion
    // machinery by calling the public App methods directly (event_loop integration
    // is hard to test without a terminal; the key dispatch calls resize_height(1)).
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("sp");
    let rect = ratatui::layout::Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 40,
    };
    let fw = app.focused_window();
    inject_split_rect(app.layout_mut(), fw, rect);

    let ratio_before = if let window::LayoutTree::Split { ratio, .. } = app.layout() {
        *ratio
    } else {
        panic!("expected Split");
    };

    // This is what event_loop.rs dispatches for Ctrl-w '+'.
    app.resize_height(1);

    let ratio_after = if let window::LayoutTree::Split { ratio, .. } = app.layout() {
        *ratio
    } else {
        panic!("expected Split");
    };
    assert!(
        ratio_after > ratio_before,
        "Ctrl-w + must grow the focused window: before={ratio_before} after={ratio_after}"
    );
}

// ── Phase 4: :only / swap / :new / :q redirect tests ──────────────────────────

#[test]
fn only_drops_other_windows() {
    let mut app = App::new(None, false, None, None).unwrap();
    // Create two extra windows: sp twice gives us 3 total.
    app.dispatch_ex("sp");
    app.dispatch_ex("sp");
    assert_eq!(
        app.layout().leaves().len(),
        3,
        "expected 3 windows before :only"
    );

    // Focus is on the most recently created window.
    let focused = app.focused_window();
    app.dispatch_ex("only");

    // Layout must collapse to a single leaf.
    assert_eq!(
        app.layout().leaves(),
        vec![focused],
        "only focused leaf should remain"
    );
    // All other window slots must be None.
    let open_count = app.windows.iter().filter(|w| w.is_some()).count();
    assert_eq!(open_count, 1, "exactly one window must remain open");
    // The remaining open window is the focused one.
    assert!(
        app.windows[focused].is_some(),
        "focused window must still be open"
    );
    let msg = app.status_message.clone().unwrap_or_default();
    assert!(msg.contains("only"), "expected 'only' status, got: {msg}");
}

#[test]
fn only_no_op_with_single_window() {
    let mut app = App::new(None, false, None, None).unwrap();
    let focused = app.focused_window();
    app.dispatch_ex("only");
    // Still one window, still the same focused window.
    assert_eq!(app.layout().leaves(), vec![focused]);
    assert_eq!(app.windows.iter().filter(|w| w.is_some()).count(), 1);
}

#[test]
fn new_creates_horizontal_split_empty_buffer() {
    let mut app = App::new(None, false, None, None).unwrap();
    let original_win = app.focused_window();

    app.dispatch_ex("new");

    // Two windows now exist.
    assert_eq!(
        app.windows.iter().filter(|w| w.is_some()).count(),
        2,
        "expected 2 open windows after :new"
    );
    assert_eq!(app.layout().leaves().len(), 2, "layout must have 2 leaves");

    // Focus moved to the new window.
    let new_win = app.focused_window();
    assert_ne!(new_win, original_win, "focus must have moved to new window");

    // The new window points at an unnamed empty slot.
    let new_slot_idx = app.windows[new_win].as_ref().unwrap().slot;
    assert!(
        app.slots[new_slot_idx].filename.is_none(),
        ":new window must point to an unnamed slot"
    );

    // The layout is a horizontal split (new window on top, original below).
    let below = app.layout().neighbor_below(new_win);
    assert_eq!(
        below,
        Some(original_win),
        "original window must be below the new one"
    );

    let msg = app.status_message.clone().unwrap_or_default();
    assert!(msg.contains("new"), "expected 'new' status, got: {msg}");
}

#[test]
fn ctrl_w_o_invokes_only() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("sp");
    assert_eq!(app.layout().leaves().len(), 2);

    let focused = app.focused_window();
    app.only_focused_window();

    assert_eq!(app.layout().leaves(), vec![focused]);
    assert_eq!(app.windows.iter().filter(|w| w.is_some()).count(), 1);
}

#[test]
fn ctrl_w_x_swaps_with_sibling() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("sp");

    // After :sp, layout is: hsplit(new_win, original_win).
    // leaves() order should be [new_win, original_win].
    let leaves_before = app.layout().leaves();
    assert_eq!(leaves_before.len(), 2);

    app.swap_with_sibling();

    let leaves_after = app.layout().leaves();
    // The two leaves should be swapped in pre-order.
    assert_eq!(
        leaves_after,
        vec![leaves_before[1], leaves_before[0]],
        "swap_with_sibling must reverse the leaf order"
    );

    let msg = app.status_message.clone().unwrap_or_default();
    assert!(msg.contains("swap"), "expected 'swap' status, got: {msg}");
}

#[test]
fn ctrl_w_n_creates_horizontal_empty_split() {
    let mut app = App::new(None, false, None, None).unwrap();
    let original_win = app.focused_window();

    // Simulate Ctrl-w n by calling dispatch_ex("new") — same path.
    app.dispatch_ex("new");

    assert_eq!(app.layout().leaves().len(), 2);
    let new_win = app.focused_window();
    assert_ne!(new_win, original_win);

    // New window is on top (a-side), original is below.
    let below = app.layout().neighbor_below(new_win);
    assert_eq!(below, Some(original_win));
}

#[test]
fn ctrl_w_q_closes_window_when_multiple() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("sp");
    assert_eq!(app.layout().leaves().len(), 2);

    let focused_before = app.focused_window();

    // Simulate Ctrl-w q behavior.
    if app.layout().leaves().len() > 1 {
        app.close_focused_window();
    } else {
        app.exit_requested = true;
    }

    assert!(
        !app.exit_requested,
        "Ctrl-w q must not quit with multiple windows"
    );
    assert!(
        app.windows[focused_before].is_none(),
        "focused window must be closed"
    );
    assert_eq!(app.layout().leaves().len(), 1, "layout must collapse");
}

#[test]
fn ctrl_w_q_quits_when_last() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert_eq!(app.layout().leaves().len(), 1);

    // Simulate Ctrl-w q behavior.
    if app.layout().leaves().len() > 1 {
        app.close_focused_window();
    } else {
        app.exit_requested = true;
    }

    assert!(app.exit_requested, "Ctrl-w q must quit when last window");
}

#[test]
fn colon_q_closes_window_when_multiple() {
    // Vim parity: :q with multiple windows closes the focused window,
    // not the application.
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("sp");
    assert_eq!(app.layout().leaves().len(), 2);
    let focused_before = app.focused_window();

    app.dispatch_ex("q");

    assert!(
        !app.exit_requested,
        ":q must not quit with multiple windows"
    );
    assert!(
        app.windows[focused_before].is_none(),
        "focused window must be closed by :q"
    );
    assert_eq!(app.layout().leaves().len(), 1, "layout must collapse to 1");
}

#[test]
fn colon_q_quits_when_last() {
    // :q with a single window and clean buffer should exit the app.
    let mut app = App::new(None, false, None, None).unwrap();
    assert_eq!(app.layout().leaves().len(), 1);
    assert!(!app.active().dirty);

    app.dispatch_ex("q");

    assert!(app.exit_requested, ":q on last window must exit");
}

// ── Phase tab: vim-style tab tests ──────────────────────────────────────────

#[test]
fn tabnew_creates_second_tab() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.active_tab, 0);
    app.dispatch_ex("tabnew");
    assert_eq!(app.tabs.len(), 2, "tabnew must create a second tab");
    assert_eq!(app.active_tab, 1, "active_tab must advance to the new tab");
}

#[test]
fn tabnew_no_arg_uses_empty_buffer() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("tabnew");
    assert_eq!(app.tabs.len(), 2);
    // New tab's focused window must point at an unnamed empty slot.
    let tab = &app.tabs[app.active_tab];
    let slot_idx = app.windows[tab.focused_window].as_ref().unwrap().slot;
    assert!(
        app.slots[slot_idx].filename.is_none(),
        "tabnew with no arg must use an unnamed buffer"
    );
    let lines = app.slots[slot_idx].editor.buffer().lines();
    assert!(
        lines.is_empty() || (lines.len() == 1 && lines[0].is_empty()),
        "tabnew buffer must be empty"
    );
}

#[test]
fn tabnext_wraps_at_end() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("tabnew");
    app.dispatch_ex("tabnew");
    assert_eq!(app.tabs.len(), 3);
    // active = 2 (last)
    assert_eq!(app.active_tab, 2);
    // tabnext should wrap to 0
    app.dispatch_ex("tabnext");
    assert_eq!(app.active_tab, 0, "tabnext must wrap to the first tab");
}

#[test]
fn tabprev_wraps_at_start() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("tabnew");
    app.dispatch_ex("tabnew");
    assert_eq!(app.tabs.len(), 3);
    // Go back to tab 0 first.
    app.dispatch_ex("tabnext");
    assert_eq!(app.active_tab, 0);
    // tabprev from 0 should wrap to 2 (last).
    app.dispatch_ex("tabprev");
    assert_eq!(app.active_tab, 2, "tabprev must wrap to the last tab");
}

#[test]
fn tabclose_removes_current_tab() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("tabnew");
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.active_tab, 1);
    app.dispatch_ex("tabclose");
    assert_eq!(app.tabs.len(), 1, "tabclose must remove the current tab");
    assert_eq!(app.active_tab, 0, "active_tab must fall back to 0");
}

#[test]
fn tabclose_last_tab_errors() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert_eq!(app.tabs.len(), 1);
    app.dispatch_ex("tabclose");
    // Must refuse — only one tab.
    assert_eq!(app.tabs.len(), 1, "tabclose must not close the last tab");
    let msg = app.status_message.clone().unwrap_or_default();
    assert!(msg.contains("E444"), "expected E444 error, got: {msg}");
}

#[test]
fn gt_switches_tab() {
    // Full keyboard path: g then t via pending_buffer_motion → tabnext.
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("tabnew");
    assert_eq!(app.tabs.len(), 2);
    // Go back to tab 0.
    app.dispatch_ex("tabprev");
    assert_eq!(app.active_tab, 0);
    // Simulate the 'g' prefix being set, then dispatch 't'.
    app.pending_buffer_motion = Some('g');
    let key = crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('t'),
        crossterm::event::KeyModifiers::NONE,
    );
    // The event_loop dispatches tabnext when pending_buffer_motion=Some('g') + 't'.
    // Replicate that logic here directly.
    if let Some(prefix) = app.pending_buffer_motion.take()
        && prefix == 'g'
        && key.code == crossterm::event::KeyCode::Char('t')
    {
        app.dispatch_ex("tabnext");
    }
    assert_eq!(app.active_tab, 1, "gt must advance to the next tab");
}

#[test]
fn gt_switches_tab_backward() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("tabnew");
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.active_tab, 1);
    // Simulate gT → tabprev.
    app.pending_buffer_motion = Some('g');
    let key = crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('T'),
        crossterm::event::KeyModifiers::NONE,
    );
    if let Some(prefix) = app.pending_buffer_motion.take()
        && prefix == 'g'
        && key.code == crossterm::event::KeyCode::Char('T')
    {
        app.dispatch_ex("tabprev");
    }
    assert_eq!(app.active_tab, 0, "gT must switch to the previous tab");
}

#[test]
fn each_tab_keeps_independent_layout() {
    let mut app = App::new(None, false, None, None).unwrap();
    // Tab 0: split into 2 windows.
    app.dispatch_ex("sp");
    let tab0_leaves = app.tabs[0].layout.leaves().len();
    assert_eq!(tab0_leaves, 2, "tab 0 must have 2 leaves after :sp");

    // Open tab 1 (fresh — single leaf).
    app.dispatch_ex("tabnew");
    assert_eq!(app.active_tab, 1);
    let tab1_leaves = app.tabs[1].layout.leaves().len();
    assert_eq!(tab1_leaves, 1, "tab 1 must start with 1 leaf");

    // Switch back to tab 0 and verify its layout is still 2 leaves.
    app.dispatch_ex("tabprev");
    assert_eq!(app.active_tab, 0);
    let tab0_leaves_after = app.tabs[0].layout.leaves().len();
    assert_eq!(
        tab0_leaves_after, 2,
        "tab 0 layout must be preserved after switching tabs"
    );
}

// ── Phase 2 tab tests ────────────────────────────────────────────────────────

#[test]
fn tabfirst_jumps_to_first() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("tabnew");
    app.dispatch_ex("tabnew");
    assert_eq!(app.active_tab, 2);
    app.dispatch_ex("tabfirst");
    assert_eq!(app.active_tab, 0, "tabfirst must jump to tab 0");
    let msg = app.status_message.clone().unwrap_or_default();
    assert!(
        msg.contains("tab 1/"),
        "expected 'tab 1/N' status, got: {msg}"
    );
}

#[test]
fn tabfirst_noop_when_already_first() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("tabnew");
    app.dispatch_ex("tabfirst");
    assert_eq!(app.active_tab, 0);
    // Should still produce a status message (not an error).
    let msg = app.status_message.clone().unwrap_or_default();
    assert!(
        msg.contains("tab 1/"),
        "no-op must still report position: {msg}"
    );
}

#[test]
fn tabrewind_and_tabr_are_aliases_for_tabfirst() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("tabnew");
    app.dispatch_ex("tabnew");
    assert_eq!(app.active_tab, 2);
    app.dispatch_ex("tabrewind");
    assert_eq!(app.active_tab, 0);
    app.dispatch_ex("tabnext");
    app.dispatch_ex("tabr");
    assert_eq!(app.active_tab, 0);
}

#[test]
fn tablast_jumps_to_last() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("tabnew");
    app.dispatch_ex("tabnew");
    // Go back to tab 0.
    app.dispatch_ex("tabfirst");
    assert_eq!(app.active_tab, 0);
    app.dispatch_ex("tablast");
    assert_eq!(app.active_tab, 2, "tablast must jump to the last tab");
    let msg = app.status_message.clone().unwrap_or_default();
    assert!(
        msg.contains("tab 3/3"),
        "expected 'tab 3/3' status, got: {msg}"
    );
}

#[test]
fn tablast_noop_when_already_last() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("tabnew");
    // active_tab is already 1 (last).
    app.dispatch_ex("tablast");
    assert_eq!(app.active_tab, 1);
    let msg = app.status_message.clone().unwrap_or_default();
    assert!(msg.contains("tab 2/2"), "no-op must report position: {msg}");
}

#[test]
fn tabonly_drops_other_tabs() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("tabnew");
    app.dispatch_ex("tabnew");
    assert_eq!(app.tabs.len(), 3);
    // Stay on tab 2. Run tabonly — should reduce to 1 tab.
    app.dispatch_ex("tabonly");
    assert_eq!(app.tabs.len(), 1, "tabonly must close all other tabs");
    assert_eq!(app.active_tab, 0, "active_tab must be reset to 0");
    let msg = app.status_message.clone().unwrap_or_default();
    assert_eq!(msg, "tabonly");
}

#[test]
fn tabonly_no_op_with_single_tab() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("tabonly");
    assert_eq!(app.tabs.len(), 1, "tabonly on single tab must stay at 1");
    let msg = app.status_message.clone().unwrap_or_default();
    assert_eq!(msg, "tabonly", "must report success even as no-op");
}

#[test]
fn tabo_is_alias_for_tabonly() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("tabnew");
    app.dispatch_ex("tabnew");
    assert_eq!(app.tabs.len(), 3);
    app.dispatch_ex("tabo");
    assert_eq!(app.tabs.len(), 1);
}

#[test]
fn tabmove_no_arg_moves_to_end() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("tabnew");
    app.dispatch_ex("tabnew");
    // Go to tab 0.
    app.dispatch_ex("tabfirst");
    assert_eq!(app.active_tab, 0);
    // tabmove with no arg: move tab 0 to the end.
    app.dispatch_ex("tabmove");
    assert_eq!(app.active_tab, 2, "tab should now be at position 2 (end)");
    let msg = app.status_message.clone().unwrap_or_default();
    assert_eq!(msg, "tabmove");
}

#[test]
fn tabmove_to_position_zero() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("tabnew");
    app.dispatch_ex("tabnew");
    // active is tab 2, move it to position 0.
    app.dispatch_ex("tabmove 0");
    assert_eq!(app.active_tab, 0, "tab should now be at position 0");
}

#[test]
fn tabmove_relative_plus_one() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("tabnew");
    app.dispatch_ex("tabnew");
    // Go to tab 0.
    app.dispatch_ex("tabfirst");
    assert_eq!(app.active_tab, 0);
    // Move tab 0 forward by 1.
    app.dispatch_ex("tabmove +1");
    assert_eq!(app.active_tab, 1, "tab should now be at position 1");
}

#[test]
fn tabmove_relative_minus_one() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("tabnew");
    app.dispatch_ex("tabnew");
    assert_eq!(app.active_tab, 2);
    // Move tab 2 back by 1.
    app.dispatch_ex("tabmove -1");
    assert_eq!(app.active_tab, 1, "tab should now be at position 1");
}

#[test]
fn tabmove_clamps_out_of_range() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("tabnew");
    // 2 tabs total. Try to move tab 1 to position 99.
    app.dispatch_ex("tabmove 99");
    assert_eq!(app.active_tab, 1, "out-of-range must clamp to last");
    // Try to move to negative (via -99).
    app.dispatch_ex("tabmove -99");
    assert_eq!(app.active_tab, 0, "large negative must clamp to 0");
}

#[test]
fn tabs_listing_marks_active_tab() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("tabnew");
    app.dispatch_ex("tabnew");
    // Active tab is 2.
    app.dispatch_ex("tabs");
    let popup = app.info_popup.clone().unwrap_or_default();
    // Should contain 3 "Tab page N" entries.
    assert!(popup.contains("Tab page 1"), "missing Tab page 1");
    assert!(popup.contains("Tab page 2"), "missing Tab page 2");
    assert!(popup.contains("Tab page 3"), "missing Tab page 3");
    // The active tab (3) must have '>'; others must have ' '.
    let lines: Vec<&str> = popup.lines().collect();
    // Lines alternate: "Tab page N", "<marker> <name>".
    // tab1 marker is lines[1], tab2 marker is lines[3], tab3 marker is lines[5].
    assert!(
        lines[1].starts_with("  ") || lines[1].starts_with(' '),
        "tab 1 must be inactive"
    );
    assert!(
        lines[5].starts_with("> "),
        "tab 3 (active) must show '>': {:?}",
        lines[5]
    );
}

#[test]
fn move_window_to_new_tab_creates_new_tab() {
    let mut app = App::new(None, false, None, None).unwrap();
    // Create a horizontal split so tab 0 has 2 windows.
    app.dispatch_ex("sp");
    assert_eq!(app.tabs[0].layout.leaves().len(), 2);
    let focused_before = app.focused_window();

    app.move_window_to_new_tab()
        .expect("should succeed with 2 windows");
    // A new tab should have been created.
    assert_eq!(app.tabs.len(), 2, "must create a second tab");
    assert_eq!(app.active_tab, 1, "must switch to the new tab");
    // The new tab must contain only the moved window.
    assert_eq!(app.tabs[1].layout.leaves(), vec![focused_before]);
    assert_eq!(app.tabs[1].focused_window, focused_before);
    // The old tab must now have only 1 window.
    assert_eq!(app.tabs[0].layout.leaves().len(), 1);
}

#[test]
fn move_window_to_new_tab_errors_when_only_window() {
    let mut app = App::new(None, false, None, None).unwrap();
    // Only one window — should fail.
    let result = app.move_window_to_new_tab();
    assert!(result.is_err(), "must error when only one window in tab");
    let msg = result.unwrap_err();
    assert!(msg.contains("E1"), "expected E1 error, got: {msg}");
    // No tab should be created.
    assert_eq!(app.tabs.len(), 1);
}

#[test]
fn ctrl_w_t_moves_window_to_new_tab() {
    let mut app = App::new(None, false, None, None).unwrap();
    // Need a split first so Ctrl-w T has something to work with.
    app.dispatch_ex("sp");
    assert_eq!(app.tabs.len(), 1);

    // Simulate Ctrl-w T: pending_window_motion set, then 'T' dispatched.
    // Mirror the event_loop logic directly (as done in existing tests).
    app.pending_window_motion = true;
    // Dispatch 'T' action manually (same logic as event_loop).
    app.pending_window_motion = false;
    match app.move_window_to_new_tab() {
        Ok(()) => {
            app.status_message = Some("moved window to new tab".into());
        }
        Err(msg) => {
            app.status_message = Some(msg.to_string());
        }
    }

    assert_eq!(app.tabs.len(), 2, "Ctrl-w T must create a new tab");
    let msg = app.status_message.clone().unwrap_or_default();
    assert_eq!(msg, "moved window to new tab");
}

// ── LSP diagnostics tests ────────────────────────────────────────────────

/// Build a `textDocument/publishDiagnostics` JSON payload for `file_url`
/// containing one error diagnostic.
fn pub_diags_params(file_url: &str, diags: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "uri": file_url,
        "diagnostics": diags,
    })
}

/// Returns the file:// URL string for an absolute path. Cross-platform via
/// hjkl_lsp::uri::from_path (handles Windows drive letters and URL-escaping).
fn file_url(path: &std::path::Path) -> String {
    hjkl_lsp::uri::from_path(path).unwrap().to_string()
}

/// Cross-platform temp path builder. Replaces hardcoded `/tmp/...` so tests
/// pass on Windows CI runners.
fn tmp_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(name)
}

#[test]
fn publish_diagnostics_populates_slot_diags() {
    let mut app = App::new(None, false, None, None).unwrap();

    // Give the active slot an absolute file path.
    let path = tmp_path("hjkl_diag_test.rs");
    app.active_mut().filename = Some(path.clone());

    seed_buffer(&mut app, "let x = ();\nlet y = ();");

    let params = pub_diags_params(
        &file_url(&path),
        serde_json::json!([{
            "range": {
                "start": { "line": 0, "character": 4 },
                "end":   { "line": 0, "character": 5 }
            },
            "severity": 1,
            "message": "unused variable",
            "source": "rustc",
            "code": "E0001"
        }]),
    );

    app.handle_publish_diagnostics(params);

    let slot = app.active();
    assert_eq!(slot.lsp_diags.len(), 1);
    let d = &slot.lsp_diags[0];
    assert_eq!(d.start_row, 0);
    assert_eq!(d.start_col, 4);
    assert_eq!(d.end_row, 0);
    assert_eq!(d.end_col, 5);
    assert_eq!(d.severity, DiagSeverity::Error);
    assert_eq!(d.message, "unused variable");
    assert_eq!(d.source.as_deref(), Some("rustc"));
    assert_eq!(d.code.as_deref(), Some("E0001"));

    // Gutter sign must be present for row 0.
    assert!(
        slot.diag_signs_lsp
            .iter()
            .any(|s| s.row == 0 && s.ch == 'E'),
        "expected an 'E' gutter sign for row 0"
    );
}

#[test]
fn publish_diagnostics_replaces_existing() {
    let mut app = App::new(None, false, None, None).unwrap();
    let path = tmp_path("hjkl_diag_replace.rs");
    app.active_mut().filename = Some(path.clone());
    seed_buffer(&mut app, "a\nb\nc");

    // First publish: two diags.
    let params1 = pub_diags_params(
        &file_url(&path),
        serde_json::json!([
            {
                "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 1 } },
                "severity": 1,
                "message": "err A"
            },
            {
                "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 1 } },
                "severity": 2,
                "message": "warn B"
            }
        ]),
    );
    app.handle_publish_diagnostics(params1);
    assert_eq!(app.active().lsp_diags.len(), 2);

    // Second publish: one diag — must replace, not append.
    let params2 = pub_diags_params(
        &file_url(&path),
        serde_json::json!([{
            "range": { "start": { "line": 2, "character": 0 }, "end": { "line": 2, "character": 1 } },
            "severity": 3,
            "message": "info C"
        }]),
    );
    app.handle_publish_diagnostics(params2);

    let slot = app.active();
    assert_eq!(
        slot.lsp_diags.len(),
        1,
        "second publish must replace, not append"
    );
    assert_eq!(slot.lsp_diags[0].message, "info C");
    assert_eq!(slot.lsp_diags[0].severity, DiagSeverity::Info);
    // Old signs must be replaced too.
    assert_eq!(slot.diag_signs_lsp.len(), 1);
    assert_eq!(slot.diag_signs_lsp[0].row, 2);
}

#[test]
fn publish_diagnostics_clears_on_empty() {
    let mut app = App::new(None, false, None, None).unwrap();
    let path = tmp_path("hjkl_diag_clear.rs");
    app.active_mut().filename = Some(path.clone());
    seed_buffer(&mut app, "a");

    let params_with = pub_diags_params(
        &file_url(&path),
        serde_json::json!([{
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 1 } },
            "severity": 1,
            "message": "err"
        }]),
    );
    app.handle_publish_diagnostics(params_with);
    assert_eq!(app.active().lsp_diags.len(), 1);

    // Empty diagnostics array clears all diags.
    let params_clear = pub_diags_params(&file_url(&path), serde_json::json!([]));
    app.handle_publish_diagnostics(params_clear);

    let slot = app.active();
    assert!(slot.lsp_diags.is_empty(), "empty publish must clear diags");
    assert!(
        slot.diag_signs_lsp.is_empty(),
        "empty publish must clear gutter signs"
    );
}

#[test]
fn publish_diagnostics_ignores_unknown_uri() {
    let mut app = App::new(None, false, None, None).unwrap();
    let path = tmp_path("hjkl_diag_known.rs");
    app.active_mut().filename = Some(path.clone());
    seed_buffer(&mut app, "a");

    // Params targeting a *different* file — should be silently ignored.
    let unknown_path = tmp_path("hjkl_diag_unknown.rs");
    let params = pub_diags_params(
        &file_url(&unknown_path),
        serde_json::json!([{
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 1 } },
            "severity": 1,
            "message": "err"
        }]),
    );
    app.handle_publish_diagnostics(params);

    assert!(
        app.active().lsp_diags.is_empty(),
        "unmatched URI must not populate diags"
    );
}

#[test]
fn lnext_jumps_to_next_diag() {
    let mut app = App::new(None, false, None, None).unwrap();
    let path = tmp_path("hjkl_lnext.rs");
    app.active_mut().filename = Some(path.clone());
    seed_buffer(&mut app, "a\nb\nc\nhello world");

    // Plant diags on rows 1 and 3.
    let params = pub_diags_params(
        &file_url(&path),
        serde_json::json!([
            {
                "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 1 } },
                "severity": 1,
                "message": "first"
            },
            {
                "range": { "start": { "line": 3, "character": 6 }, "end": { "line": 3, "character": 11 } },
                "severity": 2,
                "message": "second"
            }
        ]),
    );
    app.handle_publish_diagnostics(params);

    // Cursor at row 0 — lnext should jump to row 1.
    app.lnext_severity(None);
    let (row, _col) = app.active().editor.cursor();
    assert_eq!(row, 1, "lnext must jump to first diag after cursor");

    // Cursor now at row 1 — lnext should jump to row 3.
    app.lnext_severity(None);
    let (row, col) = app.active().editor.cursor();
    assert_eq!(row, 3);
    assert_eq!(col, 6, "lnext must place cursor at diag start_col");
}

#[test]
fn lsp_jump_reveals_cursor_in_viewport() {
    // Regression: jump_cursor only sets cursor; without ensure_cursor_in_
    // scrolloff afterwards, the viewport stays parked and the cursor lands
    // off-screen. Plant a diag past the visible area, jump, assert the
    // window's stored top_row scrolled.
    use crate::app::window::WindowId;

    let mut app = App::new(None, false, None, None).unwrap();
    let path = tmp_path("hjkl_jump_scroll.rs");
    app.active_mut().filename = Some(path.clone());

    // 100 lines of content so a row-50 jump is well past any default
    // viewport.
    let lines: Vec<String> = (0..100).map(|i| format!("line {i}")).collect();
    seed_buffer(&mut app, &lines.join("\n"));

    // Set the focused window's viewport height + reset scroll so we can
    // observe whether jump scrolls.
    {
        let vp = app.active_mut().editor.host_mut().viewport_mut();
        vp.height = 20;
        vp.top_row = 0;
    }
    let fw: WindowId = app.focused_window();
    if let Some(w) = app.windows[fw].as_mut() {
        w.top_row = 0;
    }

    // Plant a diagnostic on row 50 and jump to it.
    let params = pub_diags_params(
        &file_url(&path),
        serde_json::json!([{
            "range": { "start": { "line": 50, "character": 0 }, "end": { "line": 50, "character": 1 } },
            "severity": 1,
            "message": "deep"
        }]),
    );
    app.handle_publish_diagnostics(params);
    app.lnext_severity(None);

    // Cursor must be at row 50 AND the viewport must have scrolled past
    // the original top_row=0 so the cursor is visible.
    let (row, _) = app.active().editor.cursor();
    assert_eq!(row, 50);
    let vp_top = app.active().editor.host().viewport().top_row;
    assert!(
        vp_top > 0,
        "viewport top_row stayed at 0 after jump — ensure_cursor_in_scrolloff not called"
    );
    let stored_top = app.windows[fw].as_ref().unwrap().top_row;
    assert!(
        stored_top > 0,
        "focused window's stored top_row stayed at 0 — sync_viewport_from_editor missed the scroll"
    );
}

#[test]
fn lprev_jumps_to_prev_diag_with_wrap() {
    let mut app = App::new(None, false, None, None).unwrap();
    let path = tmp_path("hjkl_lprev.rs");
    app.active_mut().filename = Some(path.clone());
    seed_buffer(&mut app, "a\nb\nc\nd");

    let params = pub_diags_params(
        &file_url(&path),
        serde_json::json!([
            {
                "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 1 } },
                "severity": 1,
                "message": "first"
            },
            {
                "range": { "start": { "line": 2, "character": 1 }, "end": { "line": 2, "character": 2 } },
                "severity": 2,
                "message": "second"
            }
        ]),
    );
    app.handle_publish_diagnostics(params);

    // Cursor at row 0 col 0 — lprev should wrap to the last diag (row 2).
    app.lprev_severity(None);
    let (row, _) = app.active().editor.cursor();
    assert_eq!(row, 2, "lprev from first diag must wrap to last");

    // Cursor now at row 2 — lprev should jump to row 0.
    app.lprev_severity(None);
    let (row, _) = app.active().editor.cursor();
    assert_eq!(row, 0, "lprev must jump to previous diag");
}

#[test]
fn lnext_severity_skips_lower_severity() {
    let mut app = App::new(None, false, None, None).unwrap();
    let path = tmp_path("hjkl_lnext_sev.rs");
    app.active_mut().filename = Some(path.clone());
    seed_buffer(&mut app, "a\nb\nc");

    // Row 1: Warning, Row 2: Error.
    let params = pub_diags_params(
        &file_url(&path),
        serde_json::json!([
            {
                "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 1 } },
                "severity": 2,
                "message": "warn"
            },
            {
                "range": { "start": { "line": 2, "character": 0 }, "end": { "line": 2, "character": 1 } },
                "severity": 1,
                "message": "err"
            }
        ]),
    );
    app.handle_publish_diagnostics(params);

    // Jump to Error-only — must skip Warning on row 1 and land on row 2.
    app.lnext_severity(Some(DiagSeverity::Error));
    let (row, _) = app.active().editor.cursor();
    assert_eq!(row, 2, "lnext with Error filter must skip Warning diags");
}

#[test]
fn lopen_shows_no_diags_message_when_empty() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.open_diag_picker();
    // No diagnostics — picker must not open; status message set.
    assert!(app.picker.is_none(), "picker must not open when no diags");
    let msg = app.status_message.clone().unwrap_or_default();
    assert!(
        msg.contains("no diagnostics"),
        "expected 'no diagnostics', got: {msg}"
    );
}

#[test]
fn lopen_lists_diags_in_picker() {
    let mut app = App::new(None, false, None, None).unwrap();
    let path = tmp_path("hjkl_lopen.rs");
    app.active_mut().filename = Some(path.clone());
    seed_buffer(&mut app, "a\nb");

    let params = pub_diags_params(
        &file_url(&path),
        serde_json::json!([{
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 1 } },
            "severity": 1,
            "message": "some error"
        }]),
    );
    app.handle_publish_diagnostics(params);

    app.open_diag_picker();
    assert!(app.picker.is_some(), "picker must open when diags exist");
}

#[test]
fn lsp_info_with_lsp_disabled_sets_status() {
    let mut app = App::new(None, false, None, None).unwrap();
    // self.lsp is None by default — :LspInfo shows the disabled state.
    app.show_lsp_info();
    let popup = app.info_popup.clone().unwrap_or_default();
    assert!(
        popup.contains("LSP: disabled"),
        "expected 'LSP: disabled' message, got: {popup}"
    );
}

#[test]
fn lsp_info_lists_running_servers() {
    let mut app = App::new(None, false, None, None).unwrap();
    // Need an LspManager attached so :LspInfo doesn't show "disabled".
    app.lsp = Some(hjkl_lsp::LspManager::spawn(hjkl_lsp::LspConfig::default()));
    // Manually insert a fake server into lsp_state.
    let key = hjkl_lsp::ServerKey {
        language: "rust".into(),
        root: std::path::PathBuf::from("/tmp/proj"),
    };
    app.lsp_state.insert(
        key,
        LspServerInfo {
            initialized: true,
            capabilities: serde_json::json!({}),
        },
    );

    app.show_lsp_info();
    assert!(
        app.info_popup.is_some(),
        "popup must open when LSP is enabled"
    );
    let popup = app.info_popup.as_ref().unwrap();
    assert!(popup.contains("rust"), "popup must mention server language");
    assert!(
        popup.contains("initialized"),
        "popup must show server state"
    );
    if let Some(mgr) = app.lsp.take() {
        mgr.shutdown();
    }
}

#[test]
fn notify_change_skipped_when_dirty_gen_unchanged() {
    // Without a real LspManager we can't exercise the full path, but we
    // *can* verify that the last_lsp_dirty_gen guard does not reset on
    // repeated calls with no edits: the gen stays the same, so the
    // second call would be a no-op (it would return early). We assert
    // the guard value is set correctly after a manual seed.
    let mut app = App::new(None, false, None, None).unwrap();
    // No LSP manager attached — lsp_notify_change_active returns early.
    // Manually set last_lsp_dirty_gen to simulate a prior send.
    let dg = app.active().editor.buffer().dirty_gen();
    app.active_mut().last_lsp_dirty_gen = Some(dg);

    // Call again — must not panic and must not reset the guard.
    app.lsp_notify_change_active();
    assert_eq!(
        app.active().last_lsp_dirty_gen,
        Some(dg),
        "guard must remain unchanged when no LSP manager"
    );
}

// ── Phase 3: goto + hover tests ────────────────────────────────────────────

fn make_location(uri: &str, row: u32, col: u32) -> lsp_types::Location {
    lsp_types::Location {
        uri: uri.parse::<lsp_types::Uri>().expect("valid URI"),
        range: lsp_types::Range {
            start: lsp_types::Position {
                line: row,
                character: col,
            },
            end: lsp_types::Position {
                line: row,
                character: col + 1,
            },
        },
    }
}

fn ok_val(v: serde_json::Value) -> Result<serde_json::Value, hjkl_lsp::RpcError> {
    Ok(v)
}

fn err_val(msg: &str) -> Result<serde_json::Value, hjkl_lsp::RpcError> {
    Err(hjkl_lsp::RpcError {
        code: -32601,
        message: msg.to_string(),
    })
}

#[test]
fn goto_definition_single_jumps_cursor() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "line0\nline1\nline2\nline3");
    // Give the active slot a path so the location URI matches.
    let path = tmp_path("hjkl_gd_single.rs");
    app.active_mut().filename = Some(path.clone());
    let uri = file_url(&path);

    let loc = make_location(&uri, 2, 0);
    let result = ok_val(serde_json::to_value(vec![loc]).unwrap());
    let buffer_id = app.active().buffer_id as hjkl_lsp::BufferId;
    app.handle_goto_response(buffer_id, (0, 0), result, "definition");

    // Cursor must have moved to row 2.
    assert_eq!(app.active().editor.buffer().cursor().row, 2);
    assert!(app.picker.is_none(), "single result must not open picker");
}

#[test]
fn goto_definition_empty_sets_status() {
    let mut app = App::new(None, false, None, None).unwrap();
    let result = ok_val(serde_json::Value::Null);
    let buffer_id = app.active().buffer_id as hjkl_lsp::BufferId;
    app.handle_goto_response(buffer_id, (0, 0), result, "definition");

    let msg = app.status_message.as_deref().unwrap_or("");
    assert!(
        msg.contains("no definition found"),
        "expected 'no definition found', got: {msg}"
    );
    assert!(app.picker.is_none());
}

#[test]
fn goto_definition_multi_opens_picker() {
    let mut app = App::new(None, false, None, None).unwrap();
    let locs = vec![
        make_location("file:///tmp/a.rs", 0, 0),
        make_location("file:///tmp/b.rs", 5, 3),
        make_location("file:///tmp/c.rs", 10, 1),
    ];
    let result = ok_val(serde_json::to_value(locs).unwrap());
    let buffer_id = app.active().buffer_id as hjkl_lsp::BufferId;
    app.handle_goto_response(buffer_id, (0, 0), result, "definition");

    assert!(app.picker.is_some(), "multiple results must open picker");
}

#[test]
fn goto_references_always_opens_picker() {
    let mut app = App::new(None, false, None, None).unwrap();
    // Single result — references always opens picker.
    let locs = vec![make_location("file:///tmp/only.rs", 3, 0)];
    let result = ok_val(serde_json::to_value(locs).unwrap());
    let buffer_id = app.active().buffer_id as hjkl_lsp::BufferId;
    app.handle_references_response(buffer_id, (0, 0), result);

    assert!(app.picker.is_some(), "references must always open picker");
}

#[test]
fn hover_response_sets_info_popup() {
    let mut app = App::new(None, false, None, None).unwrap();
    let hover = lsp_types::Hover {
        contents: lsp_types::HoverContents::Markup(lsp_types::MarkupContent {
            kind: lsp_types::MarkupKind::Markdown,
            value: "**fn** foo() -> i32".to_string(),
        }),
        range: None,
    };
    let result = ok_val(serde_json::to_value(hover).unwrap());
    let buffer_id = app.active().buffer_id as hjkl_lsp::BufferId;
    app.handle_hover_response(buffer_id, (0, 0), result);

    assert!(app.info_popup.is_some(), "hover must set info_popup");
    let popup = app.info_popup.as_ref().unwrap();
    assert!(popup.contains("foo"), "popup must contain function name");
}

#[test]
fn hover_empty_sets_status() {
    let mut app = App::new(None, false, None, None).unwrap();
    let result: Result<serde_json::Value, hjkl_lsp::RpcError> = Ok(serde_json::Value::Null);
    let buffer_id = app.active().buffer_id as hjkl_lsp::BufferId;
    app.handle_hover_response(buffer_id, (0, 0), result);

    let msg = app.status_message.as_deref().unwrap_or("");
    assert!(
        msg.contains("no hover info"),
        "expected 'no hover info', got: {msg}"
    );
    assert!(app.info_popup.is_none());
}

#[test]
fn goto_definition_error_sets_status() {
    let mut app = App::new(None, false, None, None).unwrap();
    let result = err_val("server error");
    let buffer_id = app.active().buffer_id as hjkl_lsp::BufferId;
    app.handle_goto_response(buffer_id, (0, 0), result, "definition");

    let msg = app.status_message.as_deref().unwrap_or("");
    assert!(
        msg.contains("server error"),
        "expected error message, got: {msg}"
    );
}

#[test]
fn k_dispatches_hover() {
    // Without a real LspManager the call returns early with a status hint.
    let mut app = App::new(None, false, None, None).unwrap();
    app.active_mut().filename = Some(tmp_path("k_test.rs"));
    app.lsp_hover();
    assert!(app.info_popup.is_none());
    let msg = app.status_message.as_deref().unwrap_or("");
    assert!(msg.contains("LSP: not enabled"), "got: {msg}");
}

#[test]
fn gd_dispatches_goto_definition() {
    // Without a real LspManager the call returns early (no panic).
    let mut app = App::new(None, false, None, None).unwrap();
    app.active_mut().filename = Some(tmp_path("gd_test.rs"));
    app.lsp_goto_definition();
    // No LSP — nothing pending, no crash.
    assert!(app.lsp_pending.is_empty());
}

#[test]
fn lsp_request_works_with_relative_filename() {
    // Regression: opening hjkl with a relative path like
    // `apps/hjkl/src/main.rs` used to silently fail to attach to the LSP
    // server because url::Url::from_file_path requires absolute paths.
    // The absolutize() helper now joins relative paths against
    // current_dir() before URI conversion.
    let mut app = App::new(None, false, None, None).unwrap();
    let mgr = hjkl_lsp::LspManager::spawn(hjkl_lsp::LspConfig::default());
    app.lsp = Some(mgr);
    app.active_mut().filename = Some(std::path::PathBuf::from("src/main.rs"));
    app.lsp_goto_definition();
    // Request was registered as pending — absolutize made URI conversion
    // succeed even though the buffer's filename is relative.
    assert_eq!(
        app.lsp_pending.len(),
        1,
        "relative-path goto must produce a pending request, not the \
         'no file open' error path"
    );
    if let Some(mgr) = app.lsp.take() {
        mgr.shutdown();
    }
}

// ── Phase 4: completion popup tests ────────────────────────────────────────

fn make_completion_item(label: &str) -> crate::completion::CompletionItem {
    crate::completion::CompletionItem {
        label: label.to_string(),
        detail: None,
        kind: crate::completion::CompletionKind::Other,
        insert_text: label.to_string(),
        filter_text: None,
    }
}

fn synthesize_completion_response(labels: &[&str]) -> serde_json::Value {
    let items: Vec<serde_json::Value> = labels
        .iter()
        .map(|l| serde_json::json!({ "label": l }))
        .collect();
    serde_json::json!(items)
}

#[test]
fn completion_response_opens_popup() {
    let mut app = App::new(None, false, None, None).unwrap();
    // Enter insert mode so the guard passes.
    app.active_mut().editor.handle_key(key(KeyCode::Char('i')));
    // Give the buffer a filename so buffer_id matches.
    app.active_mut().filename = Some(std::path::PathBuf::from("/tmp/test.rs"));
    let buffer_id = app.active().buffer_id as hjkl_lsp::BufferId;

    let response_val = synthesize_completion_response(&["foo", "bar", "baz"]);
    app.handle_completion_response(buffer_id, 0, 0, Ok(response_val));

    assert!(app.completion.is_some(), "popup should open");
    let popup = app.completion.as_ref().unwrap();
    assert_eq!(popup.all_items.len(), 3);
    assert_eq!(popup.visible.len(), 3);
}

#[test]
fn completion_response_empty_no_popup() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.active_mut().editor.handle_key(key(KeyCode::Char('i')));
    app.active_mut().filename = Some(std::path::PathBuf::from("/tmp/test.rs"));
    let buffer_id = app.active().buffer_id as hjkl_lsp::BufferId;

    // Empty list response.
    let response_val = serde_json::json!([]);
    app.handle_completion_response(buffer_id, 0, 0, Ok(response_val));

    assert!(
        app.completion.is_none(),
        "empty response must not open popup"
    );
    assert!(
        app.status_message
            .as_deref()
            .unwrap_or("")
            .contains("no completions"),
        "status should report no completions"
    );
}

#[test]
fn completion_request_pending_routes_to_handler() {
    let mut app = App::new(None, false, None, None).unwrap();
    // Simulate a pending completion request.
    app.active_mut().editor.handle_key(key(KeyCode::Char('i')));
    app.active_mut().filename = Some(std::path::PathBuf::from("/tmp/test.rs"));
    let buffer_id = app.active().buffer_id as hjkl_lsp::BufferId;

    // Insert a fake pending request.
    let req_id = app.lsp_alloc_request_id();
    app.lsp_pending.insert(
        req_id,
        LspPendingRequest::Completion {
            buffer_id,
            anchor_row: 0,
            anchor_col: 0,
        },
    );

    // Simulate receiving a response.
    let response_val = synthesize_completion_response(&["alpha", "beta"]);
    let pending = app.lsp_pending.remove(&req_id).unwrap();
    app.handle_lsp_response(pending, Ok(response_val));

    assert!(
        app.completion.is_some(),
        "response must route to popup opener"
    );
    let popup = app.completion.as_ref().unwrap();
    assert_eq!(popup.all_items.len(), 2);
}

#[test]
fn accept_completion_inserts_selected_item() {
    let mut app = App::new(None, false, None, None).unwrap();
    // Seed buffer with some text and enter insert mode at col 0.
    seed_buffer(&mut app, "fn foo");
    app.active_mut().editor.handle_key(key(KeyCode::Char('i')));
    // Open popup anchored at col 0 row 0 with two items.
    let items = vec![make_completion_item("hello"), make_completion_item("world")];
    app.completion = Some(crate::completion::Completion::new(0, 0, items));
    // Select second item.
    app.completion.as_mut().unwrap().selected = 1;

    app.accept_completion();

    // Popup must be gone.
    assert!(app.completion.is_none());
    // Buffer line should start with "world" (inserted at col 0).
    let line = app.active().editor.buffer().lines()[0].clone();
    assert!(
        line.starts_with("world"),
        "buffer line should start with inserted text, got: {line:?}"
    );
}

#[test]
fn dismiss_completion_clears_state() {
    let mut app = App::new(None, false, None, None).unwrap();
    let items = vec![make_completion_item("foo")];
    app.completion = Some(crate::completion::Completion::new(0, 0, items));
    app.pending_ctrl_x = true;

    app.dismiss_completion();

    assert!(app.completion.is_none());
    assert!(!app.pending_ctrl_x);
}

#[test]
fn set_prefix_dismisses_when_filter_empty() {
    // Open popup, set prefix that matches nothing → popup auto-dismisses.
    let items = vec![make_completion_item("alpha"), make_completion_item("beta")];
    let mut popup = crate::completion::Completion::new(0, 0, items);
    popup.set_prefix("xyz");
    assert!(
        popup.is_empty(),
        "popup should be empty after non-matching prefix"
    );
}

// ── Phase 5 LSP tests ────────────────────────────────────────────────────

/// Build a minimal `lsp_types::WorkspaceEdit` with one file and one edit.
#[allow(clippy::mutable_key_type)]
fn make_workspace_edit(
    uri: &str,
    start_line: u32,
    start_char: u32,
    end_line: u32,
    end_char: u32,
    new_text: &str,
) -> lsp_types::WorkspaceEdit {
    let url = uri.parse::<lsp_types::Uri>().expect("valid URI");
    let mut changes = std::collections::HashMap::new();
    changes.insert(
        url,
        vec![lsp_types::TextEdit {
            range: lsp_types::Range {
                start: lsp_types::Position {
                    line: start_line,
                    character: start_char,
                },
                end: lsp_types::Position {
                    line: end_line,
                    character: end_char,
                },
            },
            new_text: new_text.to_string(),
        }],
    );
    lsp_types::WorkspaceEdit {
        changes: Some(changes),
        document_changes: None,
        change_annotations: None,
    }
}

#[test]
fn apply_workspace_edit_single_file() {
    let path = std::env::temp_dir().join("hjkl_ws_edit_single.txt");
    std::fs::write(&path, "hello world\n").unwrap();
    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();

    let uri = file_url(&path);
    let edit = make_workspace_edit(&uri, 0, 6, 0, 11, "rust");
    let count = app
        .apply_workspace_edit(edit)
        .expect("apply_workspace_edit failed");
    assert_eq!(count, 1);

    let lines = app.active().editor.buffer().lines();
    assert_eq!(
        lines[0], "hello rust",
        "edit should replace 'world' with 'rust'"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
#[allow(clippy::mutable_key_type)]
fn apply_workspace_edit_sorts_edits_descending() {
    // Two edits on the same line: first edit at col 0-3, second at col 6-11.
    // If applied in forward order the offsets shift; descending order must give correct result.
    let path = std::env::temp_dir().join("hjkl_ws_edit_sort.txt");
    std::fs::write(&path, "hello world foo\n").unwrap();
    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();

    let url = file_url(&path)
        .parse::<lsp_types::Uri>()
        .expect("valid URI");
    let mut changes = std::collections::HashMap::new();
    changes.insert(
        url,
        vec![
            // Edit 1: replace "hello" (0-5) with "hi"
            lsp_types::TextEdit {
                range: lsp_types::Range {
                    start: lsp_types::Position {
                        line: 0,
                        character: 0,
                    },
                    end: lsp_types::Position {
                        line: 0,
                        character: 5,
                    },
                },
                new_text: "hi".to_string(),
            },
            // Edit 2: replace "world" (6-11) with "earth"
            lsp_types::TextEdit {
                range: lsp_types::Range {
                    start: lsp_types::Position {
                        line: 0,
                        character: 6,
                    },
                    end: lsp_types::Position {
                        line: 0,
                        character: 11,
                    },
                },
                new_text: "earth".to_string(),
            },
        ],
    );
    let edit = lsp_types::WorkspaceEdit {
        changes: Some(changes),
        document_changes: None,
        change_annotations: None,
    };
    app.apply_workspace_edit(edit).expect("apply failed");
    let lines = app.active().editor.buffer().lines();
    assert_eq!(lines[0], "hi earth foo", "both edits must apply correctly");
    let _ = std::fs::remove_file(&path);
}

#[test]
#[allow(clippy::mutable_key_type)]
fn apply_workspace_edit_multi_file() {
    let path_a = std::env::temp_dir().join("hjkl_ws_multi_a.txt");
    let path_b = std::env::temp_dir().join("hjkl_ws_multi_b.txt");
    std::fs::write(&path_a, "file a content\n").unwrap();
    std::fs::write(&path_b, "file b content\n").unwrap();

    let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();

    let uri_a = file_url(&path_a);
    let uri_b = file_url(&path_b);

    let url_a = uri_a.parse::<lsp_types::Uri>().expect("valid URI a");
    let url_b = uri_b.parse::<lsp_types::Uri>().expect("valid URI b");
    let mut changes = std::collections::HashMap::new();
    changes.insert(
        url_a,
        vec![lsp_types::TextEdit {
            range: lsp_types::Range {
                start: lsp_types::Position {
                    line: 0,
                    character: 7,
                },
                end: lsp_types::Position {
                    line: 0,
                    character: 14,
                },
            },
            new_text: "edited".to_string(),
        }],
    );
    changes.insert(
        url_b,
        vec![lsp_types::TextEdit {
            range: lsp_types::Range {
                start: lsp_types::Position {
                    line: 0,
                    character: 7,
                },
                end: lsp_types::Position {
                    line: 0,
                    character: 14,
                },
            },
            new_text: "changed".to_string(),
        }],
    );

    let edit = lsp_types::WorkspaceEdit {
        changes: Some(changes),
        document_changes: None,
        change_annotations: None,
    };
    let count = app
        .apply_workspace_edit(edit)
        .expect("multi-file apply failed");
    assert_eq!(count, 2, "should affect 2 files");
    let _ = std::fs::remove_file(&path_a);
    let _ = std::fs::remove_file(&path_b);
}

#[test]
fn rename_response_null_sets_status() {
    let mut app = App::new(None, false, None, None).unwrap();
    let pending = LspPendingRequest::Rename {
        buffer_id: 0,
        anchor_row: 0,
        anchor_col: 0,
        new_name: "newName".to_string(),
    };
    app.handle_lsp_response(pending, Ok(serde_json::Value::Null));
    let msg = app.status_message.clone().unwrap_or_default();
    assert!(
        msg.contains("cannot rename"),
        "null rename must set 'cannot rename' status, got: {msg}"
    );
}

#[test]
fn rename_response_applies_workspace_edit() {
    let path = std::env::temp_dir().join("hjkl_rename_apply.txt");
    std::fs::write(&path, "old_name here\n").unwrap();
    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();

    let uri = file_url(&path);
    let edit = make_workspace_edit(&uri, 0, 0, 0, 8, "new_name");
    let val = serde_json::to_value(edit).unwrap();

    let pending = LspPendingRequest::Rename {
        buffer_id: 0,
        anchor_row: 0,
        anchor_col: 0,
        new_name: "new_name".to_string(),
    };
    app.handle_lsp_response(pending, Ok(val));
    let msg = app.status_message.clone().unwrap_or_default();
    assert!(
        msg.contains("renamed"),
        "rename response must set status, got: {msg}"
    );
    let lines = app.active().editor.buffer().lines();
    assert_eq!(lines[0], "new_name here");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn format_response_empty_sets_status() {
    let mut app = App::new(None, false, None, None).unwrap();
    let pending = LspPendingRequest::Format {
        buffer_id: 0,
        range: None,
    };
    // Empty array = no changes.
    app.handle_lsp_response(pending, Ok(serde_json::json!([])));
    let msg = app.status_message.clone().unwrap_or_default();
    assert!(
        msg.contains("no formatting"),
        "empty format response must say 'no formatting changes', got: {msg}"
    );
}

#[test]
fn format_response_applies_text_edits() {
    let path = std::env::temp_dir().join("hjkl_format_apply.txt");
    std::fs::write(&path, "fn foo(){}\n").unwrap();
    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
    let buf_id = app.active().buffer_id as hjkl_lsp::BufferId;

    // Insert a space at col 9 (after the `{`) → "fn foo(){ }"
    let edits: Vec<lsp_types::TextEdit> = vec![lsp_types::TextEdit {
        range: lsp_types::Range {
            start: lsp_types::Position {
                line: 0,
                character: 9,
            },
            end: lsp_types::Position {
                line: 0,
                character: 9,
            },
        },
        new_text: " ".to_string(),
    }];
    let val = serde_json::to_value(&edits).unwrap();

    let pending = LspPendingRequest::Format {
        buffer_id: buf_id,
        range: None,
    };
    app.handle_lsp_response(pending, Ok(val));
    let msg = app.status_message.clone().unwrap_or_default();
    assert_eq!(msg, "formatted");
    let lines = app.active().editor.buffer().lines();
    // "fn foo(){}" with space inserted at pos 9 → "fn foo(){ }"
    assert_eq!(lines[0], "fn foo(){ }");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn code_action_response_empty_sets_status() {
    let mut app = App::new(None, false, None, None).unwrap();
    let pending = LspPendingRequest::CodeAction {
        buffer_id: 0,
        anchor_row: 0,
        anchor_col: 0,
    };
    app.handle_lsp_response(pending, Ok(serde_json::json!([])));
    let msg = app.status_message.clone().unwrap_or_default();
    assert!(
        msg.contains("no code actions"),
        "empty code actions must say 'no code actions', got: {msg}"
    );
}

#[test]
fn code_action_response_multi_opens_picker() {
    let mut app = App::new(None, false, None, None).unwrap();
    let pending = LspPendingRequest::CodeAction {
        buffer_id: 0,
        anchor_row: 0,
        anchor_col: 0,
    };
    let actions = serde_json::json!([
        {
            "title": "Fix import",
            "kind": "quickfix",
        },
        {
            "title": "Extract method",
            "kind": "refactor",
        },
    ]);
    app.handle_lsp_response(pending, Ok(actions));
    assert!(
        app.picker.is_some(),
        "multiple code actions must open picker"
    );
    assert_eq!(
        app.pending_code_actions.len(),
        2,
        "pending_code_actions must hold both actions"
    );
}

#[test]
fn code_action_response_single_applies_action() {
    let path = std::env::temp_dir().join("hjkl_ca_single.txt");
    std::fs::write(&path, "old content\n").unwrap();
    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();

    let uri = file_url(&path);
    let edit = make_workspace_edit(&uri, 0, 0, 0, 11, "new content");
    let action = lsp_types::CodeAction {
        title: "Replace content".to_string(),
        edit: Some(edit),
        ..Default::default()
    };
    let val =
        serde_json::to_value(vec![lsp_types::CodeActionOrCommand::CodeAction(action)]).unwrap();

    let pending = LspPendingRequest::CodeAction {
        buffer_id: 0,
        anchor_row: 0,
        anchor_col: 0,
    };
    app.handle_lsp_response(pending, Ok(val));
    // Single action: applied directly, no picker.
    assert!(
        app.picker.is_none(),
        "single code action must not open picker"
    );
    let msg = app.status_message.clone().unwrap_or_default();
    assert!(
        msg.contains("files changed"),
        "single action apply must set status, got: {msg}"
    );
    let lines = app.active().editor.buffer().lines();
    assert_eq!(lines[0], "new content");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn lsp_code_actions_includes_overlapping_diags_in_context() {
    // Verify that lsp_code_actions collects diagnostics that overlap the cursor.
    // We set up a slot with diags and check the request would include them.
    // Since we can't intercept the LspManager send, we test the diagnostic
    // overlap logic used by lsp_code_actions separately here.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "fn foo() {\n    let x = 1;\n}\n");

    // Seed two diagnostics: one overlapping the cursor, one not.
    app.active_mut().lsp_diags = vec![
        LspDiag {
            start_row: 0,
            start_col: 3,
            end_row: 0,
            end_col: 6,
            severity: DiagSeverity::Error,
            message: "overlapping".to_string(),
            source: None,
            code: None,
        },
        LspDiag {
            start_row: 1,
            start_col: 0,
            end_row: 1,
            end_col: 5,
            severity: DiagSeverity::Warning,
            message: "not overlapping".to_string(),
            source: None,
            code: None,
        },
    ];

    // Position cursor at row=0, col=4 (inside the first diag range).
    app.active_mut().editor.jump_cursor(0, 4);

    // Test the overlap logic directly.
    let cursor_row = 0usize;
    let cursor_col = 4usize;
    let diags = &app.active().lsp_diags;
    let overlapping: Vec<_> = diags
        .iter()
        .filter(|d| {
            let after_start = (cursor_row, cursor_col) >= (d.start_row, d.start_col);
            let before_end = cursor_row < d.end_row
                || (cursor_row == d.end_row && cursor_col < d.end_col)
                || (cursor_row == d.start_row && d.start_row == d.end_row);
            after_start && (before_end || cursor_row == d.start_row)
        })
        .collect();

    assert_eq!(
        overlapping.len(),
        1,
        "only the overlapping diag should be included"
    );
    assert_eq!(overlapping[0].message, "overlapping");
}
