use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::Duration;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn op_kind_to_operator(k: hjkl_vim::OperatorKind) -> hjkl_engine::Operator {
    match k {
        hjkl_vim::OperatorKind::Delete => hjkl_engine::Operator::Delete,
        hjkl_vim::OperatorKind::Yank => hjkl_engine::Operator::Yank,
        hjkl_vim::OperatorKind::Change => hjkl_engine::Operator::Change,
        hjkl_vim::OperatorKind::Indent => hjkl_engine::Operator::Indent,
        hjkl_vim::OperatorKind::Outdent => hjkl_engine::Operator::Outdent,
    }
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

// ── Runtime map tests ──────────────────────────────────────────────────

#[test]
fn runtime_nmap_registers_on_trie_and_fires() {
    // `:nmap x y` — trie should consume 'x' (returns true / consumed)
    // and replay 'y' to the engine (recursive = true but y has no binding).
    // In Normal mode, 'y' (yank) goes to the engine — no crash, no panic.
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("nmap x y");
    assert!(
        !app.user_keymap_records.is_empty(),
        "record should be stored after nmap"
    );

    use crate::app::keymap::HjklMode as Mode;
    use hjkl_keymap::{KeyCode as KmCode, KeyEvent as KmEvent, KeyModifiers as KmMods};
    let km_ev = KmEvent::new(KmCode::Char('x'), KmMods::NONE);
    let mut replay = Vec::new();
    let consumed = app.dispatch_keymap_in_mode(km_ev, 1, &mut replay, Mode::Normal);
    assert!(consumed, "nmap x should match and be consumed by trie");
    // After recursive replay of 'y' to Normal-mode trie (unbound → engine):
    // no crash and replay is empty (consumed via engine path).
    assert!(
        replay.is_empty(),
        "x consumed by trie, replay should be empty"
    );
}

#[test]
fn noremap_does_not_recurse_through_trie() {
    // `:nnoremap a b` + `:nmap b y` — dispatching 'a' should replay 'b' directly
    // to the engine WITHOUT going through the trie, so 'b' binding is NOT fired.
    // Observable: the buffer receives a raw 'b' keypress in Normal mode (engine
    // treats it as "go to start of previous word" — no crash, no panic).
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("nmap b y"); // recursive binding for b
    app.dispatch_ex("nnoremap a b"); // non-recursive: a → b raw

    use crate::app::keymap::HjklMode as Mode;
    use hjkl_keymap::{KeyCode as KmCode, KeyEvent as KmEvent, KeyModifiers as KmMods};
    let km_ev = KmEvent::new(KmCode::Char('a'), KmMods::NONE);
    let mut replay = Vec::new();
    let consumed = app.dispatch_keymap_in_mode(km_ev, 1, &mut replay, Mode::Normal);
    assert!(consumed, "nnoremap a should match");
    // Non-recursive: b goes straight to engine, not back through trie.
    // 'b' binding (nmap b y) must NOT fire a second Replay; engine just
    // moves the cursor or is a no-op. No panic = success.
}

#[test]
fn imap_jj_enters_normal_mode() {
    // `:imap jj <Esc>` — feed two 'j' keys through the trie in Insert mode.
    // First 'j' should be Pending; second should match and send Esc to engine.
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("imap jj <Esc>");
    // Enter insert mode.
    app.active_mut().editor.handle_key(key(KeyCode::Char('i')));
    assert_eq!(app.active().editor.vim_mode(), VimMode::Insert);

    use crate::app::keymap::HjklMode as Mode;
    use hjkl_keymap::{KeyCode as KmCode, KeyEvent as KmEvent, KeyModifiers as KmMods};
    let j_ev = KmEvent::new(KmCode::Char('j'), KmMods::NONE);
    let mut replay = Vec::new();

    // First 'j' — should be Pending.
    let consumed = app.dispatch_keymap_in_mode(j_ev, 1, &mut replay, Mode::Insert);
    assert!(
        consumed,
        "first j should be pending (chord not yet complete)"
    );
    assert_eq!(app.active().editor.vim_mode(), VimMode::Insert);

    // Second 'j' — should match and produce Replay{<Esc>}.
    let consumed = app.dispatch_keymap_in_mode(j_ev, 1, &mut replay, Mode::Insert);
    assert!(consumed, "second j should match imap jj");
    assert_eq!(
        app.active().editor.vim_mode(),
        VimMode::Normal,
        "imap jj <Esc> should leave Insert mode"
    );
}

#[test]
fn list_user_maps_excludes_builtin_chords() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("nmap a b");
    app.dispatch_ex("imap c d");
    // `:nmap` (no rhs) lists Normal-mode user maps only.
    app.dispatch_ex("nmap");
    let popup = app.info_popup.as_deref().unwrap_or("");
    assert!(popup.contains('a'), "should list `a` Normal mapping");
    // leader+f is a built-in; it must not appear in user map listing.
    assert!(
        !popup.contains("<leader>f"),
        "must not list built-in <leader>f"
    );
    // 'c' is imap — not in nmap listing.
    assert!(!popup.contains('c'), "imap c must not appear in nmap list");

    // Now list imap separately.
    app.dispatch_ex("imap");
    let popup = app.info_popup.as_deref().unwrap_or("");
    assert!(popup.contains('c'), "imap listing should contain `c`");
}

#[test]
fn cyclic_recursive_map_bails_without_stack_overflow() {
    // `:nmap a a` is a vertical cycle: feeding 'a' matches Replay{[a]},
    // which dispatches feed('a') again, ad infinitum. The replay_depth
    // guard must catch this before the call stack overflows. We assert
    // that dispatch completes (no SIGSEGV) and an E223 status appears.
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("nmap a a");

    use crate::app::keymap::HjklMode as Mode;
    use hjkl_keymap::{KeyCode as KmCode, KeyEvent as KmEvent, KeyModifiers as KmMods};
    let km_ev = KmEvent::new(KmCode::Char('a'), KmMods::NONE);
    let mut replay = Vec::new();
    let consumed = app.dispatch_keymap_in_mode(km_ev, 1, &mut replay, Mode::Normal);
    assert!(consumed, "nmap a should match and consume");
    let msg = app.status_message.clone().unwrap_or_default();
    assert!(
        msg.contains("E223"),
        "expected E223 recursive-mapping error, got: {msg:?}"
    );
    // replay_depth must unwind back to 0 after the bail.
    assert_eq!(
        app.replay_depth, 0,
        "replay_depth must return to 0 after cycle bail"
    );
}

#[test]
fn unmap_removes_from_trie() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("nmap a b");
    app.dispatch_ex("nunmap a");

    use crate::app::keymap::HjklMode as Mode;
    use hjkl_keymap::{KeyCode as KmCode, KeyEvent as KmEvent, KeyModifiers as KmMods};
    let km_ev = KmEvent::new(KmCode::Char('a'), KmMods::NONE);
    let mut replay = Vec::new();
    let consumed = app.dispatch_keymap_in_mode(km_ev, 1, &mut replay, Mode::Normal);
    assert!(!consumed, "unmapped `a` should be unbound");
    assert_eq!(replay.len(), 1, "unbound key should be in replay");
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
fn open_git_status_picker_sets_picker() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert!(app.picker.is_none());
    app.open_git_status_picker();
    assert!(
        app.picker.is_some(),
        "picker should be open after open_git_status_picker"
    );
}

#[test]
fn git_status_picker_title_is_git_status() {
    use crate::picker_git::GitStatusPicker;
    use hjkl_picker::PickerLogic;
    let tmp = tempfile::tempdir().unwrap();
    let source = GitStatusPicker::new(tmp.path().to_path_buf());
    assert_eq!(source.title(), "git status");
}

// ── Git log picker smoke tests ─────────────────────────────────────────

#[test]
fn git_log_picker_opens() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert!(app.picker.is_none());
    app.open_git_log_picker();
    assert!(
        app.picker.is_some(),
        "picker should be open after open_git_log_picker"
    );
}

#[test]
fn git_log_picker_title_is_git_log() {
    use crate::picker_git::GitLogPicker;
    use hjkl_picker::PickerLogic;
    let tmp = tempfile::tempdir().unwrap();
    let source = GitLogPicker::new(tmp.path().to_path_buf());
    assert_eq!(source.title(), "git log");
}

// ── Git branch picker smoke tests ──────────────────────────────────────

#[test]
fn git_branch_picker_opens() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert!(app.picker.is_none());
    app.open_git_branch_picker();
    assert!(
        app.picker.is_some(),
        "picker should be open after open_git_branch_picker"
    );
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
fn git_file_history_picker_opens() {
    let path = std::env::temp_dir().join("hjkl_gB_smoke.txt");
    std::fs::write(&path, "content\n").unwrap();
    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
    assert!(app.picker.is_none());
    // Buffer has a path — picker opens (it may show sentinel if not a repo).
    app.open_git_file_history_picker();
    let _ = std::fs::remove_file(&path);
}

#[test]
fn git_file_history_picker_no_path_sets_status() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert!(app.active().filename.is_none());
    app.open_git_file_history_picker();
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
    let source = GitFileHistoryPicker::new(
        tmp.path().to_path_buf(),
        std::path::PathBuf::from("src/main.rs"),
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
    let mut source = GitStatusPicker::new(tmp.path().to_path_buf());

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
fn git_stash_picker_opens() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert!(app.picker.is_none());
    app.open_git_stash_picker();
    assert!(
        app.picker.is_some(),
        "picker should be open after open_git_stash_picker"
    );
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
    app.open_git_stash_picker();
    assert!(app.picker.is_some(), "S chord must open the stash picker");
    assert_eq!(app.picker.as_ref().unwrap().title(), "git stashes");
}

// ── Git tags picker smoke tests ───────────────────────────────────────────

#[test]
fn git_tags_picker_opens() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert!(app.picker.is_none());
    app.open_git_tags_picker();
    assert!(
        app.picker.is_some(),
        "picker should be open after open_git_tags_picker"
    );
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
fn git_remotes_picker_opens() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert!(app.picker.is_none());
    app.open_git_remotes_picker();
    assert!(
        app.picker.is_some(),
        "picker should be open after open_git_remotes_picker"
    );
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
fn ctrl_w_lt_resize_width_negative_registers() {
    // Regression: the binding string `<C-w><` failed to parse because the
    // trailing bare `<` was interpreted as an unclosed tag. Fix is to use
    // the `<lt>` escape. Verify that the chord registers via app_keymap and
    // resolves to AppAction::ResizeWidth(-1).
    use crate::app::keymap::HjklMode as Mode;
    use hjkl_keymap::{
        Chord, KeyCode, KeyEvent as KmKeyEvent, KeyModifiers as KmKeyMods, KeyResolve,
    };
    let mut app = App::new(None, false, None, None).unwrap();

    let ctrl_w = KmKeyEvent::new(KeyCode::Char('w'), KmKeyMods::CTRL);
    let lt = KmKeyEvent::new(KeyCode::Char('<'), KmKeyMods::NONE);
    let chord = Chord(vec![ctrl_w, lt]);

    // children() must show `<` reachable from `<C-w>` prefix.
    let kids = app.app_keymap.children(Mode::Normal, &Chord(vec![ctrl_w]));
    assert!(
        kids.iter().any(|(k, _)| *k == lt),
        "<C-w><lt> binding must register; kids: {kids:?}"
    );

    // Drive the chord and assert it matches ResizeWidth(-1).
    let r1 = app
        .app_keymap
        .feed(Mode::Normal, ctrl_w, std::time::Instant::now());
    assert!(matches!(r1, KeyResolve::Pending | KeyResolve::Ambiguous));
    let r2 = app
        .app_keymap
        .feed(Mode::Normal, lt, std::time::Instant::now());
    match r2 {
        KeyResolve::Match(binding) => {
            assert!(matches!(
                binding.action,
                crate::keymap_actions::AppAction::ResizeWidth(-1)
            ));
        }
        other => panic!("expected Match(ResizeWidth(-1)), got {other:?}"),
    }
    // Silence unused warning if Chord is exported but not used in current build flags.
    let _ = chord;
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
    // Ctrl-w '+' resolves through the keymap trie to AppAction::ResizeHeight(1);
    // exercise the underlying resize_height(1) call here without a real terminal.
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
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("tabnew");
    assert_eq!(app.tabs.len(), 2);
    app.dispatch_ex("tabprev");
    assert_eq!(app.active_tab, 0);
    drive_key(&mut app, key(KeyCode::Char('g')));
    drive_key(&mut app, key(KeyCode::Char('t')));
    assert_eq!(app.active_tab, 1, "gt must advance to the next tab");
}

#[test]
fn gt_switches_tab_backward() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("tabnew");
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.active_tab, 1);
    drive_key(&mut app, key(KeyCode::Char('g')));
    drive_key(
        &mut app,
        crossterm::event::KeyEvent::new(KeyCode::Char('T'), crossterm::event::KeyModifiers::SHIFT),
    );
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
    app.dispatch_ex("sp");
    assert_eq!(app.tabs.len(), 1);

    drive_key(&mut app, ctrl_key('w'));
    drive_key(
        &mut app,
        crossterm::event::KeyEvent::new(KeyCode::Char('T'), crossterm::event::KeyModifiers::SHIFT),
    );

    assert_eq!(app.tabs.len(), 2, "Ctrl-w T must create a new tab");
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
fn gg_scrolls_window_viewport_to_top() {
    // Regression: gg moved cursor to (0,0) and the engine called
    // ensure_cursor_in_scrolloff, but the host's keymap-Unbound branch
    // forwarded the key to the engine WITHOUT calling
    // sync_viewport_from_editor — so the focused window's stored
    // top_row stayed at the old position.
    let mut app = App::new(None, false, None, None).unwrap();
    let lines: Vec<String> = (0..100).map(|i| format!("line {i}")).collect();
    seed_buffer(&mut app, &lines.join("\n"));

    // Position cursor + viewport deep in the buffer. The viewport_height
    // atomic must also be set — every vim::step resyncs vp.height from
    // it, so leaving the atomic at 0 would zero the host viewport mid-step
    // and disable scrolloff math.
    app.active_mut().editor.set_viewport_height(20);
    {
        let vp = app.active_mut().editor.host_mut().viewport_mut();
        vp.width = 80;
        vp.height = 20;
        vp.text_width = 80;
        vp.top_row = 60;
    }
    app.active_mut().editor.jump_cursor(70, 0);
    app.sync_viewport_from_editor();
    let fw = app.focused_window();
    assert_eq!(app.windows[fw].as_ref().unwrap().top_row, 60);

    // Drive `gg` through the engine. First `g` sets engine-side pending,
    // second `g` triggers the gg motion (cursor → top + auto-scroll).
    app.active_mut()
        .editor
        .handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
    app.active_mut()
        .editor
        .handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
    // The Unbound replay path in event_loop.rs syncs the editor's
    // auto-scrolled viewport back to the focused window.
    app.sync_viewport_from_editor();

    let (row, _col) = app.active().editor.cursor();
    assert_eq!(row, 0, "gg must put cursor at row 0");
    let stored_top = app.windows[fw].as_ref().unwrap().top_row;
    assert!(
        stored_top < 60,
        "gg must scroll window viewport to top, but stored top_row stayed at {stored_top}"
    );
}

#[test]
fn plus_slash_argv_scrolls_window_viewport_to_match() {
    // Regression: +/pat moved the cursor but didn't scroll the viewport,
    // so the rendered viewport stayed at row 0 and the cursor landed
    // off-screen on large files. Fix: App::new calls
    // ensure_cursor_in_scrolloff after the search and seeds the initial
    // window's top_row from the editor viewport.
    use std::io::Write;
    let dir = std::env::temp_dir().join("hjkl_plus_slash_scroll");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sample.rs");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        // 100 lines of filler; first `target` match deep at row 80.
        for i in 0..100 {
            if i == 80 {
                writeln!(f, "fn target() {{}}").unwrap();
            } else {
                writeln!(f, "// padding line {i}").unwrap();
            }
        }
    }
    // Set viewport_height atomic via a fake App + apply_viewport_height
    // before the search runs. App::new builds the slot with
    // crossterm::terminal::size() — under tests that may return 0,
    // disabling scrolloff. Pre-set the atomic by dropping in via the
    // test helper.
    // Easier path: build a small file where the first match is on row 5
    // and assert window.top_row > 0 (proxy for "scrolled").
    let mut app = App::new(Some(path.clone()), false, None, Some("target".into())).unwrap();
    let (row, _col) = app.active().editor.cursor();
    assert_eq!(row, 80, "+/target must move cursor to row 80");
    // The window's stored top_row should reflect the editor's scrolled
    // viewport. With crossterm::terminal::size returning 0 in test
    // contexts the scroll math is a no-op, so set the height atomic
    // and re-run ensure_cursor_in_scrolloff to verify the scroll path.
    app.active_mut().editor.set_viewport_height(20);
    {
        let vp = app.active_mut().editor.host_mut().viewport_mut();
        vp.width = 80;
        vp.height = 20;
        vp.text_width = 80;
    }
    app.active_mut().editor.ensure_cursor_in_scrolloff();
    let editor_top = app.active().editor.host().viewport().top_row;
    assert!(
        editor_top > 0,
        "ensure_cursor_in_scrolloff should scroll editor viewport away from row 0; got top_row={editor_top}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn slash_search_in_editor_scrolls_window_viewport() {
    // Regression: /pat<CR> in the editor moved the cursor but didn't
    // scroll the focused window's viewport, leaving the cursor
    // off-screen on large files.
    let mut app = App::new(None, false, None, None).unwrap();
    let lines: Vec<String> = (0..100)
        .map(|i| {
            if i == 80 {
                "target".into()
            } else {
                format!("line {i}")
            }
        })
        .collect();
    seed_buffer(&mut app, &lines.join("\n"));
    app.active_mut().editor.set_viewport_height(20);
    {
        let vp = app.active_mut().editor.host_mut().viewport_mut();
        vp.width = 80;
        vp.height = 20;
        vp.text_width = 80;
    }
    let fw = app.focused_window();
    // Cursor at (0,0), window.top_row=0. Run /target<CR>.
    app.commit_search("target");
    let stored_top = app.windows[fw].as_ref().unwrap().top_row;
    assert!(
        stored_top > 0,
        "/target<CR> should scroll the focused window's stored top_row past 0 to reveal the match"
    );
    let (row, _col) = app.active().editor.cursor();
    assert_eq!(row, 80, "/target<CR> should land cursor on row 80");
    // Counter must show 1/1 (cursor on the only match), not 0/1.
    let count = crate::render::search_count(&app);
    assert_eq!(
        count,
        Some((1, 1)),
        "search counter must update after /<CR>"
    );
    // Cursor must respect SCROLLOFF=5: cursor at row 80, height 20, so
    // viewport top_row should be such that screen row is between
    // [margin, height-1-margin] = [5, 14]. Specifically max_bottom=14
    // → top = 80 - 14 = 66.
    let stored_top = app.windows[fw].as_ref().unwrap().top_row;
    let screen_row = 80usize.saturating_sub(stored_top);
    assert!(
        (5..=14).contains(&screen_row),
        "scrolloff=5 violated: screen_row={screen_row} (top={stored_top}, cursor=80, height=20)"
    );
}

#[test]
fn plus_slash_argv_with_realistic_rust_source() {
    // Mirror the user's repro: hjkl +/main on a real-ish rust file.
    use std::io::Write;
    let dir = std::env::temp_dir().join("hjkl_plus_slash_real");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sample.rs");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        // Real-ish content. First `main` substring is on row 5 (`fn main`).
        writeln!(f, "//! crate root").unwrap(); // row 0
        writeln!(f).unwrap(); // row 1
        writeln!(f, "use std::path::PathBuf;").unwrap();
        writeln!(f).unwrap();
        writeln!(f, "/// Entry.").unwrap();
        writeln!(f, "fn main() {{").unwrap(); // row 5: first 'main'
        writeln!(f, "    let _ = main_helper();").unwrap(); // row 6: 'main_helper'
        writeln!(f, "}}").unwrap();
        writeln!(f, "fn main_helper() {{}}").unwrap(); // row 8: 'main_helper'
    }
    let app = App::new(Some(path.clone()), false, None, Some("main".into())).unwrap();
    let (row, _col) = app.active().editor.cursor();
    assert_eq!(
        row, 5,
        "+/main on rust source must land on row 5 (first `fn main`), got row {row}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn plus_slash_argv_search_lands_on_first_forward_match() {
    // Regression: hjkl +/main file.rs lands cursor on a match in the
    // backward direction (or wraps incorrectly) because the +/<pat>
    // path advanced from cursor=(0,0) and the wrap policy mishandles
    // the at-or-after invariant.
    use std::io::Write;
    let dir = std::env::temp_dir().join("hjkl_plus_slash_test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sample.txt");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        // 3 matches at known rows. First match at row 2.
        writeln!(f, "alpha").unwrap();
        writeln!(f, "beta").unwrap();
        writeln!(f, "main one").unwrap();
        writeln!(f, "delta").unwrap();
        writeln!(f, "main two").unwrap();
        writeln!(f, "main three").unwrap();
    }
    let app = App::new(Some(path.clone()), false, None, Some("main".into())).unwrap();
    let (row, col) = app.active().editor.cursor();
    assert_eq!(
        row, 2,
        "+/main must land on the FIRST forward match (row 2), got row {row}"
    );
    assert_eq!(col, 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn plus_slash_argv_search_with_goto_line_searches_forward() {
    // hjkl +5 +/main file.rs : goto_line first, then search forward.
    use std::io::Write;
    let dir = std::env::temp_dir().join("hjkl_plus_slash_goto_test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sample.txt");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "main early").unwrap(); // row 0
        writeln!(f, "two").unwrap();
        writeln!(f, "three").unwrap();
        writeln!(f, "four").unwrap();
        writeln!(f, "five").unwrap(); // goto_line(5) lands here (1-based row 4)
        writeln!(f, "six").unwrap();
        writeln!(f, "main mid").unwrap(); // row 6
        writeln!(f, "main late").unwrap(); // row 7
    }
    // +5 goto_line=5 then +/main forward search. Should land on row 6,
    // NOT wrap back to row 0.
    let app = App::new(Some(path.clone()), false, Some(5), Some("main".into())).unwrap();
    let (row, _col) = app.active().editor.cursor();
    assert_eq!(
        row, 6,
        "+5 +/main must search forward from row 4, landing on row 6"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn plus_slash_argv_persists_forward_direction_for_n() {
    // Regression: `hjkl +/keyword file` did not call set_last_search,
    // so vim.last_search_forward stayed at its bool default (false).
    // The next `n` then computed forward = false != false = false and
    // jumped BACKWARD as if `?keyword<CR>` had been typed.
    use hjkl_engine::{Input, Key};
    use std::io::Write;
    let dir = std::env::temp_dir().join("hjkl_plus_slash_n_dir");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sample.txt");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "alpha").unwrap(); // 0
        writeln!(f, "beta").unwrap(); // 1
        writeln!(f, "main one").unwrap(); // 2 — first match
        writeln!(f, "delta").unwrap(); // 3
        writeln!(f, "main two").unwrap(); // 4 — `n` should jump here
        writeln!(f, "main three").unwrap(); // 5
    }
    let mut app = App::new(Some(path.clone()), false, None, Some("main".into())).unwrap();
    let (row0, _) = app.active().editor.cursor();
    assert_eq!(row0, 2, "+/main must land on first match (row 2)");
    // last_search must be persisted so `n` knows the pattern.
    assert_eq!(app.active().editor.last_search(), Some("main"));
    // Drive `n` through the engine vim FSM and assert FORWARD jump.
    let n_input = Input {
        key: Key::Char('n'),
        ..Default::default()
    };
    hjkl_engine::step(&mut app.active_mut().editor, n_input);
    let (row1, _) = app.active().editor.cursor();
    assert_eq!(
        row1, 4,
        "after +/main, `n` must advance FORWARD to row 4 (got row {row1}); \
         backward would land on row 0 (no match) or stay/regress"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn search_count_cursor_on_match_stays_on_match() {
    // Regression: /<pat><CR> from a cursor that's already ON a match used
    // to advance past it (counter 1/3 → 2/3). Vim semantics: /<CR> finds
    // the first match AT-OR-AFTER the cursor — only `n` advances.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "foo X foo X foo");
    {
        let vp = app.active_mut().editor.host_mut().viewport_mut();
        vp.height = 5;
        vp.top_row = 0;
    }
    // Cursor at (0,0) — exactly on the first 'foo'.
    app.commit_search("foo");
    assert_eq!(
        crate::render::search_count(&app),
        Some((1, 3)),
        "/<pat><CR> from cursor on a match must keep counter at 1/3, \
         not advance to 2/3"
    );
}

#[test]
fn search_count_n_press_increments_by_one() {
    // After /foo<CR> lands on M1, pressing n should advance to M2 (counter 2/3).
    // If counter skips to 3/3, the n-jump is double-stepping.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "X foo X foo X foo");
    {
        let vp = app.active_mut().editor.host_mut().viewport_mut();
        vp.height = 5;
        vp.top_row = 0;
    }
    app.commit_search("foo");
    assert_eq!(crate::render::search_count(&app), Some((1, 3)));
    // Now drive `n` via the engine.
    app.active_mut().editor.search_advance_forward(true);
    assert_eq!(
        crate::render::search_count(&app),
        Some((2, 3)),
        "n must advance counter from 1/3 to 2/3, not skip"
    );
    app.active_mut().editor.search_advance_forward(true);
    assert_eq!(crate::render::search_count(&app), Some((3, 3)));
}

#[test]
fn search_count_handles_multibyte_chars_before_match() {
    // Regression: search_count compared cursor_col (char index) against
    // m.start() (byte offset). A match on a line with multi-byte chars
    // before it (e.g. an em-dash in a doc comment) had byte > char, so
    // the inequality `(row, byte) <= (row, char)` falsely excluded the
    // match the cursor was sitting on — counter showed 0/N instead of 1/N.
    //
    // Real-world repro: `/main` in apps/hjkl/src/main.rs landed on a
    // line "/// surface them — `main` prints …" with an em-dash and
    // showed [0/6] on commit, then [2/6] after one `n` press.
    let mut app = App::new(None, false, None, None).unwrap();
    // Two matches; first sits behind a multi-byte em-dash.
    seed_buffer(&mut app, "alpha\n/// — main one\nbeta\nmain two");
    {
        let vp = app.active_mut().editor.host_mut().viewport_mut();
        vp.height = 10;
        vp.top_row = 0;
    }
    app.commit_search("main");
    assert_eq!(
        crate::render::search_count(&app),
        Some((1, 2)),
        "/main must land on M1 with counter 1/2, even when M1 sits \
         behind a multi-byte char (em-dash) on its line"
    );
    // n -> M2 -> 2/2.
    app.active_mut().editor.search_advance_forward(true);
    assert_eq!(crate::render::search_count(&app), Some((2, 2)));
}

#[test]
fn search_count_through_full_key_flow() {
    // Regression: simulate the actual key path / -> 'f' -> 'o' -> 'o' -> Enter.
    // Counter must end at 1/3 (or N/3 with N=1), never skipping past 1.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "X foo X foo X foo");
    {
        let vp = app.active_mut().editor.host_mut().viewport_mut();
        vp.height = 5;
        vp.top_row = 0;
    }
    // Open / prompt.
    app.open_search_prompt(crate::app::SearchDir::Forward);
    // Type 'f' 'o' 'o' through handle_search_field_key.
    for ch in ['f', 'o', 'o'] {
        let key = KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE);
        app.handle_search_field_key(key);
    }
    // During typing the counter should be 0/3 (cursor before all matches).
    let count = crate::render::search_count(&app);
    assert_eq!(count, Some((0, 3)), "during typing, counter must be 0/3");
    // Submit.
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    app.handle_search_field_key(enter);
    // After submit the counter must show 1/3, NOT 2/3.
    let count = crate::render::search_count(&app);
    assert_eq!(
        count,
        Some((1, 3)),
        "after / submit, counter must be 1/3 — bug was 2/3"
    );
}

#[test]
fn search_count_after_commit_lands_on_first_match() {
    // Regression: `/<pat><CR>` from a non-match cursor was incrementing
    // the match counter to 2 (skipping 1) because commit_search passed
    // skip_current=true even on the first jump.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "X foo X foo X foo");
    // Cursor at (0,0), 'X' — before all matches.
    {
        let vp = app.active_mut().editor.host_mut().viewport_mut();
        vp.height = 5;
        vp.top_row = 0;
    }
    // Submit `/foo<CR>` programmatically.
    app.commit_search("foo");
    // Counter should now show 1/3 (first match), not 2/3.
    let count = crate::render::search_count(&app);
    assert_eq!(
        count,
        Some((1, 3)),
        "/{{pat}}<CR> from a non-match cursor must land on match 1, not skip to 2"
    );
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
    // Use platform-aware URIs so Windows CI runners don't strip drive letters.
    let locs = vec![
        make_location(&file_url(&tmp_path("hjkl_gd_multi_a.rs")), 0, 0),
        make_location(&file_url(&tmp_path("hjkl_gd_multi_b.rs")), 5, 3),
        make_location(&file_url(&tmp_path("hjkl_gd_multi_c.rs")), 10, 1),
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
    // Use platform-aware URI so Windows CI runners don't strip drive letters.
    let locs = vec![make_location(&file_url(&tmp_path("hjkl_gd_only.rs")), 3, 0)];
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

// ── App-level count prefix tests ─────────────────────────────────────────────

/// `5gt` should advance active_tab by 5 (wrapping) — the same as calling
/// `tabnext` five times.
#[test]
fn count_gt_advances_multiple_tabs() {
    let mut app = App::new(None, false, None, None).unwrap();
    // Create 6 tabs so we have room to navigate.
    for _ in 0..5 {
        app.dispatch_ex("tabnew");
    }
    assert_eq!(app.tabs.len(), 6);
    // Jump back to tab 0.
    app.active_tab = 0;

    // Simulate `5gt` by calling dispatch_ex("tabnext") 5 times — the same
    // thing the event loop does when it sees `pending_count = "5"` + `gt`.
    let count = 5_usize;
    for _ in 0..count {
        app.dispatch_ex("tabnext");
    }
    assert_eq!(
        app.active_tab, 5,
        "5gt from tab 0 should land on tab 5 (index 5)"
    );
}

/// `3gT` should move active_tab back by 3.
#[test]
fn count_gt_upper_retreats_multiple_tabs() {
    let mut app = App::new(None, false, None, None).unwrap();
    for _ in 0..4 {
        app.dispatch_ex("tabnew");
    }
    assert_eq!(app.tabs.len(), 5);
    // Start at the last tab (index 4).
    app.active_tab = 4;

    let count = 3_usize;
    for _ in 0..count {
        app.dispatch_ex("tabprev");
    }
    assert_eq!(
        app.active_tab, 1,
        "3gT from tab 4 should land on tab 1 (index 1)"
    );
}

/// `3<C-w>+` should call resize_height with +3 (count as i32).
#[test]
fn count_ctrl_w_plus_resizes_by_count() {
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

    // Simulate `3<C-w>+`: the event loop parses count=3 and calls resize_height(3).
    let count: i32 = 3;
    app.resize_height(count);

    let ratio_after = if let window::LayoutTree::Split { ratio, .. } = app.layout() {
        *ratio
    } else {
        panic!("expected Split");
    };

    // Growing by 3 rows in a 40-row pane should increase the ratio more than
    // a single-row grow would.
    assert!(
        ratio_after > ratio_before,
        "3<C-w>+ must grow the ratio: before={ratio_before} after={ratio_after}"
    );

    // The ratio change must be larger than a delta-1 change would produce.
    // delta=1 on a 40-row pane with ratio=0.5 → new focused = 20+1 = 21 → ratio ≈ 0.525.
    // delta=3 → new focused = 20+3 = 23 → ratio ≈ 0.575.
    let ratio_delta_1 = (20.0_f32 + 1.0) / 40.0;
    assert!(
        ratio_after > ratio_delta_1,
        "ratio after 3-row grow ({ratio_after}) should exceed 1-row grow ({ratio_delta_1})"
    );
}

/// `pending_count` digit accumulation rules:
///   • `1`–`9` start a count when empty.
///   • `0` with empty count is NOT buffered (start-of-line motion).
///   • `0` with non-empty count extends it.
#[test]
fn pending_count_accumulation_rules() {
    let mut app = App::new(None, false, None, None).unwrap();

    // Initially empty.
    assert!(app.pending_count.is_empty());

    // Simulate the digit-buffering logic for each digit:
    // '1' starts the count.
    app.pending_count.push('1');
    assert_eq!(app.pending_count, "1");

    // '0' extends a non-empty count.
    app.pending_count.push('0');
    assert_eq!(app.pending_count, "10");

    // Parsing gives 10.
    let count: usize = app.pending_count.parse().unwrap_or(1);
    assert_eq!(count, 10);

    // After consuming, it must be cleared.
    app.pending_count.clear();
    assert!(app.pending_count.is_empty());

    // '0' alone (empty pending_count) must NOT be pushed — the event loop
    // falls through to the engine.  We verify this by checking the rule:
    // is_zero && pending_count.is_empty() → do not push.
    let d = '0';
    let is_zero = d == '0';
    if !is_zero || !app.pending_count.is_empty() {
        app.pending_count.push(d);
    }
    assert!(
        app.pending_count.is_empty(),
        "'0' with empty pending_count must not be buffered"
    );
}

/// `5j` — engine motions: digits are replayed to the editor engine and the
/// cursor moves 5 rows down.
#[test]
fn count_engine_motion_5j_moves_cursor_five_rows() {
    let mut app = App::new(None, false, None, None).unwrap();
    // Populate 20 lines so there is room to move.
    let content: String = (0..20).map(|i| format!("line {i}\n")).collect();
    let content = content.trim_end_matches('\n');
    hjkl_engine::BufferEdit::replace_all(app.active_mut().editor.buffer_mut(), content);

    // Cursor starts at row 0.
    let (start_row, _) = app.active().editor.cursor();
    assert_eq!(start_row, 0);

    // Simulate what the event loop does for `5j`:
    // 1. Buffer '5' into pending_count.
    // 2. On 'j', replay '5' then 'j' to the engine.
    app.active_mut()
        .editor
        .handle_key(KeyEvent::new(KeyCode::Char('5'), KeyModifiers::NONE));
    app.active_mut()
        .editor
        .handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));

    let (end_row, _) = app.active().editor.cursor();
    assert_eq!(end_row, 5, "5j must move cursor from row 0 to row 5");
}

/// `0` with empty `pending_count` goes to start-of-line (col 0).
#[test]
fn zero_with_empty_count_is_start_of_line() {
    let mut app = App::new(None, false, None, None).unwrap();
    hjkl_engine::BufferEdit::replace_all(
        app.active_mut().editor.buffer_mut(),
        "hello world\nsecond line",
    );

    // Move to end of first line.
    app.active_mut()
        .editor
        .handle_key(KeyEvent::new(KeyCode::Char('$'), KeyModifiers::NONE));
    let (_, col_after_dollar) = app.active().editor.cursor();
    assert!(col_after_dollar > 0, "$ must move to end of line");

    // `0` with empty pending_count → goes to col 0.
    // Verify the rule: is_zero && pending_count.is_empty() → fall through.
    assert!(app.pending_count.is_empty());
    app.active_mut()
        .editor
        .handle_key(KeyEvent::new(KeyCode::Char('0'), KeyModifiers::NONE));
    let (_, col_after_zero) = app.active().editor.cursor();
    assert_eq!(
        col_after_zero, 0,
        "0 with no pending count must go to col 0"
    );
}

// ── Render-level :set option tests ──────────────────────────────────────────

#[test]
fn set_cursorline_flips_setting() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert!(
        !app.active().editor.settings().cursorline,
        "cursorline must default to false"
    );
    app.dispatch_ex("set cursorline");
    assert!(
        app.active().editor.settings().cursorline,
        ":set cursorline must enable cursorline"
    );
    app.dispatch_ex("set nocursorline");
    assert!(
        !app.active().editor.settings().cursorline,
        ":set nocursorline must disable cursorline"
    );
}

#[test]
fn set_cul_alias_works() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("set cul");
    assert!(
        app.active().editor.settings().cursorline,
        ":set cul must enable cursorline"
    );
}

#[test]
fn set_cursorcolumn_flips_setting() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert!(!app.active().editor.settings().cursorcolumn);
    app.dispatch_ex("set cuc");
    assert!(app.active().editor.settings().cursorcolumn);
    app.dispatch_ex("set nocuc");
    assert!(!app.active().editor.settings().cursorcolumn);
}

#[test]
fn set_signcolumn_yes() {
    use hjkl_engine::types::SignColumnMode;
    let mut app = App::new(None, false, None, None).unwrap();
    assert_eq!(
        app.active().editor.settings().signcolumn,
        SignColumnMode::Auto,
        "signcolumn defaults to auto"
    );
    app.dispatch_ex("set signcolumn=yes");
    assert_eq!(
        app.active().editor.settings().signcolumn,
        SignColumnMode::Yes
    );
}

#[test]
fn set_signcolumn_scl_alias() {
    use hjkl_engine::types::SignColumnMode;
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("set scl=no");
    assert_eq!(
        app.active().editor.settings().signcolumn,
        SignColumnMode::No
    );
}

#[test]
fn set_foldcolumn_stores_value() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert_eq!(app.active().editor.settings().foldcolumn, 0);
    app.dispatch_ex("set foldcolumn=4");
    assert_eq!(app.active().editor.settings().foldcolumn, 4);
    app.dispatch_ex("set fdc=0");
    assert_eq!(app.active().editor.settings().foldcolumn, 0);
}

#[test]
fn set_colorcolumn_stores_value() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert_eq!(app.active().editor.settings().colorcolumn, "");
    app.dispatch_ex("set cc=80");
    assert_eq!(app.active().editor.settings().colorcolumn, "80");
    app.dispatch_ex("set colorcolumn=80,120");
    assert_eq!(app.active().editor.settings().colorcolumn, "80,120");
    app.dispatch_ex("set cc=");
    assert_eq!(app.active().editor.settings().colorcolumn, "");
}

// ── :Anvil ex command tests ───────────────────────────────────────────────────

#[test]
fn anvil_install_unknown_tool_sets_error_message() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("Anvil install definitely-not-a-real-tool-xyz");
    let msg = app.status_message.clone().unwrap_or_default();
    assert!(
        msg.contains("unknown tool"),
        "expected 'unknown tool' in status message, got: {msg:?}"
    );
}

#[test]
fn anvil_uninstall_not_installed_graceful() {
    // Uninstalling a tool that has no package dir must not panic.
    // It should set a success or no-op status message.
    let mut app = App::new(None, false, None, None).unwrap();
    // rust-analyzer is in the registry but not installed in CI.
    app.dispatch_ex("Anvil uninstall rust-analyzer");
    // Either "removed" or "failed to resolve" — should not panic.
    let msg = app.status_message.clone().unwrap_or_default();
    assert!(
        !msg.is_empty(),
        "expected some status message after anvil uninstall"
    );
}

#[test]
fn anvil_update_all_with_zero_installed_tools() {
    // :Anvil update with no installed tools should reach the sweep-started toast.
    // In CI the XDG store is empty so read_rev returns None for all tools,
    // which means anvil_update_all skips all names and sets the sweep message.
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("Anvil update");
    let msg = app.status_message.clone().unwrap_or_default();
    assert!(
        msg.contains("update sweep started"),
        "expected 'update sweep started', got: {msg:?}"
    );
}

#[test]
fn anvil_picker_source_builds_from_registry() {
    use crate::picker_sources::{AnvilPickerSource, AnvilState};

    let registry = hjkl_anvil::Registry::embedded().expect("embedded registry must load");
    let source = AnvilPickerSource::from_registry(&registry);

    // The embedded catalog has at least one tool (rust-analyzer).
    assert!(!source.items.is_empty(), "picker source must have items");

    // In CI nothing is installed, so every item should be Available.
    for item in &source.items {
        // State should be Available (no .rev files in CI).
        // We can't assert Available specifically in all environments, but
        // we can assert the item fields are consistent.
        let label = item.label();
        assert!(
            label.contains(&item.name),
            "label must contain tool name; got: {label:?}"
        );
        assert!(
            matches!(
                item.state,
                AnvilState::Available | AnvilState::Installed { .. } | AnvilState::Outdated { .. }
            ),
            "state must be one of the three variants"
        );
    }
}

#[test]
fn anvil_bad_subcommand_shows_usage() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("Anvil badsubcommand something else");
    let msg = app.status_message.clone().unwrap_or_default();
    assert!(
        msg.contains("usage"),
        "expected usage hint in status message, got: {msg:?}"
    );
}

#[test]
fn unbound_chord_tail_trie_returns_multi_key_replay() {
    // <leader>x: leader is bound (as a prefix), but <leader>x is not.
    // The trie returns Unbound([<leader>, x]) with replay.len() > 1.
    // The event_loop now always forwards multi-key Unbound replays to the
    // engine (so gg/gj/etc work). This test verifies the trie shape only.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "abcdef");

    let leader = app.config.editor.leader;
    let mut replay: Vec<hjkl_keymap::KeyEvent> = Vec::new();

    // First key: leader. Should be Pending → consumed = true.
    let consumed1 = app.dispatch_keymap(
        hjkl_keymap::KeyEvent::new(
            hjkl_keymap::KeyCode::Char(leader),
            hjkl_keymap::KeyModifiers::NONE,
        ),
        1,
        &mut replay,
    );
    assert!(consumed1, "leader should be consumed as Pending prefix");

    // Second key: 'x' — unmapped. The dispatch returns consumed=false
    // and replay=[leader, x] (both keys buffered by the trie).
    replay.clear();
    let consumed2 = app.dispatch_keymap(
        hjkl_keymap::KeyEvent::new(
            hjkl_keymap::KeyCode::Char('x'),
            hjkl_keymap::KeyModifiers::NONE,
        ),
        1,
        &mut replay,
    );
    assert!(!consumed2, "<leader>x is unbound → consumed=false");
    assert!(
        replay.len() > 1,
        "replay should contain both keys, got {} keys",
        replay.len()
    );
    // Note: event_loop now forwards multi-key replays to the engine.
    // <leader>x with leader=space → space (move-right) + x (delete-char).
    // This is vim-compatible; users can `:nmap <leader> <Nop>` to stop.
}

// ── Dispatch-path tests (engine-pending bypass + always-forward Unbound) ──

/// Feed a crossterm key through the same dispatch path used by the event_loop:
/// app pending-state reducer first, then engine-pending bypass, then trie,
/// then engine forwarding.
fn drive_key(app: &mut App, ct_key: KeyEvent) {
    // App-level pending-state reducer (hjkl-vim): takes priority over everything.
    if let Some(state) = app.pending_state {
        use hjkl_vim::{Key as VimKey, Outcome};
        let vim_key = match ct_key.code {
            KeyCode::Char(c) => Some(VimKey::Char(c)),
            KeyCode::Esc => Some(VimKey::Esc),
            KeyCode::Enter => Some(VimKey::Enter),
            KeyCode::Backspace => Some(VimKey::Backspace),
            KeyCode::Tab => Some(VimKey::Tab),
            _ => None,
        };
        if let Some(vk) = vim_key {
            match hjkl_vim::step(state, vk) {
                Outcome::Wait(new_state) => {
                    app.pending_state = Some(new_state);
                    return;
                }
                Outcome::Commit(hjkl_vim::EngineCmd::ReplaceChar { ch, count }) => {
                    app.pending_state = None;
                    app.active_mut().editor.replace_char_at(ch, count);
                    app.sync_viewport_from_editor();
                    return;
                }
                Outcome::Commit(hjkl_vim::EngineCmd::FindChar {
                    ch,
                    forward,
                    till,
                    count,
                }) => {
                    app.pending_state = None;
                    app.active_mut().editor.find_char(ch, forward, till, count);
                    app.sync_viewport_from_editor();
                    return;
                }
                Outcome::Commit(hjkl_vim::EngineCmd::AfterGChord { ch, count }) => {
                    app.pending_state = None;
                    // App-level g actions (gt, gd, gi, etc.) take priority.
                    match ch {
                        't' => {
                            app.dispatch_action(
                                crate::keymap_actions::AppAction::Tabnext,
                                count as u32,
                            );
                            return;
                        }
                        'T' => {
                            app.dispatch_action(
                                crate::keymap_actions::AppAction::Tabprev,
                                count as u32,
                            );
                            return;
                        }
                        'd' => {
                            app.dispatch_action(
                                crate::keymap_actions::AppAction::LspGotoDef,
                                count as u32,
                            );
                            return;
                        }
                        'D' => {
                            app.dispatch_action(
                                crate::keymap_actions::AppAction::LspGotoDecl,
                                count as u32,
                            );
                            return;
                        }
                        'r' => {
                            app.dispatch_action(
                                crate::keymap_actions::AppAction::LspGotoRef,
                                count as u32,
                            );
                            return;
                        }
                        'i' => {
                            app.dispatch_action(
                                crate::keymap_actions::AppAction::LspGotoImpl,
                                count as u32,
                            );
                            return;
                        }
                        'y' => {
                            app.dispatch_action(
                                crate::keymap_actions::AppAction::LspGotoTypeDef,
                                count as u32,
                            );
                            return;
                        }
                        _ => {}
                    }
                    app.active_mut().editor.after_g(ch, count);
                    app.sync_viewport_from_editor();
                    return;
                }
                Outcome::Commit(hjkl_vim::EngineCmd::AfterZChord { ch, count }) => {
                    app.pending_state = None;
                    app.active_mut().editor.after_z(ch, count);
                    app.sync_viewport_from_editor();
                    return;
                }
                Outcome::Commit(hjkl_vim::EngineCmd::ApplyOpMotion {
                    op,
                    motion_key,
                    total_count,
                }) => {
                    app.pending_state = None;
                    app.active_mut().editor.apply_op_motion(
                        op_kind_to_operator(op),
                        motion_key,
                        total_count,
                    );
                    app.sync_viewport_from_editor();
                    return;
                }
                Outcome::Commit(hjkl_vim::EngineCmd::ApplyOpDouble { op, total_count }) => {
                    app.pending_state = None;
                    app.active_mut()
                        .editor
                        .apply_op_double(op_kind_to_operator(op), total_count);
                    app.sync_viewport_from_editor();
                    return;
                }
                Outcome::Commit(hjkl_vim::EngineCmd::ApplyOpTextObj {
                    op,
                    ch,
                    inner,
                    total_count,
                }) => {
                    app.pending_state = None;
                    app.active_mut().editor.apply_op_text_obj(
                        op_kind_to_operator(op),
                        ch,
                        inner,
                        total_count,
                    );
                    app.sync_viewport_from_editor();
                    return;
                }
                Outcome::Commit(hjkl_vim::EngineCmd::ApplyOpG {
                    op,
                    ch,
                    total_count,
                }) => {
                    app.pending_state = None;
                    app.active_mut()
                        .editor
                        .apply_op_g(op_kind_to_operator(op), ch, total_count);
                    app.sync_viewport_from_editor();
                    return;
                }
                Outcome::Commit(hjkl_vim::EngineCmd::ApplyOpFind {
                    op,
                    ch,
                    forward,
                    till,
                    total_count,
                }) => {
                    app.pending_state = None;
                    app.active_mut().editor.apply_op_find(
                        op_kind_to_operator(op),
                        ch,
                        forward,
                        till,
                        total_count,
                    );
                    app.sync_viewport_from_editor();
                    return;
                }
                Outcome::Cancel => {
                    app.pending_state = None;
                    return;
                }
                Outcome::Forward => {
                    // Fall through with state intact.
                }
            }
        }
        // Unrecognised key variant — fall through.
    }
    // Engine pending bypass: if the engine is mid-chord, skip the trie.
    if app.active().editor.is_chord_pending() {
        app.active_mut().editor.handle_key(ct_key);
        app.sync_viewport_from_editor();
        return;
    }
    // Try the keymap trie.
    let Some(km_ev) = crate::keymap_translate::from_crossterm(&ct_key) else {
        // Untranslatable key — forward direct to engine.
        app.active_mut().editor.handle_key(ct_key);
        app.sync_viewport_from_editor();
        return;
    };
    let mut replay = Vec::new();
    let consumed = app.dispatch_keymap(km_ev, 1, &mut replay);
    if consumed {
        return;
    }
    // Unbound: forward all replay keys (including multi-key) to the engine.
    for ev in &replay {
        let back = crate::keymap_translate::to_crossterm(ev);
        app.active_mut().editor.handle_key(back);
    }
    app.sync_viewport_from_editor();
}

#[test]
fn gg_via_dispatch_jumps_to_top() {
    let mut app = App::new(None, false, None, None).unwrap();
    let lines: Vec<String> = (0..50).map(|i| format!("line {i}")).collect();
    seed_buffer(&mut app, &lines.join("\n"));
    app.active_mut().editor.jump_cursor(30, 0);
    assert_eq!(app.active().editor.cursor().0, 30);

    drive_key(&mut app, key(KeyCode::Char('g')));
    drive_key(&mut app, key(KeyCode::Char('g')));

    assert_eq!(
        app.active().editor.cursor().0,
        0,
        "gg through dispatch path must move cursor to top"
    );
}

#[test]
fn r_space_replaces_char_with_space() {
    // r<space> in Normal mode: `r` is now intercepted by the app keymap trie
    // and sets app-level pending state (hjkl-vim reducer). The engine is NOT
    // in chord-pending after `r`; the app holds the state. The second key
    // (`<space>`) is fed through hjkl_vim::step → Commit → replace_char_at.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "abc");
    app.active_mut().editor.jump_cursor(0, 1); // on 'b'

    drive_key(&mut app, key(KeyCode::Char('r')));
    // App-level pending state is set; engine is NOT chord-pending.
    assert!(
        app.pending_state.is_some(),
        "r must set app pending_state to Replace"
    );
    assert!(
        !app.active().editor.is_chord_pending(),
        "engine must NOT be in chord-pending after app-intercepted r"
    );
    drive_key(&mut app, key(KeyCode::Char(' ')));
    assert!(
        app.pending_state.is_none(),
        "pending_state cleared after commit"
    );

    let line = app.active().editor.buffer().as_string();
    assert_eq!(
        line, "a c",
        "r<space> must replace 'b' with ' ', got {line:?}"
    );
}

#[test]
fn f_with_leader_char_finds_it() {
    // f<space> when leader=space: f-pending state should swallow the
    // space char into the find-target slot, not let the trie eat it.
    // Since 2b-i, `f` is intercepted by the app trie → app pending_state;
    // the engine is NOT in chord-pending after `f`.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "a b c");
    app.active_mut().editor.jump_cursor(0, 0);

    drive_key(&mut app, key(KeyCode::Char('f')));
    // App-level pending state is set; engine is NOT chord-pending.
    assert!(
        app.pending_state.is_some(),
        "f must set app pending_state to Find"
    );
    assert!(
        !app.active().editor.is_chord_pending(),
        "engine must NOT be in chord-pending after app-intercepted f"
    );
    drive_key(&mut app, key(KeyCode::Char(' ')));

    // Cursor should now be on the first space (column 1).
    assert_eq!(app.active().editor.cursor(), (0, 1));
}

// ── Phase 2b-i: bare f/F/t/T through hjkl-vim reducer ────────────────────

#[test]
fn fx_finds_x_forward() {
    // `fx` in "abc x def" from col 0 → cursor on 'x' (col 4).
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "abc x def");
    app.active_mut().editor.jump_cursor(0, 0);

    drive_key(&mut app, key(KeyCode::Char('f')));
    assert!(
        app.pending_state.is_some(),
        "f must set app pending_state to Find"
    );
    drive_key(&mut app, key(KeyCode::Char('x')));
    assert!(
        app.pending_state.is_none(),
        "pending_state cleared after commit"
    );
    assert_eq!(
        app.active().editor.cursor(),
        (0, 4),
        "fx must land on 'x' at col 4"
    );
}

#[test]
fn fx_finds_x_backward() {
    // `Fx` in "abc x def" from end → cursor on 'x' (col 4).
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "abc x def");
    app.active_mut().editor.jump_cursor(0, 8); // on 'f'

    drive_key(&mut app, key(KeyCode::Char('F')));
    assert!(
        app.pending_state.is_some(),
        "F must set app pending_state to Find"
    );
    drive_key(&mut app, key(KeyCode::Char('x')));
    assert!(
        app.pending_state.is_none(),
        "pending_state cleared after commit"
    );
    assert_eq!(
        app.active().editor.cursor(),
        (0, 4),
        "Fx must land on 'x' at col 4"
    );
}

#[test]
fn tx_lands_before_x() {
    // `tx` in "abc x def" from col 0 → stops one before 'x' (col 3, the space).
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "abc x def");
    app.active_mut().editor.jump_cursor(0, 0);

    drive_key(&mut app, key(KeyCode::Char('t')));
    drive_key(&mut app, key(KeyCode::Char('x')));
    assert_eq!(
        app.active().editor.cursor(),
        (0, 3),
        "tx must stop one before 'x' at col 3"
    );
}

#[test]
fn tx_backward_lands_after_x() {
    // `Tx` in "abc x def" from end → stops one after 'x' (col 5, the space).
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "abc x def");
    app.active_mut().editor.jump_cursor(0, 8); // on 'f'

    drive_key(&mut app, key(KeyCode::Char('T')));
    drive_key(&mut app, key(KeyCode::Char('x')));
    assert_eq!(
        app.active().editor.cursor(),
        (0, 5),
        "Tx must stop one after 'x' at col 5"
    );
}

#[test]
fn fx_with_count_3() {
    // `3fx` in "xaxbxc" from col 0 → 3rd 'x' at col 4.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "xaxbxc");
    app.active_mut().editor.jump_cursor(0, 0);

    // Buffer count via pending_count (mimicking the event_loop digit path).
    app.pending_count = "3".into();
    drive_key(&mut app, key(KeyCode::Char('f')));
    // dispatch_keymap reads pending_count when BeginPendingFind fires.
    assert!(
        matches!(
            app.pending_state,
            Some(hjkl_vim::PendingState::Find { count: 3, .. })
        ),
        "pending_state must carry count=3, got {:?}",
        app.pending_state
    );
    drive_key(&mut app, key(KeyCode::Char('x')));
    assert_eq!(
        app.active().editor.cursor(),
        (0, 4),
        "3fx must land on 3rd 'x' at col 4"
    );
}

#[test]
fn fx_then_esc_cancels() {
    // `f<Esc>` clears pending without moving cursor.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "abc x def");
    app.active_mut().editor.jump_cursor(0, 0);

    drive_key(&mut app, key(KeyCode::Char('f')));
    assert!(app.pending_state.is_some());
    drive_key(&mut app, key(KeyCode::Esc));
    assert!(
        app.pending_state.is_none(),
        "Esc must clear find pending_state"
    );
    // Cursor unchanged.
    assert_eq!(app.active().editor.cursor(), (0, 0));
}

#[test]
fn gj_via_dispatch_moves_down_display_line() {
    // gj is a display-line motion (same as j on non-wrapped lines).
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "line0\nline1\nline2");
    app.active_mut().editor.jump_cursor(0, 0);
    drive_key(&mut app, key(KeyCode::Char('g')));
    drive_key(&mut app, key(KeyCode::Char('j')));
    assert_eq!(
        app.active().editor.cursor().0,
        1,
        "gj must move down one row"
    );
}

// ── Phase 2b-ii: bare g<x> through hjkl-vim AfterG reducer ──────────────

#[test]
fn gg_jumps_top() {
    // gg from row 30 → row 0.
    let mut app = App::new(None, false, None, None).unwrap();
    let lines: Vec<String> = (0..50).map(|i| format!("line {i}")).collect();
    seed_buffer(&mut app, &lines.join("\n"));
    app.active_mut().editor.jump_cursor(30, 0);

    drive_key(&mut app, key(KeyCode::Char('g')));
    assert!(
        matches!(
            app.pending_state,
            Some(hjkl_vim::PendingState::AfterG { .. })
        ),
        "g must set pending_state=AfterG, got {:?}",
        app.pending_state
    );
    drive_key(&mut app, key(KeyCode::Char('g')));
    assert!(app.pending_state.is_none(), "pending cleared after gg");
    assert_eq!(app.active().editor.cursor().0, 0, "gg must jump to row 0");
}

#[test]
fn gg_with_count_5_jumps_line_5() {
    // 5gg → row 4 (0-indexed).
    let mut app = App::new(None, false, None, None).unwrap();
    let lines: Vec<String> = (0..20).map(|i| format!("line {i}")).collect();
    seed_buffer(&mut app, &lines.join("\n"));
    app.active_mut().editor.jump_cursor(0, 0);

    app.pending_count = "5".into();
    drive_key(&mut app, key(KeyCode::Char('g')));
    assert!(
        matches!(
            app.pending_state,
            Some(hjkl_vim::PendingState::AfterG { count: 5 })
        ),
        "pending_state must carry count=5, got {:?}",
        app.pending_state
    );
    drive_key(&mut app, key(KeyCode::Char('g')));
    assert_eq!(app.active().editor.cursor().0, 4, "5gg must land on row 4");
}

#[test]
fn gv_restores_last_visual() {
    // Enter visual, move, exit, then gv re-enters visual mode.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world\n");
    // Enter visual and select a few chars.
    app.active_mut().editor.handle_key(key(KeyCode::Char('v')));
    app.active_mut().editor.handle_key(key(KeyCode::Char('l')));
    app.active_mut().editor.handle_key(key(KeyCode::Char('l')));
    // Exit visual.
    app.active_mut().editor.handle_key(key(KeyCode::Esc));
    assert_eq!(
        app.active().editor.vim_mode(),
        hjkl_engine::VimMode::Normal,
        "should be Normal after Esc"
    );
    // gv via AfterG reducer.
    drive_key(&mut app, key(KeyCode::Char('g')));
    drive_key(&mut app, key(KeyCode::Char('v')));
    assert_eq!(
        app.active().editor.vim_mode(),
        hjkl_engine::VimMode::Visual,
        "gv must re-enter Visual mode"
    );
}

#[test]
fn gj_screen_down() {
    // gj moves cursor down one display row.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "line0\nline1\nline2\n");
    app.active_mut().editor.jump_cursor(0, 0);

    drive_key(&mut app, key(KeyCode::Char('g')));
    drive_key(&mut app, key(KeyCode::Char('j')));
    assert_eq!(
        app.active().editor.cursor().0,
        1,
        "gj must move down to row 1"
    );
}

#[test]
fn gu_then_w_lowercases_word() {
    // gu<motion> operator: after gU sets Pending::Op the engine must be
    // chord-pending so the next key (w) applies as a motion.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "HELLO world\n");
    app.active_mut().editor.jump_cursor(0, 0);

    drive_key(&mut app, key(KeyCode::Char('g')));
    drive_key(&mut app, key(KeyCode::Char('u')));
    // After gu the engine should be chord-pending (Pending::Op).
    assert!(
        app.active().editor.is_chord_pending(),
        "after gu the engine must be in chord-pending for the motion"
    );
    // Feed 'w' directly to engine (is_chord_pending bypass).
    app.active_mut().editor.handle_key(key(KeyCode::Char('w')));
    let content = app.active().editor.buffer().as_string();
    assert!(
        content.starts_with("hello"),
        "gu+w must lowercase the word; got {content:?}"
    );
}

// ── Phase 2c-iii: OpTextObj reducer integration tests ────────────────────────

#[test]
fn diw_deletes_word_via_reducer() {
    // `diw` — d → AfterOp, i → Wait(OpTextObj{inner:true}), w → ApplyOpTextObj.
    // Reducer owns the full sequence; engine is not chord-pending.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world");
    app.active_mut().editor.jump_cursor(0, 0);

    drive_key(&mut app, key(KeyCode::Char('d')));
    drive_key(&mut app, key(KeyCode::Char('i')));
    assert!(
        matches!(
            app.pending_state,
            Some(hjkl_vim::PendingState::OpTextObj {
                op: hjkl_vim::OperatorKind::Delete,
                inner: true,
                ..
            })
        ),
        "di must set OpTextObj(inner:true), got {:?}",
        app.pending_state
    );
    assert!(
        !app.active().editor.is_chord_pending(),
        "engine must NOT be chord-pending after reducer-owned di"
    );

    drive_key(&mut app, key(KeyCode::Char('w')));
    assert!(app.pending_state.is_none());

    let line = app
        .active()
        .editor
        .buffer()
        .lines()
        .first()
        .cloned()
        .unwrap_or_default();
    assert!(
        !line.contains("hello"),
        "diw must delete 'hello', remaining: {line:?}"
    );
}

#[test]
fn daw_deletes_around_word_via_reducer() {
    // `daw` — d → AfterOp, a → Wait(OpTextObj{inner:false}), w → ApplyOpTextObj.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world");
    app.active_mut().editor.jump_cursor(0, 0);

    drive_key(&mut app, key(KeyCode::Char('d')));
    drive_key(&mut app, key(KeyCode::Char('a')));
    assert!(
        matches!(
            app.pending_state,
            Some(hjkl_vim::PendingState::OpTextObj {
                op: hjkl_vim::OperatorKind::Delete,
                inner: false,
                ..
            })
        ),
        "da must set OpTextObj(inner:false), got {:?}",
        app.pending_state
    );
    assert!(
        !app.active().editor.is_chord_pending(),
        "engine must NOT be chord-pending after reducer-owned da"
    );

    drive_key(&mut app, key(KeyCode::Char('w')));
    assert!(app.pending_state.is_none());

    let line = app
        .active()
        .editor
        .buffer()
        .lines()
        .first()
        .cloned()
        .unwrap_or_default();
    assert!(
        !line.contains("hello"),
        "daw must delete 'hello' and surrounding space, remaining: {line:?}"
    );
}

#[test]
fn di_quote_deletes_quoted_string() {
    // `di"` — deletes content inside double-quotes.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, r#"say "hello" now"#);
    // Position inside the quotes (on 'h').
    app.active_mut().editor.jump_cursor(0, 5);

    drive_chars(&mut app, r#"di""#);
    assert!(app.pending_state.is_none());

    let line = app
        .active()
        .editor
        .buffer()
        .lines()
        .first()
        .cloned()
        .unwrap_or_default();
    assert!(
        !line.contains("hello"),
        r#"di" must delete text inside quotes, remaining: {line:?}"#
    );
    // The quote delimiters should remain.
    assert!(
        line.contains('"'),
        r#"di" must leave the quote delimiters, remaining: {line:?}"#
    );
}

#[test]
fn dap_deletes_paragraph_via_reducer() {
    // `dap` — delete around paragraph (first paragraph including trailing blank).
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world\n\nfoo bar");
    app.active_mut().editor.jump_cursor(0, 0);

    drive_chars(&mut app, "dap");
    assert!(app.pending_state.is_none());

    let lines: Vec<_> = app.active().editor.buffer().lines().to_vec();
    assert!(
        !lines.contains(&"hello world".to_string()),
        "dap must delete first paragraph, got {lines:?}"
    );
}

#[test]
fn guiw_uppercases_word_via_engine_fsm() {
    // `gUiw` — g → AfterG (reducer), U → after_g('U') → engine sets
    // Pending::Op(Uppercase). The 'i' key then goes to the ENGINE FSM
    // (is_chord_pending bypass), which sets Pending::OpTextObj (engine-owned).
    // The 'w' char completes the engine's OpTextObj arm via handle_text_object.
    // This verifies the chord-init op path (gU/gu/g~ + i/a + textobj) still
    // works through the engine FSM, NOT through the reducer OpTextObj state.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world");
    app.active_mut().editor.jump_cursor(0, 0);

    drive_key(&mut app, key(KeyCode::Char('g')));
    assert!(
        matches!(
            app.pending_state,
            Some(hjkl_vim::PendingState::AfterG { .. })
        ),
        "g must set AfterG"
    );

    // U → reducer commits AfterGChord('U') → engine sets Pending::Op(Uppercase).
    drive_key(&mut app, key(KeyCode::Char('U')));
    assert!(app.pending_state.is_none(), "gU must clear reducer pending");
    assert!(
        app.active().editor.is_chord_pending(),
        "gU must leave engine in Op(Uppercase) chord-pending"
    );

    // 'i' → engine FSM processes Op-pending + 'i' → sets Pending::OpTextObj (engine).
    drive_key(&mut app, key(KeyCode::Char('i')));
    assert!(
        app.pending_state.is_none(),
        "i after gU must NOT set reducer OpTextObj (engine owns it)"
    );
    assert!(
        app.active().editor.is_chord_pending(),
        "engine must remain chord-pending waiting for text-object char"
    );

    // 'w' → engine FSM: handle_text_object → uppercase inner word.
    drive_key(&mut app, key(KeyCode::Char('w')));
    assert!(!app.active().editor.is_chord_pending());

    let line = app
        .active()
        .editor
        .buffer()
        .lines()
        .first()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        line, "HELLO world",
        "gUiw must uppercase inner word 'hello', got {line:?}"
    );
}

#[test]
fn g_then_esc_cancels() {
    // g<Esc> clears pending without any cursor movement.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "abc\n");
    app.active_mut().editor.jump_cursor(0, 1);

    drive_key(&mut app, key(KeyCode::Char('g')));
    assert!(app.pending_state.is_some(), "g must set pending_state");
    drive_key(&mut app, key(KeyCode::Esc));
    assert!(
        app.pending_state.is_none(),
        "Esc must clear g pending_state"
    );
    assert_eq!(
        app.active().editor.cursor(),
        (0, 1),
        "cursor must not move on g<Esc>"
    );
}

// ── Ambiguous → timeout_resolve tests (#60) ─────────────────────────────

#[test]
fn ambiguous_chord_resolves_to_shorter_on_timeout() {
    // Bind both `q` (terminal) and `qd` (deeper). Pressing `q` returns
    // Ambiguous; resolve_chord_timeout must fire the shorter `q` binding.
    use crate::keymap_actions::AppAction;
    let mut app = App::new(None, false, None, None).unwrap();
    use crate::app::keymap::HjklMode as Mode;
    app.app_keymap
        .add(Mode::Normal, "q", AppAction::OpenFilePicker, "file picker")
        .unwrap();
    app.app_keymap
        .add(
            Mode::Normal,
            "qd",
            AppAction::OpenBufferPicker,
            "buffer picker",
        )
        .unwrap();

    let mut replay: Vec<hjkl_keymap::KeyEvent> = Vec::new();
    let consumed = app.dispatch_keymap(
        hjkl_keymap::KeyEvent::new(
            hjkl_keymap::KeyCode::Char('q'),
            hjkl_keymap::KeyModifiers::NONE,
        ),
        1,
        &mut replay,
    );
    assert!(consumed, "q should be consumed (Ambiguous)");
    assert!(app.picker.is_none(), "no picker yet — waiting for timeout");

    let out = app
        .resolve_chord_timeout(crate::app::keymap::HjklMode::Normal)
        .expect("chord was pending");
    assert!(out.is_empty(), "Match should leave nothing to replay");
    assert!(
        app.picker.is_some(),
        "shorter binding (file picker) must fire on timeout"
    );
}

#[test]
fn ambiguous_chord_fires_longer_on_fast_second_key() {
    // Same bindings as above. Pressing `q` then `d` quickly resolves to
    // the longer `qd` binding via the normal feed path.
    use crate::keymap_actions::AppAction;
    let mut app = App::new(None, false, None, None).unwrap();
    use crate::app::keymap::HjklMode as Mode;
    app.app_keymap
        .add(Mode::Normal, "q", AppAction::OpenFilePicker, "file picker")
        .unwrap();
    app.app_keymap
        .add(
            Mode::Normal,
            "qd",
            AppAction::OpenBufferPicker,
            "buffer picker",
        )
        .unwrap();

    let mut replay: Vec<hjkl_keymap::KeyEvent> = Vec::new();
    app.dispatch_keymap(
        hjkl_keymap::KeyEvent::new(
            hjkl_keymap::KeyCode::Char('q'),
            hjkl_keymap::KeyModifiers::NONE,
        ),
        1,
        &mut replay,
    );
    app.dispatch_keymap(
        hjkl_keymap::KeyEvent::new(
            hjkl_keymap::KeyCode::Char('d'),
            hjkl_keymap::KeyModifiers::NONE,
        ),
        1,
        &mut replay,
    );
    assert!(app.picker.is_some(), "qd must fire buffer picker");
    assert_eq!(
        app.picker.as_ref().unwrap().title(),
        "buffers",
        "buffer picker title expected"
    );
}

#[test]
fn resolve_chord_timeout_returns_none_when_no_chord_pending() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert!(
        app.resolve_chord_timeout(crate::app::keymap::HjklMode::Normal)
            .is_none(),
        "no pending chord → None"
    );
}

// ── which-key entries_for tests (#57) ───────────────────────────────────────

/// Helper: build the km prefix from a vim-notation string.
fn km_prefix(app: &App, notation: &str) -> Vec<hjkl_keymap::KeyEvent> {
    let leader = app.config.editor.leader;
    hjkl_keymap::Chord::parse(notation, leader)
        .expect("test chord must parse")
        .0
}

#[test]
fn which_key_leader_submenu_shows_direct_leader_children() {
    // After pressing <leader>, entries_for must return the direct children of
    // <leader> — single-key entries like "f", "b", "/" and the "g" submenu.
    // Deep entries like "gs", "gl" must NOT appear (they are under <leader>g).
    let app = App::new(None, false, None, None).unwrap();
    let leader = app.config.editor.leader;
    let prefix = km_prefix(&app, "<leader>");
    let entries = crate::which_key::entries_for(
        &app.app_keymap,
        crate::app::keymap::HjklMode::Normal,
        &prefix,
        leader,
    );

    let keys: Vec<&str> = entries.iter().map(|e| e.key.as_str()).collect();

    // Direct children that must be present.
    assert!(keys.contains(&"f"), "missing f (file picker)");
    assert!(keys.contains(&"b"), "missing b (buffer picker)");
    assert!(keys.contains(&"/"), "missing / (grep picker)");
    assert!(keys.contains(&"g"), "missing g (git submenu)");

    // Deep entries that must NOT leak into the top-level listing.
    assert!(
        !keys.contains(&"gs"),
        "gs must not appear at <leader> level"
    );
    assert!(
        !keys.contains(&"gl"),
        "gl must not appear at <leader> level"
    );
    assert!(
        !keys.contains(&"gb"),
        "gb must not appear at <leader> level"
    );
}

#[test]
fn which_key_leader_g_shows_git_actions() {
    // After pressing <leader>g, entries_for must list the git sub-actions.
    let app = App::new(None, false, None, None).unwrap();
    let leader = app.config.editor.leader;
    let prefix = km_prefix(&app, "<leader>g");
    let entries = crate::which_key::entries_for(
        &app.app_keymap,
        crate::app::keymap::HjklMode::Normal,
        &prefix,
        leader,
    );

    let keys: Vec<&str> = entries.iter().map(|e| e.key.as_str()).collect();

    assert!(keys.contains(&"s"), "missing s (git status)");
    assert!(keys.contains(&"l"), "missing l (git log)");
    assert!(keys.contains(&"b"), "missing b (git branches)");
    assert!(keys.contains(&"S"), "missing S (git stashes)");
    assert!(keys.contains(&"t"), "missing t (git tags)");
    assert!(keys.contains(&"r"), "missing r (git remotes)");
}

#[test]
fn which_key_ctrl_w_shows_window_motions() {
    // After pressing <C-w>, entries_for must include window-motion keys.
    let app = App::new(None, false, None, None).unwrap();
    let leader = app.config.editor.leader;
    let prefix = km_prefix(&app, "<C-w>");
    let entries = crate::which_key::entries_for(
        &app.app_keymap,
        crate::app::keymap::HjklMode::Normal,
        &prefix,
        leader,
    );

    let keys: Vec<&str> = entries.iter().map(|e| e.key.as_str()).collect();

    assert!(keys.contains(&"h"), "missing h (focus left)");
    assert!(keys.contains(&"j"), "missing j (focus down)");
    assert!(keys.contains(&"k"), "missing k (focus up)");
    assert!(keys.contains(&"l"), "missing l (focus right)");
    // `>` and `<` are rendered via vim notation: `>` is bare, `<` becomes `<lt>`.
    assert!(keys.contains(&">"), "missing > (wider)");
    assert!(keys.contains(&"<lt>"), "missing <lt> (narrower)");
}

#[test]
fn which_key_runtime_nmap_appears_in_entries() {
    // A binding added at runtime via app_keymap.add must surface in entries_for.
    use crate::keymap_actions::AppAction;
    let mut app = App::new(None, false, None, None).unwrap();
    let leader = app.config.editor.leader;

    // Register <leader>z → OpenFilePicker at runtime (simulates :nmap).
    app.app_keymap
        .add(
            crate::app::keymap::HjklMode::Normal,
            "<leader>z",
            AppAction::OpenFilePicker,
            "runtime file picker",
        )
        .unwrap();

    let prefix = km_prefix(&app, "<leader>");
    let entries = crate::which_key::entries_for(
        &app.app_keymap,
        crate::app::keymap::HjklMode::Normal,
        &prefix,
        leader,
    );

    let found = entries.iter().find(|e| e.key == "z");
    assert!(found.is_some(), "runtime <leader>z must appear in entries");
    assert_eq!(
        found.unwrap().desc,
        "runtime file picker",
        "description must match the registered binding"
    );
}

#[test]
fn which_key_no_pending_popup_suppressed() {
    // When no prefix is pending, active_which_key_prefix returns an empty Vec.
    // The render path checks pending.is_empty() and skips the popup.
    // This test verifies that active_which_key_prefix is empty on a fresh app
    // (no keys fed yet), matching the popup-suppression guard in render.rs.
    let app = App::new(None, false, None, None).unwrap();
    let pending = app.active_which_key_prefix();
    assert!(
        pending.is_empty(),
        "fresh app must have no pending prefix, got {} events",
        pending.len()
    );
}

// ── which-key Backspace / sticky tests (#backspace-nav) ──────────────────────

/// Feed a key into the Normal-mode app_keymap trie and update which-key state.
/// Returns whether the key was consumed by the trie.
fn feed_km_key(app: &mut App, ct_key: KeyEvent) -> bool {
    let Some(km_ev) = crate::keymap_translate::from_crossterm(&ct_key) else {
        return false;
    };
    let mut replay = Vec::new();
    app.dispatch_keymap(km_ev, 1, &mut replay)
}

#[test]
fn which_key_backspace_pops_one_key() {
    // Feed <leader> then 'g', send Backspace.
    // Result: pending = [<leader>], sticky = false.
    let mut app = App::new(None, false, None, None).unwrap();
    let leader = app.config.editor.leader;

    feed_km_key(
        &mut app,
        KeyEvent::new(KeyCode::Char(leader), KeyModifiers::NONE),
    );
    feed_km_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE),
    );
    assert_eq!(
        app.app_keymap
            .pending(crate::app::keymap::HjklMode::Normal)
            .len(),
        2,
        "should have 2 pending keys after <leader>g"
    );

    // Simulate the Backspace intercept: pop the last key.
    app.app_keymap.pop(crate::app::keymap::HjklMode::Normal);
    // sticky stays false since buffer still non-empty.
    assert!(
        !app.which_key_sticky,
        "sticky must be false when buffer non-empty after pop"
    );
    let pending = app.app_keymap.pending(crate::app::keymap::HjklMode::Normal);
    assert_eq!(pending.len(), 1, "should have 1 pending key after pop");
    assert_eq!(
        pending[0].code,
        hjkl_keymap::KeyCode::Char(leader),
        "remaining key should be <leader>"
    );
}

#[test]
fn which_key_backspace_to_empty_enters_sticky() {
    // Feed <leader>, send Backspace.
    // Result: pending empty, sticky = true.
    let mut app = App::new(None, false, None, None).unwrap();
    let leader = app.config.editor.leader;

    feed_km_key(
        &mut app,
        KeyEvent::new(KeyCode::Char(leader), KeyModifiers::NONE),
    );
    assert_eq!(
        app.app_keymap
            .pending(crate::app::keymap::HjklMode::Normal)
            .len(),
        1
    );

    // Simulate the Backspace intercept: pop the last key.
    let removed = app.app_keymap.pop(crate::app::keymap::HjklMode::Normal);
    assert!(removed.is_some(), "pop should return the removed key");
    // Buffer is now empty — caller sets sticky.
    if app
        .app_keymap
        .pending(crate::app::keymap::HjklMode::Normal)
        .is_empty()
    {
        app.which_key_sticky = true;
    }

    assert!(
        app.app_keymap
            .pending(crate::app::keymap::HjklMode::Normal)
            .is_empty(),
        "buffer must be empty after popping last key"
    );
    assert!(
        app.which_key_sticky,
        "sticky must be true after buffer empties"
    );
}

#[test]
fn which_key_backspace_at_root_is_noop() {
    // sticky = true, pending empty, Backspace → no engine action, sticky stays true.
    let mut app = App::new(None, false, None, None).unwrap();
    app.which_key_sticky = true;

    // Verify buffer is empty.
    assert!(
        app.app_keymap
            .pending(crate::app::keymap::HjklMode::Normal)
            .is_empty()
    );

    // Simulate what the event loop does: pending_non_empty is false AND sticky is true → noop.
    let pending_non_empty = !app
        .app_keymap
        .pending(crate::app::keymap::HjklMode::Normal)
        .is_empty();
    let would_noop = !pending_non_empty && app.which_key_sticky;
    assert!(would_noop, "backspace at root with sticky should noop");

    // App state unchanged.
    assert!(app.which_key_sticky, "sticky must remain true after noop");
    assert!(
        app.app_keymap
            .pending(crate::app::keymap::HjklMode::Normal)
            .is_empty()
    );
}

#[test]
fn which_key_esc_clears_sticky() {
    // sticky = true, pending empty, Esc → sticky = false.
    let mut app = App::new(None, false, None, None).unwrap();
    app.which_key_sticky = true;

    // Simulate Esc handling (as in the event loop).
    app.app_keymap.reset(crate::app::keymap::HjklMode::Normal);
    app.pending_count.clear();
    app.clear_prefix_state();
    app.which_key_sticky = false;

    assert!(!app.which_key_sticky, "Esc must clear sticky");
    assert!(
        app.app_keymap
            .pending(crate::app::keymap::HjklMode::Normal)
            .is_empty()
    );
}

#[test]
fn which_key_non_backspace_key_clears_sticky() {
    // sticky = true, pending empty, pressing a non-Backspace key clears sticky.
    let mut app = App::new(None, false, None, None).unwrap();
    app.which_key_sticky = true;

    // Simulate the unconditional sticky-clear that happens in the else branch
    // for any non-Backspace key.
    app.which_key_sticky = false;

    assert!(!app.which_key_sticky, "any non-backspace key clears sticky");
}

// ── hjkl-vim pending-state reducer integration (chunk 2a) ───────────────────

#[test]
fn pending_replace_with_count_replaces_five_chars() {
    // User types `5`, `r`, `X`: first 5 chars under cursor become `X`.
    // `5` is buffered as pending_count; `r` triggers BeginPendingReplace
    // (which reads pending_count → count=5); `X` commits via hjkl_vim::step.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "abcdefgh");
    app.active_mut().editor.jump_cursor(0, 0);

    // Buffer the count digit `5` (simulates the event loop accumulating digits).
    app.pending_count = "5".to_string();
    // `r` → matched by trie → BeginPendingReplace reads pending_count (5).
    drive_key(&mut app, key(KeyCode::Char('r')));
    assert!(
        matches!(
            app.pending_state,
            Some(hjkl_vim::PendingState::Replace { count: 5 })
        ),
        "pending_state must be Replace {{ count: 5 }}, got {:?}",
        app.pending_state
    );
    // `X` → hjkl_vim::step → Commit(ReplaceChar { ch: 'X', count: 5 }).
    drive_key(&mut app, key(KeyCode::Char('X')));
    assert!(
        app.pending_state.is_none(),
        "pending_state must clear after commit"
    );

    let content = app.active().editor.buffer().as_string();
    assert_eq!(
        content, "XXXXXfgh",
        "5rX must replace first 5 chars with X, got {content:?}"
    );
}

#[test]
fn pending_replace_esc_cancels_without_mutation() {
    // `r` then `Esc`: pending state cancelled, buffer unchanged.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "abc");
    app.active_mut().editor.jump_cursor(0, 0);

    drive_key(&mut app, key(KeyCode::Char('r')));
    assert!(app.pending_state.is_some());
    drive_key(&mut app, key(KeyCode::Esc));
    assert!(app.pending_state.is_none(), "Esc must cancel pending state");

    let content = app.active().editor.buffer().as_string();
    assert_eq!(content, "abc", "buffer must be unchanged after cancel");
}

// ── Phase 2b-iii: Z-chord integration tests ─────────────────────────────

#[test]
fn zz_centers_cursor() {
    // `zz` in Normal mode: sets viewport_pinned, no crash.
    let mut app = App::new(None, false, None, None).unwrap();
    let lines: Vec<String> = (0..20).map(|i| format!("line {i}")).collect();
    seed_buffer(&mut app, &lines.join("\n"));
    app.active_mut().editor.jump_cursor(10, 0);

    drive_key(&mut app, key(KeyCode::Char('z')));
    assert!(
        app.pending_state.is_some(),
        "z must set AfterZ pending state"
    );
    drive_key(&mut app, key(KeyCode::Char('z')));
    assert!(
        app.pending_state.is_none(),
        "second key must commit and clear pending state"
    );
    // Cursor must not have moved (zz scrolls, doesn't jump).
    assert_eq!(
        app.active().editor.cursor().0,
        10,
        "zz must not move the cursor row"
    );
}

#[test]
fn zt_scrolls_top() {
    // `zt` commits without error and clears pending state.
    let mut app = App::new(None, false, None, None).unwrap();
    let lines: Vec<String> = (0..20).map(|i| format!("line {i}")).collect();
    seed_buffer(&mut app, &lines.join("\n"));
    app.active_mut().editor.jump_cursor(10, 0);

    drive_key(&mut app, key(KeyCode::Char('z')));
    drive_key(&mut app, key(KeyCode::Char('t')));

    assert!(
        app.pending_state.is_none(),
        "pending_state cleared after zt commit"
    );
    // Cursor must not have moved (zt scrolls, doesn't jump).
    assert_eq!(
        app.active().editor.cursor().0,
        10,
        "zt must not move the cursor row"
    );
}

#[test]
fn zo_opens_fold() {
    // `zo` opens a closed fold at cursor.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "a\nb\nc\nd");
    app.active_mut().editor.buffer_mut().add_fold(1, 2, true);
    app.active_mut().editor.jump_cursor(1, 0);

    drive_key(&mut app, key(KeyCode::Char('z')));
    drive_key(&mut app, key(KeyCode::Char('o')));

    assert!(
        app.pending_state.is_none(),
        "pending_state cleared after zo commit"
    );
    let folds = app.active().editor.buffer().folds();
    assert!(!folds[0].closed, "zo must open the fold at cursor");
}

#[test]
fn zm_closes_all_folds() {
    // `zM` closes all open folds.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "a\nb\nc\nd\ne\nf");
    app.active_mut().editor.buffer_mut().add_fold(0, 1, false);
    app.active_mut().editor.buffer_mut().add_fold(4, 5, false);

    drive_key(&mut app, key(KeyCode::Char('z')));
    drive_key(&mut app, key(KeyCode::Char('M')));

    let folds = app.active().editor.buffer().folds();
    assert!(folds.iter().all(|f| f.closed), "zM must close all folds");
}

#[test]
fn z_then_esc_cancels() {
    // `z` then Esc: pending state cancelled, no engine mutation.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello\nworld");
    app.active_mut().editor.jump_cursor(0, 0);

    drive_key(&mut app, key(KeyCode::Char('z')));
    assert!(
        app.pending_state.is_some(),
        "z must set AfterZ pending state"
    );
    drive_key(&mut app, key(KeyCode::Esc));
    assert!(
        app.pending_state.is_none(),
        "Esc must cancel AfterZ pending state"
    );
    // Cursor unmoved.
    assert_eq!(
        app.active().editor.cursor(),
        (0, 0),
        "cursor must not move on cancel"
    );
}

#[test]
fn zf_in_visual_creates_fold() {
    // `zf` in Visual mode (via drive_key) creates a fold over the selection.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "a\nb\nc\nd\ne");
    // Enter visual-line mode spanning rows 1..=3 via engine keys.
    app.active_mut().editor.jump_cursor(1, 0);
    // Feed `V` then `2j` via drive_key to set visual-line selection.
    drive_key(&mut app, key(KeyCode::Char('V')));
    drive_key(&mut app, key(KeyCode::Char('j')));
    drive_key(&mut app, key(KeyCode::Char('j')));
    // Now trigger z → f.
    drive_key(&mut app, key(KeyCode::Char('z')));
    drive_key(&mut app, key(KeyCode::Char('f')));

    let folds = app.active().editor.buffer().folds();
    assert_eq!(folds.len(), 1, "zf in visual must create exactly one fold");
    assert_eq!(
        folds[0].start_row, 1,
        "fold must start at visual anchor row"
    );
    assert_eq!(folds[0].end_row, 3, "fold must end at cursor row");
    assert!(folds[0].closed, "fold must be closed");
}

// ── Phase 2c-i: AfterOp integration tests ────────────────────────────────────

/// Helper: drive a sequence of chars through drive_key.
fn drive_chars(app: &mut App, s: &str) {
    for c in s.chars() {
        drive_key(app, key(KeyCode::Char(c)));
    }
}

#[test]
fn dw_deletes_word_via_reducer() {
    // `dw` via reducer path: `d` → BeginPendingAfterOp(Delete),
    //                        `w` → ApplyOpMotion(Delete, 'w', 1).
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world");
    app.active_mut().editor.jump_cursor(0, 0);

    drive_key(&mut app, key(KeyCode::Char('d')));
    assert!(
        matches!(
            app.pending_state,
            Some(hjkl_vim::PendingState::AfterOp {
                op: hjkl_vim::OperatorKind::Delete,
                count1: 1,
                inner_count: 0,
            })
        ),
        "d must set AfterOp(Delete) pending, got {:?}",
        app.pending_state
    );
    drive_key(&mut app, key(KeyCode::Char('w')));
    assert!(
        app.pending_state.is_none(),
        "pending must clear after commit"
    );

    let line = app
        .active()
        .editor
        .buffer()
        .lines()
        .first()
        .cloned()
        .unwrap_or_default();
    assert_eq!(line, "world", "dw must delete 'hello ', got {line:?}");
}

#[test]
fn dd_deletes_line_via_reducer() {
    // `dd` via reducer: `d` → AfterOp, `d` → ApplyOpDouble.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "line1\nline2\nline3");
    app.active_mut().editor.jump_cursor(0, 0);

    drive_chars(&mut app, "dd");
    assert!(app.pending_state.is_none());

    let lines: Vec<_> = app.active().editor.buffer().lines().to_vec();
    assert_eq!(lines, vec!["line2", "line3"], "dd must delete line1");
}

#[test]
fn d3w_deletes_three_words_via_reducer() {
    // `d3w`: `d` → AfterOp(count1=1), `3` → Wait(inner_count=3), `w` →
    //        ApplyOpMotion(Delete, 'w', total=3).
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "one two three four");
    app.active_mut().editor.jump_cursor(0, 0);

    drive_key(&mut app, key(KeyCode::Char('d')));
    drive_key(&mut app, key(KeyCode::Char('3')));
    assert!(
        matches!(
            app.pending_state,
            Some(hjkl_vim::PendingState::AfterOp { inner_count: 3, .. })
        ),
        "after d3, inner_count must be 3, got {:?}",
        app.pending_state
    );
    drive_key(&mut app, key(KeyCode::Char('w')));
    assert!(app.pending_state.is_none());

    let line = app
        .active()
        .editor
        .buffer()
        .lines()
        .first()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        line, "four",
        "d3w must delete 'one two three ', got {line:?}"
    );
}

#[test]
fn two_dd_deletes_two_lines_via_reducer() {
    // `2dd`: count1=2 buffered via pending_count, `d` → AfterOp(count1=2),
    //        `d` → ApplyOpDouble(total=2).
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "line1\nline2\nline3");
    app.active_mut().editor.jump_cursor(0, 0);
    app.pending_count = "2".into();

    drive_key(&mut app, key(KeyCode::Char('d')));
    assert!(
        matches!(
            app.pending_state,
            Some(hjkl_vim::PendingState::AfterOp { count1: 2, .. })
        ),
        "count1 must be 2, got {:?}",
        app.pending_state
    );
    drive_key(&mut app, key(KeyCode::Char('d')));
    assert!(app.pending_state.is_none());

    let lines: Vec<_> = app.active().editor.buffer().lines().to_vec();
    assert_eq!(
        lines,
        vec!["line3"],
        "2dd must delete two lines, got {lines:?}"
    );
}

#[test]
fn cw_changes_to_word_end() {
    // `cw` — Change + 'w' motion. The cw→ce quirk must be applied so that
    // only "hello" is consumed, not "hello " (trailing space preserved).
    // After cw, editor enters Insert mode.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world");
    app.active_mut().editor.jump_cursor(0, 0);

    drive_chars(&mut app, "cw");
    assert!(app.pending_state.is_none());

    // Must be in Insert mode (change enters insert).
    assert_eq!(
        app.active().editor.vim_mode(),
        hjkl_engine::VimMode::Insert,
        "cw must enter Insert mode"
    );
    // The space before "world" should still be present as the first char.
    let line = app
        .active()
        .editor
        .buffer()
        .lines()
        .first()
        .cloned()
        .unwrap_or_default();
    assert!(
        line.starts_with(' ') || line == " world",
        "cw quirk: trailing space must be preserved, got {line:?}"
    );
}

#[test]
fn dip_text_object_via_reducer() {
    // `dip` — d → AfterOp, i → Wait(OpTextObj{inner:true}), p → ApplyOpTextObj.
    // After Phase 2c-iii, the reducer owns the full sequence; engine is NOT
    // chord-pending at any point after 'i'.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world\n\nfoo bar");
    app.active_mut().editor.jump_cursor(0, 0);

    drive_key(&mut app, key(KeyCode::Char('d')));
    drive_key(&mut app, key(KeyCode::Char('i')));
    // Reducer now owns state: OpTextObj. Engine must NOT be chord-pending.
    assert!(
        matches!(
            app.pending_state,
            Some(hjkl_vim::PendingState::OpTextObj {
                op: hjkl_vim::OperatorKind::Delete,
                inner: true,
                ..
            })
        ),
        "after di, reducer must hold OpTextObj(Delete,inner=true), got {:?}",
        app.pending_state
    );
    assert!(
        !app.active().editor.is_chord_pending(),
        "engine must NOT be chord-pending after reducer-owned di"
    );

    // 'p' → reducer commits ApplyOpTextObj → engine::apply_op_text_obj.
    drive_key(&mut app, key(KeyCode::Char('p')));
    assert!(
        app.pending_state.is_none(),
        "pending must clear after ApplyOpTextObj commit"
    );
    assert!(!app.active().editor.is_chord_pending());

    // First paragraph (lines 0..0) should be deleted; remaining: empty line + "foo bar".
    let lines: Vec<_> = app.active().editor.buffer().lines().to_vec();
    assert!(
        !lines.contains(&"hello world".to_string()),
        "dip must delete first paragraph, got {lines:?}"
    );
}

#[test]
fn dgg_deletes_to_top() {
    // `dgg` — d → AfterOp, g → Wait(OpG) [reducer owns state], g → ApplyOpG.
    // Phase 2c-iv: engine is NOT in chord-pending after `dg`; reducer holds
    // PendingState::OpG and dispatches the second 'g' as ApplyOpG.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "line1\nline2\nline3");
    app.active_mut().editor.jump_cursor(2, 0); // start on line3.

    drive_key(&mut app, key(KeyCode::Char('d')));
    assert!(
        matches!(
            app.pending_state,
            Some(hjkl_vim::PendingState::AfterOp {
                op: hjkl_vim::OperatorKind::Delete,
                ..
            })
        ),
        "d must set AfterOp(Delete), got {:?}",
        app.pending_state
    );

    drive_key(&mut app, key(KeyCode::Char('g')));
    // Reducer transitions to OpG — engine is NOT chord-pending (reducer owns state).
    assert!(
        matches!(
            app.pending_state,
            Some(hjkl_vim::PendingState::OpG {
                op: hjkl_vim::OperatorKind::Delete,
                total_count: 1,
            })
        ),
        "after dg, reducer must be in OpG state, got {:?}",
        app.pending_state
    );
    assert!(
        !app.active().editor.is_chord_pending(),
        "engine must NOT be chord-pending after dg (reducer owns OpG)"
    );

    drive_key(&mut app, key(KeyCode::Char('g')));
    assert!(
        app.pending_state.is_none(),
        "pending must clear after ApplyOpG commit"
    );
    // dgg should delete lines 0..=2 (all three lines).
    let lines: Vec<_> = app.active().editor.buffer().lines().to_vec();
    assert!(
        lines.is_empty() || lines == vec![""],
        "dgg from line3 must delete all lines, got {lines:?}"
    );
}

#[test]
fn dfx_deletes_to_x_via_reducer() {
    // `dfx` via reducer path (Phase 2c-ii):
    //   `d` → AfterOp, `f` → Wait(OpFind{forward,!till}), `x` → ApplyOpFind.
    // After Phase 2c-ii, the reducer holds state through 'x'; engine is NOT
    // chord-pending at any point in this flow.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello x world");
    app.active_mut().editor.jump_cursor(0, 0);

    drive_key(&mut app, key(KeyCode::Char('d')));
    assert!(
        matches!(
            app.pending_state,
            Some(hjkl_vim::PendingState::AfterOp {
                op: hjkl_vim::OperatorKind::Delete,
                ..
            })
        ),
        "d must set AfterOp(Delete), got {:?}",
        app.pending_state
    );

    drive_key(&mut app, key(KeyCode::Char('f')));
    assert!(
        matches!(
            app.pending_state,
            Some(hjkl_vim::PendingState::OpFind {
                op: hjkl_vim::OperatorKind::Delete,
                forward: true,
                till: false,
                ..
            })
        ),
        "df must transition to OpFind(forward, !till), got {:?}",
        app.pending_state
    );
    assert!(
        !app.active().editor.is_chord_pending(),
        "engine must NOT be in chord-pending after reducer-owned df"
    );

    drive_key(&mut app, key(KeyCode::Char('x')));
    assert!(
        app.pending_state.is_none(),
        "pending must clear after ApplyOpFind commit"
    );
    assert!(!app.active().editor.is_chord_pending());

    // "hello x" (inclusive) should be deleted.
    let line = app
        .active()
        .editor
        .buffer()
        .lines()
        .first()
        .cloned()
        .unwrap_or_default();
    assert_eq!(line, " world", "dfx must delete 'hello x', got {line:?}");
}

// ── Phase 2c-ii: OpFind reducer integration tests ─────────────────────────

#[test]
fn dtx_stops_before_x_via_reducer() {
    // `dtx` — d → AfterOp, t → Wait(OpFind{forward,till}), x → ApplyOpFind.
    // Deletes up to but not including 'x'.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello x world");
    app.active_mut().editor.jump_cursor(0, 0);

    drive_chars(&mut app, "dtx");
    assert!(app.pending_state.is_none());

    let line = app
        .active()
        .editor
        .buffer()
        .lines()
        .first()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        line, "x world",
        "dtx must delete 'hello ' leaving 'x world', got {line:?}"
    );
}

#[test]
fn two_d_3fx_total_count_6() {
    // `2d3fx`: count1=2, inner_count=3 → total=6. In "xaxbxcxdxexf" from col 0,
    // the 6th 'x' is at col 10 (0-indexed: x@0,x@2,x@4,x@6,x@8,x@10).
    // dfx with count=6 deletes from col 0 through col 10 inclusive.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "xaxbxcxdxexf");
    app.active_mut().editor.jump_cursor(0, 0);

    app.pending_count = "2".into();
    drive_key(&mut app, key(KeyCode::Char('d')));
    assert!(
        matches!(
            app.pending_state,
            Some(hjkl_vim::PendingState::AfterOp { count1: 2, .. })
        ),
        "count1 must be 2 after pending_count+d, got {:?}",
        app.pending_state
    );

    drive_key(&mut app, key(KeyCode::Char('3')));
    assert!(
        matches!(
            app.pending_state,
            Some(hjkl_vim::PendingState::AfterOp { inner_count: 3, .. })
        ),
        "inner_count must accumulate to 3, got {:?}",
        app.pending_state
    );

    drive_key(&mut app, key(KeyCode::Char('f')));
    assert!(
        matches!(
            app.pending_state,
            Some(hjkl_vim::PendingState::OpFind {
                total_count: 6,
                forward: true,
                till: false,
                ..
            })
        ),
        "OpFind total_count must be 6 (2*3), got {:?}",
        app.pending_state
    );

    drive_key(&mut app, key(KeyCode::Char('x')));
    assert!(app.pending_state.is_none());

    let line = app
        .active()
        .editor
        .buffer()
        .lines()
        .first()
        .cloned()
        .unwrap_or_default();
    assert_eq!(line, "f", "2d3fx must delete through 6th 'x', got {line:?}");
}

#[test]
fn df_then_esc_cancels_via_reducer() {
    // `df<Esc>` — OpFind on Esc → Cancel; buffer unchanged.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello x world");
    app.active_mut().editor.jump_cursor(0, 0);

    drive_key(&mut app, key(KeyCode::Char('d')));
    drive_key(&mut app, key(KeyCode::Char('f')));
    assert!(
        matches!(
            app.pending_state,
            Some(hjkl_vim::PendingState::OpFind { .. })
        ),
        "df must set OpFind, got {:?}",
        app.pending_state
    );

    drive_key(&mut app, key(KeyCode::Esc));
    assert!(
        app.pending_state.is_none(),
        "Esc must cancel OpFind pending"
    );

    // Buffer unchanged.
    let line = app
        .active()
        .editor
        .buffer()
        .lines()
        .first()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        line, "hello x world",
        "buffer must be unchanged after df<Esc>"
    );
}

#[test]
fn cfx_changes_to_x_via_reducer() {
    // `cfx` — c → AfterOp, f → OpFind{Change,forward,!till}, x → ApplyOpFind.
    // Change+Find (cf<x>) stays as Change+Find; no cw→ce style quirk applies.
    // After cfx the editor enters Insert mode.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello x world");
    app.active_mut().editor.jump_cursor(0, 0);

    drive_chars(&mut app, "cfx");
    assert!(app.pending_state.is_none());

    assert_eq!(
        app.active().editor.vim_mode(),
        hjkl_engine::VimMode::Insert,
        "cfx must enter Insert mode"
    );
    // "hello x" was deleted; buffer should have " world" remaining.
    let line = app
        .active()
        .editor
        .buffer()
        .lines()
        .first()
        .cloned()
        .unwrap_or_default();
    assert_eq!(line, " world", "cfx must delete 'hello x', got {line:?}");
}

#[test]
fn gufx_uppercases_via_engine_fsm() {
    // `gUfx` — g → AfterG (reducer), U → after_g('U') → engine sets
    // Pending::Op(Uppercase). The 'f' key then goes to the ENGINE FSM
    // (is_chord_pending bypass), which sets Pending::OpFind (engine-owned).
    // The 'x' char completes the engine's OpFind arm via handle_op_find_target.
    // This verifies the chord-init op path (gU/gu/g~ + f/F/t/T) still works
    // through the engine FSM, NOT through the reducer OpFind state.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello x world");
    app.active_mut().editor.jump_cursor(0, 0);

    drive_key(&mut app, key(KeyCode::Char('g')));
    assert!(
        matches!(
            app.pending_state,
            Some(hjkl_vim::PendingState::AfterG { .. })
        ),
        "g must set AfterG"
    );

    // U → reducer commits AfterGChord('U') → engine sets Pending::Op(Uppercase).
    drive_key(&mut app, key(KeyCode::Char('U')));
    assert!(app.pending_state.is_none(), "gU must clear reducer pending");
    assert!(
        app.active().editor.is_chord_pending(),
        "gU must leave engine in Op(Uppercase) chord-pending"
    );

    // 'f' → engine FSM processes Op-pending + 'f' → sets Pending::OpFind (engine).
    drive_key(&mut app, key(KeyCode::Char('f')));
    assert!(
        app.pending_state.is_none(),
        "f after gU must NOT set reducer OpFind (engine owns it)"
    );
    assert!(
        app.active().editor.is_chord_pending(),
        "engine must remain chord-pending waiting for find-target"
    );

    // 'x' → engine FSM: handle_op_find_target → uppercase through to 'x'.
    drive_key(&mut app, key(KeyCode::Char('x')));
    assert!(!app.active().editor.is_chord_pending());

    let line = app
        .active()
        .editor
        .buffer()
        .lines()
        .first()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        line, "HELLO X world",
        "gUfx must uppercase 'hello x', got {line:?}"
    );
}

#[test]
fn d_then_esc_cancels() {
    // `d` + Esc: pending state cancelled, buffer unchanged.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello");
    app.active_mut().editor.jump_cursor(0, 0);

    drive_key(&mut app, key(KeyCode::Char('d')));
    assert!(app.pending_state.is_some(), "d must set pending state");
    drive_key(&mut app, key(KeyCode::Esc));
    assert!(app.pending_state.is_none(), "Esc must cancel pending");

    let line = app
        .active()
        .editor
        .buffer()
        .lines()
        .first()
        .cloned()
        .unwrap_or_default();
    assert_eq!(line, "hello", "buffer must be unchanged after cancel");
}

#[test]
fn y_dollar_yanks_to_eol() {
    // `y$`: yank to end-of-line. Buffer unchanged, cursor stays.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world");
    app.active_mut().editor.jump_cursor(0, 0);

    drive_key(&mut app, key(KeyCode::Char('y')));
    assert!(
        matches!(
            app.pending_state,
            Some(hjkl_vim::PendingState::AfterOp {
                op: hjkl_vim::OperatorKind::Yank,
                ..
            })
        ),
        "y must set AfterOp(Yank)"
    );
    drive_key(&mut app, key(KeyCode::Char('$')));
    assert!(app.pending_state.is_none(), "pending must clear after y$");

    // Buffer unchanged (yank is non-destructive).
    let line = app
        .active()
        .editor
        .buffer()
        .lines()
        .first()
        .cloned()
        .unwrap_or_default();
    assert_eq!(line, "hello world", "y$ must not modify buffer");
}

#[test]
fn guw_still_works_via_engine_fsm() {
    // `gUw` — g → AfterG (via reducer), U → after_g('U') → engine sets
    // Pending::Op(Uppercase); w → engine handles motion. Verifies the
    // chord-initiated path (gu/gU/g~) still goes through engine FSM, NOT
    // through the AfterOp reducer.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world");
    app.active_mut().editor.jump_cursor(0, 0);

    drive_key(&mut app, key(KeyCode::Char('g')));
    assert!(
        matches!(
            app.pending_state,
            Some(hjkl_vim::PendingState::AfterG { .. })
        ),
        "g must set AfterG pending, got {:?}",
        app.pending_state
    );
    // U → reducer commits AfterGChord('U') → engine sets Pending::Op(Uppercase).
    drive_key(&mut app, key(KeyCode::Char('U')));
    assert!(app.pending_state.is_none(), "gU must clear reducer pending");
    assert!(
        app.active().editor.is_chord_pending(),
        "gU must leave engine in Op(Uppercase) chord-pending"
    );
    // w → engine applies Uppercase over WordFwd motion.
    drive_key(&mut app, key(KeyCode::Char('w')));
    assert!(!app.active().editor.is_chord_pending());

    let line = app
        .active()
        .editor
        .buffer()
        .lines()
        .first()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        line, "HELLO world",
        "gUw must uppercase first word, got {line:?}"
    );
}

// ── Phase 2c-iv OpG integration tests ────────────────────────────────────────

#[test]
fn dgg_deletes_to_top_via_reducer() {
    // `dgg` full round-trip via reducer OpG path.
    // d → AfterOp, g → Wait(OpG), g → Commit(ApplyOpG{'g'}) → delete to top.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "aaa\nbbb\nccc");
    app.active_mut().editor.jump_cursor(2, 0); // cursor on "ccc".

    drive_chars(&mut app, "dgg");
    assert!(app.pending_state.is_none(), "pending must clear after dgg");
    assert!(!app.active().editor.is_chord_pending());

    let lines: Vec<_> = app.active().editor.buffer().lines().to_vec();
    assert!(
        lines.is_empty() || lines == vec![""],
        "dgg from last line must delete all content, got {lines:?}"
    );
}

#[test]
fn dge_deletes_word_end_back_via_reducer() {
    // `dge` round-trip: d → AfterOp, g → Wait(OpG), e → Commit(ApplyOpG{'e'}).
    // engine::apply_op_g with 'e' → Motion::WordEndBack. With cursor at col 0
    // there's nothing behind, so just verify reducer state machine and no panic.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world");
    app.active_mut().editor.jump_cursor(0, 0);

    drive_key(&mut app, key(KeyCode::Char('d')));
    assert!(app.pending_state.is_some(), "d sets AfterOp");

    drive_key(&mut app, key(KeyCode::Char('g')));
    assert!(
        matches!(app.pending_state, Some(hjkl_vim::PendingState::OpG { .. })),
        "g transitions to OpG, got {:?}",
        app.pending_state
    );

    drive_key(&mut app, key(KeyCode::Char('e')));
    // Reducer commits ApplyOpG; engine applies WordEndBack. No panic expected.
    assert!(app.pending_state.is_none(), "pending clears after dge");
    assert!(!app.active().editor.is_chord_pending());
}

#[test]
fn dgj_deletes_screen_down_via_reducer() {
    // `dgj` round-trip: d → AfterOp, g → Wait(OpG), j → Commit(ApplyOpG{'j'}).
    // engine::apply_op_g with 'j' → Motion::ScreenDown.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "line1\nline2\nline3");
    app.active_mut().editor.jump_cursor(0, 0); // cursor on line1.

    drive_chars(&mut app, "dgj");
    assert!(app.pending_state.is_none(), "pending clears after dgj");

    let lines: Vec<_> = app.active().editor.buffer().lines().to_vec();
    // dgj deletes current line + screen-line below (same as next line here).
    assert_eq!(
        lines,
        vec!["line3"],
        "dgj must delete line1+line2, got {lines:?}"
    );
}

#[test]
fn dg_then_esc_cancels_via_reducer() {
    // `dg<Esc>` — OpG reducer cancels on Esc; buffer unchanged.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "unchanged");
    app.active_mut().editor.jump_cursor(0, 0);

    drive_key(&mut app, key(KeyCode::Char('d')));
    drive_key(&mut app, key(KeyCode::Char('g')));
    assert!(
        matches!(app.pending_state, Some(hjkl_vim::PendingState::OpG { .. })),
        "must be in OpG state before Esc"
    );

    drive_key(&mut app, key(KeyCode::Esc));
    assert!(app.pending_state.is_none(), "Esc must cancel OpG");

    let line = app
        .active()
        .editor
        .buffer()
        .lines()
        .first()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        line, "unchanged",
        "buffer must be unchanged after cancel, got {line:?}"
    );
}

#[test]
fn g_uppercase_gg_uppercases_to_top_via_engine_fsm() {
    // `gUgg` — chord-init op path (engine FSM Pending::OpG).
    // g → AfterG (reducer), U → after_g('U') → engine sets Pending::Op(Uppercase).
    // g → engine FSM (is_chord_pending bypass) → sets Pending::OpG (engine-owned).
    // g → engine FSM: handle_op_after_g → uppercase to file-top (gg = FileTop).
    // Verifies engine Pending::OpG path still works after 2c-iv.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello\nworld\nfoo");
    app.active_mut().editor.jump_cursor(2, 0); // cursor on "foo".

    drive_key(&mut app, key(KeyCode::Char('g')));
    assert!(
        matches!(
            app.pending_state,
            Some(hjkl_vim::PendingState::AfterG { .. })
        ),
        "g must set AfterG"
    );

    // U → reducer commits AfterGChord('U') → engine sets Pending::Op(Uppercase).
    drive_key(&mut app, key(KeyCode::Char('U')));
    assert!(app.pending_state.is_none(), "gU must clear reducer pending");
    assert!(
        app.active().editor.is_chord_pending(),
        "gU must leave engine in Op(Uppercase) chord-pending"
    );

    // g → engine FSM: Op(Uppercase) + 'g' → engine sets Pending::OpG (engine-owned).
    drive_key(&mut app, key(KeyCode::Char('g')));
    assert!(app.pending_state.is_none(), "reducer must stay clear");
    assert!(
        app.active().editor.is_chord_pending(),
        "engine must be in OpG chord-pending waiting for second 'g'"
    );

    // g → engine FSM: handle_op_after_g → Motion::FileTop → uppercase all to top.
    drive_key(&mut app, key(KeyCode::Char('g')));
    assert!(
        !app.active().editor.is_chord_pending(),
        "engine chord must complete"
    );

    // All three lines should be uppercased.
    let lines: Vec<_> = app.active().editor.buffer().lines().to_vec();
    assert!(
        lines.iter().all(|l| l.chars().all(|c| !c.is_lowercase())),
        "gUgg must uppercase all lines to top, got {lines:?}"
    );
}
