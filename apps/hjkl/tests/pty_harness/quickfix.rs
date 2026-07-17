//! Quickfix bottom-dock end-to-end test (#63 Phase B): drive the real
//! `hjkl` binary under a pty — populate the quickfix list, `:copen` into a
//! REAL window/buffer, yank an entry line with real vim `yy`, `:cclose`, and
//! prove the yank actually landed by pasting it into the regular window.
//! This is the end-to-end twin of the in-process
//! `dock_yy_yanks_the_entry_line` test in `app/tests/quickfix.rs`.

use super::harness::TerminalSession;

/// Populates via `:cexpr` (simplest population path through a pty — no
/// external `grep`/`rg` binary dependency, and no shell/ex quoting headaches:
/// `:cexpr`'s argument is used verbatim when it isn't quote-wrapped, see
/// `qf_run_expr`/`parse_expr_text`).
#[test]
fn copen_dock_supports_real_yank_then_closes() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("target.txt");
    std::fs::write(&target, "alpha\nbeta\ngamma\n").unwrap();

    let mut session = TerminalSession::spawn_in_dir_with_file(tmp.path(), &target);

    // `%f:%l:%c:%m` matches "target.txt:2:1:sample message" → one entry at
    // (1-based) line 2, col 1, message "sample message". Every bare `%` is
    // escaped as `\%` here: ex-command args go through `expand_args`
    // (vim-style `%`/`#` filename expansion) for every command except
    // substitute/global/vglobal/normal — `set` is NOT exempt, so a literal
    // `%f` would otherwise get clobbered into "<current file>f".
    session.keys(":set errorformat=\\%f:\\%l:\\%c:\\%m<Enter>");
    session.keys(":cexpr target.txt:2:1:sample message<Enter>");
    session.keys(":copen<Enter>");

    // The dock is a real buffer: some row must render the formatted entry
    // (`path|row col N| message`, #63 Phase B's `qf_format_list`).
    let shows_entry = (0..24).any(|r| session.line(r).contains("|2 col 1| sample message"));
    assert!(
        shows_entry,
        "the :copen dock buffer must show the formatted quickfix entry"
    );

    // `yy` — a REAL vim yank, impossible against the old Clear+List overlay
    // (the user's original complaint). `:cclose` returns focus to the
    // regular window; `Gp` pastes the yanked line after the last line so we
    // can observe it landed.
    session.keys("yy");
    session.keys(":cclose<Enter>");
    session.keys("Gp");

    let pasted = (0..24).any(|r| session.line(r).contains("|2 col 1| sample message"));
    assert!(
        pasted,
        "the dock-yanked line must paste into the regular buffer, proving \
         `yy` actually worked against a real buffer"
    );
}
