/// Phase 6.6c integration tests: validate that `hjkl_vim::dispatch_input`
/// correctly routes the search-prompt FSM through `hjkl-vim` rather than
/// the deprecated engine shim.
use hjkl_engine::{Editor, Input, Key};

fn editor_with(content: &str) -> Editor {
    let opts = hjkl_engine::Options::default();
    let mut e = Editor::new(
        hjkl_buffer::Buffer::new(),
        hjkl_engine::DefaultHost::new(),
        opts,
    );
    e.set_content(content);
    e
}

fn inp(key: Key) -> Input {
    Input {
        key,
        ctrl: false,
        alt: false,
        shift: false,
    }
}

fn ctrl(key: Key) -> Input {
    Input {
        key,
        ctrl: true,
        alt: false,
        shift: false,
    }
}

/// Run a string of keys through `dispatch_input` (not the deprecated shim).
/// Supports the same `<tag>` notation the engine tests use.
fn dispatch_keys(e: &mut Editor, keys: &str) {
    let mut iter = keys.chars().peekable();
    while let Some(c) = iter.next() {
        if c == '<' {
            let mut tag = String::new();
            for ch in iter.by_ref() {
                if ch == '>' {
                    break;
                }
                tag.push(ch);
            }
            let input = match tag.as_str() {
                "Esc" => inp(Key::Esc),
                "CR" => inp(Key::Enter),
                "BS" => inp(Key::Backspace),
                "Up" => inp(Key::Up),
                "Down" => inp(Key::Down),
                s if s.starts_with("C-") => {
                    let ch = s.chars().nth(2).unwrap();
                    ctrl(Key::Char(ch))
                }
                _ => continue,
            };
            hjkl_vim::dispatch_input(e, input);
        } else {
            hjkl_vim::dispatch_input(e, inp(Key::Char(c)));
        }
    }
}

// ── insert-mode dispatch tests (Phase 6.6d) ───────────────────────────────────

#[test]
fn insert_char_appends_to_buffer() {
    // Enter insert mode via the public API, then dispatch a Char key through
    // `dispatch_input`. The buffer should contain the typed character.
    let mut e = editor_with("");
    e.enter_insert_i(1);
    hjkl_vim::dispatch_input(&mut e, inp(Key::Char('x')));
    hjkl_vim::dispatch_input(&mut e, inp(Key::Esc));
    // Buffer always has a trailing newline.
    assert!(
        e.content().starts_with('x'),
        "dispatch_input should type 'x' in insert mode; got: {:?}",
        e.content()
    );
}

#[test]
fn insert_mode_esc_returns_to_normal() {
    use hjkl_engine::VimMode;
    let mut e = editor_with("hello");
    e.enter_insert_i(1);
    assert_eq!(e.vim_mode(), VimMode::Insert);
    hjkl_vim::dispatch_input(&mut e, inp(Key::Esc));
    assert_eq!(
        e.vim_mode(),
        VimMode::Normal,
        "Esc via dispatch_input should exit insert mode"
    );
}

#[test]
fn insert_backspace_deletes_char() {
    let mut e = editor_with("");
    e.enter_insert_i(1);
    hjkl_vim::dispatch_input(&mut e, inp(Key::Char('a')));
    hjkl_vim::dispatch_input(&mut e, inp(Key::Char('b')));
    hjkl_vim::dispatch_input(&mut e, inp(Key::Backspace));
    hjkl_vim::dispatch_input(&mut e, inp(Key::Esc));
    // Buffer content starts with 'a'; trailing newline is expected.
    assert!(
        e.content().starts_with('a') && !e.content().starts_with("ab"),
        "Backspace via dispatch_input should delete last char; got: {:?}",
        e.content()
    );
}

#[test]
fn insert_ctrl_r_pastes_register() {
    // Write "hi" into the 'z' named register directly, then paste via Ctrl-R z.
    let mut e = editor_with("");
    e.registers_mut()
        .record_yank("hi".to_string(), false, Some('z'));
    e.enter_insert_i(1);
    // Ctrl-R arms the register wait, then 'z' pastes.
    hjkl_vim::dispatch_input(&mut e, ctrl(Key::Char('r')));
    hjkl_vim::dispatch_input(&mut e, inp(Key::Char('z')));
    hjkl_vim::dispatch_input(&mut e, inp(Key::Esc));
    let content = e.content();
    assert!(
        content.contains("hi"),
        "Ctrl-R z should paste register contents; got: {content:?}"
    );
}

// ── search-prompt dispatch tests ──────────────────────────────────────────────

#[test]
fn search_forward_commit_moves_cursor() {
    // Buffer: "alpha beta" — cursor at col 0.
    // `/beta<CR>` should advance the cursor to col 6 (start of "beta").
    let mut e = editor_with("alpha beta");
    dispatch_keys(&mut e, "/beta<CR>");
    assert_eq!(e.cursor(), (0, 6), "cursor should land on 'beta'");
}

#[test]
fn search_commit_no_match_does_not_push_jump_via_dispatch() {
    // A search for a pattern that doesn't exist should leave the jumplist
    // unchanged — same invariant as the engine's own test, but exercised
    // through `dispatch_input`.
    let mut e = editor_with("alpha beta\nfoo end");
    e.jump_cursor(0, 3);
    let pre_len = e.jump_back_list().len();
    dispatch_keys(&mut e, "/zzznotfound<CR>");
    assert_eq!(
        e.jump_back_list().len(),
        pre_len,
        "no match → jumplist should not grow"
    );
}

#[test]
fn search_esc_cancels_without_moving_cursor() {
    let mut e = editor_with("alpha beta");
    let pre = e.cursor();
    dispatch_keys(&mut e, "/beta<Esc>");
    assert_eq!(e.cursor(), pre, "Esc should not move the cursor");
}

#[test]
fn search_backspace_trims_pattern() {
    // Open `/`, type "beta", backspace once → pattern is "bet",
    // then Enter — "bet" matches at col 6.
    let mut e = editor_with("alpha beta");
    dispatch_keys(&mut e, "/beta<BS><CR>");
    // "bet" still matches start of "beta" at col 6.
    assert_eq!(e.cursor(), (0, 6));
}

// ── vim-sneak FSM tests ───────────────────────────────────────────────────────

/// `sba` from [0,0] on "foo bar baz qux\n" → cursor [0,4] (start of "ba" in "bar").
#[test]
fn sneak_forward_fsm_jumps_to_digraph() {
    let mut e = editor_with("foo bar baz qux");
    dispatch_keys(&mut e, "sba");
    assert_eq!(e.cursor(), (0, 4), "s+ba should land on 'ba' in 'bar'");
}

/// `Sba` from [0,12] → cursor [0,8] (backward to "baz").
#[test]
fn sneak_backward_fsm_s_uppercase() {
    let mut e = editor_with("foo bar baz qux");
    e.jump_cursor(0, 12);
    dispatch_keys(&mut e, "Sba");
    assert_eq!(e.cursor(), (0, 8), "S+ba backward should land on 'baz'");
}

/// After `sba`, `;` should repeat forward (sneak-repeat, not f-repeat).
#[test]
fn sneak_fsm_semicolon_repeats_forward() {
    let mut e = editor_with("foo bar baz qux");
    dispatch_keys(&mut e, "sba");
    assert_eq!(e.cursor(), (0, 4));
    dispatch_keys(&mut e, ";");
    assert_eq!(
        e.cursor(),
        (0, 8),
        "semicolon after sneak should jump to next 'ba'"
    );
}

/// After `sba` from [0,0], `,` (reverse) — no prior "ba" → stays at [0,4].
#[test]
fn sneak_fsm_comma_reverse_no_prior_match() {
    let mut e = editor_with("foo bar baz qux");
    dispatch_keys(&mut e, "sba");
    assert_eq!(e.cursor(), (0, 4));
    let pre = e.cursor();
    dispatch_keys(&mut e, ",");
    assert_eq!(e.cursor(), pre, "comma with no prior 'ba' should not move");
}

/// `dsab` on "hello ab world" from [0,0] → "ab world".
#[test]
fn sneak_fsm_operator_pending_delete() {
    let mut e = editor_with("hello ab world");
    dispatch_keys(&mut e, "dsab");
    let content = e.content();
    assert!(
        content.starts_with("ab world"),
        "dsab should delete up to 'ab' leaving 'ab world'; got: {content:?}"
    );
}

/// `sneak_disabled_falls_through_to_substitute_char`:
/// `:set nomotion_sneak` (via settings_mut) then `sx<Esc>` → substitute char.
#[test]
fn sneak_disabled_falls_through_to_substitute_char() {
    let mut e = editor_with("foo");
    e.settings_mut().motion_sneak = false;
    // `s` with sneak disabled should substitute char (enter insert, delete 'f', type 'x').
    dispatch_keys(&mut e, "sx<Esc>");
    // Buffer starts with 'x' (substitute-char path was taken).
    let content = e.content();
    assert!(
        content.starts_with('x'),
        "with motion_sneak=false, s should substitute char; got: {content:?}"
    );
    // Cursor should be at col 0 after Esc.
    assert_eq!(e.cursor().1, 0, "cursor should be col 0 after s+char+Esc");
}
