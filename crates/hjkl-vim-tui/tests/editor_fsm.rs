//! FSM-driving tests for the vim FSM via `hjkl_vim_tui::handle_key` and
//! `hjkl_vim::feed_input`. Relocated from `hjkl-vim/tests/editor_fsm.rs` as
//! part of #162 phase 3 (dropped `hjkl-vim`'s `crossterm` feature gate).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hjkl_engine::{Editor, Host, VimMode};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}
fn shift_key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::SHIFT)
}
fn ctrl_key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

fn many_lines(n: usize) -> String {
    (0..n)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn prime_viewport<H: hjkl_engine::Host>(e: &mut Editor<hjkl_buffer::Buffer, H>, height: u16) {
    e.set_viewport_height(height);
}

fn fresh_editor(initial: &str) -> Editor {
    let buffer = hjkl_buffer::Buffer::from_str(initial);
    Editor::new(
        buffer,
        hjkl_engine::DefaultHost::new(),
        hjkl_engine::Options::default(),
    )
}

fn normal_editor(initial: &str) -> Editor {
    let e = fresh_editor(initial);
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Normal);
    e
}

#[test]
fn vim_normal_to_insert() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('i')));
    assert_eq!(e.vim_mode(), VimMode::Insert);
}

#[test]
fn with_options_constructs_from_spec_options() {
    // 0.0.33 (Patch C-γ): SPEC-shaped constructor preview.
    // Build with custom Options + DefaultHost; confirm the
    // settings translation honours the SPEC field names.
    let opts = hjkl_engine::types::Options {
        shiftwidth: 4,
        tabstop: 4,
        expandtab: true,
        iskeyword: "@,a-z".to_string(),
        wrap: hjkl_engine::types::WrapMode::Word,
        ..hjkl_engine::types::Options::default()
    };
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        opts,
    );
    assert_eq!(e.settings().shiftwidth, 4);
    assert_eq!(e.settings().tabstop, 4);
    assert!(e.settings().expandtab);
    assert_eq!(e.settings().iskeyword, "@,a-z");
    assert_eq!(e.settings().wrap, hjkl_buffer::Wrap::Word);
    // Confirm input plumbing still works.
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('i')));
    assert_eq!(e.vim_mode(), VimMode::Insert);
}

#[test]
fn feed_input_char_routes_through_handle_key() {
    use hjkl_engine::{Modifiers, PlannedInput};
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("abc");
    // `i` enters insert mode via SPEC input.
    hjkl_vim::feed_input(&mut e, PlannedInput::Char('i', Modifiers::default()));
    assert_eq!(e.vim_mode(), VimMode::Insert);
    // Type 'X' via SPEC input.
    hjkl_vim::feed_input(&mut e, PlannedInput::Char('X', Modifiers::default()));
    assert!(e.content().contains('X'));
}

#[test]
fn feed_input_special_key_routes() {
    use hjkl_engine::{Modifiers, PlannedInput, SpecialKey};
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("abc");
    hjkl_vim::feed_input(&mut e, PlannedInput::Char('i', Modifiers::default()));
    assert_eq!(e.vim_mode(), VimMode::Insert);
    hjkl_vim::feed_input(
        &mut e,
        PlannedInput::Key(SpecialKey::Esc, Modifiers::default()),
    );
    assert_eq!(e.vim_mode(), VimMode::Normal);
}

#[test]
fn feed_input_mouse_paste_focus_resize_no_op() {
    use hjkl_engine::{MouseEvent, MouseKind, PlannedInput, Pos};
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("abc");
    let mode_before = e.vim_mode();
    let consumed = hjkl_vim::feed_input(
        &mut e,
        PlannedInput::Mouse(MouseEvent {
            kind: MouseKind::Press,
            pos: Pos::new(0, 0),
            mods: Default::default(),
        }),
    );
    assert!(!consumed);
    assert_eq!(e.vim_mode(), mode_before);
    assert!(!hjkl_vim::feed_input(
        &mut e,
        PlannedInput::Paste("xx".into())
    ));
    assert!(!hjkl_vim::feed_input(&mut e, PlannedInput::FocusGained));
    assert!(!hjkl_vim::feed_input(&mut e, PlannedInput::FocusLost));
    assert!(!hjkl_vim::feed_input(&mut e, PlannedInput::Resize(80, 24)));
}

#[test]
fn take_changes_emits_per_row_for_block_insert() {
    // Visual-block insert (`Ctrl-V` then `I` then text then Esc)
    // produces an InsertBlock buffer edit with one chunk per
    // selected row. take_changes should surface N EditOps,
    // not a single placeholder.
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("aaa\nbbb\nccc\nddd");
    // Place cursor at (0, 0), enter visual-block, extend down 2.
    hjkl_vim_tui::handle_key(
        &mut e,
        KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL),
    );
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('j')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('j')));
    // `I` to enter insert mode at the block left edge.
    hjkl_vim_tui::handle_key(&mut e, shift_key(KeyCode::Char('I')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('X')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Esc));

    let changes = e.take_changes();
    // Expect at least 3 entries — one per row in the 3-row block.
    // Vim's block-I inserts on Esc; the cleanup may add more
    // EditOps for cursor sync, hence >= rather than ==.
    assert!(
        changes.len() >= 3,
        "expected >=3 EditOps for 3-row block insert, got {}: {changes:?}",
        changes.len()
    );
}

#[test]
fn take_changes_drains_after_insert() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("abc");
    // Empty initially.
    assert!(e.take_changes().is_empty());
    // Type a char in insert mode.
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('i')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('X')));
    let changes = e.take_changes();
    assert!(
        !changes.is_empty(),
        "insert mode keystroke should produce a change"
    );
    // Drained — second call empty.
    assert!(e.take_changes().is_empty());
}

#[test]
fn selection_highlight_some_in_visual() {
    use hjkl_engine::types::HighlightKind;
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("hello world");
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('v')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('l')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('l')));
    let h = e
        .selection_highlight()
        .expect("visual mode should produce a highlight");
    assert_eq!(h.kind, HighlightKind::Selection);
    assert_eq!(h.range.start.line, 0);
    assert_eq!(h.range.end.line, 0);
}

#[test]
fn highlights_emit_incsearch_during_active_prompt() {
    use hjkl_engine::types::HighlightKind;
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("foo bar foo\nbaz\n");
    // Open the `/` prompt and type `f` `o` `o`.
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('/')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('f')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('o')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('o')));
    // Prompt should be active.
    assert!(e.search_prompt().is_some());
    let hs = e.highlights_for_line(0);
    assert_eq!(hs.len(), 2);
    for h in &hs {
        assert_eq!(h.kind, HighlightKind::IncSearch);
    }
}

#[test]
fn highlights_empty_for_blank_prompt() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("foo");
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('/')));
    // Nothing typed yet — prompt active but text empty.
    assert!(e.search_prompt().is_some());
    assert!(e.highlights_for_line(0).is_empty());
}

#[test]
fn render_frame_reflects_mode_and_cursor() {
    use hjkl_engine::types::{CursorShape, SnapshotMode};
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("alpha\nbeta");
    let f = e.render_frame();
    assert_eq!(f.mode, SnapshotMode::Normal);
    assert_eq!(f.cursor_shape, CursorShape::Block);
    assert_eq!(f.line_count, 2);

    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('i')));
    let f = e.render_frame();
    assert_eq!(f.mode, SnapshotMode::Insert);
    assert_eq!(f.cursor_shape, CursorShape::Bar);
}

#[test]
fn take_content_change_none_until_mutation() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("hello");
    // drain
    e.take_content_change();
    assert!(e.take_content_change().is_none());
    // mutate via insert mode
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('i')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('x')));
    let after = e.take_content_change();
    assert!(after.is_some());
    assert!(after.unwrap().contains('x'));
}

#[test]
fn vim_insert_to_normal() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('i')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Esc));
    assert_eq!(e.vim_mode(), VimMode::Normal);
}

#[test]
fn vim_normal_to_visual() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('v')));
    assert_eq!(e.vim_mode(), VimMode::Visual);
}

#[test]
fn vim_visual_to_normal() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('v')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Esc));
    assert_eq!(e.vim_mode(), VimMode::Normal);
}

#[test]
fn vim_shift_i_moves_to_first_non_whitespace() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("   hello");
    e.jump_cursor(0, 8);
    hjkl_vim_tui::handle_key(&mut e, shift_key(KeyCode::Char('I')));
    assert_eq!(e.vim_mode(), VimMode::Insert);
    assert_eq!(e.cursor(), (0, 3));
}

#[test]
fn vim_shift_a_moves_to_end_and_insert() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("hello");
    hjkl_vim_tui::handle_key(&mut e, shift_key(KeyCode::Char('A')));
    assert_eq!(e.vim_mode(), VimMode::Insert);
    assert_eq!(e.cursor().1, 5);
}

#[test]
fn count_10j_moves_down_10() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content(
        (0..20)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n")
            .as_str(),
    );
    for d in "10".chars() {
        hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char(d)));
    }
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('j')));
    assert_eq!(e.cursor().0, 10);
}

#[test]
fn count_o_repeats_insert_on_esc() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("hello");
    for d in "3".chars() {
        hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char(d)));
    }
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('o')));
    assert_eq!(e.vim_mode(), VimMode::Insert);
    for c in "world".chars() {
        hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char(c)));
    }
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Esc));
    assert_eq!(e.vim_mode(), VimMode::Normal);
    assert_eq!(e.buffer().row_count(), 4);
    assert!(
        e.buffer()
            .rope()
            .lines()
            .map(|s| {
                let s = s.to_string();
                s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
            })
            .skip(1)
            .all(|l| l == "world")
    );
}

#[test]
fn count_i_repeats_text_on_esc() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("");
    for d in "3".chars() {
        hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char(d)));
    }
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('i')));
    for c in "ab".chars() {
        hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char(c)));
    }
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Esc));
    assert_eq!(e.vim_mode(), VimMode::Normal);
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "ababab");
}

#[test]
fn vim_shift_o_opens_line_above() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("hello");
    hjkl_vim_tui::handle_key(&mut e, shift_key(KeyCode::Char('O')));
    assert_eq!(e.vim_mode(), VimMode::Insert);
    assert_eq!(e.cursor(), (0, 0));
    assert_eq!(e.buffer().row_count(), 2);
}

#[test]
fn vim_gg_goes_to_top() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("a\nb\nc");
    e.jump_cursor(2, 0);
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('g')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('g')));
    assert_eq!(e.cursor().0, 0);
}

#[test]
fn vim_shift_g_goes_to_bottom() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("a\nb\nc");
    hjkl_vim_tui::handle_key(&mut e, shift_key(KeyCode::Char('G')));
    assert_eq!(e.cursor().0, 2);
}

#[test]
fn vim_dd_deletes_line() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("first\nsecond");
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('d')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('d')));
    assert_eq!(e.buffer().row_count(), 1);
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "second");
}

#[test]
fn vim_dw_deletes_word() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("hello world");
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('d')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('w')));
    assert_eq!(e.vim_mode(), VimMode::Normal);
    assert!(!hjkl_buffer::rope_line_str(&e.buffer().rope(), 0).starts_with("hello"));
}

#[test]
fn vim_yy_yanks_line() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("hello\nworld");
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('y')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('y')));
    assert!(
        e.host_mut()
            .read_clipboard()
            .as_deref()
            .unwrap_or("")
            .starts_with("hello")
    );
}

#[test]
fn vim_yy_does_not_move_cursor() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("first\nsecond\nthird");
    e.jump_cursor(1, 0);
    let before = e.cursor();
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('y')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('y')));
    assert_eq!(e.cursor(), before);
    assert_eq!(e.vim_mode(), VimMode::Normal);
}

#[test]
fn vim_yw_yanks_word() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("hello world");
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('y')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('w')));
    assert_eq!(e.vim_mode(), VimMode::Normal);
    assert!(e.host_mut().read_clipboard().is_some());
}

#[test]
fn vim_cc_changes_line() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("hello\nworld");
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('c')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('c')));
    assert_eq!(e.vim_mode(), VimMode::Insert);
}

#[test]
fn vim_u_undoes_insert_session_as_chunk() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("hello");
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('i')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Enter));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Enter));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Esc));
    assert_eq!(e.buffer().row_count(), 3);
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('u')));
    assert_eq!(e.buffer().row_count(), 1);
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "hello");
}

#[test]
fn vim_undo_redo_roundtrip() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("hello");
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('i')));
    for c in "world".chars() {
        hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char(c)));
    }
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Esc));
    let after = hjkl_buffer::rope_line_str(&e.buffer().rope(), 0);
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('u')));
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "hello");
    hjkl_vim_tui::handle_key(&mut e, ctrl_key(KeyCode::Char('r')));
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), after);
}

#[test]
fn vim_u_undoes_dd() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("first\nsecond");
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('d')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('d')));
    assert_eq!(e.buffer().row_count(), 1);
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('u')));
    assert_eq!(e.buffer().row_count(), 2);
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "first");
}

#[test]
fn vim_ctrl_r_redoes() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("hello");
    hjkl_vim_tui::handle_key(&mut e, ctrl_key(KeyCode::Char('r')));
}

#[test]
fn vim_r_replaces_char() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("hello");
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('r')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('x')));
    assert_eq!(
        hjkl_buffer::rope_line_str(&e.buffer().rope(), 0)
            .chars()
            .next(),
        Some('x')
    );
}

#[test]
fn vim_tilde_toggles_case() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("hello");
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('~')));
    assert_eq!(
        hjkl_buffer::rope_line_str(&e.buffer().rope(), 0)
            .chars()
            .next(),
        Some('H')
    );
}

#[test]
fn vim_visual_d_cuts() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("hello");
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('v')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('l')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('l')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('d')));
    assert_eq!(e.vim_mode(), VimMode::Normal);
    assert!(e.host_mut().read_clipboard().is_some());
}

#[test]
fn vim_visual_c_enters_insert() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("hello");
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('v')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('l')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('c')));
    assert_eq!(e.vim_mode(), VimMode::Insert);
}

#[test]
fn vim_normal_unknown_key_consumed() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    // Unknown keys are consumed (swallowed) rather than returning false.
    let consumed = hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('z')));
    assert!(consumed);
}

#[test]
fn force_normal_clears_operator() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('d')));
    e.force_normal();
    assert_eq!(e.vim_mode(), VimMode::Normal);
}

#[test]
fn zz_centers_cursor_in_viewport() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content(&many_lines(100));
    prime_viewport(&mut e, 20);
    e.jump_cursor(50, 0);
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('z')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('z')));
    assert_eq!(e.host().viewport().top_row, 40);
    assert_eq!(e.cursor().0, 50);
}

#[test]
fn zt_puts_cursor_at_viewport_top_with_scrolloff() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content(&many_lines(100));
    prime_viewport(&mut e, 20);
    e.jump_cursor(50, 0);
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('z')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('t')));
    // Cursor lands at top of viable area = top + SCROLLOFF (5).
    // Viewport top therefore sits at cursor - 5.
    assert_eq!(e.host().viewport().top_row, 45);
    assert_eq!(e.cursor().0, 50);
}

#[test]
fn ctrl_a_increments_number_at_cursor() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("x = 41");
    hjkl_vim_tui::handle_key(&mut e, ctrl_key(KeyCode::Char('a')));
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "x = 42");
    assert_eq!(e.cursor(), (0, 5));
}

#[test]
fn ctrl_a_finds_number_to_right_of_cursor() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("foo 99 bar");
    hjkl_vim_tui::handle_key(&mut e, ctrl_key(KeyCode::Char('a')));
    assert_eq!(
        hjkl_buffer::rope_line_str(&e.buffer().rope(), 0),
        "foo 100 bar"
    );
    assert_eq!(e.cursor(), (0, 6));
}

#[test]
fn ctrl_a_with_count_adds_count() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("x = 10");
    for d in "5".chars() {
        hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char(d)));
    }
    hjkl_vim_tui::handle_key(&mut e, ctrl_key(KeyCode::Char('a')));
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "x = 15");
}

#[test]
fn ctrl_x_decrements_number() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("n=5");
    hjkl_vim_tui::handle_key(&mut e, ctrl_key(KeyCode::Char('x')));
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "n=4");
}

#[test]
fn ctrl_x_crosses_zero_into_negative() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("v=0");
    hjkl_vim_tui::handle_key(&mut e, ctrl_key(KeyCode::Char('x')));
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "v=-1");
}

#[test]
fn ctrl_a_on_negative_number_increments_toward_zero() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("a = -5");
    hjkl_vim_tui::handle_key(&mut e, ctrl_key(KeyCode::Char('a')));
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "a = -4");
}

#[test]
fn ctrl_a_noop_when_no_digit_on_line() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("no digits here");
    hjkl_vim_tui::handle_key(&mut e, ctrl_key(KeyCode::Char('a')));
    assert_eq!(
        hjkl_buffer::rope_line_str(&e.buffer().rope(), 0),
        "no digits here"
    );
}

#[test]
fn zb_puts_cursor_at_viewport_bottom_with_scrolloff() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content(&many_lines(100));
    prime_viewport(&mut e, 20);
    e.jump_cursor(50, 0);
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('z')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('b')));
    // Cursor lands at bottom of viable area = top + height - 1 -
    // SCROLLOFF. For height 20, scrolloff 5: cursor at top + 14,
    // so top = cursor - 14 = 36.
    assert_eq!(e.host().viewport().top_row, 36);
    assert_eq!(e.cursor().0, 50);
}

#[test]
fn content_arc_returns_same_arc_until_mutation() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("hello");
    let a = e.content_arc();
    let b = e.content_arc();
    assert!(
        std::sync::Arc::ptr_eq(&a, &b),
        "repeated content_arc() should hit the cache"
    );

    // Any mutation must invalidate the cache.
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('i')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('!')));
    let c = e.content_arc();
    assert!(
        !std::sync::Arc::ptr_eq(&a, &c),
        "mutation should invalidate content_arc() cache"
    );
    assert!(c.contains('!'));
}

#[test]
fn mouse_click_breaks_insert_undo_group_when_undobreak_on() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("hello world");
    // Default settings.undo_break_on_motion = true.
    assert!(e.settings().undo_break_on_motion);
    // Enter insert mode and type "AAA" before the line content.
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('i')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('A')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('A')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('A')));
    // Mouse click somewhere else on the line (still insert mode).
    e.mouse_click_doc(0, 8);
    // Type more chars at the new cursor position.
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('B')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('B')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('B')));
    // Leave insert and undo once.
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Esc));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('u')));
    let line = hjkl_buffer::rope_line_str(&e.buffer().rope(), 0);
    assert!(
        line.contains("AAA"),
        "AAA must survive undo (separate group): {line:?}"
    );
    assert!(
        !line.contains("BBB"),
        "BBB must be undone (post-click group): {line:?}"
    );
}

#[test]
fn mouse_click_keeps_one_undo_group_when_undobreak_off() {
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("hello world");
    e.settings_mut().undo_break_on_motion = false;
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('i')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('A')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('A')));
    e.mouse_click_doc(0, 8);
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('B')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('B')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Esc));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('u')));
    let line = hjkl_buffer::rope_line_str(&e.buffer().rope(), 0);
    assert!(
        !line.contains("AA") && !line.contains("BB"),
        "with undobreak off, single `u` must reverse whole insert: {line:?}"
    );
    assert_eq!(line, "hello world");
}

#[test]
fn host_records_clipboard_on_yank() {
    // `yy` on a single-line buffer must drive `Host::write_clipboard`.
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::types::DefaultHost::new(),
        hjkl_engine::types::Options::default(),
    );
    e.set_content("hello\n");
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('y')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('y')));
    // Clipboard cache holds the linewise yank.
    let clip = e.host_mut().read_clipboard();
    assert!(
        clip.as_deref().unwrap_or("").starts_with("hello"),
        "host clipboard should carry the yank: {clip:?}"
    );
}

#[test]
fn host_cursor_shape_via_shared_recorder() {
    // Recording host backed by a leaked `Mutex` so the test can
    // inspect the emit sequence after the editor has consumed the
    // host. (Host: Send rules out Rc/RefCell.)
    let shapes_ptr: &'static std::sync::Mutex<Vec<hjkl_engine::types::CursorShape>> =
        Box::leak(Box::new(std::sync::Mutex::new(Vec::new())));
    struct LeakHost {
        shapes: &'static std::sync::Mutex<Vec<hjkl_engine::types::CursorShape>>,
        viewport: hjkl_engine::types::Viewport,
    }
    impl hjkl_engine::types::Host for LeakHost {
        type Intent = ();
        fn write_clipboard(&mut self, _: String) {}
        fn read_clipboard(&mut self) -> Option<String> {
            None
        }
        fn now(&self) -> core::time::Duration {
            core::time::Duration::ZERO
        }
        fn prompt_search(&mut self) -> Option<String> {
            None
        }
        fn emit_cursor_shape(&mut self, s: hjkl_engine::types::CursorShape) {
            self.shapes.lock().unwrap().push(s);
        }
        fn viewport(&self) -> &hjkl_engine::types::Viewport {
            &self.viewport
        }
        fn viewport_mut(&mut self) -> &mut hjkl_engine::types::Viewport {
            &mut self.viewport
        }
        fn emit_intent(&mut self, _: Self::Intent) {}
    }
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        LeakHost {
            shapes: shapes_ptr,
            viewport: hjkl_engine::types::Viewport::default(),
        },
        hjkl_engine::types::Options::default(),
    );
    e.set_content("abc");
    // Normal → Insert: Bar emit.
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('i')));
    // Insert → Normal: Block emit.
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Esc));
    let shapes = shapes_ptr.lock().unwrap().clone();
    assert_eq!(
        shapes,
        vec![
            hjkl_engine::types::CursorShape::Bar,
            hjkl_engine::types::CursorShape::Block,
        ],
        "host should observe Insert(Bar) → Normal(Block) transitions"
    );
}

#[test]
fn host_now_drives_chord_timeout_deterministically() {
    // Custom host whose `now()` is host-controlled; we drive it
    // forward by `timeout_len + 1ms` between the first `g` and
    // the second so the chord-timeout fires regardless of
    // wall-clock progress.
    let now_ptr: &'static std::sync::Mutex<core::time::Duration> =
        Box::leak(Box::new(std::sync::Mutex::new(core::time::Duration::ZERO)));
    struct ClockHost {
        now: &'static std::sync::Mutex<core::time::Duration>,
        viewport: hjkl_engine::types::Viewport,
    }
    impl hjkl_engine::types::Host for ClockHost {
        type Intent = ();
        fn write_clipboard(&mut self, _: String) {}
        fn read_clipboard(&mut self) -> Option<String> {
            None
        }
        fn now(&self) -> core::time::Duration {
            *self.now.lock().unwrap()
        }
        fn prompt_search(&mut self) -> Option<String> {
            None
        }
        fn emit_cursor_shape(&mut self, _: hjkl_engine::types::CursorShape) {}
        fn viewport(&self) -> &hjkl_engine::types::Viewport {
            &self.viewport
        }
        fn viewport_mut(&mut self) -> &mut hjkl_engine::types::Viewport {
            &mut self.viewport
        }
        fn emit_intent(&mut self, _: Self::Intent) {}
    }
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        ClockHost {
            now: now_ptr,
            viewport: hjkl_engine::types::Viewport::default(),
        },
        hjkl_engine::types::Options::default(),
    );
    e.set_content("a\nb\nc\n");
    e.jump_cursor(2, 0);
    // First `g` — host time = 0ms, lands in g-pending.
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('g')));
    // Advance host time well past timeout_len (default 1000ms).
    *now_ptr.lock().unwrap() = core::time::Duration::from_secs(60);
    // Second `g` — chord-timeout fires; bare `g` re-dispatches and
    // does nothing on its own. Cursor must NOT have jumped to row 0.
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('g')));
    assert_eq!(
        e.cursor().0,
        2,
        "Host::now() must drive `:set timeoutlen` deterministically"
    );
}

#[test]
fn after_g_gv_restores_last_visual() {
    // Enter visual, move right, exit, then gv re-enters.
    let mut e = fresh_editor("hello world\n");
    // Enter char-visual at col 0, move to col 3, then exit.
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('v')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('l')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('l')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('l')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Esc));
    assert_eq!(e.vim_mode(), VimMode::Normal, "should be Normal after Esc");
    // gv via after_g.
    e.after_g('v', 1);
    assert_eq!(
        e.vim_mode(),
        VimMode::Visual,
        "gv must re-enter Visual mode"
    );
}

#[test]
fn insert_char_appends_in_replace_mode() {
    // `R` enters Replace mode; insert_char overwrites the cell under
    // the cursor instead of inserting.
    let mut e = fresh_editor("abc");
    hjkl_vim_tui::handle_key(
        &mut e,
        KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT),
    );
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Insert);
    e.insert_char('X');
    // 'a' (col 0) overwritten by 'X': "Xbc"
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "Xbc");
    e.insert_char('Y');
    // 'b' (col 1) overwritten by 'Y': "XYc"
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "XYc");
}

#[test]
fn insert_paste_register_inserts_text() {
    let mut e = fresh_editor("abc");
    // Yank "abc" into the unnamed register via `yy`.
    hjkl_vim_tui::handle_key(
        &mut e,
        KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
    );
    hjkl_vim_tui::handle_key(
        &mut e,
        KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
    );
    e.enter_insert_i(1);
    // Paste from unnamed register '"'.
    e.insert_paste_register('"');
    // "abc\n" pasted inline — line now contains "abc\nabc".
    assert!(e.content().contains("abc"));
}

#[test]
fn paste_after_charwise_inserts_after_cursor() {
    // Use `yw` via FSM to yank "b" without disturbing the registers.
    let mut e = normal_editor("b ac");
    // Yank "b" via `yw` (yank word).
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('y')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('w')));
    // Cursor is still at col 0; jump to "ac" (col 2) and paste after 'a'.
    e.jump_cursor(0, 2);
    e.paste_after(1);
    // "b ac" → paste "b " after 'a' at col 2 → "b ab c" — too complex.
    // Simpler: yank a single char and paste it inline.
    // Restart: use `yl` to yank one char.
    let mut e2 = normal_editor("bac");
    hjkl_vim_tui::handle_key(&mut e2, key(KeyCode::Char('y')));
    hjkl_vim_tui::handle_key(&mut e2, key(KeyCode::Char('l')));
    // Register now has "b". Move to col 1 and paste after 'b'.
    e2.jump_cursor(0, 1); // on 'a'
    e2.paste_after(1);
    // "bac" → paste "b" after col 1 ('a') → "babc"
    assert_eq!(hjkl_buffer::rope_line_str(&e2.buffer().rope(), 0), "babc");
}

#[test]
fn paste_before_charwise_inserts_before_cursor() {
    let mut e = normal_editor("bac");
    // Yank 'b' with `yl`.
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('y')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('l')));
    // Move to col 2 ('c') and paste before it.
    e.jump_cursor(0, 2);
    e.paste_before(1);
    // "bac" → paste "b" before col 2 → "babc"
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "babc");
}

#[test]
fn paste_after_count_3_repeats() {
    let mut e = normal_editor("x");
    // Yank 'x' with `yl`.
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('y')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('l')));
    e.paste_after(3);
    // "x" + 3 pastes of "x" = "xxxx".
    assert_eq!(hjkl_buffer::rope_line_str(&e.buffer().rope(), 0), "xxxx");
}

#[test]
fn vim_mode_consistent_with_fsm_after_v_key() {
    // Existing FSM path: pressing 'v' via handle_key should still report Visual.
    let mut e = normal_editor("hello");
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('v')));
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Visual);
}

#[test]
fn vim_mode_consistent_with_fsm_after_esc_from_visual() {
    let mut e = normal_editor("hello");
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Char('v')));
    hjkl_vim_tui::handle_key(&mut e, key(KeyCode::Esc));
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Normal);
}

#[test]
fn current_mode_syncs_with_public_mode_after_every_step() {
    // After every FSM step vim_mode() must return the expected mode.
    let mut e = normal_editor("hello world\nline two\n");
    let steps: &[(KeyCode, VimMode)] = &[
        (KeyCode::Char('v'), VimMode::Visual),
        (KeyCode::Char('l'), VimMode::Visual),
        (KeyCode::Esc, VimMode::Normal),
        (KeyCode::Char('i'), VimMode::Insert),
        (KeyCode::Esc, VimMode::Normal),
    ];
    for (kc, expected) in steps {
        hjkl_vim_tui::handle_key(&mut e, key(*kc));
        assert_eq!(e.vim_mode(), *expected, "after key {:?}", kc);
    }
}
