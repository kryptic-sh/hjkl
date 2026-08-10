//! Shared test helpers for hjkl integration tests.
//!
//! The `feed` / `feed_insert` keystroke mappers were forked four times across
//! the integration test files (tag_rename, autopair, comment_continuation,
//! smartindent_html), each with a drifted key set. This module holds the union
//! of those encodings so a key maps the same way everywhere:
//! `\n` → Enter, `\x08` → Backspace, `\x1b` → Esc, anything else → Char.

use hjkl_buffer::View;
use hjkl_engine::{DefaultHost, Editor, Input, Key};
use hjkl_vim::{dispatch_input, insert::step_insert};

/// Encode a single keystroke char as the engine `Input` it represents.
fn char_input(ch: char) -> Input {
    let key = match ch {
        '\n' => Key::Enter,
        '\x08' => Key::Backspace,
        '\x1b' => Key::Esc,
        _ => Key::Char(ch),
    };
    Input {
        key,
        ..Input::default()
    }
}

/// Feed keystrokes through `dispatch_input` (the hjkl-vim normal/insert FSM).
pub fn feed(ed: &mut Editor<View, DefaultHost>, keys: &str) {
    for ch in keys.chars() {
        dispatch_input(ed, char_input(ch));
    }
}

/// Feed keystrokes directly to the insert-mode FSM (`step_insert`).
pub fn feed_insert(ed: &mut Editor<View, DefaultHost>, keys: &str) {
    for ch in keys.chars() {
        step_insert(ed, char_input(ch));
    }
}
