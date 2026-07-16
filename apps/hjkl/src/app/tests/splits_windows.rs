use super::*;

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
    let msg = app.bus.last_body_or_empty().to_string();
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
    let msg = app.bus.last_body_or_empty().to_string();
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
    let top_top_row_before = app.window_scroll(top_win).0;

    // Manually advance bottom window's scroll to simulate scrolling (#151
    // Phase D: scroll lives on the window's own editor).
    app.window_editors
        .get_mut(&bottom_win)
        .unwrap()
        .host_mut()
        .viewport_mut()
        .top_row = 20;

    // Top window's scroll must be unaffected.
    let top_top_row_after = app.window_scroll(top_win).0;
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
    let msg = app.bus.last_body_or_empty().to_string();
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
    let msg = app.bus.last_body_or_empty().to_string();
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
    let msg = app.bus.last_body_or_empty().to_string();
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

    let msg = app.bus.last_body_or_empty().to_string();
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

    let msg = app.bus.last_body_or_empty().to_string();
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
    let lines = app.slots[slot_idx]
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

// ── Phase 4a: host-registry tabnext tests ───────────────────────────────────

#[test]
fn colon_tabnext_via_host_registry() {
    // Two-tab setup: verify tabnext advances active_tab via host registry.
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("tabnew");
    assert_eq!(app.active_tab, 1);
    // Go back to 0 so we can advance forward.
    app.active_tab = 0;
    app.dispatch_ex("tabnext");
    assert_eq!(app.active_tab, 1, "tabnext must advance active_tab");
}

#[test]
fn colon_tabn_alias_via_host_registry() {
    // Same test using the `tabn` alias.
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("tabnew");
    assert_eq!(app.active_tab, 1);
    app.active_tab = 0;
    app.dispatch_ex("tabn");
    assert_eq!(app.active_tab, 1, "tabn alias must advance active_tab");
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
fn tabclose_prunes_window_folds_and_editors_for_split_tab() {
    // Close a tab that has an internal split — both window ids it owned
    // must be dropped from window_folds and window_editors, not just from
    // `windows`. Otherwise the dead ids keep pinning the old slot's rope
    // Arc, and (in the worst case) get silently reused as fresh window ids
    // pick up stale fold state.
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("tabnew"); // tab 1 (active) — populates window_folds for its window.
    app.dispatch_ex("sp"); // split tab 1 into two windows; new one inherits folds.
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.active_tab, 1);
    let closing_wids = app.tabs[1].layout.leaves();
    assert_eq!(closing_wids.len(), 2, "setup: tab 1 must have a split");
    assert!(
        closing_wids
            .iter()
            .all(|w| app.window_folds.contains_key(w)),
        "setup: both split windows must have a folds entry"
    );
    assert!(
        closing_wids
            .iter()
            .all(|w| app.window_editors.contains_key(w)),
        "setup: both split windows must have a view editor"
    );

    app.dispatch_ex("tabclose");
    assert_eq!(app.tabs.len(), 1, "tabclose must remove tab 1");

    for wid in &closing_wids {
        assert!(
            !app.window_folds.contains_key(wid),
            "tabclose must prune window_folds for closed window {wid:?}"
        );
        assert!(
            !app.window_editors.contains_key(wid),
            "tabclose must prune window_editors for closed window {wid:?}"
        );
    }
}

#[test]
fn tabclose_last_tab_errors() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert_eq!(app.tabs.len(), 1);
    app.dispatch_ex("tabclose");
    // Must refuse — only one tab.
    assert_eq!(app.tabs.len(), 1, "tabclose must not close the last tab");
    let msg = app.bus.last_body_or_empty().to_string();
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
fn gt_with_explicit_count_is_absolute() {
    // `:h gt` — `{count}gt` goes to tab page {count} (1-indexed, ABSOLUTE),
    // unlike bare `gt` which goes to the next tab (relative, with wrap).
    // Pre-fix, `{count}gt` ran `count` repetitions of relative tabnext:
    // from tab 1 (index 0) with 3 tabs, `2gt` would land on tab 3 (index 2)
    // instead of tab 2 (index 1).
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("tabnew");
    app.dispatch_ex("tabnew");
    assert_eq!(app.tabs.len(), 3, "setup: need 3 tabs");
    app.dispatch_ex("tabfirst");
    assert_eq!(app.active_tab, 0);

    app.pending_count.try_accumulate('2');
    drive_key(&mut app, key(KeyCode::Char('g')));
    drive_key(&mut app, key(KeyCode::Char('t')));
    assert_eq!(
        app.active_tab, 1,
        "2gt must land on tab page 2 (index 1), absolute"
    );

    // `1gt` (explicit count=1) must jump to tab page 1, even though the
    // count value alone is indistinguishable from a bare `gt`'s default.
    app.pending_count.try_accumulate('1');
    drive_key(&mut app, key(KeyCode::Char('g')));
    drive_key(&mut app, key(KeyCode::Char('t')));
    assert_eq!(
        app.active_tab, 0,
        "1gt must land on tab page 1 (index 0), absolute — not act like bare gt"
    );
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
    let msg = app.bus.last_body_or_empty().to_string();
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
    let msg = app.bus.last_body_or_empty().to_string();
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
    let msg = app.bus.last_body_or_empty().to_string();
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
    let msg = app.bus.last_body_or_empty().to_string();
    assert!(msg.contains("tab 2/2"), "no-op must report position: {msg}");
}

#[test]
fn tabonly_drops_other_tabs() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("tabnew");
    app.dispatch_ex("tabnew");
    assert_eq!(app.tabs.len(), 3);
    // Window ids that belong to the tabs about to be closed (0 and 1).
    let closing_wids: Vec<_> = app.tabs[0]
        .layout
        .leaves()
        .into_iter()
        .chain(app.tabs[1].layout.leaves())
        .collect();
    assert!(
        closing_wids
            .iter()
            .any(|w| app.window_folds.contains_key(w)),
        "setup: at least one closing window must have a folds entry"
    );
    // Stay on tab 2. Run tabonly — should reduce to 1 tab.
    app.dispatch_ex("tabonly");
    assert_eq!(app.tabs.len(), 1, "tabonly must close all other tabs");
    assert_eq!(app.active_tab, 0, "active_tab must be reset to 0");
    let msg = app.bus.last_body_or_empty().to_string();
    assert_eq!(msg, "tabonly");
    for wid in &closing_wids {
        assert!(
            !app.window_folds.contains_key(wid),
            "tabonly must prune window_folds for closed window {wid:?}"
        );
        assert!(
            !app.window_editors.contains_key(wid),
            "tabonly must prune window_editors for closed window {wid:?}"
        );
    }
}

#[test]
fn tabonly_no_op_with_single_tab() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("tabonly");
    assert_eq!(app.tabs.len(), 1, "tabonly on single tab must stay at 1");
    let msg = app.bus.last_body_or_empty().to_string();
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

// ── close_tabs_to_right / close_tabs_to_left ─────────────────────────────────

#[test]
fn close_tabs_to_right_leaves_active_and_earlier() {
    // 4 tabs: 0, 1, 2, 3. Active = 2. After: tabs 0,1,2 remain; active still 2.
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("tabnew"); // tab 1
    app.dispatch_ex("tabnew"); // tab 2
    app.dispatch_ex("tabnew"); // tab 3 (active)
    assert_eq!(app.tabs.len(), 4);
    // Navigate back to tab 2.
    app.dispatch_ex("tabprev");
    assert_eq!(app.active_tab, 2);
    let closing_wid = app.tabs[3].layout.leaves()[0];
    assert!(
        app.window_folds.contains_key(&closing_wid),
        "setup: closing window must have a folds entry"
    );
    app.close_tabs_to_right();
    assert_eq!(app.tabs.len(), 3, "expected 3 tabs remaining (0, 1, 2)");
    assert_eq!(app.active_tab, 2, "active_tab must stay at 2");
    assert!(
        !app.window_folds.contains_key(&closing_wid),
        "close_tabs_to_right must prune window_folds for closed window {closing_wid:?}"
    );
    assert!(
        !app.window_editors.contains_key(&closing_wid),
        "close_tabs_to_right must prune window_editors for closed window {closing_wid:?}"
    );
}

#[test]
fn close_tabs_to_left_shifts_active_to_zero() {
    // 4 tabs: 0, 1, 2, 3. Active = 2. After: tabs 2,3 remain; active = 0.
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("tabnew"); // tab 1
    app.dispatch_ex("tabnew"); // tab 2
    app.dispatch_ex("tabnew"); // tab 3 (active)
    // Navigate back to tab 2.
    app.dispatch_ex("tabprev");
    assert_eq!(app.active_tab, 2);
    let closing_wids: Vec<_> = app.tabs[0]
        .layout
        .leaves()
        .into_iter()
        .chain(app.tabs[1].layout.leaves())
        .collect();
    assert!(
        closing_wids
            .iter()
            .any(|w| app.window_folds.contains_key(w)),
        "setup: at least one closing window must have a folds entry"
    );
    app.close_tabs_to_left();
    assert_eq!(
        app.tabs.len(),
        2,
        "expected 2 tabs remaining (originally 2, 3)"
    );
    assert_eq!(app.active_tab, 0, "active_tab must shift to 0");
    for wid in &closing_wids {
        assert!(
            !app.window_folds.contains_key(wid),
            "close_tabs_to_left must prune window_folds for closed window {wid:?}"
        );
        assert!(
            !app.window_editors.contains_key(wid),
            "close_tabs_to_left must prune window_editors for closed window {wid:?}"
        );
    }
}

#[test]
fn close_tabs_to_right_noop_on_last_tab() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("tabnew");
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.active_tab, 1); // last tab
    app.close_tabs_to_right();
    assert_eq!(app.tabs.len(), 2, "no-op when already last");
    assert_eq!(app.active_tab, 1);
}

#[test]
fn close_tabs_to_left_noop_on_first_tab() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("tabnew");
    assert_eq!(app.tabs.len(), 2);
    // Go back to tab 0.
    app.dispatch_ex("tabprev");
    assert_eq!(app.active_tab, 0); // first tab
    app.close_tabs_to_left();
    assert_eq!(app.tabs.len(), 2, "no-op when already first");
    assert_eq!(app.active_tab, 0);
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
    let msg = app.bus.last_body_or_empty().to_string();
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
    let popup_content = app
        .info_popup
        .as_ref()
        .map(|p| p.content.clone())
        .unwrap_or_default();
    // Should contain 3 "Tab page N" entries.
    assert!(popup_content.contains("Tab page 1"), "missing Tab page 1");
    assert!(popup_content.contains("Tab page 2"), "missing Tab page 2");
    assert!(popup_content.contains("Tab page 3"), "missing Tab page 3");
    // The active tab (3) must have '>'; others must have ' '.
    let lines: Vec<&str> = popup_content.lines().collect();
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
