use super::*;

// ── LSP diagnostics tests ────────────────────────────────────────────────

/// Build a `textDocument/publishDiagnostics` JSON payload for `file_url`
/// containing one error diagnostic.
#[test]
fn publish_diagnostics_populates_slot_diags() {
    let mut app = App::new(None, false, None, None).unwrap();

    // Give the active slot an absolute file path.
    let path = tmp_path("hjkl_diag_test.rs");
    app.active_mut().filename = Some(path.clone());

    seed_buffer(&mut app, "let x = ();\nlet y = ();");

    let params = pub_diags_params(
        &file_url(&path),
        serde_json::json!([{
            "range": {
                "start": { "line": 0, "character": 4 },
                "end":   { "line": 0, "character": 5 }
            },
            "severity": 1,
            "message": "unused variable",
            "source": "rustc",
            "code": "E0001"
        }]),
    );

    app.handle_publish_diagnostics(params, hjkl_lsp::PositionEncoding::Utf8);

    let slot = app.active();
    assert_eq!(slot.lsp_diags.len(), 1);
    let d = &slot.lsp_diags[0];
    assert_eq!(d.start_row, 0);
    assert_eq!(d.start_col, 4);
    assert_eq!(d.end_row, 0);
    assert_eq!(d.end_col, 5);
    assert_eq!(d.severity, DiagSeverity::Error);
    assert_eq!(d.message, "unused variable");
    assert_eq!(d.source.as_deref(), Some("rustc"));
    assert_eq!(d.code.as_deref(), Some("E0001"));

    // Gutter sign must be present for row 0.
    assert!(
        slot.diag_signs_lsp
            .iter()
            .any(|s| s.row == 0 && s.ch == 'E'),
        "expected an 'E' gutter sign for row 0"
    );
}

#[test]
fn publish_diagnostics_replaces_existing() {
    let mut app = App::new(None, false, None, None).unwrap();
    let path = tmp_path("hjkl_diag_replace.rs");
    app.active_mut().filename = Some(path.clone());
    seed_buffer(&mut app, "a\nb\nc");

    // First publish: two diags.
    let params1 = pub_diags_params(
        &file_url(&path),
        serde_json::json!([
            {
                "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 1 } },
                "severity": 1,
                "message": "err A"
            },
            {
                "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 1 } },
                "severity": 2,
                "message": "warn B"
            }
        ]),
    );
    app.handle_publish_diagnostics(params1, hjkl_lsp::PositionEncoding::Utf8);
    assert_eq!(app.active().lsp_diags.len(), 2);

    // Second publish: one diag — must replace, not append.
    let params2 = pub_diags_params(
        &file_url(&path),
        serde_json::json!([{
            "range": { "start": { "line": 2, "character": 0 }, "end": { "line": 2, "character": 1 } },
            "severity": 3,
            "message": "info C"
        }]),
    );
    app.handle_publish_diagnostics(params2, hjkl_lsp::PositionEncoding::Utf8);

    let slot = app.active();
    assert_eq!(
        slot.lsp_diags.len(),
        1,
        "second publish must replace, not append"
    );
    assert_eq!(slot.lsp_diags[0].message, "info C");
    assert_eq!(slot.lsp_diags[0].severity, DiagSeverity::Info);
    // Old signs must be replaced too.
    assert_eq!(slot.diag_signs_lsp.len(), 1);
    assert_eq!(slot.diag_signs_lsp[0].row, 2);
}

#[test]
fn publish_diagnostics_clears_on_empty() {
    let mut app = App::new(None, false, None, None).unwrap();
    let path = tmp_path("hjkl_diag_clear.rs");
    app.active_mut().filename = Some(path.clone());
    seed_buffer(&mut app, "a");

    let params_with = pub_diags_params(
        &file_url(&path),
        serde_json::json!([{
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 1 } },
            "severity": 1,
            "message": "err"
        }]),
    );
    app.handle_publish_diagnostics(params_with, hjkl_lsp::PositionEncoding::Utf8);
    assert_eq!(app.active().lsp_diags.len(), 1);

    // Empty diagnostics array clears all diags.
    let params_clear = pub_diags_params(&file_url(&path), serde_json::json!([]));
    app.handle_publish_diagnostics(params_clear, hjkl_lsp::PositionEncoding::Utf8);

    let slot = app.active();
    assert!(slot.lsp_diags.is_empty(), "empty publish must clear diags");
    assert!(
        slot.diag_signs_lsp.is_empty(),
        "empty publish must clear gutter signs"
    );
}

#[test]
fn publish_diagnostics_ignores_unknown_uri() {
    let mut app = App::new(None, false, None, None).unwrap();
    let path = tmp_path("hjkl_diag_known.rs");
    app.active_mut().filename = Some(path.clone());
    seed_buffer(&mut app, "a");

    // Params targeting a *different* file — should be silently ignored.
    let unknown_path = tmp_path("hjkl_diag_unknown.rs");
    let params = pub_diags_params(
        &file_url(&unknown_path),
        serde_json::json!([{
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 1 } },
            "severity": 1,
            "message": "err"
        }]),
    );
    app.handle_publish_diagnostics(params, hjkl_lsp::PositionEncoding::Utf8);

    assert!(
        app.active().lsp_diags.is_empty(),
        "unmatched URI must not populate diags"
    );
}

/// Regression (audit R2, UTF-16 fix): a diagnostic reported by a
/// UTF-16-only server on a line containing an astral char (one whose UTF-16
/// encoding is a surrogate pair — 2 code units for 1 char, e.g. an emoji)
/// must land on the correct hjkl-internal char column, NOT the raw UTF-16
/// wire value. This is the round-trip scenario from the fix's spec: a
/// diagnostic at the wire column of `#` on a line with a multibyte prefix
/// must convert to the char column of `#`.
#[test]
fn publish_diagnostics_converts_utf16_wire_columns_to_char_columns() {
    let mut app = App::new(None, false, None, None).unwrap();
    let path = tmp_path("hjkl_diag_utf16.rs");
    app.active_mut().filename = Some(path.clone());
    // "🎉hello world" — U+1F389 is 1 char, 2 UTF-16 code units, 4 UTF-8 bytes.
    // Char indices: 0:🎉 1:h 2:e 3:l 4:l 5:o 6:' ' 7:w ...
    // UTF-16 wire offsets: before 'h'=2 (past the emoji's 2 units), before
    // 'w'=8 (2 + "hello "'s 6 ASCII units).
    seed_buffer(&mut app, "🎉hello world");

    let params = pub_diags_params(
        &file_url(&path),
        serde_json::json!([{
            // Wire range covers "hello" using UTF-16 units: [2, 7).
            "range": { "start": { "line": 0, "character": 2 }, "end": { "line": 0, "character": 7 } },
            "severity": 1,
            "message": "utf16 diag"
        }]),
    );
    app.handle_publish_diagnostics(params, hjkl_lsp::PositionEncoding::Utf16);

    let slot = app.active();
    assert_eq!(slot.lsp_diags.len(), 1);
    let d = &slot.lsp_diags[0];
    // Correct char columns: 1 (start of "hello") .. 6 (just past "hello",
    // before the space). A pre-fix build would store the raw wire values
    // 2..7 here instead, which point at "ello " (chars 2..7) — one char too
    // far right because the emoji ate 2 wire units but only 1 char slot.
    assert_eq!(
        d.start_col, 1,
        "wire col 2 (UTF-16) must convert to char col 1"
    );
    assert_eq!(
        d.end_col, 6,
        "wire col 7 (UTF-16) must convert to char col 6"
    );
}

#[test]
fn lnext_jumps_to_next_diag() {
    let mut app = App::new(None, false, None, None).unwrap();
    let path = tmp_path("hjkl_lnext.rs");
    app.active_mut().filename = Some(path.clone());
    seed_buffer(&mut app, "a\nb\nc\nhello world");

    // Plant diags on rows 1 and 3.
    let params = pub_diags_params(
        &file_url(&path),
        serde_json::json!([
            {
                "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 1 } },
                "severity": 1,
                "message": "first"
            },
            {
                "range": { "start": { "line": 3, "character": 6 }, "end": { "line": 3, "character": 11 } },
                "severity": 2,
                "message": "second"
            }
        ]),
    );
    app.handle_publish_diagnostics(params, hjkl_lsp::PositionEncoding::Utf8);

    // Cursor at row 0 — lnext should jump to row 1.
    app.lnext_severity(None);
    let (row, _col) = app.active_editor().cursor();
    assert_eq!(row, 1, "lnext must jump to first diag after cursor");

    // Cursor now at row 1 — lnext should jump to row 3.
    app.lnext_severity(None);
    let (row, col) = app.active_editor().cursor();
    assert_eq!(row, 3);
    assert_eq!(col, 6, "lnext must place cursor at diag start_col");
}

#[test]
fn gg_scrolls_window_viewport_to_top() {
    // Regression: gg moved cursor to (0,0) and the engine called
    // ensure_cursor_in_scrolloff, but the host's keymap-Unbound branch
    // forwarded the key to the engine WITHOUT calling
    // sync_viewport_from_editor — so the focused window's stored
    // top_row stayed at the old position.
    let mut app = App::new(None, false, None, None).unwrap();
    let lines: Vec<String> = (0..100).map(|i| format!("line {i}")).collect();
    seed_buffer(&mut app, &lines.join("\n"));

    // Position cursor + viewport deep in the buffer. The viewport_height
    // atomic must also be set — every vim::step resyncs vp.height from
    // it, so leaving the atomic at 0 would zero the host viewport mid-step
    // and disable scrolloff math.
    app.active_editor_mut().set_viewport_height(20);
    {
        let vp = app.active_editor_mut().host_mut().viewport_mut();
        vp.width = 80;
        vp.height = 20;
        vp.text_width = 80;
        vp.top_row = 60;
    }
    app.active_editor_mut().jump_cursor(70, 0);
    app.sync_viewport_from_editor();
    let fw = app.focused_window();
    assert_eq!(app.window_scroll(fw).0, 60);

    // Drive `gg` through the engine. First `g` sets engine-side pending,
    // second `g` triggers the gg motion (cursor → top + auto-scroll).
    hjkl_vim_tui::handle_key(
        app.active_editor_mut(),
        KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE),
    );
    hjkl_vim_tui::handle_key(
        app.active_editor_mut(),
        KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE),
    );
    // The Unbound replay path in event_loop.rs syncs the editor's
    // auto-scrolled viewport back to the focused window.
    app.sync_viewport_from_editor();

    let (row, _col) = app.active_editor().cursor();
    assert_eq!(row, 0, "gg must put cursor at row 0");
    let stored_top = app.window_scroll(fw).0;
    assert!(
        stored_top < 60,
        "gg must scroll window viewport to top, but stored top_row stayed at {stored_top}"
    );
}

#[test]
fn plus_slash_argv_scrolls_window_viewport_to_match() {
    // Regression: +/pat moved the cursor but didn't scroll the viewport,
    // so the rendered viewport stayed at row 0 and the cursor landed
    // off-screen on large files. Fix: App::new calls
    // ensure_cursor_in_scrolloff after the search and seeds the initial
    // window's top_row from the editor viewport.
    use std::io::Write;
    let dir = std::env::temp_dir().join("hjkl_plus_slash_scroll");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sample.rs");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        // 100 lines of filler; first `target` match deep at row 80.
        for i in 0..100 {
            if i == 80 {
                writeln!(f, "fn target() {{}}").unwrap();
            } else {
                writeln!(f, "// padding line {i}").unwrap();
            }
        }
    }
    // Set viewport_height atomic via a fake App + apply_viewport_height
    // before the search runs. App::new builds the slot with
    // crossterm::terminal::size() — under tests that may return 0,
    // disabling scrolloff. Pre-set the atomic by dropping in via the
    // test helper.
    // Easier path: build a small file where the first match is on row 5
    // and assert window.top_row > 0 (proxy for "scrolled").
    let mut app = App::new(Some(path.clone()), false, None, Some("target".into())).unwrap();
    let (row, _col) = app.active_editor().cursor();
    assert_eq!(row, 80, "+/target must move cursor to row 80");
    // The window's stored top_row should reflect the editor's scrolled
    // viewport. With crossterm::terminal::size returning 0 in test
    // contexts the scroll math is a no-op, so set the height atomic
    // and re-run ensure_cursor_in_scrolloff to verify the scroll path.
    app.active_editor_mut().set_viewport_height(20);
    {
        let vp = app.active_editor_mut().host_mut().viewport_mut();
        vp.width = 80;
        vp.height = 20;
        vp.text_width = 80;
    }
    app.active_editor_mut().ensure_cursor_in_scrolloff();
    let editor_top = app.active_editor().host().viewport().top_row;
    assert!(
        editor_top > 0,
        "ensure_cursor_in_scrolloff should scroll editor viewport away from row 0; got top_row={editor_top}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn slash_search_in_editor_scrolls_window_viewport() {
    // Regression: /pat<CR> in the editor moved the cursor but didn't
    // scroll the focused window's viewport, leaving the cursor
    // off-screen on large files.
    let mut app = App::new(None, false, None, None).unwrap();
    let lines: Vec<String> = (0..100)
        .map(|i| {
            if i == 80 {
                "target".into()
            } else {
                format!("line {i}")
            }
        })
        .collect();
    seed_buffer(&mut app, &lines.join("\n"));
    app.active_editor_mut().set_viewport_height(20);
    {
        let vp = app.active_editor_mut().host_mut().viewport_mut();
        vp.width = 80;
        vp.height = 20;
        vp.text_width = 80;
    }
    let fw = app.focused_window();
    // Cursor at (0,0), window.top_row=0. Run /target<CR>.
    app.commit_search("target");
    let stored_top = app.window_scroll(fw).0;
    assert!(
        stored_top > 0,
        "/target<CR> should scroll the focused window's stored top_row past 0 to reveal the match"
    );
    let (row, _col) = app.active_editor().cursor();
    assert_eq!(row, 80, "/target<CR> should land cursor on row 80");
    // Counter must show 1/1 (cursor on the only match), not 0/1.
    let count = crate::render::search_count(&app);
    assert_eq!(
        count,
        Some((1, 1)),
        "search counter must update after /<CR>"
    );
    // Cursor must respect SCROLLOFF=5: cursor at row 80, height 20, so
    // viewport top_row should be such that screen row is between
    // [margin, height-1-margin] = [5, 14]. Specifically max_bottom=14
    // → top = 80 - 14 = 66.
    let stored_top = app.window_scroll(fw).0;
    let screen_row = 80usize.saturating_sub(stored_top);
    assert!(
        (5..=14).contains(&screen_row),
        "scrolloff=5 violated: screen_row={screen_row} (top={stored_top}, cursor=80, height=20)"
    );
}

#[test]
fn plus_slash_argv_with_realistic_rust_source() {
    // Mirror the user's repro: hjkl +/main on a real-ish rust file.
    use std::io::Write;
    let dir = std::env::temp_dir().join("hjkl_plus_slash_real");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sample.rs");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        // Real-ish content. First `main` substring is on row 5 (`fn main`).
        writeln!(f, "//! crate root").unwrap(); // row 0
        writeln!(f).unwrap(); // row 1
        writeln!(f, "use std::path::PathBuf;").unwrap();
        writeln!(f).unwrap();
        writeln!(f, "/// Entry.").unwrap();
        writeln!(f, "fn main() {{").unwrap(); // row 5: first 'main'
        writeln!(f, "    let _ = main_helper();").unwrap(); // row 6: 'main_helper'
        writeln!(f, "}}").unwrap();
        writeln!(f, "fn main_helper() {{}}").unwrap(); // row 8: 'main_helper'
    }
    let app = App::new(Some(path.clone()), false, None, Some("main".into())).unwrap();
    let (row, _col) = app.active_editor().cursor();
    assert_eq!(
        row, 5,
        "+/main on rust source must land on row 5 (first `fn main`), got row {row}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn plus_slash_argv_search_lands_on_first_forward_match() {
    // Regression: hjkl +/main file.rs lands cursor on a match in the
    // backward direction (or wraps incorrectly) because the +/<pat>
    // path advanced from cursor=(0,0) and the wrap policy mishandles
    // the at-or-after invariant.
    use std::io::Write;
    let dir = std::env::temp_dir().join("hjkl_plus_slash_test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sample.txt");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        // 3 matches at known rows. First match at row 2.
        writeln!(f, "alpha").unwrap();
        writeln!(f, "beta").unwrap();
        writeln!(f, "main one").unwrap();
        writeln!(f, "delta").unwrap();
        writeln!(f, "main two").unwrap();
        writeln!(f, "main three").unwrap();
    }
    let app = App::new(Some(path.clone()), false, None, Some("main".into())).unwrap();
    let (row, col) = app.active_editor().cursor();
    assert_eq!(
        row, 2,
        "+/main must land on the FIRST forward match (row 2), got row {row}"
    );
    assert_eq!(col, 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn plus_slash_argv_search_with_goto_line_searches_forward() {
    // hjkl +5 +/main file.rs : goto_line first, then search forward.
    use std::io::Write;
    let dir = std::env::temp_dir().join("hjkl_plus_slash_goto_test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sample.txt");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "main early").unwrap(); // row 0
        writeln!(f, "two").unwrap();
        writeln!(f, "three").unwrap();
        writeln!(f, "four").unwrap();
        writeln!(f, "five").unwrap(); // goto_line(5) lands here (1-based row 4)
        writeln!(f, "six").unwrap();
        writeln!(f, "main mid").unwrap(); // row 6
        writeln!(f, "main late").unwrap(); // row 7
    }
    // +5 goto_line=5 then +/main forward search. Should land on row 6,
    // NOT wrap back to row 0.
    let app = App::new(Some(path.clone()), false, Some(5), Some("main".into())).unwrap();
    let (row, _col) = app.active_editor().cursor();
    assert_eq!(
        row, 6,
        "+5 +/main must search forward from row 4, landing on row 6"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn plus_slash_argv_persists_forward_direction_for_n() {
    // Regression: `hjkl +/keyword file` did not call set_last_search,
    // so vim.last_search_forward stayed at its bool default (false).
    // The next `n` then computed forward = false != false = false and
    // jumped BACKWARD as if `?keyword<CR>` had been typed.
    use hjkl_engine::{Input, Key};
    use std::io::Write;
    let dir = std::env::temp_dir().join("hjkl_plus_slash_n_dir");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sample.txt");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "alpha").unwrap(); // 0
        writeln!(f, "beta").unwrap(); // 1
        writeln!(f, "main one").unwrap(); // 2 — first match
        writeln!(f, "delta").unwrap(); // 3
        writeln!(f, "main two").unwrap(); // 4 — `n` should jump here
        writeln!(f, "main three").unwrap(); // 5
    }
    let mut app = App::new(Some(path.clone()), false, None, Some("main".into())).unwrap();
    let (row0, _) = app.active_editor().cursor();
    assert_eq!(row0, 2, "+/main must land on first match (row 2)");
    // last_search must be persisted so `n` knows the pattern.
    assert_eq!(app.active_editor().last_search(), Some("main".to_string()));
    // Drive `n` through the engine vim FSM and assert FORWARD jump.
    let n_input = Input {
        key: Key::Char('n'),
        ..Default::default()
    };
    hjkl_vim::dispatch_input(app.active_editor_mut(), n_input);
    let (row1, _) = app.active_editor().cursor();
    assert_eq!(
        row1, 4,
        "after +/main, `n` must advance FORWARD to row 4 (got row {row1}); \
        backward would land on row 0 (no match) or stay/regress"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn search_count_cursor_on_match_stays_on_match() {
    // Regression: /<pat><CR> from a cursor that's already ON a match used
    // to advance past it (counter 1/3 → 2/3). Vim semantics: /<CR> finds
    // the first match AT-OR-AFTER the cursor — only `n` advances.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "foo X foo X foo");
    {
        let vp = app.active_editor_mut().host_mut().viewport_mut();
        vp.height = 5;
        vp.top_row = 0;
    }
    // Cursor at (0,0) — exactly on the first 'foo'.
    app.commit_search("foo");
    assert_eq!(
        crate::render::search_count(&app),
        Some((1, 3)),
        "/<pat><CR> from cursor on a match must keep counter at 1/3, \
        not advance to 2/3"
    );
}

#[test]
fn search_count_n_press_increments_by_one() {
    // After /foo<CR> lands on M1, pressing n should advance to M2 (counter 2/3).
    // If counter skips to 3/3, the n-jump is double-stepping.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "X foo X foo X foo");
    {
        let vp = app.active_editor_mut().host_mut().viewport_mut();
        vp.height = 5;
        vp.top_row = 0;
    }
    app.commit_search("foo");
    assert_eq!(crate::render::search_count(&app), Some((1, 3)));
    // Now drive `n` via the engine.
    app.active_editor_mut().search_advance_forward(true);
    assert_eq!(
        crate::render::search_count(&app),
        Some((2, 3)),
        "n must advance counter from 1/3 to 2/3, not skip"
    );
    app.active_editor_mut().search_advance_forward(true);
    assert_eq!(crate::render::search_count(&app), Some((3, 3)));
}

#[test]
fn search_count_handles_multibyte_chars_before_match() {
    // Regression: search_count compared cursor_col (char index) against
    // m.start() (byte offset). A match on a line with multi-byte chars
    // before it (e.g. an em-dash in a doc comment) had byte > char, so
    // the inequality `(row, byte) <= (row, char)` falsely excluded the
    // match the cursor was sitting on — counter showed 0/N instead of 1/N.
    //
    // Real-world repro: `/main` in apps/hjkl/src/main.rs landed on a
    // line "/// surface them — `main` prints …" with an em-dash and
    // showed [0/6] on commit, then [2/6] after one `n` press.
    let mut app = App::new(None, false, None, None).unwrap();
    // Two matches; first sits behind a multi-byte em-dash.
    seed_buffer(&mut app, "alpha\n/// — main one\nbeta\nmain two");
    {
        let vp = app.active_editor_mut().host_mut().viewport_mut();
        vp.height = 10;
        vp.top_row = 0;
    }
    app.commit_search("main");
    assert_eq!(
        crate::render::search_count(&app),
        Some((1, 2)),
        "/main must land on M1 with counter 1/2, even when M1 sits \
        behind a multi-byte char (em-dash) on its line"
    );
    // n -> M2 -> 2/2.
    app.active_editor_mut().search_advance_forward(true);
    assert_eq!(crate::render::search_count(&app), Some((2, 2)));
}

#[test]
fn search_count_through_full_key_flow() {
    // Regression: simulate the actual key path / -> 'f' -> 'o' -> 'o' -> Enter.
    // Counter must end at 1/3 (or N/3 with N=1), never skipping past 1.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "X foo X foo X foo");
    {
        let vp = app.active_editor_mut().host_mut().viewport_mut();
        vp.height = 5;
        vp.top_row = 0;
    }
    // Open / prompt.
    app.open_search_prompt(crate::app::SearchDir::Forward);
    // Type 'f' 'o' 'o' through handle_search_field_key.
    for ch in ['f', 'o', 'o'] {
        let key = KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE);
        app.handle_search_field_key(key);
    }
    // During typing the counter should be 0/3 (cursor before all matches).
    let count = crate::render::search_count(&app);
    assert_eq!(count, Some((0, 3)), "during typing, counter must be 0/3");
    // Submit.
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    app.handle_search_field_key(enter);
    // After submit the counter must show 1/3, NOT 2/3.
    let count = crate::render::search_count(&app);
    assert_eq!(
        count,
        Some((1, 3)),
        "after / submit, counter must be 1/3 — bug was 2/3"
    );
}

#[test]
fn search_count_after_commit_lands_on_first_match() {
    // Regression: `/<pat><CR>` from a non-match cursor was incrementing
    // the match counter to 2 (skipping 1) because commit_search passed
    // skip_current=true even on the first jump.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "X foo X foo X foo");
    // Cursor at (0,0), 'X' — before all matches.
    {
        let vp = app.active_editor_mut().host_mut().viewport_mut();
        vp.height = 5;
        vp.top_row = 0;
    }
    // Submit `/foo<CR>` programmatically.
    app.commit_search("foo");
    // Counter should now show 1/3 (first match), not 2/3.
    let count = crate::render::search_count(&app);
    assert_eq!(
        count,
        Some((1, 3)),
        "/{{pat}}<CR> from a non-match cursor must land on match 1, not skip to 2"
    );
}

#[test]
fn lsp_jump_reveals_cursor_in_viewport() {
    // Regression: jump_cursor only sets cursor; without ensure_cursor_in_
    // scrolloff afterwards, the viewport stays parked and the cursor lands
    // off-screen. Plant a diag past the visible area, jump, assert the
    // window's stored top_row scrolled.
    use crate::app::window::WindowId;

    let mut app = App::new(None, false, None, None).unwrap();
    let path = tmp_path("hjkl_jump_scroll.rs");
    app.active_mut().filename = Some(path.clone());

    // 100 lines of content so a row-50 jump is well past any default
    // viewport.
    let lines: Vec<String> = (0..100).map(|i| format!("line {i}")).collect();
    seed_buffer(&mut app, &lines.join("\n"));

    // Set the focused window's viewport height + reset scroll so we can
    // observe whether jump scrolls.
    {
        let vp = app.active_editor_mut().host_mut().viewport_mut();
        vp.height = 20;
        vp.top_row = 0;
    }
    let fw: WindowId = app.focused_window();
    if let Some(_w) = app.windows[fw].as_mut() {}

    // Plant a diagnostic on row 50 and jump to it.
    let params = pub_diags_params(
        &file_url(&path),
        serde_json::json!([{
            "range": { "start": { "line": 50, "character": 0 }, "end": { "line": 50, "character": 1 } },
            "severity": 1,
            "message": "deep"
        }]),
    );
    app.handle_publish_diagnostics(params, hjkl_lsp::PositionEncoding::Utf8);
    app.lnext_severity(None);

    // Cursor must be at row 50 AND the viewport must have scrolled past
    // the original top_row=0 so the cursor is visible.
    let (row, _) = app.active_editor().cursor();
    assert_eq!(row, 50);
    let vp_top = app.active_editor().host().viewport().top_row;
    assert!(
        vp_top > 0,
        "viewport top_row stayed at 0 after jump — ensure_cursor_in_scrolloff not called"
    );
    let stored_top = app.window_scroll(fw).0;
    assert!(
        stored_top > 0,
        "focused window's stored top_row stayed at 0 — sync_viewport_from_editor missed the scroll"
    );
}

#[test]
fn lprev_jumps_to_prev_diag_with_wrap() {
    let mut app = App::new(None, false, None, None).unwrap();
    let path = tmp_path("hjkl_lprev.rs");
    app.active_mut().filename = Some(path.clone());
    seed_buffer(&mut app, "a\nb\nc\nd");

    let params = pub_diags_params(
        &file_url(&path),
        serde_json::json!([
            {
                "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 1 } },
                "severity": 1,
                "message": "first"
            },
            {
                "range": { "start": { "line": 2, "character": 1 }, "end": { "line": 2, "character": 2 } },
                "severity": 2,
                "message": "second"
            }
        ]),
    );
    app.handle_publish_diagnostics(params, hjkl_lsp::PositionEncoding::Utf8);

    // Cursor at row 0 col 0 — lprev should wrap to the last diag (row 2).
    app.lprev_severity(None);
    let (row, _) = app.active_editor().cursor();
    assert_eq!(row, 2, "lprev from first diag must wrap to last");

    // Cursor now at row 2 — lprev should jump to row 0.
    app.lprev_severity(None);
    let (row, _) = app.active_editor().cursor();
    assert_eq!(row, 0, "lprev must jump to previous diag");
}

#[test]
fn lnext_severity_skips_lower_severity() {
    let mut app = App::new(None, false, None, None).unwrap();
    let path = tmp_path("hjkl_lnext_sev.rs");
    app.active_mut().filename = Some(path.clone());
    seed_buffer(&mut app, "a\nb\nc");

    // Row 1: Warning, Row 2: Error.
    let params = pub_diags_params(
        &file_url(&path),
        serde_json::json!([
            {
                "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 1 } },
                "severity": 2,
                "message": "warn"
            },
            {
                "range": { "start": { "line": 2, "character": 0 }, "end": { "line": 2, "character": 1 } },
                "severity": 1,
                "message": "err"
            }
        ]),
    );
    app.handle_publish_diagnostics(params, hjkl_lsp::PositionEncoding::Utf8);

    // Jump to Error-only — must skip Warning on row 1 and land on row 2.
    app.lnext_severity(Some(DiagSeverity::Error));
    let (row, _) = app.active_editor().cursor();
    assert_eq!(row, 2, "lnext with Error filter must skip Warning diags");
}

#[test]
fn lopen_shows_no_diags_message_when_empty() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.open_diag_picker();
    // No diagnostics — picker must not open; status message set.
    assert!(app.picker.is_none(), "picker must not open when no diags");
    let msg = app.bus.last_body_or_empty().to_string();
    assert!(
        msg.contains("no diagnostics"),
        "expected 'no diagnostics', got: {msg}"
    );
}

#[test]
fn lopen_lists_diags_in_picker() {
    let mut app = App::new(None, false, None, None).unwrap();
    let path = tmp_path("hjkl_lopen.rs");
    app.active_mut().filename = Some(path.clone());
    seed_buffer(&mut app, "a\nb");

    let params = pub_diags_params(
        &file_url(&path),
        serde_json::json!([{
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 1 } },
            "severity": 1,
            "message": "some error"
        }]),
    );
    app.handle_publish_diagnostics(params, hjkl_lsp::PositionEncoding::Utf8);

    app.open_diag_picker();
    assert!(app.picker.is_some(), "picker must open when diags exist");
}

#[test]
fn lsp_info_with_lsp_disabled_sets_status() {
    let mut app = App::new(None, false, None, None).unwrap();
    // self.lsp is None by default — :LspInfo shows the disabled state.
    app.show_lsp_info();
    let popup_content = app
        .info_popup
        .as_ref()
        .map(|p| p.content.as_str())
        .unwrap_or_default();
    assert!(
        popup_content.contains("LSP: disabled"),
        "expected 'LSP: disabled' message, got: {popup_content}"
    );
}

#[test]
fn lsp_info_lists_running_servers() {
    let mut app = App::new(None, false, None, None).unwrap();
    // Need an LspManager attached so :LspInfo doesn't show "disabled".
    app.lsp = Some(hjkl_lsp::LspManager::spawn(hjkl_lsp::LspConfig::default()));
    // Manually insert a fake server into lsp_state.
    let key = hjkl_lsp::ServerKey {
        language: "rust".into(),
        root: tmp_path("proj"),
    };
    app.lsp_state.insert(
        key,
        LspServerInfo {
            initialized: true,
            capabilities: serde_json::json!({}),
        },
    );

    app.show_lsp_info();
    assert!(
        app.info_popup.is_some(),
        "popup must open when LSP is enabled"
    );
    let popup = app.info_popup.as_ref().unwrap();
    assert!(
        popup.content.contains("rust"),
        "popup must mention server language"
    );
    assert!(
        popup.content.contains("initialized"),
        "popup must show server state"
    );
    if let Some(mgr) = app.lsp.take() {
        mgr.shutdown();
    }
}

#[test]
fn notify_change_skipped_when_dirty_gen_unchanged() {
    // Without a real LspManager we can't exercise the full path, but we
    // *can* verify that the last_lsp_dirty_gen guard does not reset on
    // repeated calls with no edits: the gen stays the same, so the
    // second call would be a no-op (it would return early). We assert
    // the guard value is set correctly after a manual seed.
    let mut app = App::new(None, false, None, None).unwrap();
    // No LSP manager attached — lsp_notify_change_active returns early.
    // Manually set last_lsp_dirty_gen to simulate a prior send.
    let dg = app.active_editor().buffer().dirty_gen();
    app.active_mut().last_lsp_dirty_gen = Some(dg);

    // Call again — must not panic and must not reset the guard.
    app.lsp_notify_change_active(&[]);
    assert_eq!(
        app.active().last_lsp_dirty_gen,
        Some(dg),
        "guard must remain unchanged when no LSP manager"
    );
}

/// Regression: `:s/foo/bar/` on an LSP-attached buffer must notify the
/// server via `textDocument/didChange` (through `lsp_notify_change_active`),
/// same as every other engine-mutation path. Before the fix, the
/// `ExEffect::Substituted` handler drained the ContentEdit batch into the
/// syntax layer but never called `lsp_notify_change_active`, so the edits
/// were lost from the log without ever reaching the server — the server's
/// document silently desynced and stayed that way (a later full-sync
/// wouldn't help, but the *next* incremental edit would apply against a
/// stale base and corrupt the server's copy permanently).
#[test]
fn substitute_notifies_lsp() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.lsp = Some(hjkl_lsp::LspManager::spawn(hjkl_lsp::LspConfig::default()));
    app.active_mut().filename = Some(tmp_path("hjkl_substitute_lsp.rs"));
    seed_buffer(&mut app, "foo bar");
    // Precondition: no didChange sent yet.
    assert_eq!(app.active().last_lsp_dirty_gen, None);

    app.dispatch_ex("s/foo/baz/");

    let dg = app.active_editor().buffer().dirty_gen();
    assert_eq!(
        app.active().last_lsp_dirty_gen,
        Some(dg),
        ":s must drive lsp_notify_change_active so the server's document \
        stays in sync with the buffer"
    );

    if let Some(mgr) = app.lsp.take() {
        mgr.shutdown();
    }
}

// ── Phase 3: goto + hover tests ────────────────────────────────────────────

#[test]
fn goto_definition_single_jumps_cursor() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "line0\nline1\nline2\nline3");
    // Give the active slot a path so the location URI matches.
    let path = tmp_path("hjkl_gd_single.rs");
    app.active_mut().filename = Some(path.clone());
    let uri = file_url(&path);

    let loc = make_location(&uri, 2, 0);
    let result = ok_val(serde_json::to_value(vec![loc]).unwrap());
    let buffer_id = app.active().buffer_id as hjkl_lsp::BufferId;
    app.handle_goto_response(
        buffer_id,
        (0, 0),
        result,
        "definition",
        hjkl_lsp::PositionEncoding::Utf8,
    );

    // Cursor must have moved to row 2.
    assert_eq!(app.active_editor().buffer().cursor().row, 2);
    assert!(app.picker.is_none(), "single result must not open picker");
}

#[test]
fn goto_definition_empty_sets_status() {
    let mut app = App::new(None, false, None, None).unwrap();
    let result = ok_val(serde_json::Value::Null);
    let buffer_id = app.active().buffer_id as hjkl_lsp::BufferId;
    app.handle_goto_response(
        buffer_id,
        (0, 0),
        result,
        "definition",
        hjkl_lsp::PositionEncoding::Utf8,
    );

    let msg = app.bus.last_body_or_empty();
    assert!(
        msg.contains("no definition found"),
        "expected 'no definition found', got: {msg}"
    );
    assert!(app.picker.is_none());
}

#[test]
fn goto_definition_multi_opens_picker() {
    let mut app = App::new(None, false, None, None).unwrap();
    // Use platform-aware URIs so Windows CI runners don't strip drive letters.
    let locs = vec![
        make_location(&file_url(&tmp_path("hjkl_gd_multi_a.rs")), 0, 0),
        make_location(&file_url(&tmp_path("hjkl_gd_multi_b.rs")), 5, 3),
        make_location(&file_url(&tmp_path("hjkl_gd_multi_c.rs")), 10, 1),
    ];
    let result = ok_val(serde_json::to_value(locs).unwrap());
    let buffer_id = app.active().buffer_id as hjkl_lsp::BufferId;
    app.handle_goto_response(
        buffer_id,
        (0, 0),
        result,
        "definition",
        hjkl_lsp::PositionEncoding::Utf8,
    );

    assert!(app.picker.is_some(), "multiple results must open picker");
}

#[test]
fn goto_references_always_opens_picker() {
    let mut app = App::new(None, false, None, None).unwrap();
    // Single result — references always opens picker.
    // Use platform-aware URI so Windows CI runners don't strip drive letters.
    let locs = vec![make_location(&file_url(&tmp_path("hjkl_gd_only.rs")), 3, 0)];
    let result = ok_val(serde_json::to_value(locs).unwrap());
    let buffer_id = app.active().buffer_id as hjkl_lsp::BufferId;
    app.handle_references_response(buffer_id, (0, 0), result, hjkl_lsp::PositionEncoding::Utf8);

    assert!(app.picker.is_some(), "references must always open picker");
}

#[test]
fn hover_response_sets_hover_popup() {
    let mut app = App::new(None, false, None, None).unwrap();
    let hover = lsp_types::Hover {
        contents: lsp_types::HoverContents::Markup(lsp_types::MarkupContent {
            kind: lsp_types::MarkupKind::Markdown,
            value: "**fn** foo() -> i32".to_string(),
        }),
        range: None,
    };
    let result = ok_val(serde_json::to_value(hover).unwrap());
    let buffer_id = app.active().buffer_id as hjkl_lsp::BufferId;
    app.handle_hover_response(buffer_id, (0, 0), result);

    // K-key hover now uses the compact, cursor-anchored hover_popup (the same
    // widget mouse hover uses), not the 80%×60% info_popup modal.
    assert!(
        app.info_popup.is_none(),
        "K hover should no longer use the info_popup modal"
    );
    let popup = app
        .hover_popup
        .as_ref()
        .expect("hover must set hover_popup");
    assert!(
        popup.content.contains("foo"),
        "hover_popup must contain the function name"
    );
}

#[test]
fn hover_empty_sets_status() {
    let mut app = App::new(None, false, None, None).unwrap();
    let result: Result<serde_json::Value, hjkl_lsp::RpcError> = Ok(serde_json::Value::Null);
    let buffer_id = app.active().buffer_id as hjkl_lsp::BufferId;
    app.handle_hover_response(buffer_id, (0, 0), result);

    let msg = app.bus.last_body_or_empty();
    assert!(
        msg.contains("no hover info"),
        "expected 'no hover info', got: {msg}"
    );
    assert!(app.info_popup.is_none());
}

#[test]
fn goto_definition_error_sets_status() {
    let mut app = App::new(None, false, None, None).unwrap();
    let result = err_val("server error");
    let buffer_id = app.active().buffer_id as hjkl_lsp::BufferId;
    app.handle_goto_response(
        buffer_id,
        (0, 0),
        result,
        "definition",
        hjkl_lsp::PositionEncoding::Utf8,
    );

    let msg = app.bus.last_body_or_empty();
    assert!(
        msg.contains("server error"),
        "expected error message, got: {msg}"
    );
}

#[test]
fn k_dispatches_hover() {
    // Without a real LspManager the call returns early with a status hint.
    let mut app = App::new(None, false, None, None).unwrap();
    app.active_mut().filename = Some(tmp_path("k_test.rs"));
    app.lsp_hover();
    assert!(app.info_popup.is_none());
    let msg = app.bus.last_body_or_empty();
    assert!(msg.contains("LSP: not enabled"), "got: {msg}");
}

#[test]
fn gd_dispatches_goto_definition() {
    // Without a real LspManager the call returns early (no panic).
    let mut app = App::new(None, false, None, None).unwrap();
    app.active_mut().filename = Some(tmp_path("gd_test.rs"));
    app.lsp_goto_definition();
    // No LSP — nothing pending, no crash.
    assert!(app.lsp_pending.is_empty());
}

#[test]
fn lsp_request_works_with_relative_filename() {
    // Regression: opening hjkl with a relative path like
    // `apps/hjkl/src/main.rs` used to silently fail to attach to the LSP
    // server because url::Url::from_file_path requires absolute paths.
    // The absolutize() helper now joins relative paths against
    // current_dir() before URI conversion.
    let mut app = App::new(None, false, None, None).unwrap();
    let mgr = hjkl_lsp::LspManager::spawn(hjkl_lsp::LspConfig::default());
    app.lsp = Some(mgr);
    app.active_mut().filename = Some(std::path::PathBuf::from("src/main.rs"));
    // Requests are gated on an initialized server advertising the capability,
    // so register a rust server that supports goto-definition.
    app.lsp_state.insert(
        hjkl_lsp::ServerKey {
            language: "rust".into(),
            root: tmp_path("proj"),
        },
        LspServerInfo {
            initialized: true,
            capabilities: serde_json::json!({ "definitionProvider": true }),
        },
    );
    app.lsp_goto_definition();
    // Request was registered as pending — absolutize made URI conversion
    // succeed even though the buffer's filename is relative.
    assert_eq!(
        app.lsp_pending.len(),
        1,
        "relative-path goto must produce a pending request, not the \
        'no file open' error path"
    );
    if let Some(mgr) = app.lsp.take() {
        mgr.shutdown();
    }
}

// ── Phase 4: completion popup tests ────────────────────────────────────────

#[test]
fn completion_response_opens_popup() {
    let mut app = App::new(None, false, None, None).unwrap();
    // Enter insert mode so the guard passes.
    hjkl_vim_tui::handle_key(app.active_editor_mut(), key(KeyCode::Char('i')));
    // Give the buffer a filename so buffer_id matches.
    app.active_mut().filename = Some(tmp_path("test.rs"));
    let buffer_id = app.active().buffer_id as hjkl_lsp::BufferId;

    let response_val = synthesize_completion_response(&["foo", "bar", "baz"]);
    app.handle_completion_response(buffer_id, 0, 0, false, Ok(response_val));

    assert!(app.completion.is_some(), "popup should open");
    let popup = app.completion.as_ref().unwrap();
    assert_eq!(popup.all_items.len(), 3);
    assert_eq!(popup.visible.len(), 3);
}

#[test]
fn completion_response_empty_no_popup() {
    let mut app = App::new(None, false, None, None).unwrap();
    hjkl_vim_tui::handle_key(app.active_editor_mut(), key(KeyCode::Char('i')));
    app.active_mut().filename = Some(tmp_path("test.rs"));
    let buffer_id = app.active().buffer_id as hjkl_lsp::BufferId;

    // Empty list response.
    let response_val = serde_json::json!([]);
    app.handle_completion_response(buffer_id, 0, 0, false, Ok(response_val));

    assert!(
        app.completion.is_none(),
        "empty response must not open popup"
    );
    assert!(
        app.bus.last_body_or_empty().contains("no completions"),
        "status should report no completions"
    );
}

#[test]
fn completion_request_pending_routes_to_handler() {
    let mut app = App::new(None, false, None, None).unwrap();
    // Simulate a pending completion request.
    hjkl_vim_tui::handle_key(app.active_editor_mut(), key(KeyCode::Char('i')));
    app.active_mut().filename = Some(tmp_path("test.rs"));
    let buffer_id = app.active().buffer_id as hjkl_lsp::BufferId;

    // Insert a fake pending request.
    let req_id = app.lsp_alloc_request_id();
    app.lsp_pending.insert(
        req_id,
        LspPendingRequest::Completion {
            buffer_id,
            anchor_row: 0,
            anchor_col: 0,
            auto: false,
        },
    );

    // Simulate receiving a response.
    let response_val = synthesize_completion_response(&["alpha", "beta"]);
    let pending = app.lsp_pending.remove(&req_id).unwrap();
    app.handle_lsp_response(pending, Ok(response_val));

    assert!(
        app.completion.is_some(),
        "response must route to popup opener"
    );
    let popup = app.completion.as_ref().unwrap();
    assert_eq!(popup.all_items.len(), 2);
}

#[test]
fn identifier_start_col_snaps_to_word_boundary() {
    let mut app = App::new(None, false, None, None).unwrap();
    // "compute" begins at CHAR col 10; cursor at end of line (all-ASCII, so
    // char and byte columns coincide here).
    seed_buffer(&mut app, "let val = compute");
    assert_eq!(
        app.identifier_start_col(0, "let val = compute".chars().count()),
        10,
        "anchor should snap to the start of the identifier under the cursor"
    );
    // Mid-word: cursor after "comp" still anchors at the word start.
    assert_eq!(app.identifier_start_col(0, 14), 10);
    // Right after a non-identifier char (e.g. a `.`), anchor stays at cursor
    // so member completion behaves exactly as before.
    seed_buffer(&mut app, "obj.");
    assert_eq!(app.identifier_start_col(0, 4), 4);
}

/// Regression (audit-r2 fix 7): `col` is a CHAR column (matching
/// `View::cursor().col`), not a byte column — treating it as a byte offset
/// silently mis-scanned the identifier boundary on any line with multibyte
/// content before the cursor.
#[test]
fn identifier_start_col_multibyte_prefix_scans_correctly() {
    let mut app = App::new(None, false, None, None).unwrap();
    // "héllo wor" = 9 chars (h,é,l,l,o,' ',w,o,r) but 10 bytes (é is 2
    // bytes) — a byte-offset bug would slice one char short and miss the
    // trailing 'r', still anchoring at 'w' here but truncating the token
    // (see `token_between_multibyte_prefix_returns_full_token` below).
    seed_buffer(&mut app, "héllo wor");
    let cursor_col = "héllo wor".chars().count(); // 9
    assert_eq!(
        app.identifier_start_col(0, cursor_col),
        6,
        "anchor must land on 'w' (char col 6), not shifted by é's extra byte"
    );

    // The identifier itself contains the multibyte char: cursor right after
    // "héllo" must anchor at its start (char col 0), not misplaced by é's
    // extra byte the way a byte-offset scan would.
    seed_buffer(&mut app, "héllo");
    assert_eq!(app.identifier_start_col(0, "héllo".chars().count()), 0);
}

#[test]
fn identifier_start_col_out_of_range_col_clamps_without_panicking() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "\u{f1617} buffer_ops.rs");
    // A col past the line's char count clamps safely rather than panicking.
    let _ = app.identifier_start_col(0, 9999);
}

/// Regression (audit-r2 fix 7): companion to
/// `identifier_start_col_multibyte_prefix_scans_correctly` — the full
/// completion-prefix path (`identifier_start_col` anchor feeding
/// `token_between`) must recover the WHOLE identifier, not a byte-shortened
/// prefix, when multibyte content precedes the cursor.
#[test]
fn token_between_multibyte_prefix_returns_full_token() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "héllo wor");
    let cursor_col = "héllo wor".chars().count();
    let anchor = app.identifier_start_col(0, cursor_col);
    assert_eq!(
        app.token_between(0, anchor, cursor_col),
        "wor",
        "must recover the full typed prefix, not truncated by é's extra byte"
    );

    seed_buffer(&mut app, "héllo");
    let cursor_col = "héllo".chars().count();
    let anchor = app.identifier_start_col(0, cursor_col);
    assert_eq!(app.token_between(0, anchor, cursor_col), "héllo");
}

#[test]
fn token_between_out_of_range_cols_clamp_without_panicking() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "ab — cd"); // em-dash U+2014, CHAR col 3
    assert_eq!(app.token_between(0, 0, 4), "ab —");
    // Out-of-range columns clamp safely rather than panicking.
    let _ = app.token_between(0, 0, 9999);
    let _ = app.token_between(0, 9999, 9999);
}

#[test]
fn accept_completion_inserts_selected_item() {
    let mut app = App::new(None, false, None, None).unwrap();
    // Seed buffer with some text and enter insert mode at col 0.
    seed_buffer(&mut app, "fn foo");
    hjkl_vim_tui::handle_key(app.active_editor_mut(), key(KeyCode::Char('i')));
    // Open popup anchored at col 0 row 0 with two items.
    let items = vec![make_completion_item("hello"), make_completion_item("world")];
    app.completion = Some(crate::completion::Completion::new(0, 0, items));
    // Select second item.
    app.completion.as_mut().unwrap().selected = 1;

    app.accept_completion();
    app.sync_after_engine_mutation();

    // Popup must be gone.
    assert!(app.completion.is_none());
    // View line should start with "world" (inserted at col 0).
    let line = hjkl_buffer::rope_line_str(&app.active_editor().buffer().rope(), 0);
    assert!(
        line.starts_with("world"),
        "buffer line should start with inserted text, got: {line:?}"
    );
    // Sync footer must have drained dirty + content_edits.
    assert!(
        !app.active_editor_mut().take_dirty(),
        "accept_completion call site must drain dirty via sync_after_engine_mutation"
    );
    assert!(
        app.active_editor_mut().take_content_edits().is_empty(),
        "accept_completion call site must drain content_edits"
    );
}

#[test]
fn active_comment_lead_matches_language() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.active_mut().filename = Some(tmp_path("x.rs"));
    assert_eq!(app.active_comment_lead(), "//", "rust");
    app.active_mut().filename = Some(tmp_path("x.py"));
    assert_eq!(app.active_comment_lead(), "#", "python");
    app.active_mut().filename = Some(tmp_path("x.js"));
    assert_eq!(app.active_comment_lead(), "//", "javascript");
    // Unknown / no extension falls back to `//`.
    app.active_mut().filename = Some(tmp_path("x.unknownext"));
    assert_eq!(app.active_comment_lead(), "//", "unknown fallback");
}

#[test]
fn buffer_word_items_collects_unique_identifiers() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "let foo = bar;\nfoo_bar(foo, 123, x)");
    let items = app.buffer_word_items("");
    let labels: std::collections::HashSet<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(labels.contains("foo"), "labels: {labels:?}");
    assert!(labels.contains("bar"));
    assert!(labels.contains("foo_bar"));
    assert!(labels.contains("let"));
    assert!(
        !labels.contains("x"),
        "single-char tokens excluded (len < 2)"
    );
    assert!(!labels.contains("123"), "digit-leading tokens excluded");
    // "foo" appears twice in the source but must be listed once.
    assert_eq!(items.iter().filter(|i| i.label == "foo").count(), 1);
}

#[test]
fn buffer_word_items_excludes_current_token() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "alpha beta alpha");
    let items = app.buffer_word_items("alpha");
    assert!(
        !items.iter().any(|i| i.label == "alpha"),
        "the word being typed must not suggest itself"
    );
    assert!(items.iter().any(|i| i.label == "beta"));
}

#[test]
fn accept_completion_records_content_edit_for_resync() {
    // Regression (#143): accepting a completion must go through the editor's
    // tracked mutation funnel so a `ContentEdit` is recorded. Otherwise the
    // tree-sitter tree and the LSP server never learn about the change and
    // stale parse-error / diagnostic gutter signs linger.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "fn foo");
    hjkl_vim_tui::handle_key(app.active_editor_mut(), key(KeyCode::Char('i')));
    app.completion = Some(crate::completion::Completion::new(
        0,
        0,
        vec![make_completion_item("world")],
    ));
    app.accept_completion();
    // Before any sync drains them, the edit must be present for fan-out.
    let edits = app.active_editor_mut().take_content_edits();
    assert!(
        !edits.is_empty(),
        "accept_completion must record a ContentEdit so syntax + LSP resync"
    );
}

#[test]
fn accept_function_completion_places_cursor_in_parens() {
    // Bare name "foo" with kind=Function → inserts "foo()" with cursor between parens.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "");
    hjkl_vim_tui::handle_key(app.active_editor_mut(), key(KeyCode::Char('i')));

    // Anchor at col 0, row 0; insert_text = "foo", kind = Function.
    let mut item = crate::completion::CompletionItem::new("foo");
    item.kind = crate::completion::CompletionKind::Function;
    app.completion = Some(crate::completion::Completion::new(0, 0, vec![item]));

    app.accept_completion();
    app.sync_after_engine_mutation();

    assert!(app.completion.is_none(), "popup must be dismissed");
    let line = hjkl_buffer::rope_line_str(&app.active_editor().buffer().rope(), 0);
    assert_eq!(
        line.trim_end_matches('\n'),
        "foo()",
        "buffer must contain 'foo()' after function completion"
    );
    // Cursor must sit between the parens: anchor_col(0) + len("foo") + 1 = col 4.
    let (_, col) = app.active_editor().cursor();
    assert_eq!(
        col, 4,
        "cursor must be inside parens at col 4, got col {col}"
    );
}

#[test]
fn accept_function_completion_with_existing_parens() {
    // insert_text already contains "bar()" → cursor placed between existing parens.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "");
    hjkl_vim_tui::handle_key(app.active_editor_mut(), key(KeyCode::Char('i')));

    let mut item = crate::completion::CompletionItem::new("bar()");
    item.kind = crate::completion::CompletionKind::Method;
    item.label = "bar".to_string();
    app.completion = Some(crate::completion::Completion::new(0, 0, vec![item]));

    app.accept_completion();
    app.sync_after_engine_mutation();

    assert!(app.completion.is_none(), "popup must be dismissed");
    let line = hjkl_buffer::rope_line_str(&app.active_editor().buffer().rope(), 0);
    assert_eq!(
        line.trim_end_matches('\n'),
        "bar()",
        "buffer must contain 'bar()' without double-parens"
    );
    // `(` is at byte offset 3 in "bar()" → cursor at anchor_col(0) + 3 + 1 = col 4.
    let (_, col) = app.active_editor().cursor();
    assert_eq!(
        col, 4,
        "cursor must be inside parens at col 4, got col {col}"
    );
}

#[test]
fn accept_variable_completion_cursor_at_end() {
    // kind=Variable → no parens added, cursor at end of inserted text.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "");
    hjkl_vim_tui::handle_key(app.active_editor_mut(), key(KeyCode::Char('i')));

    let mut item = crate::completion::CompletionItem::new("x");
    item.kind = crate::completion::CompletionKind::Variable;
    app.completion = Some(crate::completion::Completion::new(0, 0, vec![item]));

    app.accept_completion();
    app.sync_after_engine_mutation();

    assert!(app.completion.is_none(), "popup must be dismissed");
    let line = hjkl_buffer::rope_line_str(&app.active_editor().buffer().rope(), 0);
    assert_eq!(
        line.trim_end_matches('\n'),
        "x",
        "buffer must contain 'x' with no parens added"
    );
    // Cursor at end: anchor_col(0) + len("x") = col 1.
    let (_, col) = app.active_editor().cursor();
    assert_eq!(
        col, 1,
        "cursor must be at end (col 1) for non-function completion"
    );
}

#[test]
fn dismiss_completion_clears_state() {
    let mut app = App::new(None, false, None, None).unwrap();
    let items = vec![make_completion_item("foo")];
    app.completion = Some(crate::completion::Completion::new(0, 0, items));
    app.pending_ctrl_x = true;

    app.dismiss_completion();

    assert!(app.completion.is_none());
    assert!(!app.pending_ctrl_x);
}

#[test]
fn set_prefix_dismisses_when_filter_empty() {
    // Open popup, set prefix that matches nothing → popup auto-dismisses.
    let items = vec![make_completion_item("alpha"), make_completion_item("beta")];
    let mut popup = crate::completion::Completion::new(0, 0, items);
    popup.set_prefix("xyz");
    assert!(
        popup.is_empty(),
        "popup should be empty after non-matching prefix"
    );
}

// ── Phase 5 LSP tests ────────────────────────────────────────────────────

#[test]
#[allow(clippy::mutable_key_type)]
fn apply_workspace_edit_single_file() {
    let path = std::env::temp_dir().join("hjkl_ws_edit_single.txt");
    std::fs::write(&path, "hello world\n").unwrap();
    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();

    let uri = file_url(&path);
    let edit = make_workspace_edit(&uri, 0, 6, 0, 11, "rust");
    let count = app
        .apply_workspace_edit(edit, hjkl_lsp::PositionEncoding::Utf8)
        .expect("apply_workspace_edit failed");
    assert_eq!(count, 1);

    let lines = app
        .active_editor()
        .buffer()
        .rope()
        .lines()
        .map(|s| {
            let s = s.to_string();
            s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        lines[0], "hello rust",
        "edit should replace 'world' with 'rust'"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
#[allow(clippy::mutable_key_type)]
fn apply_workspace_edit_sorts_edits_descending() {
    // Two edits on the same line: first edit at col 0-3, second at col 6-11.
    // If applied in forward order the offsets shift; descending order must give correct result.
    let path = std::env::temp_dir().join("hjkl_ws_edit_sort.txt");
    std::fs::write(&path, "hello world foo\n").unwrap();
    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();

    let url = file_url(&path)
        .parse::<lsp_types::Uri>()
        .expect("valid URI");
    let mut changes = std::collections::HashMap::new();
    changes.insert(
        url,
        vec![
            // Edit 1: replace "hello" (0-5) with "hi"
            lsp_types::TextEdit {
                range: lsp_types::Range {
                    start: lsp_types::Position {
                        line: 0,
                        character: 0,
                    },
                    end: lsp_types::Position {
                        line: 0,
                        character: 5,
                    },
                },
                new_text: "hi".to_string(),
            },
            // Edit 2: replace "world" (6-11) with "earth"
            lsp_types::TextEdit {
                range: lsp_types::Range {
                    start: lsp_types::Position {
                        line: 0,
                        character: 6,
                    },
                    end: lsp_types::Position {
                        line: 0,
                        character: 11,
                    },
                },
                new_text: "earth".to_string(),
            },
        ],
    );
    let edit = lsp_types::WorkspaceEdit {
        changes: Some(changes),
        document_changes: None,
        change_annotations: None,
    };
    app.apply_workspace_edit(edit, hjkl_lsp::PositionEncoding::Utf8)
        .expect("apply failed");
    let lines = app
        .active_editor()
        .buffer()
        .rope()
        .lines()
        .map(|s| {
            let s = s.to_string();
            s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
        })
        .collect::<Vec<_>>();
    assert_eq!(lines[0], "hi earth foo", "both edits must apply correctly");
    let _ = std::fs::remove_file(&path);
}

#[test]
#[allow(clippy::mutable_key_type)]
fn apply_workspace_edit_multi_file() {
    let path_a = std::env::temp_dir().join("hjkl_ws_multi_a.txt");
    let path_b = std::env::temp_dir().join("hjkl_ws_multi_b.txt");
    std::fs::write(&path_a, "file a content\n").unwrap();
    std::fs::write(&path_b, "file b content\n").unwrap();

    let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();

    let uri_a = file_url(&path_a);
    let uri_b = file_url(&path_b);

    let url_a = uri_a.parse::<lsp_types::Uri>().expect("valid URI a");
    let url_b = uri_b.parse::<lsp_types::Uri>().expect("valid URI b");
    let mut changes = std::collections::HashMap::new();
    changes.insert(
        url_a,
        vec![lsp_types::TextEdit {
            range: lsp_types::Range {
                start: lsp_types::Position {
                    line: 0,
                    character: 7,
                },
                end: lsp_types::Position {
                    line: 0,
                    character: 14,
                },
            },
            new_text: "edited".to_string(),
        }],
    );
    changes.insert(
        url_b,
        vec![lsp_types::TextEdit {
            range: lsp_types::Range {
                start: lsp_types::Position {
                    line: 0,
                    character: 7,
                },
                end: lsp_types::Position {
                    line: 0,
                    character: 14,
                },
            },
            new_text: "changed".to_string(),
        }],
    );

    let edit = lsp_types::WorkspaceEdit {
        changes: Some(changes),
        document_changes: None,
        change_annotations: None,
    };
    let count = app
        .apply_workspace_edit(edit, hjkl_lsp::PositionEncoding::Utf8)
        .expect("multi-file apply failed");
    assert_eq!(count, 2, "should affect 2 files");
    let _ = std::fs::remove_file(&path_a);
    let _ = std::fs::remove_file(&path_b);
}

/// Regression (audit R2, fix 3): a multi-file workspace edit must send
/// `textDocument/didChange` for EVERY touched buffer, not just the focused
/// one. Before the fix, `apply_workspace_edit` mutated non-focused slots'
/// buffers but never drained their ContentEdits, so those buffers' server
/// copies stayed stale (wrong diagnostics / cross-file positions) until the
/// user happened to focus that slot.
#[test]
#[allow(clippy::mutable_key_type)]
fn apply_workspace_edit_multi_file_notifies_both_buffers() {
    let path_a = std::env::temp_dir().join("hjkl_ws_multi_notify_a.txt");
    let path_b = std::env::temp_dir().join("hjkl_ws_multi_notify_b.txt");
    std::fs::write(&path_a, "file a content\n").unwrap();
    std::fs::write(&path_b, "file b content\n").unwrap();

    // path_a is opened (and focused) up front; path_b is only opened as a
    // side effect of apply_workspace_edit, so it starts out unfocused.
    let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
    app.lsp = Some(hjkl_lsp::LspManager::spawn(hjkl_lsp::LspConfig::default()));

    let uri_a = file_url(&path_a);
    let uri_b = file_url(&path_b);
    let url_a = uri_a.parse::<lsp_types::Uri>().expect("valid URI a");
    let url_b = uri_b.parse::<lsp_types::Uri>().expect("valid URI b");
    let mut changes = std::collections::HashMap::new();
    changes.insert(
        url_a,
        vec![lsp_types::TextEdit {
            range: lsp_types::Range {
                start: lsp_types::Position {
                    line: 0,
                    character: 0,
                },
                end: lsp_types::Position {
                    line: 0,
                    character: 4,
                },
            },
            new_text: "FILE".to_string(),
        }],
    );
    changes.insert(
        url_b,
        vec![lsp_types::TextEdit {
            range: lsp_types::Range {
                start: lsp_types::Position {
                    line: 0,
                    character: 0,
                },
                end: lsp_types::Position {
                    line: 0,
                    character: 4,
                },
            },
            new_text: "FILE".to_string(),
        }],
    );

    let edit = lsp_types::WorkspaceEdit {
        changes: Some(changes),
        document_changes: None,
        change_annotations: None,
    };
    let count = app
        .apply_workspace_edit(edit, hjkl_lsp::PositionEncoding::Utf8)
        .expect("multi-file apply failed");
    assert_eq!(count, 2, "should affect 2 files");

    let slot_a = app
        .slots
        .iter()
        .position(|s| s.filename.as_deref() == Some(path_a.as_path()))
        .expect("slot a must exist");
    let slot_b = app
        .slots
        .iter()
        .position(|s| s.filename.as_deref() == Some(path_b.as_path()))
        .expect("slot b must have been opened by apply_workspace_edit");
    assert_ne!(slot_a, slot_b, "sanity: two distinct slots");

    let dg_a = app.slots[slot_a].buffer().dirty_gen();
    let dg_b = app.slots[slot_b].buffer().dirty_gen();
    assert_eq!(
        app.slots[slot_a].last_lsp_dirty_gen,
        Some(dg_a),
        "focused slot's buffer must be didChange-notified"
    );
    assert_eq!(
        app.slots[slot_b].last_lsp_dirty_gen,
        Some(dg_b),
        "non-focused slot's buffer must ALSO be didChange-notified, not \
        left stale until the user focuses it"
    );

    if let Some(mgr) = app.lsp.take() {
        mgr.shutdown();
    }
    let _ = std::fs::remove_file(&path_a);
    let _ = std::fs::remove_file(&path_b);
}

#[test]
fn rename_response_null_sets_status() {
    let mut app = App::new(None, false, None, None).unwrap();
    let pending = LspPendingRequest::Rename {
        buffer_id: 0,
        anchor_row: 0,
        anchor_col: 0,
        new_name: "newName".to_string(),
        encoding: hjkl_lsp::PositionEncoding::Utf8,
    };
    app.handle_lsp_response(pending, Ok(serde_json::Value::Null));
    let msg = app.bus.last_body_or_empty().to_string();
    assert!(
        msg.contains("cannot rename"),
        "null rename must set 'cannot rename' status, got: {msg}"
    );
}

#[test]
fn rename_response_applies_workspace_edit() {
    let path = std::env::temp_dir().join("hjkl_rename_apply.txt");
    std::fs::write(&path, "old_name here\n").unwrap();
    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();

    let uri = file_url(&path);
    let edit = make_workspace_edit(&uri, 0, 0, 0, 8, "new_name");
    let val = serde_json::to_value(edit).unwrap();

    let pending = LspPendingRequest::Rename {
        buffer_id: 0,
        anchor_row: 0,
        anchor_col: 0,
        new_name: "new_name".to_string(),
        encoding: hjkl_lsp::PositionEncoding::Utf8,
    };
    app.handle_lsp_response(pending, Ok(val));
    let msg = app.bus.last_body_or_empty().to_string();
    assert!(
        msg.contains("renamed"),
        "rename response must set status, got: {msg}"
    );
    let lines = app
        .active_editor()
        .buffer()
        .rope()
        .lines()
        .map(|s| {
            let s = s.to_string();
            s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
        })
        .collect::<Vec<_>>();
    assert_eq!(lines[0], "new_name here");
    let _ = std::fs::remove_file(&path);
}

/// Regression (audit R2, UTF-16 fix) — THE motivating corruption case: a
/// `workspace/applyEdit` (equally, a `textDocument/rename` response) from a
/// UTF-16-only server whose `TextEdit.range` crosses an astral char (an
/// emoji here — 1 char, 2 UTF-16 units, 4 UTF-8 bytes) must land on the
/// correct chars. Feeding the raw UTF-16 wire columns straight into
/// `hjkl_engine::Pos.col` (a CHAR index) — what the pre-fix code did —
/// silently slices the wrong text out of the buffer.
///
/// Both blocks below operate on the same line, `"🎉hello world"` (char
/// indices: 0:🎉 1:h 2:e 3:l 4:l 5:o 6:' ' 7:w 8:o 9:r 10:l 11:d). The
/// intended edit replaces "hello" (char range [1, 6)) with "HELLO". A
/// UTF-16 server reports that range as wire columns [2, 7) — the emoji's 2
/// units push every char index after it two spots further right in wire
/// space than in char space.
#[test]
fn workspace_edit_utf16_wire_columns_convert_to_correct_chars() {
    let path = std::env::temp_dir().join("hjkl_ws_edit_utf16_corruption.txt");
    std::fs::write(&path, "🎉hello world\n").unwrap();

    // ── Pre-fix evidence ────────────────────────────────────────────────
    // Simulates exactly what the old code did: build `Pos` straight from
    // the wire `character` values with no encoding conversion, then apply
    // via the same `BufferEdit::replace_range` the fixed path still uses.
    // Char range [2, 7) is "ello " (e,l,l,o,space) — replacing it with
    // "HELLO" leaves a stray leading 'h' and swallows the space before
    // "world", corrupting the line.
    {
        use hjkl_engine::{BufferEdit, Pos};
        let mut corrupt_app = App::new(Some(path.clone()), false, None, None).unwrap();
        let start = Pos { line: 0, col: 2 }; // WRONG: raw UTF-16 wire value used as char col
        let end = Pos { line: 0, col: 7 };
        BufferEdit::replace_range(
            corrupt_app.active_editor_mut().buffer_mut(),
            start..end,
            "HELLO",
        );
        let corrupted = corrupt_app
            .active_editor()
            .buffer()
            .rope()
            .line(0)
            .to_string();
        assert_eq!(
            corrupted.trim_end(),
            "🎉hHELLOworld",
            "pre-fix evidence: raw UTF-16-wire-as-char-index corrupts the line \
            (drops \"e\", the space before \"world\", and leaves a stray \"h\")"
        );
    }

    // ── Post-fix ────────────────────────────────────────────────────────
    // `apply_workspace_edit` with the server's negotiated UTF-16 encoding
    // must convert [2, 7) (wire) -> [1, 6) (char) and replace exactly
    // "hello".
    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
    let uri = file_url(&path);
    let edit = make_workspace_edit(&uri, 0, 2, 0, 7, "HELLO");
    match app.apply_workspace_edit(edit, hjkl_lsp::PositionEncoding::Utf16) {
        Ok(count) => assert_eq!(count, 1),
        Err(e) => panic!("apply_workspace_edit failed: {e}"),
    }
    let fixed = app.active_editor().buffer().rope().line(0).to_string();
    assert_eq!(
        fixed.trim_end(),
        "🎉HELLO world",
        "UTF-16 wire columns must convert to the char range covering exactly \"hello\""
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn format_response_empty_sets_status() {
    let mut app = App::new(None, false, None, None).unwrap();
    let pending = LspPendingRequest::Format {
        buffer_id: 0,
        range: None,
        encoding: hjkl_lsp::PositionEncoding::Utf8,
    };
    // Empty array = no changes.
    app.handle_lsp_response(pending, Ok(serde_json::json!([])));
    let msg = app.bus.last_body_or_empty().to_string();
    assert!(
        msg.contains("no formatting"),
        "empty format response must say 'no formatting changes', got: {msg}"
    );
}

#[test]
fn format_response_applies_text_edits() {
    let path = std::env::temp_dir().join("hjkl_format_apply.txt");
    std::fs::write(&path, "fn foo(){}\n").unwrap();
    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
    let buf_id = app.active().buffer_id as hjkl_lsp::BufferId;

    // Insert a space at col 9 (after the `{`) → "fn foo(){ }"
    let edits: Vec<lsp_types::TextEdit> = vec![lsp_types::TextEdit {
        range: lsp_types::Range {
            start: lsp_types::Position {
                line: 0,
                character: 9,
            },
            end: lsp_types::Position {
                line: 0,
                character: 9,
            },
        },
        new_text: " ".to_string(),
    }];
    let val = serde_json::to_value(&edits).unwrap();

    let pending = LspPendingRequest::Format {
        buffer_id: buf_id,
        range: None,
        encoding: hjkl_lsp::PositionEncoding::Utf8,
    };
    app.handle_lsp_response(pending, Ok(val));
    let msg = app.bus.last_body_or_empty().to_string();
    assert_eq!(msg, "formatted");
    let lines = app
        .active_editor()
        .buffer()
        .rope()
        .lines()
        .map(|s| {
            let s = s.to_string();
            s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
        })
        .collect::<Vec<_>>();
    // "fn foo(){}" with space inserted at pos 9 → "fn foo(){ }"
    assert_eq!(lines[0], "fn foo(){ }");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn code_action_response_empty_sets_status() {
    let mut app = App::new(None, false, None, None).unwrap();
    let pending = LspPendingRequest::CodeAction {
        buffer_id: 0,
        anchor_row: 0,
        anchor_col: 0,
        encoding: hjkl_lsp::PositionEncoding::Utf8,
    };
    app.handle_lsp_response(pending, Ok(serde_json::json!([])));
    let msg = app.bus.last_body_or_empty().to_string();
    assert!(
        msg.contains("no code actions"),
        "empty code actions must say 'no code actions', got: {msg}"
    );
}

#[test]
fn code_action_response_multi_opens_picker() {
    let mut app = App::new(None, false, None, None).unwrap();
    let pending = LspPendingRequest::CodeAction {
        buffer_id: 0,
        anchor_row: 0,
        anchor_col: 0,
        encoding: hjkl_lsp::PositionEncoding::Utf8,
    };
    let actions = serde_json::json!([
        {
            "title": "Fix import",
            "kind": "quickfix",
        },
        {
            "title": "Extract method",
            "kind": "refactor",
        },
    ]);
    app.handle_lsp_response(pending, Ok(actions));
    assert!(
        app.picker.is_some(),
        "multiple code actions must open picker"
    );
    assert_eq!(
        app.pending_code_actions.len(),
        2,
        "pending_code_actions must hold both actions"
    );
}

#[test]
fn code_action_response_single_applies_action() {
    let path = std::env::temp_dir().join("hjkl_ca_single.txt");
    std::fs::write(&path, "old content\n").unwrap();
    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();

    let uri = file_url(&path);
    let edit = make_workspace_edit(&uri, 0, 0, 0, 11, "new content");
    let action = lsp_types::CodeAction {
        title: "Replace content".to_string(),
        edit: Some(edit),
        ..Default::default()
    };
    let val =
        serde_json::to_value(vec![lsp_types::CodeActionOrCommand::CodeAction(action)]).unwrap();

    let pending = LspPendingRequest::CodeAction {
        buffer_id: 0,
        anchor_row: 0,
        anchor_col: 0,
        encoding: hjkl_lsp::PositionEncoding::Utf8,
    };
    app.handle_lsp_response(pending, Ok(val));
    // Single action: applied directly, no picker.
    assert!(
        app.picker.is_none(),
        "single code action must not open picker"
    );
    let msg = app.bus.last_body_or_empty().to_string();
    assert!(
        msg.contains("files changed"),
        "single action apply must set status, got: {msg}"
    );
    let lines = app
        .active_editor()
        .buffer()
        .rope()
        .lines()
        .map(|s| {
            let s = s.to_string();
            s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
        })
        .collect::<Vec<_>>();
    assert_eq!(lines[0], "new content");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn lsp_code_actions_includes_overlapping_diags_in_context() {
    // Verify that lsp_code_actions collects diagnostics that overlap the cursor.
    // We set up a slot with diags and check the request would include them.
    // Since we can't intercept the LspManager send, we test the diagnostic
    // overlap logic used by lsp_code_actions separately here.
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "fn foo() {\n    let x = 1;\n}\n");

    // Seed two diagnostics: one overlapping the cursor, one not.
    app.active_mut().lsp_diags = vec![
        LspDiag {
            start_row: 0,
            start_col: 3,
            end_row: 0,
            end_col: 6,
            severity: DiagSeverity::Error,
            message: "overlapping".to_string(),
            source: None,
            code: None,
        },
        LspDiag {
            start_row: 1,
            start_col: 0,
            end_row: 1,
            end_col: 5,
            severity: DiagSeverity::Warning,
            message: "not overlapping".to_string(),
            source: None,
            code: None,
        },
    ];

    // Position cursor at row=0, col=4 (inside the first diag range).
    app.active_editor_mut().jump_cursor(0, 4);

    // Test the overlap logic directly.
    let cursor_row = 0usize;
    let cursor_col = 4usize;
    let diags = &app.active().lsp_diags;
    let overlapping: Vec<_> = diags
        .iter()
        .filter(|d| {
            let after_start = (cursor_row, cursor_col) >= (d.start_row, d.start_col);
            let before_end = cursor_row < d.end_row
                || (cursor_row == d.end_row && cursor_col < d.end_col)
                || (cursor_row == d.start_row && d.start_row == d.end_row);
            after_start && (before_end || cursor_row == d.start_row)
        })
        .collect();

    assert_eq!(
        overlapping.len(),
        1,
        "only the overlapping diag should be included"
    );
    assert_eq!(overlapping[0].message, "overlapping");
}

/// After `lnext_severity` jumps to a diagnostic at col 5, pressing `j`
/// must aim for col 5 on the next row (or clamp) — not snap to the stale
/// pre-jump column. Validates that `jump_cursor` inside `lnext_severity`
/// now resets `sticky_col`.
#[test]
fn lnext_then_j_preserves_diag_col() {
    use hjkl_engine::{Input, Key};
    let mut app = App::new(None, false, None, None).unwrap();
    let path = tmp_path("hjkl_lnext_sticky.rs");
    app.active_mut().filename = Some(path.clone());
    // Row 0: "hello"          (5 chars)
    // Row 1: "world"          (5 chars)
    // Row 2: "abcde fghij"    (11 chars) — diag at col 5
    // Row 3: "0123456789"     (10 chars) — j should land at col 5
    seed_buffer(&mut app, "hello\nworld\nabcde fghij\n0123456789\n");

    // Plant a diagnostic at row 2, col 5.
    let params = pub_diags_params(
        &file_url(&path),
        serde_json::json!([{
            "range": {
                "start": { "line": 2, "character": 5 },
                "end":   { "line": 2, "character": 10 }
            },
            "severity": 1,
            "message": "sticky col test diag"
        }]),
    );
    app.handle_publish_diagnostics(params, hjkl_lsp::PositionEncoding::Utf8);

    // Cursor starts at (0, 0) — lnext must jump to row 2, col 5.
    app.lnext_severity(None);
    let (row, col) = app.active_editor().cursor();
    assert_eq!(row, 2, "lnext must jump to row 2");
    assert_eq!(col, 5, "lnext must place cursor at diag col 5");
    assert_eq!(
        app.active_editor().sticky_col(),
        Some(5),
        "lnext must reset sticky_col to 5 via jump_cursor"
    );

    // Press j — must aim for col 5 on row 3 ("0123456789" has 10 chars).
    hjkl_vim::dispatch_input(
        app.active_editor_mut(),
        Input {
            key: Key::Char('j'),
            ctrl: false,
            alt: false,
            shift: false,
        },
    );
    let (row2, col2) = app.active_editor().cursor();
    assert_eq!(row2, 3, "j must move to row 3");
    assert_eq!(
        col2, 5,
        "j after lnext must aim for col 5 (set by jump_cursor); got col {col2} — \
         sticky_col may not have been reset by lnext_severity"
    );
}

// ── pending-request timeout sweep ──────────────────────────────────────────

/// A pending request older than the timeout is dropped (clears the spinner)
/// while a fresh one survives.
#[test]
fn stale_lsp_pending_is_swept() {
    use std::time::{Duration, Instant};
    let mut app = App::new(None, false, None, None).unwrap();
    let buffer_id = app.active().buffer_id as hjkl_lsp::BufferId;

    let stale = app.lsp_alloc_request_id();
    let fresh = app.lsp_alloc_request_id();
    for id in [stale, fresh] {
        app.lsp_pending.insert(
            id,
            LspPendingRequest::Hover {
                buffer_id,
                origin: (0, 0),
            },
        );
    }

    let now = Instant::now();
    // First sweep stamps both as seen "now".
    app.sweep_stale_lsp_pending_at(now);
    assert_eq!(
        app.lsp_pending.len(),
        2,
        "fresh requests must not be dropped"
    );

    // A later sweep past the timeout drops both (both were stamped at `now`).
    app.sweep_stale_lsp_pending_at(now + Duration::from_secs(21));
    assert!(
        app.lsp_pending.is_empty(),
        "requests older than the timeout must be swept"
    );
    assert!(
        app.lsp_pending_seen_at.is_empty(),
        "timestamps for dropped requests must be cleaned up"
    );
}

/// A resolved request (removed from `lsp_pending`) has its timestamp cleaned up
/// on the next sweep, so the map can't grow unbounded.
#[test]
fn resolved_lsp_pending_timestamp_is_cleaned() {
    use std::time::{Duration, Instant};
    let mut app = App::new(None, false, None, None).unwrap();
    let buffer_id = app.active().buffer_id as hjkl_lsp::BufferId;
    let id = app.lsp_alloc_request_id();
    app.lsp_pending.insert(
        id,
        LspPendingRequest::Hover {
            buffer_id,
            origin: (0, 0),
        },
    );
    let now = Instant::now();
    app.sweep_stale_lsp_pending_at(now);
    assert!(app.lsp_pending_seen_at.contains_key(&id));
    // Request resolves (response handler removed it from lsp_pending).
    app.lsp_pending.remove(&id);
    app.sweep_stale_lsp_pending_at(now + Duration::from_millis(1));
    assert!(
        !app.lsp_pending_seen_at.contains_key(&id),
        "timestamp for a resolved request must be cleaned up"
    );
}

// ── capability gating ──────────────────────────────────────────────────────

fn server_with_caps(app: &mut App, language: &str, caps: serde_json::Value) {
    app.lsp = Some(hjkl_lsp::LspManager::spawn(hjkl_lsp::LspConfig::default()));
    app.lsp_state.insert(
        hjkl_lsp::ServerKey {
            language: language.into(),
            root: tmp_path("proj"),
        },
        LspServerInfo {
            initialized: true,
            capabilities: caps,
        },
    );
}

/// Completion must NOT fire when the server advertises no completion support —
/// otherwise the auto-fire piles up pending requests and hangs the spinner.
#[test]
fn completion_gated_without_capability() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.active_mut().filename = Some(tmp_path("gate_none.rs"));
    server_with_caps(&mut app, "rust", serde_json::json!({}));
    app.lsp_request_completion();
    assert!(
        app.lsp_pending.is_empty(),
        "completion must be skipped when the server has no completionProvider"
    );
}

/// Completion fires when the server advertises completion support.
#[test]
fn completion_fires_with_capability() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.active_mut().filename = Some(tmp_path("gate_yes.rs"));
    server_with_caps(
        &mut app,
        "rust",
        serde_json::json!({ "completionProvider": {} }),
    );
    app.lsp_request_completion();
    assert_eq!(
        app.lsp_pending.len(),
        1,
        "completion must fire when the server advertises completionProvider"
    );
}

/// Requests are gated until the server finishes initializing.
#[test]
fn completion_gated_before_initialized() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.active_mut().filename = Some(tmp_path("gate_init.rs"));
    app.lsp = Some(hjkl_lsp::LspManager::spawn(hjkl_lsp::LspConfig::default()));
    app.lsp_state.insert(
        hjkl_lsp::ServerKey {
            language: "rust".into(),
            root: tmp_path("proj"),
        },
        LspServerInfo {
            initialized: false,
            capabilities: serde_json::json!({ "completionProvider": {} }),
        },
    );
    app.lsp_request_completion();
    assert!(
        app.lsp_pending.is_empty(),
        "requests must wait until the server is initialized"
    );
}

/// Auto-fired completion uses a short timeout so an unresponsive server
/// (e.g. taplo on TOML) doesn't keep the spinner lit.
#[test]
fn auto_completion_pending_times_out_quickly() {
    use std::time::{Duration, Instant};
    let mut app = App::new(None, false, None, None).unwrap();
    let buffer_id = app.active().buffer_id as hjkl_lsp::BufferId;
    let id = app.lsp_alloc_request_id();
    app.lsp_pending.insert(
        id,
        LspPendingRequest::Completion {
            buffer_id,
            anchor_row: 0,
            anchor_col: 0,
            auto: true,
        },
    );
    let now = Instant::now();
    app.sweep_stale_lsp_pending_at(now); // stamp
    app.sweep_stale_lsp_pending_at(now + Duration::from_secs(2));
    assert_eq!(app.lsp_pending.len(), 1, "auto completion survives 2s");
    app.sweep_stale_lsp_pending_at(now + Duration::from_secs(4));
    assert!(
        app.lsp_pending.is_empty(),
        "auto completion is dropped after the 3s timeout"
    );
}

// ── App::shutdown (audit finding B1) ───────────────────────────────────────
//
// Regression coverage for the orphaned-LSP-child bug: quitting hjkl used to
// return from `main` without ever calling `LspManager::shutdown()`, so
// spawned language servers (rust-analyzer, gopls, tsserver, …) survived the
// process exit — `Drop for LspManager` only fire-and-forgets a `ShutdownAll`
// on a background thread the process exit races. `App::shutdown` is the
// testable seam `main` now calls on every exit path.

/// With no LSP manager attached (the common case — `lsp.enabled = false` by
/// default), `App::shutdown` must be a safe no-op: `self.lsp` stays `None`
/// and nothing panics.
#[test]
fn shutdown_with_no_lsp_manager_is_noop() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert!(app.lsp.is_none());
    app.shutdown();
    assert!(app.lsp.is_none());
}

/// With a real `LspManager` attached, `App::shutdown` must take it out of
/// `self.lsp` (leaving `None`) and drive the manager's blocking
/// `shutdown()` — the graceful `ShutdownAll` + bounded (~2s) thread join
/// that actually kills and reaps any spawned server child. This is the
/// core regression assertion for B1: before the fix, nothing on any exit
/// path called this.
#[test]
fn shutdown_takes_and_shuts_down_attached_lsp_manager() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.lsp = Some(hjkl_lsp::LspManager::spawn(hjkl_lsp::LspConfig::default()));
    assert!(app.lsp.is_some(), "precondition: manager attached");

    app.shutdown();

    assert!(
        app.lsp.is_none(),
        "shutdown must take self.lsp, leaving None"
    );
}

/// `App::shutdown` must be idempotent — safe to call twice (e.g. once from
/// an early `+wq`-style exit path and, hypothetically, again from a later
/// one) without panicking or double-joining a thread that's already gone.
#[test]
fn shutdown_is_idempotent() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.lsp = Some(hjkl_lsp::LspManager::spawn(hjkl_lsp::LspConfig::default()));

    app.shutdown();
    assert!(app.lsp.is_none());

    // Second call: self.lsp is already None — must not panic.
    app.shutdown();
    assert!(app.lsp.is_none());
}
