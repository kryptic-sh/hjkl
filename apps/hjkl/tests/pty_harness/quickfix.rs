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

    let mut session = TerminalSession::spawn_in_dir_with_file_wide(tmp.path(), &target);

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
    // (`path:line:col message` — single space, no alignment or separator;
    // see `qf_row_layouts`). Poll for the redraw rather than scanning once —
    // `keys()` only waits a fixed settle, which the macOS pty can outlast.
    let shows_entry = session.wait_for_screen_contains("target.txt:2:1 sample message", 2000);
    assert!(
        shows_entry,
        "the :copen dock buffer must show the formatted quickfix entry\nscreen:\n{}",
        session.dump_screen()
    );

    // `yy` — a REAL vim yank, impossible against the old Clear+List overlay
    // (the user's original complaint). `:cclose` returns focus to the
    // regular window; `Gp` pastes the yanked line after the last line so we
    // can observe it landed.
    session.keys("yy");
    session.keys(":cclose<Enter>");
    session.keys("Gp");

    let pasted = session.wait_for_screen_contains("target.txt:2:1 sample message", 2000);
    assert!(
        pasted,
        "the dock-yanked line must paste into the regular buffer, proving \
         `yy` actually worked against a real buffer"
    );
}

/// Populate three entries, `:copen`, navigate the dock with real vim motions
/// (`j`) and a real incremental search (`/second<Enter>`), then `<CR>` jumps
/// to the entry under the cursor. End-to-end twin of the in-process
/// `qf_dock_jump_at_cursor` / `qf_after_nav` unit tests: proves the whole
/// chain — dock is a real searchable/navigable buffer, `<CR>` reads the
/// RIGHT row, and the jump lands in the main area (not back in the readonly
/// dock) on the correct file and line (#63 Phase C).
#[test]
fn copen_dock_vim_navigate_then_enter_jumps_to_correct_entry() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("aaa.txt"), "a1\na2\na3\n").unwrap();
    std::fs::write(tmp.path().join("bbb.txt"), "b1\nb2\nb3\n").unwrap();
    std::fs::write(tmp.path().join("ccc.txt"), "c1\nc2\nc3\n").unwrap();
    let first = tmp.path().join("aaa.txt");

    let mut session = TerminalSession::spawn_in_dir_with_file_wide(tmp.path(), &first);

    // Populate all three entries in one `:cexpr` — the quoted-string form
    // (`parse_expr_text`) expands `\n` into real newlines, so this is
    // equivalent to three `:caddexpr` calls but in one round-trip.
    session.keys(":set errorformat=\\%f:\\%l:\\%c:\\%m<Enter>");
    session.keys(":cexpr \"aaa.txt:1:1:first\\nbbb.txt:2:1:second\\nccc.txt:3:1:third\"<Enter>");
    session.keys(":copen<Enter>");

    let shows_all = session.wait_for_screen_contains("aaa.txt:1:1 first", 2000)
        && session.wait_for_screen_contains("bbb.txt:2:1 second", 2000)
        && session.wait_for_screen_contains("ccc.txt:3:1 third", 2000);
    assert!(
        shows_all,
        "dock must list all three quickfix entries\nscreen:\n{}",
        session.dump_screen()
    );

    // `j`: real vim motion moves the dock's cursor off entry 0 (first).
    // `/second<Enter>`: real incremental search lands the cursor on the
    // "second" entry's row — impossible against the old Clear+List overlay,
    // which owned every keypress and had no buffer for `/` to search.
    session.keys("j");
    session.keys("/second<Enter>");

    // `<CR>`: jump to the entry under the cursor (`qf_dock_jump_at_cursor`).
    session.keys("<Enter>");

    // Must land in a REGULAR window on bbb.txt at (0-based) row 1 — the
    // screen row the cursor cell sits on must render "b2" (bbb.txt's 2nd
    // line), not "b1"/"b3" or anywhere in the still-open dock below.
    let (cursor_row, _) = session.cursor_cell_wait();
    let cursor_line = session.line(cursor_row);
    assert!(
        cursor_line.contains("b2"),
        "the jump must land the cursor on bbb.txt's line 2 (\"b2\"), the \
         \"second\" entry's target line; cursor is on row {cursor_row} \
         which renders {cursor_line:?}\nscreen:\n{}",
        (0..24)
            .map(|r| session.line(r))
            .collect::<Vec<_>>()
            .join("\n")
    );
    // Also confirm bbb.txt (not aaa.txt) is now the focused buffer, via the
    // status line filename.
    let status_shows_bbb = session.wait_for_screen_contains("bbb.txt", 2000);
    assert!(
        status_shows_bbb,
        "bbb.txt must be the file that was opened by the jump"
    );

    // The dock itself must still be open (vim's `<CR>` moves focus to the
    // target window but does not close the quickfix window) and must still
    // show all three entries — the jump must not have torn anything down.
    let dock_still_open = session.wait_for_screen_contains("bbb.txt:2:1 second", 2000);
    assert!(
        dock_still_open,
        "the quickfix dock must stay open after <CR> jumps out of it"
    );
}

/// `-q [ERRORFILE]` (nvim "quickfix mode" alias, mirrored via `-q`
/// dispatching `:cfile <errfile>` at startup — see `main`): reads the
/// errorfile through the DEFAULT `&errorformat`
/// (`"%f:%l:%c:%m,%f:%l:%m,%l:%c:%m"` — see `Settings::default`), so no
/// `:set errorformat=` is needed here (unlike the `:cexpr` tests above,
/// which set it explicitly to dodge `%` filename-expansion on ex-command
/// args — `-q` never goes through that path since it reads the file
/// directly). Populates the quickfix list AND jumps to the first entry,
/// opening a DIFFERENT file than the one given on argv — proving both
/// halves of nvim's `-q` contract.
#[test]
fn dash_q_flag_populates_quickfix_and_jumps_to_first_error() {
    let tmp = tempfile::tempdir().unwrap();
    let opened = tmp.path().join("main.txt");
    std::fs::write(&opened, "m1\nm2\nm3\n").unwrap();
    std::fs::write(tmp.path().join("second.txt"), "s1\ns2\ns3\n").unwrap();
    // "second.txt:2:1:oops" matches `%f:%l:%c:%m` → path=second.txt,
    // line=2, col=1, message="oops".
    std::fs::write(tmp.path().join("errors.err"), "second.txt:2:1:oops\n").unwrap();

    let mut session = TerminalSession::spawn_in_dir_with_file_config_args_wide(
        tmp.path(),
        &opened,
        "",
        &["-q", "errors.err"],
    );

    // Cursor must land on second.txt's (0-based) line 1 — "s2" — not
    // anywhere in main.txt, proving the jump actually switched buffers.
    let (cursor_row, _) = session.cursor_cell_wait();
    let cursor_line = session.line(cursor_row);
    assert!(
        cursor_line.contains("s2"),
        "-q must jump to second.txt's line 2 (\"s2\"); cursor is on row \
         {cursor_row} which renders {cursor_line:?}\nscreen:\n{}",
        (0..24)
            .map(|r| session.line(r))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let status_shows_second = session.wait_for_screen_contains("second.txt", 2000);
    assert!(
        status_shows_second,
        "second.txt must be the focused buffer after -q's jump"
    );

    // The quickfix list itself must be populated too (not just the jump) —
    // `:copen` shows the dock with the parsed entry.
    session.keys(":copen<Enter>");
    let shows_entry = session.wait_for_screen_contains("second.txt:2:1 oops", 2000);
    assert!(
        shows_entry,
        "the quickfix list populated by -q must contain the parsed entry\nscreen:\n{}",
        session.dump_screen()
    );
}
