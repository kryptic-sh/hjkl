//! Key event kinds at the terminal read boundary (`App::handle_key_event`).
//!
//! The Windows console reports a `KeyEventKind::Release` for every key-up, so
//! each keystroke arrives there as a Press + Release pair. Acting on the
//! release handled every key twice: `:qa` typed `::qqaa`, Backspace deleted
//! two chars. These tests feed Windows-shaped event streams through the same
//! method both `run()` read arms use, so they guard the fix on every platform
//! CI runs, not only on a Windows console.

use super::*;
use crossterm::event::KeyEventKind;

fn with_kind(code: KeyCode, kind: KeyEventKind) -> KeyEvent {
    KeyEvent::new_with_kind(code, KeyModifiers::NONE, kind)
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
