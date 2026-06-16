//! Diff mode (#208 Phase 2) — state, ex commands, and alignment cache.

use super::*;
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
    app.active_mut().editor.jump_cursor(0, 0);

    // ]c → first change (line 1), then second change (line 4).
    app.dispatch_action(AppAction::DiffNextChange, 1);
    assert_eq!(app.active().editor.cursor().0, 1);
    app.dispatch_action(AppAction::DiffNextChange, 1);
    assert_eq!(app.active().editor.cursor().0, 4);
    // No change below → cursor stays put.
    app.dispatch_action(AppAction::DiffNextChange, 1);
    assert_eq!(app.active().editor.cursor().0, 4);

    // [c → back to line 1.
    app.dispatch_action(AppAction::DiffPrevChange, 1);
    assert_eq!(app.active().editor.cursor().0, 1);

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
