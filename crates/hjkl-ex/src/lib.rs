//! Ex-command registry and dispatch layer for the hjkl editor stack.
//!
//! Phase 1: provides an extensible [`Registry`] and a minimal set of
//! built-in commands (`:q`, `:q!`). Additional commands migrate in
//! subsequent phases.
//!
//! Phase 2a: adds range parsing infrastructure and migrates the no-arg /
//! no-range terminal commands (`:w`, `:wq`, `:x`, `:wa`, `:wqa`, `:noh`,
//! `:undo`, `:redo`, `:qall`, `:qall!`, `:wqall`, `:wqall!`).

pub use complete::{
    ArgSources, CompletionKind, Completions, collect_host_registry_names, collect_registry_names,
    complete, complete_arg, complete_command_from_names, first_word_end, longest_common_prefix,
};
pub use effect::ExEffect;
pub use range::{LineRange, parse_range};
pub use registry::{ArgKind, ExCommand, HostCmd, HostRegistry, Registry};

mod builtins;
mod complete;
mod effect;
mod listings;
mod parse;
mod range;
mod registry;
mod setopt;

pub use setopt::all_setting_names;

/// Try to dispatch `input` (without the leading `:`) through the registry.
///
/// A leading range prefix (`5`, `5,10`, `.,$`, `%`, `'a,'b`) is parsed and
/// stripped before command resolution — Phase 2d commands will receive the
/// range; Phase 2a no-arg commands ignore it. If the range is syntactically
/// invalid the error is surfaced as `Some(ExEffect::Error(...))`.
///
/// Returns:
/// - `Some(ExEffect)` when a registered command handled it
/// - `None` when no registered command matched (caller falls back to legacy handling)
pub fn try_dispatch<H: hjkl_engine::Host>(
    reg: &Registry<H>,
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    input: &str,
) -> Option<ExEffect> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }

    // Parse a leading range (`5`, `5,10`, `.,$`, `%`, `'a,'b`).
    let (range, cmd_str) = match parse_range(input, editor) {
        Ok(pair) => pair,
        Err(e) => return Some(ExEffect::Error(e)),
    };

    let (name, args) = parse::split_name_args(cmd_str);
    if name.is_empty() {
        // Bare `:N` or bare range — jump to line.
        return handle_bare_line_number(editor, cmd_str, range);
    }
    let cmd = reg.resolve(name)?;
    // Handler may return None to defer this invocation to the legacy path.
    (cmd.run)(editor, args, range)
}

/// Try to dispatch `input` (without the leading `:`) through a host registry.
///
/// Returns `Some(ExEffect)` when a host command claimed the invocation,
/// `None` when no command matched or the matched command deferred.
///
/// Unlike [`try_dispatch`] this function does not parse a range prefix — host
/// commands in Phase 4 don't accept ranges.
pub fn try_dispatch_host<Ctx>(
    reg: &HostRegistry<Ctx>,
    ctx: &mut Ctx,
    input: &str,
) -> Option<ExEffect> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    let (name, args) = parse::split_name_args(input);
    if name.is_empty() {
        return None;
    }
    let cmd = reg.resolve(name)?;
    cmd.run(ctx, args)
}

/// Handle bare `:N` (jump to line N) and bare `:{range}` (jump to range start).
///
/// - `cmd_str` parses as `usize` AND `range.is_none()` → goto that line.
/// - `range.is_some()` AND `cmd_str.is_empty()` → goto range start (vim semantics).
/// - Otherwise → `None` (let caller fall back to legacy).
fn handle_bare_line_number<H: hjkl_engine::Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    cmd_str: &str,
    range: Option<LineRange>,
) -> Option<ExEffect> {
    if let Ok(line) = cmd_str.trim().parse::<usize>()
        && range.is_none()
    {
        editor.goto_line(line);
        return Some(ExEffect::Ok);
    }
    if let Some(r) = range
        && cmd_str.trim().is_empty()
    {
        editor.goto_line(r.start_one_based());
        return Some(ExEffect::Ok);
    }
    None
}

/// Build a [`Registry`] seeded with the Phase 1 + Phase 2a default commands.
pub fn default_registry<H: hjkl_engine::Host>() -> Registry<H> {
    let mut reg = Registry::new();
    builtins::register_builtins(&mut reg);
    reg
}

#[cfg(test)]
mod tests {
    use super::*;
    use hjkl_engine::{DefaultHost, Editor, Options};

    fn make_editor() -> Editor<hjkl_buffer::Buffer, DefaultHost> {
        let buf = hjkl_buffer::Buffer::new();
        let host = DefaultHost::new();
        Editor::new(buf, host, Options::default())
    }

    fn make_editor_with_lines(lines: &[&str]) -> Editor<hjkl_buffer::Buffer, DefaultHost> {
        let content = lines.join("\n");
        let buf = hjkl_buffer::Buffer::from_str(&content);
        let host = DefaultHost::new();
        Editor::new(buf, host, Options::default())
    }

    // ---- Phase 1 tests (kept) ----------------------------------------------

    #[test]
    fn dispatch_q_returns_quit() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        let result = try_dispatch(&reg, &mut editor, "q");
        assert_eq!(
            result,
            Some(ExEffect::Quit {
                force: false,
                save: false
            })
        );
    }

    #[test]
    fn dispatch_quit_returns_quit() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        let result = try_dispatch(&reg, &mut editor, "quit");
        assert_eq!(
            result,
            Some(ExEffect::Quit {
                force: false,
                save: false
            })
        );
    }

    #[test]
    fn dispatch_q_bang_returns_force_quit() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        let result = try_dispatch(&reg, &mut editor, "q!");
        assert_eq!(
            result,
            Some(ExEffect::Quit {
                force: true,
                save: false
            })
        );
    }

    #[test]
    fn dispatch_nonexistent_returns_none() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        let result = try_dispatch(&reg, &mut editor, "nonexistent");
        assert_eq!(result, None);
    }

    #[test]
    fn dispatch_empty_returns_none() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        let result = try_dispatch(&reg, &mut editor, "");
        assert_eq!(result, None);
    }

    #[test]
    fn dispatch_whitespace_only_returns_none() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        let result = try_dispatch(&reg, &mut editor, "   ");
        assert_eq!(result, None);
    }

    // ---- Phase 2a: write ---------------------------------------------------

    #[test]
    fn dispatch_w_returns_save() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(try_dispatch(&reg, &mut editor, "w"), Some(ExEffect::Save));
    }

    #[test]
    fn dispatch_write_returns_save() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(
            try_dispatch(&reg, &mut editor, "write"),
            Some(ExEffect::Save)
        );
    }

    #[test]
    fn dispatch_w_with_path_returns_save_as_phase_2b() {
        // Phase 2b: handler returns Some(SaveAs) for non-empty args.
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        let result = try_dispatch(&reg, &mut editor, "w /tmp/foo.txt");
        assert_eq!(result, Some(ExEffect::SaveAs("/tmp/foo.txt".into())));
    }

    #[test]
    fn dispatch_wa_returns_save() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(try_dispatch(&reg, &mut editor, "wa"), Some(ExEffect::Save));
    }

    #[test]
    fn dispatch_wall_returns_save() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(
            try_dispatch(&reg, &mut editor, "wall"),
            Some(ExEffect::Save)
        );
    }

    // ---- Phase 2a: wq / x --------------------------------------------------

    #[test]
    fn dispatch_wq_returns_quit_save() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(
            try_dispatch(&reg, &mut editor, "wq"),
            Some(ExEffect::Quit {
                force: false,
                save: true
            })
        );
    }

    #[test]
    fn dispatch_x_returns_quit_save() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(
            try_dispatch(&reg, &mut editor, "x"),
            Some(ExEffect::Quit {
                force: false,
                save: true
            })
        );
    }

    #[test]
    fn dispatch_wq_bang_returns_force_quit_save() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(
            try_dispatch(&reg, &mut editor, "wq!"),
            Some(ExEffect::Quit {
                force: true,
                save: true
            })
        );
    }

    #[test]
    fn dispatch_x_bang_returns_force_quit_save() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(
            try_dispatch(&reg, &mut editor, "x!"),
            Some(ExEffect::Quit {
                force: true,
                save: true
            })
        );
    }

    // ---- Phase 2a: wqall ---------------------------------------------------

    #[test]
    fn dispatch_wqa_returns_quit_save() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(
            try_dispatch(&reg, &mut editor, "wqa"),
            Some(ExEffect::Quit {
                force: false,
                save: true
            })
        );
    }

    #[test]
    fn dispatch_wqall_returns_quit_save() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(
            try_dispatch(&reg, &mut editor, "wqall"),
            Some(ExEffect::Quit {
                force: false,
                save: true
            })
        );
    }

    #[test]
    fn dispatch_wqa_bang_returns_quit_save() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(
            try_dispatch(&reg, &mut editor, "wqa!"),
            Some(ExEffect::Quit {
                force: false,
                save: true
            })
        );
    }

    #[test]
    fn dispatch_wqall_bang_returns_quit_save() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(
            try_dispatch(&reg, &mut editor, "wqall!"),
            Some(ExEffect::Quit {
                force: false,
                save: true
            })
        );
    }

    // ---- Phase 2a: qall ----------------------------------------------------

    #[test]
    fn dispatch_qa_returns_quit_no_save() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(
            try_dispatch(&reg, &mut editor, "qa"),
            Some(ExEffect::Quit {
                force: false,
                save: false
            })
        );
    }

    #[test]
    fn dispatch_qall_returns_quit_no_save() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(
            try_dispatch(&reg, &mut editor, "qall"),
            Some(ExEffect::Quit {
                force: false,
                save: false
            })
        );
    }

    #[test]
    fn dispatch_qa_bang_returns_force_quit_no_save() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(
            try_dispatch(&reg, &mut editor, "qa!"),
            Some(ExEffect::Quit {
                force: true,
                save: false
            })
        );
    }

    #[test]
    fn dispatch_qall_bang_returns_force_quit_no_save() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(
            try_dispatch(&reg, &mut editor, "qall!"),
            Some(ExEffect::Quit {
                force: true,
                save: false
            })
        );
    }

    // ---- Phase 2a: nohlsearch ----------------------------------------------

    #[test]
    fn dispatch_noh_clears_search_and_returns_ok() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(try_dispatch(&reg, &mut editor, "noh"), Some(ExEffect::Ok));
    }

    #[test]
    fn dispatch_nohl_returns_ok() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(try_dispatch(&reg, &mut editor, "nohl"), Some(ExEffect::Ok));
    }

    #[test]
    fn dispatch_nohlsearch_returns_ok() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(
            try_dispatch(&reg, &mut editor, "nohlsearch"),
            Some(ExEffect::Ok)
        );
    }

    // ---- Phase 2a: undo / redo ---------------------------------------------

    #[test]
    fn dispatch_u_returns_ok() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(try_dispatch(&reg, &mut editor, "u"), Some(ExEffect::Ok));
    }

    #[test]
    fn dispatch_undo_returns_ok() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(try_dispatch(&reg, &mut editor, "undo"), Some(ExEffect::Ok));
    }

    #[test]
    fn dispatch_redo_returns_ok() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(try_dispatch(&reg, &mut editor, "redo"), Some(ExEffect::Ok));
    }

    // `red` → min_prefix=3 so `:red` resolves to `:redo`
    #[test]
    fn dispatch_red_returns_ok() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(try_dispatch(&reg, &mut editor, "red"), Some(ExEffect::Ok));
    }

    // `:re` — redo needs min_prefix=3 so doesn't match; read matches (min_prefix=1).
    // `:re` unambiguously resolves to `:read` with no args → None (no path).
    #[test]
    fn dispatch_re_resolves_to_read_no_args() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        // read_handler returns None when no path given, so try_dispatch returns None.
        assert_eq!(try_dispatch(&reg, &mut editor, "re"), None);
    }

    // ---- Phase 2b: write with path -----------------------------------------

    #[test]
    fn dispatch_write_with_path_returns_save_as() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(
            try_dispatch(&reg, &mut editor, "write foo.txt"),
            Some(ExEffect::SaveAs("foo.txt".into()))
        );
    }

    // ---- Phase 2b: edit ----------------------------------------------------

    #[test]
    fn dispatch_e_with_path_returns_edit_file() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(
            try_dispatch(&reg, &mut editor, "e foo.txt"),
            Some(ExEffect::EditFile {
                path: "foo.txt".into(),
                force: false
            })
        );
    }

    #[test]
    fn dispatch_edit_with_path_returns_edit_file() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(
            try_dispatch(&reg, &mut editor, "edit src/main.rs"),
            Some(ExEffect::EditFile {
                path: "src/main.rs".into(),
                force: false
            })
        );
    }

    #[test]
    fn dispatch_e_no_args_returns_edit_file_empty_path() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        // No-arg edit: reload current buffer (empty path, no force).
        assert_eq!(
            try_dispatch(&reg, &mut editor, "e"),
            Some(ExEffect::EditFile {
                path: "".into(),
                force: false
            })
        );
    }

    #[test]
    fn dispatch_e_bang_with_path_returns_edit_file_force() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(
            try_dispatch(&reg, &mut editor, "e! foo.txt"),
            Some(ExEffect::EditFile {
                path: "foo.txt".into(),
                force: true
            })
        );
    }

    // ---- Phase 2b: read ----------------------------------------------------

    #[test]
    fn dispatch_r_with_path_returns_read_file() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(
            try_dispatch(&reg, &mut editor, "r LICENSE"),
            Some(ExEffect::ReadFile {
                path: "LICENSE".into()
            })
        );
    }

    #[test]
    fn dispatch_read_with_path_returns_read_file() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(
            try_dispatch(&reg, &mut editor, "read LICENSE"),
            Some(ExEffect::ReadFile {
                path: "LICENSE".into()
            })
        );
    }

    #[test]
    fn dispatch_r_no_args_returns_none() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(try_dispatch(&reg, &mut editor, "r"), None);
    }

    // ---- Phase 2b: bdelete -------------------------------------------------

    #[test]
    fn dispatch_bd_returns_buffer_delete() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(
            try_dispatch(&reg, &mut editor, "bd"),
            Some(ExEffect::BufferDelete {
                force: false,
                wipe: false
            })
        );
    }

    #[test]
    fn dispatch_bdelete_returns_buffer_delete() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(
            try_dispatch(&reg, &mut editor, "bdelete"),
            Some(ExEffect::BufferDelete {
                force: false,
                wipe: false
            })
        );
    }

    #[test]
    fn dispatch_bd_bang_returns_buffer_delete_force() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(
            try_dispatch(&reg, &mut editor, "bd!"),
            Some(ExEffect::BufferDelete {
                force: true,
                wipe: false
            })
        );
    }

    #[test]
    fn dispatch_bdelete_bang_returns_buffer_delete_force() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(
            try_dispatch(&reg, &mut editor, "bdelete!"),
            Some(ExEffect::BufferDelete {
                force: true,
                wipe: false
            })
        );
    }

    // ---- Phase 2b: bwipeout ------------------------------------------------

    #[test]
    fn dispatch_bw_returns_buffer_wipeout() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(
            try_dispatch(&reg, &mut editor, "bw"),
            Some(ExEffect::BufferDelete {
                force: false,
                wipe: true
            })
        );
    }

    #[test]
    fn dispatch_bwipeout_returns_buffer_wipeout() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(
            try_dispatch(&reg, &mut editor, "bwipeout"),
            Some(ExEffect::BufferDelete {
                force: false,
                wipe: true
            })
        );
    }

    #[test]
    fn dispatch_bw_bang_returns_buffer_wipeout_force() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(
            try_dispatch(&reg, &mut editor, "bw!"),
            Some(ExEffect::BufferDelete {
                force: true,
                wipe: true
            })
        );
    }

    #[test]
    fn dispatch_bwipeout_bang_returns_buffer_wipeout_force() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(
            try_dispatch(&reg, &mut editor, "bwipeout!"),
            Some(ExEffect::BufferDelete {
                force: true,
                wipe: true
            })
        );
    }

    // `:r` resolves to `:read` (min_prefix=1); `:re` also resolves to `:read`
    // since `:redo` requires min_prefix=3.
    #[test]
    fn dispatch_r_resolves_to_read_not_redo() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        // `:r foo` → ReadFile, confirming `:r` means `:read` not `:redo`.
        assert_eq!(
            try_dispatch(&reg, &mut editor, "r foo"),
            Some(ExEffect::ReadFile { path: "foo".into() })
        );
    }

    // ---- Phase 2c: registers -----------------------------------------------

    #[test]
    fn dispatch_reg_returns_info_registers() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        let result = try_dispatch(&reg, &mut editor, "reg");
        match result {
            Some(ExEffect::Info(s)) => assert!(s.starts_with("--- Registers ---"), "got: {s}"),
            other => panic!("expected Some(Info(_)), got {other:?}"),
        }
    }

    #[test]
    fn dispatch_registers_returns_info_registers() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        let result = try_dispatch(&reg, &mut editor, "registers");
        match result {
            Some(ExEffect::Info(s)) => assert!(s.starts_with("--- Registers ---"), "got: {s}"),
            other => panic!("expected Some(Info(_)), got {other:?}"),
        }
    }

    // ---- Phase 2c: marks ---------------------------------------------------

    #[test]
    fn dispatch_marks_returns_info_marks() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        let result = try_dispatch(&reg, &mut editor, "marks");
        match result {
            Some(ExEffect::Info(s)) => assert!(s.starts_with("--- Marks ---"), "got: {s}"),
            other => panic!("expected Some(Info(_)), got {other:?}"),
        }
    }

    // ---- Phase 2c: jumps ---------------------------------------------------

    #[test]
    fn dispatch_jumps_returns_info_jumps_empty() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        let result = try_dispatch(&reg, &mut editor, "jumps");
        match result {
            Some(ExEffect::Info(s)) => assert!(s.starts_with("(no jumps"), "got: {s}"),
            other => panic!("expected Some(Info(_)), got {other:?}"),
        }
    }

    // ---- Phase 2c: changes -------------------------------------------------

    #[test]
    fn dispatch_changes_returns_info_changes_empty() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        let result = try_dispatch(&reg, &mut editor, "changes");
        match result {
            Some(ExEffect::Info(s)) => assert!(s.starts_with("(no changes"), "got: {s}"),
            other => panic!("expected Some(Info(_)), got {other:?}"),
        }
    }

    // ---- Phase 2c: prefix gating (marks) -----------------------------------

    #[test]
    fn dispatch_m_returns_none_below_min_prefix() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        // `:m` — below min_prefix=5 for marks; no other registered command starts with "m"
        assert_eq!(try_dispatch(&reg, &mut editor, "m"), None);
    }

    #[test]
    fn dispatch_mark_returns_none_below_min_prefix() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        // `:mark` is 4 chars, min_prefix=5 → no match
        assert_eq!(try_dispatch(&reg, &mut editor, "mark"), None);
    }

    #[test]
    fn dispatch_marks_full_name_returns_some() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert!(try_dispatch(&reg, &mut editor, "marks").is_some());
    }

    // ---- Phase 2c: prefix gating (registers) -------------------------------

    // `:r` resolves to `:read` (existing), `:re` resolves to `:read` (no-args → None).
    // `:reg` is an alias for `:registers` → Info.
    #[test]
    fn dispatch_reg_via_alias_returns_info() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert!(matches!(
            try_dispatch(&reg, &mut editor, "reg"),
            Some(ExEffect::Info(_))
        ));
    }

    #[test]
    fn dispatch_re_still_resolves_to_read_no_args() {
        // `:re` — resolves to `:read` (min_prefix=1), no path → None
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert_eq!(try_dispatch(&reg, &mut editor, "re"), None);
    }

    // ---- Phase 2d: bare line number / bare range ---------------------------

    #[test]
    fn dispatch_bare_number_jumps_to_line() {
        // `:5` on a 5-line buffer → cursor row 4 (0-based).
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["a", "b", "c", "d", "e"]);
        let result = try_dispatch(&reg, &mut editor, "5");
        assert_eq!(result, Some(ExEffect::Ok));
        assert_eq!(editor.cursor().0, 4);
    }

    #[test]
    fn dispatch_bare_range_jumps_to_range_start() {
        // `:1,5` → jump to line 1 (cursor row 0).
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["a", "b", "c", "d", "e"]);
        let result = try_dispatch(&reg, &mut editor, "1,5");
        assert_eq!(result, Some(ExEffect::Ok));
        assert_eq!(editor.cursor().0, 0);
    }

    // ---- Phase 2d: :delete -------------------------------------------------

    #[test]
    fn dispatch_d_no_range_deletes_cursor_line() {
        // `:d` with cursor on line 1 (row 0) → removes first line.
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["first", "second", "third"]);
        let result = try_dispatch(&reg, &mut editor, "d");
        assert_eq!(result, Some(ExEffect::Ok));
        // "first" gone; remaining lines start with "second".
        assert_eq!(editor.buffer().lines()[0], "second");
        assert_eq!(editor.buffer().lines().len(), 2);
    }

    #[test]
    fn dispatch_1d_deletes_line_1() {
        // `:1d` → deletes line 1 from a 3-line buffer.
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["first", "second", "third"]);
        let result = try_dispatch(&reg, &mut editor, "1d");
        assert_eq!(result, Some(ExEffect::Ok));
        assert_eq!(editor.buffer().lines()[0], "second");
        assert_eq!(editor.buffer().lines().len(), 2);
    }

    #[test]
    fn dispatch_1_2d_deletes_lines_1_and_2() {
        // `:1,2d` → removes first two lines.
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["first", "second", "third"]);
        let result = try_dispatch(&reg, &mut editor, "1,2d");
        assert_eq!(result, Some(ExEffect::Ok));
        assert_eq!(editor.buffer().lines()[0], "third");
        assert_eq!(editor.buffer().lines().len(), 1);
    }

    // ---- Phase 2d: :sort ---------------------------------------------------

    #[test]
    fn dispatch_sort_sorts_whole_buffer() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["banana", "apple", "cherry"]);
        let result = try_dispatch(&reg, &mut editor, "sort");
        assert_eq!(result, Some(ExEffect::Ok));
        let lines = editor.buffer().lines().to_vec();
        assert_eq!(lines, vec!["apple", "banana", "cherry"]);
    }

    #[test]
    fn dispatch_1_3sort_sorts_range_only() {
        // `:1,3sort` on 5-line buffer sorts lines 1–3, leaves 4–5 intact.
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["cherry", "apple", "banana", "zebra", "mango"]);
        let result = try_dispatch(&reg, &mut editor, "1,3sort");
        assert_eq!(result, Some(ExEffect::Ok));
        let lines = editor.buffer().lines().to_vec();
        assert_eq!(lines[0], "apple");
        assert_eq!(lines[1], "banana");
        assert_eq!(lines[2], "cherry");
        assert_eq!(lines[3], "zebra");
        assert_eq!(lines[4], "mango");
    }

    // ---- Phase 2e: :substitute (:s) ----------------------------------------

    #[test]
    fn substitute_single_occurrence_on_cursor_line() {
        // `:s/foo/bar/` — replace first `foo` on current line (row 0).
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["foo"]);
        let result = try_dispatch(&reg, &mut editor, "s/foo/bar/");
        assert_eq!(
            result,
            Some(ExEffect::Substituted {
                count: 1,
                lines_changed: 1
            })
        );
        assert_eq!(editor.buffer().lines()[0], "bar");
    }

    #[test]
    fn substitute_global_flag_replaces_all_occurrences() {
        // `:s/foo/bar/g` — replace every `foo` on current line.
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["foo foo foo"]);
        let result = try_dispatch(&reg, &mut editor, "s/foo/bar/g");
        assert_eq!(
            result,
            Some(ExEffect::Substituted {
                count: 3,
                lines_changed: 1
            })
        );
        assert_eq!(editor.buffer().lines()[0], "bar bar bar");
    }

    #[test]
    fn substitute_percent_range_applies_to_all_lines() {
        // `:%s/foo/bar/g` — whole buffer.
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["foo", "foo bar", "baz"]);
        let result = try_dispatch(&reg, &mut editor, "%s/foo/bar/g");
        assert_eq!(
            result,
            Some(ExEffect::Substituted {
                count: 2,
                lines_changed: 2
            })
        );
        assert_eq!(editor.buffer().lines()[0], "bar");
        assert_eq!(editor.buffer().lines()[1], "bar bar");
        assert_eq!(editor.buffer().lines()[2], "baz");
    }

    #[test]
    fn substitute_explicit_range_applied_correctly() {
        // `:1,2s/x/y/` — only lines 1–2 (0-based 0–1); line 3 unchanged.
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["x", "x", "x"]);
        let result = try_dispatch(&reg, &mut editor, "1,2s/x/y/");
        assert_eq!(
            result,
            Some(ExEffect::Substituted {
                count: 2,
                lines_changed: 2
            })
        );
        assert_eq!(editor.buffer().lines()[0], "y");
        assert_eq!(editor.buffer().lines()[1], "y");
        assert_eq!(editor.buffer().lines()[2], "x"); // untouched
    }

    #[test]
    fn substitute_bad_regex_returns_error() {
        // `:s/[bad/` — engine parse_substitute should fail (unclosed `[`).
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["foo"]);
        let result = try_dispatch(&reg, &mut editor, "s/[bad/foo/");
        assert!(
            matches!(result, Some(ExEffect::Error(_))),
            "expected Some(Error(_)), got {result:?}"
        );
    }

    #[test]
    fn substitute_no_body_returns_error() {
        // `:s` with no args — parse_substitute("") fails (no leading `/`).
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["foo"]);
        let result = try_dispatch(&reg, &mut editor, "s");
        assert!(
            matches!(result, Some(ExEffect::Error(_))),
            "expected Some(Error(_)), got {result:?}"
        );
    }

    #[test]
    fn substitute_empty_pattern_no_prior_search_returns_error() {
        // `:s//bar/` — empty pattern with no last_search → engine error.
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["foo"]);
        let result = try_dispatch(&reg, &mut editor, "s//bar/");
        assert!(
            matches!(result, Some(ExEffect::Error(_))),
            "expected Some(Error(_)), got {result:?}"
        );
    }

    // ---- Phase 3: :set -------------------------------------------------------

    #[test]
    fn dispatch_set_bare_returns_info() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        let result = try_dispatch(&reg, &mut editor, "set");
        assert!(
            matches!(result, Some(ExEffect::Info(_))),
            "expected Some(Info(_)), got {result:?}"
        );
    }

    #[test]
    fn dispatch_se_prefix_returns_info() {
        // `:se` — min_prefix=2 so resolves to `:set`.
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        let result = try_dispatch(&reg, &mut editor, "se");
        assert!(
            matches!(result, Some(ExEffect::Info(_))),
            "expected Some(Info(_)), got {result:?}"
        );
    }

    #[test]
    fn dispatch_set_number_enables_number() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        let result = try_dispatch(&reg, &mut editor, "set number");
        assert_eq!(result, Some(ExEffect::Ok));
        assert!(editor.settings().number);
    }

    #[test]
    fn dispatch_set_nonumber_disables_number() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        editor.settings_mut().number = true;
        let result = try_dispatch(&reg, &mut editor, "set nonumber");
        assert_eq!(result, Some(ExEffect::Ok));
        assert!(!editor.settings().number);
    }

    #[test]
    fn dispatch_set_tabstop_eq_4() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        let result = try_dispatch(&reg, &mut editor, "set tabstop=4");
        assert_eq!(result, Some(ExEffect::Ok));
        assert_eq!(editor.settings().tabstop, 4);
    }

    // ---- try_dispatch_host tests -------------------------------------------

    struct TestCtx {
        counter: i32,
    }

    struct PingCmd;
    impl HostCmd<TestCtx> for PingCmd {
        fn name(&self) -> &'static str {
            "ping"
        }
        fn aliases(&self) -> &'static [&'static str] {
            &["pn"]
        }
        fn min_prefix(&self) -> usize {
            2
        }
        fn run(&self, ctx: &mut TestCtx, _args: &str) -> Option<ExEffect> {
            ctx.counter += 1;
            Some(ExEffect::Ok)
        }
    }

    struct EchoCmd;
    impl HostCmd<TestCtx> for EchoCmd {
        fn name(&self) -> &'static str {
            "echo"
        }
        fn min_prefix(&self) -> usize {
            4
        }
        fn run(&self, _ctx: &mut TestCtx, args: &str) -> Option<ExEffect> {
            if args.is_empty() {
                None
            } else {
                Some(ExEffect::Info(args.to_string()))
            }
        }
    }

    fn make_host_registry() -> HostRegistry<TestCtx> {
        let mut reg = HostRegistry::new();
        reg.add(Box::new(PingCmd));
        reg.add(Box::new(EchoCmd));
        reg
    }

    #[test]
    fn try_dispatch_host_claims_exact_name() {
        let reg = make_host_registry();
        let mut ctx = TestCtx { counter: 0 };
        let result = try_dispatch_host(&reg, &mut ctx, "ping");
        assert_eq!(result, Some(ExEffect::Ok));
        assert_eq!(ctx.counter, 1);
    }

    #[test]
    fn try_dispatch_host_claims_alias() {
        let reg = make_host_registry();
        let mut ctx = TestCtx { counter: 0 };
        let result = try_dispatch_host(&reg, &mut ctx, "pn");
        assert_eq!(result, Some(ExEffect::Ok));
        assert_eq!(ctx.counter, 1);
    }

    #[test]
    fn try_dispatch_host_claims_prefix() {
        let reg = make_host_registry();
        let mut ctx = TestCtx { counter: 0 };
        // "pi" meets min_prefix=2 for "ping"
        let result = try_dispatch_host(&reg, &mut ctx, "pi");
        assert_eq!(result, Some(ExEffect::Ok));
    }

    #[test]
    fn try_dispatch_host_returns_none_on_miss() {
        let reg = make_host_registry();
        let mut ctx = TestCtx { counter: 0 };
        let result = try_dispatch_host(&reg, &mut ctx, "unknown");
        assert!(result.is_none());
        assert_eq!(ctx.counter, 0);
    }

    #[test]
    fn try_dispatch_host_returns_none_on_empty_input() {
        let reg = make_host_registry();
        let mut ctx = TestCtx { counter: 0 };
        assert!(try_dispatch_host(&reg, &mut ctx, "").is_none());
        assert!(try_dispatch_host(&reg, &mut ctx, "   ").is_none());
    }

    #[test]
    fn try_dispatch_host_passes_args() {
        let reg = make_host_registry();
        let mut ctx = TestCtx { counter: 0 };
        let result = try_dispatch_host(&reg, &mut ctx, "echo hello world");
        assert_eq!(result, Some(ExEffect::Info("hello world".to_string())));
    }

    #[test]
    fn try_dispatch_host_defers_when_command_returns_none() {
        let reg = make_host_registry();
        let mut ctx = TestCtx { counter: 0 };
        // echo with no args returns None (defers)
        let result = try_dispatch_host(&reg, &mut ctx, "echo");
        assert!(result.is_none());
    }

    // ---- Phase 5a: collect_registry_names + completion integration -----------

    fn noop_handler(
        _editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, DefaultHost>,
        _args: &str,
        _range: Option<crate::range::LineRange>,
    ) -> Option<ExEffect> {
        Some(ExEffect::Ok)
    }

    #[test]
    fn collect_registry_names_includes_aliases() {
        let mut reg = crate::Registry::<DefaultHost>::new();
        reg.add(crate::ExCommand {
            name: "test",
            aliases: &["t1", "t2"],
            arg_kind: crate::ArgKind::None,
            min_prefix: 1,
            run: noop_handler,
        });
        let names = collect_registry_names(&reg);
        assert!(names.contains(&"test".to_string()));
        assert!(names.contains(&"t1".to_string()));
        assert!(names.contains(&"t2".to_string()));
    }

    #[test]
    fn default_registry_includes_quit_and_q_bang() {
        let reg = default_registry::<DefaultHost>();
        let names = collect_registry_names(&reg);
        assert!(
            names.contains(&"quit".to_string()),
            "missing 'quit': {names:?}"
        );
        assert!(names.contains(&"q!".to_string()), "missing 'q!': {names:?}");
    }

    #[test]
    fn complete_through_default_registry() {
        let reg = default_registry::<DefaultHost>();
        let names = collect_registry_names(&reg);
        let result = complete_command_from_names("qu", 2, &names);
        assert_eq!(result.kind, CompletionKind::Command);
        assert!(
            result.candidates.contains(&"quit".to_string()),
            "missing 'quit': {:?}",
            result.candidates
        );
        assert!(
            result.candidates.contains(&"quit!".to_string()),
            "missing 'quit!': {:?}",
            result.candidates
        );
    }
}
