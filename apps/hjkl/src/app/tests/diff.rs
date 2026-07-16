//! Diff mode (#208 Phase 2) — state, ex commands, and alignment cache.

use super::*;
use crate::app::event_loop::KeyOutcome;
use crate::keymap_actions::AppAction;
use hjkl_app::diff::DiffRowKind;

/// `:diffsplit {file}` opens the file in a split and forms a 2-window diff pair
/// with a populated alignment cache.
#[test]
fn diffsplit_forms_pair_and_caches_alignment() {
    let a = std::env::temp_dir().join("hjkl_diffsplit_a.txt");
    let b = std::env::temp_dir().join("hjkl_diffsplit_b.txt");
    std::fs::write(&a, "one\ntwo\nthree\n").unwrap();
    std::fs::write(&b, "one\nTWO\nthree\n").unwrap();
    let mut app = App::new(Some(a.clone()), false, None, None).unwrap();

    app.dispatch_ex(&format!("diffsplit {}", b.display()));

    assert_eq!(
        app.diff_windows.len(),
        2,
        "diffsplit forms a 2-window group"
    );
    assert!(app.diff_pair().is_some(), "a diff pair must be active");
    let cache = app.diff_cache.as_ref().expect("alignment must be cached");
    // Exactly one Change row (line 2 differs), rest Equal.
    let changes = cache
        .diff
        .rows
        .iter()
        .filter(|r| r.kind == DiffRowKind::Change)
        .count();
    assert_eq!(changes, 1, "one changed line between the buffers");
    assert!(!cache.diff.is_empty_diff());

    let _ = std::fs::remove_file(&a);
    let _ = std::fs::remove_file(&b);
}

/// `:diffoff` disbands the (2-window) group and drops the cache.
#[test]
fn diffoff_disbands_group() {
    let a = std::env::temp_dir().join("hjkl_diffoff_a.txt");
    let b = std::env::temp_dir().join("hjkl_diffoff_b.txt");
    std::fs::write(&a, "x\n").unwrap();
    std::fs::write(&b, "y\n").unwrap();
    let mut app = App::new(Some(a.clone()), false, None, None).unwrap();
    app.dispatch_ex(&format!("diffsplit {}", b.display()));
    assert!(app.diff_pair().is_some());

    app.dispatch_ex("diffoff");

    assert!(app.diff_windows.is_empty(), "diffoff clears the group");
    assert!(app.diff_pair().is_none());
    assert!(app.diff_cache.is_none(), "cache dropped on diffoff");

    let _ = std::fs::remove_file(&a);
    let _ = std::fs::remove_file(&b);
}

/// A single `:diffthis` window is not yet a pair (needs a second window).
#[test]
fn diffthis_single_window_is_not_a_pair() {
    let a = std::env::temp_dir().join("hjkl_diffthis_solo.txt");
    std::fs::write(&a, "x\n").unwrap();
    let mut app = App::new(Some(a.clone()), false, None, None).unwrap();

    app.dispatch_ex("diffthis");

    assert_eq!(app.diff_windows.len(), 1);
    assert!(
        app.diff_pair().is_none(),
        "one window cannot form a diff pair"
    );
    assert!(app.diff_cache.is_none());

    let _ = std::fs::remove_file(&a);
}

/// `]c` / `[c` jump the cursor between change hunks in a diff window.
#[test]
fn diff_change_navigation_jumps_between_hunks() {
    let a = std::env::temp_dir().join("hjkl_diffnav_a.txt");
    let b = std::env::temp_dir().join("hjkl_diffnav_b.txt");
    std::fs::write(&a, "l0\nl1\nl2\nl3\nl4\n").unwrap();
    std::fs::write(&b, "l0\nX1\nl2\nl3\nX4\n").unwrap();
    let mut app = App::new(Some(a.clone()), false, None, None).unwrap();
    app.dispatch_ex(&format!("diffsplit {}", b.display()));
    // Focused window is the opened (b) side; start at the top.
    app.active_editor_mut().jump_cursor(0, 0);

    // ]c → first change (line 1), then second change (line 4).
    app.dispatch_action(AppAction::DiffNextChange, 1);
    assert_eq!(app.active_editor().cursor().0, 1);
    app.dispatch_action(AppAction::DiffNextChange, 1);
    assert_eq!(app.active_editor().cursor().0, 4);
    // No change below → cursor stays put.
    app.dispatch_action(AppAction::DiffNextChange, 1);
    assert_eq!(app.active_editor().cursor().0, 4);

    // [c → back to line 1.
    app.dispatch_action(AppAction::DiffPrevChange, 1);
    assert_eq!(app.active_editor().cursor().0, 1);

    let _ = std::fs::remove_file(&a);
    let _ = std::fs::remove_file(&b);
}

/// Scroll-bind: scrolling the focused diff window aligns the partner's top row
/// across an insertion (`ins` exists only in b, so b's lines are offset by one).
#[test]
fn diff_scroll_binds_partner_top_row() {
    let a = std::env::temp_dir().join("hjkl_diffscroll_a.txt");
    let b = std::env::temp_dir().join("hjkl_diffscroll_b.txt");
    std::fs::write(&a, "l0\nl1\nl2\nl3\nl4\nl5\nzebra\n").unwrap();
    std::fs::write(&b, "l0\nl1\nins\nl2\nl3\nl4\nl5\nzebra\n").unwrap();
    let mut app = App::new(Some(a.clone()), false, None, None).unwrap();
    app.dispatch_ex(&format!("diffsplit {}", b.display()));

    // Pair: a_win = original (a), b_win = opened (b); focus is on b.
    let (a_win, b_win) = app.diff_pair().unwrap();
    assert_eq!(app.focused_window(), b_win);

    // Scroll the focused (b) window so its top line is `l2` (b line index 3).
    // Scroll lives on the window's own editor (#151 Phase D).
    app.window_editors
        .get_mut(&b_win)
        .unwrap()
        .host_mut()
        .viewport_mut()
        .top_row = 3;
    app.sync_diff_scroll();

    // `l2` is a line index 2 in buffer a → partner top must align there.
    assert_eq!(
        app.window_scroll(a_win).0,
        2,
        "partner window must scroll-bind to the aligned line"
    );

    let _ = std::fs::remove_file(&a);
    let _ = std::fs::remove_file(&b);
}

/// Regression for the `dispatch_fallthrough_key` drift bug (audit R2 fix 1):
/// a fall-through key edit — here repeated Insert-mode `<Delete>`, which (like
/// Backspace/Enter/Ctrl-w/etc.) is NOT intercepted by any of `handle_keypress`'s
/// overlay/completion arms and so reaches `KeyOutcome::FallThrough` — must
/// refresh the diff alignment cache exactly like `sync_after_engine_mutation`
/// does, and must never leave `diff_cache` pointing at rows the shrunk buffer
/// no longer has.
///
/// Drives keys through the SAME path `App::run` uses in production —
/// `handle_keypress` first, falling through to `dispatch_fallthrough_key` on
/// `KeyOutcome::FallThrough` — rather than calling engine primitives directly,
/// so this exercises the exact call site that drifted from
/// `sync_after_engine_mutation` and dropped diff-cache refresh + sibling fold
/// invalidation. Before the fix, deleting buffer `b` down to fewer lines than
/// the stale cache's `Change` row indexes panics exactly like the bug report's
/// `dG`-near-EOF repro:
/// `thread '...' panicked at .../ropey-.../src/rope.rs:826:13: Attempt to
/// index past end of Rope: line index 2, Rope line length 2` — raised from
/// `hjkl_buffer::rope_line_str` (`line_text` in `diff_mode.rs`) when
/// `diff_line_classes` reads a `Change` row whose cached `b` index the
/// shrunk buffer no longer has. That's exactly what a diff-mode render calls
/// every frame, so this is a straight-line panic repro, not just a staleness
/// check.
#[test]
fn fallthrough_key_edit_refreshes_diff_alignment_and_avoids_stale_row_panic() {
    let a = tmp_path("hjkl_diff_dg_a.txt");
    let b = tmp_path("hjkl_diff_dg_b.txt");
    std::fs::write(&a, "one\ntwo\nthree\nfour\nfive\n").unwrap();
    std::fs::write(&b, "one\ntwo\nTHREE\nfour\nfive\n").unwrap();
    let mut app = App::new(Some(a.clone()), false, None, None).unwrap();
    app.dispatch_ex(&format!("diffsplit {}", b.display()));
    let (_, b_win) = app.diff_pair().expect("diffsplit must form a pair");
    // Sanity: alignment cache has 5 aligned rows, one `Change` at b-line 2
    // ("THREE" vs "three") — the row whose stale index must not survive.
    assert_eq!(app.diff_cache.as_ref().unwrap().diff.rows.len(), 5);

    // Focus (after diffsplit) is on the opened `b` window. Enter Insert mode
    // at the start of line 1 ("two") and hold <Delete> down: each press
    // merges/eats forward, eventually deleting "two\nTHREE\nfour\nfive\n"
    // entirely and leaving just "one\n" — shrinking `b` well past the stale
    // cache's `Change` row (b-line 2), which is exactly what triggers the
    // rope-index panic if the cache isn't refreshed.
    app.active_editor_mut().jump_cursor(1, 0);
    app.sync_viewport_from_editor();
    app.active_editor_mut().enter_insert_i(1);
    app.sync_after_engine_mutation();
    let delete_key =
        crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Delete, KeyModifiers::NONE);
    for _ in 0..40 {
        // Mirror `App::run` exactly: `handle_keypress` first, and only on
        // `FallThrough` call `dispatch_fallthrough_key` — the method this fix
        // introduced to close the drift gap. Plain <Delete> in Insert mode
        // (no completion popup open) is one of the keys that actually reaches
        // this path in production (unlike most Normal-mode operators, which
        // route entirely through `route_chord_key` + `sync_after_engine_mutation`
        // already).
        if let KeyOutcome::FallThrough = app.handle_keypress(delete_key) {
            app.dispatch_fallthrough_key(delete_key);
        }
    }
    assert_eq!(
        app.active_editor().buffer().rope().to_string(),
        "one\n",
        "buffer b must have shrunk to just the first line"
    );

    // The alignment cache must have been refreshed against the shrunk
    // buffer — no aligned row may reference a `b` line that no longer
    // exists (a stale row referencing b-line 2 is exactly what panics below).
    let cache = app.diff_cache.as_ref().expect("cache must still exist");
    for row in &cache.diff.rows {
        if let Some(bi) = row.b {
            assert!(
                bi < 1,
                "stale diff row references deleted b-line {bi} — diff_cache was not refreshed"
            );
        }
    }

    // The actual panic repro: rendering calls this every frame. Must not
    // panic even though the buffer shrank past the old cache's row count.
    let _ = app.diff_line_classes(b_win);

    let _ = std::fs::remove_file(&a);
    let _ = std::fs::remove_file(&b);
}

/// Editing a diff buffer refreshes the cached alignment (gen-keyed recompute).
#[test]
fn editing_refreshes_alignment_cache() {
    let a = std::env::temp_dir().join("hjkl_diffedit_a.txt");
    let b = std::env::temp_dir().join("hjkl_diffedit_b.txt");
    std::fs::write(&a, "same\nsame\n").unwrap();
    std::fs::write(&b, "same\nsame\n").unwrap();
    let mut app = App::new(Some(a.clone()), false, None, None).unwrap();
    app.dispatch_ex(&format!("diffsplit {}", b.display()));
    // Identical buffers → no changes.
    assert!(app.diff_cache.as_ref().unwrap().diff.is_empty_diff());

    // Focus is on the opened (b) window after diffsplit; edit it.
    seed_buffer(&mut app, "same\nCHANGED\n");
    app.sync_after_engine_mutation();

    let cache = app.diff_cache.as_ref().unwrap();
    assert!(
        !cache.diff.is_empty_diff(),
        "edit must invalidate + recompute the alignment"
    );

    let _ = std::fs::remove_file(&a);
    let _ = std::fs::remove_file(&b);
}
