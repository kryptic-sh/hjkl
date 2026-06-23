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
