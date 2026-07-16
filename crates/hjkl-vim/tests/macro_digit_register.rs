//! `q{0-9}` macro recordings are playable, not silently discarded
//! (audit-r2 fix 5).
//!
//! Vim's `:h q` accepts `q{0-9a-zA-Z"}` as a recording target — digits
//! included, shadowing whatever the delete/change ring held there.
//! Before this fix, `stop_macro_record` -> `set_named_register_text` only
//! wrote `'a'..='z'` / `'A'..='Z'` slots, so `q1 ... q` silently dropped
//! the recording and `@1` replayed nothing.

use hjkl_engine::{Editor, Input, Key};

fn editor_with(content: &str) -> Editor {
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::new(),
        hjkl_engine::DefaultHost::new(),
        hjkl_engine::Options::default(),
    );
    e.set_content(content);
    e
}

fn press(e: &mut Editor, ch: char) {
    hjkl_vim::dispatch_input(
        e,
        Input {
            key: Key::Char(ch),
            ctrl: false,
            alt: false,
            shift: false,
        },
    );
}

fn press_keys(e: &mut Editor, keys: &str) {
    for ch in keys.chars() {
        press(e, ch);
    }
}

#[test]
fn q1_records_and_at_1_replays() {
    let mut e = editor_with("aaaa\n");

    // `q1` — start recording into register "1". `x` deletes the char
    // under the cursor. `q` — stop recording.
    press_keys(&mut e, "q1xq");
    assert_eq!(
        e.content().lines().next(),
        Some("aaa"),
        "sanity: the recorded `x` actually ran once while recording"
    );

    // `@1` replays the recording — one more `x`.
    press_keys(&mut e, "@1");
    assert_eq!(
        e.content().lines().next(),
        Some("aa"),
        "`@1` must replay the q1 recording (another `x`)"
    );
}

#[test]
fn q1_register_readable_directly_after_recording() {
    let mut e = editor_with("aaaa\n");
    press_keys(&mut e, "q1xq");

    let text = e
        .with_registers(|r| r.read('1').map(|slot| slot.text.clone()))
        .unwrap_or_default();
    assert!(
        !text.is_empty(),
        "register \"1 must hold the encoded q1 recording, not be silently dropped"
    );
}

#[test]
fn at_1_is_a_noop_before_anything_records_into_it() {
    // Sanity/negative check: `@1` on an empty register 1 doesn't panic and
    // doesn't mutate the buffer.
    let mut e = editor_with("aaaa\n");
    press_keys(&mut e, "@1");
    assert_eq!(e.content().lines().next(), Some("aaaa"));
}
