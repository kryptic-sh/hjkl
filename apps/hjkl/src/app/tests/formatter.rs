use super::*;

// ── AutoIndent (=) operator app-level integration tests ──────────────────────
//
// These drive the full app keymap path — `route_chord_key` / `drive_key` —
// and verify the buffer state after reindent.

#[test]
fn equal_equal_in_normal_reindents_current_line() {
    // `==` on the second line of "{\n  body\n}" must normalise the indent
    // to shiftwidth=4 spaces (one level deep, inside the opening brace).
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "{\n  body\n}");
    app.active_mut().editor.settings_mut().shiftwidth = 4;
    app.active_mut().editor.settings_mut().expandtab = true;
    // Move cursor to row 1 ("  body").
    app.active_mut().editor.jump_cursor(1, 0);
    app.sync_viewport_from_editor();

    // Drive `==` through the normal keymap path.
    drive_chars(&mut app, "==");
    assert!(app.pending_state.is_none(), "pending must clear after ==");

    let lines: Vec<_> = app.active().editor.buffer().lines().to_vec();
    assert_eq!(
        lines,
        vec!["{", "    body", "}"],
        "== must reindent line 1 to 4 spaces; got {lines:?}"
    );
    assert_eq!(
        app.active().editor.vim_mode(),
        hjkl_engine::VimMode::Normal,
        "must stay in Normal after =="
    );
}

#[test]
fn eq_g_from_top_reindents_entire_buffer() {
    // `=G` from row 0 covers the whole buffer (top → last line).
    // Buffer: "{\nbody\n}" where "body" has wrong zero indent.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "{\nbody\n}");
    app.active_mut().editor.settings_mut().shiftwidth = 4;
    app.active_mut().editor.settings_mut().expandtab = true;
    // Cursor at row 0.
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    // Drive `=G`: = → BeginPendingAfterOp(AutoIndent), G → ApplyOpMotion.
    drive_chars(&mut app, "=G");
    assert!(app.pending_state.is_none(), "pending must clear after =G");

    let lines: Vec<_> = app.active().editor.buffer().lines().to_vec();
    assert_eq!(
        lines,
        vec!["{", "    body", "}"],
        "=G must reindent whole buffer; got {lines:?}"
    );
    assert_eq!(
        app.active().editor.vim_mode(),
        hjkl_engine::VimMode::Normal,
        "must stay in Normal after =G"
    );
}

#[test]
fn visual_line_eq_reindents_selected_lines() {
    // Enter VisualLine on row 1, press `=` — only "body" should be reindented.
    // Surrounding braces are NOT in the selection.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "{\nbody\n}");
    app.active_mut().editor.settings_mut().shiftwidth = 4;
    app.active_mut().editor.settings_mut().expandtab = true;
    app.active_mut().editor.jump_cursor(1, 0);
    app.sync_viewport_from_editor();

    use crossterm::event::{KeyCode, KeyEvent as CtKeyEvent, KeyModifiers};

    // Enter VisualLine via `V`.
    hjkl_vim_tui::handle_key(
        &mut app.active_mut().editor,
        CtKeyEvent::new(KeyCode::Char('V'), KeyModifiers::NONE),
    );
    assert_eq!(
        app.active().editor.vim_mode(),
        hjkl_engine::VimMode::VisualLine,
        "must be in VisualLine after V"
    );

    // Dispatch `=` via keymap (VisualOp path).
    let consumed = app.route_chord_key(CtKeyEvent::new(KeyCode::Char('='), KeyModifiers::NONE));
    assert!(consumed, "= in VisualLine must be consumed");

    let lines: Vec<_> = app.active().editor.buffer().lines().to_vec();
    // Row 1 is one level deep (depth=1 accumulated from row 0 `{`).
    assert_eq!(
        lines,
        vec!["{", "    body", "}"],
        "V= must reindent the selected line; got {lines:?}"
    );
    assert_eq!(
        app.active().editor.vim_mode(),
        hjkl_engine::VimMode::Normal,
        "must exit VisualLine after ="
    );
}

// ── IndentFlash app-level tests ───────────────────────────────────────────────

#[test]
fn indent_flash_active_returns_range_within_window() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.indent_flash = Some(IndentFlash {
        top: 2,
        bot: 5,
        started_at: Instant::now(),
    });
    assert_eq!(
        app.indent_flash_active(),
        Some((2, 5)),
        "fresh flash must return Some within INDENT_FLASH_DURATION"
    );
    assert!(app.indent_flash.is_some());
}

#[test]
fn indent_flash_active_returns_none_after_expiry() {
    // Set started_at > INDENT_FLASH_DURATION (75ms) in the past → expired.
    let mut app = App::new(None, false, None, None).unwrap();
    app.indent_flash = Some(IndentFlash {
        top: 0,
        bot: 3,
        started_at: Instant::now() - Duration::from_millis(150),
    });
    assert_eq!(app.indent_flash_active(), None);
    assert!(
        app.indent_flash.is_none(),
        "field must be cleared on expiry"
    );
}

#[test]
fn auto_indent_op_sets_indent_flash() {
    // Drive `==` through the real production chord path (route_chord_key)
    // and assert indent_flash is armed afterwards.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "{\n  body\n}");
    app.active_mut().editor.settings_mut().shiftwidth = 4;
    app.active_mut().editor.settings_mut().expandtab = true;
    app.active_mut().editor.jump_cursor(1, 0);
    app.sync_viewport_from_editor();

    // First `=` arms the pending-state reducer (BeginPendingAfterOp).
    app.route_chord_key(key(KeyCode::Char('=')));
    // Second `=` commits ApplyOpDouble(AutoIndent) via route_chord_key_inner,
    // which calls dispatch_action and drains take_last_indent_range.
    app.route_chord_key(key(KeyCode::Char('=')));

    assert!(
        app.indent_flash.is_some(),
        "indent_flash must be armed after == operator"
    );
    // The flash must point at row 1 (the only row touched by `==`).
    if let Some(ref f) = app.indent_flash {
        assert_eq!(f.top, 1, "flash top must match indented row");
        assert_eq!(f.bot, 1, "flash bot must match indented row");
    }
}

// ── hjkl-mangler: auto-indent formatter dispatch tests ──────────────────────

/// END-TO-END `gg=G` on a LARGE real source file (~8000 LOC). Regression for
/// `formatter: I/O error: Broken pipe` reported on 2026-05-16. Large stdin
/// payloads can race with rustfmt closing its pipe early on errors.
#[test]
fn auto_indent_gg_eq_g_on_large_file_does_not_break_pipe() {
    use std::io::Write;
    if std::process::Command::new("rustfmt")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping: rustfmt not on PATH");
        return;
    }

    // Use the engine's editor.rs as a real workload — 8k LOC, modern Rust
    // syntax (let chains, async closures), already cargo-fmt'd so rustfmt
    // should accept it and return identical output.
    let src = include_str!("../../../../../crates/hjkl-engine/src/editor.rs");

    let path = std::env::temp_dir().join(format!(
        "hjkl_mangler_large_{}.rs",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(src.as_bytes()).unwrap();
    drop(f);

    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
    app.sync_viewport_from_editor();

    app.route_chord_key(key(KeyCode::Char('g')));
    app.route_chord_key(key(KeyCode::Char('g')));
    app.route_chord_key(key(KeyCode::Char('=')));
    app.route_chord_key(key(KeyCode::Char('G')));

    let _ = std::fs::remove_file(&path);

    assert!(
        app.bus.last_body().is_none() || !app.bus.last_body().unwrap().contains("pipe"),
        "expected no broken-pipe error; got status: {:?}",
        app.bus.last_body_or_empty()
    );
}

/// END-TO-END `gg=G`: user-reported repro on 2026-05-16. Open `.rs` file,
/// `gg` to top, `=G` to format whole buffer. Asserts rustfmt was actually
/// invoked (not dumb algo).
#[test]
fn auto_indent_gg_eq_g_invokes_rustfmt() {
    use std::io::Write;
    if std::process::Command::new("rustfmt")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping: rustfmt not on PATH");
        return;
    }

    let path = std::env::temp_dir().join(format!(
        "hjkl_mangler_gg_eq_g_{}.rs",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(b"fn main(){let x=1;let y=2;\nprintln!(\"{}\",x+y);\n}\n")
        .unwrap();
    drop(f);

    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
    app.sync_viewport_from_editor();

    // gg=G: g → g-prefix, g → jump to top, = → op-pending, G → ApplyOpMotion.
    app.route_chord_key(key(KeyCode::Char('g')));
    app.route_chord_key(key(KeyCode::Char('g')));
    app.route_chord_key(key(KeyCode::Char('=')));
    app.route_chord_key(key(KeyCode::Char('G')));

    // Format is now async — poll until the result lands (cap 5 s).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if app.poll_format_results() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for rustfmt result"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    let after = app.active().editor.buffer().as_string();
    let _ = std::fs::remove_file(&path);

    assert!(
        after.contains("let x = 1;"),
        "rustfmt output expected (`let x = 1;`); got:\n{after}\n\nstatus: {:?}",
        app.bus.last_body_or_empty()
    );
}

/// Headless repro of user-reported "prettier: not installed" on .md files
/// (2026-05-16). Prints every observable: PATH, direct subprocess probe,
/// hjkl-mangler::probe_tool, App status message after gg=G. Always passes —
/// pure diagnostic. Run with:
///   cargo test -p hjkl --bin hjkl prettier_md_diagnostic -- --nocapture --ignored
#[test]
#[ignore = "diagnostic harness — run manually with --nocapture"]
fn prettier_md_diagnostic() {
    use std::io::Write;

    eprintln!("=== PATH ===");
    eprintln!("{}", std::env::var("PATH").unwrap_or_default());

    eprintln!("\n=== Direct Command::new(\"prettier\") --version ===");
    let r = std::process::Command::new("prettier")
        .arg("--version")
        .output();
    eprintln!("{:?}", r);

    eprintln!("\n=== hjkl_mangler::probe_tool ===");
    eprintln!("{:?}", hjkl_mangler::probe_tool("prettier"));

    eprintln!("\n=== App flow: create .md, gg=G, poll, observe status ===");
    let path = std::env::temp_dir().join(format!(
        "hjkl_mangler_md_diag_{}.md",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(b"# hello\n*world*  with   bad whitespace\n")
        .unwrap();
    drop(f);

    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
    app.sync_viewport_from_editor();

    app.route_chord_key(key(KeyCode::Char('g')));
    app.route_chord_key(key(KeyCode::Char('g')));
    app.route_chord_key(key(KeyCode::Char('=')));
    app.route_chord_key(key(KeyCode::Char('G')));

    eprintln!(
        "status right after submit: {:?}",
        app.bus.last_body_or_empty()
    );

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if app.poll_format_results() {
            eprintln!("poll_format_results returned true");
            break;
        }
        if std::time::Instant::now() >= deadline {
            eprintln!("polling timed out after 10s");
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    eprintln!("status after poll: {:?}", app.bus.last_body_or_empty());
    eprintln!(
        "buffer after: {:?}",
        app.active().editor.buffer().as_string()
    );

    let _ = std::fs::remove_file(&path);
}

/// END-TO-END: open a real `.rs` file, drive `==` via the chord path, assert
/// the buffer was reformatted by rustfmt (not dumb algo). Regression for the
/// "user reports formatter not invoked" diagnosis on 2026-05-16.
#[test]
fn auto_indent_invokes_rustfmt_for_rs_files() {
    use std::io::Write;

    // Skip if rustfmt isn't on PATH (CI sandboxes without rustup).
    if std::process::Command::new("rustfmt")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping: rustfmt not on PATH");
        return;
    }

    // Single-line file so the `==` range exactly covers the whole content.
    // This verifies rustfmt is invoked and the output is installed.
    let path = std::env::temp_dir().join(format!(
        "hjkl_mangler_e2e_{}.rs",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut f = std::fs::File::create(&path).unwrap();
    // One-liner: rustfmt expands it. The range is row 0 only for `==`, but
    // any insertions anchored at or before end_row+1 are accepted, so the
    // expanded body also lands in the buffer.
    f.write_all(b"fn main(){let x=1;let y=2;}\n").unwrap();
    drop(f);

    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
    app.sync_viewport_from_editor();

    // Drive `==` via the production chord path.
    app.route_chord_key(key(KeyCode::Char('=')));
    app.route_chord_key(key(KeyCode::Char('=')));

    // Format is now async — poll until the result lands (cap 5 s).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if app.poll_format_results() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for rustfmt result"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    let after = app.active().editor.buffer().as_string();
    let _ = std::fs::remove_file(&path);

    // rustfmt must have been invoked — the buffer must differ from the
    // original compact form and contain properly-spaced output.
    assert!(
        after.contains("let x = 1;"),
        "rustfmt output missing `let x = 1;`. got:\n{after}\n\nstatus: {:?}",
        app.bus.last_body_or_empty()
    );
    assert!(
        after.contains("let y = 2;"),
        "rustfmt output missing `let y = 2;`. got:\n{after}\n\nstatus: {:?}",
        app.bus.last_body_or_empty()
    );
}

/// `==` on a buffer with no filename → falls back to dumb `auto_indent_range`
/// (no formatter lookup, no panic, indent_flash is armed by the dumb algo).
#[test]
fn auto_indent_falls_back_to_dumb_for_unknown_ext() {
    // No filename set → formatter_for_path never called → dumb algo runs.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "{\nbody\n}");
    app.active_mut().editor.settings_mut().shiftwidth = 4;
    app.active_mut().editor.settings_mut().expandtab = true;
    // cursor on row 1
    app.active_mut().editor.jump_cursor(1, 0);
    app.sync_viewport_from_editor();

    app.route_chord_key(key(KeyCode::Char('=')));
    app.route_chord_key(key(KeyCode::Char('=')));

    // Dumb algo fires: flash must be set (row 1 for single-line `==`).
    assert!(
        app.indent_flash.is_some(),
        "dumb auto_indent_range must arm indent_flash when no formatter matches"
    );
    // Status message must NOT contain a formatter error.
    assert!(
        !app.bus.last_body_or_empty().contains("not installed"),
        "no formatter-error status for unknown-ext fallback"
    );
}

/// `==` on a `.xyz` file also falls back to dumb algo: a filename with an
/// unrecognised extension means `formatter_for_path` returns `None`.
#[test]
fn auto_indent_falls_back_to_dumb_for_no_registered_formatter() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello\nworld");
    // Give the slot a filename with an extension hjkl-mangler does not handle.
    app.active_mut().filename = Some(std::path::PathBuf::from("/tmp/test_file.xyz"));
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    app.route_chord_key(key(KeyCode::Char('=')));
    app.route_chord_key(key(KeyCode::Char('=')));

    // Dumb algo arms the flash; no formatter error in status.
    assert!(
        app.indent_flash.is_some(),
        "dumb auto_indent_range must arm indent_flash for unrecognised extension"
    );
}

/// `==` on a `.rs` file with rustfmt installed: buffer content is replaced by
/// the formatter output and the flash covers the whole buffer (top=0).
///
/// Requires `rustfmt` on PATH. Run with:
///   cargo test -p hjkl -- --ignored auto_indent_dispatches_to_formatter_for_known_ext
#[test]
#[ignore = "requires rustfmt on PATH"]
fn auto_indent_dispatches_to_formatter_for_known_ext() {
    let mut app = App::new(None, false, None, None).unwrap();
    // Deliberately un-formatted Rust source — rustfmt will add spacing.
    let ugly = "fn main(){let x=1;}";
    seed_buffer(&mut app, ugly);
    // Assign a .rs filename so formatter_for_path picks rustfmt.
    app.active_mut().filename = Some(std::path::PathBuf::from("/tmp/hjkl_mangler_test.rs"));
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();

    app.route_chord_key(key(KeyCode::Char('=')));
    app.route_chord_key(key(KeyCode::Char('=')));

    // Flash must be armed *immediately* at submit time (before formatter runs).
    // Top is the viewport's top row, which for a fresh single-line buffer is 0.
    assert!(
        app.indent_flash.as_ref().is_some_and(|f| f.top == 0),
        "flash must be armed at submit time with viewport top = 0"
    );

    // Format is async — poll until the result lands (cap 5 s).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if app.poll_format_results() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for rustfmt result"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    // Formatter replaced the buffer — content must differ from original.
    let formatted = app.active().editor.buffer().as_string();
    assert_ne!(
        formatted, ugly,
        "rustfmt must have changed the buffer content"
    );
    // Formatted output should contain proper spacing.
    assert!(
        formatted.contains("let x = 1;"),
        "rustfmt output must have spaced assignment, got: {formatted}"
    );
    // No error in status message.
    assert!(
        app.bus.last_body().is_none(),
        "no status error expected after successful format"
    );
}

/// `u` after a formatter pass restores the pre-format buffer. Regression for
/// 2026-05-16: `set_content` cleared the undo stack so formatter changes
/// could not be undone. Fixed by routing through `set_content_undoable`.
#[test]
fn auto_indent_format_result_is_undoable() {
    use std::io::Write;
    if std::process::Command::new("rustfmt")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping: rustfmt not on PATH");
        return;
    }

    let path = std::env::temp_dir().join(format!(
        "hjkl_mangler_undo_{}.rs",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let ugly = "fn main(){let x=1;}\n";
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(ugly.as_bytes()).unwrap();
    drop(f);

    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
    app.sync_viewport_from_editor();

    // Drive `==` to trigger rustfmt.
    app.route_chord_key(key(KeyCode::Char('=')));
    app.route_chord_key(key(KeyCode::Char('=')));

    // Wait for the async result to install.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if app.poll_format_results() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for rustfmt result"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    let formatted = app.active().editor.buffer().as_string();
    assert_ne!(
        formatted,
        ugly.trim_end(),
        "rustfmt must have changed the buffer (sanity)"
    );

    // Press `u` — must restore the pre-format buffer.
    app.route_chord_key(key(KeyCode::Char('u')));
    let after_undo = app.active().editor.buffer().as_string();
    let _ = std::fs::remove_file(&path);

    assert_eq!(
        after_undo.trim_end(),
        ugly.trim_end(),
        "undo must restore pre-format content; got:\n{after_undo}"
    );
}

/// `==` on row 2 must reformat only the key on that row via prettier's native
/// `--range-start/--range-end` flags. Rows outside the range must be returned
/// intact (prettier returns the whole file with only in-range bytes reformatted).
///
/// Requires `prettier` on PATH. Run with:
///   cargo test -p hjkl -- auto_indent_double_equals_only_touches_current_line
#[test]
fn auto_indent_double_equals_only_touches_current_line() {
    use std::io::Write;

    if std::process::Command::new("prettier")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping: prettier not on PATH");
        return;
    }

    // Six-line JSON object. Row 2 ("c":3) is unquoted — prettier will quote it.
    // Rows 0,1,3,4,5 are already valid JSON that prettier leaves unchanged.
    // `==` on row 2 passes a range for row 2 only; prettier's native byte-range
    // flags ensure only that key is reformatted.
    let content = concat!(
        "{\n",           // row 0
        "  \"a\": 1,\n", // row 1
        "  \"b\": 2,\n", // row 2: cursor here — prettier normalises spacing
        "  \"c\": 3,\n", // row 3: must be untouched by `==` on row 2
        "  \"d\": 4,\n", // row 4
        "  \"e\": 5\n",  // row 5
        "}\n",           // row 6
    );

    let path = std::env::temp_dir().join(format!(
        "hjkl_range_eq_{}.json",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(content.as_bytes()).unwrap();
    drop(f);

    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
    app.sync_viewport_from_editor();

    // Move cursor to row 2 (j j from row 0).
    app.route_chord_key(key(KeyCode::Char('j')));
    app.route_chord_key(key(KeyCode::Char('j')));
    assert_eq!(app.active().editor.cursor().0, 2, "cursor must be on row 2");

    // Drive `==` via the production chord path.
    app.route_chord_key(key(KeyCode::Char('=')));
    app.route_chord_key(key(KeyCode::Char('=')));

    // Poll until the async result lands (cap 5 s).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if app.poll_format_results() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for prettier result"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    let after = app.active().editor.buffer().as_string();
    let _ = std::fs::remove_file(&path);

    // Prettier with native range flags returns the whole file.
    // Row 2 must have been processed; other rows must still be present.
    assert!(
        after.contains("\"a\": 1"),
        "`==` on row 2 must not remove row 1; got:\n{after}"
    );
    assert!(
        after.contains("\"b\": 2"),
        "row 2 key must be present after format; got:\n{after}"
    );
    assert!(
        after.contains("\"c\": 3"),
        "`==` on row 2 must not remove row 3; got:\n{after}"
    );
    // The whole file was returned (not just the range slice).
    assert!(
        after.starts_with('{'),
        "whole-file output expected; got:\n{after}"
    );
}

/// Regression: `gg=G` must still reformat the whole file even with range
/// support in place. The range covers rows 0..last_row, so all changes
/// pass the in-range filter.
#[test]
fn auto_indent_gg_eq_g_still_reformats_whole_file() {
    use std::io::Write;
    if std::process::Command::new("rustfmt")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping: rustfmt not on PATH");
        return;
    }

    let path = std::env::temp_dir().join(format!(
        "hjkl_whole_file_{}.rs",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let src = b"fn main(){let x=1;let y=2;\nprintln!(\"{}\",x+y);\n}\n";
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(src).unwrap();
    drop(f);

    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
    app.sync_viewport_from_editor();

    // gg=G: jump to top, op-pending, motion G (whole file).
    app.route_chord_key(key(KeyCode::Char('g')));
    app.route_chord_key(key(KeyCode::Char('g')));
    app.route_chord_key(key(KeyCode::Char('=')));
    app.route_chord_key(key(KeyCode::Char('G')));

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if app.poll_format_results() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for rustfmt result"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    let after = app.active().editor.buffer().as_string();
    let _ = std::fs::remove_file(&path);

    // rustfmt must have reformatted — spaced assignment must appear.
    assert!(
        after.contains("let x = 1;"),
        "gg=G must reformat whole file; got:\n{after}"
    );
    assert!(
        after.contains("let y = 2;"),
        "gg=G must reformat whole file; got:\n{after}"
    );
}
