//! Property-based FSM invariants.
//!
//! Random key sequences fed into [`Editor::handle_key`] must:
//!
//! - Never panic.
//! - Always leave the editor in a legal [`VimMode`].
//! - Preserve content stability when the sequence is `Esc, Esc, Esc`
//!   (or any other no-op cleanup sequence).
//!
//! First building block toward a `cargo fuzz` harness.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hjkl_engine::{Editor, KeybindingMode, VimMode};
use proptest::prelude::*;

fn keycode_strategy() -> impl Strategy<Value = KeyCode> {
    prop_oneof![
        prop::char::range('a', 'z').prop_map(KeyCode::Char),
        prop::char::range('A', 'Z').prop_map(KeyCode::Char),
        prop::char::range('0', '9').prop_map(KeyCode::Char),
        Just(KeyCode::Esc),
        Just(KeyCode::Enter),
        Just(KeyCode::Backspace),
        Just(KeyCode::Tab),
        Just(KeyCode::Up),
        Just(KeyCode::Down),
        Just(KeyCode::Left),
        Just(KeyCode::Right),
        // Punctuation that vim treats as operators / motions.
        Just(KeyCode::Char(':')),
        Just(KeyCode::Char('/')),
        Just(KeyCode::Char('?')),
        Just(KeyCode::Char('w')),
        Just(KeyCode::Char('b')),
        Just(KeyCode::Char('e')),
        Just(KeyCode::Char('$')),
        Just(KeyCode::Char('^')),
        Just(KeyCode::Char('0')),
        Just(KeyCode::Char('.')),
    ]
}

fn modifiers_strategy() -> impl Strategy<Value = KeyModifiers> {
    prop_oneof![
        Just(KeyModifiers::NONE),
        Just(KeyModifiers::CONTROL),
        Just(KeyModifiers::SHIFT),
        Just(KeyModifiers::ALT),
    ]
}

fn key_event_strategy() -> impl Strategy<Value = KeyEvent> {
    (keycode_strategy(), modifiers_strategy()).prop_map(|(c, m)| KeyEvent::new(c, m))
}

fn key_sequence_strategy() -> impl Strategy<Value = Vec<KeyEvent>> {
    prop::collection::vec(key_event_strategy(), 0..32)
}

fn legal_mode(m: VimMode) -> bool {
    matches!(
        m,
        VimMode::Normal
            | VimMode::Insert
            | VimMode::Visual
            | VimMode::VisualLine
            | VimMode::VisualBlock
    )
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        ..ProptestConfig::default()
    })]

    /// Editor never panics on random keystroke sequences and always
    /// settles in a legal mode.
    #[test]
    fn no_panic_on_random_keys(seq in key_sequence_strategy()) {
        let mut ed = Editor::new(KeybindingMode::Vim);
        ed.set_content("hello world\nsecond line\n");
        for k in seq {
            let _ = ed.handle_key(k);
        }
        prop_assert!(legal_mode(ed.vim_mode()));
    }

    /// Multiple `Esc` keystrokes drop the editor back into Normal mode
    /// regardless of starting state.
    #[test]
    fn esc_returns_to_normal(prefix in key_sequence_strategy()) {
        let mut ed = Editor::new(KeybindingMode::Vim);
        ed.set_content("hello\nworld\n");
        for k in prefix {
            let _ = ed.handle_key(k);
        }
        // Three Escapes should pop any nested mode.
        for _ in 0..3 {
            ed.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        }
        prop_assert_eq!(ed.vim_mode(), VimMode::Normal);
    }
}

#[test]
fn handle_key_no_panic_baseline() {
    let mut ed = Editor::new(KeybindingMode::Vim);
    ed.set_content("hello");
    for k in [KeyCode::Char('i'), KeyCode::Char('x'), KeyCode::Esc] {
        ed.handle_key(KeyEvent::new(k, KeyModifiers::NONE));
    }
    assert_eq!(ed.vim_mode(), VimMode::Normal);
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 128,
        ..ProptestConfig::default()
    })]

    /// Yank-then-paste round-trip: after `yy` (yank line) on row 0
    /// followed by `p` (paste below), the original line content
    /// appears at least twice in the buffer. Property: yank/paste
    /// preserves the source line text byte-for-byte.
    #[test]
    fn yy_then_p_duplicates_line(text in "[a-z]{1,15}") {
        let mut ed = Editor::new(KeybindingMode::Vim);
        ed.set_content(&text);
        // `yy` then `p`.
        ed.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        ed.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        ed.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE));
        let content = ed.content();
        prop_assert!(
            content.matches(text.as_str()).count() >= 2,
            "expected text {text:?} twice in {content:?}"
        );
    }

    /// `dd` followed by `u` (undo) restores the buffer. Property:
    /// the undo stack reverses the most recent edit fully.
    #[test]
    fn dd_then_u_restores(
        line0 in "[a-z]{1,12}",
        line1 in "[a-z]{1,12}",
    ) {
        let original = format!("{line0}\n{line1}");
        let mut ed = Editor::new(KeybindingMode::Vim);
        ed.set_content(&original);
        let before: Vec<String> = ed.buffer().lines().to_vec();
        // `dd` deletes the first line.
        ed.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        ed.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        // `u` undoes.
        ed.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
        prop_assert_eq!(before, ed.buffer().lines().to_vec());
    }

    /// take_changes drains: a second call after the first returns
    /// empty no matter what edit sequence preceded it.
    #[test]
    fn take_changes_is_idempotent(
        text in "[a-z]{1,15}",
        edits in 0u32..5,
    ) {
        let mut ed = Editor::new(KeybindingMode::Vim);
        ed.set_content(&text);
        // Enter insert mode, type some chars, exit.
        ed.handle_key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        for i in 0..edits {
            let c = char::from(b'a' + (i as u8 % 26));
            ed.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        ed.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        // First drain may or may not have entries.
        let _first = ed.take_changes();
        // Second drain must always be empty.
        prop_assert!(ed.take_changes().is_empty());
    }

    /// `apply_options(current_options())` is a fixed-point on the
    /// fields the engine actually backs today.
    #[test]
    fn options_round_trip(
        ts in 1u32..32,
        sw in 1u32..32,
        tw in 1u32..200,
        et in any::<bool>(),
        ic in any::<bool>(),
        wrap_idx in 0u8..3,
    ) {
        use hjkl_engine::{Options, WrapMode};
        let mut ed = Editor::new(KeybindingMode::Vim);
        ed.set_content("hello\n");
        let opts = Options {
            tabstop: ts,
            shiftwidth: sw,
            textwidth: tw,
            expandtab: et,
            ignorecase: ic,
            wrap: match wrap_idx {
                0 => WrapMode::None,
                1 => WrapMode::Char,
                _ => WrapMode::Word,
            },
            ..Options::default()
        };
        ed.apply_options(&opts);
        let echoed = ed.current_options();
        prop_assert_eq!(echoed.tabstop, opts.tabstop);
        prop_assert_eq!(echoed.shiftwidth, opts.shiftwidth);
        prop_assert_eq!(echoed.textwidth, opts.textwidth);
        prop_assert_eq!(echoed.expandtab, opts.expandtab);
        prop_assert_eq!(echoed.ignorecase, opts.ignorecase);
        prop_assert_eq!(echoed.wrap, opts.wrap);
    }
}
