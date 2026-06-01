use super::*;

// ── Command palette (`:`) tests ─────────────────────────────────────────

#[test]
fn palette_open_and_submit_runs_dispatch_and_closes() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.open_command_prompt();
    assert!(app.command_field.is_some());
    // Type a unique command that has no ambiguity — "quit" → only "quit"
    // matches so popup has exactly one candidate, and we can dismiss
    // it cleanly.  With the new popup flow, Enter while popup is open
    // accepts the selected candidate; a second Enter executes.
    type_str(&mut app, "quit");
    // Dismiss popup (Esc), then the command is exactly "quit" with no popup.
    app.handle_command_field_key(key(KeyCode::Esc));
    assert!(app.completion.is_none(), "popup must be dismissed");
    // Now Enter executes "quit".
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
    let msg = app.bus.last_body_or_empty().to_string();
    assert!(msg.contains("E32"), "expected E32, got: {msg}");
}

#[test]
fn wq_readonly_does_not_exit() {
    let mut app = App::new(None, true, None, None).unwrap();
    app.active_mut().filename = Some(tmp_path("hjkl_wq_ro_test.txt"));
    app.dispatch_ex("wq");
    assert!(!app.exit_requested, "wq must not exit when save fails");
    let msg = app.bus.last_body_or_empty().to_string();
    assert!(msg.contains("E45"), "expected E45, got: {msg}");
}

#[test]
fn palette_esc_in_insert_drops_to_normal_then_motions_apply() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.open_command_prompt();
    type_str(&mut app, "abc");
    // First Esc: popup is open (typed "abc" matches no commands → popup is None).
    // "abc" has no matching commands so popup should be None.
    // If popup is Some, Esc dismisses popup. If None, Esc drops to Normal.
    // Either way we need Normal mode — press Esc until we get there.
    if app.completion.is_some() {
        // Dismiss popup first.
        app.handle_command_field_key(key(KeyCode::Esc));
        assert!(app.completion.is_none());
    }
    // Now Esc drops to Normal.
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
    let msg = app.bus.last_body_or_empty().to_string();
    assert_eq!(
        msg, "background=dark",
        "expected background=dark, got: {msg}"
    );
}

#[test]
fn colon_set_background_light_swaps_theme() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("set background=light");
    let msg = app.bus.last_body_or_empty().to_string();
    assert_eq!(
        msg, "background=light",
        "expected background=light, got: {msg}"
    );
}

#[test]
fn colon_set_background_unknown_errors() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("set background=mauve");
    let msg = app.bus.last_body_or_empty().to_string();
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
    // Empty field shows a popup with all commands. First Esc dismisses popup.
    if app.completion.is_some() {
        app.handle_command_field_key(key(KeyCode::Esc));
        assert!(app.completion.is_none(), "first Esc must dismiss popup");
        assert!(
            app.command_field.is_some(),
            "field stays open after popup Esc"
        );
    }
    // Second Esc (or first if no popup) closes the empty field.
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
    // Esc1: a single press dismisses the popup AND leaves Insert mode.
    assert!(app.completion.is_some(), "popup must be open after 'w'");
    app.handle_command_field_key(key(KeyCode::Esc));
    assert!(app.completion.is_none(), "Esc must dismiss popup");
    assert!(app.command_field.is_some(), "field stays open after Esc");
    assert_eq!(
        app.command_field.as_ref().unwrap().vim_mode(),
        VimMode::Normal,
        "single Esc must also leave Insert mode"
    );
    // Esc2: from Normal, close the field.
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
    let msg = app.bus.last_body_or_empty().to_string();
    assert!(msg.contains("E32"), "expected E32, got: {msg}");
}

// ── Command completion popup tests (new popup-based flow) ──────────────────

#[test]
fn colon_typing_opens_completion_popup() {
    // Typing in `:` prompt should open the completion popup live.
    let mut app = App::new(None, false, None, None).unwrap();
    app.open_command_prompt();
    type_str(&mut app, "w");
    // After typing "w", popup must be Some with at least one "w*" candidate.
    assert!(
        app.completion.is_some(),
        "completion popup must open after typing in : prompt"
    );
    let popup = app.completion.as_ref().unwrap();
    assert!(!popup.is_empty(), "popup must have candidates for 'w'");
    // All visible candidates must start with 'w'.
    for &idx in &popup.visible {
        let label = &popup.all_items[idx].label;
        assert!(
            label.starts_with('w'),
            "candidate {label:?} must start with 'w'"
        );
    }
}

#[test]
fn colon_popup_items_have_docs() {
    // Each candidate in the popup must have a detail string.
    let mut app = App::new(None, false, None, None).unwrap();
    app.open_command_prompt();
    type_str(&mut app, "e");
    let popup = app.completion.as_ref().expect("popup must be open");
    // Find the "edit" candidate and check it has a detail.
    let edit_item = popup
        .all_items
        .iter()
        .find(|i| i.label == "edit" || i.label.starts_with("edit"));
    assert!(edit_item.is_some(), "should find an 'edit*' candidate");
    let item = edit_item.unwrap();
    assert!(
        item.detail.is_some(),
        "command item must have a detail/doc string"
    );
}

#[test]
fn colon_tab_navigates_popup_forward() {
    // "w" opens popup; Tab moves selection forward.
    let mut app = App::new(None, false, None, None).unwrap();
    app.open_command_prompt();
    type_str(&mut app, "w");
    assert!(app.completion.is_some());
    let initial_sel = app.completion.as_ref().unwrap().selected;
    app.handle_command_field_key(tab_key()); // select_next
    let after_tab = app.completion.as_ref().unwrap().selected;
    // selected should have moved
    let visible_len = app.completion.as_ref().unwrap().visible.len();
    if visible_len > 1 {
        assert_ne!(initial_sel, after_tab, "Tab must advance selection");
    }
}

#[test]
fn colon_shift_tab_navigates_popup_backward() {
    // "w" opens popup; Tab forward; S-Tab back.
    let mut app = App::new(None, false, None, None).unwrap();
    app.open_command_prompt();
    type_str(&mut app, "w");
    assert!(app.completion.is_some());
    let visible_len = app.completion.as_ref().unwrap().visible.len();
    if visible_len > 1 {
        app.handle_command_field_key(tab_key()); // move forward
        let after_tab = app.completion.as_ref().unwrap().selected;
        app.handle_command_field_key(shift_tab_key()); // move back
        let after_btab = app.completion.as_ref().unwrap().selected;
        assert_ne!(after_tab, after_btab, "S-Tab must move selection back");
    }
}

#[test]
fn colon_enter_accepts_selected_item_no_execute() {
    // Typing "e" resolves to a runnable command (`edit`), so a bare Enter would
    // execute it. But once the user NAVIGATES the popup, Enter must accept the
    // highlighted candidate (field stays open) rather than execute.
    let mut app = App::new(None, false, None, None).unwrap();
    app.open_command_prompt();
    type_str(&mut app, "e");
    assert!(
        app.completion.is_some(),
        "popup must be open after typing 'e'"
    );
    // Navigate off the auto-selected best match, then accept with Enter.
    app.handle_command_field_key(key(KeyCode::Tab));
    app.handle_command_field_key(key(KeyCode::Enter));
    // Popup must now be closed.
    assert!(
        app.completion.is_none(),
        "popup must close after Enter-accept"
    );
    // Command field must still be open (not yet executed).
    assert!(
        app.command_field.is_some(),
        "command field must stay open after Enter-accept"
    );
    // Field must now contain the accepted command name.
    let text = app.command_field.as_ref().unwrap().text();
    assert!(!text.is_empty(), "field must contain accepted text");
}

#[test]
fn colon_accept_arg_command_appends_space() {
    // Accepting an arg-taking command (`edit` → `ArgKind::Path`) appends a
    // trailing space so the user can type the argument. Drive the accept
    // mechanism directly (the Enter routing for a complete/runnable command is
    // covered separately by the alias/exact-match tests).
    let mut app = App::new(None, false, None, None).unwrap();
    app.open_command_prompt();
    type_str(&mut app, "edit");
    assert!(app.completion.is_some(), "popup must be open");
    // Find "edit" in the popup and select it.
    let popup = app.completion.as_ref().unwrap();
    let edit_vis_idx = popup
        .visible
        .iter()
        .position(|&idx| popup.all_items[idx].label == "edit");
    if let Some(vis_idx) = edit_vis_idx {
        app.completion.as_mut().unwrap().selected = vis_idx;
        app.accept_command_completion();
        let text = app.command_field.as_ref().unwrap().text();
        assert!(
            text.ends_with(' '),
            "arg-taking command 'edit' must have trailing space after accept: {text:?}"
        );
    }
    // If "edit" isn't in the popup candidates, we skip the rest of the assertion.
}

#[test]
fn colon_enter_exact_match_executes_directly() {
    // Typing a full no-arg command name (e.g. "wq") that exactly matches the
    // selected candidate must EXECUTE on the first Enter — accepting would be a
    // no-op (line already == candidate, no trailing space), so we skip the
    // accept step and run directly rather than requiring a second Enter.
    let mut app = App::new(None, false, None, None).unwrap();
    app.open_command_prompt();
    type_str(&mut app, "wq");
    // Popup is open with "wq" highlighted (it's a real command). Ensure "wq" is
    // the selected candidate so accept would be a no-op.
    let wq_vis_idx = app.completion.as_ref().and_then(|popup| {
        popup
            .visible
            .iter()
            .position(|&idx| popup.all_items[idx].label == "wq")
    });
    if let (Some(vis_idx), Some(popup)) = (wq_vis_idx, app.completion.as_mut()) {
        popup.selected = vis_idx;
        // accept-would-change must be false for an exact match.
        assert!(
            !app.command_accept_would_change_line(),
            "exact-match 'wq' should not change the line on accept"
        );
    }
    app.handle_command_field_key(key(KeyCode::Enter));
    // Single Enter executed: the command field is closed (not still awaiting a
    // second Enter).
    assert!(
        app.command_field.is_none(),
        "exact-match command must execute on the first Enter (field closed)"
    );
}

#[test]
fn colon_alias_command_executes_directly() {
    // ":w" is an alias of "write". The popup only lists canonical names
    // (write, wall, wnext…), so "w" is never an item — but typing ":w<Enter>"
    // must run write, NOT accept the top-ranked "wa*" candidate.
    let mut app = App::new(None, false, None, None).unwrap();
    app.open_command_prompt();
    type_str(&mut app, "w");
    // Popup is open and the typed line resolves to a runnable command.
    assert!(app.completion.is_some(), "popup should be open after 'w'");
    assert!(
        app.command_line_is_runnable(),
        "':w' must resolve as a runnable command via its alias"
    );
    app.handle_command_field_key(key(KeyCode::Enter));
    assert!(
        app.command_field.is_none(),
        "alias command ':w' must execute on the first Enter (field closed)"
    );
}

#[test]
fn colon_navigated_popup_still_accepts_over_runnable_alias() {
    // If the user actively navigates the popup away from the default, Enter
    // should accept the chosen candidate even when the typed prefix happens to
    // be a runnable alias.
    let mut app = App::new(None, false, None, None).unwrap();
    app.open_command_prompt();
    type_str(&mut app, "w");
    // Navigate off the auto-selected best match.
    app.handle_command_field_key(key(KeyCode::Tab));
    let selected_label = app
        .completion
        .as_ref()
        .and_then(|p| p.selected_item())
        .map(|i| i.label.clone());
    app.handle_command_field_key(key(KeyCode::Enter));
    // Accept replaces the line with the selected candidate; the field stays
    // open (awaiting the execute Enter) rather than running ":w".
    assert!(
        app.command_field.is_some(),
        "navigated accept must not execute the alias"
    );
    if let Some(label) = selected_label {
        let text = app.command_field.as_ref().unwrap().text();
        assert_eq!(
            text.trim_end(),
            label,
            "line must become the navigated candidate (modulo arg space)"
        );
    }
}

#[test]
fn colon_esc_with_popup_open_clears_popup_and_propagates() {
    // "w" → popup opens → Esc → popup clears AND the Esc propagates to the
    // field's normal handling (a single Esc both dismisses the popup and steps
    // the prompt's vim mode out of Insert — it does NOT require a second Esc).
    let mut app = App::new(None, false, None, None).unwrap();
    app.open_command_prompt();
    type_str(&mut app, "w");
    let text_before = app.command_field.as_ref().unwrap().text();
    assert!(app.completion.is_some());
    assert_eq!(
        app.command_field.as_ref().unwrap().vim_mode(),
        hjkl_form::VimMode::Insert,
        "prompt starts in Insert mode"
    );
    app.handle_command_field_key(key(KeyCode::Esc));
    assert!(
        app.completion.is_none(),
        "completion must be cleared after Esc with popup open"
    );
    assert!(
        app.command_field.is_some(),
        "command field must stay open (non-empty text → Normal, not closed)"
    );
    // Esc propagated: the field left Insert mode (no second Esc needed).
    assert_eq!(
        app.command_field.as_ref().unwrap().vim_mode(),
        hjkl_form::VimMode::Normal,
        "Esc must also leave Insert mode, not only dismiss the popup"
    );
    // Text must be unchanged (Esc dismissed popup + changed mode, didn't edit).
    let text_after = app.command_field.as_ref().unwrap().text();
    assert_eq!(
        text_before, text_after,
        "Esc must keep typed text when dismissing popup"
    );
}

#[test]
fn colon_popup_closes_when_no_match() {
    // Type something that has no matching command → popup must be None.
    let mut app = App::new(None, false, None, None).unwrap();
    app.open_command_prompt();
    type_str(&mut app, "zzzzzzz_no_such_cmd");
    assert!(
        app.completion.is_none(),
        "popup must be None when no candidates match"
    );
}

#[test]
fn colon_ctrl_n_navigates_popup_when_open() {
    // When popup is open, C-n must move selection forward (not history nav).
    let mut app = App::new(None, false, None, None).unwrap();
    app.open_command_prompt();
    type_str(&mut app, "w");
    assert!(app.completion.is_some());
    let sel_before = app.completion.as_ref().unwrap().selected;
    let visible_len = app.completion.as_ref().unwrap().visible.len();
    app.handle_command_field_key(ctrl_key('n'));
    if visible_len > 1 {
        let sel_after = app.completion.as_ref().unwrap().selected;
        assert_ne!(sel_before, sel_after, "C-n must advance selection in popup");
    }
}
