//! Tests for the hop/easymotion label-jump overlay (#197).

use super::*;

use crate::app::hop::{HopKind, assign_labels};

// ─── assign_labels tests ────────────────────────────────────────────────────

#[test]
fn assign_labels_zero() {
    assert!(assign_labels(0).is_empty());
}

#[test]
fn assign_labels_single() {
    let l = assign_labels(1);
    assert_eq!(l, vec!["a"]);
}

#[test]
fn assign_labels_52_all_one_char() {
    let labels = assign_labels(52);
    assert_eq!(labels.len(), 52);
    // All must be 1 char.
    for l in &labels {
        assert_eq!(l.chars().count(), 1, "expected 1-char label, got {l:?}");
    }
    // All must be unique.
    let unique: std::collections::HashSet<_> = labels.iter().collect();
    assert_eq!(unique.len(), 52);
}

#[test]
fn assign_labels_53_all_two_char() {
    let labels = assign_labels(53);
    assert_eq!(labels.len(), 53);
    // All must be 2 chars.
    for l in &labels {
        assert_eq!(l.chars().count(), 2, "expected 2-char label, got {l:?}");
    }
    // All unique.
    let unique: std::collections::HashSet<_> = labels.iter().collect();
    assert_eq!(unique.len(), 53);
    // No label is a strict prefix of another (all same length → trivially true).
    for (i, a) in labels.iter().enumerate() {
        for (j, b) in labels.iter().enumerate() {
            if i != j && a.len() < b.len() {
                assert!(
                    !b.starts_with(a.as_str()),
                    "label {a:?} is a prefix of {b:?}"
                );
            }
        }
    }
}

#[test]
fn assign_labels_large() {
    let labels = assign_labels(200);
    assert_eq!(labels.len(), 200);
    let unique: std::collections::HashSet<_> = labels.iter().collect();
    assert_eq!(unique.len(), 200);
}

// ─── start_hop / cancel tests ───────────────────────────────────────────────

#[test]
fn hop_word_populates_targets() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world foo\nbar baz");

    app.start_hop(HopKind::Word);

    // hop should be active with at least 5 word starts (hello, world, foo, bar, baz).
    let hop = app.hop.as_ref().expect("hop should be active");
    assert!(!hop.targets.is_empty(), "expected word targets");
    // Labels are assigned.
    for t in &hop.targets {
        assert!(!t.label.is_empty());
    }
}

#[test]
fn hop_esc_cancels_and_clears() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world");

    app.start_hop(HopKind::Word);
    assert!(app.hop.is_some());

    let (orig_row, orig_col) = app.active_editor().cursor();

    // Escape cancels.
    app.hop_handle_key(None, true);
    assert!(app.hop.is_none(), "hop should be cleared after Esc");

    // Cursor unchanged.
    let (row, col) = app.active_editor().cursor();
    assert_eq!((row, col), (orig_row, orig_col));
}

#[test]
fn hop_label_resolves_cursor() {
    let mut app = App::new(None, false, None, None).unwrap();
    // "hello world" — word starts at col 0 ('h') and col 6 ('w').
    seed_buffer(&mut app, "hello world");
    app.active_editor_mut().jump_cursor(0, 0);
    app.sync_after_engine_mutation();

    app.start_hop(HopKind::Word);
    let hop = app.hop.as_ref().unwrap();
    // Get the label and target for the second word ("world" at col 6).
    let (target_row, target_col, label) = hop
        .targets
        .iter()
        .find(|t| t.col == 6)
        .map(|t| (t.row, t.col, t.label.clone()))
        .expect("expected target at col 6 for 'world'");

    // Type the label char(s).
    for c in label.chars() {
        app.hop_handle_key(Some(c), false);
    }

    // Hop should be resolved (None) and cursor at target.
    assert!(app.hop.is_none(), "hop should resolve after typing label");
    let (row, col) = app.active_editor().cursor();
    assert_eq!(row, target_row);
    assert_eq!(col, target_col);
}

#[test]
fn hop_non_matching_label_cancels() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello world");

    app.start_hop(HopKind::Word);
    assert!(app.hop.is_some());

    // Type a char that matches NO label (digits '1'..'9' are not in the label alphabet).
    app.hop_handle_key(Some('1'), false);

    assert!(app.hop.is_none(), "hop should cancel on no-match key");
}

// ─── WordCap targets ────────────────────────────────────────────────────────

#[test]
fn hop_word_cap_targets() {
    let mut app = App::new(None, false, None, None).unwrap();
    // Three WORD tokens: "hello", "world-foo", "bar"
    seed_buffer(&mut app, "hello world-foo bar");

    app.start_hop(HopKind::WordCap);
    let hop = app.hop.as_ref().expect("hop should be active");
    let row0: Vec<_> = hop.targets.iter().filter(|t| t.row == 0).collect();
    assert_eq!(
        row0.len(),
        3,
        "expected 3 WORD starts on 'hello world-foo bar'"
    );
    assert_eq!(row0[0].col, 0);
    assert_eq!(row0[1].col, 6);
    assert_eq!(row0[2].col, 16);
}

// ─── LineBelow / LineAbove targets ──────────────────────────────────────────

#[test]
fn hop_line_below_targets() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "line0\n  line1\nline2");
    app.active_editor_mut().jump_cursor(0, 0);
    app.sync_after_engine_mutation();

    // Cursor at row 0. LineBelow should produce rows 1 and 2.
    app.start_hop(HopKind::LineBelow);
    let hop = app.hop.as_ref().expect("hop should be active");
    let rows: Vec<usize> = hop.targets.iter().map(|t| t.row).collect();
    assert!(rows.contains(&1), "expected row 1 target");
    assert!(rows.contains(&2), "expected row 2 target");
    assert!(
        !rows.contains(&0),
        "row 0 (cursor row) should not be included"
    );
    // line1 starts with "  " so first-non-blank is col 2.
    let line1_target = hop.targets.iter().find(|t| t.row == 1).unwrap();
    assert_eq!(
        line1_target.col, 2,
        "first-non-blank of '  line1' should be col 2"
    );
}

#[test]
fn hop_line_above_targets() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "line0\nline1\nline2");
    // Move cursor to row 2.
    app.active_editor_mut().jump_cursor(2, 0);
    app.sync_after_engine_mutation();

    app.start_hop(HopKind::LineAbove);
    let hop = app.hop.as_ref().expect("hop should be active");
    let rows: Vec<usize> = hop.targets.iter().map(|t| t.row).collect();
    assert!(rows.contains(&0), "expected row 0 target");
    assert!(rows.contains(&1), "expected row 1 target");
    assert!(!rows.contains(&2), "cursor row should not be included");
}

// ─── Empty buffer / no targets ──────────────────────────────────────────────

#[test]
fn hop_no_targets_does_not_activate() {
    let mut app = App::new(None, false, None, None).unwrap();
    // Empty buffer — no word starts.
    seed_buffer(&mut app, "");

    app.start_hop(HopKind::Word);
    // hop should remain None since there are no targets.
    assert!(app.hop.is_none(), "hop should not activate on empty buffer");
}

// Operator-pending hop is intentionally unsupported (#197): with leader=`<Space>`,
// `d<Space>` is vim's delete-char motion, owned by the engine — see the
// `delete_space_deletes_char_right` engine test + the tier2_space_motion oracle.
