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
