//! Quickfix list (#184) app-level tests: nav dispatch, dock toggle, and a
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
fn quickfix_dock_nav_and_toggle() {
    // `:copen` opens the bottom dock (#63 Phase B — no more Clear+List
    // overlay); `:cclose` closes it. Dock-buffer navigation itself is
    // covered by the `bottom_dock_*` tests below.
    let mut app = App::new(None, false, None, None).unwrap();
    let p = std::path::PathBuf::from("x.rs");
    app.quickfix
        .set(vec![entry(&p, 0), entry(&p, 1), entry(&p, 2)]);

    app.handle_quickfix_command(QfCommand::Open);
    assert!(app.quickfix_open());
    assert!(app.bottom_dock.is_some());

    app.handle_quickfix_command(QfCommand::Close);
    assert!(!app.quickfix_open());
    assert!(app.bottom_dock.is_none());
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
    assert!(app.quickfix_open(), ":make with errors opens the dock");
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
        !app.quickfix_open(),
        "empty quickfix list must not open the dock"
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
    // The location list is a separate list/dock from the quickfix list.
    let mut app = App::new(None, false, None, None).unwrap();
    let p = std::path::PathBuf::from("x.rs");
    app.loclist.set(vec![entry(&p, 0), entry(&p, 1)]);

    // `:lopen` opens the loclist dock, NOT the quickfix dock.
    app.handle_loclist_command(QfCommand::Open);
    assert!(app.loclist_open());
    assert!(!app.quickfix_open());

    // Move the loclist cursor directly (not via `:lnext`, which would also
    // jump the editor — out of scope for this independence check).
    app.loclist.next();
    assert_eq!(app.loclist.cursor(), 1);
    assert_eq!(app.quickfix.cursor(), 0, "quickfix list untouched");

    app.handle_loclist_command(QfCommand::Close);
    assert!(!app.loclist_open());
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

// ── :cdo / :cfdo / :ldo / :lfdo in-process tests (#261 Phase 5b "A2") ───────

/// Read the rope line at 0-based `row` from the active editor.
fn buf_row(app: &App, row: usize) -> String {
    hjkl_buffer::rope_line_str(&app.active_editor().buffer().rope(), row)
}

#[test]
fn cdo_runs_command_per_entry() {
    // App with a 5-line buffer; errorformat=%l:%c:%m; :cexpr produces two
    // empty-path entries at rows 1 (line 2) and 3 (line 4); :cdo s/^/X/
    // prepends X to each of those rows.
    let mut app = App::new(None, false, None, None).unwrap();
    app.active_editor_mut()
        .set_content("alpha\nbeta\ngamma\ndelta\nepsilon\n");
    app.active_editor_mut().settings_mut().errorformat = "%l:%c:%m".to_string();

    // Populate the quickfix list with entries at lines 2 and 4 (0-based rows 1,3).
    app.handle_quickfix_command(QfCommand::Expr {
        text: r#""2:1:a\n4:1:b""#.to_string(),
        append: false,
        jump: false,
    });
    assert_eq!(app.quickfix.len(), 2, "should have 2 entries");

    // Run :cdo s/^/X/
    app.handle_quickfix_command(QfCommand::Do {
        cmd: "s/^/X/".to_string(),
        per_file: false,
    });

    // Rows 1 and 3 should now start with X; others unchanged.
    assert_eq!(buf_row(&app, 0), "alpha", "row 0 must be untouched");
    assert!(
        buf_row(&app, 1).starts_with('X'),
        "row 1 must start with X, got: {:?}",
        buf_row(&app, 1)
    );
    assert_eq!(buf_row(&app, 2), "gamma", "row 2 must be untouched");
    assert!(
        buf_row(&app, 3).starts_with('X'),
        "row 3 must start with X, got: {:?}",
        buf_row(&app, 3)
    );
    assert_eq!(buf_row(&app, 4), "epsilon", "row 4 must be untouched");
}

#[test]
fn cfdo_runs_once_per_file() {
    // Build two real temp files, populate the quickfix list with entries
    // referencing them (2 entries in fileA, 1 in fileB), run :cfdo s/^/Z/,
    // and assert that the substitution ran exactly once per file.
    let dir = std::env::temp_dir().join(format!("hjkl_cfdo_test_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let file_a = dir.join("a.txt");
    let file_b = dir.join("b.txt");
    // fileA: 4 lines; fileB: 3 lines
    std::fs::write(&file_a, "alpha\nbeta\ngamma\ndelta\n").unwrap();
    std::fs::write(&file_b, "uno\ndos\ntres\n").unwrap();

    let mut app = App::new(None, false, None, None).unwrap();
    // Populate quickfix: two entries in fileA (rows 0,1), one in fileB (row 0).
    app.quickfix.set(vec![
        QfEntry {
            path: file_a.clone(),
            row: 0,
            col: 0,
            kind: QfKind::Grep,
            message: "a0".into(),
        },
        QfEntry {
            path: file_a.clone(),
            row: 1,
            col: 0,
            kind: QfKind::Grep,
            message: "a1".into(),
        },
        QfEntry {
            path: file_b.clone(),
            row: 0,
            col: 0,
            kind: QfKind::Grep,
            message: "b0".into(),
        },
    ]);

    // :cfdo s/^/Z/ — runs once per distinct file.
    app.handle_quickfix_command(QfCommand::Do {
        cmd: "s/^/Z/".to_string(),
        per_file: true,
    });

    // After :cfdo the editor last opened fileB (last visited file).
    // fileA row 0 was the cursor position for fileA's visit → got Z prepended.
    // fileA row 1 (second entry, same file) was NOT visited.
    // Verify by opening files explicitly.
    app.dispatch_ex(&format!("e {}", file_a.display()));
    let row0_a = buf_row(&app, 0);
    let row1_a = buf_row(&app, 1);
    assert!(
        row0_a.starts_with('Z'),
        "fileA row 0 must start with Z (cfdo visited first entry), got: {row0_a:?}"
    );
    assert!(
        !row1_a.starts_with('Z'),
        "fileA row 1 must NOT start with Z (second entry skipped by cfdo), got: {row1_a:?}"
    );

    app.dispatch_ex(&format!("e {}", file_b.display()));
    let row0_b = buf_row(&app, 0);
    assert!(
        row0_b.starts_with('Z'),
        "fileB row 0 must start with Z (cfdo visited its single entry), got: {row0_b:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ── :colder / :cnewer list-stack tests (#261 Phase 5b) ──────────────────────

/// Helper: populate quickfix via Expr (non-append) with a given `%l:%c:%m` line.
fn populate_expr(app: &mut App, text: &str) {
    app.handle_quickfix_command(QfCommand::Expr {
        text: text.to_string(),
        append: false,
        jump: false,
    });
}

#[test]
fn colder_restores_previous_list() {
    let mut app = make_app_with_buffer();
    // List A: single entry at line 2 (row 1)
    populate_expr(&mut app, r#""2:1:a""#);
    let entries_a: Vec<_> = app.quickfix.entries().to_vec();
    // List B: single entry at line 4 (row 3)
    populate_expr(&mut app, r#""4:1:b""#);
    let entries_b: Vec<_> = app.quickfix.entries().to_vec();

    // Sanity: current is B.
    assert_eq!(app.quickfix.entries(), entries_b.as_slice());

    // :colder → current should be A.
    app.handle_quickfix_command(QfCommand::Older(1));
    assert_eq!(
        app.quickfix.entries(),
        entries_a.as_slice(),
        "colder should restore list A"
    );
    assert!(
        app.quickfix_older.is_empty(),
        "older should be empty after single colder"
    );
    assert_eq!(app.quickfix_newer.len(), 1, "newer should hold list B");

    // :cnewer → current should be B again.
    app.handle_quickfix_command(QfCommand::Newer(1));
    assert_eq!(
        app.quickfix.entries(),
        entries_b.as_slice(),
        "cnewer should restore list B"
    );
    assert!(
        app.quickfix_newer.is_empty(),
        "newer should be empty after cnewer"
    );
}

#[test]
fn populate_while_older_discards_newer_branch() {
    let mut app = make_app_with_buffer();
    // Populate A, then B, then go colder (back to A).
    populate_expr(&mut app, r#""2:1:a""#);
    let entries_a: Vec<_> = app.quickfix.entries().to_vec();
    populate_expr(&mut app, r#""4:1:b""#);
    app.handle_quickfix_command(QfCommand::Older(1));
    // Current = A, newer holds B.
    assert_eq!(app.quickfix.entries(), entries_a.as_slice());
    assert_eq!(app.quickfix_newer.len(), 1);

    // Populate C while at A (not at top of newer stack).
    populate_expr(&mut app, r#""6:1:c""#);

    // newer must be cleared — populating discards the redo branch.
    assert!(
        app.quickfix_newer.is_empty(),
        "newer branch must be cleared on fresh populate"
    );
    // :cnewer should now be a no-op (no newer entries).
    let entries_c: Vec<_> = app.quickfix.entries().to_vec();
    app.handle_quickfix_command(QfCommand::Newer(1));
    assert_eq!(
        app.quickfix.entries(),
        entries_c.as_slice(),
        "cnewer should be no-op when newer is empty"
    );
}

#[test]
fn colder_saturates_when_no_older() {
    let mut app = make_app_with_buffer();
    populate_expr(&mut app, r#""2:1:a""#);
    let entries_a: Vec<_> = app.quickfix.entries().to_vec();
    // No prior list — colder should leave current unchanged.
    app.handle_quickfix_command(QfCommand::Older(1));
    assert_eq!(
        app.quickfix.entries(),
        entries_a.as_slice(),
        "colder on oldest list should not change current"
    );
}

#[test]
fn cnewer_saturates_when_no_newer() {
    let mut app = make_app_with_buffer();
    populate_expr(&mut app, r#""2:1:a""#);
    let entries_a: Vec<_> = app.quickfix.entries().to_vec();
    // No newer list — cnewer should leave current unchanged.
    app.handle_quickfix_command(QfCommand::Newer(1));
    assert_eq!(
        app.quickfix.entries(),
        entries_a.as_slice(),
        "cnewer on newest list should not change current"
    );
}

#[test]
fn loclist_colder_cnewer_work_independently() {
    let mut app = make_app_with_buffer();
    app.active_editor_mut().settings_mut().errorformat = "%l:%c:%m".to_string();

    // Populate loclist A then B.
    app.handle_loclist_command(QfCommand::Expr {
        text: r#""2:1:la""#.to_string(),
        append: false,
        jump: false,
    });
    let loc_a: Vec<_> = app.loclist.entries().to_vec();
    app.handle_loclist_command(QfCommand::Expr {
        text: r#""4:1:lb""#.to_string(),
        append: false,
        jump: false,
    });
    let loc_b: Vec<_> = app.loclist.entries().to_vec();

    // quickfix must be untouched.
    assert!(app.quickfix.is_empty());

    // lolder → restore A.
    app.handle_loclist_command(QfCommand::Older(1));
    assert_eq!(app.loclist.entries(), loc_a.as_slice());

    // lnewer → restore B.
    app.handle_loclist_command(QfCommand::Newer(1));
    assert_eq!(app.loclist.entries(), loc_b.as_slice());
}

// ── :diagnostics / :ldiagnostics in-process tests (#261 Phase 5b A3) ────────

fn make_lsp_diag(
    start_row: usize,
    start_col: usize,
    severity: DiagSeverity,
    message: &str,
) -> LspDiag {
    LspDiag {
        start_row,
        start_col,
        end_row: start_row,
        end_col: start_col + message.len(),
        severity,
        message: message.to_string(),
        source: None,
        code: None,
    }
}

#[test]
fn diagnostics_populates_quickfix_from_all_buffers() {
    let mut app = App::new(None, false, None, None).unwrap();

    // Inject diagnostics onto the active (only) slot.
    app.active_mut().lsp_diags = vec![
        make_lsp_diag(5, 3, DiagSeverity::Error, "type mismatch"),
        make_lsp_diag(2, 0, DiagSeverity::Hint, "consider renaming"),
        make_lsp_diag(2, 10, DiagSeverity::Warning, "unused variable"),
    ];

    app.handle_quickfix_command(QfCommand::Diagnostics);

    // 3 diags, sorted by (path, row, col): row2/col0, row2/col10, row5/col3.
    assert_eq!(app.quickfix.len(), 3, "quickfix should have 3 entries");
    assert!(
        app.quickfix_open(),
        "dock should be open when diags present"
    );

    let entries = app.quickfix.entries();
    assert_eq!(entries[0].row, 2);
    assert_eq!(entries[0].col, 0);
    assert_eq!(entries[0].kind, QfKind::Note, "Hint → Note");

    assert_eq!(entries[1].row, 2);
    assert_eq!(entries[1].col, 10);
    assert_eq!(entries[1].kind, QfKind::Warning, "Warning → Warning");

    assert_eq!(entries[2].row, 5);
    assert_eq!(entries[2].col, 3);
    assert_eq!(entries[2].kind, QfKind::Error, "Error → Error");

    // Location list must be untouched.
    assert!(app.loclist.is_empty(), "loclist must not be touched");
}

#[test]
fn ldiagnostics_uses_current_buffer_only() {
    let mut app = App::new(None, false, None, None).unwrap();

    app.active_mut().lsp_diags = vec![
        make_lsp_diag(1, 0, DiagSeverity::Info, "info message"),
        make_lsp_diag(0, 5, DiagSeverity::Warning, "warn here"),
    ];

    app.handle_loclist_command(QfCommand::Diagnostics);

    // 2 diags, sorted: row0/col5, row1/col0.
    assert_eq!(app.loclist.len(), 2, "loclist should have 2 entries");
    assert!(app.loclist_open(), "loclist dock should be open");

    let entries = app.loclist.entries();
    assert_eq!(entries[0].row, 0);
    assert_eq!(entries[0].col, 5);
    assert_eq!(entries[0].kind, QfKind::Warning);

    assert_eq!(entries[1].row, 1);
    assert_eq!(entries[1].col, 0);
    assert_eq!(entries[1].kind, QfKind::Info);

    // Quickfix must be untouched.
    assert!(app.quickfix.is_empty(), "quickfix must not be touched");
    assert!(!app.quickfix_open(), "quickfix dock must not be open");
}

#[test]
fn diagnostics_empty_no_dock() {
    let mut app = App::new(None, false, None, None).unwrap();

    // No diags injected — lsp_diags is empty by default.
    app.handle_quickfix_command(QfCommand::Diagnostics);

    assert!(app.quickfix.is_empty(), "list must remain empty");
    assert!(!app.quickfix_open(), "dock must stay closed when no diags");
}

/// `:cwindow` — opens the dock only when the list has entries; closes it
/// when the list is empty; empty-list invocation is silent (no toast).
#[test]
fn cwindow_opens_on_entries_closes_on_empty() {
    let mut app = App::new(None, false, None, None).unwrap();

    // Empty list: stays closed, no message (unlike `:copen`'s "list is empty").
    let toasts_before = app.bus.history().count();
    app.handle_quickfix_command(QfCommand::Window);
    assert!(
        !app.quickfix_open(),
        ":cwindow on an empty list must not open"
    );
    assert_eq!(
        app.bus.history().count(),
        toasts_before,
        ":cwindow on an empty list is silent in vim"
    );

    // Non-empty list: opens.
    let p = std::path::PathBuf::from("x.rs");
    app.quickfix.set(vec![entry(&p, 0)]);
    app.handle_quickfix_command(QfCommand::Window);
    assert!(
        app.quickfix_open(),
        ":cwindow must open when the list has entries"
    );

    // List emptied while the dock is open: `:cwindow` closes it.
    app.quickfix.set(vec![]);
    app.handle_quickfix_command(QfCommand::Window);
    assert!(
        !app.quickfix_open(),
        ":cwindow must close the dock when the list is empty"
    );
}

/// `:lwindow` — same contract against the location list.
#[test]
fn lwindow_opens_on_entries_closes_on_empty() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.handle_loclist_command(QfCommand::Window);
    assert!(!app.loclist_open());

    let p = std::path::PathBuf::from("x.rs");
    app.loclist.set(vec![entry(&p, 0)]);
    app.handle_loclist_command(QfCommand::Window);
    assert!(app.loclist_open());

    app.loclist.set(vec![]);
    app.handle_loclist_command(QfCommand::Window);
    assert!(!app.loclist_open());
}

// ── Bottom dock (#63 Phase B): real window/buffer, not a popup ─────────────

/// Build two real temp files and a quickfix list with an entry in each, so
/// dock tests can exercise real jumps.
fn make_app_with_qf_files() -> (App, std::path::PathBuf, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let file_a = dir.path().join("a.txt");
    let file_b = dir.path().join("b.txt");
    std::fs::write(&file_a, "a-line0\na-line1\na-line2\n").unwrap();
    std::fs::write(&file_b, "b-line0\nb-line1\n").unwrap();

    let mut app = App::new(None, false, None, None).unwrap();
    app.quickfix.set(vec![
        QfEntry {
            path: file_a.clone(),
            row: 0,
            col: 0,
            kind: QfKind::Grep,
            message: "first hit".into(),
        },
        QfEntry {
            path: file_a.clone(),
            row: 1,
            col: 2,
            kind: QfKind::Grep,
            message: "second hit".into(),
        },
        QfEntry {
            path: file_b.clone(),
            row: 0,
            col: 0,
            kind: QfKind::Grep,
            message: "third hit".into(),
        },
    ]);
    (app, file_a, dir)
}

/// `:copen` creates a bottom-dock window whose buffer has one line per
/// entry, formatted `path │ row:col │ message` (1-based row/col, columns
/// aligned across the list), and focuses it.
#[test]
fn copen_creates_bottom_dock_with_matching_buffer_lines() {
    let (mut app, file_a, _dir) = make_app_with_qf_files();

    app.handle_quickfix_command(QfCommand::Open);

    let dock = app.bottom_dock.as_ref().expect("bottom dock must be open");
    assert_eq!(dock.kind, crate::app::dock::DockKind::Quickfix);
    assert_eq!(
        app.focused_window(),
        dock.win_id,
        ":copen must focus the dock"
    );

    let rope = app.active_editor().buffer().rope();
    assert_eq!(rope.len_lines(), 3, "one line per entry");
    let line0 = hjkl_buffer::rope_line_str(&rope, 0);
    let line1 = hjkl_buffer::rope_line_str(&rope, 1);
    // Both entries share one path, so the path column needs no padding; the
    // location column right-aligns 1:1 and 2:3 (equal width here).
    assert_eq!(
        line0,
        format!("{} │ 1:1 │ first hit", file_a.display()),
        "row/col must be rendered 1-based with aligned columns"
    );
    assert_eq!(line1, format!("{} │ 2:3 │ second hit", file_a.display()));
    assert_eq!(
        app.window_cursor(dock.win_id).0,
        0,
        "cursor starts at entry 0"
    );
}

/// THE regression test for the user's original complaint: the quickfix dock
/// is a real buffer, so vim motions (j/k) and operators (yy) work on it —
/// unlike the old `Clear`+`List` overlay, which only understood j/k/Enter/Esc/q.
#[test]
fn dock_j_k_move_a_real_cursor_without_touching_the_list_cursor() {
    let (mut app, _file_a, _dir) = make_app_with_qf_files();
    app.handle_quickfix_command(QfCommand::Open);
    let dock_win = app.bottom_dock.as_ref().unwrap().win_id;

    super::macro_key_seq(&mut app, &[super::ck('j')]);
    assert_eq!(
        app.window_cursor(dock_win).0,
        1,
        "j must move the dock cursor"
    );
    super::macro_key_seq(&mut app, &[super::ck('j')]);
    assert_eq!(app.window_cursor(dock_win).0, 2);
    super::macro_key_seq(&mut app, &[super::ck('k')]);
    assert_eq!(app.window_cursor(dock_win).0, 1);

    // j/k alone must NOT commit to the list's "current entry" pointer —
    // only `<CR>` (vim's `:cc`-equivalent) does that.
    assert_eq!(
        app.quickfix.cursor(),
        0,
        "list cursor must stay put until <CR>"
    );
}

/// `yy` in the dock yanks the entry line into the unnamed register — the
/// concrete regression test for "no yank" in the old popup.
#[test]
fn dock_yy_yanks_the_entry_line() {
    let (mut app, file_a, _dir) = make_app_with_qf_files();
    app.handle_quickfix_command(QfCommand::Open);

    super::macro_key_seq(&mut app, &[super::ck('y'), super::ck('y')]);

    let yanked = app
        .active_editor()
        .with_registers(|r| r.read('"').map(|s| s.text.clone()))
        .unwrap_or_default();
    assert_eq!(
        yanked.trim_end_matches('\n'),
        format!("{} │ 1:1 │ first hit", file_a.display()),
        "yy must yank the exact rendered entry line, got {yanked:?}"
    );
}

/// `<CR>` jumps to the entry under the dock's cursor: commits the dock
/// cursor row to the list's current-entry pointer, opens the target file,
/// and moves focus to a REGULAR window — never back into the dock itself.
#[test]
fn dock_enter_jumps_to_entry_and_focus_lands_on_a_regular_window() {
    let (mut app, file_a, _dir) = make_app_with_qf_files();
    app.handle_quickfix_command(QfCommand::Open);
    let dock_win = app.bottom_dock.as_ref().unwrap().win_id;

    // Move to entry 1 (a.txt row 1 col 2) and jump.
    super::macro_key_seq(&mut app, &[super::ck('j')]);
    super::macro_key_seq(&mut app, &[super::key(crossterm::event::KeyCode::Enter)]);

    assert_eq!(
        app.quickfix.cursor(),
        1,
        "<CR> must commit to the list cursor"
    );
    assert_ne!(
        app.focused_window(),
        dock_win,
        "<CR> must move focus OFF the dock"
    );
    assert!(
        app.window_is_regular(app.focused_window()),
        "the jump target must be a regular window"
    );
    assert_eq!(app.active().filename.as_deref(), Some(file_a.as_path()));
    assert_eq!(app.active_editor().cursor(), (1, 2));
    assert!(
        app.bottom_dock.is_some(),
        "the dock itself must remain open after the jump (vim parity)"
    );
}

/// The dock buffer is readonly: `x`, `dd`, and `i...<Esc>` must all be
/// rejected without changing a single character.
#[test]
fn dock_buffer_is_readonly_and_rejects_edits() {
    let (mut app, _file_a, _dir) = make_app_with_qf_files();
    app.handle_quickfix_command(QfCommand::Open);
    let before = app.active_editor().buffer().rope().to_string();

    super::macro_key_seq(&mut app, &[super::ck('x')]);
    assert_eq!(
        app.active_editor().buffer().rope().to_string(),
        before,
        "x must not delete a character in the readonly dock"
    );

    super::macro_key_seq(&mut app, &[super::ck('d'), super::ck('d')]);
    assert_eq!(
        app.active_editor().buffer().rope().to_string(),
        before,
        "dd must not delete a line in the readonly dock"
    );

    super::macro_key_seq(
        &mut app,
        &[
            super::ck('i'),
            super::ck('X'),
            super::key(crossterm::event::KeyCode::Esc),
        ],
    );
    assert_eq!(
        app.active_editor().buffer().rope().to_string(),
        before,
        "insert must not add a character in the readonly dock"
    );
}

/// `:cnext` while the dock is open moves the dock's highlighted row to match
/// the newly-current entry.
#[test]
fn cnext_syncs_the_dock_cursor_row() {
    let (mut app, _file_a, _dir) = make_app_with_qf_files();
    app.handle_quickfix_command(QfCommand::Open);
    let dock_win = app.bottom_dock.as_ref().unwrap().win_id;
    assert_eq!(app.window_cursor(dock_win).0, 0);

    app.handle_quickfix_command(QfCommand::Next);

    assert_eq!(app.quickfix.cursor(), 1);
    assert_eq!(
        app.window_cursor(dock_win).0,
        1,
        ":cnext must move the dock's cursor to the new current entry"
    );
}

/// `:lopen` while `:copen`'s dock is showing REUSES the same window/slot and
/// just retargets which list it displays — vim shows one such window at a
/// time in practice.
#[test]
fn lopen_reuses_the_open_quickfix_dock() {
    let (mut app, _file_a, _dir) = make_app_with_qf_files();
    app.handle_quickfix_command(QfCommand::Open);
    let qf_dock_win = app.bottom_dock.as_ref().unwrap().win_id;

    let p = std::path::PathBuf::from("loc.rs");
    app.loclist.set(vec![entry(&p, 4)]);
    app.handle_loclist_command(QfCommand::Open);

    let dock = app.bottom_dock.as_ref().expect("dock must still be open");
    assert_eq!(dock.win_id, qf_dock_win, "the SAME window/slot is reused");
    assert_eq!(dock.kind, crate::app::dock::DockKind::Loclist);
    assert!(!app.quickfix_open());
    assert!(app.loclist_open());

    let rope = app.active_editor().buffer().rope();
    assert_eq!(rope.len_lines(), 1, "buffer now shows the loclist's entry");
    let line0 = hjkl_buffer::rope_line_str(&rope, 0);
    assert_eq!(line0, "loc.rs │ 5:1 │ hit at 4");
}
