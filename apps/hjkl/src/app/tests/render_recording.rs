use super::*;
use ratatui::{Terminal, backend::TestBackend};

// ── Fix 1: recording-register full-line takeover (v0.21.8) ──────────────────
//
// When `q{reg}` macro recording is active, `build_status_line` must return the
// plain-bold full-width banner (`" recording @r"`) instead of delegating to
// `build_normal_status_bar`. The banner must:
//   - contain "recording @a" (or whatever register was used)
//   - NOT contain a hjkl-statusline "REC @a" badge from the lualine bar
//   - fill the whole `width` with spaces so the padded line is always WIDTH chars

#[test]
fn recording_active_produces_full_line_banner() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello");

    // Start macro recording into register 'a' via the same key path as the
    // existing macro tests (drive_key handles the pending-state reducer).
    drive_key(&mut app, ck('q'));
    drive_key(&mut app, ck('a'));
    assert!(
        app.active_editor().is_recording_macro(),
        "prerequisite: qa must start macro recording"
    );
    assert_eq!(app.active_editor().recording_register(), Some('a'));

    let width: u16 = 60;
    let text = crate::render::status_line_text(&app, width);

    // Must be a full-line banner containing "recording @a".
    assert!(
        text.contains("recording @a"),
        "recording active: status line must contain 'recording @a', got {text:?}"
    );

    // Must NOT look like a lualine REC badge — the banner text starts with " recording".
    assert!(
        !text.contains("REC @a"),
        "recording banner must NOT contain the lualine badge 'REC @a', got {text:?}"
    );

    // The full rendered string must equal exactly `width` chars (the banner
    // pads to fill the row just like every other full-line takeover branch).
    assert_eq!(
        text.chars().count(),
        width as usize,
        "recording banner must fill full width {width}, got {} chars: {text:?}",
        text.chars().count()
    );
}

#[test]
fn recording_stopped_falls_through_to_normal_bar() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello");

    // Start and immediately stop recording.
    drive_key(&mut app, ck('q'));
    drive_key(&mut app, ck('a'));
    drive_key(&mut app, ck('q')); // bare q stops recording
    assert!(
        !app.active_editor().is_recording_macro(),
        "prerequisite: bare q must stop recording"
    );

    let width: u16 = 60;
    let text = crate::render::status_line_text(&app, width);

    // After recording stops the normal lualine bar takes over; it won't say
    // "recording @" anywhere.
    assert!(
        !text.contains("recording @"),
        "after stop: status line must NOT contain recording banner, got {text:?}"
    );

    // Should still be the right width.
    assert_eq!(
        text.chars().count(),
        width as usize,
        "normal bar must fill full width {width}, got {}: {text:?}",
        text.chars().count()
    );
}

#[test]
fn active_indent_flash_renders_in_zero_height_buffer_viewport() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.start_screen = None;
    app.dispatch_ex("sp");
    assert_eq!(
        app.windows.iter().filter(|window| window.is_some()).count(),
        2,
        "prerequisite: a split must allocate a zero-height child in a one-row buffer pane"
    );
    app.focus_below();
    seed_buffer(&mut app, "{\nbody\n}");
    app.active_editor_mut().jump_cursor(1, 0);
    app.sync_viewport_from_editor();
    app.route_chord_key(key(KeyCode::Char('=')));
    app.route_chord_key(key(KeyCode::Char('=')));
    assert!(
        app.indent_flash.is_some(),
        "prerequisite: auto-indent must arm the flash"
    );

    let mut terminal = Terminal::new(TestBackend::new(80, 1)).unwrap();
    let completed = terminal.draw(|frame| crate::render::frame(frame, &mut app));

    assert!(
        completed.is_ok(),
        "a one-row terminal must render an active indent flash without an invalid draw"
    );
    assert_eq!(
        app.windows[app.focused_window()]
            .as_ref()
            .and_then(|window| window.last_rect)
            .map(|rect| rect.h),
        Some(0),
        "the focused buffer viewport must receive the zero-height pane"
    );
}
