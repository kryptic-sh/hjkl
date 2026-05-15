//! Ex-command registry and dispatch layer for the hjkl editor stack.
//!
//! Phase 1: provides an extensible [`Registry`] and a minimal set of
//! built-in commands (`:q`, `:q!`). Additional commands migrate in
//! subsequent phases.
//!
//! Phase 2a: adds range parsing infrastructure and migrates the no-arg /
//! no-range terminal commands (`:w`, `:wq`, `:x`, `:wa`, `:wqa`, `:noh`,
//! `:undo`, `:redo`, `:qall`, `:qall!`, `:wqall`, `:wqall!`).

pub use effect::ExEffect;
pub use range::{LineRange, parse_range};
pub use registry::{ArgKind, ExCommand, Registry};

mod builtins;
mod effect;
mod parse;
mod range;
mod registry;

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

    // Strip a leading range so command resolution works correctly for
    // range-prefixed inputs like `5,10w`. The range value is not threaded
    // to the handler yet (Phase 2d will do that).
    let cmd_str = match parse_range(input, editor) {
        Ok((_range, rest)) => rest,
        Err(e) => return Some(ExEffect::Error(e)),
    };

    let (name, args) = parse::split_name_args(cmd_str);
    if name.is_empty() {
        return None;
    }
    let cmd = reg.resolve(name)?;
    // Handler may return None to defer this invocation to the legacy path
    // (e.g. Phase 2a's `:w` claims the no-arg form but defers `:w <path>`).
    (cmd.run)(editor, args)
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
}
