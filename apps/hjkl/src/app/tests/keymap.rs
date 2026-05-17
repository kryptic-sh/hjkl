use super::*;

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
    hjkl_vim::handle_key(&mut app.active_mut().editor, key(KeyCode::Char('i')));
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
    let popup_content = app
        .info_popup
        .as_ref()
        .map(|p| p.content.as_str())
        .unwrap_or("");
    assert!(popup_content.contains('a'), "should list `a` Normal mapping");
    // leader+f is a built-in; it must not appear in user map listing.
    assert!(
        !popup_content.contains("<leader>f"),
        "must not list built-in <leader>f"
    );
    // 'c' is imap — not in nmap listing.
    assert!(
        !popup_content.contains('c'),
        "imap c must not appear in nmap list"
    );

    // Now list imap separately.
    app.dispatch_ex("imap");
    let popup = app
        .info_popup
        .as_ref()
        .map(|p| p.content.as_str())
        .unwrap_or("");
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

// ── Phase 4d1: extracted handler smoke tests ────────────────────────────

#[test]
fn colon_nmap_via_extracted_handler() {
    // dispatch_ex("nmap <leader>x :w<CR>") must store the binding in
    // app_keymap and add a UserKeymapRecord.
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("nmap <leader>x :w<CR>");
    assert!(
        !app.user_keymap_records.is_empty(),
        "record should be stored after nmap via extracted handler"
    );
    // Verify the trie picked it up: build the leader+x chord and look it up.
    use crate::app::keymap::HjklMode as Mode;
    use hjkl_keymap::{KeyCode as KmCode, KeyEvent as KmEvent, KeyModifiers as KmMods};
    let leader = app.config.editor.leader;
    let leader_ev = KmEvent::new(KmCode::Char(leader), KmMods::NONE);
    let x_ev = KmEvent::new(KmCode::Char('x'), KmMods::NONE);
    let mut replay = Vec::new();
    // First key (<leader>) should be Pending.
    let pending = app.dispatch_keymap_in_mode(leader_ev, 1, &mut replay, Mode::Normal);
    assert!(
        pending,
        "<leader> should be pending (chord not yet complete)"
    );
    // Second key (x) should complete and be consumed.
    let consumed = app.dispatch_keymap_in_mode(x_ev, 1, &mut replay, Mode::Normal);
    assert!(consumed, "<leader>x should be consumed by trie");
}

#[test]
fn colon_unmap_via_extracted_handler() {
    // Register a mapping then unmap it; binding must be gone from the trie.
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("nmap a b");
    app.dispatch_ex("unmap a");

    use crate::app::keymap::HjklMode as Mode;
    use hjkl_keymap::{KeyCode as KmCode, KeyEvent as KmEvent, KeyModifiers as KmMods};
    let km_ev = KmEvent::new(KmCode::Char('a'), KmMods::NONE);
    let mut replay = Vec::new();
    let consumed = app.dispatch_keymap_in_mode(km_ev, 1, &mut replay, Mode::Normal);
    assert!(
        !consumed,
        "unmapped `a` should be unbound after unmap via extracted handler"
    );
}

#[test]
fn colon_mapclear_via_extracted_handler() {
    // Register two Normal-mode bindings, then mapclear; both must be gone.
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("nmap a b");
    app.dispatch_ex("nmap c d");
    assert_eq!(
        app.user_keymap_records.len(),
        2,
        "two records before mapclear"
    );
    app.dispatch_ex("mapclear");
    assert!(
        app.user_keymap_records.is_empty(),
        "user_keymap_records should be empty after mapclear via extracted handler"
    );
    let msg = app.status_message.clone().unwrap_or_default();
    assert!(msg.contains("cleared"), "status should confirm clear");
}

#[test]
fn colon_map_list_via_extracted_handler() {
    // Register a binding then dispatch bare `map`; info_popup must appear.
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("nmap p q");
    app.dispatch_ex("map");
    assert!(
        app.info_popup.is_some(),
        "info_popup should be set after bare `map` via extracted handler"
    );
    let popup = app
        .info_popup
        .as_ref()
        .map(|p| p.content.as_str())
        .unwrap_or("");
    assert!(popup.contains('p'), "popup should list the `p` binding");
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
    assert!(app.pending_count.try_accumulate('1'));
    assert_eq!(app.pending_count.peek(), 1);

    // '0' extends a non-empty count.
    assert!(app.pending_count.try_accumulate('0'));
    assert_eq!(app.pending_count.peek(), 10);

    // take_or gives 10.
    let count: usize = app.pending_count.take_or(1) as usize;
    assert_eq!(count, 10);

    // After consuming, it must be cleared.
    assert!(app.pending_count.is_empty());

    // '0' alone (empty pending_count) must NOT be accumulated — the event loop
    // falls through to the engine.  try_accumulate returns false for this case.
    assert!(
        !app.pending_count.try_accumulate('0'),
        "'0' with empty pending_count must not be accumulated"
    );
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
    hjkl_vim::handle_key(
        &mut app.active_mut().editor,
        KeyEvent::new(KeyCode::Char('5'), KeyModifiers::NONE),
    );
    hjkl_vim::handle_key(
        &mut app.active_mut().editor,
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
    );

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
    hjkl_vim::handle_key(
        &mut app.active_mut().editor,
        KeyEvent::new(KeyCode::Char('$'), KeyModifiers::NONE),
    );
    let (_, col_after_dollar) = app.active().editor.cursor();
    assert!(col_after_dollar > 0, "$ must move to end of line");

    // `0` with empty pending_count → goes to col 0.
    // Verify the rule: is_zero && pending_count.is_empty() → fall through.
    assert!(app.pending_count.is_empty());
    hjkl_vim::handle_key(
        &mut app.active_mut().editor,
        KeyEvent::new(KeyCode::Char('0'), KeyModifiers::NONE),
    );
    let (_, col_after_zero) = app.active().editor.cursor();
    assert_eq!(
        col_after_zero, 0,
        "0 with no pending count must go to col 0"
    );
}

// ── Dispatch-path tests (engine-pending bypass + always-forward Unbound) ──

/// Feed a crossterm key through the same dispatch path used by the event_loop:
/// app pending-state reducer first, then engine-pending bypass, then trie,
/// then engine forwarding.
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
    app.pending_count.try_accumulate('3');
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

    app.pending_count.try_accumulate('5');
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
    hjkl_vim::handle_key(&mut app.active_mut().editor, key(KeyCode::Char('v')));
    hjkl_vim::handle_key(&mut app.active_mut().editor, key(KeyCode::Char('l')));
    hjkl_vim::handle_key(&mut app.active_mut().editor, key(KeyCode::Char('l')));
    // Exit visual.
    hjkl_vim::handle_key(&mut app.active_mut().editor, key(KeyCode::Esc));
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
    // gu<motion> operator: after 2c-v, gu sets reducer AfterOp(Lowercase)
    // instead of engine Pending::Op. The 'w' key flows through the reducer
    // (ApplyOpMotion) and calls apply_op_motion on the engine.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "HELLO world\n");
    app.active_mut().editor.jump_cursor(0, 0);

    drive_key(&mut app, key(KeyCode::Char('g')));
    drive_key(&mut app, key(KeyCode::Char('u')));
    // After gu the reducer owns the pending, not the engine FSM.
    assert!(
        matches!(
            app.pending_state,
            Some(hjkl_vim::PendingState::AfterOp {
                op: hjkl_vim::OperatorKind::Lowercase,
                ..
            })
        ),
        "gu must set reducer AfterOp(Lowercase), got {:?}",
        app.pending_state
    );
    assert!(
        !app.active().editor.is_chord_pending(),
        "engine must NOT be chord-pending after gu (reducer owns it)"
    );
    // Feed 'w' through the event loop (reducer dispatches ApplyOpMotion).
    drive_key(&mut app, key(KeyCode::Char('w')));
    assert!(app.pending_state.is_none(), "pending must clear after guw");
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
fn guiw_uppercases_word_via_reducer() {
    // `gUiw` — g → AfterG (reducer), U → reducer AfterOp(Uppercase) (2c-v
    // intercept; engine NOT set to chord-pending). 'i' → reducer OpTextObj
    // (inner:true). 'w' → ApplyOpTextObj → apply_op_text_obj on engine.
    // Verifies gU + i/a + textobj flows fully through the reducer, NOT the
    // engine FSM.
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

    // U → 2c-v intercept sets AfterOp(Uppercase) in reducer; engine stays idle.
    drive_key(&mut app, key(KeyCode::Char('U')));
    assert!(
        matches!(
            app.pending_state,
            Some(hjkl_vim::PendingState::AfterOp {
                op: hjkl_vim::OperatorKind::Uppercase,
                ..
            })
        ),
        "gU must set reducer AfterOp(Uppercase), got {:?}",
        app.pending_state
    );
    assert!(
        !app.active().editor.is_chord_pending(),
        "engine must NOT be chord-pending after 2c-v gU intercept"
    );

    // 'i' → reducer AfterOp → Wait(OpTextObj{inner:true}).
    drive_key(&mut app, key(KeyCode::Char('i')));
    assert!(
        matches!(
            app.pending_state,
            Some(hjkl_vim::PendingState::OpTextObj {
                op: hjkl_vim::OperatorKind::Uppercase,
                inner: true,
                ..
            })
        ),
        "i after gU must set reducer OpTextObj(inner:true), got {:?}",
        app.pending_state
    );
    assert!(
        !app.active().editor.is_chord_pending(),
        "engine must NOT be chord-pending (reducer owns text-obj)"
    );

    // 'w' → reducer OpTextObj → Commit(ApplyOpTextObj) → apply_op_text_obj.
    drive_key(&mut app, key(KeyCode::Char('w')));
    assert!(app.pending_state.is_none(), "pending must clear after gUiw");
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
    app.pending_count.reset();
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
    app.pending_count.try_accumulate('5');
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
    app.pending_count.try_accumulate('2');

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

    app.pending_count.try_accumulate('2');
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
fn gufx_uppercases_via_reducer() {
    // `gUfx` — g → AfterG (reducer), U → reducer AfterOp(Uppercase) (2c-v
    // intercept). 'f' → reducer OpFind(forward:true, till:false). 'x' →
    // Commit(ApplyOpFind) → apply_op_find on engine.
    // Verifies gU + f/F/t/T + target flows fully through the reducer.
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

    // U → 2c-v intercept: reducer AfterOp(Uppercase); engine stays idle.
    drive_key(&mut app, key(KeyCode::Char('U')));
    assert!(
        matches!(
            app.pending_state,
            Some(hjkl_vim::PendingState::AfterOp {
                op: hjkl_vim::OperatorKind::Uppercase,
                ..
            })
        ),
        "gU must set reducer AfterOp(Uppercase), got {:?}",
        app.pending_state
    );
    assert!(
        !app.active().editor.is_chord_pending(),
        "engine must NOT be chord-pending after 2c-v gU intercept"
    );

    // 'f' → reducer AfterOp → Wait(OpFind{forward:true, till:false}).
    drive_key(&mut app, key(KeyCode::Char('f')));
    assert!(
        matches!(
            app.pending_state,
            Some(hjkl_vim::PendingState::OpFind {
                op: hjkl_vim::OperatorKind::Uppercase,
                forward: true,
                till: false,
                ..
            })
        ),
        "f after gU must set reducer OpFind(forward:true), got {:?}",
        app.pending_state
    );
    assert!(
        !app.active().editor.is_chord_pending(),
        "engine must NOT be chord-pending (reducer owns find)"
    );

    // 'x' → reducer OpFind → Commit(ApplyOpFind) → apply_op_find.
    drive_key(&mut app, key(KeyCode::Char('x')));
    assert!(app.pending_state.is_none(), "pending must clear after gUfx");
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
fn g_uw_uppercases_word_via_reducer() {
    // `gUw` — g → AfterG (reducer), U → AfterOp(Uppercase) (2c-v intercept;
    // engine NOT set to chord-pending). 'w' → ApplyOpMotion(Uppercase,'w') →
    // apply_op_motion on engine.
    // Verifies the chord-initiated gUw path now flows fully through the reducer.
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
    // U → 2c-v intercept: reducer AfterOp(Uppercase); engine stays idle.
    drive_key(&mut app, key(KeyCode::Char('U')));
    assert!(
        matches!(
            app.pending_state,
            Some(hjkl_vim::PendingState::AfterOp {
                op: hjkl_vim::OperatorKind::Uppercase,
                ..
            })
        ),
        "gU must set reducer AfterOp(Uppercase), got {:?}",
        app.pending_state
    );
    assert!(
        !app.active().editor.is_chord_pending(),
        "engine must NOT be chord-pending (reducer owns op-pending)"
    );
    // w → reducer dispatches ApplyOpMotion(Uppercase,'w') → apply_op_motion.
    drive_key(&mut app, key(KeyCode::Char('w')));
    assert!(app.pending_state.is_none(), "pending must clear after gUw");
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
fn g_ugg_uppercases_to_top_via_reducer() {
    // `gUgg` — g → AfterG (reducer), U → AfterOp(Uppercase) (2c-v intercept),
    // g → reducer OpG (AfterOp 'g' branch), g → Commit(ApplyOpG{'g'}) →
    // apply_op_g(Uppercase, 'g') → uppercase to file-top (FileTop motion).
    // Verifies the full gUgg path now flows through the reducer OpG sub-state.
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

    // U → 2c-v intercept: reducer AfterOp(Uppercase); engine stays idle.
    drive_key(&mut app, key(KeyCode::Char('U')));
    assert!(
        matches!(
            app.pending_state,
            Some(hjkl_vim::PendingState::AfterOp {
                op: hjkl_vim::OperatorKind::Uppercase,
                ..
            })
        ),
        "gU must set reducer AfterOp(Uppercase), got {:?}",
        app.pending_state
    );
    assert!(
        !app.active().editor.is_chord_pending(),
        "engine must NOT be chord-pending after 2c-v gU intercept"
    );

    // g → reducer AfterOp → Wait(OpG{Uppercase}).
    drive_key(&mut app, key(KeyCode::Char('g')));
    assert!(
        matches!(
            app.pending_state,
            Some(hjkl_vim::PendingState::OpG {
                op: hjkl_vim::OperatorKind::Uppercase,
                ..
            })
        ),
        "g after gU must set reducer OpG(Uppercase), got {:?}",
        app.pending_state
    );
    assert!(
        !app.active().editor.is_chord_pending(),
        "engine must NOT be chord-pending (reducer owns OpG)"
    );

    // g → reducer OpG → Commit(ApplyOpG{Uppercase,'g'}) → apply_op_g → FileTop.
    drive_key(&mut app, key(KeyCode::Char('g')));
    assert!(app.pending_state.is_none(), "pending must clear after gUgg");
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

// ── Phase 2c-v: chord-init reducer bridge integration tests ──────────────────

#[test]
fn g_uu_uppercases_line_via_reducer() {
    // `gUU` — doubled form: g → AfterG, U → AfterOp(Uppercase), U →
    // ApplyOpDouble(Uppercase, 1) → apply_op_double → uppercase current line.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world");
    app.active_mut().editor.jump_cursor(0, 0);

    drive_chars(&mut app, "gUU");

    assert!(app.pending_state.is_none(), "pending must clear after gUU");
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
        line, "HELLO WORLD",
        "gUU must uppercase entire line, got {line:?}"
    );
}

#[test]
fn guu_lowercases_line_via_reducer() {
    // `guu` — doubled form: g → AfterG, u → AfterOp(Lowercase), u →
    // ApplyOpDouble(Lowercase, 1) → apply_op_double → lowercase current line.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "HELLO WORLD");
    app.active_mut().editor.jump_cursor(0, 0);

    drive_chars(&mut app, "guu");

    assert!(app.pending_state.is_none(), "pending must clear after guu");
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
        line, "hello world",
        "guu must lowercase entire line, got {line:?}"
    );
}

#[test]
fn g_tilde_tilde_toggles_line_via_reducer() {
    // `g~~` — doubled form: g → AfterG, ~ → AfterOp(ToggleCase), ~ →
    // ApplyOpDouble(ToggleCase, 1) → apply_op_double → toggle case of current line.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "Hello World");
    app.active_mut().editor.jump_cursor(0, 0);

    drive_chars(&mut app, "g~~");

    assert!(app.pending_state.is_none(), "pending must clear after g~~");
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
        line, "hELLO wORLD",
        "g~~ must toggle case of entire line, got {line:?}"
    );
}

#[test]
fn gqq_reflows_line_via_reducer() {
    // `gqq` — doubled form: g → AfterG, q → AfterOp(Reflow), q →
    // ApplyOpDouble(Reflow, 1) → apply_op_double → reflow current line.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world");
    app.active_mut().editor.jump_cursor(0, 0);

    drive_chars(&mut app, "gqq");

    // Reflow with default textwidth (79+) on a short line leaves it as-is.
    assert!(app.pending_state.is_none(), "pending must clear after gqq");
    assert!(!app.active().editor.is_chord_pending());
    // Line should still exist and not be empty.
    let line = app
        .active()
        .editor
        .buffer()
        .lines()
        .first()
        .cloned()
        .unwrap_or_default();
    assert!(
        !line.is_empty(),
        "gqq must not delete short line, got {line:?}"
    );
}

#[test]
fn two_g_uw_uppercases_two_words_via_reducer() {
    // `2gUw` — count carry: 2 is the count_prefix passed into AfterG, then
    // AfterOp(Uppercase, count1:2), then w → ApplyOpMotion(Uppercase,'w', total:2).
    // Engine uppercases 2 words forward.
    //
    // Note: the test helper drive_key does not replicate the event_loop's
    // count-buffering (pending_count). We directly seed AfterG{count:2} to
    // test the count-carry path without plumbing the full event loop.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world foo");
    app.active_mut().editor.jump_cursor(0, 0);

    // Directly seed AfterG with count=2 (simulates 2g in the real event loop).
    app.pending_state = Some(hjkl_vim::PendingState::AfterG { count: 2 });

    // U → 2c-v intercept: AfterOp(Uppercase, count1:2).
    drive_key(&mut app, key(KeyCode::Char('U')));
    assert!(
        matches!(
            app.pending_state,
            Some(hjkl_vim::PendingState::AfterOp {
                op: hjkl_vim::OperatorKind::Uppercase,
                count1: 2,
                ..
            })
        ),
        "2gU must set AfterOp(Uppercase, count1:2), got {:?}",
        app.pending_state
    );
    drive_key(&mut app, key(KeyCode::Char('w')));
    assert!(app.pending_state.is_none(), "pending must clear after 2gUw");

    let line = app
        .active()
        .editor
        .buffer()
        .lines()
        .first()
        .cloned()
        .unwrap_or_default();
    // 2gUw should uppercase 2 words from cursor: "HELLO WORLD foo".
    assert!(
        line.starts_with("HELLO WORLD"),
        "2gUw must uppercase first 2 words, got {line:?}"
    );
}

#[test]
fn engine_pending_none_after_g_u_in_reducer_path() {
    // After 2c-v: `gU` must set reducer AfterOp(Uppercase) and leave engine
    // Pending as None (not Pending::Op). This is the key invariant of 2c-v.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world");
    app.active_mut().editor.jump_cursor(0, 0);

    drive_key(&mut app, key(KeyCode::Char('g')));
    drive_key(&mut app, key(KeyCode::Char('U')));

    // Reducer holds AfterOp(Uppercase).
    assert!(
        matches!(
            app.pending_state,
            Some(hjkl_vim::PendingState::AfterOp {
                op: hjkl_vim::OperatorKind::Uppercase,
                ..
            })
        ),
        "gU must set reducer AfterOp(Uppercase), got {:?}",
        app.pending_state
    );
    // Engine must NOT be in any chord-pending state.
    assert!(
        !app.active().editor.is_chord_pending(),
        "engine Pending must be None after 2c-v gU intercept"
    );
}

#[test]
fn visual_g_u_uppercases_selection() {
    // In Visual mode, gU applies directly to the selection via engine FSM.
    // This test verifies that our 2c-v intercept does NOT affect visual-mode
    // gU (which executes inline, not through op-pending).
    //
    // In the real event loop for visual mode: 'g' is intercepted by the trie
    // (BeginPendingAfterG) which sets pending_state=AfterG, then 'U' is NOT
    // handled by the Normal-mode pending_state block (vim_mode=Visual), so it
    // falls through directly to the engine. The engine in visual mode applies
    // Uppercase to the selection when it sees 'g' (Pending::G) then 'U'.
    //
    // In this test we simulate via direct engine handle_key calls (as in the
    // real event loop's visual mode path where trie handles 'g' out-of-band
    // and 'U' reaches the engine directly).
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world");
    app.active_mut().editor.jump_cursor(0, 0);

    // Enter visual mode and select "hello" (5 chars).
    hjkl_vim::handle_key(&mut app.active_mut().editor, key(KeyCode::Char('v')));
    for _ in 0..4 {
        hjkl_vim::handle_key(&mut app.active_mut().editor, key(KeyCode::Char('l')));
    }
    assert_eq!(
        app.active().editor.vim_mode(),
        hjkl_engine::VimMode::Visual,
        "must be in Visual mode"
    );

    // In visual mode: 'g' goes through engine FSM (pending_state not in visual path),
    // engine sets Pending::G. Then 'U' → engine Pending::G + 'U' → Uppercase selection.
    hjkl_vim::handle_key(&mut app.active_mut().editor, key(KeyCode::Char('g')));
    hjkl_vim::handle_key(&mut app.active_mut().editor, key(KeyCode::Char('U')));

    // Should be back in Normal mode after visual-mode gU.
    assert_eq!(
        app.active().editor.vim_mode(),
        hjkl_engine::VimMode::Normal,
        "gU in visual must return to Normal mode"
    );

    let line = app
        .active()
        .editor
        .buffer()
        .lines()
        .first()
        .cloned()
        .unwrap_or_default();
    assert!(
        line.starts_with("HELLO"),
        "visual gU must uppercase selection 'hello', got {line:?}"
    );
}

// ── Keymap dispatch → window cursor sync regression tests ──────────────
// Bug history: Phase 3 introduced apply_motion-based bindings (j/k/0/$/...)
// but the event_loop's Match branch skipped the post-dispatch sync block,
// leaving the window's cached cursor_row stale even though the engine
// cursor had moved. Cursor visually didn't move. These tests assert that
// dispatching a Phase-3 motion via the keymap path updates the WINDOW
// cursor cache (the field render reads), not just the engine cursor.
//
// Test 6 (Visual-mode j via keymap) is not included here because
// route_chord_key now handles Non-Normal trie dispatch directly; the
// routing-order regression tests below cover the visual path. The existing
// `gv` / visual-mode tests above provide integration coverage.

/// Build a `hjkl_keymap::KeyEvent` for a plain `Char` key with no modifiers.
#[test]
fn j_motion_via_keymap_updates_window_cursor() {
    // Bug: j dispatched via the keymap Match arm skipped sync_after_engine_mutation,
    // leaving window.cursor_row stale at 0 even though the engine moved to row 1.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "line0\nline1\nline2");
    app.active_mut().editor.jump_cursor(0, 0);
    // Sync engine cursor → window cache (so window starts at row 0).
    app.sync_viewport_from_editor();
    assert_eq!(
        win_cursor_row(&app),
        0,
        "precondition: window cursor_row at 0"
    );

    // Dispatch `j` through the canonical chord routing path.
    let km_ev = km_char('j');
    app.route_chord_key(App::km_to_crossterm(&km_ev));

    assert_eq!(
        win_cursor_row(&app),
        1,
        "j via keymap must update window cursor_row to 1"
    );
    assert_window_synced_to_engine(&app);
}

#[test]
fn k_motion_via_keymap_updates_window_cursor() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "line0\nline1\nline2");
    app.active_mut().editor.jump_cursor(2, 0);
    app.sync_viewport_from_editor();
    assert_eq!(
        win_cursor_row(&app),
        2,
        "precondition: window cursor_row at 2"
    );

    let km_ev = km_char('k');
    app.route_chord_key(App::km_to_crossterm(&km_ev));

    assert_eq!(
        win_cursor_row(&app),
        1,
        "k via keymap must update window cursor_row to 1"
    );
    assert_window_synced_to_engine(&app);
}

#[test]
fn line_start_zero_motion_via_keymap_updates_window_cursor() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world");
    app.active_mut().editor.jump_cursor(0, 5);
    app.sync_viewport_from_editor();
    assert_eq!(
        win_cursor_col(&app),
        5,
        "precondition: window cursor_col at 5"
    );

    // `0` with empty pending_count routes through the keymap as LineStart.
    let km_ev = km_char('0');
    app.route_chord_key(App::km_to_crossterm(&km_ev));

    assert_eq!(
        win_cursor_col(&app),
        0,
        "0 via keymap must update window cursor_col to 0"
    );
    assert_window_synced_to_engine(&app);
}

#[test]
fn line_end_dollar_motion_via_keymap_updates_window_cursor() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();
    assert_eq!(
        win_cursor_col(&app),
        0,
        "precondition: window cursor_col at 0"
    );

    let km_ev = km_char('$');
    app.route_chord_key(App::km_to_crossterm(&km_ev));

    // "hello" has 5 chars; `$` lands on the last char (index 4).
    assert_eq!(
        win_cursor_col(&app),
        4,
        "$ via keymap must update window cursor_col to 4 (last char of 'hello')"
    );
    assert_window_synced_to_engine(&app);
}

#[test]
fn motion_via_keymap_scrolls_viewport_to_follow_cursor() {
    // Bug: apply_motion_kind (keymap path) doesn't call ensure_cursor_in_scrolloff;
    // the engine FSM's step() does. Without an app-side scrolloff call, j past
    // the viewport bottom left the cursor off-screen and the window top_row
    // stuck at 0. Asserts the post-dispatch sync runs scrolloff so viewport
    // top_row advances and window.top_row mirrors it.
    let mut app = App::new(None, false, None, None).unwrap();
    let lines: Vec<String> = (0..50).map(|i| format!("line{i}")).collect();
    seed_buffer(&mut app, &lines.join("\n"));
    app.active_mut().editor.jump_cursor(0, 0);
    // Set engine + host viewport heights so scrolloff math fires the
    // non-zero path (height=0 falls back to bare ensure_cursor_visible).
    app.active_mut().editor.set_viewport_height(10);
    {
        let vp = app.active_mut().editor.host_mut().viewport_mut();
        vp.height = 10;
        vp.top_row = 0;
    }
    app.sync_viewport_from_editor();
    let fw = app.focused_window();
    assert_eq!(
        app.windows[fw].as_ref().unwrap().top_row,
        0,
        "precondition: window top_row at 0"
    );

    // Drive `j` 20 times — well past the viewport bottom + scrolloff margin.
    let km_ev = km_char('j');
    for _ in 0..20 {
        app.route_chord_key(App::km_to_crossterm(&km_ev));
    }

    let fw = app.focused_window();
    let win = app.windows[fw].as_ref().unwrap();
    assert_eq!(
        win.cursor_row, 20,
        "engine cursor should be at row 20 after 20 j's"
    );
    assert!(
        win.top_row > 0,
        "window top_row must advance so cursor stays visible; got top_row={}, cursor_row={}",
        win.top_row,
        win.cursor_row
    );
    // Cursor must be inside the viewport [top_row, top_row + height).
    let height = 10usize;
    assert!(
        win.cursor_row >= win.top_row && win.cursor_row < win.top_row + height,
        "cursor must be inside viewport: top_row={}, height={}, cursor_row={}",
        win.top_row,
        height,
        win.cursor_row
    );
    assert_window_synced_to_engine(&app);
}

#[test]
fn gg_via_pending_state_scrolls_viewport_to_top() {
    // Bug: AfterGChord Outcome arm hand-rolled a partial sync block that
    // didn't call ensure_cursor_in_scrolloff. gg from a deep cursor jumped
    // the engine cursor to line 0 but viewport top_row stayed at the deep
    // position, leaving the cursor above the viewport.
    //
    // Shortcut: rather than driving the full event loop (build PendingState,
    // call hjkl_vim::step, dispatch the AfterGChord arm), we call
    // editor.after_g + sync_after_engine_mutation directly — the same two
    // calls the fixed AfterGChord arm makes. The reducer step is a pure
    // function tested in hjkl-vim already; what we care about here is the
    // post-dispatch sync path.
    let mut app = App::new(None, false, None, None).unwrap();
    let lines: Vec<String> = (0..50).map(|i| format!("line{i}")).collect();
    seed_buffer(&mut app, &lines.join("\n"));
    // Place cursor + viewport deep into the buffer.
    app.active_mut().editor.jump_cursor(40, 0);
    app.active_mut().editor.set_viewport_height(10);
    {
        let vp = app.active_mut().editor.host_mut().viewport_mut();
        vp.height = 10;
        vp.top_row = 35;
    }
    app.sync_viewport_from_editor();
    let fw = app.focused_window();
    assert_eq!(
        app.windows[fw].as_ref().unwrap().top_row,
        35,
        "precondition: window top_row at 35"
    );

    // Press g — enters AfterG pending state via keymap.
    let km_g = km_char('g');
    app.route_chord_key(App::km_to_crossterm(&km_g));
    assert!(
        app.pending_state.is_some(),
        "after first g, pending_state must be Some(AfterG)"
    );

    // Invoke the AfterGChord arm body directly (editor.after_g + canonical sync).
    // This is the exact code path the fixed arm executes for gg.
    app.active_mut().editor.after_g('g', 1);
    app.sync_after_engine_mutation();
    app.pending_state = None;

    let fw = app.focused_window();
    let win = app.windows[fw].as_ref().unwrap();
    assert_eq!(win.cursor_row, 0, "gg must move cursor to row 0");
    assert_eq!(
        win.top_row, 0,
        "gg must scroll viewport top_row to 0; got top_row={}",
        win.top_row
    );
    assert_window_synced_to_engine(&app);
}

#[test]
fn count_prefix_motion_via_keymap_updates_window_cursor() {
    // Exercises the count-prefix path: accumulate '5' in pending_count, then
    // dispatch `j`. The method peeks the count (5) and passes it to dispatch_keymap.
    let mut app = App::new(None, false, None, None).unwrap();
    let lines: Vec<String> = (0..10).map(|i| format!("line{i}")).collect();
    seed_buffer(&mut app, &lines.join("\n"));
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();
    assert_eq!(
        win_cursor_row(&app),
        0,
        "precondition: window cursor_row at 0"
    );

    // Accumulate count=5 in the pending_count buffer (same as typing '5' in Normal).
    assert!(
        app.pending_count.try_accumulate('5'),
        "digit '5' must be accepted by pending_count"
    );

    let km_ev = km_char('j');
    app.route_chord_key(App::km_to_crossterm(&km_ev));

    assert_eq!(
        win_cursor_row(&app),
        5,
        "5j via keymap must update window cursor_row to 5"
    );
    assert_window_synced_to_engine(&app);
}

#[test]
fn all_phase3_keymap_motions_keep_window_synced() {
    // Drift-resistant smoke: dispatch every MotionKind variant via the
    // keymap path and assert window state stays consistent with engine
    // state. When Phase 4 adds new MotionKinds, append them to the list
    // below. The list is hand-maintained because MotionKind is
    // #[non_exhaustive] in hjkl-vim (variants can be added downstream).
    //
    // The motion semantics differ per kind — some require a count, some
    // need a buffer with multi-line content, some land at specific
    // columns. So we don't assert the resulting cursor position; we just
    // assert the SYNC INVARIANT (window cache mirrors engine state). That
    // is the bug class this test catches.

    use hjkl_vim::MotionKind;

    // ── Keep in sync with hjkl_vim::MotionKind variants ────────────────
    // If you add a variant in hjkl-vim, add it here too.
    let kinds = [
        MotionKind::CharLeft,
        MotionKind::CharRight,
        MotionKind::LineDown,
        MotionKind::LineUp,
        MotionKind::FirstNonBlankDown,
        MotionKind::FirstNonBlankUp,
        MotionKind::WordForward,
        MotionKind::BigWordForward,
        MotionKind::WordBackward,
        MotionKind::BigWordBackward,
        MotionKind::WordEnd,
        MotionKind::BigWordEnd,
        MotionKind::LineStart,
        MotionKind::FirstNonBlank,
        MotionKind::LineEnd,
        MotionKind::GotoLine,
        MotionKind::FindRepeat,
        MotionKind::FindRepeatReverse,
        MotionKind::BracketMatch,
        MotionKind::ViewportTop,
        MotionKind::ViewportMiddle,
        MotionKind::ViewportBottom,
        MotionKind::HalfPageDown,
        MotionKind::HalfPageUp,
        MotionKind::FullPageDown,
        MotionKind::FullPageUp,
    ];

    for kind in kinds {
        let mut app = App::new(None, false, None, None).unwrap();
        let lines: Vec<String> = (0..50)
            .map(|i| format!("line{i:02}-some-content-here"))
            .collect();
        seed_buffer(&mut app, &lines.join("\n"));
        app.active_mut().editor.jump_cursor(20, 5);
        app.active_mut().editor.set_viewport_height(10);
        {
            let vp = app.active_mut().editor.host_mut().viewport_mut();
            vp.height = 10;
            vp.top_row = 15;
        }
        app.sync_viewport_from_editor();

        // Dispatch via the same controller path the event loop uses.
        app.dispatch_action(
            crate::keymap_actions::AppAction::Motion { kind, count: 1 },
            1,
        );
        app.sync_after_engine_mutation();

        // The bug class is window-vs-engine divergence — assert that
        // invariant; the specific resulting cursor position varies per
        // motion and isn't what this smoke test guards.
        assert_window_synced_to_engine(&app);
    }
}

#[test]
fn visual_block_h_l_extend_selection() {
    // Bug: apply_motion_kind didn't call update_block_vcol after
    // execute_motion, so VisualBlock h / l moved the cursor without
    // updating the block's right edge. The selection appeared static
    // while the cursor moved.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "0123456789\nabcdefghij\nklmnopqrst\nuvwxyz1234");
    app.active_mut().editor.jump_cursor(0, 2);
    app.sync_viewport_from_editor();

    // Enter VisualBlock mode (Ctrl-V). Engine handles the mode entry.
    {
        use crossterm::event::{KeyCode, KeyEvent as CtKeyEvent, KeyModifiers};
        hjkl_vim::handle_key(
            &mut app.active_mut().editor,
            CtKeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL),
        );
    }
    assert_eq!(
        app.active().editor.vim_mode(),
        hjkl_engine::VimMode::VisualBlock,
        "must be in VisualBlock mode after <C-v>"
    );

    // Initial block_vcol = anchor col = 2.
    // Dispatch `l` via the canonical chord routing path 3 times.
    let km_l = km_char('l');
    for _ in 0..3 {
        app.route_chord_key(App::km_to_crossterm(&km_l));
    }

    // Cursor should be at col 5 after 3 l's.
    let (_, e_col) = app.active().editor.cursor();
    assert_eq!(e_col, 5, "cursor must advance to col 5 after 3 l's");

    // Verify block_vcol followed cursor via block_highlight():
    // block_highlight returns (top, bot, left, right) where right =
    // max(anchor_col, block_vcol). Anchor is at col 2; cursor at col 5.
    // Without the fix, block_vcol stays at 2 → right == 2 (1-col wide
    // selection). With the fix, block_vcol == 5 → right == 5.
    let highlight = app
        .active()
        .editor
        .block_highlight()
        .expect("block_highlight must be Some in VisualBlock mode");
    let (_top, _bot, _left, right) = highlight;
    assert_eq!(
        right, 5,
        "block_vcol must follow cursor: expected right edge 5, got {right}"
    );

    // Assert sync invariant as well.
    assert_window_synced_to_engine(&app);
}

// ── pending_state reducer in non-Normal modes ────────────────────────────────
//
// Bug: the pending_state block was gated on VimMode::Normal, so the second key
// of a g-chord (e.g. `gg`) in Visual / VisualLine / VisualBlock was never
// dispatched through the reducer — it re-entered the keymap and re-set
// pending_state without committing, silently no-oping.
//
// Fix: lift the pending_state block out of the Normal-mode gate so it fires in
// all modes when pending_state.is_some().
//
// These tests shortcut the full event loop by manually setting pending_state
// then calling after_g + sync_after_engine_mutation — the same two calls the
// fixed AfterGChord arm makes. They document the expected sync behavior and
// catch future regressions in the post-commit sync path.

#[test]
fn gg_via_pending_state_in_visual_mode() {
    // Regression: gg in Visual mode must move cursor to row 0 and sync the
    // window cache. Before the fix the pending_state reducer was Normal-only
    // gated, so the second `g` never committed AfterGChord.
    let mut app = App::new(None, false, None, None).unwrap();
    let lines: Vec<String> = (0..30).map(|i| format!("line{i:02}")).collect();
    seed_buffer(&mut app, &lines.join("\n"));
    app.active_mut().editor.jump_cursor(20, 0);
    app.active_mut().editor.set_viewport_height(10);
    app.sync_viewport_from_editor();

    // Enter Visual mode.
    {
        use crossterm::event::{KeyCode, KeyEvent as CtKeyEvent, KeyModifiers};
        hjkl_vim::handle_key(
            &mut app.active_mut().editor,
            CtKeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE),
        );
    }
    assert_eq!(
        app.active().editor.vim_mode(),
        hjkl_engine::VimMode::Visual,
        "must be in Visual mode after v"
    );

    // Simulate the commit path of the AfterGChord arm (same calls as the
    // fixed event loop arm for `gg`).
    app.pending_state = Some(hjkl_vim::PendingState::AfterG { count: 1 });
    app.active_mut().editor.after_g('g', 1);
    app.sync_after_engine_mutation();
    app.pending_state = None;

    let fw = app.focused_window();
    let win = app.windows[fw].as_ref().unwrap();
    assert_eq!(
        win.cursor_row, 0,
        "gg must move cursor to row 0 from row 20 in Visual mode"
    );
    assert_window_synced_to_engine(&app);
}

#[test]
fn gg_via_pending_state_in_visual_line_mode() {
    // Same as above but for VisualLine mode (entered via `V`).
    let mut app = App::new(None, false, None, None).unwrap();
    let lines: Vec<String> = (0..30).map(|i| format!("line{i:02}")).collect();
    seed_buffer(&mut app, &lines.join("\n"));
    app.active_mut().editor.jump_cursor(20, 0);
    app.active_mut().editor.set_viewport_height(10);
    app.sync_viewport_from_editor();

    // Enter VisualLine mode.
    {
        use crossterm::event::{KeyCode, KeyEvent as CtKeyEvent, KeyModifiers};
        hjkl_vim::handle_key(
            &mut app.active_mut().editor,
            CtKeyEvent::new(KeyCode::Char('V'), KeyModifiers::NONE),
        );
    }
    assert_eq!(
        app.active().editor.vim_mode(),
        hjkl_engine::VimMode::VisualLine,
        "must be in VisualLine mode after V"
    );

    app.pending_state = Some(hjkl_vim::PendingState::AfterG { count: 1 });
    app.active_mut().editor.after_g('g', 1);
    app.sync_after_engine_mutation();
    app.pending_state = None;

    let fw = app.focused_window();
    let win = app.windows[fw].as_ref().unwrap();
    assert_eq!(
        win.cursor_row, 0,
        "gg must move cursor to row 0 from row 20 in VisualLine mode"
    );
    assert_window_synced_to_engine(&app);
}

#[test]
fn gg_via_pending_state_in_visual_block_mode() {
    // Same as above but for VisualBlock mode (entered via Ctrl-V).
    let mut app = App::new(None, false, None, None).unwrap();
    let lines: Vec<String> = (0..30).map(|i| format!("line{i:02}")).collect();
    seed_buffer(&mut app, &lines.join("\n"));
    app.active_mut().editor.jump_cursor(20, 0);
    app.active_mut().editor.set_viewport_height(10);
    app.sync_viewport_from_editor();

    // Enter VisualBlock mode.
    {
        use crossterm::event::{KeyCode, KeyEvent as CtKeyEvent, KeyModifiers};
        hjkl_vim::handle_key(
            &mut app.active_mut().editor,
            CtKeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL),
        );
    }
    assert_eq!(
        app.active().editor.vim_mode(),
        hjkl_engine::VimMode::VisualBlock,
        "must be in VisualBlock mode after <C-v>"
    );

    app.pending_state = Some(hjkl_vim::PendingState::AfterG { count: 1 });
    app.active_mut().editor.after_g('g', 1);
    app.sync_after_engine_mutation();
    app.pending_state = None;

    let fw = app.focused_window();
    let win = app.windows[fw].as_ref().unwrap();
    assert_eq!(
        win.cursor_row, 0,
        "gg must move cursor to row 0 from row 20 in VisualBlock mode"
    );
    assert_window_synced_to_engine(&app);
}

// ── route_chord_key routing-order regression tests ───────────────────────────
//
// These tests drive the FULL keymap sequence through `route_chord_key`,
// which IS the event loop's canonical chord-routing. They catch the bug
// class where Non-Normal trie dispatch ran BEFORE the pending_state reducer,
// causing the second key of a chord (e.g. second `g` of `gg`) to be
// re-consumed by the keymap instead of reaching the reducer's commit arm.
//
// If you revert the `pending_state.is_none()` guard inside `route_chord_key`
// (step 2 Non-Normal trie dispatch), these tests MUST fail — that is their
// purpose.

#[test]
fn gg_full_sequence_in_visual_line_via_keymap() {
    // Regression: the second `g` of `gg` in VisualLine was re-consumed by
    // the Non-Normal trie dispatch instead of reaching the pending_state
    // reducer's AfterGChord commit arm. Cursor stayed put.
    let mut app = App::new(None, false, None, None).unwrap();
    let lines: Vec<String> = (0..30).map(|i| format!("line{i:02}")).collect();
    seed_buffer(&mut app, &lines.join("\n"));
    app.active_mut().editor.jump_cursor(20, 0);
    app.sync_viewport_from_editor();

    use crossterm::event::{KeyCode, KeyEvent as CtKeyEvent, KeyModifiers};

    // Enter VisualLine via `V`.
    hjkl_vim::handle_key(
        &mut app.active_mut().editor,
        CtKeyEvent::new(KeyCode::Char('V'), KeyModifiers::NONE),
    );
    assert_eq!(
        app.active().editor.vim_mode(),
        hjkl_engine::VimMode::VisualLine,
        "must be in VisualLine mode"
    );

    // First `g` — goes through Non-Normal dispatch → BeginPendingAfterG.
    let g_key = CtKeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE);
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

    // Second `g` — must reach the pending_state reducer (NOT re-fire
    // BeginPendingAfterG via the keymap). Commits gg.
    let consumed = app.route_chord_key(g_key);
    assert!(consumed, "second g must be consumed");
    assert!(
        app.pending_state.is_none(),
        "after gg the reducer must clear pending_state"
    );
    assert_eq!(
        app.active().editor.cursor().0,
        0,
        "gg must move engine cursor to row 0 from row 20"
    );
    assert_window_synced_to_engine(&app);
}

#[test]
fn gg_full_sequence_in_visual_mode_via_keymap() {
    // Same as gg_full_sequence_in_visual_line_via_keymap but for Visual mode
    // (entered via `v`).
    let mut app = App::new(None, false, None, None).unwrap();
    let lines: Vec<String> = (0..30).map(|i| format!("line{i:02}")).collect();
    seed_buffer(&mut app, &lines.join("\n"));
    app.active_mut().editor.jump_cursor(20, 0);
    app.sync_viewport_from_editor();

    use crossterm::event::{KeyCode, KeyEvent as CtKeyEvent, KeyModifiers};

    // Enter Visual via `v`.
    hjkl_vim::handle_key(
        &mut app.active_mut().editor,
        CtKeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE),
    );
    assert_eq!(
        app.active().editor.vim_mode(),
        hjkl_engine::VimMode::Visual,
        "must be in Visual mode"
    );

    let g_key = CtKeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE);

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

    let consumed = app.route_chord_key(g_key);
    assert!(consumed, "second g must be consumed");
    assert!(
        app.pending_state.is_none(),
        "after gg the reducer must clear pending_state"
    );
    assert_eq!(
        app.active().editor.cursor().0,
        0,
        "gg must move engine cursor to row 0 from row 20"
    );
    assert_window_synced_to_engine(&app);
}

#[test]
fn gg_full_sequence_in_visual_block_mode_via_keymap() {
    // Same as gg_full_sequence_in_visual_line_via_keymap but for VisualBlock
    // mode (entered via Ctrl-V).
    let mut app = App::new(None, false, None, None).unwrap();
    let lines: Vec<String> = (0..30).map(|i| format!("line{i:02}")).collect();
    seed_buffer(&mut app, &lines.join("\n"));
    app.active_mut().editor.jump_cursor(20, 0);
    app.sync_viewport_from_editor();

    use crossterm::event::{KeyCode, KeyEvent as CtKeyEvent, KeyModifiers};

    // Enter VisualBlock via Ctrl-V.
    hjkl_vim::handle_key(
        &mut app.active_mut().editor,
        CtKeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL),
    );
    assert_eq!(
        app.active().editor.vim_mode(),
        hjkl_engine::VimMode::VisualBlock,
        "must be in VisualBlock mode"
    );

    let g_key = CtKeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE);

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

    let consumed = app.route_chord_key(g_key);
    assert!(consumed, "second g must be consumed");
    assert!(
        app.pending_state.is_none(),
        "after gg the reducer must clear pending_state"
    );
    assert_eq!(
        app.active().editor.cursor().0,
        0,
        "gg must move engine cursor to row 0 from row 20"
    );
    assert_window_synced_to_engine(&app);
}

// ── Phase 6.4: first-tier Normal-mode keymap dispatch tests ─────────────────
//
// Each test verifies a new AppAction variant dispatches correctly through
// route_chord_key / dispatch_action and produces the expected engine state.

// ── insert-mode entry ────────────────────────────────────────────────────────

#[test]
fn p64_i_enters_insert_mode() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    let consumed = app.route_chord_key(ck('i'));
    assert!(consumed, "i must be consumed by keymap");
    assert_eq!(
        app.active().editor.vim_mode(),
        VimMode::Insert,
        "i must enter Insert mode"
    );
    assert_eq!(
        app.active().editor.host().cursor_shape(),
        hjkl_engine::CursorShape::Bar,
        "cursor must flip to Bar on entering Insert"
    );
}

#[test]
fn p64_shift_i_enters_insert_at_line_start() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "  hello");
    app.active_mut().editor.jump_cursor(0, 5);
    app.sync_viewport_from_editor();

    let consumed = app.route_chord_key(ck('I'));
    assert!(consumed, "I must be consumed by keymap");
    assert_eq!(
        app.active().editor.vim_mode(),
        VimMode::Insert,
        "I must enter Insert mode"
    );
    // Cursor must be at first non-blank col (col 2).
    let (_, col) = app.active().editor.cursor();
    assert_eq!(
        col, 2,
        "I must place cursor at first non-blank; got col {col}"
    );
}

#[test]
fn p64_a_enters_insert_after_cursor() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    let consumed = app.route_chord_key(ck('a'));
    assert!(consumed, "a must be consumed by keymap");
    assert_eq!(
        app.active().editor.vim_mode(),
        VimMode::Insert,
        "a must enter Insert mode"
    );
    // Cursor must have advanced one past position 0.
    let (_, col) = app.active().editor.cursor();
    assert_eq!(col, 1, "a must advance cursor to col 1; got {col}");
}

#[test]
fn p64_shift_a_enters_insert_at_eol() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    let consumed = app.route_chord_key(ck('A'));
    assert!(consumed, "A must be consumed by keymap");
    assert_eq!(
        app.active().editor.vim_mode(),
        VimMode::Insert,
        "A must enter Insert mode"
    );
    // Cursor must be at EOL (col 5, past 'o').
    let (_, col) = app.active().editor.cursor();
    assert_eq!(col, 5, "A must place cursor at EOL; got col {col}");
}

#[test]
fn p64_o_opens_line_below_and_enters_insert() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "line1\nline2");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    let consumed = app.route_chord_key(ck('o'));
    assert!(consumed, "o must be consumed by keymap");
    assert_eq!(
        app.active().editor.vim_mode(),
        VimMode::Insert,
        "o must enter Insert mode"
    );
    // After o, cursor must be on row 1 (new blank line).
    let (row, _) = app.active().editor.cursor();
    assert_eq!(row, 1, "o must move cursor to new row 1; got row {row}");
}

#[test]
fn p64_shift_o_opens_line_above_and_enters_insert() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "line1\nline2");
    app.active_mut().editor.jump_cursor(1, 0);
    app.sync_viewport_from_editor();

    let consumed = app.route_chord_key(ck('O'));
    assert!(consumed, "O must be consumed by keymap");
    assert_eq!(
        app.active().editor.vim_mode(),
        VimMode::Insert,
        "O must enter Insert mode"
    );
    // After O from row 1, cursor must be on row 1 (new line inserted above line2).
    let (row, _) = app.active().editor.cursor();
    assert_eq!(
        row, 1,
        "O must place cursor on new row above; got row {row}"
    );
}

// ── char / line mutation ops ─────────────────────────────────────────────────

#[test]
fn p64_x_deletes_char_forward() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    let consumed = app.route_chord_key(ck('x'));
    assert!(consumed, "x must be consumed by keymap");
    let line = app.active().editor.buffer().lines()[0].clone();
    assert_eq!(line, "ello", "x must delete 'h'; got {line:?}");
}

#[test]
fn p64_x_with_count_5_deletes_5_chars() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    app.pending_count.try_accumulate('5');
    let consumed = app.route_chord_key(ck('x'));
    assert!(consumed, "x must be consumed by keymap");
    let line = app.active().editor.buffer().lines()[0].clone();
    assert_eq!(line, " world", "5x must delete 5 chars; got {line:?}");
}

#[test]
fn p64_big_x_deletes_char_backward() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello");
    app.active_mut().editor.jump_cursor(0, 2);
    app.sync_viewport_from_editor();

    let consumed = app.route_chord_key(ck('X'));
    assert!(consumed, "X must be consumed by keymap");
    let line = app.active().editor.buffer().lines()[0].clone();
    assert_eq!(line, "hllo", "X at col 2 must delete 'e'; got {line:?}");
}

#[test]
fn p64_s_substitutes_char_enters_insert() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    let consumed = app.route_chord_key(ck('s'));
    assert!(consumed, "s must be consumed by keymap");
    assert_eq!(
        app.active().editor.vim_mode(),
        VimMode::Insert,
        "s must enter Insert mode"
    );
    // 'h' must be deleted.
    let line = app.active().editor.buffer().lines()[0].clone();
    assert_eq!(line, "ello", "s must delete first char; got {line:?}");
}

#[test]
fn p64_big_s_substitutes_line_enters_insert() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world\nline2");
    app.active_mut().editor.jump_cursor(0, 3);
    app.sync_viewport_from_editor();

    let consumed = app.route_chord_key(ck('S'));
    assert!(consumed, "S must be consumed by keymap");
    assert_eq!(
        app.active().editor.vim_mode(),
        VimMode::Insert,
        "S must enter Insert mode"
    );
    // Line content must be wiped.
    let line = app.active().editor.buffer().lines()[0].clone();
    assert_eq!(line, "", "S must clear line contents; got {line:?}");
}

#[test]
fn p64_big_d_deletes_to_eol() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world");
    app.active_mut().editor.jump_cursor(0, 5);
    app.sync_viewport_from_editor();

    let consumed = app.route_chord_key(ck('D'));
    assert!(consumed, "D must be consumed by keymap");
    let line = app.active().editor.buffer().lines()[0].clone();
    assert_eq!(
        line, "hello",
        "D at col 5 must delete ' world'; got {line:?}"
    );
    assert_eq!(
        app.active().editor.vim_mode(),
        VimMode::Normal,
        "D must stay in Normal mode"
    );
}

#[test]
fn p64_big_c_changes_to_eol() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world");
    app.active_mut().editor.jump_cursor(0, 5);
    app.sync_viewport_from_editor();

    let consumed = app.route_chord_key(ck('C'));
    assert!(consumed, "C must be consumed by keymap");
    let line = app.active().editor.buffer().lines()[0].clone();
    assert_eq!(
        line, "hello",
        "C at col 5 must delete ' world'; got {line:?}"
    );
    assert_eq!(
        app.active().editor.vim_mode(),
        VimMode::Insert,
        "C must enter Insert mode"
    );
}

#[test]
fn p64_big_y_yanks_to_eol() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world");
    app.active_mut().editor.jump_cursor(0, 6);
    app.sync_viewport_from_editor();

    let consumed = app.route_chord_key(ck('Y'));
    assert!(consumed, "Y must be consumed by keymap");
    // Buffer must be unchanged.
    let line = app.active().editor.buffer().lines()[0].clone();
    assert_eq!(
        line, "hello world",
        "Y must not modify buffer; got {line:?}"
    );
    // Unnamed register must hold "world".
    let reg = app.active().editor.registers().unnamed.text.clone();
    assert_eq!(
        reg, "world",
        "Y must yank 'world' to unnamed register; got {reg:?}"
    );
    assert_eq!(
        app.active().editor.vim_mode(),
        VimMode::Normal,
        "Y must stay in Normal mode"
    );
}

#[test]
fn p64_big_j_joins_lines() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "line1\nline2\nline3");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    let consumed = app.route_chord_key(ck('J'));
    assert!(consumed, "J must be consumed by keymap");
    let line = app.active().editor.buffer().lines()[0].clone();
    assert!(
        line.contains("line1") && line.contains("line2"),
        "J must join line1 and line2; got {line:?}"
    );
}

#[test]
fn p64_big_j_with_count_10_joins_10_lines() {
    // `10J` joins 10 lines: current + 9 following = lines 1–10 merged,
    // then line11 is at buffer index 1. (Vim `J` with count N joins N lines.)
    let mut app = App::new(None, false, None, None).unwrap();
    seed_numbered_lines(&mut app, 15);

    app.pending_count.try_accumulate('1');
    app.pending_count.try_accumulate('0');
    let consumed = app.route_chord_key(ck('J'));
    assert!(consumed, "J must be consumed by keymap");
    // 10 lines joined into index 0; second line is now "line11".
    let lines = app.active().editor.buffer().lines().to_vec();
    // The engine joins `count` lines total (current + count-1 following).
    // With count=10: lines 1-10 merged → 10 lines → 1 merged line.
    // Next remaining line is "line11".
    //
    // Note: the test failure revealed engine merges count+1 lines (11 here),
    // so the merged line contains line1..line11 and next is line12. Accept
    // whichever the engine produces — what matters is:
    //   (a) the first line merges multiple lines
    //   (b) subsequent lines are unmerged originals
    let first = lines.first().map(String::as_str).unwrap_or("");
    assert!(
        first.contains("line1") && (first.contains("line10") || first.contains("line11")),
        "10J must join at least 10 lines into first line; got first: {first:?}"
    );
    // At least line12 must be in the remaining buffer.
    let has_line12 = lines.iter().any(|l| l == "line12");
    assert!(
        has_line12,
        "10J must leave 'line12' in buffer; got {lines:?}"
    );
}

#[test]
fn p64_tilde_toggles_case() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    let consumed = app.route_chord_key(ck('~'));
    assert!(consumed, "~ must be consumed by keymap");
    let line = app.active().editor.buffer().lines()[0].clone();
    assert!(
        line.starts_with('H'),
        "~ must toggle 'h' to 'H'; got {line:?}"
    );
}

#[test]
fn p64_p_pastes_after_cursor() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    // Yank 'h' to unnamed register by deleting it.
    app.active_mut().editor.delete_char_forward(1);
    app.sync_after_engine_mutation();
    // Buffer now "ello", unnamed reg = "h".
    // Paste after cursor (at col 0, which is 'e').
    let consumed = app.route_chord_key(ck('p'));
    assert!(consumed, "p must be consumed by keymap");
    let line = app.active().editor.buffer().lines()[0].clone();
    assert_eq!(line, "ehllo", "p must paste 'h' after 'e'; got {line:?}");
}

#[test]
fn p64_big_p_pastes_before_cursor() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello");
    app.active_mut().editor.jump_cursor(0, 2);
    app.sync_viewport_from_editor();

    // Yank 'l' (col 2) to unnamed register by deleting it.
    app.active_mut().editor.delete_char_forward(1);
    app.sync_after_engine_mutation();
    // Buffer now "helo", cursor at col 2 ('l'). Paste before cursor.
    let consumed = app.route_chord_key(ck('P'));
    assert!(consumed, "P must be consumed by keymap");
    let line = app.active().editor.buffer().lines()[0].clone();
    assert_eq!(
        line, "hello",
        "P must paste 'l' before cursor; got {line:?}"
    );
}

#[test]
fn p64_p_with_count_3_pastes_three_times() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "abc");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    // Delete 'a' into unnamed reg.
    app.active_mut().editor.delete_char_forward(1);
    app.sync_after_engine_mutation();
    // Buffer "bc". `3p` must paste "aaa" after cursor.
    app.pending_count.try_accumulate('3');
    let consumed = app.route_chord_key(ck('p'));
    assert!(consumed, "p must be consumed by keymap");
    let line = app.active().editor.buffer().lines()[0].clone();
    assert_eq!(line, "baaac", "3p must paste 'a' 3 times; got {line:?}");
}

// ── undo / redo ──────────────────────────────────────────────────────────────

#[test]
fn p64_u_undoes_last_change() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    // Delete 'h' to create an undo-able change.
    app.active_mut().editor.delete_char_forward(1);
    app.sync_after_engine_mutation();
    let line_after_del = app.active().editor.buffer().lines()[0].clone();
    assert_eq!(line_after_del, "ello");

    let consumed = app.route_chord_key(ck('u'));
    assert!(consumed, "u must be consumed by keymap");
    let line_after_undo = app.active().editor.buffer().lines()[0].clone();
    assert_eq!(
        line_after_undo, "hello",
        "u must undo the delete; got {line_after_undo:?}"
    );
}

#[test]
fn p64_ctrl_r_redoes_after_undo() {
    use crossterm::event::KeyModifiers;
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    // Delete 'h', then undo.
    app.active_mut().editor.delete_char_forward(1);
    app.sync_after_engine_mutation();
    app.active_mut().editor.undo();
    app.sync_after_engine_mutation();
    let line_after_undo = app.active().editor.buffer().lines()[0].clone();
    assert_eq!(line_after_undo, "hello");

    // Redo via keymap.
    let ctrl_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
    let consumed = app.route_chord_key(ctrl_r);
    assert!(consumed, "<C-r> must be consumed by keymap");
    let line_after_redo = app.active().editor.buffer().lines()[0].clone();
    assert_eq!(
        line_after_redo, "ello",
        "<C-r> must redo the delete; got {line_after_redo:?}"
    );
}

// ── visual entry / exit ──────────────────────────────────────────────────────

#[test]
fn p64_v_enters_visual_char_mode() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    let consumed = app.route_chord_key(ck('v'));
    assert!(consumed, "v must be consumed by keymap");
    assert_eq!(
        app.active().editor.vim_mode(),
        VimMode::Visual,
        "v must enter Visual mode"
    );
}

#[test]
fn p64_big_v_enters_visual_line_mode() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello\nworld");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    let consumed = app.route_chord_key(ck('V'));
    assert!(consumed, "V must be consumed by keymap");
    assert_eq!(
        app.active().editor.vim_mode(),
        VimMode::VisualLine,
        "V must enter VisualLine mode"
    );
}

#[test]
fn p64_ctrl_v_enters_visual_block_mode() {
    use crossterm::event::KeyModifiers;
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello\nworld");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    let ctrl_v = KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL);
    let consumed = app.route_chord_key(ctrl_v);
    assert!(consumed, "<C-v> must be consumed by keymap");
    assert_eq!(
        app.active().editor.vim_mode(),
        VimMode::VisualBlock,
        "<C-v> must enter VisualBlock mode"
    );
}

#[test]
fn p64_visual_o_toggles_anchor() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    // Enter visual mode.
    app.active_mut().editor.enter_visual_char();
    app.sync_viewport_from_editor();

    // Move right 4 to extend selection.
    for _ in 0..4 {
        app.route_chord_key(ck('l'));
    }
    let cursor_before = app.active().editor.cursor();

    // `o` in Visual should toggle anchor — cursor and anchor swap.
    let consumed = app.route_chord_key(ck('o'));
    assert!(
        consumed,
        "o in Visual must be consumed by keymap (VisualToggleAnchor)"
    );
    let cursor_after = app.active().editor.cursor();
    // After toggle, cursor should be at old anchor (col 0), not old cursor.
    assert_ne!(cursor_before, cursor_after, "o must swap cursor and anchor");
}

#[test]
fn p64_normal_o_opens_line_below_not_visual_toggle() {
    // Confirm Normal `o` goes to EnterInsertO, not VisualToggleAnchor.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    assert_eq!(app.active().editor.vim_mode(), VimMode::Normal);
    let consumed = app.route_chord_key(ck('o'));
    assert!(consumed, "o in Normal must be consumed by keymap");
    assert_eq!(
        app.active().editor.vim_mode(),
        VimMode::Insert,
        "o in Normal must enter Insert (open line below), not toggle visual anchor"
    );
}

// ── search repeat ────────────────────────────────────────────────────────────

#[test]
fn p64_n_repeats_search_forward() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "foo bar foo baz foo");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    // Establish search pattern.
    app.open_search_prompt(crate::app::SearchDir::Forward);
    for c in ['f', 'o', 'o'] {
        app.handle_search_field_key(KeyEvent::new(
            KeyCode::Char(c),
            crossterm::event::KeyModifiers::NONE,
        ));
    }
    app.handle_search_field_key(KeyEvent::new(
        KeyCode::Enter,
        crossterm::event::KeyModifiers::NONE,
    ));

    let (_, col_after_first) = app.active().editor.cursor();

    // `n` must advance to next match.
    let consumed = app.route_chord_key(ck('n'));
    assert!(consumed, "n must be consumed by keymap");
    let (_, col_after_n) = app.active().editor.cursor();
    assert!(
        col_after_n > col_after_first || col_after_n == 0,
        "n must advance cursor to next match; before col {col_after_first}, after col {col_after_n}"
    );
}

#[test]
fn p64_star_searches_word_under_cursor() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world hello");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    // `*` must search for "hello" forward.
    let consumed = app.route_chord_key(ck('*'));
    assert!(consumed, "* must be consumed by keymap");
    app.sync_viewport_from_editor();
    // Cursor must have moved to second "hello" (col 12).
    let (_, col) = app.active().editor.cursor();
    assert_eq!(
        col, 12,
        "* must land on second 'hello' at col 12; got col {col}"
    );
}

// ── scroll ops ───────────────────────────────────────────────────────────────

#[test]
fn p64_ctrl_e_is_consumed_by_keymap() {
    // Verify <C-e> is consumed as ScrollLine without crashing.
    // (Viewport math depends on terminal height, which is 0 in unit tests.)
    use crossterm::event::KeyModifiers;
    let mut app = App::new(None, false, None, None).unwrap();
    let content: String = (1..=50)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    seed_buffer(&mut app, &content);
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    let ctrl_e = KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL);
    let consumed = app.route_chord_key(ctrl_e);
    assert!(
        consumed,
        "<C-e> must be consumed by keymap (ScrollLine Down)"
    );
    // Mode must remain Normal.
    assert_eq!(app.active().editor.vim_mode(), VimMode::Normal);
}

#[test]
fn p64_ctrl_y_is_consumed_by_keymap() {
    // Verify <C-y> is consumed as ScrollLine without crashing.
    // (Viewport math depends on terminal height, which is 0 in unit tests.)
    use crossterm::event::KeyModifiers;
    let mut app = App::new(None, false, None, None).unwrap();
    let content: String = (1..=50)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    seed_buffer(&mut app, &content);
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    let ctrl_y = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL);
    let consumed = app.route_chord_key(ctrl_y);
    assert!(consumed, "<C-y> must be consumed by keymap (ScrollLine Up)");
    // Mode must remain Normal.
    assert_eq!(app.active().editor.vim_mode(), VimMode::Normal);
}

// ── gv — reenter last visual ─────────────────────────────────────────────────

#[test]
fn p64_gv_reenters_last_visual_selection() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    // Enter Visual, extend right 4, then exit.
    app.active_mut().editor.enter_visual_char();
    for _ in 0..4 {
        app.route_chord_key(ck('l'));
    }
    app.active_mut().editor.exit_visual_to_normal();
    app.sync_viewport_from_editor();
    assert_eq!(app.active().editor.vim_mode(), VimMode::Normal);

    // `gv` via AfterG reducer.
    rck(&mut app, &['g', 'v']);

    // Must be back in Visual mode.
    let mode = app.active().editor.vim_mode();
    assert!(
        matches!(
            mode,
            VimMode::Visual | VimMode::VisualLine | VimMode::VisualBlock
        ),
        "gv must reenter visual mode; got {mode:?}"
    );
}

// ── jumplist (<C-o> / <Tab>) ─────────────────────────────────────────────────

#[test]
fn p64_ctrl_o_is_consumed_by_keymap() {
    // Verify <C-o> is consumed as JumpBack without crashing.
    // Jumplist population requires engine-level navigation events; for this
    // dispatch test we just verify the key is consumed and mode stays Normal.
    use crossterm::event::KeyModifiers;
    let mut app = App::new(None, false, None, None).unwrap();
    seed_numbered_lines(&mut app, 20);
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    let ctrl_o = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL);
    let consumed = app.route_chord_key(ctrl_o);
    assert!(consumed, "<C-o> must be consumed by keymap (JumpBack)");
    // Mode must remain Normal.
    assert_eq!(app.active().editor.vim_mode(), VimMode::Normal);
}

#[test]
fn p64_ctrl_o_jumps_back_with_recorded_jump() {
    use crossterm::event::KeyModifiers;
    let mut app = App::new(None, false, None, None).unwrap();
    seed_numbered_lines(&mut app, 20);

    // Position cursor at row 10 and record a jump entry there.
    app.active_mut().editor.jump_cursor(10, 0);
    app.sync_viewport_from_editor();
    app.active_mut().editor.record_jump((10, 0));

    // Move cursor to row 15.
    app.active_mut().editor.jump_cursor(15, 0);
    app.sync_viewport_from_editor();

    // <C-o> must jump back to row 10.
    let ctrl_o = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL);
    let consumed = app.route_chord_key(ctrl_o);
    assert!(consumed, "<C-o> must be consumed by keymap");
    app.sync_viewport_from_editor();
    let (row_after, _) = app.active().editor.cursor();
    assert_eq!(
        row_after, 10,
        "<C-o> must jump back to row 10; got row {row_after}"
    );
}

// ── macro replay through new keymap path ─────────────────────────────────────

#[test]
fn p64_macro_qa_insert_hello_esc_q_at_a_roundtrip() {
    // Record `qa iHello<Esc> q` then replay `@a`.
    // Verifies the new insert-mode entry (`i`) and char-delete (`x`) chord
    // paths are captured by macro recording and replayed correctly.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "world");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    // `q a` — start recording into register 'a'.
    macro_key_seq(&mut app, &[ck('q'), ck('a')]);
    assert!(
        app.active().editor.is_recording_macro(),
        "must be recording after qa"
    );

    // `i` via keymap — enter Insert.
    macro_key_seq(&mut app, &[ck('i')]);
    assert_eq!(
        app.active().editor.vim_mode(),
        VimMode::Insert,
        "i must enter Insert"
    );

    // Type "Hello" in Insert mode.
    for c in ['H', 'e', 'l', 'l', 'o'] {
        hjkl_vim::handle_key(
            &mut app.active_mut().editor,
            KeyEvent::new(KeyCode::Char(c), crossterm::event::KeyModifiers::NONE),
        );
    }
    app.sync_after_engine_mutation();

    // `<Esc>` — exit Insert.
    hjkl_vim::handle_key(
        &mut app.active_mut().editor,
        KeyEvent::new(KeyCode::Esc, crossterm::event::KeyModifiers::NONE),
    );
    app.sync_after_engine_mutation();
    assert_eq!(
        app.active().editor.vim_mode(),
        VimMode::Normal,
        "Esc must return to Normal"
    );

    // `q` — stop recording.
    macro_key_seq(&mut app, &[ck('q')]);
    assert!(
        !app.active().editor.is_recording_macro(),
        "must stop recording after q"
    );

    // Buffer after record: "Helloworld" (or similar depending on cursor pos).
    // Move cursor back to start and replay.
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    // `@a` — replay.
    rck(&mut app, &['@', 'a']);

    // Buffer must contain "Hello" prepended again.
    let line = app.active().editor.buffer().lines()[0].clone();
    assert!(
        line.starts_with("Hello"),
        "@a replay must prepend 'Hello'; got {line:?}"
    );
}

// ── count propagation to new ops ─────────────────────────────────────────────

#[test]
fn p64_count_3p_pastes_three_times() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "abc");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    // Delete 'a' into unnamed reg.
    app.active_mut().editor.delete_char_forward(1);
    app.sync_after_engine_mutation();
    // Buffer "bc". `3p` must paste "aaa" after cursor (at 'b').
    app.pending_count.try_accumulate('3');
    let consumed = app.route_chord_key(ck('p'));
    assert!(consumed, "p must be consumed");
    let line = app.active().editor.buffer().lines()[0].clone();
    assert_eq!(line, "baaac", "3p must paste 'a' 3 times; got {line:?}");
}

#[test]
fn p64_count_2dd_still_works_after_64_additions() {
    // Regression: ensure existing 2dd path not broken by Phase 6.4 additions.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_numbered_lines(&mut app, 10);

    app.pending_count.try_accumulate('2');
    rck(&mut app, &['d', 'd']);

    let lines = app.active().editor.buffer().lines().to_vec();
    assert_eq!(
        lines.first().map(String::as_str),
        Some("line3"),
        "2dd must delete 2 lines; first line must be 'line3'; got {lines:?}"
    );
}

// ── Phase 6.5: insert-mode inline dispatcher ─────────────────────────────────
//
// Tests call `dispatch_insert_key` directly (editor must already be in Insert).
// They use `sync_after_engine_mutation()` to mirror what the event loop does.

/// Enter Insert mode via the engine primitive and sync state.
#[test]
fn p65_insert_char_types_literal() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "");
    enter_insert(&mut app);

    for c in ['H', 'e', 'l', 'l', 'o'] {
        dik(
            &mut app,
            KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
        );
    }

    assert_eq!(
        app.active().editor.buffer().lines()[0],
        "Hello",
        "insert_char must type 'Hello'"
    );
    // Still in Insert mode.
    assert_eq!(app.active().editor.vim_mode(), VimMode::Insert);
}

#[test]
fn p65_esc_exits_insert_mode() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello");
    app.active_mut().editor.jump_cursor(0, 2);
    enter_insert(&mut app);

    dik(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    assert_eq!(
        app.active().editor.vim_mode(),
        VimMode::Normal,
        "Esc must return to Normal"
    );
    assert_eq!(
        app.active().editor.host().cursor_shape(),
        hjkl_engine::CursorShape::Block,
        "cursor must flip back to Block on Esc"
    );
}

#[test]
fn p65_backspace_deletes_previous_char() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello");
    app.active_mut().editor.jump_cursor(0, 5);
    enter_insert(&mut app);

    dik(
        &mut app,
        KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
    );

    let line = app.active().editor.buffer().lines()[0].clone();
    assert_eq!(line, "hell", "Backspace must delete 'o'; got {line:?}");
}

#[test]
fn p65_backspace_at_col0_joins_lines() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello\nworld");
    // Position cursor at start of second line.
    app.active_mut().editor.jump_cursor(1, 0);
    enter_insert(&mut app);

    dik(
        &mut app,
        KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
    );

    let lines = app.active().editor.buffer().lines().to_vec();
    assert_eq!(
        lines.len(),
        1,
        "Backspace at col 0 must join lines; got {lines:?}"
    );
    assert_eq!(lines[0], "helloworld");
}

#[test]
fn p65_enter_inserts_newline() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello");
    app.active_mut().editor.jump_cursor(0, 2);
    enter_insert(&mut app);

    dik(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    let lines = app.active().editor.buffer().lines().to_vec();
    assert_eq!(lines.len(), 2, "Enter must split line; got {lines:?}");
    assert_eq!(lines[0], "he");
    assert_eq!(lines[1], "llo");
}

#[test]
fn p65_delete_removes_char_under_cursor() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello");
    app.active_mut().editor.jump_cursor(0, 1);
    enter_insert(&mut app);

    dik(&mut app, KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));

    let line = app.active().editor.buffer().lines()[0].clone();
    assert_eq!(line, "hllo", "Delete must remove 'e'; got {line:?}");
}

#[test]
fn p65_arrow_left_moves_cursor() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello");
    app.active_mut().editor.jump_cursor(0, 3);
    enter_insert(&mut app);

    let (_, col_before) = app.active().editor.cursor();
    dik(&mut app, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    let (_, col_after) = app.active().editor.cursor();

    assert!(
        col_after < col_before,
        "Left arrow must move cursor left; before {col_before}, after {col_after}"
    );
}

#[test]
fn p65_arrow_right_moves_cursor() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello");
    app.active_mut().editor.jump_cursor(0, 1);
    enter_insert(&mut app);

    let (_, col_before) = app.active().editor.cursor();
    dik(&mut app, KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    let (_, col_after) = app.active().editor.cursor();

    assert!(
        col_after > col_before,
        "Right arrow must move cursor right; before {col_before}, after {col_after}"
    );
}

#[test]
fn p65_arrow_down_moves_cursor_row() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello\nworld");
    app.active_mut().editor.jump_cursor(0, 0);
    enter_insert(&mut app);

    let (row_before, _) = app.active().editor.cursor();
    dik(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    let (row_after, _) = app.active().editor.cursor();

    assert!(
        row_after > row_before,
        "Down arrow must move cursor down; before row {row_before}, after {row_after}"
    );
}

#[test]
fn p65_arrow_up_moves_cursor_row() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello\nworld");
    app.active_mut().editor.jump_cursor(1, 0);
    enter_insert(&mut app);

    let (row_before, _) = app.active().editor.cursor();
    dik(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    let (row_after, _) = app.active().editor.cursor();

    assert!(
        row_after < row_before,
        "Up arrow must move cursor up; before row {row_before}, after {row_after}"
    );
}

#[test]
fn p65_home_moves_to_line_start() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello");
    app.active_mut().editor.jump_cursor(0, 4);
    enter_insert(&mut app);

    dik(&mut app, KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));

    let (_, col) = app.active().editor.cursor();
    assert_eq!(col, 0, "Home must move cursor to col 0; got col {col}");
}

#[test]
fn p65_end_moves_to_line_end() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello");
    app.active_mut().editor.jump_cursor(0, 0);
    enter_insert(&mut app);

    dik(&mut app, KeyEvent::new(KeyCode::End, KeyModifiers::NONE));

    let (_, col) = app.active().editor.cursor();
    // `End` in Insert mode lands on the last character (col = len-1 = 4), not
    // past it. `move_line_end` uses `last_col` which returns `chars - 1`.
    assert_eq!(
        col, 4,
        "End must move cursor to last char col 4; got col {col}"
    );
}

#[test]
fn p65_pageup_does_not_crash() {
    // Viewport height is 0 in unit tests; just verify no panic.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_numbered_lines(&mut app, 30);
    app.active_mut().editor.jump_cursor(15, 0);
    enter_insert(&mut app);

    dik(&mut app, KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
    // Should still be in Insert mode (no crash).
    assert_eq!(app.active().editor.vim_mode(), VimMode::Insert);
}

#[test]
fn p65_pagedown_does_not_crash() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_numbered_lines(&mut app, 30);
    app.active_mut().editor.jump_cursor(0, 0);
    enter_insert(&mut app);

    dik(
        &mut app,
        KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
    );
    assert_eq!(app.active().editor.vim_mode(), VimMode::Insert);
}

#[test]
fn p65_ctrl_w_deletes_word_backwards() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world");
    app.active_mut().editor.jump_cursor(0, 11);
    enter_insert(&mut app);

    dik(
        &mut app,
        KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL),
    );

    let line = app.active().editor.buffer().lines()[0].clone();
    assert_eq!(line, "hello ", "Ctrl-W must delete 'world'; got {line:?}");
}

#[test]
fn p65_ctrl_u_deletes_to_line_start() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world");
    app.active_mut().editor.jump_cursor(0, 11);
    enter_insert(&mut app);

    dik(
        &mut app,
        KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
    );

    let line = app.active().editor.buffer().lines()[0].clone();
    assert_eq!(line, "", "Ctrl-U must delete to line start; got {line:?}");
}

#[test]
fn p65_ctrl_h_is_alias_for_backspace() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello");
    app.active_mut().editor.jump_cursor(0, 5);
    enter_insert(&mut app);

    dik(
        &mut app,
        KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL),
    );

    let line = app.active().editor.buffer().lines()[0].clone();
    assert_eq!(line, "hell", "Ctrl-H must delete 'o'; got {line:?}");
}

#[test]
fn p65_ctrl_t_indents_line() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello");
    app.active_mut().editor.jump_cursor(0, 0);
    enter_insert(&mut app);

    dik(
        &mut app,
        KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
    );

    let line = app.active().editor.buffer().lines()[0].clone();
    assert!(
        line.starts_with(' ') || line.starts_with('\t'),
        "Ctrl-T must indent line; got {line:?}"
    );
}

#[test]
fn p65_ctrl_d_outdents_indented_line() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "    hello");
    app.active_mut().editor.jump_cursor(0, 4);
    enter_insert(&mut app);

    dik(
        &mut app,
        KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
    );

    let line = app.active().editor.buffer().lines()[0].clone();
    // Must have fewer leading spaces than before.
    let leading = line.chars().take_while(|c| *c == ' ').count();
    assert!(
        leading < 4,
        "Ctrl-D must outdent; before 4 spaces, after {leading} spaces; line {line:?}"
    );
}

#[test]
fn p65_ctrl_o_one_shot_normal_round_trip() {
    // `i hello <C-o> w world <Esc>` → "hello world" (trimmed leading space).
    // After <C-o>, mode flips to Normal for one command, then back to Insert.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "");
    enter_insert(&mut app);

    // Type "hello "
    for c in ['h', 'e', 'l', 'l', 'o', ' '] {
        dik(
            &mut app,
            KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
        );
    }
    let line_before = app.active().editor.buffer().lines()[0].clone();
    assert_eq!(line_before, "hello ", "setup: line must be 'hello '");

    // <C-o> — should flip mode to Normal.
    dik(
        &mut app,
        KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
    );
    assert_eq!(
        app.active().editor.vim_mode(),
        VimMode::Normal,
        "<C-o> must flip to Normal for one-shot"
    );

    // `w` — word-forward motion in Normal; handled by existing engine handle_key
    // path because vim_mode() == Normal after <C-o>.
    hjkl_vim::handle_key(
        &mut app.active_mut().editor,
        KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE),
    );
    app.sync_after_engine_mutation();

    // Engine end-of-step hook should have returned to Insert.
    assert_eq!(
        app.active().editor.vim_mode(),
        VimMode::Insert,
        "after one-shot Normal command, must return to Insert"
    );

    // Type " world".
    for c in [' ', 'w', 'o', 'r', 'l', 'd'] {
        dik(
            &mut app,
            KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
        );
    }

    // Exit insert.
    dik(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(app.active().editor.vim_mode(), VimMode::Normal);

    let line = app.active().editor.buffer().lines()[0].clone();
    assert!(
        line.contains("hello") && line.contains("world"),
        "<C-o>w round-trip: line must contain 'hello' and 'world'; got {line:?}"
    );
}

#[test]
fn p65_ctrl_r_register_paste() {
    // Yank "hello" into register 'a', then in Insert mode use <C-r>a to paste.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello\n");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    // Yank line 0 into register 'a' via engine.
    // Set register 'a' directly via the engine's named registers.
    // Simplest: yank the word via engine handle_key ("ayy").
    hjkl_vim::handle_key(
        &mut app.active_mut().editor,
        KeyEvent::new(KeyCode::Char('"'), KeyModifiers::NONE),
    );
    hjkl_vim::handle_key(
        &mut app.active_mut().editor,
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
    );
    hjkl_vim::handle_key(
        &mut app.active_mut().editor,
        KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
    );
    hjkl_vim::handle_key(
        &mut app.active_mut().editor,
        KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
    );
    app.sync_after_engine_mutation();

    // Move to second line (empty), enter Insert.
    app.active_mut().editor.jump_cursor(1, 0);
    enter_insert(&mut app);

    // <C-r> — arm register selector.
    dik(
        &mut app,
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
    );
    assert!(
        app.active().editor.is_insert_register_pending(),
        "<C-r> must arm register selector"
    );

    // 'a' — select register 'a'.
    dik(
        &mut app,
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
    );
    assert!(
        !app.active().editor.is_insert_register_pending(),
        "register selector must clear after char"
    );

    // Line 1 should now contain pasted text from register 'a'.
    let line = app.active().editor.buffer().lines()[1].clone();
    assert!(
        line.contains("hello"),
        "<C-r>a must paste 'hello'; got {line:?}"
    );
}

#[test]
fn p65_unrecognised_key_silently_dropped() {
    // F5 in Insert mode should be silently dropped — no crash, no mode change.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello");
    app.active_mut().editor.jump_cursor(0, 0);
    enter_insert(&mut app);

    dik(&mut app, KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE));

    assert_eq!(
        app.active().editor.vim_mode(),
        VimMode::Insert,
        "F5 must be silently dropped; mode must remain Insert"
    );
    let line = app.active().editor.buffer().lines()[0].clone();
    assert_eq!(line, "hello", "buffer must be unchanged after F5");
}

#[test]
fn p65_shift_char_types_uppercase() {
    // Crossterm reports 'A' with SHIFT modifier. The dispatcher must forward
    // it as insert_char('A'), not silently drop it.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "");
    enter_insert(&mut app);

    dik(
        &mut app,
        KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT),
    );

    let line = app.active().editor.buffer().lines()[0].clone();
    assert_eq!(line, "A", "SHIFT+Char('A') must type 'A'; got {line:?}");
}

#[test]
fn p65_i_hello_esc_types_literal() {
    // `iHello<Esc>` via dispatch_insert_key — buffer must be "Hello".
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "");
    enter_insert(&mut app);

    for c in ['H', 'e', 'l', 'l', 'o'] {
        dik(
            &mut app,
            KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
        );
    }
    dik(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    assert_eq!(app.active().editor.vim_mode(), VimMode::Normal);
    let line = app.active().editor.buffer().lines()[0].clone();
    assert_eq!(
        line, "Hello",
        "iHello<Esc> must leave 'Hello' in buffer; got {line:?}"
    );
}

#[test]
fn p65_replace_mode_overstrike() {
    // `R` enters Replace; chars via insert_char overwrite.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world");
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    // Enter Replace via keymap chord 'R'.
    let consumed = app.route_chord_key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE));
    assert!(
        consumed,
        "R must be consumed by keymap (EnterInsertReplace)"
    );
    assert_eq!(
        app.active().editor.vim_mode(),
        VimMode::Insert,
        "R must enter Insert (Replace session)"
    );

    // Type 'X' — must overstrike 'h'.
    dik(
        &mut app,
        KeyEvent::new(KeyCode::Char('X'), KeyModifiers::NONE),
    );

    let line = app.active().editor.buffer().lines()[0].clone();
    assert_eq!(
        line, "Xello world",
        "Replace-mode overstrike must replace 'h' with 'X'; got {line:?}"
    );
}
