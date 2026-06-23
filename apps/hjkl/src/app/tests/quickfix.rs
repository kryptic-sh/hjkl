//! Quickfix list (#184) app-level tests: nav dispatch, popup toggle, and a
//! real jump-to-entry through a temp file.

use super::*;
use hjkl_ex::QfCommand;
use hjkl_quickfix::{QfEntry, QfKind};

fn entry(path: &std::path::Path, row: usize) -> QfEntry {
    QfEntry {
        path: path.to_path_buf(),
        row,
        col: 0,
        kind: QfKind::Grep,
        message: format!("hit at {row}"),
    }
}

#[test]
fn quickfix_popup_nav_and_toggle() {
    let mut app = App::new(None, false, None, None).unwrap();
    let p = std::path::PathBuf::from("x.rs");
    app.quickfix
        .set(vec![entry(&p, 0), entry(&p, 1), entry(&p, 2)]);

    // `:copen` shows the popup; popup j/k move the highlight without jumping.
    app.handle_quickfix_command(QfCommand::Open);
    assert!(app.quickfix_open);
    app.quickfix_popup_down();
    assert_eq!(app.quickfix.cursor(), 1);
    app.quickfix_popup_down();
    assert_eq!(app.quickfix.cursor(), 2);
    app.quickfix_popup_up();
    assert_eq!(app.quickfix.cursor(), 1);

    // `:cclose` hides it.
    app.handle_quickfix_command(QfCommand::Close);
    assert!(!app.quickfix_open);
}

#[cfg(unix)]
#[test]
fn quickfix_make_parses_output_into_list() {
    // A fake `makeprg` script that emits a gcc-style diagnostic to stderr.
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("hjkl_make_test_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("fakemake.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\necho 'src/main.rs:3:5: error: boom' 1>&2\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut app = App::new(None, false, None, None).unwrap();
    app.active_editor_mut().settings_mut().makeprg = script.to_string_lossy().into_owned();
    app.handle_quickfix_command(QfCommand::Make(String::new()));

    assert_eq!(app.quickfix.len(), 1, ":make should populate the list");
    assert!(app.quickfix_open, ":make with errors opens the popup");
    let e = app.quickfix.current().unwrap();
    assert_eq!((e.row, e.col), (2, 4)); // 0-based
    assert_eq!(e.kind, QfKind::Error);
    assert_eq!(e.message, "boom");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn quickfix_open_empty_does_not_show() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.handle_quickfix_command(QfCommand::Open);
    assert!(
        !app.quickfix_open,
        "empty quickfix list must not open the popup"
    );
}

#[test]
fn quickfix_next_jumps_to_entry() {
    // `:cnext` moves the list cursor AND jumps the editor to that file+row.
    let dir = std::env::temp_dir().join(format!("hjkl_qf_test_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("sample.txt");
    std::fs::write(&file, "line zero\nline one\nline two\n").unwrap();

    let mut app = App::new(None, false, None, None).unwrap();
    app.quickfix.set(vec![entry(&file, 0), entry(&file, 2)]);

    // Cursor starts at entry 0; :cnext → entry 1 (row 2) and opens the file.
    app.handle_quickfix_command(QfCommand::Next);
    assert_eq!(app.quickfix.cursor(), 1);
    assert_eq!(
        app.active_editor().cursor().0,
        2,
        "cnext should jump the editor cursor to the entry's row"
    );

    // `]q` / `[q` route through the same path: [q → back to entry 0 (row 0).
    app.dispatch_action(crate::keymap_actions::AppAction::QuickfixPrev, 1);
    assert_eq!(app.quickfix.cursor(), 0);
    assert_eq!(app.active_editor().cursor().0, 0);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn loclist_independent_from_quickfix() {
    // The location list is a separate list/popup from the quickfix list.
    let mut app = App::new(None, false, None, None).unwrap();
    let p = std::path::PathBuf::from("x.rs");
    app.loclist.set(vec![entry(&p, 0), entry(&p, 1)]);

    // `:lopen` opens the loclist popup, NOT the quickfix popup.
    app.handle_loclist_command(QfCommand::Open);
    assert!(app.loclist_open);
    assert!(!app.quickfix_open);

    app.loclist_popup_down();
    assert_eq!(app.loclist.cursor(), 1);
    assert_eq!(app.quickfix.cursor(), 0, "quickfix list untouched");

    app.handle_loclist_command(QfCommand::Close);
    assert!(!app.loclist_open);
}

#[test]
fn loclist_next_jumps_and_dispatch_routes() {
    let dir = std::env::temp_dir().join(format!("hjkl_ll_test_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("sample.txt");
    std::fs::write(&file, "line zero\nline one\nline two\n").unwrap();

    let mut app = App::new(None, false, None, None).unwrap();
    app.loclist.set(vec![entry(&file, 0), entry(&file, 2)]);

    app.handle_loclist_command(QfCommand::Next);
    assert_eq!(app.loclist.cursor(), 1);
    assert_eq!(app.active_editor().cursor().0, 2);

    // `[l` routes through the loclist via dispatch.
    app.dispatch_action(crate::keymap_actions::AppAction::LoclistPrev, 1);
    assert_eq!(app.loclist.cursor(), 0);
    assert_eq!(app.active_editor().cursor().0, 0);

    let _ = std::fs::remove_dir_all(&dir);
}

// ── :cexpr / :cgetexpr / :caddexpr in-process tests (#261) ─────────────────

/// Build an App with a 6-line in-memory buffer (no real file) and set
/// `errorformat` to `%l:%c:%m` so `:cexpr "3:2:three\n5:1:five"` produces
/// two empty-path entries.
fn make_app_with_buffer() -> App {
    let mut app = App::new(None, false, None, None).unwrap();
    // Populate the buffer with 6 lines.
    let content = "line1\nline2\nline3\nline4\nline5\nline6\n";
    app.active_editor_mut().set_content(content);
    // Set errorformat to no-file form.
    app.active_editor_mut().settings_mut().errorformat = "%l:%c:%m".to_string();
    app
}

#[test]
fn cexpr_populates_list_and_jumps_to_first() {
    let mut app = make_app_with_buffer();
    // `:cexpr "3:2:three\n5:1:five"` — two entries, jump=true
    app.handle_quickfix_command(QfCommand::Expr {
        text: r#""3:2:three\n5:1:five""#.to_string(),
        append: false,
        jump: true,
    });
    assert_eq!(app.quickfix.len(), 2, "should have 2 entries");
    // First entry: row=2 (0-based from line 3), col=1
    assert_eq!(app.quickfix.cursor(), 0);
    let e0 = app.quickfix.current().unwrap();
    assert_eq!(e0.row, 2, "entry 0 row should be 2 (0-based)");
    assert_eq!(e0.col, 1, "entry 0 col should be 1 (0-based)");
    // Editor cursor must have jumped to row 2
    assert_eq!(
        app.active_editor().cursor().0,
        2,
        "editor row should be at first entry (row 2)"
    );
    assert_eq!(
        app.active_editor().cursor().1,
        1,
        "editor col should be at first entry (col 1)"
    );
}

#[test]
fn cexpr_then_cnext_moves_to_second_entry() {
    let mut app = make_app_with_buffer();
    app.handle_quickfix_command(QfCommand::Expr {
        text: r#""3:2:three\n5:1:five""#.to_string(),
        append: false,
        jump: true,
    });
    // :cnext should move to the second entry (row 4, col 0)
    app.handle_quickfix_command(QfCommand::Next);
    assert_eq!(app.quickfix.cursor(), 1);
    let e1 = app.quickfix.current().unwrap();
    assert_eq!(e1.row, 4, "entry 1 row should be 4 (0-based from line 5)");
    assert_eq!(e1.col, 0, "entry 1 col should be 0 (0-based from col 1)");
    assert_eq!(
        app.active_editor().cursor().0,
        4,
        "editor row should be at second entry (row 4)"
    );
}

#[test]
fn cgetexpr_populates_but_does_not_jump() {
    let mut app = make_app_with_buffer();
    let initial_row = app.active_editor().cursor().0;
    // :cgetexpr → jump=false, cursor should not move
    app.handle_quickfix_command(QfCommand::Expr {
        text: r#""3:2:three""#.to_string(),
        append: false,
        jump: false,
    });
    assert_eq!(app.quickfix.len(), 1, "list should have 1 entry");
    assert_eq!(
        app.active_editor().cursor().0,
        initial_row,
        "cgetexpr must not move cursor"
    );
}

#[test]
fn caddexpr_appends_to_existing_list() {
    let mut app = make_app_with_buffer();
    // First populate with one entry
    app.handle_quickfix_command(QfCommand::Expr {
        text: r#""2:0:two""#.to_string(),
        append: false,
        jump: false,
    });
    assert_eq!(app.quickfix.len(), 1);
    // caddexpr appends
    app.handle_quickfix_command(QfCommand::Expr {
        text: r#""4:1:four""#.to_string(),
        append: true,
        jump: false,
    });
    assert_eq!(app.quickfix.len(), 2, "caddexpr should append to list");
    // Cursor stays at 0 (not jumped)
    assert_eq!(app.quickfix.cursor(), 0);
}

// ── :cbuffer / :cgetbuffer / :caddbuffer in-process tests (#261 Phase 5b) ───

/// Build an App with buffer content set to 3 error-format lines and errorformat
/// configured to `%l:%c:%m` (no file column) so `:cbuffer` produces empty-path
/// entries at lines 1/2/3 col 1 each (0-based: row 0/1/2 col 0).
fn make_app_with_cbuffer_content() -> App {
    let mut app = App::new(None, false, None, None).unwrap();
    // Three lines matching %l:%c:%m: "1:1:a", "2:1:b", "3:1:c"
    app.active_editor_mut().set_content("1:1:a\n2:1:b\n3:1:c\n");
    app.active_editor_mut().settings_mut().errorformat = "%l:%c:%m".to_string();
    app
}

#[test]
fn cbuffer_populates_list_and_jumps_to_first() {
    let mut app = make_app_with_cbuffer_content();
    app.handle_quickfix_command(QfCommand::FromBuffer {
        append: false,
        jump: true,
    });
    assert_eq!(app.quickfix.len(), 3, "cbuffer should produce 3 entries");
    assert_eq!(app.quickfix.cursor(), 0);
    // First entry: line 1 col 1 → row=0 col=0 (0-based)
    let e0 = app.quickfix.current().unwrap();
    assert_eq!(e0.row, 0, "first entry row should be 0");
    assert_eq!(e0.col, 0, "first entry col should be 0");
    // Editor jumped to first entry
    assert_eq!(
        app.active_editor().cursor().0,
        0,
        "editor row should be at first entry (row 0)"
    );
}

#[test]
fn cbuffer_then_cnext_moves_to_second_entry() {
    let mut app = make_app_with_cbuffer_content();
    app.handle_quickfix_command(QfCommand::FromBuffer {
        append: false,
        jump: true,
    });
    // :cnext → second entry (line 2 col 1 → row=1 col=0)
    app.handle_quickfix_command(QfCommand::Next);
    assert_eq!(app.quickfix.cursor(), 1);
    let e1 = app.quickfix.current().unwrap();
    assert_eq!(e1.row, 1, "second entry row should be 1");
    assert_eq!(
        app.active_editor().cursor().0,
        1,
        "editor row should be at second entry (row 1)"
    );
}

#[test]
fn cgetbuffer_populates_but_does_not_jump() {
    let mut app = make_app_with_cbuffer_content();
    let initial_row = app.active_editor().cursor().0;
    app.handle_quickfix_command(QfCommand::FromBuffer {
        append: false,
        jump: false,
    });
    assert_eq!(app.quickfix.len(), 3, "cgetbuffer should produce 3 entries");
    assert_eq!(
        app.active_editor().cursor().0,
        initial_row,
        "cgetbuffer must not move cursor"
    );
}

#[test]
fn caddbuffer_appends_to_existing_list() {
    let mut app = make_app_with_cbuffer_content();
    // Populate with one entry first
    app.handle_quickfix_command(QfCommand::FromBuffer {
        append: false,
        jump: false,
    });
    assert_eq!(app.quickfix.len(), 3);
    // Change buffer content and caddbuffer — should append
    app.active_editor_mut().set_content("4:1:d\n5:1:e\n");
    app.handle_quickfix_command(QfCommand::FromBuffer {
        append: true,
        jump: false,
    });
    assert_eq!(
        app.quickfix.len(),
        5,
        "caddbuffer should append to existing list"
    );
    // Cursor stays at 0 (not jumped)
    assert_eq!(app.quickfix.cursor(), 0);
}

// ── :cfile / :cgetfile / :caddfile in-process tests (#261 Phase 5b) ─────────

#[test]
fn cfile_populates_list_from_disk_and_jumps() {
    // Write a temp file containing errorformat lines.
    let dir = std::env::temp_dir().join(format!("hjkl_cfile_test_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let errfile = dir.join("errors.txt");
    // Use %f:%l:%c:%m so entries reference themselves; for jump we need a
    // reachable file — use the errfile itself so no extra files are needed.
    // Actually, for a simpler test use %l:%c:%m (no %f) so path is empty and
    // the jump stays in-buffer (no do_edit).
    let errfile_path = errfile.to_str().unwrap().to_string();
    std::fs::write(&errfile, "1:1:alpha\n2:1:beta\n3:1:gamma\n").unwrap();

    let mut app = App::new(None, false, None, None).unwrap();
    app.active_editor_mut().settings_mut().errorformat = "%l:%c:%m".to_string();

    app.handle_quickfix_command(QfCommand::FromFile {
        path: errfile_path,
        append: false,
        jump: true,
    });

    assert_eq!(app.quickfix.len(), 3, "cfile should produce 3 entries");
    let e0 = app.quickfix.current().unwrap();
    assert_eq!(e0.row, 0, "first entry row should be 0");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cfile_missing_path_reports_error() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.active_editor_mut().settings_mut().errorformat = "%l:%c:%m".to_string();
    // Non-existent file should produce an error bus message and leave list empty.
    app.handle_quickfix_command(QfCommand::FromFile {
        path: "/nonexistent/path/errors.err".to_string(),
        append: false,
        jump: false,
    });
    assert_eq!(
        app.quickfix.len(),
        0,
        "cfile on missing file leaves list empty"
    );
}
