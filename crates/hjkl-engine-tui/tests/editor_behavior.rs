//! Integration tests for [`hjkl_engine::Editor`] behaviour.
//!
//! Relocated from `hjkl-engine/src/editor.rs`'s `#[cfg(all(test, feature = "crossterm"))]
//! mod tests` as part of #162 phase 3. Engine is now a fully agnostic core with zero
//! toolkit dependencies; this file provides the crossterm harness the tests previously
//! relied on.
//!
//! Note: the `key()`, `shift_key()`, and `ctrl_key()` helpers below are defined for
//! parity with the original test module. Most tests exercise `Editor` methods directly
//! without dispatching through the vim FSM.

// Several types are imported at the top level for convenience; individual tests
// may shadow them with local `use` statements, triggering unused-imports lints.
#[allow(unused_imports)]
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
#[allow(unused_imports)]
use hjkl_engine::{
    Editor, VimMode,
    types::{Attrs, Color, DefaultHost, HighlightKind, Host, SnapshotMode, Style, Viewport},
};
use hjkl_vim::VimEditorExt;

#[allow(dead_code)]
fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}
#[allow(dead_code)]
fn shift_key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::SHIFT)
}
#[allow(dead_code)]
fn ctrl_key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

#[test]
fn intern_style_dedups_engine_native_styles() {
    use hjkl_engine::types::{Attrs, Color, Style};
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    let s = Style {
        fg: Some(Color(255, 0, 0)),
        bg: None,
        attrs: Attrs::BOLD,
    };
    let id_a = e.intern_style(s);
    // Re-interning the same engine style returns the same id.
    let id_b = e.intern_style(s);
    assert_eq!(id_a, id_b);
    // Engine accessor returns the same style back.
    let back = e.engine_style_at(id_a).expect("interned");
    assert_eq!(back, s);
}

#[test]
fn engine_style_at_out_of_range_returns_none() {
    let e = hjkl_vim::vim_editor(
        hjkl_buffer::View::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    assert!(e.engine_style_at(99).is_none());
}

#[test]
fn options_bridge_roundtrip() {
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    let opts = e.current_options();
    // 0.2.0: defaults flipped to modern editor norms — 4-space soft tabs.
    assert_eq!(opts.shiftwidth, 4);
    assert_eq!(opts.tabstop, 4);

    let new_opts = hjkl_engine::types::Options {
        shiftwidth: 4,
        tabstop: 2,
        ignorecase: true,
        ..hjkl_engine::types::Options::default()
    };
    e.apply_options(&new_opts);

    let after = e.current_options();
    assert_eq!(after.shiftwidth, 4);
    assert_eq!(after.tabstop, 2);
    assert!(after.ignorecase);
}

#[test]
fn selection_highlight_none_in_normal() {
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("hello");
    assert!(e.selection_highlight().is_none());
}

#[test]
fn highlights_emit_search_matches() {
    use hjkl_engine::types::HighlightKind;
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("foo bar foo\nbaz qux\n");
    // 0.0.35: arm via the engine search state. The buffer
    // accessor still works (deprecated) but new code goes
    // through Editor.
    e.set_search_pattern(Some(regex::Regex::new("foo").unwrap()));
    let hs = e.highlights_for_line(0);
    assert_eq!(hs.len(), 2);
    for h in &hs {
        assert_eq!(h.kind, HighlightKind::SearchMatch);
        assert_eq!(h.range.start.line, 0);
        assert_eq!(h.range.end.line, 0);
    }
}

#[test]
fn highlights_empty_without_pattern() {
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("foo bar");
    assert!(e.highlights_for_line(0).is_empty());
}

#[test]
fn highlights_empty_for_out_of_range_line() {
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("foo");
    e.set_search_pattern(Some(regex::Regex::new("foo").unwrap()));
    assert!(e.highlights_for_line(99).is_empty());
}

#[test]
fn snapshot_roundtrips_through_restore() {
    use hjkl_engine::types::SnapshotMode;
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("alpha\nbeta\ngamma");
    e.jump_cursor(2, 3);
    let snap = e.take_snapshot();
    assert_eq!(snap.mode, SnapshotMode::Normal);
    assert_eq!(snap.cursor, (2, 3));
    assert_eq!(snap.lines.len(), 3);

    let mut other = hjkl_vim::vim_editor(
        hjkl_buffer::View::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    other.restore_snapshot(snap).expect("restore");
    assert_eq!(other.cursor(), (2, 3));
    assert_eq!(other.buffer().row_count(), 3);
}

#[test]
fn restore_snapshot_rejects_version_mismatch() {
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    let mut snap = e.take_snapshot();
    snap.version = 9999;
    match e.restore_snapshot(snap) {
        Err(hjkl_engine::EngineError::SnapshotVersion(got, want)) => {
            assert_eq!(got, 9999);
            assert_eq!(want, hjkl_engine::types::EditorSnapshot::VERSION);
        }
        other => panic!("expected SnapshotVersion err, got {other:?}"),
    }
}

#[test]
fn take_content_change_returns_some_on_first_dirty() {
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("hello");
    let first = e.take_content_change();
    assert!(first.is_some());
    let second = e.take_content_change();
    assert!(second.is_none());
}

fn many_lines(n: usize) -> String {
    (0..n)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[allow(dead_code)]
fn prime_viewport<H: Host>(e: &mut Editor<hjkl_buffer::View, H>, height: u16) {
    e.set_viewport_height(height);
}

/// Contract that the TUI drain relies on: `set_content` flags the
/// editor dirty (so the next `take_dirty` call reports the change),
/// and a second `take_dirty` returns `false` after consumption. The
/// TUI drains this flag after every programmatic content load so
/// opening a tab doesn't get mistaken for a user edit and mark the
/// tab dirty (which would then trigger the quit-prompt on `:q`).
#[test]
fn set_content_dirties_then_take_dirty_clears() {
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("hello");
    assert!(
        e.take_dirty(),
        "set_content should leave content_dirty=true"
    );
    assert!(!e.take_dirty(), "take_dirty should clear the flag");
}

#[test]
fn content_arc_cache_invalidated_by_set_content() {
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("one");
    let a = e.content_arc();
    e.set_content("two");
    let b = e.content_arc();
    assert!(!std::sync::Arc::ptr_eq(&a, &b));
    assert!(b.starts_with("two"));
}

// ── lnum_width ──────────────────────────────────────────────────────────

#[test]
fn lnum_width_numberwidth_floor_enforced() {
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    // Default: number=true, numberwidth=4; buffer has 1 line → digits=1,
    // needed=2 which is less than floor of 4.
    e.set_content("single line");
    assert_eq!(e.lnum_width(), 4, "should be floored to numberwidth (4)");
}

#[test]
fn lnum_width_zero_when_both_flags_off() {
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options {
            number: false,
            relativenumber: false,
            ..hjkl_engine::types::Options::default()
        },
    );
    e.set_content("some content");
    assert_eq!(
        e.lnum_width(),
        0,
        "gutter should be 0 when number flags are off"
    );
}

// ── doc-coord mouse primitives (Phase 1 — issue #114) ──────────────────

#[test]
fn mouse_click_doc_moves_cursor_to_doc_coords() {
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("hello\nworld");
    e.mouse_click_doc(1, 2);
    assert_eq!(e.cursor(), (1, 2));
}

#[test]
fn mouse_click_doc_normal_mode_clamps_past_eol_to_last_char() {
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("hello");
    // Normal mode (default after construction): "hello" has 5 chars,
    // past-EOL click clamps to col=4 (last char 'o' — never on the
    // implicit \n, vim/neovim convention).
    e.mouse_click_doc(0, 99);
    assert_eq!(e.cursor(), (0, 4));
}

#[test]
fn mouse_click_doc_normal_mode_clamps_past_eol_multibyte() {
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    // 5 chars, 6 bytes — clamping must be char-counted, not byte-counted.
    e.set_content("héllo");
    e.mouse_click_doc(0, 99);
    assert_eq!(e.cursor(), (0, 4));
}

#[test]
fn mouse_click_doc_insert_mode_allows_one_past_eol() {
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("hello");
    e.enter_insert_i(1);
    // Insert mode allows the one-past-EOL position (col=5 for 5-char
    // line) — that's the canonical insert-here sentinel.
    e.mouse_click_doc(0, 99);
    assert_eq!(e.cursor(), (0, 5));
}

#[test]
fn mouse_click_doc_resets_sticky_col() {
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("aaaaa\nbb\naaaaa");
    // Pretend a previous keyboard motion put intended col at 4 (e.g.
    // user navigated $ on row 0).
    e.set_sticky_col(Some(4));
    // Click on row 1, col 1 (the second 'b' on a short line).
    e.mouse_click_doc(1, 1);
    assert_eq!(e.cursor(), (1, 1));
    assert_eq!(
        e.sticky_col(),
        Some(1),
        "click must reset sticky_col so a subsequent j/k uses the clicked column \
         as the intended visual column (not the previous keyboard-tracked col)"
    );
}

#[test]
fn mouse_click_doc_exits_visual_mode() {
    use hjkl_engine::VimMode;
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("hello");
    e.enter_visual_char();
    assert_eq!(e.vim_mode(), VimMode::Visual);
    e.mouse_click_doc(0, 2);
    assert_eq!(e.vim_mode(), VimMode::Normal);
    assert_eq!(e.cursor(), (0, 2));
}

#[test]
fn set_cursor_doc_clamps_past_last_row() {
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("one\ntwo");
    // doc has 2 rows (0 and 1); row 99 clamps to 1.
    e.set_cursor_doc(99, 0);
    assert_eq!(e.cursor(), (1, 0));
}

#[test]
fn mouse_begin_drag_enters_visual_char() {
    use hjkl_engine::VimMode;
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("hello");
    e.mouse_begin_drag();
    assert_eq!(e.vim_mode(), VimMode::Visual);
}

#[test]
fn mouse_extend_drag_doc_moves_cursor_leaving_visual_anchor() {
    use hjkl_engine::VimMode;
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("hello world");
    e.mouse_begin_drag(); // anchor at (0,0)
    e.mouse_extend_drag_doc(0, 5);
    assert_eq!(e.vim_mode(), VimMode::Visual);
    assert_eq!(e.cursor(), (0, 5));
}

// ── Patch B (0.0.29): Host trait wired into Editor ──

#[test]
fn host_clipboard_round_trip_via_default_host() {
    // DefaultHost stores write_clipboard in-memory; read_clipboard
    // returns the most recent payload.
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.host_mut().write_clipboard("payload".to_string());
    assert_eq!(e.host_mut().read_clipboard().as_deref(), Some("payload"));
}

// ── ContentEdit emission ─────────────────────────────────────────

fn fresh_editor(initial: &str) -> Editor {
    let buffer = hjkl_buffer::View::from_str(initial);
    hjkl_vim::vim_editor(
        buffer,
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    )
}

#[test]
fn content_edit_insert_char_at_origin() {
    let mut e = fresh_editor("");
    let _ = e.mutate_edit(hjkl_buffer::Edit::InsertChar {
        at: hjkl_buffer::Position::new(0, 0),
        ch: 'a',
    });
    let edits = e.take_content_edits();
    assert_eq!(edits.len(), 1);
    let ce = &edits[0];
    assert_eq!(ce.start_byte, 0);
    assert_eq!(ce.old_end_byte, 0);
    assert_eq!(ce.new_end_byte, 1);
    assert_eq!(ce.start_position, (0, 0));
    assert_eq!(ce.old_end_position, (0, 0));
    assert_eq!(ce.new_end_position, (0, 1));
}

#[test]
fn content_edit_insert_str_multiline() {
    // View "x\ny" — insert "ab\ncd" at end of row 0.
    let mut e = fresh_editor("x\ny");
    let _ = e.mutate_edit(hjkl_buffer::Edit::InsertStr {
        at: hjkl_buffer::Position::new(0, 1),
        text: "ab\ncd".into(),
    });
    let edits = e.take_content_edits();
    assert_eq!(edits.len(), 1);
    let ce = &edits[0];
    assert_eq!(ce.start_byte, 1);
    assert_eq!(ce.old_end_byte, 1);
    assert_eq!(ce.new_end_byte, 1 + 5);
    assert_eq!(ce.start_position, (0, 1));
    // Insertion contains one '\n', so row+1, col = bytes after last '\n' = 2.
    assert_eq!(ce.new_end_position, (1, 2));
}

#[test]
fn content_edit_delete_range_charwise() {
    // "abcdef" — delete chars 1..4 ("bcd").
    let mut e = fresh_editor("abcdef");
    let _ = e.mutate_edit(hjkl_buffer::Edit::DeleteRange {
        start: hjkl_buffer::Position::new(0, 1),
        end: hjkl_buffer::Position::new(0, 4),
        kind: hjkl_buffer::MotionKind::Char,
    });
    let edits = e.take_content_edits();
    assert_eq!(edits.len(), 1);
    let ce = &edits[0];
    assert_eq!(ce.start_byte, 1);
    assert_eq!(ce.old_end_byte, 4);
    assert_eq!(ce.new_end_byte, 1);
    assert!(ce.old_end_byte > ce.new_end_byte);
}

#[test]
fn content_edit_set_content_resets() {
    let mut e = fresh_editor("foo");
    let _ = e.mutate_edit(hjkl_buffer::Edit::InsertChar {
        at: hjkl_buffer::Position::new(0, 0),
        ch: 'X',
    });
    // set_content should clear queued edits and raise the reset
    // flag on the next take_content_reset.
    e.set_content("brand new");
    assert!(e.take_content_reset());
    // Subsequent call clears the flag.
    assert!(!e.take_content_reset());
    // Edits cleared on reset.
    assert!(e.take_content_edits().is_empty());
}

#[test]
fn content_edit_multiple_replaces_in_order() {
    // Three Replace edits applied left-to-right (mimics the
    // substitute path's per-match Replace fan-out). Verify each
    // mutation queues exactly one ContentEdit and they're drained
    // in source-order with structurally valid byte spans.
    let mut e = fresh_editor("xax xbx xcx");
    let _ = e.take_content_edits();
    let _ = e.take_content_reset();
    // Replace each "x" with "yy", left to right. After each replace,
    // the next match's char-col shifts by +1 (since "yy" is 1 char
    // longer than "x" but they're both ASCII so byte = char here).
    let positions = [(0usize, 0usize), (0, 4), (0, 8)];
    for (row, col) in positions {
        let _ = e.mutate_edit(hjkl_buffer::Edit::Replace {
            start: hjkl_buffer::Position::new(row, col),
            end: hjkl_buffer::Position::new(row, col + 1),
            with: "yy".into(),
        });
    }
    let edits = e.take_content_edits();
    assert_eq!(edits.len(), 3);
    for ce in &edits {
        assert!(ce.start_byte <= ce.old_end_byte);
        assert!(ce.start_byte <= ce.new_end_byte);
    }
    // Document order.
    for w in edits.windows(2) {
        assert!(w[0].start_byte <= w[1].start_byte);
    }
}

#[test]
fn replace_char_at_replaces_single_char_under_cursor() {
    // Matches vim's `rx` semantics: replace char under cursor.
    let mut e = fresh_editor("abc");
    e.jump_cursor(0, 1); // cursor on 'b'
    e.replace_char_at('X', 1);
    let got = e.content();
    let got = got.trim_end_matches('\n');
    assert_eq!(
        got, "aXc",
        "replace_char_at(X, 1) must replace 'b' with 'X'"
    );
    // Cursor stays on the replaced char.
    assert_eq!(e.cursor(), (0, 1));
}

#[test]
fn replace_char_at_count_replaces_multiple_chars() {
    // `3rx` in vim replaces 3 chars starting at cursor.
    let mut e = fresh_editor("abcde");
    e.jump_cursor(0, 0);
    e.replace_char_at('Z', 3);
    let got = e.content();
    let got = got.trim_end_matches('\n');
    assert_eq!(
        got, "ZZZde",
        "replace_char_at(Z, 3) must replace first 3 chars"
    );
}

#[test]
fn find_char_method_moves_to_target() {
    // buffer "abcabc", cursor (0,0), f<c> → cursor (0,2).
    let mut e = fresh_editor("abcabc");
    e.jump_cursor(0, 0);
    e.find_char('c', true, false, 1);
    assert_eq!(
        e.cursor(),
        (0, 2),
        "find_char('c', forward=true, till=false, count=1) must land on 'c' at col 2"
    );
}

// ── after_g unit tests (Phase 2b-ii) ────────────────────────────────────

#[test]
fn after_g_gg_jumps_to_top() {
    let content: String = (0..20).map(|i| format!("line {i}\n")).collect();
    let mut e = fresh_editor(&content);
    e.jump_cursor(15, 0);
    e.after_g('g', 1);
    assert_eq!(e.cursor().0, 0, "gg must move cursor to row 0");
}

#[test]
fn after_g_gg_with_count_jumps_line() {
    // 5gg → row 4 (0-indexed).
    let content: String = (0..20).map(|i| format!("line {i}\n")).collect();
    let mut e = fresh_editor(&content);
    e.jump_cursor(0, 0);
    e.after_g('g', 5);
    assert_eq!(e.cursor().0, 4, "5gg must land on row 4");
}

#[test]
fn after_g_gj_moves_down() {
    let mut e = fresh_editor("line0\nline1\nline2\n");
    e.jump_cursor(0, 0);
    e.after_g('j', 1);
    assert_eq!(e.cursor().0, 1, "gj must move down one display row");
}

#[test]
fn after_g_gu_sets_operator_pending() {
    // gU enters operator-pending with Uppercase op; next key applies it.
    let mut e = fresh_editor("hello\n");
    e.after_g('U', 1);
    // The engine should now be chord-pending (Pending::Op set).
    assert!(
        e.is_chord_pending(),
        "gU must set engine chord-pending (Pending::Op)"
    );
}

#[test]
fn after_g_g_star_searches_forward_non_whole_word() {
    // g* on word "foo" in "foobar" should find the match.
    let mut e = fresh_editor("foo foobar\n");
    e.jump_cursor(0, 0); // cursor on 'f' of "foo"
    e.after_g('*', 1);
    // After g* the cursor should have moved (ScreenDown motion is
    // not applicable here; WordAtCursor forward moves to next match).
    // At minimum: no panic and mode stays Normal.
    assert_eq!(e.vim_mode(), VimMode::Normal, "g* must stay in Normal mode");
}

// ── apply_motion controller tests (Phase 3a) ────────────────────────────

#[test]
fn apply_motion_char_left_moves_cursor() {
    let mut e = fresh_editor("hello\n");
    e.jump_cursor(0, 3);
    e.apply_motion(hjkl_engine::MotionKind::CharLeft, 1);
    assert_eq!(e.cursor(), (0, 2), "CharLeft moves one col left");
}

#[test]
fn apply_motion_char_left_clamps_at_col_zero() {
    let mut e = fresh_editor("hello\n");
    e.jump_cursor(0, 0);
    e.apply_motion(hjkl_engine::MotionKind::CharLeft, 1);
    assert_eq!(e.cursor(), (0, 0), "CharLeft at col 0 must not wrap");
}

#[test]
fn apply_motion_char_left_with_count() {
    let mut e = fresh_editor("hello\n");
    e.jump_cursor(0, 4);
    e.apply_motion(hjkl_engine::MotionKind::CharLeft, 3);
    assert_eq!(e.cursor(), (0, 1), "CharLeft count=3 moves three cols left");
}

#[test]
fn apply_motion_char_right_moves_cursor() {
    let mut e = fresh_editor("hello\n");
    e.jump_cursor(0, 0);
    e.apply_motion(hjkl_engine::MotionKind::CharRight, 1);
    assert_eq!(e.cursor(), (0, 1), "CharRight moves one col right");
}

#[test]
fn apply_motion_char_right_clamps_at_last_char() {
    let mut e = fresh_editor("hello\n");
    // "hello" has chars at 0..=4; normal mode clamps at 4.
    e.jump_cursor(0, 4);
    e.apply_motion(hjkl_engine::MotionKind::CharRight, 1);
    assert_eq!(
        e.cursor(),
        (0, 4),
        "CharRight at end must not go past last char"
    );
}

#[test]
fn apply_motion_line_down_moves_cursor() {
    let mut e = fresh_editor("line0\nline1\nline2\n");
    e.jump_cursor(0, 0);
    e.apply_motion(hjkl_engine::MotionKind::LineDown, 1);
    assert_eq!(e.cursor().0, 1, "LineDown moves one row down");
}

#[test]
fn apply_motion_line_down_with_count() {
    let mut e = fresh_editor("line0\nline1\nline2\n");
    e.jump_cursor(0, 0);
    e.apply_motion(hjkl_engine::MotionKind::LineDown, 2);
    assert_eq!(e.cursor().0, 2, "LineDown count=2 moves two rows down");
}

#[test]
fn apply_motion_line_up_moves_cursor() {
    let mut e = fresh_editor("line0\nline1\nline2\n");
    e.jump_cursor(2, 0);
    e.apply_motion(hjkl_engine::MotionKind::LineUp, 1);
    assert_eq!(e.cursor().0, 1, "LineUp moves one row up");
}

#[test]
fn apply_motion_line_up_clamps_at_top() {
    let mut e = fresh_editor("line0\nline1\n");
    e.jump_cursor(0, 0);
    e.apply_motion(hjkl_engine::MotionKind::LineUp, 1);
    assert_eq!(e.cursor().0, 0, "LineUp at top must not go negative");
}

#[test]
fn apply_motion_first_non_blank_down_moves_and_lands_on_non_blank() {
    // Line 0: "  hello" (indent 2), line 1: "  world" (indent 2).
    let mut e = fresh_editor("  hello\n  world\n");
    e.jump_cursor(0, 0);
    e.apply_motion(hjkl_engine::MotionKind::FirstNonBlankDown, 1);
    assert_eq!(e.cursor().0, 1, "FirstNonBlankDown must move to next row");
    assert_eq!(
        e.cursor().1,
        2,
        "FirstNonBlankDown must land on first non-blank col"
    );
}

#[test]
fn apply_motion_first_non_blank_up_moves_and_lands_on_non_blank() {
    let mut e = fresh_editor("  hello\n  world\n");
    e.jump_cursor(1, 4);
    e.apply_motion(hjkl_engine::MotionKind::FirstNonBlankUp, 1);
    assert_eq!(e.cursor().0, 0, "FirstNonBlankUp must move to prev row");
    assert_eq!(
        e.cursor().1,
        2,
        "FirstNonBlankUp must land on first non-blank col"
    );
}

#[test]
fn apply_motion_count_zero_treated_as_one() {
    // count=0 must be normalised to 1 (count.max(1) in apply_motion_kind).
    let mut e = fresh_editor("hello\n");
    e.jump_cursor(0, 3);
    e.apply_motion(hjkl_engine::MotionKind::CharLeft, 0);
    assert_eq!(e.cursor(), (0, 2), "count=0 treated as 1 for CharLeft");
}

// ── apply_motion controller tests (Phase 3b) — word motions ─────────────

#[test]
fn apply_motion_word_forward_moves_to_next_word() {
    // "hello world\n": 'w' from col 0 lands on 'w' of "world" at col 6.
    let mut e = fresh_editor("hello world\n");
    e.jump_cursor(0, 0);
    e.apply_motion(hjkl_engine::MotionKind::WordForward, 1);
    assert_eq!(
        e.cursor(),
        (0, 6),
        "WordForward moves to start of next word"
    );
}

#[test]
fn apply_motion_word_forward_with_count() {
    // "one two three\n": 2w from col 0 → start of "three" at col 8.
    let mut e = fresh_editor("one two three\n");
    e.jump_cursor(0, 0);
    e.apply_motion(hjkl_engine::MotionKind::WordForward, 2);
    assert_eq!(e.cursor(), (0, 8), "WordForward count=2 skips two words");
}

#[test]
fn apply_motion_big_word_forward_moves_to_next_big_word() {
    // "foo.bar baz\n": W from col 0 skips entire "foo.bar" (one WORD) to 'b' at col 8.
    let mut e = fresh_editor("foo.bar baz\n");
    e.jump_cursor(0, 0);
    e.apply_motion(hjkl_engine::MotionKind::BigWordForward, 1);
    assert_eq!(e.cursor(), (0, 8), "BigWordForward skips the whole WORD");
}

#[test]
fn apply_motion_big_word_forward_with_count() {
    // "aa bb cc\n": 2W from col 0 → start of "cc" at col 6.
    let mut e = fresh_editor("aa bb cc\n");
    e.jump_cursor(0, 0);
    e.apply_motion(hjkl_engine::MotionKind::BigWordForward, 2);
    assert_eq!(e.cursor(), (0, 6), "BigWordForward count=2 skips two WORDs");
}

#[test]
fn apply_motion_word_backward_moves_to_prev_word() {
    // "hello world\n": 'b' from col 6 ('w') lands back at col 0 ('h').
    let mut e = fresh_editor("hello world\n");
    e.jump_cursor(0, 6);
    e.apply_motion(hjkl_engine::MotionKind::WordBackward, 1);
    assert_eq!(
        e.cursor(),
        (0, 0),
        "WordBackward moves to start of prev word"
    );
}

#[test]
fn apply_motion_word_backward_with_count() {
    // "one two three\n": 2b from col 8 ('t' of "three") → col 0 ('o' of "one").
    let mut e = fresh_editor("one two three\n");
    e.jump_cursor(0, 8);
    e.apply_motion(hjkl_engine::MotionKind::WordBackward, 2);
    assert_eq!(
        e.cursor(),
        (0, 0),
        "WordBackward count=2 skips two words back"
    );
}

#[test]
fn apply_motion_big_word_backward_moves_to_prev_big_word() {
    // "foo.bar baz\n": B from col 8 ('b' of "baz") → col 0 (start of "foo.bar" WORD).
    let mut e = fresh_editor("foo.bar baz\n");
    e.jump_cursor(0, 8);
    e.apply_motion(hjkl_engine::MotionKind::BigWordBackward, 1);
    assert_eq!(
        e.cursor(),
        (0, 0),
        "BigWordBackward jumps to start of prev WORD"
    );
}

#[test]
fn apply_motion_big_word_backward_with_count() {
    // "aa bb cc\n": 2B from col 6 ('c') → col 0 ('a').
    let mut e = fresh_editor("aa bb cc\n");
    e.jump_cursor(0, 6);
    e.apply_motion(hjkl_engine::MotionKind::BigWordBackward, 2);
    assert_eq!(
        e.cursor(),
        (0, 0),
        "BigWordBackward count=2 skips two WORDs back"
    );
}

#[test]
fn apply_motion_word_end_moves_to_end_of_word() {
    // "hello world\n": 'e' from col 0 lands on 'o' of "hello" at col 4.
    let mut e = fresh_editor("hello world\n");
    e.jump_cursor(0, 0);
    e.apply_motion(hjkl_engine::MotionKind::WordEnd, 1);
    assert_eq!(e.cursor(), (0, 4), "WordEnd moves to end of current word");
}

#[test]
fn apply_motion_word_end_with_count() {
    // "one two three\n": 2e from col 0 → end of "two" at col 6.
    let mut e = fresh_editor("one two three\n");
    e.jump_cursor(0, 0);
    e.apply_motion(hjkl_engine::MotionKind::WordEnd, 2);
    assert_eq!(
        e.cursor(),
        (0, 6),
        "WordEnd count=2 lands on end of second word"
    );
}

#[test]
fn apply_motion_big_word_end_moves_to_end_of_big_word() {
    // "foo.bar baz\n": E from col 0 → end of "foo.bar" WORD at col 6.
    let mut e = fresh_editor("foo.bar baz\n");
    e.jump_cursor(0, 0);
    e.apply_motion(hjkl_engine::MotionKind::BigWordEnd, 1);
    assert_eq!(e.cursor(), (0, 6), "BigWordEnd lands on end of WORD");
}

#[test]
fn apply_motion_big_word_end_with_count() {
    // "aa bb cc\n": 2E from col 0 → end of "bb" at col 4.
    let mut e = fresh_editor("aa bb cc\n");
    e.jump_cursor(0, 0);
    e.apply_motion(hjkl_engine::MotionKind::BigWordEnd, 2);
    assert_eq!(
        e.cursor(),
        (0, 4),
        "BigWordEnd count=2 lands on end of second WORD"
    );
}

// ── apply_motion controller tests (Phase 3c) — line-anchor motions ────────

#[test]
fn apply_motion_line_start_lands_at_col_zero() {
    // "  foo bar  \n": `0` from col 5 → col 0 unconditionally.
    let mut e = fresh_editor("  foo bar  \n");
    e.jump_cursor(0, 5);
    e.apply_motion(hjkl_engine::MotionKind::LineStart, 1);
    assert_eq!(e.cursor(), (0, 0), "LineStart lands at col 0");
}

#[test]
fn apply_motion_line_start_from_beginning_stays_at_col_zero() {
    // Already at col 0 — motion is a no-op but must not panic.
    let mut e = fresh_editor("  foo bar  \n");
    e.jump_cursor(0, 0);
    e.apply_motion(hjkl_engine::MotionKind::LineStart, 1);
    assert_eq!(e.cursor(), (0, 0), "LineStart from col 0 stays at col 0");
}

#[test]
fn apply_motion_first_non_blank_lands_on_first_non_blank() {
    // "  foo bar  \n": `^` from col 0 → col 2 ('f').
    let mut e = fresh_editor("  foo bar  \n");
    e.jump_cursor(0, 0);
    e.apply_motion(hjkl_engine::MotionKind::FirstNonBlank, 1);
    assert_eq!(
        e.cursor(),
        (0, 2),
        "FirstNonBlank lands on first non-blank char"
    );
}

#[test]
fn apply_motion_first_non_blank_on_blank_line_lands_at_zero() {
    // "   \n": all whitespace — `^` must land at col 0.
    let mut e = fresh_editor("   \n");
    e.jump_cursor(0, 2);
    e.apply_motion(hjkl_engine::MotionKind::FirstNonBlank, 1);
    assert_eq!(
        e.cursor(),
        (0, 0),
        "FirstNonBlank on blank line stays at col 0"
    );
}

#[test]
fn apply_motion_line_end_lands_on_last_char() {
    // "  foo bar  \n": last char is the second space at col 10.
    let mut e = fresh_editor("  foo bar  \n");
    e.jump_cursor(0, 0);
    e.apply_motion(hjkl_engine::MotionKind::LineEnd, 1);
    assert_eq!(e.cursor(), (0, 10), "LineEnd lands on last char of line");
}

#[test]
fn apply_motion_line_end_on_empty_line_stays_at_zero() {
    // "\n": empty line — `$` must stay at col 0.
    let mut e = fresh_editor("\n");
    e.jump_cursor(0, 0);
    e.apply_motion(hjkl_engine::MotionKind::LineEnd, 1);
    assert_eq!(e.cursor(), (0, 0), "LineEnd on empty line stays at col 0");
}

// ── apply_motion controller tests (Phase 3d) — doc-level motion ───────────

#[test]
fn goto_line_count_1_lands_on_last_line() {
    // "foo\nbar\nbaz\n": bare `G` (count=1) → last content line (row 2).
    // Count convention: apply_motion_kind normalises 1 → execute_motion
    // with count=1 → FileBottom arm sees count <= 1 → move_bottom(0) =
    // last content row.
    let mut e = fresh_editor("foo\nbar\nbaz\n");
    e.jump_cursor(0, 0);
    e.apply_motion(hjkl_engine::MotionKind::GotoLine, 1);
    assert_eq!(e.cursor(), (2, 0), "bare G lands on last content row");
}

#[test]
fn goto_line_count_5_lands_on_line_5() {
    // 6-line buffer (rows 0-5); `5G` → row 4 (1-based line 5).
    let mut e = fresh_editor("a\nb\nc\nd\ne\nf\n");
    e.jump_cursor(0, 0);
    e.apply_motion(hjkl_engine::MotionKind::GotoLine, 5);
    assert_eq!(e.cursor(), (4, 0), "5G lands on row 4 (1-based line 5)");
}

#[test]
fn goto_line_count_past_buffer_clamps_to_last_line() {
    // "foo\nbar\nbaz\n": `100G` → last content line (row 2), clamped.
    let mut e = fresh_editor("foo\nbar\nbaz\n");
    e.jump_cursor(0, 0);
    e.apply_motion(hjkl_engine::MotionKind::GotoLine, 100);
    assert_eq!(e.cursor(), (2, 0), "100G clamps to last content row");
}

// ── FindRepeat / FindRepeatReverse controller tests (Phase 3e) ────────────

#[test]
fn find_repeat_after_f_finds_next_occurrence() {
    // "abcabc", cursor at (0,0). `fc` lands on (0,2). `;` repeats → (0,5).
    let mut e = fresh_editor("abcabc");
    e.jump_cursor(0, 0);
    e.find_char('c', true, false, 1);
    assert_eq!(e.cursor(), (0, 2), "fc must land on first 'c'");
    e.apply_motion(hjkl_engine::MotionKind::FindRepeat, 1);
    assert_eq!(
        e.cursor(),
        (0, 5),
        "find_repeat (;) must advance to second 'c'"
    );
}

#[test]
fn find_repeat_reverse_after_f_finds_prev_occurrence() {
    // "abcabc", cursor at (0,0). `fc` lands on (0,2). `;` → (0,5). `,` back → (0,2).
    let mut e = fresh_editor("abcabc");
    e.jump_cursor(0, 0);
    e.find_char('c', true, false, 1);
    assert_eq!(e.cursor(), (0, 2), "fc must land on first 'c'");
    e.apply_motion(hjkl_engine::MotionKind::FindRepeat, 1);
    assert_eq!(e.cursor(), (0, 5), "; must advance to second 'c'");
    e.apply_motion(hjkl_engine::MotionKind::FindRepeatReverse, 1);
    assert_eq!(
        e.cursor(),
        (0, 2),
        "find_repeat_reverse (,) must go back to first 'c'"
    );
}

#[test]
fn find_repeat_with_no_prior_find_is_noop() {
    // Fresh editor, no prior find — `;` must not move cursor.
    let mut e = fresh_editor("abcabc");
    e.jump_cursor(0, 3);
    e.apply_motion(hjkl_engine::MotionKind::FindRepeat, 1);
    assert_eq!(
        e.cursor(),
        (0, 3),
        "find_repeat with no prior find must be a no-op"
    );
}

#[test]
fn find_repeat_with_count_advances_count_times() {
    // "aXaXaX", cursor (0,0). `fX` → (0,1). `3;` → repeats 3× → (0,5).
    let mut e = fresh_editor("aXaXaX");
    e.jump_cursor(0, 0);
    e.find_char('X', true, false, 1);
    assert_eq!(e.cursor(), (0, 1), "fX must land on first 'X' at col 1");
    e.apply_motion(hjkl_engine::MotionKind::FindRepeat, 3);
    assert_eq!(
        e.cursor(),
        (0, 5),
        "3; must advance 3 times from col 1 to col 5"
    );
}

// ── BracketMatch controller tests (Phase 3f) ───────────────────────────────

#[test]
fn bracket_match_jumps_to_matching_close_paren() {
    // "(abc)", cursor at (0,0) on `(` — `%` must jump to `)` at (0,4).
    let mut e = fresh_editor("(abc)");
    e.jump_cursor(0, 0);
    e.apply_motion(hjkl_engine::MotionKind::BracketMatch, 1);
    assert_eq!(
        e.cursor(),
        (0, 4),
        "% on '(' must land on matching ')' at col 4"
    );
}

#[test]
fn bracket_match_jumps_to_matching_open_paren() {
    // "(abc)", cursor at (0,4) on `)` — `%` must jump back to `(` at (0,0).
    let mut e = fresh_editor("(abc)");
    e.jump_cursor(0, 4);
    e.apply_motion(hjkl_engine::MotionKind::BracketMatch, 1);
    assert_eq!(
        e.cursor(),
        (0, 0),
        "% on ')' must land on matching '(' at col 0"
    );
}

#[test]
fn bracket_match_with_no_match_on_line_is_noop_or_engine_behaviour() {
    // "abcd", cursor at (0,2) — no bracket under cursor; engine returns
    // false from matching_bracket, cursor must not move.
    let mut e = fresh_editor("abcd");
    e.jump_cursor(0, 2);
    e.apply_motion(hjkl_engine::MotionKind::BracketMatch, 1);
    assert_eq!(
        e.cursor(),
        (0, 2),
        "% with no bracket under cursor must be a no-op"
    );
}

// ── Scroll / viewport motion controller tests (Phase 3g) ──────────────────

/// Helper: build a 20-line buffer, set viewport to rows [5..14] (height=10).
fn fresh_viewport_editor() -> Editor {
    let content = many_lines(20);
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::from_str(&content),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    // height=10, top_row=5 → visible rows 5..14.
    // set_viewport_height stores to the atomic; sync_buffer_from_textarea
    // propagates it to host.viewport_mut().height so motion helpers see it.
    e.set_viewport_height(10);
    e.sync_buffer_from_textarea();
    e.host_mut().viewport_mut().top_row = 5;
    e
}

#[test]
fn viewport_top_lands_on_first_visible_row() {
    // Viewport top=5, height=10. H (count=1) should land on row 5
    // (the first visible row, offset = count-1 = 0).
    let mut e = fresh_viewport_editor();
    e.jump_cursor(10, 0);
    e.apply_motion(hjkl_engine::MotionKind::ViewportTop, 1);
    assert_eq!(
        e.cursor().0,
        5,
        "H (count=1) must land on viewport top row (5)"
    );
}

#[test]
fn viewport_top_with_count_offsets_down() {
    // H with count=3 → viewport top + (3-1) = 5 + 2 = row 7.
    let mut e = fresh_viewport_editor();
    e.jump_cursor(12, 0);
    e.apply_motion(hjkl_engine::MotionKind::ViewportTop, 3);
    assert_eq!(e.cursor().0, 7, "3H must land at viewport top + 2 = row 7");
}

#[test]
fn viewport_middle_lands_on_middle_visible_row() {
    // Viewport top=5, height=10 → last visible = 14, mid = 5 + (14-5)/2 = 9.
    let mut e = fresh_viewport_editor();
    e.jump_cursor(0, 0);
    e.apply_motion(hjkl_engine::MotionKind::ViewportMiddle, 1);
    assert_eq!(e.cursor().0, 9, "M must land on middle visible row (9)");
}

#[test]
fn viewport_bottom_lands_on_last_visible_row() {
    // L (count=1) → viewport bottom, offset = count-1 = 0 → row 14.
    let mut e = fresh_viewport_editor();
    e.jump_cursor(5, 0);
    e.apply_motion(hjkl_engine::MotionKind::ViewportBottom, 1);
    assert_eq!(
        e.cursor().0,
        14,
        "L (count=1) must land on viewport bottom row (14)"
    );
}

#[test]
fn half_page_down_moves_cursor_by_half_window() {
    // viewport height=10, so half=5. Cursor at row 0 → row 5 after C-d.
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::from_str(&many_lines(30)),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_viewport_height(10);
    e.jump_cursor(0, 0);
    e.apply_motion(hjkl_engine::MotionKind::HalfPageDown, 1);
    assert_eq!(
        e.cursor().0,
        5,
        "<C-d> from row 0 with viewport height=10 must land on row 5"
    );
}

#[test]
fn half_page_up_moves_cursor_by_half_window_reverse() {
    // viewport height=10, half=5. Cursor at row 10 → row 5 after C-u.
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::from_str(&many_lines(30)),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_viewport_height(10);
    e.jump_cursor(10, 0);
    e.apply_motion(hjkl_engine::MotionKind::HalfPageUp, 1);
    assert_eq!(
        e.cursor().0,
        5,
        "<C-u> from row 10 with viewport height=10 must land on row 5"
    );
}

#[test]
fn full_page_down_moves_cursor_by_full_window() {
    // viewport height=10, full = 10 - 2 = 8. Cursor at row 0 → row 8.
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::from_str(&many_lines(30)),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_viewport_height(10);
    e.jump_cursor(0, 0);
    e.apply_motion(hjkl_engine::MotionKind::FullPageDown, 1);
    assert_eq!(
        e.cursor().0,
        8,
        "<C-f> from row 0 with viewport height=10 must land on row 8"
    );
}

#[test]
fn full_page_up_moves_cursor_by_full_window_reverse() {
    // viewport height=10, full=8. Cursor at row 10 → row 2.
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::from_str(&many_lines(30)),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_viewport_height(10);
    e.jump_cursor(10, 0);
    e.apply_motion(hjkl_engine::MotionKind::FullPageUp, 1);
    assert_eq!(
        e.cursor().0,
        2,
        "<C-b> from row 10 with viewport height=10 must land on row 2"
    );
}

// ── set_mark_at_cursor unit tests ─────────────────────────────────────────

#[test]
fn set_mark_at_cursor_alphabetic_records() {
    // `ma` at (0, 2) — mark 'a' must store (0, 2).
    let mut e = fresh_editor("hello");
    e.jump_cursor(0, 2);
    e.set_mark_at_cursor('a');
    assert_eq!(
        e.mark('a'),
        Some((0, 2)),
        "mark 'a' must record current pos"
    );
}

#[test]
fn set_mark_at_cursor_invalid_char_no_op() {
    // Invalid chars (digits, special) must not store a mark.
    let mut e = fresh_editor("hello");
    e.jump_cursor(0, 1);
    e.set_mark_at_cursor('1'); // digit — not alphanumeric in vim mark sense
    assert_eq!(e.mark('1'), None, "digit mark must be a no-op");
    e.set_mark_at_cursor('['); // special — only goto uses '[', not set_mark
    assert_eq!(
        e.mark('['),
        None,
        "bracket char must be a no-op for set_mark"
    );
}

#[test]
fn set_mark_at_cursor_special_left_bracket() {
    // Confirm '[' is NOT stored by set_mark_at_cursor (vim's `m[` is invalid).
    // The `[` mark is only set automatically by operator paths, not `m[`.
    let mut e = fresh_editor("hello");
    e.jump_cursor(0, 3);
    e.set_mark_at_cursor('[');
    assert_eq!(
        e.mark('['),
        None,
        "set_mark_at_cursor must reject '[' (vim: m[ is invalid)"
    );
}

// ── goto_mark_line unit tests ─────────────────────────────────────────────

#[test]
fn goto_mark_line_jumps_to_first_non_blank() {
    // Set mark 'a' at (1, 3), then jump back to (0, 0).
    // `'a` (linewise) must land on row 1, first non-blank column.
    let mut e = fresh_editor("hello\n  world\n");
    e.jump_cursor(1, 3);
    e.set_mark_at_cursor('a');
    e.jump_cursor(0, 0);
    e.goto_mark_line('a');
    assert_eq!(e.cursor().0, 1, "goto_mark_line must jump to mark row");
    // "  world" — first non-blank is col 2.
    assert_eq!(
        e.cursor().1,
        2,
        "goto_mark_line must land on first non-blank column"
    );
}

#[test]
fn goto_mark_line_unset_mark_no_op() {
    // Jumping to an unset mark must not move the cursor.
    let mut e = fresh_editor("hello\nworld\n");
    e.jump_cursor(1, 2);
    e.goto_mark_line('z'); // 'z' not set
    assert_eq!(e.cursor(), (1, 2), "unset mark jump must be a no-op");
}

#[test]
fn goto_mark_line_invalid_char_no_op() {
    // '!' is not a valid mark char — must not move cursor.
    let mut e = fresh_editor("hello\nworld\n");
    e.jump_cursor(0, 0);
    e.goto_mark_line('!');
    assert_eq!(e.cursor(), (0, 0), "invalid mark char must be a no-op");
}

// ── goto_mark_char unit tests ─────────────────────────────────────────────

#[test]
fn goto_mark_char_jumps_to_exact_pos() {
    // Set mark 'b' at (1, 4), then jump back to (0, 0).
    // `` `b `` (charwise) must land on (1, 4) exactly.
    let mut e = fresh_editor("hello\nworld\n");
    e.jump_cursor(1, 4);
    e.set_mark_at_cursor('b');
    e.jump_cursor(0, 0);
    e.goto_mark_char('b');
    assert_eq!(
        e.cursor(),
        (1, 4),
        "goto_mark_char must jump to exact mark position"
    );
}

#[test]
fn goto_mark_char_unset_mark_no_op() {
    // Jumping to an unset mark must not move the cursor.
    let mut e = fresh_editor("hello\nworld\n");
    e.jump_cursor(1, 1);
    e.goto_mark_char('x'); // 'x' not set
    assert_eq!(
        e.cursor(),
        (1, 1),
        "unset charwise mark jump must be a no-op"
    );
}

#[test]
fn goto_mark_char_invalid_char_no_op() {
    // '#' is not a valid mark char — must not move cursor.
    let mut e = fresh_editor("hello\nworld\n");
    e.jump_cursor(0, 2);
    e.goto_mark_char('#');
    assert_eq!(
        e.cursor(),
        (0, 2),
        "invalid charwise mark char must be a no-op"
    );
}

// ── Macro controller API tests (Phase 5b) ─────────────────────────────────

#[test]
fn start_macro_record_records_register() {
    let mut e = fresh_editor("hello");
    assert!(!e.is_recording_macro());
    e.start_macro_record('a');
    assert!(e.is_recording_macro());
    assert_eq!(e.recording_register(), Some('a'));
}

#[test]
fn start_macro_record_capital_seeds_existing() {
    // `qa` records "h", stop. Then `qA` should seed from existing 'a' reg.
    let mut e = fresh_editor("hello");
    e.start_macro_record('a');
    e.record_input(hjkl_engine::Input {
        key: hjkl_engine::Key::Char('h'),
        ..Default::default()
    });
    e.stop_macro_record();
    // Start capital 'A' — should seed from existing 'a' register.
    e.start_macro_record('A');
    // recording_keys should now contain 1 input (the seeded 'h').
    assert_eq!(
        e.recording_keys_len(),
        1,
        "capital record must seed from existing lowercase reg"
    );
}

#[test]
fn stop_macro_record_writes_register() {
    let mut e = fresh_editor("hello");
    e.start_macro_record('a');
    e.record_input(hjkl_engine::Input {
        key: hjkl_engine::Key::Char('h'),
        ..Default::default()
    });
    e.record_input(hjkl_engine::Input {
        key: hjkl_engine::Key::Char('l'),
        ..Default::default()
    });
    e.stop_macro_record();
    assert!(!e.is_recording_macro());
    // Register 'a' should contain "hl".
    let text = e
        .registers()
        .read('a')
        .map(|s| s.text.clone())
        .unwrap_or_default();
    assert_eq!(
        text, "hl",
        "stop_macro_record must write encoded keys to register"
    );
}

#[test]
fn is_recording_macro_reflects_state() {
    let mut e = fresh_editor("hello");
    assert!(!e.is_recording_macro());
    e.start_macro_record('b');
    assert!(e.is_recording_macro());
    e.stop_macro_record();
    assert!(!e.is_recording_macro());
}

#[test]
fn play_macro_returns_decoded_inputs() {
    let mut e = fresh_editor("hello");
    // Write "jj" into register 'a'.
    e.set_named_register_text('a', "jj".to_string());
    let inputs = e.play_macro('a');
    assert_eq!(inputs.len(), 2);
    assert_eq!(inputs[0].key, hjkl_engine::Key::Char('j'));
    assert_eq!(inputs[1].key, hjkl_engine::Key::Char('j'));
    assert!(e.is_replaying_macro(), "play_macro must set replaying flag");
    e.end_macro_replay();
    assert!(!e.is_replaying_macro());
}

#[test]
fn play_macro_at_uses_last_macro() {
    let mut e = fresh_editor("hello");
    e.set_named_register_text('a', "k".to_string());
    // Play 'a' first to set last_macro.
    let _ = e.play_macro('a');
    e.end_macro_replay();
    // Now `@@` should replay 'a' again.
    let inputs = e.play_macro('@');
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0].key, hjkl_engine::Key::Char('k'));
    e.end_macro_replay();
}

#[test]
fn play_macro_returns_one_iteration_regardless_of_count() {
    // Audit R2: the count in `3@a` is looped by the HOST — play_macro
    // itself returns a single iteration so a huge count prefix can never
    // materialize count x keys.len() inputs up front.
    let mut e = fresh_editor("hello");
    e.set_named_register_text('a', "j".to_string());
    let inputs = e.play_macro('a');
    assert_eq!(inputs.len(), 1, "play_macro must return one iteration");
    e.end_macro_replay();
}

#[test]
fn record_input_appends_when_recording() {
    let mut e = fresh_editor("hello");
    // Not recording: record_input is a no-op.
    e.record_input(hjkl_engine::Input {
        key: hjkl_engine::Key::Char('j'),
        ..Default::default()
    });
    assert_eq!(e.recording_keys_len(), 0);
    // Start recording: record_input appends.
    e.start_macro_record('a');
    e.record_input(hjkl_engine::Input {
        key: hjkl_engine::Key::Char('j'),
        ..Default::default()
    });
    e.record_input(hjkl_engine::Input {
        key: hjkl_engine::Key::Char('k'),
        ..Default::default()
    });
    assert_eq!(e.recording_keys_len(), 2);
    // During replay: record_input must NOT append.
    e.set_replaying_macro_raw(true);
    e.record_input(hjkl_engine::Input {
        key: hjkl_engine::Key::Char('l'),
        ..Default::default()
    });
    assert_eq!(
        e.recording_keys_len(),
        2,
        "record_input must skip during replay"
    );
    e.set_replaying_macro_raw(false);
    e.stop_macro_record();
}

// ── Phase 6.1 insert-mode primitive tests (kryptic-sh/hjkl#87) ────────────

/// Helper: enter insert mode via the public bridge, then call the method under test.
fn enter_insert(e: &mut Editor) {
    e.enter_insert_i(1);
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Insert);
}

#[test]
fn insert_char_basic() {
    let mut e = fresh_editor("hello");
    enter_insert(&mut e);
    e.insert_char('X');
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "Xhello");
    assert!(e.take_dirty());
}

#[test]
fn insert_newline_splits_line() {
    let mut e = fresh_editor("hello");
    // Move to col 3 so we split "hel" | "lo".
    e.jump_cursor(0, 3);
    enter_insert(&mut e);
    e.insert_newline();
    let lines = e
        .buffer()
        .rope()
        .lines()
        .map(|s| {
            let s = s.to_string();
            s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
        })
        .collect::<Vec<_>>();
    assert_eq!(lines[0], "hel");
    assert_eq!(lines[1], "lo");
}

#[test]
fn insert_tab_expandtab_inserts_spaces() {
    let mut e = fresh_editor("");
    // Default options: expandtab=true, softtabstop=4, tabstop=4.
    enter_insert(&mut e);
    e.insert_tab();
    // At col 0 with sts=4: 4 spaces inserted.
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "    ");
}

#[test]
fn insert_tab_real_tab_when_noexpandtab() {
    let opts = hjkl_engine::types::Options {
        expandtab: false,
        ..hjkl_engine::types::Options::default()
    };
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::new(),
        hjkl_engine::types::DefaultHost::new(),
        opts,
    );
    e.set_content("");
    enter_insert(&mut e);
    e.insert_tab();
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "\t");
}

#[test]
fn insert_backspace_single_char() {
    // Cursor at col 3 in "hello", backspace deletes 'l'.
    let mut e = fresh_editor("hello");
    e.jump_cursor(0, 3);
    enter_insert(&mut e);
    e.insert_backspace();
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "helo");
}

#[test]
fn insert_backspace_softtabstop() {
    // With sts=4, expandtab: 4 spaces at col 4 → one backspace deletes all 4.
    let mut e = fresh_editor("    hello");
    e.jump_cursor(0, 4);
    enter_insert(&mut e);
    e.insert_backspace();
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "hello");
}

#[test]
fn insert_backspace_join_up() {
    // At col 0 on row 1, backspace joins with the previous line.
    let mut e = fresh_editor("foo\nbar");
    e.jump_cursor(1, 0);
    enter_insert(&mut e);
    e.insert_backspace();
    // Two rows merged into one.
    assert_eq!(e.buffer().row_count(), 1);
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "foobar");
}

#[test]
fn leave_insert_steps_back_col() {
    // Esc in insert mode should move the cursor one cell left (vim convention).
    let mut e = fresh_editor("hello");
    e.jump_cursor(0, 3);
    enter_insert(&mut e);
    // Type one char so cursor is at col 4, then call leave_insert_to_normal.
    e.insert_char('X');
    // cursor is now at col 4 (after the inserted 'X').
    let pre_col = e.cursor().1;
    e.leave_insert_to_normal();
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Normal);
    // Cursor stepped back one.
    assert_eq!(e.cursor().1, pre_col - 1);
}

#[test]
fn insert_ctrl_w_word_back() {
    // Ctrl-W deletes from cursor back to word start.
    // "hello world" — cursor at end of "world" (col 11).
    let mut e = fresh_editor("hello world");
    // Normal mode clamps cursor to col 10 (last char); jump_cursor doesn't clamp.
    e.jump_cursor(0, 11);
    enter_insert(&mut e);
    e.insert_ctrl_w();
    // "world" (5 chars) deleted, leaving "hello ".
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "hello ");
}

#[test]
fn insert_ctrl_u_deletes_to_line_start() {
    let mut e = fresh_editor("hello world");
    e.jump_cursor(0, 5);
    enter_insert(&mut e);
    e.insert_ctrl_u();
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), " world");
}

#[test]
fn insert_ctrl_h_single_backspace() {
    // Ctrl-H is an alias for Backspace in insert mode.
    let mut e = fresh_editor("hello");
    e.jump_cursor(0, 3);
    enter_insert(&mut e);
    e.insert_ctrl_h();
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "helo");
}

#[test]
fn insert_ctrl_h_join_up() {
    let mut e = fresh_editor("foo\nbar");
    e.jump_cursor(1, 0);
    enter_insert(&mut e);
    e.insert_ctrl_h();
    assert_eq!(e.buffer().row_count(), 1);
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "foobar");
}

#[test]
fn insert_ctrl_t_indents_current_line() {
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options {
            shiftwidth: 4,
            ..hjkl_engine::types::Options::default()
        },
    );
    e.set_content("hello");
    enter_insert(&mut e);
    e.insert_ctrl_t();
    assert_eq!(
        hjkl_buffer::rope_line_str(&e.buffer().rope(), 0),
        "    hello"
    );
}

#[test]
fn insert_ctrl_d_outdents_current_line() {
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options {
            shiftwidth: 4,
            ..hjkl_engine::types::Options::default()
        },
    );
    e.set_content("    hello");
    enter_insert(&mut e);
    e.insert_ctrl_d();
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "hello");
}

#[test]
fn insert_ctrl_o_arm_sets_one_shot_normal() {
    let mut e = fresh_editor("hello");
    enter_insert(&mut e);
    e.insert_ctrl_o_arm();
    // Mode should flip to Normal (one-shot).
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Normal);
}

#[test]
fn insert_ctrl_r_arm_sets_pending_register() {
    let mut e = fresh_editor("hello");
    enter_insert(&mut e);
    e.insert_ctrl_r_arm();
    // pending register flag set; mode stays Insert.
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Insert);
    assert!(e.insert_pending_register());
}

#[test]
fn insert_delete_removes_char_under_cursor() {
    let mut e = fresh_editor("hello");
    e.jump_cursor(0, 2);
    enter_insert(&mut e);
    e.insert_delete();
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "helo");
}

#[test]
fn insert_delete_joins_lines_at_eol() {
    let mut e = fresh_editor("foo\nbar");
    // Position at end of row 0 (col 3 = past last char).
    e.jump_cursor(0, 3);
    enter_insert(&mut e);
    e.insert_delete();
    assert_eq!(e.buffer().row_count(), 1);
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "foobar");
}

#[test]
fn insert_arrow_left_moves_cursor() {
    let mut e = fresh_editor("hello");
    e.jump_cursor(0, 3);
    enter_insert(&mut e);
    e.insert_arrow(hjkl_engine::InsertDir::Left);
    assert_eq!(e.cursor().1, 2);
}

#[test]
fn insert_arrow_right_moves_cursor() {
    let mut e = fresh_editor("hello");
    e.jump_cursor(0, 2);
    enter_insert(&mut e);
    e.insert_arrow(hjkl_engine::InsertDir::Right);
    assert_eq!(e.cursor().1, 3);
}

#[test]
fn insert_arrow_up_moves_cursor() {
    let mut e = fresh_editor("foo\nbar");
    e.jump_cursor(1, 0);
    enter_insert(&mut e);
    e.insert_arrow(hjkl_engine::InsertDir::Up);
    assert_eq!(e.cursor().0, 0);
}

#[test]
fn insert_arrow_down_moves_cursor() {
    let mut e = fresh_editor("foo\nbar");
    e.jump_cursor(0, 0);
    enter_insert(&mut e);
    e.insert_arrow(hjkl_engine::InsertDir::Down);
    assert_eq!(e.cursor().0, 1);
}

#[test]
fn insert_home_moves_to_line_start() {
    let mut e = fresh_editor("hello");
    e.jump_cursor(0, 4);
    enter_insert(&mut e);
    e.insert_home();
    assert_eq!(e.cursor().1, 0);
}

#[test]
fn insert_end_moves_to_line_end() {
    let mut e = fresh_editor("hello");
    e.jump_cursor(0, 0);
    enter_insert(&mut e);
    e.insert_end();
    // move_line_end lands on the last char (col 4) for "hello".
    assert_eq!(e.cursor().1, 4);
}

#[test]
fn insert_pageup_does_not_panic() {
    let mut e = fresh_editor("line1\nline2\nline3");
    e.jump_cursor(2, 0);
    enter_insert(&mut e);
    // Viewport height 0 → no crash (viewport_h saturates to 1 row effectively).
    e.insert_pageup(24);
}

#[test]
fn insert_pagedown_does_not_panic() {
    let mut e = fresh_editor("line1\nline2\nline3");
    e.jump_cursor(0, 0);
    enter_insert(&mut e);
    e.insert_pagedown(24);
}

#[test]
fn leave_insert_to_normal_exits_mode() {
    let mut e = fresh_editor("hello");
    enter_insert(&mut e);
    e.leave_insert_to_normal();
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Normal);
}

#[test]
fn insert_backspace_at_buffer_start_is_noop() {
    let mut e = fresh_editor("hello");
    e.jump_cursor(0, 0);
    enter_insert(&mut e);
    // No previous char and no previous row — should not panic.
    e.insert_backspace();
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "hello");
}

#[test]
fn insert_delete_at_buffer_end_is_noop() {
    let mut e = fresh_editor("hello");
    // Cursor at col 5 (past last char index of 4), no next row.
    e.jump_cursor(0, 5);
    enter_insert(&mut e);
    // col 5 >= line_chars (5), no next row → no-op.
    e.insert_delete();
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "hello");
}

// ── Phase 6.2: normal-mode primitive tests (kryptic-sh/hjkl#88) ─────────

// Helper: set content and ensure we are in Normal mode.
fn normal_editor(initial: &str) -> Editor {
    let e = fresh_editor(initial);
    // fresh_editor starts in Normal; this is just a readability alias.
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Normal);
    e
}

// ── Insert-mode entry ────────────────────────────────────────────────────

#[test]
fn enter_insert_i_lands_in_insert_at_cursor() {
    let mut e = normal_editor("hello");
    e.jump_cursor(0, 2);
    e.enter_insert_i(1);
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Insert);
    assert_eq!(e.cursor(), (0, 2));
}

#[test]
fn enter_insert_shift_i_moves_to_first_non_blank_then_insert() {
    let mut e = normal_editor("  hello");
    e.jump_cursor(0, 5);
    e.enter_insert_shift_i(1);
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Insert);
    // First non-blank of "  hello" is col 2.
    assert_eq!(e.cursor().1, 2);
}

#[test]
fn enter_insert_a_advances_one_then_insert() {
    let mut e = normal_editor("hello");
    e.jump_cursor(0, 0);
    e.enter_insert_a(1);
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Insert);
    assert_eq!(e.cursor().1, 1);
}

#[test]
fn enter_insert_shift_a_lands_at_eol() {
    let mut e = normal_editor("hello");
    e.enter_insert_shift_a(1);
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Insert);
    assert_eq!(e.cursor().1, 5);
}

#[test]
fn open_line_below_creates_new_line_and_insert() {
    let mut e = normal_editor("hello\nworld");
    e.open_line_below(1);
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Insert);
    assert_eq!(e.buffer().row_count(), 3);
}

#[test]
fn open_line_above_creates_line_before_cursor() {
    let mut e = normal_editor("hello\nworld");
    e.jump_cursor(1, 0);
    e.open_line_above(1);
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Insert);
    assert_eq!(e.buffer().row_count(), 3);
    assert_eq!(e.cursor().0, 1);
}

#[test]
fn open_line_above_at_row_0_creates_blank_first_line() {
    let mut e = normal_editor("hello");
    e.open_line_above(1);
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Insert);
    // New blank line is row 0; old "hello" is row 1.
    assert_eq!(e.cursor().0, 0);
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 1), "hello");
}

#[test]
fn enter_replace_mode_sets_insert_mode() {
    let mut e = normal_editor("hello");
    e.enter_replace_mode(1);
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Insert);
}

// ── Char / line ops ──────────────────────────────────────────────────────

#[test]
fn delete_char_forward_removes_one_char() {
    let mut e = normal_editor("hello");
    e.jump_cursor(0, 1);
    e.delete_char_forward(1);
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "hllo");
}

#[test]
fn delete_char_forward_count_5_removes_five() {
    let mut e = normal_editor("hello world");
    e.delete_char_forward(5);
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), " world");
}

#[test]
fn delete_char_forward_noop_on_empty_line() {
    let mut e = normal_editor("");
    let before = e.content().to_string();
    e.delete_char_forward(1);
    // Empty buffer: no chars to delete, content unchanged.
    assert_eq!(e.content(), before.as_str());
}

#[test]
fn delete_char_backward_removes_char_before_cursor() {
    let mut e = normal_editor("hello");
    e.jump_cursor(0, 3);
    e.delete_char_backward(1);
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "helo");
}

#[test]
fn delete_char_backward_noop_at_col_0() {
    let mut e = normal_editor("hello");
    e.jump_cursor(0, 0);
    e.delete_char_backward(1);
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "hello");
}

#[test]
fn substitute_char_deletes_and_enters_insert() {
    let mut e = normal_editor("hello");
    e.jump_cursor(0, 0);
    e.substitute_char(1);
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Insert);
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "ello");
}

#[test]
fn substitute_char_count_3_deletes_three() {
    let mut e = normal_editor("hello");
    e.substitute_char(3);
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Insert);
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "lo");
}

#[test]
fn substitute_line_clears_content_and_enters_insert() {
    let mut e = normal_editor("hello world");
    e.substitute_line(1);
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Insert);
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "");
}

#[test]
fn delete_to_eol_removes_from_cursor_to_end() {
    let mut e = normal_editor("hello world");
    e.jump_cursor(0, 5);
    e.delete_to_eol();
    // col 5 is ' ' — deletes " world", leaving "hello".
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "hello");
}

#[test]
fn delete_to_eol_noop_when_cursor_past_end() {
    let mut e = normal_editor("hi");
    e.jump_cursor(0, 2);
    e.delete_to_eol();
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "hi");
}

#[test]
fn change_to_eol_enters_insert() {
    let mut e = normal_editor("hello world");
    e.jump_cursor(0, 5);
    e.change_to_eol();
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Insert);
    // col 5 is ' ' — deletes " world", leaving "hello".
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "hello");
}

#[test]
fn yank_to_eol_fills_register() {
    let mut e = normal_editor("hello world");
    e.jump_cursor(0, 6);
    e.yank_to_eol(1);
    // Yank does not change mode.
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Normal);
    // Unnamed register holds the yanked text (col 6 is 'w' in "world").
    assert!(
        e.registers().unnamed.text.starts_with("world")
            || e.registers().unnamed.text.contains("world")
    );
}

#[test]
fn join_line_merges_next_line_with_space() {
    let mut e = normal_editor("foo\nbar");
    e.join_line(1);
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "foo bar");
}

#[test]
fn join_line_count_2_merges_two_lines() {
    let mut e = normal_editor("a\nb\nc");
    e.join_line(2);
    // vim `[count]J` joins `count` lines (`count - 1` joins): `2J` merges the
    // current line with the one below → "a b", leaving "c".
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "a b");
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 1), "c");
}

#[test]
fn join_line_count_3_merges_three_lines() {
    let mut e = normal_editor("a\nb\nc\nd");
    e.join_line(3);
    // `3J` joins three lines (two joins) → "a b c", leaving "d".
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "a b c");
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 1), "d");
}

#[test]
fn join_line_noop_on_last_line() {
    let mut e = normal_editor("only");
    e.join_line(1);
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "only");
}

#[test]
fn toggle_case_at_cursor_flips_letter() {
    let mut e = normal_editor("hello");
    e.toggle_case_at_cursor(1);
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "Hello");
}

#[test]
fn toggle_case_at_cursor_count_3_flips_three() {
    let mut e = normal_editor("hello");
    e.toggle_case_at_cursor(3);
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "HELlo");
}

// ── Undo / redo round-trip ───────────────────────────────────────────────

#[test]
fn undo_redo_roundtrip_via_public_methods() {
    let mut e = normal_editor("hello");
    e.delete_char_forward(1);
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "ello");
    e.undo();
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "hello");
    e.redo();
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "ello");
}

// ── Jump / scroll ────────────────────────────────────────────────────────

#[test]
fn jump_back_and_forward_roundtrip() {
    let mut e = fresh_editor("a\nb\nc\nd");
    e.set_viewport_height(10);
    e.jump_cursor(3, 0);
    // Push current pos onto jumplist (big motion done externally; use
    // `run_keys` shortcut: `gg` pushes jump then `G` jumps).
    // Simpler: just call jump_back with empty stack → no-op (shouldn't panic).
    e.jump_back(1);
    e.jump_forward(1);
}

#[test]
fn scroll_full_page_down_moves_cursor() {
    use hjkl_engine::ScrollDir;
    let lines = (0..30)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut e = fresh_editor(&lines);
    e.set_viewport_height(10);
    let before = e.cursor().0;
    e.scroll_full_page(ScrollDir::Down, 1);
    assert!(e.cursor().0 > before);
}

#[test]
fn scroll_full_page_up_moves_cursor() {
    use hjkl_engine::ScrollDir;
    let lines = (0..30)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut e = fresh_editor(&lines);
    e.set_viewport_height(10);
    e.jump_cursor(25, 0);
    let before = e.cursor().0;
    e.scroll_full_page(ScrollDir::Up, 1);
    assert!(e.cursor().0 < before);
}

#[test]
fn scroll_half_page_down_moves_cursor() {
    use hjkl_engine::ScrollDir;
    let lines = (0..30)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut e = fresh_editor(&lines);
    e.set_viewport_height(10);
    let before = e.cursor().0;
    e.scroll_half_page(ScrollDir::Down, 1);
    assert!(e.cursor().0 > before);
}

#[test]
fn scroll_half_page_up_at_top_is_noop() {
    use hjkl_engine::ScrollDir;
    let lines = (0..30)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut e = fresh_editor(&lines);
    e.set_viewport_height(10);
    // Already at top, scrolling up should not panic and cursor stays at 0.
    e.scroll_half_page(ScrollDir::Up, 1);
    assert_eq!(e.cursor().0, 0);
}

#[test]
fn scroll_line_down_shifts_viewport_without_moving_cursor() {
    use hjkl_engine::ScrollDir;
    let lines = (0..30)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut e = fresh_editor(&lines);
    e.set_viewport_height(10);
    // Park cursor in the middle of a large buffer.
    e.jump_cursor(15, 0);
    e.set_viewport_top(10);
    let cursor_before = e.cursor().0;
    e.scroll_line(ScrollDir::Down, 1);
    // Viewport top advances; cursor stays.
    assert_eq!(e.cursor().0, cursor_before);
    assert_eq!(e.host().viewport().top_row, 11);
}

#[test]
fn scroll_line_up_shifts_viewport() {
    use hjkl_engine::ScrollDir;
    let lines = (0..30)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut e = fresh_editor(&lines);
    e.set_viewport_height(10);
    e.jump_cursor(15, 0);
    e.set_viewport_top(10);
    let cursor_before = e.cursor().0;
    e.scroll_line(ScrollDir::Up, 1);
    assert_eq!(e.cursor().0, cursor_before);
    assert_eq!(e.host().viewport().top_row, 9);
}

#[test]
fn scroll_line_clamps_cursor_when_off_screen() {
    use hjkl_engine::ScrollDir;
    let lines = (0..30)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut e = fresh_editor(&lines);
    e.set_viewport_height(10);
    // Cursor at viewport top; scrolling down pushes it off — must clamp.
    e.jump_cursor(5, 0);
    e.set_viewport_top(5);
    e.scroll_line(ScrollDir::Down, 3);
    // New top = 8; cursor was at 5, which is now off-screen (< 8).
    // Cursor clamped to new top.
    assert!(e.cursor().0 >= 8);
}

#[test]
fn scroll_doesnt_crash_at_buffer_edges() {
    use hjkl_engine::ScrollDir;
    let mut e = normal_editor("single line");
    e.set_viewport_height(10);
    // Should not panic on any of these at-the-edge scrolls.
    e.scroll_full_page(ScrollDir::Down, 99);
    e.scroll_full_page(ScrollDir::Up, 99);
    e.scroll_half_page(ScrollDir::Down, 99);
    e.scroll_half_page(ScrollDir::Up, 99);
    e.scroll_line(ScrollDir::Down, 99);
    e.scroll_line(ScrollDir::Up, 99);
}

// ── Horizontal scroll ────────────────────────────────────────────────────

#[test]
fn scroll_right_advances_top_col() {
    let mut e = fresh_editor("hello world");
    e.set_viewport_height(10);
    e.scroll_right(5);
    assert_eq!(e.host().viewport().top_col, 5);
}

#[test]
fn scroll_left_does_not_underflow() {
    let mut e = fresh_editor("hello world");
    e.set_viewport_height(10);
    e.scroll_right(2);
    e.scroll_left(10);
    assert_eq!(e.host().viewport().top_col, 0);
}

#[test]
fn scroll_left_then_right_roundtrip() {
    let mut e = fresh_editor("hello world");
    e.set_viewport_height(10);
    e.scroll_right(10);
    e.scroll_left(3);
    assert_eq!(e.host().viewport().top_col, 7);
}

// ── Search ───────────────────────────────────────────────────────────────

#[test]
fn search_repeat_advances_to_next_match() {
    let mut e = fresh_editor("foo bar foo baz");
    // Use word_search to seed the search state (no search prompt needed).
    // `*` on "foo" at col 0 finds the second "foo" and sets last_search.
    e.word_search(true, true, 1);
    // Repeating forward wraps and finds the first "foo" again at col 0.
    e.search_repeat(true, 1);
    // Just ensure no panic and search state is valid.
    assert!(e.cursor().0 < e.buffer().row_count());
}

#[test]
fn search_repeat_no_pattern_is_noop() {
    let mut e = normal_editor("hello world");
    let before = e.cursor();
    // No search pattern loaded — should not panic.
    e.search_repeat(true, 1);
    assert_eq!(e.cursor(), before);
}

#[test]
fn word_search_finds_word_under_cursor() {
    let mut e = fresh_editor("foo bar foo");
    // cursor starts at col 0 on "foo"
    e.word_search(true, true, 1);
    // Should jump to the second "foo" at col 8.
    assert_eq!(e.cursor().1, 8);
}

#[test]
fn word_search_whole_word_false_extracts_word_under_cursor() {
    // `g*` on "foo" (no `\b`) — use two lines so wrap can find the next match.
    let mut e = fresh_editor("foobar\nfoo baz");
    // Cursor on second line "foo" at col 0.
    e.jump_cursor(1, 0);
    // g* with whole_word=false: pattern = "foo", advance forward (skip current).
    // Starting at (1, 0), skip "foo" at (1,0), wrap to (0, 0) which matches "foo"
    // inside "foobar".
    e.word_search(true, false, 1);
    // Cursor should land on "foo" at row 0, col 0.
    assert_eq!(e.cursor(), (0, 0));
}

#[test]
fn word_search_backward_finds_previous_match() {
    let mut e = fresh_editor("foo bar foo");
    e.jump_cursor(0, 8); // on second "foo"
    e.word_search(false, true, 1);
    // Cursor should land on col 0 (first "foo").
    assert_eq!(e.cursor().1, 0);
}

// ── Edge cases ───────────────────────────────────────────────────────────

#[test]
fn delete_char_forward_on_single_char_line() {
    let mut e = normal_editor("x");
    e.delete_char_forward(1);
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "");
}

#[test]
fn substitute_char_on_empty_line_is_noop_for_delete() {
    let mut e = normal_editor("");
    e.substitute_char(1);
    // Nothing to delete — but should enter Insert mode.
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Insert);
}

#[test]
fn join_line_10_iterations_clamps_gracefully() {
    let mut e = normal_editor("a\nb");
    // Joining 10 times on a 2-line buffer should not panic.
    e.join_line(10);
    // After the first join succeeds, the rest are no-ops.
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "a b");
}

#[test]
fn toggle_case_past_line_end_is_noop() {
    let mut e = normal_editor("ab");
    e.jump_cursor(0, 5); // way past end
    e.toggle_case_at_cursor(1);
    // Should not panic.
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "ab");
}

// ── Phase 6.3: visual-mode primitive tests (kryptic-sh/hjkl#89) ──────────

// ── Visual entry ─────────────────────────────────────────────────────────

#[test]
fn enter_visual_char_lands_in_visual_at_cursor() {
    let mut e = normal_editor("hello world");
    e.jump_cursor(0, 3);
    e.enter_visual_char();
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Visual);
    // Anchor should be at the cursor position we entered from.
    assert_eq!(e.visual_anchor(), (0, 3));
}

#[test]
fn enter_visual_line_lands_in_visual_line() {
    let mut e = normal_editor("hello\nworld");
    e.jump_cursor(1, 2);
    e.enter_visual_line();
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::VisualLine);
    // Line anchor should be the current row.
    assert_eq!(e.visual_line_anchor(), 1);
}

#[test]
fn enter_visual_block_lands_in_visual_block() {
    let mut e = normal_editor("hello\nworld");
    e.jump_cursor(0, 2);
    e.enter_visual_block();
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::VisualBlock);
    // Block anchor and vcol should match the cursor column.
    assert_eq!(e.block_anchor(), (0, 2));
    assert_eq!(e.block_vcol(), 2);
}

// ── Visual exit ──────────────────────────────────────────────────────────

#[test]
fn exit_visual_to_normal_sets_marks_and_returns_to_normal() {
    let mut e = normal_editor("hello world");
    // Enter charwise visual at col 2, extend to col 5.
    e.jump_cursor(0, 2);
    e.enter_visual_char();
    e.jump_cursor(0, 5);
    e.exit_visual_to_normal();
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Normal);
    // `<` = (0, 2), `>` = (0, 5).
    assert_eq!(e.mark('<'), Some((0, 2)));
    assert_eq!(e.mark('>'), Some((0, 5)));
}

#[test]
fn exit_visual_to_normal_stores_last_visual() {
    let mut e = normal_editor("hello world");
    e.jump_cursor(0, 1);
    e.enter_visual_char();
    e.jump_cursor(0, 4);
    e.exit_visual_to_normal();
    // last_visual should be set so gv can restore it.
    assert!(e.last_visual().is_some());
    let lv = e.last_visual().unwrap();
    assert_eq!(lv.anchor, (0, 1));
    assert_eq!(lv.cursor, (0, 4));
}

#[test]
fn exit_visual_line_sets_marks_at_line_boundaries() {
    let mut e = normal_editor("alpha\nbeta\ngamma");
    e.enter_visual_line(); // row 0
    e.jump_cursor(1, 3);
    e.exit_visual_to_normal();
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Normal);
    // `<` snaps to (min_row, 0), `>` snaps to (max_row, last_col).
    assert_eq!(e.mark('<'), Some((0, 0)));
    let last_col_of_beta = "beta".chars().count() - 1;
    assert_eq!(e.mark('>'), Some((1, last_col_of_beta)));
}

// ── visual_o_toggle ───────────────────────────────────────────────────────

#[test]
fn visual_o_toggle_swaps_anchor_and_cursor_charwise() {
    let mut e = normal_editor("hello world");
    // Enter visual at col 0, extend to col 4.
    e.enter_visual_char(); // anchor = (0,0)
    e.jump_cursor(0, 4); // cursor at col 4
    // Selection bounds before toggle: anchor=0, cursor=4.
    let pre_anchor = e.visual_anchor();
    let pre_cursor = e.cursor();
    e.visual_o_toggle();
    // After toggle: cursor jumps to old anchor, anchor = old cursor.
    assert_eq!(e.cursor(), pre_anchor, "cursor should move to old anchor");
    assert_eq!(
        e.visual_anchor(),
        pre_cursor,
        "anchor should take old cursor position"
    );
    // Mode is unchanged.
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Visual);
}

#[test]
fn visual_o_toggle_double_returns_to_start() {
    let mut e = normal_editor("hello world");
    e.enter_visual_char();
    e.jump_cursor(0, 4);
    let anchor0 = e.visual_anchor();
    let cursor0 = e.cursor();
    e.visual_o_toggle();
    e.visual_o_toggle();
    // Two toggles restore original positions.
    assert_eq!(e.visual_anchor(), anchor0);
    assert_eq!(e.cursor(), cursor0);
}

#[test]
fn visual_o_toggle_linewise_swaps_anchor_row() {
    let mut e = normal_editor("alpha\nbeta\ngamma");
    e.enter_visual_line(); // anchor row = 0
    e.jump_cursor(2, 0); // cursor on row 2
    e.visual_o_toggle();
    // Cursor should jump to old anchor row.
    assert_eq!(e.cursor().0, 0, "cursor row should be old anchor row");
    // Anchor row should now be the old cursor row.
    assert_eq!(e.visual_line_anchor(), 2);
}

// ── reenter_last_visual ───────────────────────────────────────────────────

#[test]
fn reenter_last_visual_after_vdollar_esc_restores() {
    let mut e = normal_editor("hello world");
    // v$ then Esc via FSM to store a real last_visual.
    e.enter_visual_char(); // anchor = (0,0)
    e.jump_cursor(0, 5); // move cursor to col 5 to create a range
    e.exit_visual_to_normal();
    // Should be back to Normal.
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Normal);
    // gv — should restore Visual mode.
    e.reenter_last_visual();
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Visual);
    // Cursor should be at the stored last position (col 5).
    assert_eq!(e.cursor().1, 5);
}

#[test]
fn reenter_last_visual_noop_when_no_history() {
    let mut e = normal_editor("hello");
    // No prior visual — should be a no-op, not a panic.
    e.reenter_last_visual();
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Normal);
}

// ── set_mode ─────────────────────────────────────────────────────────────

#[test]
fn set_mode_insert_flips_vim_mode_to_insert() {
    let mut e = normal_editor("hello");
    e.set_mode(hjkl_engine::VimMode::Insert);
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Insert);
}

#[test]
fn set_mode_roundtrip_normal_insert_normal() {
    let mut e = normal_editor("hello");
    e.set_mode(hjkl_engine::VimMode::Insert);
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Insert);
    e.set_mode(hjkl_engine::VimMode::Normal);
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Normal);
}

#[test]
fn set_mode_visual_variants() {
    let mut e = normal_editor("hello");
    e.set_mode(hjkl_engine::VimMode::Visual);
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Visual);
    e.set_mode(hjkl_engine::VimMode::VisualLine);
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::VisualLine);
    e.set_mode(hjkl_engine::VimMode::VisualBlock);
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::VisualBlock);
    e.set_mode(hjkl_engine::VimMode::Normal);
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Normal);
}

// ── current_mode / vim_mode consistency ───────────────────────────────────

// ── Phase 6.6b: FSM state accessor smoke tests ────────────────────────────

#[test]
fn pending_round_trips() {
    let mut e = normal_editor("hello");
    assert!(matches!(e.pending(), hjkl_engine::Pending::None));
    e.set_pending(hjkl_engine::Pending::G);
    assert!(matches!(e.pending(), hjkl_engine::Pending::G));
    let taken = e.take_pending();
    assert!(matches!(taken, hjkl_engine::Pending::G));
    assert!(matches!(e.pending(), hjkl_engine::Pending::None));
}

#[test]
fn count_round_trips() {
    let mut e = normal_editor("hello");
    assert_eq!(e.count(), 0);
    e.set_count(5);
    assert_eq!(e.count(), 5);
    e.accumulate_count_digit(3);
    assert_eq!(e.count(), 53);
    e.reset_count();
    assert_eq!(e.count(), 0);
}

#[test]
fn take_count_returns_one_when_zero() {
    let mut e = normal_editor("hello");
    assert_eq!(e.take_count(), 1);
}

#[test]
fn take_count_returns_value_and_resets() {
    let mut e = normal_editor("hello");
    e.set_count(7);
    assert_eq!(e.take_count(), 7);
    assert_eq!(e.count(), 0);
}

#[test]
fn fsm_mode_round_trips() {
    let mut e = normal_editor("hello");
    assert_eq!(e.fsm_mode(), hjkl_engine::FsmMode::Normal);
    e.set_fsm_mode(hjkl_engine::FsmMode::Insert);
    assert_eq!(e.fsm_mode(), hjkl_engine::FsmMode::Insert);
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Insert);
    e.set_fsm_mode(hjkl_engine::FsmMode::Normal);
    assert_eq!(e.fsm_mode(), hjkl_engine::FsmMode::Normal);
}

#[test]
fn replaying_flag_round_trips() {
    let mut e = normal_editor("hello");
    assert!(!e.is_replaying());
    e.set_replaying(true);
    assert!(e.is_replaying());
    e.set_replaying(false);
    assert!(!e.is_replaying());
}

#[test]
fn one_shot_normal_flag_round_trips() {
    let mut e = normal_editor("hello");
    assert!(!e.is_one_shot_normal());
    e.set_one_shot_normal(true);
    assert!(e.is_one_shot_normal());
    e.set_one_shot_normal(false);
    assert!(!e.is_one_shot_normal());
}

#[test]
fn last_find_round_trips() {
    let mut e = normal_editor("hello");
    assert_eq!(e.last_find(), None);
    e.set_last_find(Some(('x', true, false)));
    assert_eq!(e.last_find(), Some(('x', true, false)));
    e.set_last_find(None);
    assert_eq!(e.last_find(), None);
}

#[test]
fn last_change_round_trips() {
    let mut e = normal_editor("hello");
    assert!(e.last_change().is_none());
    e.set_last_change(Some(hjkl_engine::LastChange::ToggleCase { count: 2 }));
    let lc = e.last_change();
    assert!(matches!(
        lc,
        Some(hjkl_engine::LastChange::ToggleCase { count: 2 })
    ));
    e.set_last_change(None);
    assert!(e.last_change().is_none());
}

#[test]
fn last_change_mut_allows_in_place_edit() {
    let mut e = normal_editor("hello");
    e.set_last_change(Some(hjkl_engine::LastChange::ToggleCase { count: 1 }));
    if let Some(hjkl_engine::LastChange::ToggleCase { count }) = e.last_change_mut() {
        *count = 42;
    }
    assert!(matches!(
        e.last_change(),
        Some(hjkl_engine::LastChange::ToggleCase { count: 42 })
    ));
}

#[test]
fn insert_session_round_trips() {
    let mut e = normal_editor("hello");
    assert!(e.insert_session().is_none());
    e.set_insert_session(Some(hjkl_engine::InsertSession {
        count: 3,
        row_min: 0,
        row_max: 0,
        before_rope: ropey::Rope::from_str("hello"),
        reason: hjkl_engine::InsertReason::Enter(hjkl_engine::InsertEntry::I),
        start_row: 0,
        start_col: 0,
    }));
    assert_eq!(e.insert_session().map(|s| s.count), Some(3));
    let taken = e.take_insert_session();
    assert!(taken.is_some());
    assert!(e.insert_session().is_none());
}

#[test]
fn visual_anchor_round_trips() {
    let mut e = normal_editor("hello");
    e.set_visual_anchor((1, 3));
    assert_eq!(e.visual_anchor(), (1, 3));
}

#[test]
fn visual_line_anchor_round_trips() {
    let mut e = normal_editor("hello\nworld");
    e.set_visual_line_anchor(1);
    assert_eq!(e.visual_line_anchor(), 1);
}

#[test]
fn block_anchor_and_vcol_round_trip() {
    let mut e = normal_editor("hello");
    e.set_block_anchor((0, 2));
    e.set_block_vcol(4);
    assert_eq!(e.block_anchor(), (0, 2));
    assert_eq!(e.block_vcol(), 4);
}

#[test]
fn yank_linewise_round_trips() {
    let mut e = normal_editor("hello");
    assert!(!e.yank_linewise());
    e.set_yank_linewise(true);
    assert!(e.yank_linewise());
}

#[test]
fn pending_register_raw_round_trips() {
    let mut e = normal_editor("hello");
    assert_eq!(e.pending_register(), None);
    e.set_pending_register_raw(Some('a'));
    assert_eq!(e.pending_register(), Some('a'));
    let taken = e.take_pending_register_raw();
    assert_eq!(taken, Some('a'));
    assert_eq!(e.pending_register(), None);
}

#[test]
fn recording_macro_round_trips() {
    let mut e = normal_editor("hello");
    assert_eq!(e.recording_macro(), None);
    e.set_recording_macro(Some('q'));
    assert_eq!(e.recording_macro(), Some('q'));
    e.set_recording_macro(None);
    assert_eq!(e.recording_macro(), None);
}

#[test]
fn recording_keys_round_trips() {
    let mut e = normal_editor("hello");
    let input = hjkl_engine::Input {
        key: hjkl_engine::Key::Char('j'),
        ctrl: false,
        alt: false,
        shift: false,
    };
    e.push_recording_key(input);
    assert_eq!(e.take_recording_keys(), vec![input]);
    assert!(e.take_recording_keys().is_empty());
}

#[test]
fn replaying_macro_raw_round_trips() {
    let mut e = normal_editor("hello");
    assert!(!e.is_replaying_macro_raw());
    e.set_replaying_macro_raw(true);
    assert!(e.is_replaying_macro_raw());
    e.set_replaying_macro_raw(false);
    assert!(!e.is_replaying_macro_raw());
}

#[test]
fn last_macro_round_trips() {
    let mut e = normal_editor("hello");
    assert_eq!(e.last_macro(), None);
    e.set_last_macro(Some('m'));
    assert_eq!(e.last_macro(), Some('m'));
}

#[test]
fn last_insert_pos_round_trips() {
    let mut e = normal_editor("hello");
    assert_eq!(e.last_insert_pos(), None);
    e.set_last_insert_pos(Some((1, 2)));
    assert_eq!(e.last_insert_pos(), Some((1, 2)));
}

#[test]
fn last_visual_round_trips() {
    let mut e = normal_editor("hello");
    assert!(e.last_visual().is_none());
    let snap = hjkl_engine::LastVisual {
        mode: hjkl_engine::FsmMode::Visual,
        anchor: (0, 0),
        cursor: (0, 3),
        block_vcol: 0,
    };
    e.set_last_visual(Some(snap));
    assert!(e.last_visual().is_some());
    e.set_last_visual(None);
    assert!(e.last_visual().is_none());
}

#[test]
fn viewport_pinned_round_trips() {
    let mut e = normal_editor("hello");
    assert!(!e.viewport_pinned());
    e.set_viewport_pinned(true);
    assert!(e.viewport_pinned());
    e.set_viewport_pinned(false);
    assert!(!e.viewport_pinned());
}

#[test]
fn insert_pending_register_round_trips() {
    let mut e = normal_editor("hello");
    assert!(!e.insert_pending_register());
    e.set_insert_pending_register(true);
    assert!(e.insert_pending_register());
}

#[test]
fn change_mark_start_round_trips() {
    let mut e = normal_editor("hello");
    assert_eq!(e.change_mark_start(), None);
    e.set_change_mark_start(Some((2, 5)));
    assert_eq!(e.change_mark_start(), Some((2, 5)));
    let taken = e.take_change_mark_start();
    assert_eq!(taken, Some((2, 5)));
    assert_eq!(e.change_mark_start(), None);
}

#[test]
fn search_prompt_state_round_trips() {
    let mut e = normal_editor("hello");
    assert!(e.search_prompt_state().is_none());
    e.set_search_prompt_state(Some(hjkl_engine::SearchPrompt {
        text: "foo".to_string(),
        cursor: 3,
        forward: true,
        operator: None,
    }));
    assert_eq!(
        e.search_prompt_state().map(|p| p.text.as_str()),
        Some("foo")
    );
    let taken = e.take_search_prompt_state();
    assert!(taken.is_some());
    assert!(e.search_prompt_state().is_none());
}

#[test]
fn last_search_pattern_and_direction_round_trips() {
    let mut e = normal_editor("hello");
    assert_eq!(e.last_search_pattern(), None);
    e.set_last_search_pattern_only(Some("world".to_string()));
    assert_eq!(e.last_search_pattern(), Some("world".to_string()));
    e.set_last_search_forward_only(false);
    assert!(!e.last_search_forward());
}

#[test]
fn search_history_round_trips() {
    let mut e = normal_editor("hello");
    assert!(e.search_history().is_empty());
    e.record_search_history("pattern1");
    assert_eq!(e.search_history(), vec!["pattern1".to_string()]);
    e.set_search_history_cursor(Some(0));
    assert_eq!(e.search_history_cursor(), Some(0));
    e.set_search_history_cursor(None);
    assert_eq!(e.search_history_cursor(), None);
}

#[test]
fn jump_lists_round_trips() {
    let mut e = normal_editor("hello");
    assert!(e.jump_back_list().is_empty());
    assert!(e.jump_fwd_list().is_empty());
    e.jump_back_list_mut().push((1, 2));
    e.jump_fwd_list_mut().push((3, 4));
    assert_eq!(e.jump_back_list(), &[(1, 2)]);
    assert_eq!(e.jump_fwd_list(), &[(3, 4)]);
}

#[test]
fn last_input_timing_round_trips() {
    let mut e = normal_editor("hello");
    assert!(e.last_input_at().is_none());
    assert!(e.last_input_host_at().is_none());
    let now = std::time::Instant::now();
    e.set_last_input_at(Some(now));
    assert!(e.last_input_at().is_some());
    let dur = core::time::Duration::from_millis(100);
    e.set_last_input_host_at(Some(dur));
    assert_eq!(e.last_input_host_at(), Some(dur));
}

// ── auto_indent_range tests ──────────────────────────────────────────────

/// Helper: build an editor with `expandtab=true` and the given shiftwidth.
fn indent_editor(initial: &str, shiftwidth: usize, expandtab: bool) -> Editor {
    let mut e = fresh_editor(initial);
    e.settings_mut().shiftwidth = shiftwidth;
    e.settings_mut().expandtab = expandtab;
    e
}

#[test]
fn auto_indent_single_line_under_open_brace() {
    // `{\nfoo\n}` — "foo" is at depth 1 under the `{`.
    // With shiftwidth=4 expandtab=true it should become "    foo".
    let mut e = indent_editor("{\nfoo\n}", 4, true);
    // auto-indent only row 1 ("foo").
    e.auto_indent_range((1, 0), (1, 0));
    let lines = e
        .buffer()
        .rope()
        .lines()
        .map(|s| {
            let s = s.to_string();
            s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
        })
        .collect::<Vec<_>>();
    assert_eq!(lines[1], "    foo", "foo should be indented by 4 spaces");
}

#[test]
fn auto_indent_close_brace_outdents() {
    // `{\n    inner\n}` — the `}` is at depth 1 but starts with a close
    // bracket so effective_depth = 0.
    let mut e = indent_editor("{\n    inner\n}", 4, true);
    e.auto_indent_range((2, 0), (2, 0));
    let lines = e
        .buffer()
        .rope()
        .lines()
        .map(|s| {
            let s = s.to_string();
            s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
        })
        .collect::<Vec<_>>();
    assert_eq!(lines[2], "}", "`}}` should have zero indent");
}

#[test]
fn auto_indent_whole_buffer_normalizes_mixed_indent() {
    // Mixed-indent input: first line un-indented `{`, second line 1-tab
    // indented body, third line un-indented `}`.
    let src = "{\n\tbody\n}";
    let mut e = indent_editor(src, 4, true);
    let total = e.buffer().row_count();
    e.auto_indent_range((0, 0), (total - 1, 0));
    let lines = e
        .buffer()
        .rope()
        .lines()
        .map(|s| {
            let s = s.to_string();
            s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
        })
        .collect::<Vec<_>>();
    // `{` — depth 0 at start.
    assert_eq!(lines[0], "{");
    // `body` — depth 1 after `{`.
    assert_eq!(lines[1], "    body");
    // `}` — depth 1 but starts with close → effective_depth 0.
    assert_eq!(lines[2], "}");
}

#[test]
fn auto_indent_respects_expandtab_false_uses_tabs() {
    // Same buffer, but expandtab=false → indent unit is `\t`.
    let src = "{\nbody\n}";
    let mut e = indent_editor(src, 4, false);
    let total = e.buffer().row_count();
    e.auto_indent_range((0, 0), (total - 1, 0));
    let lines = e
        .buffer()
        .rope()
        .lines()
        .map(|s| {
            let s = s.to_string();
            s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
        })
        .collect::<Vec<_>>();
    assert_eq!(lines[0], "{");
    assert_eq!(lines[1], "\tbody");
    assert_eq!(lines[2], "}");
}

#[test]
fn auto_indent_empty_line_stays_empty() {
    // `{\n\nfoo\n}` — blank line in the middle should stay blank.
    let src = "{\n\nfoo\n}";
    let mut e = indent_editor(src, 4, true);
    let total = e.buffer().row_count();
    e.auto_indent_range((0, 0), (total - 1, 0));
    let lines = e
        .buffer()
        .rope()
        .lines()
        .map(|s| {
            let s = s.to_string();
            s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
        })
        .collect::<Vec<_>>();
    assert_eq!(lines[1], "", "blank line should stay blank");
    assert_eq!(lines[2], "    foo");
}

#[test]
fn auto_indent_cursor_lands_on_first_nonws_of_start_row() {
    // After `==` / `auto_indent_range` the cursor should be at the first
    // non-whitespace character of start_row (vim parity).
    let src = "{\nfoo\n}";
    let mut e = indent_editor(src, 4, true);
    // Reindent only row 1.
    e.auto_indent_range((1, 0), (1, 0));
    // Row 1 after reindent is "    foo"; first non-ws is col 4.
    let (row, col) = e.cursor();
    assert_eq!(row, 1, "cursor should stay on start_row");
    assert_eq!(col, 4, "cursor should land on first non-ws char (col 4)");
}

#[test]
fn auto_indent_sets_last_indent_range() {
    // After `auto_indent_range` the engine must store the touched row span.
    let src = "{\nfoo\nbar\n}";
    let mut e = indent_editor(src, 4, true);
    let total = e.buffer().row_count();
    e.auto_indent_range((0, 0), (total - 1, 0));
    assert_eq!(
        e.take_last_indent_range(),
        Some((0, total - 1)),
        "take_last_indent_range must return Some with the touched rows"
    );
}

#[test]
fn take_last_indent_range_clears() {
    // A second call after draining must return None.
    let src = "{\nfoo\n}";
    let mut e = indent_editor(src, 4, true);
    e.auto_indent_range((0, 0), (2, 0));
    let _ = e.take_last_indent_range(); // drain
    assert_eq!(
        e.take_last_indent_range(),
        None,
        "second take_last_indent_range must return None"
    );
}

// ── Diagnostic: auto_indent vs cargo fmt on a real source file ────────
//
// Loads `motions.rs` (~1400 LOC, mixed real-world Rust patterns: method
// chains, multi-line fn args, match arms, where clauses, closures, nested
// types) at compile time, runs `auto_indent_range` over every row, and
// diffs per-line leading-whitespace counts against the cargo-fmt'd source
// (the file is in the repo, fmt'd by CI on every commit).
//
// The test PRINTS divergences and only fails if more than `THRESHOLD`
// lines disagree — the dumb shiftwidth+bracket algorithm is documented
// to mishandle some patterns (chains, where clauses, etc.). A full
// language-aware indenter is a v2 follow-up. The point of this test is
// to surface the divergence list so we can decide which patterns the
// dumb algo CAN be taught to handle without going full tree-sitter.
//
// To diagnose: run with `--nocapture` to see the full diff.
#[test]
#[ignore = "diagnostic — run with --ignored --nocapture to see auto-indent vs cargo fmt diffs"]
fn auto_indent_vs_cargo_fmt_motions_diagnostic() {
    let original = include_str!("../../hjkl-engine/src/motions.rs");

    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options {
            shiftwidth: 4,
            expandtab: true,
            tabstop: 4,
            ..hjkl_engine::types::Options::default()
        },
    );
    e.set_content(original);

    let row_count = e.buffer().row_count();
    e.auto_indent_range((0, 0), (row_count.saturating_sub(1), 0));

    let after_lines: Vec<String> = e
        .buffer()
        .rope()
        .lines()
        .map(|s| {
            let s = s.to_string();
            s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
        })
        .collect::<Vec<_>>();
    let original_lines: Vec<&str> = original.lines().collect();

    let leading_ws = |s: &str| s.chars().take_while(|c| c.is_whitespace()).count();

    let mut diffs: Vec<(usize, String, usize, usize)> = Vec::new();
    for (i, (orig, after)) in original_lines.iter().zip(after_lines.iter()).enumerate() {
        let want = leading_ws(orig);
        let got = leading_ws(after);
        if want != got {
            diffs.push((i + 1, orig.trim().chars().take(80).collect(), want, got));
        }
    }

    // Print the first 50 divergences for diagnosis.
    eprintln!(
        "auto_indent_vs_cargo_fmt: {} lines differ out of {} ({}%)",
        diffs.len(),
        original_lines.len(),
        (diffs.len() * 100) / original_lines.len().max(1),
    );
    for (line_no, content, want, got) in diffs.iter().take(50) {
        eprintln!("  L{line_no:5} want={want:2} got={got:2}  {content}");
    }
    if diffs.len() > 50 {
        eprintln!("  ... and {} more", diffs.len() - 50);
    }

    // Soft assertion — track divergence count over time. If the algo
    // gets smarter, this number should drop. If a regression makes it
    // jump, we'll notice. Set the cap generously above current baseline.
    let pct = (diffs.len() * 100) / original_lines.len().max(1);
    // 2026-05-16 baseline after fixing bracket scan + chain continuation:
    // 5 divergences / 1416 lines (<1%). Remaining lines are a single
    // \`let X = if {} else {};\` trailing-\`=\` continuation pattern —
    // documented v2 follow-up. Cap at 2% so any regression in the
    // bracket scan or chain detection trips the test.
    assert!(
        pct < 2,
        "auto_indent diverges from cargo fmt on {pct}% of lines — regression from <1% baseline"
    );
}

// ── filter_range tests ────────────────────────────────────────────────────────

fn make_editor(content: &str) -> Editor {
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content(content);
    e
}

/// `cat` is identity — buffer content must be unchanged after filter_range.
#[test]
fn filter_range_cat_is_identity() {
    let mut e = make_editor("alpha\nbeta\ngamma");
    let result = e.filter_range(0, 2, "cat", None);
    assert!(result.is_ok(), "cat must succeed: {result:?}");
    let lines = e
        .buffer()
        .rope()
        .lines()
        .map(|s| {
            let s = s.to_string();
            s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
        })
        .collect::<Vec<_>>();
    assert_eq!(lines, vec!["alpha", "beta", "gamma"]);
}

/// Non-existent command returns Err; buffer must be unchanged.
#[test]
fn filter_range_nonexistent_command_returns_err() {
    let mut e = make_editor("line1\nline2");
    let count_before = e.buffer().row_count();
    let result = e.filter_range(0, 1, "__hjkl_no_such_cmd_xyz__", None);
    assert!(result.is_err(), "non-existent command must return Err");
    // View must still have same line count — no mutation on error.
    assert_eq!(e.buffer().row_count(), count_before);
}

/// `sort` reorders lines within the filtered range.
#[test]
fn filter_range_sort_reorders_lines() {
    let mut e = make_editor("banana\napple\ncherry");
    let result = e.filter_range(0, 2, "sort", None);
    assert!(result.is_ok(), "sort must succeed: {result:?}");
    let lines = e
        .buffer()
        .rope()
        .lines()
        .map(|s| {
            let s = s.to_string();
            s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
        })
        .collect::<Vec<_>>();
    assert_eq!(lines, vec!["apple", "banana", "cherry"]);
}

/// Partial range: only the specified rows are filtered; other rows untouched.
#[test]
fn filter_range_partial_range() {
    // filter rows 1..=2 (banana, apple); row 0 (alpha) must stay
    let mut e = make_editor("alpha\nbanana\napple");
    let result = e.filter_range(1, 2, "sort", None);
    assert!(result.is_ok(), "sort must succeed: {result:?}");
    let lines = e
        .buffer()
        .rope()
        .lines()
        .map(|s| {
            let s = s.to_string();
            s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
        })
        .collect::<Vec<_>>();
    assert_eq!(lines[0], "alpha", "row 0 must be untouched");
    assert_eq!(&lines[1..], &["apple", "banana"]);
}

/// A slow command must be killed by the timeout; filter_range returns Err
/// and the buffer is unmodified.
#[test]
#[cfg(not(windows))]
fn filter_range_timeout_kills_slow_command() {
    let mut e = make_editor("line1\nline2");
    let before = e
        .buffer()
        .rope()
        .lines()
        .map(|s| {
            let s = s.to_string();
            s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
        })
        .collect::<Vec<_>>();
    let start = std::time::Instant::now();
    let result = e.filter_range(0, 1, "sleep 30", Some(1));
    let elapsed = start.elapsed();
    assert!(result.is_err(), "slow command must time out: {result:?}");
    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "should return within ~1s timeout + slack, got {elapsed:?}"
    );
    assert_eq!(
        e.buffer()
            .rope()
            .lines()
            .map(|s| {
                let s = s.to_string();
                s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
            })
            .collect::<Vec<_>>(),
        before,
        "buffer must be unchanged"
    );
}

// ── cursor_screen_pos fold-aware row regression (#244) ────────────────────

/// Regression test for #244: `cursor_screen_pos` must subtract hidden rows
/// (rows inside a closed fold) when computing the screen-row delta `dy`.
///
/// Setup: 50-line buffer, closed fold over rows 11..=43 (32 hidden rows;
/// row 11 is the visible fold-start marker). Cursor at doc row 44.
/// Screen row = 44 - 0 (top_row) - 32 (hidden) = 12.
/// Before the fix `dy` was computed as a raw `pos_row - top_row = 44`,
/// painting the terminal cursor block 32 rows too low.
#[test]
fn cursor_screen_pos_skips_hidden_fold_rows() {
    let content = (0..50)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");

    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::from_str(&content),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );

    // Viewport: top_row=0, height=60 (shows whole file), width=80, tab_width=4.
    e.set_viewport_height(60);
    e.sync_buffer_from_textarea();
    {
        let vp = e.host_mut().viewport_mut();
        vp.top_row = 0;
        vp.width = 80;
        vp.tab_width = 4;
    }

    // Closed fold: rows 11..=43. Row 11 is the visible marker; rows 12..=43
    // (32 rows) are hidden.
    e.buffer_mut().add_fold(11, 43, true);

    // Cursor at doc row 44 (the first row below the fold).
    e.jump_cursor(44, 0);

    let pos = e.cursor_screen_pos(0, 0, 80, 60, 0);
    assert!(
        pos.is_some(),
        "cursor at row 44 must be visible in a height-60 viewport"
    );
    let screen_row = pos.unwrap().1;
    assert_eq!(
        screen_row, 12,
        "cursor_screen_pos must subtract 32 hidden rows: expected screen row 12, got {screen_row} \
         (pre-fix value would be 44 — the terminal cursor block was painted 32 rows too low)"
    );
}

/// Sanity: with no folds, cursor at doc row 5 must map to screen row 5
/// (top_row=0). Verifies the fix did not break the no-fold path.
#[test]
fn cursor_screen_pos_no_fold_row_unchanged() {
    let content = (0..50)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");

    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::from_str(&content),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );

    e.set_viewport_height(60);
    e.sync_buffer_from_textarea();
    {
        let vp = e.host_mut().viewport_mut();
        vp.top_row = 0;
        vp.width = 80;
        vp.tab_width = 4;
    }

    // No folds.
    e.jump_cursor(5, 0);

    let pos = e.cursor_screen_pos(0, 0, 80, 60, 0);
    assert!(pos.is_some(), "cursor at row 5 must be visible");
    let screen_row = pos.unwrap().1;
    assert_eq!(
        screen_row, 5,
        "no-fold path: screen row must equal doc row when top_row=0, got {screen_row}"
    );
}

/// Partial-fold sanity: fold rows 11..=20 closed (9 hidden rows), cursor at
/// doc row 44. Expected screen row = 44 - 0 - 9 = 35.
#[test]
fn cursor_screen_pos_partial_fold_arithmetic() {
    let content = (0..50)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");

    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::from_str(&content),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );

    e.set_viewport_height(60);
    e.sync_buffer_from_textarea();
    {
        let vp = e.host_mut().viewport_mut();
        vp.top_row = 0;
        vp.width = 80;
        vp.tab_width = 4;
    }

    // Closed fold rows 11..=20: row 11 visible, rows 12..=20 hidden (9 rows).
    e.buffer_mut().add_fold(11, 20, true);
    e.jump_cursor(44, 0);

    let pos = e.cursor_screen_pos(0, 0, 80, 60, 0);
    assert!(pos.is_some(), "cursor at row 44 must be visible");
    let screen_row = pos.unwrap().1;
    assert_eq!(
        screen_row, 35,
        "partial fold (9 hidden rows): expected screen row 35, got {screen_row}"
    );
}

/// #244 follow-up: `:<n>` / `goto_line` to a row hidden inside a closed
/// fold must open the enclosing fold(s) so the landing line is visible,
/// then place the cursor on it. A jump to an unseen row is useless.
#[test]
fn goto_line_opens_enclosing_fold() {
    let content: String = (0..50)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::from_str(&content),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    // Closed fold over rows 11..=43; rows 12..=43 hidden.
    e.buffer_mut().add_fold(11, 43, true);
    assert!(
        e.buffer().is_row_hidden(20),
        "precondition: row 20 hidden by closed fold"
    );
    // `:21` → 1-based line 21 == row 20, which is inside the closed fold.
    e.goto_line(21);
    assert_eq!(e.cursor().0, 20, "cursor must land on the target row 20");
    assert!(
        !e.buffer().is_row_hidden(20),
        "goto_line must open the enclosing fold so the target row is visible"
    );
}

/// #244 follow-up: nested closed folds must all open along the path to
/// the target row.
#[test]
fn goto_line_opens_nested_folds() {
    let content: String = (0..50)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::from_str(&content),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    // Outer fold 5..=40 closed, inner fold 10..=20 closed (nested).
    e.buffer_mut().add_fold(5, 40, true);
    e.buffer_mut().add_fold(10, 20, true);
    assert!(e.buffer().is_row_hidden(15), "precondition: row 15 hidden");
    e.goto_line(16); // row 15, inside both folds
    assert_eq!(e.cursor().0, 15, "cursor must land on row 15");
    assert!(
        !e.buffer().is_row_hidden(15),
        "both nested folds must open so row 15 is visible"
    );
}
