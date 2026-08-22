/// Phase 6.6c integration tests: validate that `hjkl_vim::dispatch_input`
/// correctly routes the search-prompt FSM through `hjkl-vim` rather than
/// the deprecated engine shim.
use hjkl_engine::{Editor, Input, Key};
use hjkl_vim::VimEditorExt;

fn editor_with(content: &str) -> Editor {
    let opts = hjkl_engine::Options::default();
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::new(),
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

fn modified(key: Key, ctrl: bool, alt: bool, shift: bool) -> Input {
    Input {
        key,
        ctrl,
        alt,
        shift,
    }
}

/// Run a string of keys through `dispatch_input` (not the deprecated shim).
/// Supports the same `<tag>` notation the engine tests use.
fn dispatch_keys(e: &mut Editor, keys: &str) {
    let mut iter = keys.chars();
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

#[test]
fn failed_linewise_motion_does_not_delete_at_buffer_edge() {
    for keys in ["dk", "dj", "d+", "d-", "dgk", "dgj"] {
        let mut e = editor_with("only line");
        e.jump_cursor(0, 3);

        dispatch_keys(&mut e, keys);

        assert_eq!(
            e.content(),
            "only line\n",
            "{keys} must be a no-op at the edge"
        );
    }
}

#[test]
fn count_one_d_deletes_the_current_line() {
    let mut e = editor_with("only line");

    dispatch_keys(&mut e, "d_");

    assert_eq!(e.content(), "\n");
}

#[test]
fn counted_linewise_paste_is_one_undoable_edit() {
    let mut e = editor_with("base");
    e.with_registers_mut(|r| r.record_yank("one\ntwo\n".to_string(), true, None));

    dispatch_keys(&mut e, "3p");

    assert_eq!(e.content(), "base\none\ntwo\none\ntwo\none\ntwo\n");
    e.undo();
    assert_eq!(e.content(), "base\n");
}

/// The budget counts the `count` prefix's multiplication, so it is driven here
/// by a huge count over a one-byte register rather than by a huge register —
/// same guard, without allocating the budget in the test.
#[test]
fn oversized_paste_is_rejected_without_mutation() {
    let mut e = editor_with("base");
    e.with_registers_mut(|r| r.record_yank("x".to_string(), false, None));
    let cursor = e.cursor();
    let undo_depth = e.undo_stack_len();

    dispatch_keys(&mut e, &format!("{}p", hjkl_vim::MAX_PASTE_BYTES + 1));

    assert_eq!(e.content(), "base\n");
    assert_eq!(e.cursor(), cursor);
    assert_eq!(e.undo_stack_len(), undo_depth);
}

#[test]
fn oversized_block_paste_is_rejected_without_mutation() {
    let mut e = editor_with("tail");
    e.record_yank_block("x".to_string(), hjkl_vim::MAX_PASTE_BYTES + 1, None);
    let cursor = e.cursor();
    let undo_depth = e.undo_stack_len();

    dispatch_keys(&mut e, "p");

    assert_eq!(e.content(), "tail\n");
    assert_eq!(e.cursor(), cursor);
    assert_eq!(e.undo_stack_len(), undo_depth);
}

/// A rejection used to be silent AND indistinguishable from an empty
/// register: `do_paste` returned `false` and there was no channel at all for
/// a message. It queues vim's own code on the editor now, which the host
/// drains after each key.
#[test]
fn oversized_paste_reports_vims_out_of_memory_error() {
    let mut e = editor_with("base");
    e.with_registers_mut(|r| r.record_yank("x".to_string(), false, None));

    let count = hjkl_vim::MAX_PASTE_BYTES + 1;
    dispatch_keys(&mut e, &format!("{count}p"));

    let errors = e.take_errors();
    assert_eq!(
        errors,
        vec![format!("E342: Out of memory!  (allocating {count} bytes)")],
    );
    // Draining is destructive: a second read must not repeat the message.
    assert!(e.take_errors().is_empty());
}

/// An EMPTY register is not an error — it is the ordinary "nothing to paste"
/// case, and vim says nothing about it either. This is the case the rejection
/// message used to be indistinguishable from.
#[test]
fn pasting_an_empty_register_reports_nothing() {
    let mut e = editor_with("base");
    dispatch_keys(&mut e, "p");
    assert!(e.take_errors().is_empty());
    assert_eq!(e.content(), "base\n");
}

#[test]
fn oversized_block_paste_reports_vims_out_of_memory_error() {
    let mut e = editor_with("tail");
    e.record_yank_block("x".to_string(), hjkl_vim::MAX_PASTE_BYTES + 1, None);

    dispatch_keys(&mut e, "p");

    let errors = e.take_errors();
    assert_eq!(errors.len(), 1, "expected one message, got {errors:?}");
    assert!(
        errors[0].starts_with("E342: Out of memory!"),
        "unexpected message: {}",
        errors[0]
    );
}

#[test]
fn rejected_paste_preserves_the_prior_dot_change() {
    let mut e = editor_with("abc");
    dispatch_keys(&mut e, "x");
    e.with_registers_mut(|r| r.record_yank("x".to_string(), false, None));

    dispatch_keys(&mut e, &format!("{}p.", hjkl_vim::MAX_PASTE_BYTES + 1));

    assert_eq!(e.content(), "c\n");
}

#[test]
fn empty_replace_repeat_reanchors_sticky_column_without_undo() {
    let mut e = editor_with("hello world");
    dispatch_keys(&mut e, "R<Esc>e");
    let cursor = e.cursor();
    let undo_depth = e.undo_stack_len();

    dispatch_keys(&mut e, ".");

    assert_eq!(e.cursor(), cursor);
    assert_eq!(e.sticky_col(), Some(cursor.1));
    assert_eq!(e.undo_stack_len(), undo_depth);
}

#[test]
fn modified_dot_does_not_repeat_last_change() {
    for (ctrl, alt, shift) in [
        (true, false, false),
        (false, true, false),
        (false, false, true),
    ] {
        let mut e = editor_with("abc");
        dispatch_keys(&mut e, "x");
        assert_eq!(e.content(), "bc\n");

        hjkl_vim::dispatch_input(&mut e, modified(Key::Char('.'), ctrl, alt, shift));

        assert_eq!(e.content(), "bc\n");
    }
}

#[test]
fn empty_replace_repeat_allows_the_next_change_to_be_repeated() {
    let mut e = editor_with("abc");

    dispatch_keys(&mut e, "R<Esc>.iX<Esc>.");

    assert_eq!(e.content(), "XXabc\n");
}

#[test]
fn insert_char_appends_to_buffer() {
    // Enter insert mode via the public API, then dispatch a Char key through
    // `dispatch_input`. The buffer should contain the typed character.
    let mut e = editor_with("");
    e.enter_insert_i(1);
    hjkl_vim::dispatch_input(&mut e, inp(Key::Char('x')));
    hjkl_vim::dispatch_input(&mut e, inp(Key::Esc));
    // View always has a trailing newline.
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
    // View content starts with 'a'; trailing newline is expected.
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
    e.with_registers_mut(|r| r.record_yank("hi".to_string(), false, Some('z')));
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

// ── visual `*` / `#` search dispatch tests ───────────────────────────────────

/// `<C-v>j*` searches the block text (`f\nf`) and exits VisualBlock instead of
/// treating `*` as a block-extending motion. Pinned against nvim 0.12.4.
#[test]
fn visual_block_star_searches_selected_text() {
    let mut e = editor_with("foo bar\nfoo baz\nqux foo\n");
    dispatch_keys(&mut e, "<C-v>j*");

    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Normal);
    assert_eq!(e.cursor(), (1, 0));
    assert_eq!(e.last_search(), Some(r"\Vf\nf".to_owned()));
    assert!(e.last_search_forward());
    assert_eq!(e.search_history(), vec![r"\Vf\nf".to_owned()]);
}

#[test]
fn visual_block_star_and_hash_repeat_literal_selection() {
    let mut e = editor_with("abc abc abc\n");
    dispatch_keys(&mut e, "<C-v>ll*");
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Normal);
    assert_eq!(e.last_search(), Some(r"\Vabc".to_owned()));
    assert!(e.last_search_forward());
    assert_eq!(e.cursor(), (0, 4));
    dispatch_keys(&mut e, "n");
    assert_eq!(e.cursor(), (0, 8));

    let mut e = editor_with("abc abc abc\n");
    e.jump_cursor(0, 4);
    dispatch_keys(&mut e, "<C-v>ll#");
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Normal);
    assert_eq!(e.last_search(), Some(r"\Vabc".to_owned()));
    assert!(!e.last_search_forward());
    assert_eq!(e.cursor(), (0, 0));
    dispatch_keys(&mut e, "N");
    assert_eq!(e.cursor(), (0, 4));
}

#[test]
fn visual_star_treats_punctuation_as_literal() {
    let mut e = editor_with("a.b axb a.b\n");
    dispatch_keys(&mut e, "vll*");

    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Normal);
    assert_eq!(e.last_search(), Some(r"\Va.b".to_owned()));
    assert_eq!(e.cursor(), (0, 8));
}

#[test]
fn visual_star_escapes_literal_backslashes() {
    let mut e = editor_with("a\\b axb a\\b\n");
    dispatch_keys(&mut e, "vll*");

    assert_eq!(e.last_search(), Some(r"\Va\\b".to_owned()));
    assert_eq!(e.cursor(), (0, 8));
}

#[test]
fn visual_line_star_preserves_multiline_pattern_and_head_on_no_match() {
    let mut e = editor_with("one\ntwo\n");
    dispatch_keys(&mut e, "Vj*");

    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Normal);
    assert_eq!(e.last_search(), Some("\\Vone\\ntwo".to_owned()));
    assert_eq!(e.cursor(), (1, 0));
}

#[test]
fn visual_line_star_on_empty_line_preserves_prior_search() {
    let mut e = editor_with("needle\n\nneedle\n");
    dispatch_keys(&mut e, "/needle<CR>");
    e.jump_cursor(1, 0);
    let head = e.cursor();
    let prior_pattern = e.last_search();
    let prior_forward = e.last_search_forward();
    let prior_history = e.search_history();

    dispatch_keys(&mut e, "V*");

    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Normal);
    assert_eq!(e.cursor(), head);
    assert_eq!(e.last_search(), prior_pattern);
    assert_eq!(e.last_search_forward(), prior_forward);
    assert_eq!(e.search_history(), prior_history);
}

#[test]
fn visual_search_gv_restores_completed_selection() {
    let mut e = editor_with("foo bar\nfoo baz\nqux foo\n");
    dispatch_keys(&mut e, "<C-v>j*gv");

    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::VisualBlock);
    assert_eq!(e.cursor(), (1, 0));
}

// ── search-prompt dispatch tests ──────────────────────────────────────────────

#[test]
fn search_forward_commit_moves_cursor() {
    // View: "alpha beta" — cursor at col 0.
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
    // View starts with 'x' (substitute-char path was taken).
    let content = e.content();
    assert!(
        content.starts_with('x'),
        "with motion_sneak=false, s should substitute char; got: {content:?}"
    );
    // Cursor should be at col 0 after Esc.
    assert_eq!(e.cursor().1, 0, "cursor should be col 0 after s+char+Esc");
}

// ── count-threading regression tests ──────────────────────────────────────────

/// Completing a sneak jump must not leave a stale count in the editor: after
/// `sba`, `0` is the LineStart motion, not a count digit.
#[test]
fn sneak_does_not_leak_count_into_next_command() {
    let mut e = editor_with("foo bar baz qux");
    dispatch_keys(&mut e, "sba");
    assert_eq!(e.cursor(), (0, 4), "s+ba should land on 'ba' in 'bar'");
    assert_eq!(e.count(), 0, "sneak must not leave a stale count behind");
    dispatch_keys(&mut e, "0");
    assert_eq!(e.cursor(), (0, 0), "0 after a sneak must be LineStart");
}

/// Cancelling `f` with Esc must drop the stashed count: `3f<Esc>x` deletes
/// one char, not three.
#[test]
fn cancelled_find_drops_count() {
    let mut e = editor_with("abcdef");
    dispatch_keys(&mut e, "3f<Esc>x");
    assert!(
        e.content().starts_with("bcdef"),
        "3f<Esc> must discard the count; got: {:?}",
        e.content()
    );
}

/// Cancelling `r` with Esc must drop the stashed count: `3r<Esc>x` deletes
/// one char, not three.
#[test]
fn cancelled_replace_drops_count() {
    let mut e = editor_with("abcdef");
    dispatch_keys(&mut e, "3r<Esc>x");
    assert!(
        e.content().starts_with("bcdef"),
        "3r<Esc> must discard the count; got: {:?}",
        e.content()
    );
}

// Note: the pathological `count1 * count2` saturation is covered by the
// `op_total_count_saturates_instead_of_overflowing` unit test in
// `pending.rs`. It is NOT exercised end-to-end here because feeding a
// `usize::MAX` count into the real engine makes the engine's operator-apply
// loop iterate that many times (a separate, engine-side unbounded-work
// concern outside this crate's slice).

/// `@1` plays register 1 — a digit after `@` names a register, it is not a
/// count prefix (mirrors `q1` / `"1`).
#[test]
fn at_digit_plays_numbered_register() {
    let mut e = editor_with("ab");
    // Register `"1` is the head of the delete ring. Seed it with a line-sized
    // delete — small (sub-line) deletes go to `"-`, not the numbered ring.
    e.with_registers_mut(|r| r.record_delete("x".to_string(), true, None));
    dispatch_keys(&mut e, "@1");
    assert!(
        e.content().starts_with('b'),
        "@1 should play the `x` macro in register 1; got: {:?}",
        e.content()
    );
}

/// A self-recursive macro (register `a` containing `@a`) must terminate at
/// the replay-depth cap instead of overflowing the stack.
#[test]
fn recursive_macro_terminates() {
    let mut e = editor_with("hello");
    e.with_registers_mut(|r| r.record_yank("@a".to_string(), false, Some('a')));
    dispatch_keys(&mut e, "@a");
    // Reaching here (no stack overflow) is the regression assertion.
}

/// A huge numeric search offset (`/pat/e+N`) must saturate instead of
/// overflowing isize arithmetic (panic in debug builds).
#[test]
fn search_offset_huge_value_does_not_panic() {
    let mut e = editor_with("abx");
    dispatch_keys(&mut e, "/x/e+9223372036854775807<CR>");
    assert_eq!(e.cursor().0, 0, "cursor stays on the matched row");
    let mut e2 = editor_with("foo\nbar x baz");
    e2.jump_cursor(1, 0);
    dispatch_keys(&mut e2, "/x/-9223372036854775808<CR>");
    // Reaching here without a panic is the regression assertion.
}

/// `esc_exits_blame_view`: BLAME is an FSM-owned read-only view; Esc in Normal
/// leaves it (the host no longer intercepts the key).
#[test]
fn esc_exits_blame_view() {
    let mut e = editor_with("hello\nworld");
    e.enter_blame();
    assert!(e.is_blame());
    dispatch_keys(&mut e, "<Esc>");
    assert!(!e.is_blame(), "Esc must exit BLAME via the FSM");
}

/// `mode_entry_key_exits_blame_view`: pressing `v` (or any mode-entry key) in
/// BLAME drops the overlay and enters that mode, all inside the FSM.
#[test]
fn mode_entry_key_exits_blame_view() {
    let mut e = editor_with("hello\nworld");
    e.enter_blame();
    dispatch_keys(&mut e, "v");
    assert!(!e.is_blame());
    assert_eq!(e.vim_mode(), hjkl_engine::VimMode::Visual);
}

// ── dot-repeat count override (audit A2) ──────────────────────────────────

/// `3ihello<Esc>` then `5.` — an explicit `[count]` before `.` must
/// *override* the count recorded on the insert-mode change (`:h .`), not be
/// ignored in favour of the recorded count. `LastChange::InsertAt`'s replay
/// loop used to iterate on the raw recorded `count`, skipping the same
/// `scaled(...)` override every other `LastChange` arm applies.
///
/// Each "hello" contributes exactly one 'h', so counting 'h' characters in
/// the result is a splice-position-independent way to count insertions
/// (avoids asserting the exact string, which depends on where the cursor
/// sits after `Esc`).
#[test]
fn dot_repeat_count_override_applies_to_insert_mode_change() {
    let mut e = editor_with("");
    dispatch_keys(&mut e, "3ihello<Esc>");
    let after_insert = e.content();
    assert_eq!(
        after_insert.matches('h').count(),
        3,
        "3ihello<Esc> must insert 'hello' 3 times; got {after_insert:?}"
    );

    dispatch_keys(&mut e, "5.");
    let after_repeat = e.content();
    assert_eq!(
        after_repeat.matches('h').count(),
        8,
        "5. must override the recorded count 3 with 5 (3 + 5 = 8 total 'hello' \
         insertions), got {after_repeat:?}"
    );
}

/// `2iX<Esc>` then `.` with NO explicit count must reuse the recorded count
/// of 2 (regression guard: the override must default to the recorded count,
/// not to 1 or 0, when the user types no count before `.`).
#[test]
fn dot_repeat_without_count_reuses_recorded_count() {
    let mut e = editor_with("");
    dispatch_keys(&mut e, "2iX<Esc>");
    assert!(
        e.content().starts_with("XX"),
        "2iX<Esc> must insert 'X' twice; got {:?}",
        e.content()
    );

    dispatch_keys(&mut e, ".");
    assert!(
        e.content().starts_with("XXXX"),
        "bare . must reuse the recorded count 2 (2 + 2 = 4 'X's); got {:?}",
        e.content()
    );
}

// ── VisualBlock `A` / `I` / `c` edge-column resolution (audit-r2) ──────────

fn lines_of(e: &Editor) -> Vec<String> {
    e.buffer()
        .rope()
        .lines()
        .map(|s| {
            let s = s.to_string();
            s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
        })
        .collect()
}

#[test]
fn block_append_pads_rows_shorter_than_the_top_row_to_the_block_edge() {
    // Fix 1: block `A`'s append column used to be clamped by the TOP row's
    // length alone, so on rows LONGER than the top row the typed text
    // landed inside the block instead of past its right edge. vim `v_b_A`
    // (`:h v_b_A`) pads every row shorter than the block's right edge to
    // reach it, then appends there — verified against `nvim --headless`.
    let mut e = editor_with("ab\nabcdef");
    e.jump_cursor(1, 5);
    dispatch_keys(&mut e, "<C-v>k");
    dispatch_keys(&mut e, "A");
    dispatch_keys(&mut e, "X<Esc>");
    assert_eq!(
        lines_of(&e),
        &["ab    X".to_string(), "abcdefX".to_string()]
    );
}

#[test]
fn block_highlight_delete_bridge_honors_ragged_flag() {
    // Regression: the REAL app never calls `apply_visual_operator` for
    // VisualBlock `d`/`y`/`c` — its keymap intercepts those keys with
    // `AppAction::VisualOp`, which reads `block_highlight()` (a static
    // (top,bot,left,right) snapshot) and calls `delete_block` /
    // `yank_block` / `change_block` (see apps/hjkl/src/app/engine_
    // actions.rs). Those bridges must still resolve a ragged (`$`) block
    // per row rather than reusing the snapshotted `right_col` — this is
    // the exact call shape `engine_actions.rs` uses.
    let mut e = editor_with("short\nmuchlongerline");
    dispatch_keys(&mut e, "l<C-v>$j");
    let (top, bot, left, right) = e.block_highlight().expect("in VisualBlock mode");
    e.delete_block(top, bot, left, right, '"');
    assert_eq!(lines_of(&e), &["s".to_string(), "m".to_string()]);
}

#[test]
fn block_dollar_delete_removes_to_each_rows_own_eol() {
    // Fix 3: `$` in VisualBlock makes the block ragged (`:h v_b_$`) — every
    // row deletes to ITS OWN EOL, not a fixed-width rectangle capped by
    // whichever row the cursor was on when `$` was pressed. Verified
    // against `nvim --headless`.
    let mut e = editor_with("short\nmuchlongerline");
    dispatch_keys(&mut e, "l<C-v>$jd");
    assert_eq!(lines_of(&e), &["s".to_string(), "m".to_string()]);
}

#[test]
fn block_insert_skips_rows_shorter_than_the_block_column() {
    // Fix 2: block `I` used to pad rows shorter than the block's left
    // column (same padding `A` uses). vim `v_b_I` (`:h v_b_I`) SKIPS those
    // rows entirely instead — no padding, no insert — verified against
    // `nvim --headless`.
    let mut e = editor_with("aaaa\nx\nbbbb");
    e.jump_cursor(0, 2);
    dispatch_keys(&mut e, "<C-v>jj");
    dispatch_keys(&mut e, "I");
    dispatch_keys(&mut e, "Z<Esc>");
    assert_eq!(
        lines_of(&e),
        &["aaZaa".to_string(), "x".to_string(), "bbZbb".to_string()]
    );
}

/// B1: an insert-mode ctrl-key combo with no dedicated binding (e.g.
/// `<C-b>`) must be a no-op, NOT insert the literal letter (the pre-fix
/// bug — `<C-a>` used to insert "a"). This intentionally diverges from
/// real nvim, which inserts the raw control byte for most unbound ctrl
/// keys (verified: `<C-b>` in nvim inserts a literal ^B) — chosen because
/// reproducing an unprintable control byte in the buffer
/// has no user-facing benefit and no-op is the safer choice.
#[test]
fn insert_unhandled_ctrl_key_is_noop_not_literal_letter() {
    let mut e = editor_with("hello\n");
    dispatch_keys(&mut e, "i<C-b>x<Esc>");
    assert_eq!(
        lines_of(&e),
        &["xhello".to_string(), String::new()],
        "unhandled ctrl key must not insert its letter literally"
    );
}

// ── macro recorder literal q (audit B1) ─────────────────────────────────

/// Recording must not drop literal `q` keys — the recorder's
/// `input.key != Key::Char('q')` clause was filtering every `q`,
/// including insert-mode text (`iquick<Esc>` recorded as `iuick<Esc>`)
/// and pending-operator targets (`fq` recorded as `f`).
#[test]
fn macro_records_literal_q_in_insert_mode() {
    let mut e = editor_with("\n");
    dispatch_keys(&mut e, "qaiquick<Esc>q");
    // Register "a must hold the encoded recording.
    let text = e
        .with_registers(|r| r.read('a').map(|slot| slot.text.clone()))
        .expect("recording must populate register a");
    assert_eq!(
        text, "iquick<Esc>",
        "insert-mode literal q must survive recording; got {text:?}"
    );
    // Replay on a fresh empty buffer — copy the register to a new editor
    // so the replay has the macro text available.
    let mut e2 = editor_with("\n");
    e2.with_registers_mut(|r| r.record_yank(text, false, Some('a')));
    dispatch_keys(&mut e2, "@a");
    assert_eq!(
        e2.content().trim_end_matches('\n'),
        "quick",
        "replay of register a must produce 'quick'; got {:?}",
        e2.content()
    );
}

/// When `f`/`t`/`F`/`T` targets the letter `q`, that target must be
/// recorded so the macro replays the correct search character instead of
/// desynchronising by reusing the next recorded key.
#[test]
fn macro_records_q_as_pending_target() {
    // `q` must sit off col 0 so `fq` has somewhere to move to — with the
    // cursor already on the `q`, a no-op `fq` would satisfy the cursor
    // assertion below vacuously.
    let mut e = editor_with("xq\n");
    dispatch_keys(&mut e, "qb0fqq");
    let text = e
        .with_registers(|r| r.read('b').map(|slot| slot.text.clone()))
        .expect("recording must populate register b");
    assert_eq!(
        text, "0fq",
        "pending-target q must survive recording; got {text:?}"
    );
    // The cursor should have landed on 'q' during recording.
    assert_eq!(
        e.cursor(),
        (0, 1),
        "fq must land on the q at col 1 while recording; got {:?}",
        e.cursor()
    );
}

// ── J join no-space exceptions (audit B8) ──────────────────────────────

/// `J` with a first line ending in whitespace must not insert an extra
/// space (vim `:h J` exception #1).
#[test]
fn join_trailing_whitespace_no_extra_space() {
    let mut e = editor_with("hello \nworld");
    dispatch_keys(&mut e, "J");
    assert_eq!(e.content(), "hello world\n", "trailing space on first line");
}

/// `J` with a first line ending in tab must not insert an extra space.
#[test]
fn join_trailing_tab_no_extra_space() {
    let mut e = editor_with("hello\t\nworld");
    dispatch_keys(&mut e, "J");
    assert_eq!(e.content(), "hello\tworld\n", "trailing tab on first line");
}

/// `J` with a second line starting with `)` must not insert a space
/// (vim `:h J` exception #2).
#[test]
fn join_leading_paren_no_space() {
    let mut e = editor_with("hello\n)world");
    dispatch_keys(&mut e, "J");
    assert_eq!(e.content(), "hello)world\n", "leading ) on second line");
}

/// `J` with both lines non-empty and no exception conditions inserts a
/// single space (the baseline behaviour must remain).
#[test]
fn join_plain_inserts_space() {
    let mut e = editor_with("hello\nworld");
    dispatch_keys(&mut e, "J");
    assert_eq!(e.content(), "hello world\n");
}

/// Visual hex increment: `Vg<C-a>` on `0xFF` must produce `0x100`,
/// matching nvim behaviour.
#[test]
fn visual_hex_increment() {
    let mut e = editor_with("0xFF");
    // Enter linewise visual on the row, then g<C-a> for hex increment.
    dispatch_keys(&mut e, "Vg<C-a>");
    assert_eq!(e.content(), "0x100\n");
}

/// Visual hex decrement: `Vg<C-x>` on `0x10` must produce `0x0f`,
/// preserving the hex digit width.
#[test]
fn visual_hex_decrement() {
    let mut e = editor_with("0x10");
    dispatch_keys(&mut e, "Vg<C-x>");
    assert_eq!(e.content(), "0x0f\n");
}

/// Hex digit case follows the *last* letter digit of the original, in both
/// normal and visual mode. Expectations checked against nvim 0.12.4.
#[test]
fn hex_increment_keeps_digit_case() {
    for (before, after) in [
        ("0xAB", "0xAC"),
        ("0xab", "0xac"),
        // Mixed case: the last letter digit wins over the first.
        ("0xaB", "0xAC"),
        ("0xAb", "0xac"),
        ("0xDEADBEEF", "0xDEADBEF0"),
        ("0xdeadbeef", "0xdeadbef0"),
    ] {
        let mut normal = editor_with(before);
        dispatch_keys(&mut normal, "<C-a>");
        assert_eq!(normal.content(), format!("{after}\n"), "normal {before}");

        let mut visual = editor_with(before);
        dispatch_keys(&mut visual, "V<C-a>");
        assert_eq!(visual.content(), format!("{after}\n"), "visual {before}");
    }
}

/// With no letter digit to copy the case from, vim falls back to the case of
/// the `x`/`X` prefix itself: `0X19` <C-a> -> `0X1A`, `0x19` -> `0x1a`.
#[test]
fn hex_increment_falls_back_to_prefix_case() {
    for (before, after) in [
        ("0X19", "0X1A"),
        ("0x19", "0x1a"),
        ("0X1", "0X2"),
        ("0xF", "0x10"),
    ] {
        let mut normal = editor_with(before);
        dispatch_keys(&mut normal, "<C-a>");
        assert_eq!(normal.content(), format!("{after}\n"), "normal {before}");

        let mut visual = editor_with(before);
        dispatch_keys(&mut visual, "V<C-a>");
        assert_eq!(visual.content(), format!("{after}\n"), "visual {before}");
    }
}

/// Visual `<C-a>`/`<C-x>` must zero-pad exactly like normal mode does: pad
/// back to the original digit width only when the original led with a zero,
/// and never count the `-` sign in that width.
#[test]
fn visual_decimal_preserves_zero_padding() {
    for (before, keys, after) in [
        ("007", "V<C-a>", "008"),
        ("-007", "V<C-a>", "-006"),
        ("09", "V<C-a>", "10"),
        // No leading zero: no padding, so a shrinking number stays short.
        ("10", "V<C-x>", "9"),
        // Crossing zero still pads the digits, sign excluded.
        ("009", "V20<C-x>", "-011"),
    ] {
        let mut visual = editor_with(before);
        dispatch_keys(&mut visual, keys);
        assert_eq!(visual.content(), format!("{after}\n"), "visual {before}");
    }
}

/// `g<C-a>` scales the increment by the number of numbers adjusted so far.
/// Rows without a number must not consume a step of that sequence.
#[test]
fn visual_sequential_increment_skips_numberless_rows() {
    let mut e = editor_with("1\nno digits here\n1\n1");
    dispatch_keys(&mut e, "VGg<C-a>");
    assert_eq!(e.content(), "2\nno digits here\n3\n4\n");
}

/// Blockwise paste geometry, pinned before `do_block_paste` was rewritten off
/// the whole-document `Vec<String>` rebuild. Expectations are vim's semantics
/// (`:h v_b_p`), not a snapshot of hjkl's output, so a divergence surfaces as a
/// failure rather than being encoded.
#[test]
fn block_paste_geometry() {
    // (buffer, cursor, register, width, keys, expected)
    for (buf, cur, reg, width, keys, want) in [
        // `P` inserts AT the cursor column, `p` after it.
        (
            "abcd\nefgh\nijkl",
            (0, 1),
            "XY\nZW",
            2,
            "P",
            "aXYbcd\neZWfgh\nijkl\n",
        ),
        (
            "abcd\nefgh\nijkl",
            (0, 1),
            "XY\nZW",
            2,
            "p",
            "abXYcd\nefZWgh\nijkl\n",
        ),
        // Rows past the buffer end are created.
        ("ab", (0, 0), "XY\nZW", 2, "P", "XYab\nZW\n"),
        // A row shorter than the insert column is space-padded up to it.
        (
            "abcd\ne\nijkl",
            (0, 2),
            "XY\nZW",
            2,
            "P",
            "abXYcd\ne ZW\nijkl\n",
        ),
        // At end-of-line vim adds no trailing padding to a short segment.
        ("ab\ncd", (0, 1), "X\nY", 3, "p", "abX\ncdY\n"),
        // A count repeats each segment horizontally.
        (
            "abcd\nefgh",
            (0, 1),
            "XY\nZW",
            2,
            "2P",
            "aXYXYbcd\neZWZWfgh\n",
        ),
    ] {
        let mut e = editor_with(buf);
        e.jump_cursor(cur.0, cur.1);
        e.record_yank_block(reg.to_string(), width, None);
        dispatch_keys(&mut e, keys);
        assert_eq!(e.content(), want, "{keys} on {buf:?} at {cur:?}");
    }
}

/// A blockwise paste must undo to exactly the pre-paste buffer — including the
/// space padding it inserted into ragged rows, which is carried by the
/// `DeleteBlockChunks` inverse's `pads` field rather than recomputed.
#[test]
fn block_paste_undo_round_trips_including_padding() {
    for (buf, cur, reg, width) in [
        ("abcd\nefgh\nijkl", (0, 1), "XY\nZW", 2),
        // Ragged: row 1 gets padded out to the insert column.
        ("abcd\ne\nijkl", (0, 2), "XY\nZW", 2),
        // Past EOF: rows are created by the paste.
        ("ab", (0, 0), "XY\nZW", 2),
    ] {
        let mut e = editor_with(buf);
        let before = e.content();
        e.jump_cursor(cur.0, cur.1);
        e.record_yank_block(reg.to_string(), width, None);
        dispatch_keys(&mut e, "P");
        assert_ne!(e.content(), before, "paste must change {buf:?}");
        e.undo();
        assert_eq!(e.content(), before, "undo must restore {buf:?}");
    }
}

/// One blockwise paste is one undo step, matching the counted-linewise case.
#[test]
fn block_paste_is_one_undoable_edit() {
    let mut e = editor_with("ab");
    e.record_yank_block("XY\nZW".to_string(), 2, None);
    dispatch_keys(&mut e, "P");
    assert_eq!(e.content(), "XYab\nZW\n");
    e.undo();
    assert_eq!(e.content(), "ab\n");
}

/// `p` / `P` paste raw: the register's own leading whitespace is preserved
/// verbatim and `autoindent` must not add to it. Vim only reindents for the
/// explicit `]p` / `[p` forms.
#[test]
fn paste_is_raw_regardless_of_autoindent() {
    for ai in [false, true] {
        for keys in ["p", "P", "2p"] {
            let mut e = editor_with("        fn outer() {");
            e.settings_mut().autoindent = ai;
            e.with_registers_mut(|r| {
                r.record_yank("    let x = 1;\n\tlet y = 2;\n".to_string(), true, None)
            });
            dispatch_keys(&mut e, keys);
            let body: String = e
                .content()
                .lines()
                .filter(|l| l.contains("let"))
                .collect::<Vec<_>>()
                .join("|");
            let want = if keys == "2p" {
                "    let x = 1;|\tlet y = 2;|    let x = 1;|\tlet y = 2;"
            } else {
                "    let x = 1;|\tlet y = 2;"
            };
            assert_eq!(body, want, "autoindent={ai} keys={keys}");
        }
    }
}

/// A block paste must not change whether the buffer ends in a newline.
///
/// `content()` appends a trailing newline when the rope lacks one, so this is
/// invisible through it — which is why the geometry cases above missed the
/// regression. Read the rope directly.
///
/// Regression: opening rows past EOF spliced into ropey's phantom trailing
/// line (the newline terminator) instead of past it, so a terminated buffer
/// came back unterminated. Caught by the nvim oracle's
/// `visual_block_paste_past_eof`, which also bypasses `content()`.
#[test]
fn block_paste_past_eof_preserves_the_trailing_newline() {
    for seed in ["abcd\nefgh\nijkl\n", "abcd\nefgh\nijkl"] {
        let mut e = editor_with(seed);
        let before = hjkl_engine::types::Query::rope(e.buffer())
            .to_string()
            .ends_with('\n');
        e.jump_cursor(2, 1);
        e.record_yank_block("ab\n ef\n ij".to_string(), 3, None);

        dispatch_keys(&mut e, "P");

        let text = hjkl_engine::types::Query::rope(e.buffer()).to_string();
        assert_eq!(
            text.ends_with('\n'),
            before,
            "paste changed the terminator for seed {seed:?}: {text:?}"
        );
    }
}

/// `"a` before a change names the register the change writes, and `.` must
/// reuse it rather than falling back to the unnamed one (`:h redo-register`).
/// Each expectation below was taken from neovim 0.12.4 with
/// `nvim --headless -u NONE -c 'normal! <keys>'` over the same seed.
#[test]
fn dot_repeat_reuses_the_explicit_register() {
    // `unnamed` is loaded with something distinguishable first, so a fallback
    // to it is visible in the register rather than only in the buffer.
    for (keys, want_a) in [
        ("\"adw.", "bravo "),
        ("\"adiw.", " "),
        ("\"aD.", "alpha bravo charlie delta"),
        ("\"adfa.", " bra"),
    ] {
        let mut e = editor_with("alpha bravo charlie delta\nsecond line here");
        let got = dispatch_and_read_reg_a(&mut e, keys);
        assert_eq!(got, want_a, "register a after {keys:?}");
    }
}

/// Same rule for `x` / `X`. The live path already honoured `"a`;
/// `LastChange::CharDel` carried no register, so the `.` deleted into the
/// unnamed one and left `"a` holding the FIRST deletion. Every expectation
/// measured on neovim 0.12.4 over `"abcdef"`.
#[test]
fn dot_repeat_char_delete_reuses_the_explicit_register() {
    // `"axl.` — delete `a`, step right, repeat: `"a` ends up with `c`.
    let mut e = editor_with("abcdef");
    assert_eq!(dispatch_and_read_reg_a(&mut e, "\"axl."), "c");
    assert_eq!(e.content(), "bdef\n");

    // Backward `X` repeats the same way: `"aX` at `d` takes `c`, then `.`
    // one column on takes `b`.
    let mut e = editor_with("abcdef");
    dispatch_keys(&mut e, "lll");
    assert_eq!(dispatch_and_read_reg_a(&mut e, "\"aX."), "b");
    assert_eq!(e.content(), "adef\n");

    // A counted `x` carries its count into the repeat, register and all.
    let mut e = editor_with("abcdef");
    assert_eq!(dispatch_and_read_reg_a(&mut e, "\"a2x."), "cd");
    assert_eq!(e.content(), "ef\n");
}

/// `"x` picks a register in visual mode too, right before the operator. The
/// chord was Normal-only, so `"` fell through, `a` armed the around-text-object
/// chord, and the operator was eaten as that chord's target: the buffer was
/// untouched and the selection stayed up. Expectations from neovim 0.12.4.
#[test]
fn visual_mode_takes_an_explicit_register() {
    // Charwise delete.
    let mut e = editor_with("alpha bravo charlie");
    assert_eq!(dispatch_and_read_reg_a(&mut e, "vll\"ad"), "alp");
    assert_eq!(e.content(), "ha bravo charlie\n");

    // Charwise yank leaves the buffer alone but still fills `"a`.
    let mut e = editor_with("alpha bravo charlie");
    assert_eq!(dispatch_and_read_reg_a(&mut e, "vll\"ay"), "alp");
    assert_eq!(e.content(), "alpha bravo charlie\n");

    // Linewise.
    let mut e = editor_with("one\ntwo\nthree\nfour");
    assert_eq!(dispatch_and_read_reg_a(&mut e, "V\"ad"), "one\n");
    assert_eq!(e.content(), "two\nthree\nfour\n");
}

/// A visual operator's register survives into its dot-repeat. The repeat runs
/// over a same-size region at the cursor (`:h v_.`), so each pass deletes
/// different text and a fallback to the unnamed register leaves `"a` holding
/// the FIRST rectangle. Expectations from neovim 0.12.4.
#[test]
fn visual_dot_repeat_reuses_the_explicit_register() {
    // Charwise: three chars, step a word right, repeat.
    let mut e = editor_with("alpha bravo charlie");
    assert_eq!(dispatch_and_read_reg_a(&mut e, "vll\"adw."), "bra");
    assert_eq!(e.content(), "ha vo charlie\n");

    // Linewise.
    let mut e = editor_with("one\ntwo\nthree\nfour");
    assert_eq!(dispatch_and_read_reg_a(&mut e, "V\"adj."), "three\n");
    assert_eq!(e.content(), "two\nfour\n");
}

/// `"aD` / `"aC` write the named register at all — they used to reach only the
/// unnamed one. nvim: `"aD` on `"alpha bravo"` leaves `a` holding the line.
#[test]
fn delete_to_eol_honours_an_explicit_register() {
    let mut e = editor_with("alpha bravo");
    assert_eq!(dispatch_and_read_reg_a(&mut e, "\"aD"), "alpha bravo");

    let mut e = editor_with("alpha bravo");
    assert_eq!(dispatch_and_read_reg_a(&mut e, "\"aC"), "alpha bravo");
}

/// `"ap` then `.` pastes register `a` twice. nvim leaves
/// `"alpha bravo\nalpha alpha "`; the unnamed register holds `"bravo"`, so a
/// fallback shows up as `"alpha bravo"` on the second row.
#[test]
fn dot_repeat_paste_reuses_the_explicit_register() {
    let mut e = editor_with("alpha bravo\n\n");
    dispatch_keys(&mut e, "\"aywwyw");
    dispatch_keys(&mut e, "j\"ap.");
    assert_eq!(e.content(), "alpha bravo\nalpha alpha \n\n");
}

/// A row ropey ended with a separator other than `\n` used to read one char
/// longer than its content, because `rope_line_str` stripped a literal `'\n'`
/// and nothing else. `j` then clamped against that inflated length, and the
/// debug curswant invariant fired: "row 1 has 2 chars, so 2 is not a vertical
/// clamp either" — while row 1's real content is the single `\t`.
///
/// Minimized by `cargo fuzz tmin` from a `handle_key` crash; `*` is only here
/// because it is what puts the cursor on column 2 with `sticky_col` 2.
#[test]
fn vertical_clamp_ignores_a_non_newline_line_separator() {
    let mut e = editor_with("\0/kkk\r\t\r\u{e}\0??kk");
    // ropey splits on the lone `\r`; the row's content excludes it.
    assert_eq!(e.line(1).as_deref(), Some("\t"));
    dispatch_keys(&mut e, "*");
    assert_eq!(e.cursor(), (0, 2));
    // Row 1 holds one char, so the clamp lands on column 0 — and the invariant
    // must agree, rather than measuring the row as two chars wide.
    dispatch_keys(&mut e, "<Down>");
    assert_eq!(e.cursor(), (1, 0));
}

/// Run `keys` and read `"a` back out.
fn dispatch_and_read_reg_a(e: &mut Editor, keys: &str) -> String {
    dispatch_keys(e, keys);
    e.with_registers(|r| r.named[0].text.clone())
}
