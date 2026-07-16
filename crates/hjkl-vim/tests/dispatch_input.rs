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
