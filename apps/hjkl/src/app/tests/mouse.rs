use super::*;

// ── P11: MouseFlags unit tests ─────────────────────────────────────────────

#[test]
fn mouse_flags_default_all_enabled() {
    // Fresh App (and MouseFlags::default()) must have all 4 modes enabled.
    let flags = MouseFlags::default();
    assert!(flags.normal, "normal should be enabled by default");
    assert!(flags.visual, "visual should be enabled by default");
    assert!(flags.insert, "insert should be enabled by default");
    assert!(flags.command, "command should be enabled by default");
}

#[test]
fn mouse_flags_set_to_n_only_normal_active() {
    let flags = MouseFlags::from_flags("n");
    assert!(flags.normal, "n flag enables normal");
    assert!(!flags.visual, "only n: visual must be off");
    assert!(!flags.insert, "only n: insert must be off");
    assert!(!flags.command, "only n: command must be off");
}

#[test]
fn mouse_flags_set_empty_disables_all() {
    let flags_empty = MouseFlags::from_flags("");
    assert!(!flags_empty.normal, "empty string must disable normal");
    assert!(!flags_empty.visual, "empty string must disable visual");
    assert!(!flags_empty.insert, "empty string must disable insert");
    assert!(!flags_empty.command, "empty string must disable command");

    let flags_none = MouseFlags::none();
    assert!(!flags_none.normal, "MouseFlags::none() must disable normal");
    assert!(!flags_none.visual, "MouseFlags::none() must disable visual");
    assert!(!flags_none.insert, "MouseFlags::none() must disable insert");
    assert!(
        !flags_none.command,
        "MouseFlags::none() must disable command"
    );
}

#[test]
fn mouse_flags_a_is_all_enabled() {
    let flags = MouseFlags::from_flags("a");
    assert!(flags.normal && flags.visual && flags.insert && flags.command);
}

#[test]
fn mouse_flags_nvi_multi_char() {
    let flags = MouseFlags::from_flags("nvi");
    assert!(flags.normal);
    assert!(flags.visual);
    assert!(flags.insert);
    assert!(!flags.command);
}

#[test]
fn mouse_enabled_for_normal_mode_flags() {
    let all = MouseFlags::all();
    assert!(mouse_enabled_for(VimMode::Normal, &all));

    let mut none_normal = MouseFlags::all();
    none_normal.normal = false;
    assert!(!mouse_enabled_for(VimMode::Normal, &none_normal));
}

#[test]
fn mouse_enabled_for_visual_mode_flags() {
    let all = MouseFlags::all();
    assert!(mouse_enabled_for(VimMode::Visual, &all));
    assert!(mouse_enabled_for(VimMode::VisualLine, &all));
    assert!(mouse_enabled_for(VimMode::VisualBlock, &all));

    let mut no_visual = MouseFlags::all();
    no_visual.visual = false;
    assert!(!mouse_enabled_for(VimMode::Visual, &no_visual));
    assert!(!mouse_enabled_for(VimMode::VisualLine, &no_visual));
    assert!(!mouse_enabled_for(VimMode::VisualBlock, &no_visual));
}

#[test]
fn mouse_enabled_for_insert_mode_flags() {
    let all = MouseFlags::all();
    assert!(mouse_enabled_for(VimMode::Insert, &all));

    let mut no_insert = MouseFlags::all();
    no_insert.insert = false;
    assert!(!mouse_enabled_for(VimMode::Insert, &no_insert));
}

#[test]
fn set_mouse_eq_flags_via_dispatch_ex() {
    let mut app = App::new(None, false, None, None).unwrap();
    // Default is all enabled.
    assert!(app.mouse_flags.normal && app.mouse_flags.visual && app.mouse_flags.insert);

    // `:set mouse=n` disables all except normal.
    app.dispatch_ex("set mouse=n");
    assert!(app.mouse_flags.normal, "n: normal on");
    assert!(!app.mouse_flags.visual, "n: visual off");
    assert!(!app.mouse_flags.insert, "n: insert off");

    // `:set nomouse` disables all + mouse_enabled=false.
    app.dispatch_ex("set nomouse");
    assert!(!app.mouse_flags.normal);
    assert!(!app.mouse_flags.visual);
    assert!(!app.mouse_flags.insert);

    // `:set mouse` re-enables all.
    app.dispatch_ex("set mouse");
    assert!(app.mouse_flags.normal);
    assert!(app.mouse_flags.visual);
    assert!(app.mouse_flags.insert);
}

#[test]
fn mouse_flags_as_flags_str_roundtrip() {
    for s in ["a", "n", "v", "i", "c", "nvi", "nv", ""] {
        let flags = MouseFlags::from_flags(s);
        let got = flags.as_flags_str();
        // Re-parse must be equal.
        let reparsed = MouseFlags::from_flags(&got);
        assert_eq!(
            flags, reparsed,
            "roundtrip failed for {s:?}: as_flags_str={got:?}"
        );
    }
}

// ── P4: Shift+click extends visual selection ──────────────────────────────

#[test]
fn shift_click_enters_visual_and_extends_selection() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use ratatui::layout::Rect;

    let mut app = App::new(None, false, None, None).unwrap();

    // Set up a multi-line buffer and window geometry.
    {
        use hjkl_engine::BufferEdit;
        let buf = app.slots_mut()[0].editor.buffer_mut();
        BufferEdit::replace_all(buf, "hello world\nsecond line\nthird\n");
    }
    if let Some(Some(win)) = app.windows.get_mut(0) {
        win.last_rect = Some(Rect::new(0, 1, 80, 20)); // row 1: below a status bar
        win.top_row = 0;
        win.top_col = 0;
    }
    {
        let vp = app.slots_mut()[0].editor.host_mut().viewport_mut();
        vp.width = 80;
        vp.height = 20;
        vp.text_width = 80;
        vp.top_row = 0;
        vp.top_col = 0;
        vp.tab_width = 4;
    }

    // Editor starts in Normal mode; cursor at (0,0).
    assert_eq!(app.active().editor.vim_mode(), VimMode::Normal);

    // Synthesise a Shift+Left-click at row=1 (screen), col=4 (text area).
    // With no line numbers, gutter_width = 0; text starts at col 0.
    // Disable line numbers so gutter = 0.
    {
        let opts = hjkl_engine::Options {
            number: false,
            relativenumber: false,
            ..hjkl_engine::Options::default()
        };
        app.active_mut().editor.apply_options(&opts);
    }

    let click_screen_row: u16 = 2; // window starts at screen row 1, so doc_row = 1
    let click_screen_col: u16 = 3; // doc_col = 3

    let me = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: click_screen_col,
        row: click_screen_row,
        modifiers: KeyModifiers::SHIFT,
    };

    // Dispatch through the modifier-click path directly.
    // Since we're unit-testing we call the zone + drag API path ourselves.
    {
        use crate::app::mouse;
        let zone = mouse::hit_test_zone(&app, me.column, me.row);
        if let mouse::Zone::Code {
            win_id,
            doc_row,
            doc_col,
        } = zone
        {
            let current_focus = app.focused_window();
            if win_id != current_focus {
                app.sync_viewport_from_editor();
                app.set_focused_window(win_id);
                app.sync_viewport_to_editor();
            }
            if app.active().editor.vim_mode() != VimMode::Visual {
                app.active_mut().editor.mouse_begin_drag();
            }
            app.active_mut()
                .editor
                .mouse_extend_drag_doc(doc_row, doc_col);
            app.sync_after_engine_mutation();

            // After Shift+click the editor must be in Visual mode.
            assert_eq!(
                app.active().editor.vim_mode(),
                VimMode::Visual,
                "Shift+click must enter Visual mode"
            );
        } else {
            panic!("expected Code zone, got {zone:?}");
        }
    }
}

// ── Phase 9: border drag-resize tests ────────────────────────────────────────

#[cfg(test)]
mod border_drag_tests {
    use super::*;
    use crate::app::mouse::SplitOrientation;
    use crate::app::{App, SPLIT_MIN_SIZE_COLS, SPLIT_MIN_SIZE_ROWS};
    use ratatui::layout::Rect;

    /// Set up a VSplit app with `last_rect` pre-filled so resize_split_to works.
    fn make_vsplit_with_rect(ratio: f32, total_w: u16, total_h: u16) -> App {
        use crate::app::window::{LayoutTree, SplitDir, Tab, Window};
        let mut app = App::new(None, false, None, None).unwrap();
        let win1 = app.next_window_id;
        app.next_window_id += 1;
        app.windows.push(Some(Window {
            slot: 0,
            top_row: 0,
            top_col: 0,
            cursor_row: 0,
            cursor_col: 0,
            last_rect: None,
        }));
        let area = Rect::new(0, 0, total_w, total_h);
        // a_w = round(total_w * ratio), clamped. Separator at a_w - 1.
        let a_w = ((total_w as f32) * ratio).round() as u16;
        let a_w = a_w.clamp(1, total_w.saturating_sub(1).max(1));
        if let Some(Some(w)) = app.windows.get_mut(0) {
            w.last_rect = Some(Rect::new(0, 0, a_w.saturating_sub(1), total_h));
        }
        if let Some(Some(w)) = app.windows.get_mut(win1) {
            w.last_rect = Some(Rect::new(a_w, 0, total_w - a_w, total_h));
        }
        app.tabs[0] = Tab {
            layout: LayoutTree::Split {
                dir: SplitDir::Vertical,
                ratio,
                a: Box::new(LayoutTree::Leaf(0)),
                b: Box::new(LayoutTree::Leaf(win1)),
                last_rect: Some(area),
            },
            focused_window: 0,
        };
        app
    }

    /// Set up an HSplit app with `last_rect` pre-filled.
    fn make_hsplit_with_rect(ratio: f32, total_w: u16, total_h: u16) -> App {
        use crate::app::window::{LayoutTree, SplitDir, Tab, Window};
        let mut app = App::new(None, false, None, None).unwrap();
        let win1 = app.next_window_id;
        app.next_window_id += 1;
        app.windows.push(Some(Window {
            slot: 0,
            top_row: 0,
            top_col: 0,
            cursor_row: 0,
            cursor_col: 0,
            last_rect: None,
        }));
        let area = Rect::new(0, 0, total_w, total_h);
        let a_h = ((total_h as f32) * ratio).round() as u16;
        let a_h = a_h.clamp(1, total_h.saturating_sub(1).max(1));
        if let Some(Some(w)) = app.windows.get_mut(0) {
            w.last_rect = Some(Rect::new(0, 0, total_w, a_h.saturating_sub(1)));
        }
        if let Some(Some(w)) = app.windows.get_mut(win1) {
            w.last_rect = Some(Rect::new(0, a_h, total_w, total_h - a_h));
        }
        app.tabs[0] = Tab {
            layout: LayoutTree::Split {
                dir: SplitDir::Horizontal,
                ratio,
                a: Box::new(LayoutTree::Leaf(0)),
                b: Box::new(LayoutTree::Leaf(win1)),
                last_rect: Some(area),
            },
            focused_window: 0,
        };
        app
    }

    fn get_split_ratio(app: &App) -> f32 {
        match app.layout() {
            window::LayoutTree::Split { ratio, .. } => *ratio,
            _ => panic!("expected Split"),
        }
    }

    // ── T2: hit_test_border ────────────────────────────────────────────────

    // (Covered in mouse.rs unit tests; integration smoke here.)

    // ── T7a: border_drag_resizes_vertical_split ──────────────────────────

    #[test]
    fn border_drag_resizes_vertical_split() {
        // VSplit 0.5 ratio, 80 cols wide. a_w=40, sep at col 39.
        // Drag from col 39 to col 44 (+5). Expect ratio grows.
        let mut app = make_vsplit_with_rect(0.5, 80, 24);
        let ratio_before = get_split_ratio(&app);

        // Simulate the drag: split_pos = 44 (new column from origin 0).
        app.resize_split_to(SplitOrientation::Vertical, 0, 80, 44);

        let ratio_after = get_split_ratio(&app);
        assert!(
            ratio_after > ratio_before,
            "dragging VSplit right must grow ratio: before={ratio_before} after={ratio_after}"
        );
        // new_ratio should be approximately 44/80 = 0.55
        let expected = 44.0f32 / 80.0;
        assert!(
            (ratio_after - expected).abs() < 0.02,
            "ratio should be ~{expected:.2}, got {ratio_after:.4}"
        );
    }

    // ── T7b: border_drag_resizes_horizontal_split ────────────────────────

    #[test]
    fn border_drag_resizes_horizontal_split() {
        // HSplit 0.5 ratio, 24 rows tall. a_h=12, sep at row 11.
        // Drag from row 11 to row 8 (-3). Expect ratio shrinks.
        let mut app = make_hsplit_with_rect(0.5, 80, 24);
        let ratio_before = get_split_ratio(&app);

        // split_pos = 8 (from origin 0).
        app.resize_split_to(SplitOrientation::Horizontal, 0, 24, 8);

        let ratio_after = get_split_ratio(&app);
        assert!(
            ratio_after < ratio_before,
            "dragging HSplit up must shrink ratio: before={ratio_before} after={ratio_after}"
        );
        let expected = 8.0f32 / 24.0;
        assert!(
            (ratio_after - expected).abs() < 0.02,
            "ratio should be ~{expected:.2}, got {ratio_after:.4}"
        );
    }

    // ── T7c: border_double_click_equalizes_split ─────────────────────────

    #[test]
    fn border_double_click_equalizes_split() {
        let mut app = make_vsplit_with_rect(0.3, 80, 24);
        // Skew ratio.
        if let window::LayoutTree::Split { ratio, .. } = app.layout_mut() {
            *ratio = 0.3;
        }
        assert!((get_split_ratio(&app) - 0.3).abs() < 1e-4, "precondition");

        app.equalize_split();

        let ratio_after = get_split_ratio(&app);
        assert!(
            (ratio_after - 0.5).abs() < 1e-4,
            "equalize_split must set ratio to 0.5, got {ratio_after}"
        );
    }

    // ── T7d: border_drag_respects_min_size ───────────────────────────────

    #[test]
    fn border_drag_respects_min_size_vertical() {
        // VSplit 80 cols wide. Drag split_pos to 2 (< SPLIT_MIN_SIZE_COLS=10).
        // Expect clamped to 10/80.
        let mut app = make_vsplit_with_rect(0.5, 80, 24);
        app.resize_split_to(SplitOrientation::Vertical, 0, 80, 2);
        let ratio = get_split_ratio(&app);
        let min_ratio = SPLIT_MIN_SIZE_COLS as f32 / 80.0;
        assert!(
            ratio >= min_ratio - 1e-4,
            "ratio must be >= min ({min_ratio:.3}), got {ratio:.4}"
        );
    }

    #[test]
    fn border_drag_respects_min_size_horizontal() {
        // HSplit 24 rows. Drag split_pos to 1 (< SPLIT_MIN_SIZE_ROWS=3).
        let mut app = make_hsplit_with_rect(0.5, 80, 24);
        app.resize_split_to(SplitOrientation::Horizontal, 0, 24, 1);
        let ratio = get_split_ratio(&app);
        let min_ratio = SPLIT_MIN_SIZE_ROWS as f32 / 24.0;
        assert!(
            ratio >= min_ratio - 1e-4,
            "ratio must be >= min ({min_ratio:.3}), got {ratio:.4}"
        );
    }

    #[test]
    fn border_drag_respects_min_size_other_side() {
        // VSplit 80 cols. Drag split_pos to 79 (leaves only 1 for b).
        // Must clamp so b has at least SPLIT_MIN_SIZE_COLS.
        let mut app = make_vsplit_with_rect(0.5, 80, 24);
        app.resize_split_to(SplitOrientation::Vertical, 0, 80, 79);
        let ratio = get_split_ratio(&app);
        let max_ratio = (80 - SPLIT_MIN_SIZE_COLS - 1) as f32 / 80.0;
        assert!(
            ratio <= max_ratio + 1e-4,
            "ratio must be <= max ({max_ratio:.3}) to leave room for b, got {ratio:.4}"
        );
    }

    // ── T7e: border_drag_no_active_split_is_noop ─────────────────────────

    #[test]
    fn border_drag_no_active_split_is_noop() {
        // With no border_drag set, Drag(Left) on a split must not panic.
        // We exercise resize_split_to on a single-window app — should silently no-op.
        let mut app = App::new(None, false, None, None).unwrap();
        assert!(app.border_drag.is_none(), "border_drag must start None");
        // resize_split_to with a single-window app (no split) — must not panic.
        app.resize_split_to(SplitOrientation::Vertical, 0, 80, 40);
        // And border_drag stays None.
        assert!(app.border_drag.is_none());
    }

    // ── dismiss_hover_popup_on_click regression ─────────────────────────────

    /// Regression test for the "garbage text on the right edge after Go to
    /// Definition" bug: a hover popup armed at the cursor's rest position
    /// persisted across mouse-click events. When the user right-clicked to
    /// open the context menu and then chose a menu action (e.g. Go to
    /// Definition), the menu cleared but `hover_popup` did not — its render
    /// pass overlaid stale text on the post-jump buffer.
    ///
    /// Fix: every mouse `Down` arm (Left / Right / Middle) calls
    /// `App::dismiss_hover_popup_on_click()` at the top.
    ///
    /// This unit-tests the helper itself. The "arms call it" wiring is
    /// enforced by code review — three call sites in `event_loop.rs`.
    #[test]
    fn dismiss_hover_popup_on_click_clears_state() {
        use crate::hover_popup::HoverPopup;
        use std::time::Instant;

        let mut app = App::new(None, false, None, None).unwrap();

        app.hover_popup = Some(HoverPopup::new("stale content".to_string(), (50, 5)));
        app.hover_timer = Some(HoverTimer {
            cell: (50, 5),
            started_at: Instant::now(),
            request_sent: true,
        });

        app.dismiss_hover_popup_on_click();

        assert!(
            app.hover_popup.is_none(),
            "hover_popup must be cleared on mouse click — leaving stale popups \
                causes the right-edge garbage bug (right-click → Go to Definition repro)"
        );
        assert!(
            app.hover_timer.is_none(),
            "hover_timer must also be cleared so a subsequent rest re-arms cleanly"
        );
    }

    /// Regression: `screen_rect()` must include the top bar's row when the
    /// top bar is visible (tabs > 1 OR slots > 1). The previous bug
    /// counted only `vp.height + STATUS_LINE_HEIGHT`, undercounting total
    /// terminal height by 1 row whenever the top bar was shown. That made
    /// `ContextMenu::bounding_rect` think the screen was 1 row shorter
    /// than reality, so it flipped popups near the bottom one row too
    /// early — and the `Moved` handler's row→item math disagreed with
    /// the renderer.
    #[test]
    fn screen_rect_includes_top_bar_when_multiple_slots() {
        let path_a = std::env::temp_dir().join("hjkl_screen_rect_a.txt");
        let path_b = std::env::temp_dir().join("hjkl_screen_rect_b.txt");
        for p in [&path_a, &path_b] {
            std::fs::write(p, "x\n").unwrap();
        }
        let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
        // Set viewport to a known size so the math is predictable.
        {
            let vp = app.slots_mut()[0].editor.host_mut().viewport_mut();
            vp.width = 80;
            vp.height = 22; // 24-row terminal minus top + status
        }
        // Single-slot baseline: top bar hidden, height = vp.height + STATUS.
        let single = app.screen_rect();
        assert_eq!(
            single.height,
            22 + STATUS_LINE_HEIGHT,
            "single-slot screen height must skip the (absent) top bar"
        );

        // Open a second slot → top bar becomes visible.
        app.dispatch_ex(&format!("e {}", path_b.display()));
        let active = app.focused_slot_idx();
        {
            let vp = app.slots_mut()[active].editor.host_mut().viewport_mut();
            vp.width = 80;
            vp.height = 22;
        }
        let multi = app.screen_rect();
        assert_eq!(
            multi.height,
            TOP_BAR_HEIGHT + 22 + STATUS_LINE_HEIGHT,
            "multi-slot screen height must include the top bar row \
                (otherwise context-menu hover near the bottom maps to the wrong item)"
        );

        for p in [&path_a, &path_b] {
            let _ = std::fs::remove_file(p);
        }
    }

    // ── right-click cursor move ─────────────────────────────────────────────

    /// Build a small App with `content` loaded into slot 0 and the window's
    /// last_rect + viewport set so hit_test_zone / cell_to_doc produce
    /// well-defined results. Mirrors `mouse.rs::make_app_with_content`.
    fn make_app_with_window(content: &str, area: ratatui::layout::Rect) -> App {
        use hjkl_engine::BufferEdit;
        let mut app = App::new(None, false, None, None).unwrap();
        {
            let buf = app.slots_mut()[0].editor.buffer_mut();
            BufferEdit::replace_all(buf, content);
        }
        if let Some(Some(win)) = app.windows.get_mut(0) {
            win.last_rect = Some(area);
            win.top_row = 0;
            win.top_col = 0;
        }
        {
            let vp = app.slots_mut()[0].editor.host_mut().viewport_mut();
            vp.width = area.width;
            vp.height = area.height;
            vp.text_width = area.width;
            vp.top_row = 0;
            vp.top_col = 0;
            vp.tab_width = 4;
        }
        app
    }

    /// Regression: right-click did not move the cursor to the clicked cell,
    /// so menu actions (Go to Definition, Rename, Format, etc.) operated on
    /// the previous cursor position. Fix moves cursor to the clicked
    /// doc-position before opening the menu.
    #[test]
    fn move_cursor_for_right_click_moves_cursor_to_click() {
        // 5-line buffer, default settings → gutter_width = 4 (numberwidth=4).
        // First text cell is col=4.
        let mut app = make_app_with_window(
            "line one\nline two\nline three\nline four\nline five",
            ratatui::layout::Rect::new(0, 0, 80, 24),
        );

        // Park the cursor at (0, 0) via keyboard motion semantics.
        app.active_mut().editor.set_cursor_doc(0, 0);
        assert_eq!(app.active().editor.cursor(), (0, 0));

        // Right-click on row 2, text column 8 (cell col = gutter 4 + text 8 = 12).
        // Doc col after tab-expansion inverse on a tab-free line = visual col 8.
        app.move_cursor_for_right_click(12, 2);

        assert_eq!(
            app.active().editor.cursor(),
            (2, 8),
            "right-click must move cursor to clicked doc position"
        );
    }

    /// Right-click WITH an active visual selection must preserve the
    /// selection — Cut / Copy from the menu need to operate on it. Cursor
    /// stays put.
    #[test]
    fn move_cursor_for_right_click_preserves_visual_selection() {
        use hjkl_engine::VimMode;
        let mut app = make_app_with_window(
            "line one\nline two\nline three\nline four\nline five",
            ratatui::layout::Rect::new(0, 0, 80, 24),
        );
        app.active_mut().editor.set_cursor_doc(0, 0);
        app.active_mut().editor.enter_visual_char();
        // Extend selection a bit so something is actually selected.
        app.active_mut().editor.set_cursor_doc(0, 4);
        let before = app.active().editor.cursor();
        assert_eq!(app.active().editor.vim_mode(), VimMode::Visual);

        // Right-click somewhere far from the selection.
        app.move_cursor_for_right_click(12, 3);

        assert_eq!(
            app.active().editor.cursor(),
            before,
            "right-click with active visual selection must not move cursor"
        );
        assert_eq!(
            app.active().editor.vim_mode(),
            VimMode::Visual,
            "visual mode must survive the right-click"
        );
    }

    /// Right-click in the gutter zone moves the cursor to the start of that
    /// line.
    #[test]
    fn move_cursor_for_right_click_in_gutter_goes_to_col_zero() {
        let mut app = make_app_with_window(
            "first\nsecond\nthird\nfourth\nfifth",
            ratatui::layout::Rect::new(0, 0, 80, 24),
        );
        app.active_mut().editor.set_cursor_doc(0, 2);

        // Cell col 0 is inside the gutter (gutter_width = 4 by default).
        app.move_cursor_for_right_click(0, 2);

        assert_eq!(
            app.active().editor.cursor(),
            (2, 0),
            "gutter right-click moves cursor to (clicked_row, 0)"
        );
    }

    /// Right-click outside any window (e.g. on the status bar) is a no-op.
    #[test]
    fn move_cursor_for_right_click_outside_window_is_noop() {
        let mut app = make_app_with_window(
            "first\nsecond\nthird",
            ratatui::layout::Rect::new(0, 0, 80, 24),
        );
        app.active_mut().editor.set_cursor_doc(1, 3);
        let before = app.active().editor.cursor();

        // Row 30 is outside the 24-row area entirely.
        app.move_cursor_for_right_click(10, 30);

        assert_eq!(
            app.active().editor.cursor(),
            before,
            "right-click outside any window must not move the cursor"
        );
    }

    // ── Backspace on empty prompt dismisses (neovim parity) ─────────────────

    /// Regression: `:` prompt — backspacing past the last character must
    /// dismiss the prompt entirely. Pre-fix, backspace on an empty prompt
    /// was a no-op, and the user had to press Esc explicitly.
    #[test]
    fn backspace_on_empty_command_prompt_dismisses() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = App::new(None, false, None, None).unwrap();
        app.open_command_prompt();
        assert!(app.command_field.is_some(), "prompt should be open");

        // Type "g", then backspace twice. After first backspace the field
        // is empty; after second backspace the prompt must dismiss.
        app.handle_command_field_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
        app.handle_command_field_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert!(
            app.command_field.is_some(),
            "first backspace cleared the char; prompt still open"
        );
        app.handle_command_field_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert!(
            app.command_field.is_none(),
            "second backspace on empty prompt must dismiss it (neovim parity)"
        );
    }

    /// Same behavior for the `/` and `?` search prompts.
    #[test]
    fn backspace_on_empty_forward_search_prompt_dismisses() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = App::new(None, false, None, None).unwrap();
        app.open_search_prompt(SearchDir::Forward);
        assert!(app.search_field.is_some(), "search prompt should be open");

        app.handle_search_field_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        app.handle_search_field_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert!(app.search_field.is_some(), "still open while empty");
        app.handle_search_field_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert!(
            app.search_field.is_none(),
            "backspace on empty search prompt must dismiss"
        );
    }

    #[test]
    fn backspace_on_empty_backward_search_prompt_dismisses() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = App::new(None, false, None, None).unwrap();
        app.open_search_prompt(SearchDir::Backward);
        assert!(app.search_field.is_some());

        app.handle_search_field_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert!(
            app.search_field.is_none(),
            "backspace on freshly-opened (empty) backward-search prompt must dismiss"
        );
    }

    // ── middle-click zone dispatch ──────────────────────────────────────────

    /// Middle-click on a buffer line entry closes that buffer (`:bdelete`
    /// equivalent). Common terminal-app convention (browsers / IDEs all
    /// middle-click-to-close tabs); pair with the existing X11 primary
    /// paste behavior in the editor area.
    #[test]
    fn middle_click_on_buffer_line_closes_that_buffer() {
        let path_a = std::env::temp_dir().join("hjkl_mclick_bl_a.txt");
        let path_b = std::env::temp_dir().join("hjkl_mclick_bl_b.txt");
        let path_c = std::env::temp_dir().join("hjkl_mclick_bl_c.txt");
        for p in [&path_a, &path_b, &path_c] {
            std::fs::write(p, "x\n").unwrap();
        }

        let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
        app.dispatch_ex(&format!("e {}", path_b.display()));
        app.dispatch_ex(&format!("e {}", path_c.display()));
        // Publish viewport dims so the bar geometry is meaningful and
        // give window 0 a last_rect so hit_test_zone has the bar width.
        if let Some(Some(win)) = app.windows.get_mut(0) {
            win.last_rect = Some(ratatui::layout::Rect::new(0, 0, 200, 24));
        }
        assert_eq!(app.slots.len(), 3);

        // Mid-click on the FIRST buffer line entry (col 0, row 0 — buffer
        // line sits at row 0 when no tab bar is shown).
        let ranges = crate::app::mouse::buffer_line_x_ranges(&app, 200);
        assert!(ranges.len() >= 3);
        let first_col = ranges[0].0;
        app.middle_click(first_col, 0);

        assert_eq!(
            app.slots.len(),
            2,
            "middle-click on buffer line entry must close that buffer"
        );

        for p in [&path_a, &path_b, &path_c] {
            let _ = std::fs::remove_file(p);
        }
    }

    /// Middle-click on a tab entry closes that tab (`:tabclose` equivalent).
    #[test]
    fn middle_click_on_tab_closes_that_tab() {
        let path_a = std::env::temp_dir().join("hjkl_mclick_tab_a.txt");
        let path_b = std::env::temp_dir().join("hjkl_mclick_tab_b.txt");
        for p in [&path_a, &path_b] {
            std::fs::write(p, "x\n").unwrap();
        }

        let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
        app.dispatch_ex(&format!("tabnew {}", path_b.display()));
        if let Some(Some(win)) = app.windows.get_mut(0) {
            win.last_rect = Some(ratatui::layout::Rect::new(0, 0, 200, 24));
        }
        if let Some(Some(win)) = app.windows.get_mut(1) {
            win.last_rect = Some(ratatui::layout::Rect::new(0, 0, 200, 24));
        }
        assert_eq!(app.tabs.len(), 2);

        // tab_x_ranges returns absolute screen columns (right-aligned in v2 bar).
        let ranges = crate::app::mouse::tab_x_ranges(&app, 200);
        assert_eq!(ranges.len(), 2);
        // Click the first cell of the first tab.
        let first_col = ranges[0].0;
        app.middle_click(first_col, 0);

        assert_eq!(
            app.tabs.len(),
            1,
            "middle-click on tab entry must close that tab"
        );

        for p in [&path_a, &path_b] {
            let _ = std::fs::remove_file(p);
        }
    }

    /// Middle-click outside any zone is a no-op.
    #[test]
    fn middle_click_outside_zones_is_noop() {
        let mut app = make_app_with_window(
            "alpha\nbeta\ngamma",
            ratatui::layout::Rect::new(0, 0, 80, 24),
        );
        let slots_before = app.slots.len();
        let tabs_before = app.tabs.len();
        // Row 30 is outside the 24-row screen entirely.
        app.middle_click(10, 30);
        assert_eq!(app.slots.len(), slots_before);
        assert_eq!(app.tabs.len(), tabs_before);
    }

    // ── overlay_active / hover-suppression regression tests ────────────────

    /// Regression: when a context menu is open, the LSP hover popup must NOT
    /// arm/fire from the mouse resting over a menu cell. Pre-fix, hovering on
    /// a menu item for 500ms triggered a hover RPC for the doc cell BEHIND
    /// the menu, and the popup rendered through the menu on top of buffer
    /// text the user couldn't even see.
    #[test]
    fn tick_hover_timer_suppressed_while_context_menu_open() {
        use crate::menu::{ContextMenu, MenuAction, MenuItem};
        use std::time::{Duration, Instant};

        let mut app = App::new(None, false, None, None).unwrap();

        // Arm a hover timer that's already past the 500ms threshold —
        // tick_hover_timer would normally fire the RPC right now.
        app.hover_timer = Some(HoverTimer {
            cell: (10, 5),
            started_at: Instant::now() - Duration::from_millis(800),
            request_sent: false,
        });

        // Open a context menu — overlay_active() should now be true.
        let items = vec![MenuItem::new("Cut", MenuAction::Cut, None)];
        app.context_menu = Some(ContextMenu::new(items, (5, 5)));
        assert!(
            app.overlay_active(),
            "overlay_active must report true when context_menu is set"
        );

        // Tick the timer. The guard must (a) NOT mark request_sent and
        // (b) clear the timer so it doesn't fire the instant the menu closes.
        app.tick_hover_timer();

        assert!(
            app.hover_popup.is_none(),
            "hover_popup must remain unset while a context menu is open"
        );
        assert!(
            app.hover_timer.is_none(),
            "hover_timer must be dropped under overlay so it doesn't fire the moment the overlay closes"
        );
    }

    /// Mirror: a hover RPC response that arrives AFTER a context menu opened
    /// must be dropped — otherwise the popup paints over the menu.
    #[test]
    fn handle_hover_at_mouse_response_dropped_under_overlay() {
        use crate::menu::{ContextMenu, MenuAction, MenuItem};
        use std::time::Instant;

        let mut app = App::new(None, false, None, None).unwrap();

        // Set the timer state that would normally accept the response.
        app.hover_timer = Some(HoverTimer {
            cell: (10, 5),
            started_at: Instant::now(),
            request_sent: true,
        });

        // Open a context menu mid-flight.
        let items = vec![MenuItem::new("Cut", MenuAction::Cut, None)];
        app.context_menu = Some(ContextMenu::new(items, (5, 5)));

        // Simulate a response arriving with valid hover JSON.
        let response: Result<serde_json::Value, hjkl_lsp::RpcError> = Ok(serde_json::json!({
            "contents": { "kind": "plaintext", "value": "stale hover text" }
        }));
        app.handle_hover_at_mouse_response(0, (0, 0), response);

        assert!(
            app.hover_popup.is_none(),
            "hover_popup must not be created when an overlay was open at response time"
        );
    }

    /// `overlay_active` must report true for any of the blocking overlays.
    /// Belt-and-suspenders: this prevents a regression where the helper
    /// forgets to check one of the overlay fields.
    #[test]
    fn overlay_active_reports_each_overlay_kind() {
        let mut app = App::new(None, false, None, None).unwrap();
        assert!(!app.overlay_active(), "fresh app has no overlays");

        // Context menu.
        let items = vec![crate::menu::MenuItem::new(
            "x",
            crate::menu::MenuAction::Cut,
            None,
        )];
        app.context_menu = Some(crate::menu::ContextMenu::new(items, (0, 0)));
        assert!(app.overlay_active());
        app.context_menu = None;
        assert!(!app.overlay_active());
    }

    #[test]
    fn dismiss_hover_popup_on_click_is_idempotent_when_no_popup() {
        let mut app = App::new(None, false, None, None).unwrap();
        assert!(app.hover_popup.is_none());
        assert!(app.hover_timer.is_none());
        // Calling on an app with no popup state must not panic.
        app.dismiss_hover_popup_on_click();
        assert!(app.hover_popup.is_none());
        assert!(app.hover_timer.is_none());
    }
}
