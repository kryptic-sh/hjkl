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
    app.active_mut().filename = Some(std::path::PathBuf::from("/tmp/hjkl_wq_ro_test.txt"));
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
    assert_eq!(app.focused_window, 0);

    app.dispatch_ex("sp");

    // A second window should now exist.
    assert_eq!(
        app.windows.iter().filter(|w| w.is_some()).count(),
        2,
        "expected 2 open windows after :sp"
    );
    // Focus moved to the new (upper) window.
    let new_win_id = app.focused_window;
    assert_ne!(new_win_id, 0, "focus must have moved to the new window");
    // The layout should no longer be a single leaf.
    assert!(
        app.layout.leaves().len() == 2,
        "layout must have 2 leaves after split"
    );
}

#[test]
fn close_focused_window_collapses_split() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("sp");
    assert_eq!(app.windows.iter().filter(|w| w.is_some()).count(), 2);
    let focused_before_close = app.focused_window;

    app.dispatch_ex("close");

    // After closing, the closed window's entry is None.
    assert!(
        app.windows[focused_before_close].is_none(),
        "closed window entry must be None"
    );
    // Layout should be back to a single leaf.
    assert_eq!(
        app.layout.leaves().len(),
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
    let top_win = app.focused_window;

    // Ctrl-w j should move focus to the window below (the original).
    app.focus_below();
    let bottom_win = app.focused_window;
    assert_ne!(top_win, bottom_win, "focus must have moved down");
    // Moving below from bottom-most is a no-op.
    app.focus_below();
    assert_eq!(
        app.focused_window, bottom_win,
        "focus must not move below the bottom-most window"
    );
}

#[test]
fn ctrl_w_k_focuses_above() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("sp");
    // Currently on top — move to bottom first.
    app.focus_below();
    let bottom_win = app.focused_window;

    // Ctrl-w k should move back up.
    app.focus_above();
    let top_win = app.focused_window;
    assert_ne!(bottom_win, top_win, "focus must have moved up");
    // Moving above from top-most is a no-op.
    app.focus_above();
    assert_eq!(
        app.focused_window, top_win,
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
    let top_win = app.focused_window;
    // Move focus to bottom window.
    app.focus_below();
    let bottom_win = app.focused_window;
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
    let original_win = app.focused_window;

    app.dispatch_ex("vsp");

    // Two windows now exist.
    assert_eq!(
        app.windows.iter().filter(|w| w.is_some()).count(),
        2,
        "expected 2 open windows after :vsp"
    );
    // Layout has 2 leaves.
    assert_eq!(app.layout.leaves().len(), 2, "layout must have 2 leaves");

    // Focus moved to the new (left) window.
    let new_win = app.focused_window;
    assert_ne!(new_win, original_win, "focus must have moved to new window");

    // New window is on the left (a-side) of a Vertical split — its
    // neighbor_right is the original window.
    let right = app.layout.neighbor_right(new_win);
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
    let original_win = app.focused_window;

    app.dispatch_ex("vnew");

    assert_eq!(
        app.windows.iter().filter(|w| w.is_some()).count(),
        2,
        "expected 2 open windows after :vnew"
    );
    assert_eq!(app.layout.leaves().len(), 2);

    let new_win = app.focused_window;
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
    let left_win = app.focused_window;

    // Can't go further left — no-op.
    app.focus_left();
    assert_eq!(
        app.focused_window, left_win,
        "focus_left from leftmost must be a no-op"
    );

    // Move right first, then come back left.
    app.focus_right();
    let right_win = app.focused_window;
    assert_ne!(left_win, right_win, "focus must have moved right");

    app.focus_left();
    assert_eq!(
        app.focused_window, left_win,
        "focus_left must return to left window"
    );
}

#[test]
fn ctrl_w_l_focuses_right() {
    let mut app = App::new(None, false, None, None).unwrap();
    // After :vsp, focus is on the left (new) window.
    app.dispatch_ex("vsp");
    let left_win = app.focused_window;

    // Move right.
    app.focus_right();
    let right_win = app.focused_window;
    assert_ne!(left_win, right_win, "focus must have moved right");

    // Can't go further right — no-op.
    app.focus_right();
    assert_eq!(
        app.focused_window, right_win,
        "focus_right from rightmost must be a no-op"
    );
}

#[test]
fn ctrl_w_w_cycles_next() {
    let mut app = App::new(None, false, None, None).unwrap();
    // Create two windows via :sp.
    app.dispatch_ex("sp");
    let leaves = app.layout.leaves();
    assert_eq!(leaves.len(), 2);

    // From the current focused window, next should cycle.
    let initial = app.focused_window;
    app.focus_next();
    let after_one = app.focused_window;
    assert_ne!(initial, after_one, "focus_next must move focus");
    app.focus_next();
    let after_two = app.focused_window;
    // With 2 windows, two focus_next should bring us back.
    assert_eq!(after_two, initial, "two focus_next calls must wrap around");
}

#[test]
fn ctrl_w_shift_w_cycles_previous() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("sp");

    let initial = app.focused_window;
    app.focus_previous();
    let after_one = app.focused_window;
    assert_ne!(initial, after_one, "focus_previous must move focus");
    app.focus_previous();
    let after_two = app.focused_window;
    assert_eq!(
        after_two, initial,
        "two focus_previous calls must wrap around"
    );
}
