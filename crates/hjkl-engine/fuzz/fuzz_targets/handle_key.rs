//! Fuzz target: feed an arbitrary keystroke stream into a fresh
//! [`hjkl_engine::Editor`] and assert it never panics.
//!
//! Inputs are decoded from raw bytes via `arbitrary`; the harness
//! seeds the editor with a non-empty buffer so motion + delete paths
//! actually run, then dispatches each keystroke. Final state is
//! ignored — we're only interested in panics.

#![no_main]

use arbitrary::{Arbitrary, Unstructured};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hjkl_engine::Editor;
use hjkl_engine::types::{DefaultHost, Options};
use libfuzzer_sys::fuzz_target;

#[derive(Debug, Arbitrary)]
enum FuzzKey {
    Char(char),
    Esc,
    Enter,
    Backspace,
    Tab,
    BackTab,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    F(u8),
}

#[derive(Debug, Arbitrary)]
struct FuzzMods {
    ctrl: bool,
    shift: bool,
    alt: bool,
}

impl From<FuzzMods> for KeyModifiers {
    fn from(m: FuzzMods) -> Self {
        let mut k = KeyModifiers::NONE;
        if m.ctrl {
            k |= KeyModifiers::CONTROL;
        }
        if m.shift {
            k |= KeyModifiers::SHIFT;
        }
        if m.alt {
            k |= KeyModifiers::ALT;
        }
        k
    }
}

#[derive(Debug, Arbitrary)]
struct FuzzInput {
    seed_text: String,
    keys: Vec<(FuzzKey, FuzzMods)>,
}

fn to_keycode(k: FuzzKey) -> KeyCode {
    match k {
        FuzzKey::Char(c) => KeyCode::Char(c),
        FuzzKey::Esc => KeyCode::Esc,
        FuzzKey::Enter => KeyCode::Enter,
        FuzzKey::Backspace => KeyCode::Backspace,
        FuzzKey::Tab => KeyCode::Tab,
        FuzzKey::BackTab => KeyCode::BackTab,
        FuzzKey::Up => KeyCode::Up,
        FuzzKey::Down => KeyCode::Down,
        FuzzKey::Left => KeyCode::Left,
        FuzzKey::Right => KeyCode::Right,
        FuzzKey::Home => KeyCode::Home,
        FuzzKey::End => KeyCode::End,
        FuzzKey::PageUp => KeyCode::PageUp,
        FuzzKey::PageDown => KeyCode::PageDown,
        FuzzKey::Insert => KeyCode::Insert,
        FuzzKey::Delete => KeyCode::Delete,
        FuzzKey::F(n) => KeyCode::F(n.min(12)),
    }
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);
    let Ok(input) = FuzzInput::arbitrary(&mut u) else {
        return;
    };

    let mut ed = Editor::new(
        hjkl_buffer::Buffer::new(),
        DefaultHost::new(),
        Options::default(),
    );
    // Seed with the arbitrary text so motions / deletes have something
    // to chew on. Trim to a reasonable size so the bench runs fast.
    let seed = if input.seed_text.len() > 1024 {
        &input.seed_text[..1024]
    } else {
        &input.seed_text
    };
    ed.set_content(seed);

    // Bound the dispatch length too — without this the fuzzer can
    // construct multi-MB inputs that timeout the run.
    for (key, mods) in input.keys.into_iter().take(256) {
        let code = to_keycode(key);
        let modifiers: KeyModifiers = mods.into();
        let _ = ed.handle_key(KeyEvent::new(code, modifiers));
    }

    // Drain any buffered effects so internal caches exercise too.
    let _ = ed.take_content_change();
});
