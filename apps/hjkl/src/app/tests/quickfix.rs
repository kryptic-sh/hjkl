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
