//! Key event kinds at the terminal read boundary (`App::handle_key_event`).
//!
//! The Windows console reports a `KeyEventKind::Release` for every key-up, so
//! each keystroke arrives there as a Press + Release pair. Acting on the
//! release handled every key twice: `:qa` typed `::qqaa`, Backspace deleted
//! two chars. These tests feed Windows-shaped event streams through the same
//! method both `run()` read arms use, so they guard the fix on every platform
//! CI runs, not only on a Windows console.

use super::*;
use ttyr::event::{KeyEncoding, KeyEventKind, ModifierKeyCode};

fn with_kind(code: KeyCode, kind: KeyEventKind) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE).with_kind(kind)
}

/// One physical keystroke as the Windows console delivers it.
fn tap(app: &mut App, code: KeyCode) {
    app.handle_key_event(with_kind(code, KeyEventKind::Press));
    app.handle_key_event(with_kind(code, KeyEventKind::Release));
}

fn tap_chars(app: &mut App, s: &str) {
    for c in s.chars() {
        tap(app, KeyCode::Char(c));
    }
}

fn line0(app: &App) -> String {
    hjkl_buffer::rope_line_str(&app.active_editor().buffer().rope(), 0)
}

#[test]
fn release_events_do_not_double_command_prompt_input() {
    let mut app = App::new(None, false, None, None).unwrap();
    tap_chars(&mut app, ":qa");
    let field = app
        .command_field
        .as_ref()
        .expect("`:` must open the command prompt");
    assert_eq!(field.text(), "qa");

    tap(&mut app, KeyCode::Backspace);
    let field = app.command_field.as_ref().expect("prompt stays open");
    assert_eq!(field.text(), "q", "one Backspace deletes one char");
}

#[test]
fn release_events_do_not_double_insert_mode_input() {
    let mut app = App::new(None, false, None, None).unwrap();
    tap(&mut app, KeyCode::Char('i'));
    assert_eq!(app.active_editor().vim_mode(), VimMode::Insert);

    tap_chars(&mut app, "abc");
    assert_eq!(line0(&app), "abc");

    tap(&mut app, KeyCode::Backspace);
    assert_eq!(line0(&app), "ab", "one Backspace deletes one char");

    tap(&mut app, KeyCode::Esc);
    assert_eq!(app.active_editor().vim_mode(), VimMode::Normal);
}

/// A release on its own must be a no-op in Normal mode too — `x` released
/// must not delete a char.
#[test]
fn lone_release_event_is_ignored() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "abc");
    app.handle_key_event(with_kind(KeyCode::Char('x'), KeyEventKind::Release));
    assert_eq!(line0(&app), "abc");
}

/// Holding a key reports `Repeat` (Windows auto-repeat arrives as further
/// presses; kitty's REPORT_EVENT_TYPES as `Repeat`). Repeats must keep acting.
#[test]
fn repeat_events_still_act() {
    let mut app = App::new(None, false, None, None).unwrap();
    tap(&mut app, KeyCode::Char('i'));
    app.handle_key_event(with_kind(KeyCode::Char('z'), KeyEventKind::Press));
    app.handle_key_event(with_kind(KeyCode::Char('z'), KeyEventKind::Repeat));
    app.handle_key_event(with_kind(KeyCode::Char('z'), KeyEventKind::Repeat));
    app.handle_key_event(with_kind(KeyCode::Char('z'), KeyEventKind::Release));
    assert_eq!(line0(&app), "zzz");
}

/// One step of a keystroke script.
enum Stroke {
    /// A key typed without modifiers.
    Key(char),
    /// An uppercase letter typed with Shift.
    Shifted(char),
    /// A symbol typed with Shift: the key's own character and the one it
    /// typed.
    Symbol(char, char),
    Enter,
    /// A lock key tapped on its own (Caps Lock, Num Lock, Scroll Lock),
    /// which only the Windows shape reports.
    Lock(KeyCode),
}

/// Runs `script` from `text` and reports the buffer, cursor and mode.
///
/// With `windows`, shifted keys come as ttyr reads them from the Windows
/// console: Shift's own press and release around a shifted letter, and a
/// shifted symbol as the key's own character with the typed one as
/// `shifted`. Without it, each comes as just the typed character.
fn run_script(text: &str, script: &[Stroke], windows: bool) -> (String, (usize, usize), VimMode) {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, text);
    let shift = KeyCode::Modifier(ModifierKeyCode::LeftShift);
    for stroke in script {
        match *stroke {
            Stroke::Key(c) => tap(&mut app, KeyCode::Char(c)),
            Stroke::Enter => tap(&mut app, KeyCode::Enter),
            Stroke::Lock(code) => {
                if windows {
                    tap(&mut app, code);
                }
            }
            Stroke::Symbol(base, typed) => {
                let key = if windows {
                    KeyEvent::new(KeyCode::Char(base), KeyModifiers::SHIFT)
                        .with_shifted(Some(typed))
                        .with_encoding(KeyEncoding::Windows)
                } else {
                    KeyEvent::new(KeyCode::Char(typed), KeyModifiers::SHIFT)
                };
                app.handle_key_event(key.clone().with_kind(KeyEventKind::Press));
                app.handle_key_event(key.with_kind(KeyEventKind::Release));
            }
            Stroke::Shifted(c) => {
                if windows {
                    app.handle_key_event(with_kind(shift, KeyEventKind::Press));
                }
                let key = KeyEvent::new(KeyCode::Char(c), KeyModifiers::SHIFT);
                app.handle_key_event(key.clone().with_kind(KeyEventKind::Press));
                app.handle_key_event(key.with_kind(KeyEventKind::Release));
                if windows {
                    app.handle_key_event(with_kind(shift, KeyEventKind::Release));
                }
            }
        }
    }
    let rope = app.active_editor().buffer().rope();
    (
        rope.to_string(),
        app.active_editor().cursor(),
        app.active_editor().vim_mode(),
    )
}

/// A modifier or lock key pressed on its own is not a key vim sees, so it
/// must change nothing about what the keys around it do: not end a pending
/// operator, fill a key that `f`, `r`, `"`, `q`, `@` or `m` waits for, or
/// reach Insert mode or the command prompt as input.
#[test]
fn lone_modifier_presses_change_nothing() {
    use Stroke::{Enter, Key, Lock, Shifted};
    let cases: &[(&str, &[Stroke])] = &[
        ("a\nb\nc", &[Key('d'), Shifted('G')]),
        ("a\nb\nc", &[Key('d'), Lock(KeyCode::CapsLock), Key('j')]),
        ("abc", &[Key('r'), Lock(KeyCode::NumLock), Key('z')]),
        ("abc", &[Key('i'), Lock(KeyCode::ScrollLock), Key('z')]),
        ("abXcd", &[Key('f'), Shifted('X'), Key('x')]),
        ("abc", &[Key('i'), Shifted('Q'), Key('z')]),
        ("abc", &[Key('r'), Shifted('Z')]),
        (
            "abc",
            &[
                Key('"'),
                Shifted('A'),
                Key('y'),
                Key('l'),
                Key('"'),
                Key('a'),
                Key('P'),
            ],
        ),
        (
            "ab cd ef",
            &[
                Key('q'),
                Key('q'),
                Key('w'),
                Shifted('X'),
                Key('q'),
                Key('@'),
                Key('q'),
            ],
        ),
        (
            "ab cd",
            &[
                Key('w'),
                Key('m'),
                Shifted('M'),
                Key('0'),
                Key('`'),
                Shifted('M'),
            ],
        ),
        (
            "ab",
            &[
                Key(':'),
                Key('s'),
                Key('/'),
                Key('a'),
                Key('/'),
                Shifted('Q'),
                Enter,
            ],
        ),
    ];
    for (text, script) in cases {
        assert_eq!(
            run_script(text, script, true),
            run_script(text, script, false),
            "buffer {text:?}"
        );
    }
}

/// A shifted symbol read on the Windows layout acts as the character it
/// typed: as a motion, a register name, Insert-mode text and prompt text.
#[test]
fn shifted_symbol_keys_act_as_typed_character() {
    use Stroke::{Enter, Key, Symbol};
    let cases: &[(&str, &[Stroke])] = &[
        ("ab cd", &[Symbol('4', '$'), Key('x')]),
        ("(ab)", &[Symbol('5', '%'), Key('x')]),
        ("abc", &[Key('i'), Symbol('1', '!'), Symbol('2', '@')]),
        (
            "abc",
            &[
                Symbol('\'', '"'),
                Key('a'),
                Key('y'),
                Key('l'),
                Key('$'),
                Symbol('\'', '"'),
                Key('a'),
                Key('p'),
            ],
        ),
        (
            "ab",
            &[
                Symbol(';', ':'),
                Key('s'),
                Key('/'),
                Key('a'),
                Key('/'),
                Symbol('3', '#'),
                Enter,
            ],
        ),
    ];
    for (text, script) in cases {
        assert_eq!(
            run_script(text, script, true),
            run_script(text, script, false),
            "buffer {text:?}"
        );
    }
}
