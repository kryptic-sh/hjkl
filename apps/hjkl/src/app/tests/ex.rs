use super::*;

// ── :write / :write! disk-state guard tests ─────────────────────────────

#[test]
fn colon_write_blocked_by_disk_state_guard_without_bang() {
    let path = std::env::temp_dir().join("hjkl_write_no_bang_guard.txt");
    std::fs::write(&path, "original\n").unwrap();
    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
    // Mark disk changed without reloading buffer, then dirty the buffer.
    app.active_mut().disk_state = DiskState::ChangedOnDisk;
    app.active_mut().dirty = true;
    seed_buffer(&mut app, "edited\n");
    // :write without bang must refuse.
    app.dispatch_ex("write");
    let msg = app.bus.last_body_or_empty().to_string();
    assert!(
        msg.contains("E13"),
        "expected E13 guard message, got: {msg}"
    );
    // File on disk must be unchanged.
    let on_disk = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        on_disk, "original\n",
        "disk must be unchanged after blocked :w"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn colon_write_bang_overrides_disk_state_guard() {
    let path = std::env::temp_dir().join("hjkl_write_bang_guard.txt");
    std::fs::write(&path, "original\n").unwrap();
    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
    app.active_mut().disk_state = DiskState::ChangedOnDisk;
    app.active_mut().dirty = true;
    seed_buffer(&mut app, "edited\n");
    // :write! must force-save.
    app.dispatch_ex("write!");
    let msg = app.bus.last_body_or_empty().to_string();
    assert!(!msg.contains("E13"), ":w! must not produce E13, got: {msg}");
    // disk_state must be reset to Synced.
    assert_eq!(
        app.active().disk_state,
        DiskState::Synced,
        "disk_state must be Synced after :w!"
    );
    // File on disk must contain the new content.
    let on_disk = std::fs::read_to_string(&path).unwrap();
    assert!(
        on_disk.contains("edited"),
        "disk must have new content after :w!"
    );
    let _ = std::fs::remove_file(&path);
}

// ── :e tests ────────────────────────────────────────────────────────────

#[test]
fn edit_percent_reloads_current_file() {
    let path = std::env::temp_dir().join("hjkl_edit_percent_reload.txt");
    std::fs::write(&path, "first\nsecond\n").unwrap();
    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
    std::fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();
    app.dispatch_ex("e %");
    let lines = app
        .active()
        .editor
        .buffer()
        .rope()
        .lines()
        .map(|s| {
            let s = s.to_string();
            s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
        })
        .collect::<Vec<_>>();
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
    assert_eq!(
        app.active()
            .editor
            .buffer()
            .rope()
            .lines()
            .map(|s| {
                let s = s.to_string();
                s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
            })
            .collect::<Vec<_>>(),
        vec!["v2".to_string()]
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn edit_blocks_dirty_buffer_without_force() {
    let path = std::env::temp_dir().join("hjkl_edit_dirty_block.txt");
    std::fs::write(&path, "orig\n").unwrap();
    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
    app.active_mut().dirty = true;
    app.dispatch_ex("e %");
    let msg = app.bus.last_body_or_empty().to_string();
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
        app.active()
            .editor
            .buffer()
            .rope()
            .lines()
            .map(|s| {
                let s = s.to_string();
                s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
            })
            .collect::<Vec<_>>(),
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
    hjkl_vim_tui::handle_key(&mut app.active_mut().editor, key(KeyCode::Char('i')));
    hjkl_vim_tui::handle_key(&mut app.active_mut().editor, key(KeyCode::Char('X')));
    if app.active_mut().editor.take_dirty() {
        app.active_mut().refresh_dirty_against_saved();
    }
    assert!(app.active().dirty, "edit should mark dirty");
    hjkl_vim_tui::handle_key(&mut app.active_mut().editor, key(KeyCode::Esc));
    hjkl_vim_tui::handle_key(&mut app.active_mut().editor, key(KeyCode::Char('u')));
    if app.active_mut().editor.take_dirty() {
        app.active_mut().refresh_dirty_against_saved();
    }
    assert!(
        !app.active().dirty,
        "undo to saved state should clear dirty"
    );
    let _ = std::fs::remove_file(&path);
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
        app.active()
            .editor
            .buffer()
            .rope()
            .lines()
            .map(|s| {
                let s = s.to_string();
                s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
            })
            .collect::<Vec<_>>(),
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
    let msg = app.bus.last_body_or_empty().to_string();
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
    assert_eq!(
        app.active()
            .editor
            .buffer()
            .rope()
            .lines()
            .map(|s| {
                let s = s.to_string();
                s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
            })
            .collect::<Vec<_>>(),
        vec!["a".to_string()]
    );
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
    let lines = app
        .active()
        .editor
        .buffer()
        .rope()
        .lines()
        .map(|s| {
            let s = s.to_string();
            s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
        })
        .collect::<Vec<_>>();
    assert!(
        lines.is_empty() || (lines.len() == 1 && lines[0].is_empty()),
        "expected empty scratch buffer, got: {lines:?}"
    );
    let _ = std::fs::remove_file(&path);
}

// ── :bwipeout tests (#101) ──────────────────────────────────────────────

#[test]
fn bwipeout_clears_marks_on_last_slot() {
    let path = std::env::temp_dir().join("hjkl_bwipeout_marks_last.txt");
    std::fs::write(&path, "hello\n").unwrap();
    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
    // Set a mark on the buffer.
    app.active_mut().editor.set_mark('a', (0, 0));
    assert!(
        app.active().editor.mark('a').is_some(),
        "mark should be set before wipe"
    );
    app.dispatch_ex("bw");
    // After wipe the fresh scratch buffer must have no marks.
    assert!(
        app.active().editor.mark('a').is_none(),
        ":bwipeout on last slot must not carry marks into scratch buffer"
    );
    assert_eq!(app.slots.len(), 1);
    assert!(app.active().filename.is_none());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn bwipeout_clears_jumplist_on_last_slot() {
    let path = std::env::temp_dir().join("hjkl_bwipeout_jumps_last.txt");
    std::fs::write(&path, "line1\nline2\nline3\n").unwrap();
    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
    // Push an entry into the jumplist.
    app.active_mut().editor.record_jump((2, 0));
    let (back, _) = app.active().editor.jump_list();
    assert!(
        !back.is_empty(),
        "jumplist should have an entry before wipe"
    );
    app.dispatch_ex("bw");
    // After wipe the fresh scratch buffer must have an empty jumplist.
    let (back, fwd) = app.active().editor.jump_list();
    assert!(
        back.is_empty() && fwd.is_empty(),
        ":bwipeout on last slot must not carry jumps into scratch buffer"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn bdelete_does_not_explicitly_wipe_marks_path() {
    // Verify :bdelete and :bwipeout both reach distinct code paths.
    // With two slots, :bdelete removes the active slot (different slot survives).
    let path_a = std::env::temp_dir().join("hjkl_bdelete_path_a.txt");
    let path_b = std::env::temp_dir().join("hjkl_bdelete_path_b.txt");
    std::fs::write(&path_a, "a\n").unwrap();
    std::fs::write(&path_b, "b\n").unwrap();
    let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
    app.dispatch_ex(&format!("e {}", path_b.display()));
    assert_eq!(app.slots.len(), 2);
    // Set a mark on slot 1 (path_b).
    app.active_mut().editor.set_mark('z', (0, 0));
    // :bd removes slot 1; slot 0 (path_a) survives with its own editor.
    app.dispatch_ex("bd");
    assert_eq!(app.slots.len(), 1);
    // The surviving slot (path_a) never had mark 'z'.
    assert!(
        app.active().editor.mark('z').is_none(),
        "mark from removed slot must not survive after :bd"
    );
    let _ = std::fs::remove_file(&path_a);
    let _ = std::fs::remove_file(&path_b);
}

#[test]
fn bwipeout_multi_slot_removes_slot() {
    let path_a = std::env::temp_dir().join("hjkl_bwipeout_multi_a.txt");
    let path_b = std::env::temp_dir().join("hjkl_bwipeout_multi_b.txt");
    std::fs::write(&path_a, "a\n").unwrap();
    std::fs::write(&path_b, "b\n").unwrap();
    let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
    app.dispatch_ex(&format!("e {}", path_b.display()));
    assert_eq!(app.slots.len(), 2);
    app.dispatch_ex("bw");
    assert_eq!(app.slots.len(), 1);
    let msg = app.bus.last_body_or_empty().to_string();
    assert!(
        msg.contains("wiped"),
        "expected wipe status message, got: {msg}"
    );
    let _ = std::fs::remove_file(&path_a);
    let _ = std::fs::remove_file(&path_b);
}

#[test]
fn bwipeout_blocks_dirty_without_force() {
    let path_a = std::env::temp_dir().join("hjkl_bwipeout_dirty_a.txt");
    let path_b = std::env::temp_dir().join("hjkl_bwipeout_dirty_b.txt");
    std::fs::write(&path_a, "a\n").unwrap();
    std::fs::write(&path_b, "b\n").unwrap();
    let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
    app.dispatch_ex(&format!("e {}", path_b.display()));
    app.active_mut().dirty = true;
    app.dispatch_ex("bw");
    let msg = app.bus.last_body_or_empty().to_string();
    assert!(msg.contains("E89"), "expected E89, got: {msg}");
    assert_eq!(
        app.slots.len(),
        2,
        "slot must not be removed when dirty without force"
    );
    let _ = std::fs::remove_file(&path_a);
    let _ = std::fs::remove_file(&path_b);
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
    let msg = app.bus.last_body_or_empty().to_string();
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
    let msg = app.bus.last_body_or_empty().to_string();
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
    let msg = app.bus.last_body_or_empty().to_string();
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
    let msg = app.bus.last_body_or_empty().to_string();
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

/// Phase 1 of the hjkl-ex extraction (kryptic-sh/hjkl#73): `:q!` on a dirty
/// buffer must force-quit even though the buffer is unsaved. This exercises
/// the new `hjkl_ex::try_dispatch` → `bridge_ex_effect` path; if either side
/// regresses (registry stops resolving `q!` or the bridge drops `force=true`)
/// the dirty buffer would block the exit and the assertion fails.
#[test]
fn q_bang_force_quits_dirty_buffer_via_hjkl_ex() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "unsaved work");
    // Mark the buffer dirty so a plain `:q` would refuse with E37.
    app.active_mut().dirty = true;
    app.dispatch_ex("q!");
    assert!(
        app.exit_requested,
        "`:q!` must force-quit a dirty buffer (hjkl-ex Phase 1 routing)"
    );
}

// ── checktime / disk-change detection tests ────────────────────────────

/// Helper: bump mtime by writing a file then sleeping briefly so the
/// filesystem timestamp advances past the stored baseline.
#[test]
fn checktime_reloads_clean_buffer_when_disk_changed() {
    let path = std::env::temp_dir().join("hjkl_ct_reload.txt");
    std::fs::write(&path, "line1\nline2\n").unwrap();
    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
    assert_eq!(
        app.active()
            .editor
            .buffer()
            .rope()
            .lines()
            .map(|s| {
                let s = s.to_string();
                s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
            })
            .collect::<Vec<_>>(),
        vec!["line1", "line2"]
    );

    write_and_wait(&path, "new content\n");
    app.checktime_all();

    assert_eq!(
        app.active()
            .editor
            .buffer()
            .rope()
            .lines()
            .map(|s| {
                let s = s.to_string();
                s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
            })
            .collect::<Vec<_>>(),
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
        app.active()
            .editor
            .buffer()
            .rope()
            .lines()
            .map(|s| {
                let s = s.to_string();
                s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
            })
            .collect::<Vec<_>>(),
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
    assert_eq!(
        app.active()
            .editor
            .buffer()
            .rope()
            .lines()
            .map(|s| {
                let s = s.to_string();
                s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
            })
            .collect::<Vec<_>>(),
        vec!["content"]
    );
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
        app.active()
            .editor
            .buffer()
            .rope()
            .lines()
            .map(|s| {
                let s = s.to_string();
                s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
            })
            .collect::<Vec<_>>(),
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
    let lines = app
        .active()
        .editor
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
        vec!["bar bar", "bar"],
        "buffer should be fully substituted"
    );
    let msg = app.bus.last_body_or_empty().to_string();
    assert_eq!(msg, "3 substitutions on 2 lines", "status: {msg}");
}

/// `:s/foo/bar/` on the current line replaces only the first occurrence.
#[test]
fn substitute_current_line_first_only() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "foo foo\nfoo");
    app.dispatch_ex("s/foo/bar/");
    let lines = app
        .active()
        .editor
        .buffer()
        .rope()
        .lines()
        .map(|s| {
            let s = s.to_string();
            s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
        })
        .collect::<Vec<_>>();
    assert_eq!(lines[0], "bar foo", "only first occurrence on current line");
    assert_eq!(lines[1], "foo", "second line unchanged");
    let msg = app.bus.last_body_or_empty().to_string();
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
    let lines = app
        .active()
        .editor
        .buffer()
        .rope()
        .lines()
        .map(|s| {
            let s = s.to_string();
            s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        lines[0], "hello planet",
        "should replace using last search pattern"
    );
    let msg = app.bus.last_body_or_empty().to_string();
    assert_eq!(msg, "1 substitutions on 1 lines", "status: {msg}");
}

/// `:s/foo/bar/` with no match leaves the buffer unchanged and shows "Pattern not found".
#[test]
fn substitute_no_match_shows_pattern_not_found() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world");
    app.dispatch_ex("s/xyz/bar/");
    let lines = app
        .active()
        .editor
        .buffer()
        .rope()
        .lines()
        .map(|s| {
            let s = s.to_string();
            s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
        })
        .collect::<Vec<_>>();
    assert_eq!(lines[0], "hello world", "buffer should be unchanged");
    let msg = app.bus.last_body_or_empty().to_string();
    assert_eq!(msg, "Pattern not found", "status: {msg}");
}

// ── :Anvil ex command tests ───────────────────────────────────────────────────
#[test]
fn anvil_install_unknown_tool_sets_error_message() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("Anvil install definitely-not-a-real-tool-xyz");
    let msg = app.bus.last_body_or_empty().to_string();
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
    let msg = app.bus.last_body_or_empty().to_string();
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
    let msg = app.bus.last_body_or_empty().to_string();
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
    let msg = app.bus.last_body_or_empty().to_string();
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

// ── Phase 2b hjkl-ex integration: :e <path> ────────────────────────────────

/// `:e <path>` dispatched via hjkl_ex::try_dispatch must open the file and
/// make its content visible in the active buffer.  This exercises the
/// EditFile early-intercept in dispatch_ex introduced in Phase 2b.
#[test]
fn colon_e_path_opens_file_via_hjkl_ex() {
    // Write a temp file with known content.
    let path = tmp_path("hjkl_ex_2b_edit_test.txt");
    std::fs::write(&path, "hello from hjkl-ex\n").unwrap();

    let mut app = App::new(None, false, None, None).unwrap();
    // dispatch_ex expects a command string without the leading `:`.
    app.dispatch_ex(&format!("e {}", path.display()));

    let lines = app
        .active()
        .editor
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
        vec!["hello from hjkl-ex"],
        "`:e <path>` must load the file content into the active buffer; got {lines:?}"
    );
    let active_path = app
        .active()
        .filename
        .as_deref()
        .unwrap_or(std::path::Path::new(""));
    assert_eq!(
        active_path,
        path.as_path(),
        "`:e <path>` must set the active slot filename to the opened path"
    );

    let _ = std::fs::remove_file(&path);
}

/// `:bd` dispatched via hjkl_ex::try_dispatch on the only slot must reset
/// the buffer to an empty unnamed scratch (vim parity).
#[test]
fn colon_bd_via_hjkl_ex_clears_sole_buffer() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "some content");
    // Mark clean so buffer_delete doesn't refuse.
    app.active_mut().dirty = false;
    app.dispatch_ex("bd");
    let lines = app
        .active()
        .editor
        .buffer()
        .rope()
        .lines()
        .map(|s| {
            let s = s.to_string();
            s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
        })
        .collect::<Vec<_>>();
    // After :bd on the last slot the buffer is reset to empty unnamed scratch.
    assert_eq!(
        lines,
        vec![""],
        "`:bd` on sole buffer must leave an empty scratch; got {lines:?}"
    );
    assert!(
        app.active().filename.is_none(),
        "`:bd` on sole buffer must clear the filename"
    );
}

// ── Phase 7: filename expansion (`%`, `#`) integration tests ────────────────

/// `dispatch_ex("e %")` with a current filename set must re-open the same
/// file (no error) — the `%` expands to the current buffer path at dispatch
/// time.
#[test]
fn colon_e_percent_expands_to_current_file() {
    let path = tmp_path("hjkl_phase7_percent_test.txt");
    std::fs::write(&path, "phase7 percent\n").unwrap();

    let mut app = App::new(None, false, None, None).unwrap();
    // Open the file by path first so active().filename is set.
    app.dispatch_ex(&format!("e {}", path.display()));
    let active_after_first_open = app
        .active()
        .filename
        .as_deref()
        .unwrap_or(std::path::Path::new(""))
        .to_path_buf();

    // Now dispatch `e %` — should expand to the same path and re-open.
    app.dispatch_ex("e %");

    let active_after_percent = app
        .active()
        .filename
        .as_deref()
        .unwrap_or(std::path::Path::new(""))
        .to_path_buf();

    assert_eq!(
        active_after_percent, active_after_first_open,
        "`:e %%` must expand to the current file path; got {active_after_percent:?}"
    );
    // No error message — expansion and re-open succeeded.
    assert!(
        app.bus.last_body_or_empty().is_empty() || !app.bus.last_body_or_empty().starts_with('E'),
        "`:e %%` must not produce an error; got: {:?}",
        app.bus.last_body_or_empty()
    );

    let _ = std::fs::remove_file(&path);
}

/// `dispatch_ex("e #")` after opening two files must switch back to the
/// first file (alternate buffer expansion).
#[test]
fn colon_e_hash_expands_to_alt() {
    let path_a = tmp_path("hjkl_phase7_hash_a.txt");
    let path_b = tmp_path("hjkl_phase7_hash_b.txt");
    std::fs::write(&path_a, "file a\n").unwrap();
    std::fs::write(&path_b, "file b\n").unwrap();

    let mut app = App::new(None, false, None, None).unwrap();
    // Open file A.
    app.dispatch_ex(&format!("e {}", path_a.display()));
    // Open file B — now A becomes the alternate buffer (prev_active).
    app.dispatch_ex(&format!("e {}", path_b.display()));

    let active_before = app
        .active()
        .filename
        .as_deref()
        .map(|p| p.to_path_buf())
        .unwrap();
    assert!(
        active_before.ends_with("hjkl_phase7_hash_b.txt"),
        "sanity: active must be B; got {active_before:?}"
    );

    // `e #` must expand to file A and open it.
    app.dispatch_ex("e #");

    let active_after = app
        .active()
        .filename
        .as_deref()
        .map(|p| p.to_path_buf())
        .unwrap();
    assert!(
        active_after.ends_with("hjkl_phase7_hash_a.txt"),
        "`:e #` must expand to alt (file A); got {active_after:?}"
    );

    let _ = std::fs::remove_file(&path_a);
    let _ = std::fs::remove_file(&path_b);
}

// ── Phase 4b: host-registry window/tab command tests ────────────────────────

#[test]
fn colon_split_via_host_registry() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello");
    let before = app.layout().leaves().len();
    app.dispatch_ex("split");
    assert_eq!(
        app.layout().leaves().len(),
        before + 1,
        ":split must add one leaf"
    );
}

#[test]
fn colon_sp_alias_via_host_registry() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello");
    let before = app.layout().leaves().len();
    app.dispatch_ex("sp");
    assert_eq!(
        app.layout().leaves().len(),
        before + 1,
        ":sp alias must add one leaf"
    );
}

#[test]
fn colon_vsplit_via_host_registry() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello");
    let before = app.layout().leaves().len();
    app.dispatch_ex("vsplit");
    assert_eq!(
        app.layout().leaves().len(),
        before + 1,
        ":vsplit must add one leaf"
    );
}

#[test]
fn colon_close_via_host_registry() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello");
    app.dispatch_ex("split");
    assert_eq!(app.layout().leaves().len(), 2, "setup: need 2 leaves");
    app.dispatch_ex("close");
    assert_eq!(
        app.layout().leaves().len(),
        1,
        ":close must collapse back to 1 leaf"
    );
}

#[test]
fn colon_tabnew_via_host_registry() {
    let mut app = App::new(None, false, None, None).unwrap();
    let before = app.tabs.len();
    app.dispatch_ex("tabnew");
    assert_eq!(app.tabs.len(), before + 1, ":tabnew must add a tab");
}

#[test]
fn colon_tabprev_via_host_registry() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("tabnew");
    app.dispatch_ex("tabnew");
    // active_tab = 2; go back one.
    let before = app.active_tab;
    app.dispatch_ex("tabprev");
    assert_eq!(
        app.active_tab,
        before - 1,
        ":tabprev must decrement active_tab"
    );
}

#[test]
fn colon_tabclose_via_host_registry() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("tabnew");
    assert_eq!(app.tabs.len(), 2, "setup: need 2 tabs");
    app.dispatch_ex("tabclose");
    assert_eq!(app.tabs.len(), 1, ":tabclose must remove a tab");
}

#[test]
fn colon_only_via_host_registry() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "data");
    app.dispatch_ex("split");
    app.dispatch_ex("split");
    assert!(
        app.layout().leaves().len() >= 2,
        "setup: need at least 2 leaves"
    );
    app.dispatch_ex("only");
    assert_eq!(
        app.layout().leaves().len(),
        1,
        ":only must leave exactly 1 leaf"
    );
}

// ── Phase 4c: buffer-nav host-registry tests ─────────────────────────────────

#[test]
fn colon_bnext_via_host_registry() {
    let mut app = setup_three_slot_app();
    assert_eq!(app.active_index(), 2);
    app.dispatch_ex("bnext");
    assert_eq!(app.active_index(), 0, ":bnext must wrap to first slot");
}

#[test]
fn colon_bn_alias_via_host_registry() {
    let mut app = setup_three_slot_app();
    assert_eq!(app.active_index(), 2);
    app.dispatch_ex("bn");
    assert_eq!(app.active_index(), 0, ":bn alias must wrap to first slot");
}

#[test]
fn colon_bprevious_via_host_registry() {
    let mut app = setup_three_slot_app();
    // Start at slot 2, go back to slot 1.
    app.dispatch_ex("bprevious");
    assert_eq!(app.active_index(), 1, ":bprevious must retreat one slot");
}

#[test]
fn colon_bp_alias_via_host_registry() {
    let mut app = setup_three_slot_app();
    app.dispatch_ex("bp");
    assert_eq!(app.active_index(), 1, ":bp alias must retreat one slot");
}

#[test]
fn colon_bfirst_via_host_registry() {
    let mut app = setup_three_slot_app();
    assert_eq!(app.active_index(), 2);
    app.dispatch_ex("bfirst");
    assert_eq!(app.active_index(), 0, ":bfirst must jump to slot 0");
}

#[test]
fn colon_blast_via_host_registry() {
    let mut app = setup_three_slot_app();
    // Switch to first so blast has work to do.
    app.dispatch_ex("bfirst");
    assert_eq!(app.active_index(), 0);
    app.dispatch_ex("blast");
    assert_eq!(
        app.active_index(),
        app.slots.len() - 1,
        ":blast must jump to the last slot"
    );
}

#[test]
fn colon_ls_via_host_registry() {
    let mut app = setup_three_slot_app();
    app.dispatch_ex("ls");
    let msg = app.bus.last_body_or_empty().to_string();
    assert!(!msg.is_empty(), ":ls must produce a status message");
}

#[test]
fn colon_buffers_via_host_registry() {
    let mut app = setup_three_slot_app();
    app.dispatch_ex("buffers");
    let msg = app
        .info_popup
        .as_ref()
        .map(|p| p.content.clone())
        .unwrap_or_else(|| app.bus.last_body_or_empty().to_string());
    assert!(!msg.is_empty(), ":buffers must produce output");
}

#[test]
fn colon_clipboard_via_host_registry() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("clipboard");
    let msg = app
        .info_popup
        .as_ref()
        .map(|p| p.content.clone())
        .unwrap_or_else(|| app.bus.last_body_or_empty().to_string());
    assert!(!msg.is_empty(), ":clipboard must produce output");
}

// ── Phase 4d2: misc host-registry tests ──────────────────────────────────────

#[test]
fn colon_picker_via_host_registry() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert!(app.picker.is_none(), "picker must start None");
    app.dispatch_ex("picker");
    assert!(app.picker.is_some(), ":picker must open the picker");
}

#[test]
fn colon_rg_via_host_registry() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("rg");
    assert!(app.picker.is_some(), ":rg must open the grep picker");
}

#[test]
fn colon_rg_with_pattern_via_host_registry() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("rg fn main");
    assert!(
        app.picker.is_some(),
        ":rg <pattern> must open the grep picker"
    );
}

#[test]
fn leader_slash_opens_grep_picker() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert!(app.picker.is_none(), "picker must start None");
    app.route_chord_key(key(KeyCode::Char(' ')));
    app.route_chord_key(key(KeyCode::Char('/')));
    assert!(
        app.picker.is_some(),
        "<leader>/ must open the grep picker; status={:?}",
        app.bus.last_body_or_empty()
    );
}

/// End-to-end leader-slash → type a query → confirm the rg-backed picker
/// enumerates items. Skips when rg / grep / findstr are all absent.
#[test]
fn leader_slash_grep_picker_populates_items() {
    if std::process::Command::new("rg")
        .arg("--version")
        .output()
        .is_err()
        && std::process::Command::new("grep")
            .arg("--version")
            .output()
            .is_err()
    {
        eprintln!("skipping: no rg or grep on PATH");
        return;
    }

    // Seed a tmp dir with a known match.
    let dir = std::env::temp_dir().join(format!(
        "hjkl_grep_picker_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("findme.txt");
    std::fs::write(&file, "alpha\nUNIQUE_NEEDLE_42\nomega\n").unwrap();

    // App's cwd drives the grep root.
    let orig_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(&dir).unwrap();

    let mut app = App::new(None, false, None, None).unwrap();
    app.route_chord_key(key(KeyCode::Char(' ')));
    app.route_chord_key(key(KeyCode::Char('/')));
    assert!(app.picker.is_some(), "<leader>/ must open the picker");

    // Type query, then poll until rg returns the seeded match.
    for c in "UNIQUE_NEEDLE_42".chars() {
        app.handle_picker_key(key(KeyCode::Char(c)));
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut got_match = false;
    while std::time::Instant::now() < deadline {
        // Drive both refresh (schedules requery) and tick (fires the
        // debounced rg spawn). In production, render.rs does this every frame.
        if let Some(p) = app.picker.as_mut() {
            let _ = p.refresh();
            p.tick(std::time::Instant::now());
            let _ = p.refresh();
        }
        let count = app.picker.as_ref().map(|p| p.matched()).unwrap_or(0);
        if count > 0 {
            got_match = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    std::env::set_current_dir(&orig_cwd).unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        got_match,
        "rg-backed grep picker must return at least one match for the seeded UNIQUE_NEEDLE_42; \
         status={:?}",
        app.bus.last_body_or_empty()
    );
}

#[test]
fn colon_b_numeric_via_host_registry() {
    let mut app = setup_three_slot_app();
    // slots are 0-indexed internally; :b 2 means slot index 1
    app.dispatch_ex("b 2");
    assert_eq!(app.active_index(), 1, ":b 2 must switch to slot index 1");
}

#[test]
fn colon_b_nonexistent_via_host_registry() {
    let mut app = setup_three_slot_app();
    app.dispatch_ex("b nonexistent_buffer_xyz");
    let msg = app.bus.last_body_or_empty().to_string();
    assert!(
        msg.contains("E94") || msg.contains("No matching"),
        ":b nonexistent must set error status"
    );
}

#[test]
fn colon_bpicker_via_host_registry() {
    let mut app = setup_three_slot_app();
    assert!(app.picker.is_none());
    app.dispatch_ex("bpicker");
    assert!(app.picker.is_some(), ":bpicker must open the buffer picker");
}

#[test]
fn colon_checktime_via_host_registry() {
    // checktime_all should not panic on a fresh app
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("checktime");
    // no assertion beyond no-panic; bus may or may not have entries
}

#[test]
fn colon_vnew_via_host_registry() {
    let mut app = App::new(None, false, None, None).unwrap();
    let before = app.slots.len();
    app.dispatch_ex("vnew");
    assert!(app.slots.len() > before, ":vnew must add a new buffer slot");
}

#[test]
fn colon_new_via_host_registry() {
    let mut app = App::new(None, false, None, None).unwrap();
    let before = app.slots.len();
    app.dispatch_ex("new");
    assert!(app.slots.len() > before, ":new must add a new buffer slot");
}

#[test]
fn colon_tabfirst_via_host_registry() {
    let mut app = App::new(None, false, None, None).unwrap();
    // add a second tab so tabfirst has work to do
    app.dispatch_ex("tabnew");
    assert!(app.active_tab > 0 || app.tabs.len() > 1);
    app.dispatch_ex("tabfirst");
    assert_eq!(app.active_tab, 0, ":tabfirst must jump to tab 0");
}

#[test]
fn colon_tablast_via_host_registry() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("tabnew");
    app.dispatch_ex("tabfirst");
    assert_eq!(app.active_tab, 0);
    app.dispatch_ex("tablast");
    let last = app.tabs.len() - 1;
    assert_eq!(app.active_tab, last, ":tablast must jump to the last tab");
}

// ── Phase 4f: host-registry tests ────────────────────────────────────────────

#[test]
fn colon_tabonly_via_host_registry() {
    // Two-tab setup: tabonly must close all but the current tab.
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("tabnew");
    app.dispatch_ex("tabnew");
    assert_eq!(app.tabs.len(), 3);
    // Navigate to middle tab so we aren't on the last one.
    app.dispatch_ex("tabfirst");
    app.dispatch_ex("tabnext");
    assert_eq!(app.active_tab, 1);
    app.dispatch_ex("tabonly");
    assert_eq!(app.tabs.len(), 1, ":tabonly must leave exactly one tab");
    assert_eq!(app.active_tab, 0, ":tabonly must reset active_tab to 0");
}

#[test]
fn colon_tabs_via_host_registry() {
    // Multi-tab setup: :tabs must populate info_popup.
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("tabnew");
    assert_eq!(app.tabs.len(), 2);
    app.info_popup = None;
    app.dispatch_ex("tabs");
    assert!(
        app.info_popup.is_some(),
        ":tabs must set info_popup with tab listing"
    );
    let popup = app.info_popup.as_ref().unwrap();
    assert!(
        popup.content.contains("Tab page 1"),
        "popup must list Tab page 1"
    );
    assert!(
        popup.content.contains("Tab page 2"),
        "popup must list Tab page 2"
    );
}

#[test]
fn colon_lnext_via_host_registry() {
    // No live LSP server — lnext_severity(None) must not panic on empty diag list.
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("lnext");
    // no assertion beyond no-panic
}

#[test]
fn colon_lopen_via_host_registry() {
    // open_diag_picker with no diagnostics: routes through host registry and
    // pushes "no diagnostics" toast (empty-state path, no server needed).
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("lopen");
    let msg = app.bus.last_body_or_empty().to_string();
    assert!(
        msg.contains("no diagnostics"),
        ":lopen with empty diag list must set status 'no diagnostics', got: {msg}"
    );
}

#[test]
fn colon_resize_via_host_registry() {
    // Horizontal split: dispatch `resize +5` must grow the focused window.
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

    app.dispatch_ex("resize +5");

    let ratio_after = if let window::LayoutTree::Split { ratio, .. } = app.layout() {
        *ratio
    } else {
        panic!("expected Split");
    };

    assert!(
        ratio_after > ratio_before,
        ":resize +5 must grow focused window ratio: before={ratio_before} after={ratio_after}"
    );
}

#[test]
fn colon_vertical_resize_via_host_registry() {
    // Vertical split: dispatch `vertical resize +5` must grow focused window width.
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

    app.dispatch_ex("vertical resize +5");

    let ratio_after = if let window::LayoutTree::Split { ratio, .. } = app.layout() {
        *ratio
    } else {
        panic!("expected Split");
    };

    assert!(
        ratio_after > ratio_before,
        ":vertical resize +5 must grow focused window width ratio: before={ratio_before} after={ratio_after}"
    );
}

// ── Swap file / crash-recovery tests (issue #185) ─────────────────────────

/// `:w` must delete the swap file after a successful save.
/// Uses an injected swap_path to avoid mutating XDG env vars in parallel tests.
#[test]
fn colon_write_removes_swap_file() {
    let td = tempfile::tempdir().unwrap();
    let file_path = td.path().join("test_write_swap.txt");
    std::fs::write(&file_path, "hello\n").unwrap();

    let mut app = App::new(Some(file_path.clone()), false, None, None).unwrap();
    app.pending_recovery = None;

    // Inject a swap path directly into the slot.
    let swap_path = td.path().join("test_write_swap.swp");
    app.active_mut().swap_path = Some(swap_path.clone());

    // Seed some dirty content so dirty_gen advances.
    seed_buffer(&mut app, "changed content");
    app.active_mut().dirty = true;

    // Force-write swap.
    let idx = app.focused_slot_idx();
    app.write_swap_for_slot(idx);
    assert!(
        swap_path.exists(),
        "swap file must exist after write_swap_for_slot"
    );

    // Now save the file — should delete the swap.
    app.dispatch_ex("write");
    assert!(!swap_path.exists(), "swap file must be deleted after :w");
}

/// `:preserve` must write the swap immediately.
#[test]
fn colon_preserve_writes_swap() {
    let td = tempfile::tempdir().unwrap();
    let file_path = td.path().join("test_preserve_swap.txt");
    std::fs::write(&file_path, "initial\n").unwrap();

    let mut app = App::new(Some(file_path.clone()), false, None, None).unwrap();
    app.pending_recovery = None;

    // Inject a swap path directly into the slot.
    let swap_path = td.path().join("test_preserve_swap.swp");
    app.active_mut().swap_path = Some(swap_path.clone());

    seed_buffer(&mut app, "modified content");
    app.active_mut().dirty = true;

    // `:preserve` should write the swap.
    app.dispatch_ex("preserve");

    // Verify that the swap file was actually written.
    assert!(swap_path.exists(), "swap file must exist after :preserve");
}

/// Opening a file whose swap is newer than the on-disk version enters
/// recovery state (`pending_recovery.is_some()`).
///
/// This test writes the swap directly to a temp directory and injects the
/// swap_path onto the slot to avoid racing with other tests that mutate
/// XDG_CACHE_HOME.
#[test]
fn open_file_with_newer_swap_enters_recovery_state() {
    let td = tempfile::tempdir().unwrap();
    let file_path = td.path().join("test_recovery.txt");
    std::fs::write(&file_path, "on disk content\n").unwrap();

    let canonical = std::fs::canonicalize(&file_path).unwrap();

    let file_mtime_ms = std::fs::metadata(&file_path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    // Write the swap file directly into the temp dir (bypasses XDG).
    let swap_path = td.path().join("test_recovery.swp");
    let header = hjkl_app::swap::SwapHeader {
        magic: hjkl_app::swap::SwapHeader::MAGIC,
        version: hjkl_app::swap::SwapHeader::VERSION,
        canonical_path: canonical.to_string_lossy().into_owned(),
        file_mtime_unix_ms: file_mtime_ms,
        // Write time 10s after mtime → swap is "newer".
        write_time_unix_ms: file_mtime_ms + 10_000,
        cursor: (0, 0),
        writer_pid: std::process::id(),
    };
    let rope = ropey::Rope::from_str("swap body content");
    hjkl_app::swap::write_swap(&swap_path, &header, &rope).unwrap();

    // Create app without a pre-existing swap so App::new doesn't interfere.
    let mut app = App::new(Some(file_path.clone()), false, None, None).unwrap();
    // Clear any pending_recovery from App::new, then inject our swap path
    // directly and re-check.
    app.pending_recovery = None;
    app.active_mut().swap_path = Some(swap_path.clone());
    let idx = app.focused_slot_idx();
    let recovery_needed = app.check_recovery_on_open(idx);
    assert!(
        recovery_needed,
        "check_recovery_on_open must return true when swap is newer"
    );
    assert!(
        app.pending_recovery.is_some(),
        "pending_recovery must be set when swap is newer than disk"
    );
}

/// Choosing 'y' in the recovery prompt loads the swap body.
#[test]
fn recovery_y_loads_swap_body() {
    let td = tempfile::tempdir().unwrap();
    let file_path = td.path().join("test_recovery_y.txt");
    std::fs::write(&file_path, "on disk\n").unwrap();

    let canonical = std::fs::canonicalize(&file_path).unwrap();

    let file_mtime_ms = std::fs::metadata(&file_path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    let swap_path = td.path().join("test_recovery_y.swp");
    let header = hjkl_app::swap::SwapHeader {
        magic: hjkl_app::swap::SwapHeader::MAGIC,
        version: hjkl_app::swap::SwapHeader::VERSION,
        canonical_path: canonical.to_string_lossy().into_owned(),
        file_mtime_unix_ms: file_mtime_ms,
        write_time_unix_ms: file_mtime_ms + 10_000,
        cursor: (0, 0),
        writer_pid: std::process::id(),
    };
    let rope = ropey::Rope::from_str("recovered content");
    hjkl_app::swap::write_swap(&swap_path, &header, &rope).unwrap();

    let mut app = App::new(Some(file_path.clone()), false, None, None).unwrap();
    app.pending_recovery = None;
    app.active_mut().swap_path = Some(swap_path.clone());
    let idx = app.focused_slot_idx();
    app.check_recovery_on_open(idx);
    assert!(app.pending_recovery.is_some(), "must be in recovery state");

    // Simulate pressing 'y'.
    app.handle_recovery_key(key(crossterm::event::KeyCode::Char('y')));
    assert!(
        app.pending_recovery.is_none(),
        "pending_recovery must be cleared after 'y'"
    );
    let content = app.active().editor.buffer().content_joined();
    assert!(
        content.contains("recovered content"),
        "buffer must contain swap body after 'y', got: {content:?}"
    );
}

// ── #185 recovery must reset syntax (content_reset) ──────────────────────────

#[test]
fn recovery_y_resets_syntax_spans() {
    // Repro for the syntax-highlight-breaks-after-recovery bug: the recovery
    // accept path must signal a content reset (like a normal file open) so the
    // syntax layer drops its stale tree + spans. We assert this via the proxy
    // that `handle_active_content_reset` clears `styled_spans` to empty — which
    // only happens when the content-install flags `pending_content_reset`.
    let td = tempfile::tempdir().unwrap();
    let file_path = td.path().join("recover_syntax.txt");
    std::fs::write(&file_path, "on disk\n").unwrap();
    let canonical = std::fs::canonicalize(&file_path).unwrap();
    let file_mtime_ms = std::fs::metadata(&file_path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    let swap_path = td.path().join("recover_syntax.swp");
    let header = hjkl_app::swap::SwapHeader {
        magic: hjkl_app::swap::SwapHeader::MAGIC,
        version: hjkl_app::swap::SwapHeader::VERSION,
        canonical_path: canonical.to_string_lossy().into_owned(),
        file_mtime_unix_ms: file_mtime_ms,
        write_time_unix_ms: file_mtime_ms + 10_000,
        cursor: (0, 0),
        writer_pid: std::process::id(),
    };
    let rope = ropey::Rope::from_str("recovered body line one\nline two\n");
    hjkl_app::swap::write_swap(&swap_path, &header, &rope).unwrap();

    let mut app = App::new(Some(file_path.clone()), false, None, None).unwrap();
    app.pending_recovery = None;
    app.active_mut().swap_path = Some(swap_path.clone());

    let idx = app.focused_slot_idx();

    // The slot's syntax tree was parsed against the on-disk bytes in
    // build_slot. Recovering must force a fresh parse_initial — i.e. signal a
    // FULL content reset (pending_content_reset), not an incremental ContentEdit
    // — or the retained tree drifts against the swapped-in bytes and
    // highlighting breaks (#185). Drain build_slot's own reset first, then
    // install the recovered body and assert the reset fired.
    let _ = app.active_mut().editor.take_content_reset();
    app.recover_install_content(idx, "recovered body line one\nline two\n", 0, 0);
    assert!(
        app.active_mut().editor.take_content_reset(),
        "recovery content install must signal a full content reset (set_content), \
         not an incremental edit (replace_all) — else the syntax tree drifts"
    );
    let content = app.active().editor.buffer().content_joined();
    assert!(
        content.contains("recovered body line one"),
        "buffer must hold the recovered body, got: {content:?}"
    );
}

// ── PID-lock multi-instance tests (issue #185) ────────────────────────────────

/// Opening a file whose swap was written by a LIVE process (pid 1 = init/launchd,
/// always alive on unix, always != our pid) must be refused with E325.
///
/// Gated `#[cfg(unix)]` because the liveness check uses kill(2) which is
/// unix-only; on non-unix pid_is_alive always returns false (no enforcement).
#[test]
#[cfg(unix)]
fn open_locked_file_refused_when_pid_alive() {
    let td = tempfile::tempdir().unwrap();
    let file_path = td.path().join("locked_file.txt");
    std::fs::write(&file_path, "on disk\n").unwrap();

    let canonical = std::fs::canonicalize(&file_path).unwrap();
    let file_mtime_ms = std::fs::metadata(&file_path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    let swap_path = td.path().join("locked_file.swp");
    // Use pid=1 (init/launchd) — always alive and always != our pid.
    let header = hjkl_app::swap::SwapHeader {
        magic: hjkl_app::swap::SwapHeader::MAGIC,
        version: hjkl_app::swap::SwapHeader::VERSION,
        canonical_path: canonical.to_string_lossy().into_owned(),
        file_mtime_unix_ms: file_mtime_ms,
        write_time_unix_ms: file_mtime_ms + 10_000,
        cursor: (0, 0),
        writer_pid: 1, // pid 1 = init/launchd, always alive, never our pid
    };
    let rope = ropey::Rope::from_str("locked content");
    hjkl_app::swap::write_swap(&swap_path, &header, &rope).unwrap();

    let mut app = App::new(Some(file_path.clone()), false, None, None).unwrap();
    app.pending_recovery = None;
    let slot_count_before = app.slots.len();
    app.active_mut().swap_path = Some(swap_path.clone());
    let idx = app.focused_slot_idx();
    let recovery = app.check_recovery_on_open(idx);

    // Must be refused: check_recovery_on_open returns false AND removes the slot.
    assert!(!recovery, "locked file must not enter recovery state");
    assert!(
        app.pending_recovery.is_none(),
        "pending_recovery must remain None for a locked file"
    );
    // The error message must contain E325.
    let msgs: Vec<&str> = app.bus.history().map(|h| h.body.as_str()).collect();
    assert!(
        msgs.iter().any(|m| m.contains("E325")),
        "E325 error must be reported; got: {msgs:?}"
    );
    // The slot count must have decreased (slot removed on refusal) or stayed
    // the same if there was only one slot (we never drop the last slot).
    if slot_count_before > 1 {
        assert!(
            app.slots.len() < slot_count_before,
            "refused slot must be removed"
        );
    }
}

/// Opening a file whose swap was written by a DEAD process must proceed to
/// the normal recovery prompt (not refused).
///
/// Gated `#[cfg(unix)]` because on non-unix pid_is_alive always returns false,
/// which means liveness is never enforced — but that means the dead-pid path
/// should also work correctly there (no lock; proceed to recovery).
#[test]
#[cfg(unix)]
fn open_with_dead_pid_enters_recovery() {
    let td = tempfile::tempdir().unwrap();
    let file_path = td.path().join("dead_pid_file.txt");
    std::fs::write(&file_path, "on disk\n").unwrap();

    let canonical = std::fs::canonicalize(&file_path).unwrap();
    let file_mtime_ms = std::fs::metadata(&file_path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    let swap_path = td.path().join("dead_pid_file.swp");
    // Use a very high pid that is almost certainly not a live process.
    let header = hjkl_app::swap::SwapHeader {
        magic: hjkl_app::swap::SwapHeader::MAGIC,
        version: hjkl_app::swap::SwapHeader::VERSION,
        canonical_path: canonical.to_string_lossy().into_owned(),
        file_mtime_unix_ms: file_mtime_ms,
        write_time_unix_ms: file_mtime_ms + 10_000,
        cursor: (0, 0),
        writer_pid: 999_999_999, // almost certainly dead
    };
    let rope = ropey::Rope::from_str("recovered from dead pid");
    hjkl_app::swap::write_swap(&swap_path, &header, &rope).unwrap();

    let mut app = App::new(Some(file_path.clone()), false, None, None).unwrap();
    app.pending_recovery = None;
    app.active_mut().swap_path = Some(swap_path.clone());
    let idx = app.focused_slot_idx();
    let recovery = app.check_recovery_on_open(idx);

    assert!(
        recovery,
        "stale (dead-pid) swap must enter recovery state, not be refused"
    );
    assert!(
        app.pending_recovery.is_some(),
        "pending_recovery must be set for a dead-pid swap"
    );
    // Must NOT have an E325 error.
    let msgs: Vec<&str> = app.bus.history().map(|h| h.body.as_str()).collect();
    assert!(
        !msgs.iter().any(|m| m.contains("E325")),
        "E325 must NOT fire for a dead-pid swap; got: {msgs:?}"
    );
}

/// `:recover` with no arg forces the recovery prompt even when the swap's
/// write_time is OLDER than the file on disk (which would normally cause the
/// swap to be deleted as stale).
#[test]
fn colon_recover_forces_prompt_even_when_file_newer() {
    let td = tempfile::tempdir().unwrap();
    let file_path = td.path().join("force_recover.txt");
    std::fs::write(&file_path, "on disk content\n").unwrap();

    let canonical = std::fs::canonicalize(&file_path).unwrap();
    let file_mtime_ms = std::fs::metadata(&file_path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    let swap_path = td.path().join("force_recover.swp");
    // write_time OLDER than file mtime → normally stale-deleted; :recover must override.
    let header = hjkl_app::swap::SwapHeader {
        magic: hjkl_app::swap::SwapHeader::MAGIC,
        version: hjkl_app::swap::SwapHeader::VERSION,
        canonical_path: canonical.to_string_lossy().into_owned(),
        file_mtime_unix_ms: file_mtime_ms,
        write_time_unix_ms: file_mtime_ms.saturating_sub(5_000), // 5s BEFORE file mtime
        cursor: (0, 0),
        writer_pid: std::process::id(), // our own pid → not a lock
    };
    let rope = ropey::Rope::from_str("stale swap body");
    hjkl_app::swap::write_swap(&swap_path, &header, &rope).unwrap();

    let mut app = App::new(Some(file_path.clone()), false, None, None).unwrap();
    app.pending_recovery = None;
    app.active_mut().swap_path = Some(swap_path.clone());

    // :recover (no arg) → must force recovery even though swap is "stale".
    app.dispatch_ex("recover");

    assert!(
        app.pending_recovery.is_some(),
        ":recover must enter recovery state even when swap is older than file"
    );
}

/// `:recover` with no arg when no swap file exists reports a "not found" info
/// message and does NOT enter recovery state.
#[test]
fn colon_recover_no_swap_reports_not_found() {
    let td = tempfile::tempdir().unwrap();
    let file_path = td.path().join("no_swap.txt");
    std::fs::write(&file_path, "some content\n").unwrap();

    let mut app = App::new(Some(file_path.clone()), false, None, None).unwrap();
    app.pending_recovery = None;

    // Point swap_path at a non-existent file.
    let nonexistent_swap = td.path().join("no_swap.swp");
    app.active_mut().swap_path = Some(nonexistent_swap);

    app.dispatch_ex("recover");

    assert!(
        app.pending_recovery.is_none(),
        ":recover with no swap must not enter recovery state"
    );
    let msgs: Vec<&str> = app.bus.history().map(|h| h.body.as_str()).collect();
    assert!(
        msgs.iter().any(|m| m.contains("No swap file found")),
        "must report 'No swap file found'; got: {msgs:?}"
    );
}

// ── Write-on-open / graceful-exit swap tests (#185 multi-instance) ─────────────

/// Opening a named file must write the swap immediately (arm the PID lock) so
/// that a concurrent second instance opening the same unmodified file finds a
/// swap and is refused.
///
/// Verifies via `arm_swap_on_open` called directly against an injected
/// swap_path in a tempdir (avoids XDG env mutation, which is unsafe in the
/// multi-threaded test harness).
#[test]
fn open_writes_swap_immediately() {
    let td = tempfile::tempdir().unwrap();
    let file_path = td.path().join("arm_on_open.txt");
    std::fs::write(&file_path, "initial content\n").unwrap();

    let mut app = App::new(Some(file_path.clone()), false, None, None).unwrap();
    app.pending_recovery = None;

    // Inject a swap path in the tempdir (controlled location).
    let swap_path = td.path().join("arm_on_open.swp");
    app.active_mut().swap_path = Some(swap_path.clone());
    // Reset last_swap_dirty_gen so arm_swap_on_open actually writes.
    app.active_mut().last_swap_dirty_gen = None;

    // The swap must NOT exist yet (we just set the path, haven't written it).
    assert!(
        !swap_path.exists(),
        "swap must not exist before arm_swap_on_open"
    );

    // Arm: simulates what App::new now does after a clean open.
    let idx = app.focused_slot_idx();
    app.arm_swap_on_open(idx);

    assert!(
        swap_path.exists(),
        "swap file must exist on disk immediately after arm_swap_on_open (PID lock)"
    );
}

/// `cleanup_swaps_on_exit` must delete swap files and clear swap_path on every
/// slot. This simulates the graceful-exit path so clean sessions leave no swap.
#[test]
fn graceful_exit_removes_swap() {
    let td = tempfile::tempdir().unwrap();
    let file_path = td.path().join("cleanup_exit.txt");
    std::fs::write(&file_path, "content\n").unwrap();

    let mut app = App::new(Some(file_path.clone()), false, None, None).unwrap();
    app.pending_recovery = None;

    // Inject a swap path in the tempdir and write a real swap file.
    let swap_path = td.path().join("cleanup_exit.swp");
    app.active_mut().swap_path = Some(swap_path.clone());
    let idx = app.focused_slot_idx();
    // seed a dirty gen so write_swap_for_slot writes.
    seed_buffer(&mut app, "dirty content");
    app.write_swap_for_slot(idx);
    assert!(swap_path.exists(), "swap must exist before cleanup");

    // Simulate graceful exit.
    app.cleanup_swaps_on_exit();

    assert!(
        !swap_path.exists(),
        "swap file must be removed after cleanup_swaps_on_exit"
    );
    assert!(
        app.active().swap_path.is_none(),
        "swap_path must be None after cleanup_swaps_on_exit"
    );
}

/// The second-instance lock works end-to-end when the first instance arms the
/// swap at open. Mirrors `open_locked_file_refused_when_pid_alive` but asserts
/// the comment that write-on-open is what makes this scenario real: the swap
/// with writer_pid == our-own-pid simulates a live first instance that just
/// opened (not edited) the file.
///
/// Gated `#[cfg(unix)]` because `pid_is_alive` uses kill(2).
#[test]
#[cfg(unix)]
fn second_instance_refused_after_first_opens_unmodified() {
    let td = tempfile::tempdir().unwrap();
    let file_path = td.path().join("multi_instance.txt");
    std::fs::write(&file_path, "shared content\n").unwrap();

    let canonical = std::fs::canonicalize(&file_path).unwrap();
    let file_mtime_ms = std::fs::metadata(&file_path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    // Write a swap stamped with pid=1 (init/launchd — always alive, never our
    // pid) to simulate the first instance having armed the lock at open.
    let swap_path = td.path().join("multi_instance.swp");
    let header = hjkl_app::swap::SwapHeader {
        magic: hjkl_app::swap::SwapHeader::MAGIC,
        version: hjkl_app::swap::SwapHeader::VERSION,
        canonical_path: canonical.to_string_lossy().into_owned(),
        file_mtime_unix_ms: file_mtime_ms,
        write_time_unix_ms: file_mtime_ms + 1, // just after open, no edits
        cursor: (0, 0),
        writer_pid: 1, // pid 1 = init/launchd, always alive, never our pid
    };
    let rope = ropey::Rope::from_str("shared content");
    hjkl_app::swap::write_swap(&swap_path, &header, &rope).unwrap();

    // Second instance opens the file and injects the pre-existing swap.
    let mut app = App::new(Some(file_path.clone()), false, None, None).unwrap();
    app.pending_recovery = None;
    app.active_mut().swap_path = Some(swap_path.clone());
    let idx = app.focused_slot_idx();
    let recovery = app.check_recovery_on_open(idx);

    // Must be refused with E325.
    assert!(
        !recovery,
        "second instance must be refused when first holds PID lock"
    );
    assert!(
        app.pending_recovery.is_none(),
        "pending_recovery must remain None on refusal"
    );
    let msgs: Vec<&str> = app.bus.history().map(|h| h.body.as_str()).collect();
    assert!(
        msgs.iter().any(|m| m.contains("E325")),
        "E325 must be reported; got: {msgs:?}"
    );
}

#[cfg(unix)]
#[test]
fn open_locked_sole_buffer_is_readonly() {
    // When the locked file is the ONLY buffer (e.g. `hjkl <file>` startup),
    // we can't drop the sole slot — so it opens READ-ONLY with its swap_path
    // cleared, so this process never clobbers the owner's swap or file.
    let td = tempfile::tempdir().unwrap();
    let file_path = td.path().join("locked_sole.txt");
    std::fs::write(&file_path, "owned by another\n").unwrap();
    let canonical = std::fs::canonicalize(&file_path).unwrap();
    let file_mtime_ms = std::fs::metadata(&file_path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    let swap_path = td.path().join("locked_sole.swp");
    let header = hjkl_app::swap::SwapHeader {
        magic: hjkl_app::swap::SwapHeader::MAGIC,
        version: hjkl_app::swap::SwapHeader::VERSION,
        canonical_path: canonical.to_string_lossy().into_owned(),
        file_mtime_unix_ms: file_mtime_ms,
        write_time_unix_ms: file_mtime_ms + 10_000,
        cursor: (0, 0),
        // pid 1 (init/launchd): always alive, never our pid → live lock.
        writer_pid: 1,
    };
    hjkl_app::swap::write_swap(&swap_path, &header, &ropey::Rope::from_str("x")).unwrap();

    let mut app = App::new(Some(file_path.clone()), false, None, None).unwrap();
    app.pending_recovery = None;
    let idx = app.focused_slot_idx();
    assert_eq!(app.slots.len(), 1, "precondition: sole buffer");
    app.active_mut().swap_path = Some(swap_path.clone());

    let recovery = app.check_recovery_on_open(idx);
    assert!(!recovery, "locked sole buffer must not enter recovery");
    assert!(
        app.active().editor.is_readonly(),
        "locked sole buffer must open read-only"
    );
    assert!(
        app.active().swap_path.is_none(),
        "read-only viewer must not own a swap path"
    );

    // The danger the read-only fallback guards against: `:w` from the locked
    // viewer must NOT overwrite the file the owning instance holds. Dirty the
    // buffer and attempt a write — it must be refused (E45 readonly) and the
    // on-disk content must be unchanged.
    seed_buffer(&mut app, "clobbered by viewer\n");
    app.dispatch_ex("write");
    let on_disk = std::fs::read_to_string(&file_path).unwrap();
    assert_eq!(
        on_disk, "owned by another\n",
        "locked read-only viewer must not be able to :w over the owner's file"
    );
    let msgs: Vec<String> = app.bus.history().map(|h| h.body.clone()).collect();
    assert!(
        msgs.iter()
            .any(|m| m.contains("E45") || m.contains("readonly")),
        "write must be refused with a readonly error; got: {msgs:?}"
    );
}

#[cfg(unix)]
#[test]
fn locked_secondary_slot_is_readonly_not_removed() {
    // `hjkl file1 file2` where file2 is locked by another live instance:
    // file2's slot must open READ-ONLY and stay present — not be removed
    // (silently dropping a file the user explicitly asked for) nor left
    // editable. Repro for the multi-file open lock-handling bug.
    let td = tempfile::tempdir().unwrap();
    let file1 = td.path().join("ms_file1.txt");
    let file2 = td.path().join("ms_file2.txt");
    std::fs::write(&file1, "first\n").unwrap();
    std::fs::write(&file2, "second owned\n").unwrap();

    let canon2 = std::fs::canonicalize(&file2).unwrap();
    let mtime2 = std::fs::metadata(&file2)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let swap2 = td.path().join("ms_file2.swp");
    let header = hjkl_app::swap::SwapHeader {
        magic: hjkl_app::swap::SwapHeader::MAGIC,
        version: hjkl_app::swap::SwapHeader::VERSION,
        canonical_path: canon2.to_string_lossy().into_owned(),
        file_mtime_unix_ms: mtime2,
        write_time_unix_ms: mtime2 + 10_000,
        cursor: (0, 0),
        writer_pid: 1, // alive, not us → live lock
    };
    hjkl_app::swap::write_swap(&swap2, &header, &ropey::Rope::from_str("x")).unwrap();

    let mut app = App::new(Some(file1.clone()), false, None, None).unwrap();
    app.pending_recovery = None;
    // Open file2 as a second slot (mirrors CLI `hjkl file1 file2`).
    let idx2 = app.open_new_slot(file2.clone()).unwrap();
    assert_eq!(app.slots.len(), 2, "precondition: two slots");
    app.slots[idx2].swap_path = Some(swap2.clone());

    let recovery = app.check_recovery_on_open(idx2);
    assert!(!recovery, "locked secondary slot must not enter recovery");
    assert_eq!(
        app.slots.len(),
        2,
        "locked secondary slot must NOT be removed — the file was requested"
    );
    assert!(
        app.slots[idx2].editor.is_readonly(),
        "locked secondary slot must open read-only"
    );
    assert!(
        app.slots[idx2].swap_path.is_none(),
        "locked read-only slot must not own a swap path"
    );
}
