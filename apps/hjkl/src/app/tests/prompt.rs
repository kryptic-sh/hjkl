use super::*;

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

// ── :set background= tests ───────────────────────────────────────────────

#[test]
fn colon_set_background_dark_swaps_theme() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("set background=dark");
    let msg = app.status_message.clone().unwrap_or_default();
    assert_eq!(
        msg, "background=dark",
        "expected background=dark, got: {msg}"
    );
}

#[test]
fn colon_set_background_light_swaps_theme() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("set background=light");
    let msg = app.status_message.clone().unwrap_or_default();
    assert_eq!(
        msg, "background=light",
        "expected background=light, got: {msg}"
    );
}

#[test]
fn colon_set_background_unknown_errors() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("set background=mauve");
    let msg = app.status_message.clone().unwrap_or_default();
    assert!(
        msg.starts_with("E:"),
        "expected E: error for unknown background, got: {msg}"
    );
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

// ── Wildmenu / command completion (Phase 5b) tests ──────────────────────────

#[test]
fn colon_tab_single_match_inserts_fully() {
    // "writ" → Tab → should complete to "write" and close menu.
    let mut app = App::new(None, false, None, None).unwrap();
    app.open_command_prompt();
    type_str(&mut app, "writ");
    app.handle_command_field_key(tab_key());
    assert_eq!(
        app.command_field.as_ref().unwrap().text(),
        "write",
        "single match must insert fully"
    );
    assert!(
        app.command_completion.is_none(),
        "menu must close after single match"
    );
}

#[test]
fn colon_tab_multi_match_extends_to_lcp() {
    // "w" → Tab → LCP of all w-commands; completion state must be Some.
    let mut app = App::new(None, false, None, None).unwrap();
    app.open_command_prompt();
    type_str(&mut app, "w");
    app.handle_command_field_key(tab_key());
    // After first Tab, completion menu is open (multiple w* candidates exist).
    assert!(
        app.command_completion.is_some(),
        "wildmenu must be open after Tab on multi-match"
    );
    let text = app.command_field.as_ref().unwrap().text();
    // All w* candidates share "w" as prefix at minimum.
    assert!(
        text.starts_with('w'),
        "field must start with typed prefix: {text:?}"
    );
}

#[test]
fn colon_tab_then_tab_cycles() {
    // "w" → Tab (LCP, no selection) → Tab (selects idx 0) → Tab (selects idx 1).
    let mut app = App::new(None, false, None, None).unwrap();
    app.open_command_prompt();
    type_str(&mut app, "w");
    app.handle_command_field_key(tab_key()); // first Tab → LCP, selected=None
    assert!(app.command_completion.is_some());
    assert!(app.command_completion.as_ref().unwrap().selected.is_none());

    app.handle_command_field_key(tab_key()); // second Tab → selected=Some(0)
    let idx0 = app.command_completion.as_ref().unwrap().selected;
    assert_eq!(idx0, Some(0));

    app.handle_command_field_key(tab_key()); // third Tab → selected=Some(1)
    let idx1 = app.command_completion.as_ref().unwrap().selected;
    assert_eq!(idx1, Some(1));
}

#[test]
fn colon_shift_tab_cycles_backward() {
    // "w" → Tab (open, no sel) → Tab (idx 0) → Tab (idx 1) → S-Tab (idx 0).
    let mut app = App::new(None, false, None, None).unwrap();
    app.open_command_prompt();
    type_str(&mut app, "w");
    app.handle_command_field_key(tab_key()); // open, no sel
    app.handle_command_field_key(tab_key()); // idx 0
    app.handle_command_field_key(tab_key()); // idx 1
    let before = app.command_completion.as_ref().unwrap().selected.unwrap();
    app.handle_command_field_key(shift_tab_key()); // back to idx 0
    let after = app.command_completion.as_ref().unwrap().selected.unwrap();
    assert_eq!(after, before - 1, "S-Tab must decrement selection");
}

#[test]
fn colon_esc_during_completion_reverts() {
    // "w" → Tab (open) → Tab (select idx 0, text changed) → Esc → back to "w".
    let mut app = App::new(None, false, None, None).unwrap();
    app.open_command_prompt();
    type_str(&mut app, "w");
    app.handle_command_field_key(tab_key()); // open
    app.handle_command_field_key(tab_key()); // select first candidate
    // Field now contains the candidate text, not "w".
    let candidate_text = app.command_field.as_ref().unwrap().text();
    // Esc should revert to "w" and clear completion but keep command_field open.
    app.handle_command_field_key(key(KeyCode::Esc));
    assert!(
        app.command_completion.is_none(),
        "completion must be cleared after Esc"
    );
    assert!(
        app.command_field.is_some(),
        "command field must stay open after completion Esc"
    );
    assert_eq!(
        app.command_field.as_ref().unwrap().text(),
        "w",
        "field must revert to original: was {candidate_text:?}"
    );
}

#[test]
fn colon_other_key_during_completion_commits() {
    // "w" → Tab (open, LCP applied) → Tab (select idx 0) → Space → commits, menu closed.
    let mut app = App::new(None, false, None, None).unwrap();
    app.open_command_prompt();
    type_str(&mut app, "w");
    app.handle_command_field_key(tab_key()); // open
    app.handle_command_field_key(tab_key()); // select first candidate
    let candidate_text = app.command_field.as_ref().unwrap().text();
    // Press Space — should commit the candidate and close the menu.
    app.handle_command_field_key(key(KeyCode::Char(' ')));
    assert!(
        app.command_completion.is_none(),
        "completion must be cleared after non-Tab/non-Esc key"
    );
    // Field text should start with the selected candidate (space appended after commit).
    let final_text = app.command_field.as_ref().unwrap().text();
    assert!(
        final_text.starts_with(&candidate_text),
        "field must start with committed candidate: candidate={candidate_text:?} final={final_text:?}"
    );
}
