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
    ArgSources, CommandCandidate, CompletionKind, Completions, arg_kind_usage,
    collect_host_registry_names, collect_registry_names, complete, complete_arg,
    complete_command_from_names, complete_command_meta, first_word_end, longest_common_prefix,
};
pub use effect::{ExEffect, QfCommand};
pub use expand::{ExpandContext, expand_args, expand_filename};
pub use range::{LineRange, parse_range};
pub use registry::{ArgKind, ExCommand, HostCmd, HostRegistry, Registry};

mod builtins;
mod complete;
mod effect;
pub mod expand;
mod folds;
mod global;
mod listings;
mod parse;
mod range;
mod registry;
mod setopt;
mod shell;

pub use setopt::{all_setting_names, apply_set_token, query_option_value};

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

    // Phase 8a: search-as-address `:/pat` / `:?pat`.
    // Must be checked before parse_range because `/` and `?` are not valid
    // range chars and parse_range would return None range + the original input.
    if input.starts_with('/') || input.starts_with('?') {
        return Some(handle_search_address(editor, input));
    }

    // Parse a leading range (`5`, `5,10`, `.,$`, `%`, `'a,'b`).
    let (range, cmd_str) = match parse_range(input, editor) {
        Ok(pair) => pair,
        Err(e) => return Some(ExEffect::Error(e)),
    };

    // Phase 8a: `:[range]!cmd` shell filter — special-case before split_name_args
    // because `!` is not alphabetic and split_name_args would return ("", input).
    if let Some(rest) = cmd_str.strip_prefix('!') {
        let shell_cmd = rest.trim();
        return Some(shell::shell_filter_handler(editor, shell_cmd, range));
    }

    // `:&` / `:&&` (repeat last substitute) — special-case because `&` is not
    // alphabetic and split_name_args cannot parse it as a command name.
    // `:&&` keeps the original flags; `:&` drops them (vim semantics).
    // Must check `&&` before `&` so the double-ampersand form is matched first.
    if cmd_str == "&&" || cmd_str.starts_with("&& ") {
        return Some(builtins::repeat_substitute_handler(editor, true, range));
    }
    if cmd_str == "&" || cmd_str.starts_with("& ") {
        return Some(builtins::repeat_substitute_handler(editor, false, range));
    }

    // `:[range]>` / `:[range]<` shift commands — special-cased because `>` / `<`
    // are not alphabetic and split_name_args cannot parse them as command names.
    if cmd_str.starts_with('>') || cmd_str.starts_with('<') {
        return Some(builtins::shift_handler(editor, cmd_str, range));
    }

    let (name, args) = parse::split_name_args(cmd_str);
    if name.is_empty() {
        // Bare `:N` or bare range — jump to line.
        return handle_bare_line_number(editor, cmd_str, range);
    }
    let cmd = reg.resolve(name)?;
    // Handler may return None to defer this invocation to the legacy path.
    (cmd.run)(editor, args, range)
}

/// Handle `:/pat` / `:?pat` — search-as-address.
///
/// Jumps the cursor forward (`/`) or backward (`?`) to the next line
/// matching `pat`. Empty pattern reuses `editor.last_search()`.
/// Ported from `hjkl_editor::ex::run` lines 149–181.
fn handle_search_address<H: hjkl_engine::Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    input: &str,
) -> ExEffect {
    let forward = input.starts_with('/');
    let delim = if forward { '/' } else { '?' };
    let body = &input[1..];
    let pat_str: String = match body.strip_suffix(delim).unwrap_or(body) {
        "" => match editor.last_search() {
            Some(p) if !p.is_empty() => p.to_string(),
            _ => return ExEffect::Error("no previous search pattern".into()),
        },
        s => s.to_string(),
    };
    let s = editor.settings();
    use hjkl_engine::search::{CaseMode, resolve_case_mode};
    let base = CaseMode::from_options(s.ignore_case, s.smartcase);
    let (stripped, mode) = resolve_case_mode(&pat_str, base);
    let compile_src = if mode == CaseMode::Insensitive {
        format!("(?i){stripped}")
    } else {
        stripped
    };
    match regex::Regex::new(&compile_src) {
        Ok(re) => {
            editor.set_search_pattern(Some(re));
            if forward {
                editor.search_advance_forward(false);
            } else {
                editor.search_advance_backward(true);
            }
            editor.ensure_cursor_in_scrolloff();
            editor.set_last_search(Some(pat_str), forward);
            ExEffect::Ok
        }
        Err(e) => ExEffect::Error(format!("bad search pattern: {e}")),
    }
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
        hjkl_vim::vim_editor(buf, host, Options::default())
    }

    fn make_editor_with_lines(lines: &[&str]) -> Editor<hjkl_buffer::Buffer, DefaultHost> {
        let content = lines.join("\n");
        let buf = hjkl_buffer::Buffer::from_str(&content);
        let host = DefaultHost::new();
        hjkl_vim::vim_editor(buf, host, Options::default())
    }

    fn buf_line(editor: &Editor<hjkl_buffer::Buffer, DefaultHost>, row: usize) -> String {
        hjkl_buffer::rope_line_str(&editor.buffer().rope(), row)
    }

    fn buf_lines(editor: &Editor<hjkl_buffer::Buffer, DefaultHost>) -> Vec<String> {
        let rope = editor.buffer().rope();
        (0..rope.len_lines())
            .map(|i| hjkl_buffer::rope_line_str(&rope, i))
            .collect()
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

    // ---- quickfix (#184) ----------------------------------------------------

    #[test]
    fn dispatch_quickfix_commands() {
        use crate::QfCommand;
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        let go = |ed: &mut _, s: &str| try_dispatch(&reg, ed, s);
        assert_eq!(
            go(&mut editor, "copen"),
            Some(ExEffect::Quickfix(QfCommand::Open))
        );
        assert_eq!(
            go(&mut editor, "ccl"),
            Some(ExEffect::Quickfix(QfCommand::Close))
        );
        assert_eq!(
            go(&mut editor, "cn"),
            Some(ExEffect::Quickfix(QfCommand::Next))
        );
        assert_eq!(
            go(&mut editor, "cp"),
            Some(ExEffect::Quickfix(QfCommand::Prev))
        );
        assert_eq!(
            go(&mut editor, "cc 3"),
            Some(ExEffect::Quickfix(QfCommand::Nth(3)))
        );
        assert_eq!(
            go(&mut editor, "cc"),
            Some(ExEffect::Quickfix(QfCommand::Nth(0)))
        );
        assert_eq!(
            go(&mut editor, "grep TODO"),
            Some(ExEffect::Quickfix(QfCommand::Grep("TODO".into())))
        );
        // `:grep` with no pattern errors.
        assert!(matches!(go(&mut editor, "grep"), Some(ExEffect::Error(_))));
        // :cexpr / :cgetexpr / :caddexpr (#261)
        assert_eq!(
            go(&mut editor, r#"cexpr "1:2:msg""#),
            Some(ExEffect::Quickfix(QfCommand::Expr {
                text: r#""1:2:msg""#.into(),
                append: false,
                jump: true,
            }))
        );
        assert_eq!(
            go(&mut editor, r#"cgetexpr "1:2:msg""#),
            Some(ExEffect::Quickfix(QfCommand::Expr {
                text: r#""1:2:msg""#.into(),
                append: false,
                jump: false,
            }))
        );
        assert_eq!(
            go(&mut editor, r#"caddexpr "1:2:msg""#),
            Some(ExEffect::Quickfix(QfCommand::Expr {
                text: r#""1:2:msg""#.into(),
                append: true,
                jump: false,
            }))
        );
        // :cbuffer / :cgetbuffer / :caddbuffer (#261)
        assert_eq!(
            go(&mut editor, "cbuffer"),
            Some(ExEffect::Quickfix(QfCommand::FromBuffer {
                append: false,
                jump: true,
            }))
        );
        assert_eq!(
            go(&mut editor, "cgetbuffer"),
            Some(ExEffect::Quickfix(QfCommand::FromBuffer {
                append: false,
                jump: false,
            }))
        );
        assert_eq!(
            go(&mut editor, "caddbuffer"),
            Some(ExEffect::Quickfix(QfCommand::FromBuffer {
                append: true,
                jump: false,
            }))
        );
        // :cfile / :cgetfile / :caddfile (#261)
        assert_eq!(
            go(&mut editor, "cfile errors.log"),
            Some(ExEffect::Quickfix(QfCommand::FromFile {
                path: "errors.log".into(),
                append: false,
                jump: true,
            }))
        );
        assert_eq!(
            go(&mut editor, "cgetfile errors.log"),
            Some(ExEffect::Quickfix(QfCommand::FromFile {
                path: "errors.log".into(),
                append: false,
                jump: false,
            }))
        );
        assert_eq!(
            go(&mut editor, "caddfile errors.log"),
            Some(ExEffect::Quickfix(QfCommand::FromFile {
                path: "errors.log".into(),
                append: true,
                jump: false,
            }))
        );
        // empty path → kept as empty (app resolves to "errors.err")
        assert_eq!(
            go(&mut editor, "cfile"),
            Some(ExEffect::Quickfix(QfCommand::FromFile {
                path: String::new(),
                append: false,
                jump: true,
            }))
        );
        // :colder / :cnewer — list-stack navigation (#261 Phase 5b)
        assert_eq!(
            go(&mut editor, "colder"),
            Some(ExEffect::Quickfix(QfCommand::Older(1)))
        );
        assert_eq!(
            go(&mut editor, "col"),
            Some(ExEffect::Quickfix(QfCommand::Older(1)))
        );
        assert_eq!(
            go(&mut editor, "colder 3"),
            Some(ExEffect::Quickfix(QfCommand::Older(3)))
        );
        assert_eq!(
            go(&mut editor, "cnewer"),
            Some(ExEffect::Quickfix(QfCommand::Newer(1)))
        );
        assert_eq!(
            go(&mut editor, "cnew"),
            Some(ExEffect::Quickfix(QfCommand::Newer(1)))
        );
        assert_eq!(
            go(&mut editor, "cnewer 2"),
            Some(ExEffect::Quickfix(QfCommand::Newer(2)))
        );
        // :cn and :cne must still resolve to :cnext, NOT :cnewer
        assert_eq!(
            go(&mut editor, "cn"),
            Some(ExEffect::Quickfix(QfCommand::Next))
        );
        assert_eq!(
            go(&mut editor, "cne"),
            Some(ExEffect::Quickfix(QfCommand::Next))
        );
        // :cdo / :cfdo — per-entry and per-file iteration (#261 Phase 5b "A2")
        assert_eq!(
            go(&mut editor, "cdo s/^/X/"),
            Some(ExEffect::Quickfix(QfCommand::Do {
                cmd: "s/^/X/".into(),
                per_file: false,
            }))
        );
        assert_eq!(
            go(&mut editor, "cfdo s/^/X/"),
            Some(ExEffect::Quickfix(QfCommand::Do {
                cmd: "s/^/X/".into(),
                per_file: true,
            }))
        );
        // "cfd" is the 3-char min_prefix for cfdo; "cf" must still resolve to :cfirst.
        assert_eq!(
            go(&mut editor, "cf"),
            Some(ExEffect::Quickfix(QfCommand::First))
        );
        assert_eq!(
            go(&mut editor, "cfirst"),
            Some(ExEffect::Quickfix(QfCommand::First))
        );
        // "cfile" has min_prefix=4 "cfil"; "cfi" is still :cfirst (prefix match).
        assert_eq!(
            go(&mut editor, "cfi"),
            Some(ExEffect::Quickfix(QfCommand::First))
        );
    }

    #[test]
    fn dispatch_location_commands() {
        use crate::QfCommand;
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        let go = |ed: &mut _, s: &str| try_dispatch(&reg, ed, s);
        assert_eq!(
            go(&mut editor, "lopen"),
            Some(ExEffect::Location(QfCommand::Open))
        );
        assert_eq!(
            go(&mut editor, "lcl"),
            Some(ExEffect::Location(QfCommand::Close))
        );
        assert_eq!(
            go(&mut editor, "lne"),
            Some(ExEffect::Location(QfCommand::Next))
        );
        assert_eq!(
            go(&mut editor, "lp"),
            Some(ExEffect::Location(QfCommand::Prev))
        );
        assert_eq!(
            go(&mut editor, "ll 2"),
            Some(ExEffect::Location(QfCommand::Nth(2)))
        );
        assert_eq!(
            go(&mut editor, "lgrep FIXME"),
            Some(ExEffect::Location(QfCommand::Grep("FIXME".into())))
        );
        assert_eq!(
            go(&mut editor, "lmake"),
            Some(ExEffect::Location(QfCommand::Make(String::new())))
        );
        // `:lgrep` with no pattern errors.
        assert!(matches!(go(&mut editor, "lgrep"), Some(ExEffect::Error(_))));
        // :lexpr / :lgetexpr / :laddexpr (#261)
        assert_eq!(
            go(&mut editor, r#"lexpr "3:1:hit""#),
            Some(ExEffect::Location(QfCommand::Expr {
                text: r#""3:1:hit""#.into(),
                append: false,
                jump: true,
            }))
        );
        assert_eq!(
            go(&mut editor, r#"lgetexpr "3:1:hit""#),
            Some(ExEffect::Location(QfCommand::Expr {
                text: r#""3:1:hit""#.into(),
                append: false,
                jump: false,
            }))
        );
        assert_eq!(
            go(&mut editor, r#"laddexpr "3:1:hit""#),
            Some(ExEffect::Location(QfCommand::Expr {
                text: r#""3:1:hit""#.into(),
                append: true,
                jump: false,
            }))
        );
        // :lbuffer / :lgetbuffer / :laddbuffer (#261)
        assert_eq!(
            go(&mut editor, "lbuffer"),
            Some(ExEffect::Location(QfCommand::FromBuffer {
                append: false,
                jump: true,
            }))
        );
        assert_eq!(
            go(&mut editor, "lgetbuffer"),
            Some(ExEffect::Location(QfCommand::FromBuffer {
                append: false,
                jump: false,
            }))
        );
        assert_eq!(
            go(&mut editor, "laddbuffer"),
            Some(ExEffect::Location(QfCommand::FromBuffer {
                append: true,
                jump: false,
            }))
        );
        // :lfile / :lgetfile / :laddfile (#261)
        assert_eq!(
            go(&mut editor, "lfile errors.log"),
            Some(ExEffect::Location(QfCommand::FromFile {
                path: "errors.log".into(),
                append: false,
                jump: true,
            }))
        );
        assert_eq!(
            go(&mut editor, "lgetfile errors.log"),
            Some(ExEffect::Location(QfCommand::FromFile {
                path: "errors.log".into(),
                append: false,
                jump: false,
            }))
        );
        assert_eq!(
            go(&mut editor, "laddfile errors.log"),
            Some(ExEffect::Location(QfCommand::FromFile {
                path: "errors.log".into(),
                append: true,
                jump: false,
            }))
        );
        // :lolder / :lnewer — list-stack navigation (#261 Phase 5b)
        assert_eq!(
            go(&mut editor, "lolder"),
            Some(ExEffect::Location(QfCommand::Older(1)))
        );
        assert_eq!(
            go(&mut editor, "lol"),
            Some(ExEffect::Location(QfCommand::Older(1)))
        );
        assert_eq!(
            go(&mut editor, "lolder 2"),
            Some(ExEffect::Location(QfCommand::Older(2)))
        );
        assert_eq!(
            go(&mut editor, "lnewer"),
            Some(ExEffect::Location(QfCommand::Newer(1)))
        );
        assert_eq!(
            go(&mut editor, "lnew"),
            Some(ExEffect::Location(QfCommand::Newer(1)))
        );
        assert_eq!(
            go(&mut editor, "lnewer 3"),
            Some(ExEffect::Location(QfCommand::Newer(3)))
        );
        // :lne must still resolve to :lnext, NOT :lnewer
        assert_eq!(
            go(&mut editor, "lne"),
            Some(ExEffect::Location(QfCommand::Next))
        );
        // :ldo / :lfdo — per-entry and per-file iteration (#261 Phase 5b "A2")
        assert_eq!(
            go(&mut editor, "ldo s/^/X/"),
            Some(ExEffect::Location(QfCommand::Do {
                cmd: "s/^/X/".into(),
                per_file: false,
            }))
        );
        assert_eq!(
            go(&mut editor, "lfdo s/^/X/"),
            Some(ExEffect::Location(QfCommand::Do {
                cmd: "s/^/X/".into(),
                per_file: true,
            }))
        );
        // "lfi" must still resolve to :lfirst (min_prefix=3 "lfi"), not :lfdo (min=3 "lfd").
        assert_eq!(
            go(&mut editor, "lfi"),
            Some(ExEffect::Location(QfCommand::First))
        );
        assert_eq!(
            go(&mut editor, "lfirst"),
            Some(ExEffect::Location(QfCommand::First))
        );
    }

    // ---- :diagnostics / :ldiagnostics (#261 Phase 5b A3) -------------------

    #[test]
    fn dispatch_diagnostics_commands() {
        use crate::QfCommand;
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        let go = |ed: &mut _, s: &str| try_dispatch(&reg, ed, s);

        // `:diagnostics` (min_prefix=4 "diag") → Quickfix(Diagnostics)
        assert_eq!(
            go(&mut editor, "diag"),
            Some(ExEffect::Quickfix(QfCommand::Diagnostics))
        );
        assert_eq!(
            go(&mut editor, "diagnostics"),
            Some(ExEffect::Quickfix(QfCommand::Diagnostics))
        );

        // `:ldiagnostics` (min_prefix=5 "ldiag") → Location(Diagnostics)
        assert_eq!(
            go(&mut editor, "ldiag"),
            Some(ExEffect::Location(QfCommand::Diagnostics))
        );
        assert_eq!(
            go(&mut editor, "ldiagnostics"),
            Some(ExEffect::Location(QfCommand::Diagnostics))
        );

        // "dia" (3 chars) must NOT resolve to :diagnostics (min_prefix=4).
        // It will return None (no match) or Unknown.
        let dia_result = go(&mut editor, "dia");
        assert!(
            !matches!(dia_result, Some(ExEffect::Quickfix(QfCommand::Diagnostics))),
            "\"dia\" must not resolve to :diagnostics (min_prefix is 4)"
        );

        // "ldi" (3 chars) must NOT resolve to :ldiagnostics (min_prefix=5).
        let ldi_result = go(&mut editor, "ldi");
        assert!(
            !matches!(ldi_result, Some(ExEffect::Location(QfCommand::Diagnostics))),
            "\"ldi\" must not resolve to :ldiagnostics (min_prefix is 5)"
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

    // ---- Phase 2b → 8a: read (now fully handled in hjkl-ex) ------------------
    //
    // Phase 8a: `:r` / `:read` now inserts file content directly.
    // Old tests expected `ReadFile { path }` — updated to the new behavior
    // (Ok on success, Error when file doesn't exist).

    #[test]
    fn dispatch_r_with_path_inserts_content_phase8a() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "hello\n").unwrap();
        let path = tmp.path().to_string_lossy().to_string();
        let result = try_dispatch(&reg, &mut editor, &format!("r {path}"));
        assert_eq!(result, Some(ExEffect::Ok), "got: {result:?}");
        let lines = buf_lines(&editor);
        assert!(lines.contains(&"hello".to_string()), "lines: {lines:?}");
    }

    #[test]
    fn dispatch_read_with_path_inserts_content_phase8a() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "world\n").unwrap();
        let path = tmp.path().to_string_lossy().to_string();
        let result = try_dispatch(&reg, &mut editor, &format!("read {path}"));
        assert_eq!(result, Some(ExEffect::Ok), "got: {result:?}");
        let lines = buf_lines(&editor);
        assert!(lines.contains(&"world".to_string()), "lines: {lines:?}");
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
    // Phase 8a: read_handler now acts immediately; non-existent path → Error.
    #[test]
    fn dispatch_r_resolves_to_read_not_redo() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        // `:r foo` → Error (file doesn't exist), confirming `:r` means `:read` not `:redo`.
        let result = try_dispatch(&reg, &mut editor, "r /nonexistent_test_path");
        assert!(
            matches!(result, Some(ExEffect::Error(_))),
            ":r of nonexistent file should be Error, got: {result:?}"
        );
    }

    // ---- Phase 2c: registers -----------------------------------------------

    #[test]
    fn dispatch_reg_returns_info_titled_registers() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        let result = try_dispatch(&reg, &mut editor, "reg");
        match result {
            Some(ExEffect::InfoTitled { title, content }) => {
                assert_eq!(title, "registers");
                assert!(content.starts_with("--- Registers ---"), "got: {content}");
            }
            other => panic!("expected Some(InfoTitled), got {other:?}"),
        }
    }

    #[test]
    fn dispatch_registers_returns_info_titled_registers() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        let result = try_dispatch(&reg, &mut editor, "registers");
        match result {
            Some(ExEffect::InfoTitled { title, content }) => {
                assert_eq!(title, "registers");
                assert!(content.starts_with("--- Registers ---"), "got: {content}");
            }
            other => panic!("expected Some(InfoTitled), got {other:?}"),
        }
    }

    // ---- Phase 2c: marks ---------------------------------------------------

    #[test]
    fn dispatch_marks_returns_info_titled_marks() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        let result = try_dispatch(&reg, &mut editor, "marks");
        match result {
            Some(ExEffect::InfoTitled { title, content }) => {
                assert_eq!(title, "marks");
                assert!(content.starts_with("--- Marks ---"), "got: {content}");
            }
            other => panic!("expected Some(InfoTitled), got {other:?}"),
        }
    }

    // ---- Phase 2c: jumps ---------------------------------------------------

    #[test]
    fn dispatch_jumps_returns_info_titled_jumps_empty() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        let result = try_dispatch(&reg, &mut editor, "jumps");
        match result {
            Some(ExEffect::InfoTitled { title, content }) => {
                assert_eq!(title, "jumps");
                assert!(content.starts_with("(no jumps"), "got: {content}");
            }
            other => panic!("expected Some(InfoTitled), got {other:?}"),
        }
    }

    // ---- Phase 2c: changes -------------------------------------------------

    #[test]
    fn dispatch_changes_returns_info_titled_changes_empty() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        let result = try_dispatch(&reg, &mut editor, "changes");
        match result {
            Some(ExEffect::InfoTitled { title, content }) => {
                assert_eq!(title, "changes");
                assert!(content.starts_with("(no changes"), "got: {content}");
            }
            other => panic!("expected Some(InfoTitled), got {other:?}"),
        }
    }

    // ---- Phase 2c: prefix gating (marks) -----------------------------------

    #[test]
    fn dispatch_m_resolves_to_move() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        // `:m` is the `:move` alias; with no destination address it reports an
        // error (it no longer falls through to the marks prefix gate).
        assert!(matches!(
            try_dispatch(&reg, &mut editor, "m"),
            Some(ExEffect::Error(_))
        ));
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
    fn dispatch_reg_via_alias_returns_info_titled() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        assert!(matches!(
            try_dispatch(&reg, &mut editor, "reg"),
            Some(ExEffect::InfoTitled { .. })
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
        assert_eq!(buf_line(&editor, 0), "second");
        assert_eq!(editor.buffer().row_count(), 2);
    }

    #[test]
    fn dispatch_1d_deletes_line_1() {
        // `:1d` → deletes line 1 from a 3-line buffer.
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["first", "second", "third"]);
        let result = try_dispatch(&reg, &mut editor, "1d");
        assert_eq!(result, Some(ExEffect::Ok));
        assert_eq!(buf_line(&editor, 0), "second");
        assert_eq!(editor.buffer().row_count(), 2);
    }

    #[test]
    fn dispatch_1_2d_deletes_lines_1_and_2() {
        // `:1,2d` → removes first two lines.
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["first", "second", "third"]);
        let result = try_dispatch(&reg, &mut editor, "1,2d");
        assert_eq!(result, Some(ExEffect::Ok));
        assert_eq!(buf_line(&editor, 0), "third");
        assert_eq!(editor.buffer().row_count(), 1);
    }

    #[test]
    fn dispatch_move_range_to_its_own_end_is_noop_not_error() {
        // `:1,3m3` moves lines 1-3 to after line 3 — a legal no-op in vim
        // (E134 only fires for a destination strictly inside the range).
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["a", "b", "c", "d"]);
        let result = try_dispatch(&reg, &mut editor, "1,3m3");
        assert_eq!(result, Some(ExEffect::Ok), "`:1,3m3` must be accepted");
        assert_eq!(
            buf_lines(&editor),
            vec!["a", "b", "c", "d"],
            "must be a no-op"
        );
    }

    #[test]
    fn dispatch_move_range_into_itself_errors() {
        // `:1,3m2` — destination line 2 is inside the source range → E134.
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["a", "b", "c", "d"]);
        assert!(matches!(
            try_dispatch(&reg, &mut editor, "1,3m2"),
            Some(ExEffect::Error(_))
        ));
    }

    #[test]
    fn dispatch_move_range_to_top_reorders() {
        // `:2,3m0` moves lines 2-3 to the top.
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["a", "b", "c", "d"]);
        let result = try_dispatch(&reg, &mut editor, "2,3m0");
        assert_eq!(result, Some(ExEffect::Ok));
        assert_eq!(buf_lines(&editor), vec!["b", "c", "a", "d"]);
    }

    // ---- Phase 2d: :sort ---------------------------------------------------

    #[test]
    fn dispatch_sort_sorts_whole_buffer() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["banana", "apple", "cherry"]);
        let result = try_dispatch(&reg, &mut editor, "sort");
        assert_eq!(result, Some(ExEffect::Ok));
        let lines = buf_lines(&editor);
        assert_eq!(lines, vec!["apple", "banana", "cherry"]);
    }

    #[test]
    fn dispatch_1_3sort_sorts_range_only() {
        // `:1,3sort` on 5-line buffer sorts lines 1–3, leaves 4–5 intact.
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["cherry", "apple", "banana", "zebra", "mango"]);
        let result = try_dispatch(&reg, &mut editor, "1,3sort");
        assert_eq!(result, Some(ExEffect::Ok));
        let lines = buf_lines(&editor);
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
        assert_eq!(buf_line(&editor, 0), "bar");
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
        assert_eq!(buf_line(&editor, 0), "bar bar bar");
    }

    #[test]
    fn substitute_ampersand_flag_reuses_previous_flags() {
        // A first `:s/.../g` records the `g` flag (even with no match); a later
        // `:s/c/d/&` reuses it, so BOTH `c`s are replaced — without `&` only the
        // first would be.
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["c c"]);
        try_dispatch(&reg, &mut editor, "s/zzz/q/g");
        let result = try_dispatch(&reg, &mut editor, "s/c/d/&");
        assert_eq!(
            result,
            Some(ExEffect::Substituted {
                count: 2,
                lines_changed: 1
            })
        );
        assert_eq!(buf_line(&editor, 0), "d d");
    }

    #[test]
    fn substitute_print_flag_echoes_changed_line() {
        // `:s/foo/bar/p` echoes the changed line as an Info message.
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["foo baz"]);
        let result = try_dispatch(&reg, &mut editor, "s/foo/bar/p");
        assert_eq!(result, Some(ExEffect::Info("bar baz".into())));
        assert_eq!(buf_line(&editor, 0), "bar baz");
    }

    #[test]
    fn substitute_print_hash_flag_prefixes_line_number() {
        // `:2s/foo/bar/#` prints the 1-based line number before the line.
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["x", "foo"]);
        let result = try_dispatch(&reg, &mut editor, "2s/foo/bar/#");
        assert_eq!(result, Some(ExEffect::Info("2 bar".into())));
    }

    #[test]
    fn substitute_print_list_flag_marks_eol_and_tabs() {
        // `l` flag renders `:list`-style: tabs as `^I`, `$` at end of line.
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["foo"]);
        let result = try_dispatch(&reg, &mut editor, "s/foo/a\\tb/l");
        assert_eq!(result, Some(ExEffect::Info("a^Ib$".into())));
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
        assert_eq!(buf_line(&editor, 0), "bar");
        assert_eq!(buf_line(&editor, 1), "bar bar");
        assert_eq!(buf_line(&editor, 2), "baz");
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
        assert_eq!(buf_line(&editor, 0), "y");
        assert_eq!(buf_line(&editor, 1), "y");
        assert_eq!(buf_line(&editor, 2), "x"); // untouched
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

    // ---- Phase 8a: foldindent / foldsyntax -----------------------------------

    #[test]
    fn dispatch_foldindent_on_indented_buffer_returns_info() {
        let reg = default_registry::<DefaultHost>();
        let mut editor =
            make_editor_with_lines(&["fn foo() {", "    let x = 1;", "    let y = 2;", "}"]);
        let result = try_dispatch(&reg, &mut editor, "foldindent");
        match result {
            Some(ExEffect::Info(msg)) => {
                assert!(msg.contains("fold"), "got: {msg}");
            }
            other => panic!("expected Some(Info(_)), got {other:?}"),
        }
    }

    #[test]
    fn dispatch_foldi_prefix_resolves_to_foldindent() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["fn foo() {", "    x;", "}"]);
        // min_prefix=5: "foldi" is 5 chars → resolves
        let result = try_dispatch(&reg, &mut editor, "foldi");
        assert!(result.is_some());
    }

    #[test]
    fn dispatch_foldsyntax_no_ranges_returns_info() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["fn foo() {", "    bar();", "}"]);
        let result = try_dispatch(&reg, &mut editor, "foldsyntax");
        assert_eq!(
            result,
            Some(ExEffect::Info("no syntax block ranges available".into()))
        );
    }

    // ---- Phase 8a: :read (full impl) ----------------------------------------

    #[test]
    fn dispatch_r_with_path_inserts_content() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["line1", "line2"]);
        // Write a temp file.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "inserted\n").unwrap();
        let path = tmp.path().to_string_lossy().to_string();
        let result = try_dispatch(&reg, &mut editor, &format!("r {path}"));
        assert_eq!(result, Some(ExEffect::Ok), "got: {result:?}");
        let lines = buf_lines(&editor);
        assert!(lines.contains(&"inserted".to_string()), "lines: {lines:?}");
    }

    #[cfg(unix)]
    #[test]
    fn dispatch_r_shell_cmd_inserts_output() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["line1"]);
        let result = try_dispatch(&reg, &mut editor, "r !echo hello");
        assert_eq!(result, Some(ExEffect::Ok), "got: {result:?}");
        let lines = buf_lines(&editor);
        assert!(lines.contains(&"hello".to_string()), "lines: {lines:?}");
    }

    #[test]
    fn dispatch_r_missing_file_returns_error() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["line1"]);
        let result = try_dispatch(&reg, &mut editor, "r /nonexistent/path/xyz.txt");
        assert!(
            matches!(result, Some(ExEffect::Error(_))),
            "got: {result:?}"
        );
    }

    // ---- Phase 8a: :!cmd shell filter ----------------------------------------

    #[test]
    fn dispatch_shell_empty_cmd_returns_error() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["hello"]);
        let result = try_dispatch(&reg, &mut editor, "!");
        assert!(
            matches!(result, Some(ExEffect::Error(_))),
            "got: {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn dispatch_shell_no_range_returns_info() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["hello"]);
        let result = try_dispatch(&reg, &mut editor, "!echo hello");
        match result {
            Some(ExEffect::Info(msg)) => assert!(msg.contains("hello"), "got: {msg}"),
            other => panic!("expected Some(Info(_)), got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn dispatch_shell_range_filter() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["banana", "apple", "cherry"]);
        let result = try_dispatch(&reg, &mut editor, "1,3!sort");
        assert_eq!(result, Some(ExEffect::Ok), "got: {result:?}");
        let lines = buf_lines(&editor);
        assert_eq!(lines[0], "apple");
        assert_eq!(lines[1], "banana");
        assert_eq!(lines[2], "cherry");
    }

    // ---- Phase 8a: :global / :vglobal ----------------------------------------

    #[test]
    fn dispatch_g_d_deletes_matching_lines() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["foo", "bar", "foo"]);
        let result = try_dispatch(&reg, &mut editor, "g/foo/d");
        assert!(
            matches!(result, Some(ExEffect::Substituted { count: 2, .. })),
            "got: {result:?}"
        );
        let lines = buf_lines(&editor);
        assert!(!lines.contains(&"foo".to_string()), "lines: {lines:?}");
    }

    #[test]
    fn dispatch_v_d_deletes_non_matching_lines() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["foo", "bar", "baz"]);
        let result = try_dispatch(&reg, &mut editor, "v/foo/d");
        assert!(
            matches!(result, Some(ExEffect::Substituted { .. })),
            "got: {result:?}"
        );
        let lines = buf_lines(&editor);
        assert!(!lines.contains(&"bar".to_string()));
        assert!(!lines.contains(&"baz".to_string()));
    }

    #[test]
    fn dispatch_global_full_name_works() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["foo", "bar"]);
        let result = try_dispatch(&reg, &mut editor, "global/foo/d");
        assert!(matches!(result, Some(ExEffect::Substituted { .. })));
    }

    #[test]
    fn dispatch_vglobal_full_name_works() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["foo", "bar"]);
        let result = try_dispatch(&reg, &mut editor, "vglobal/foo/d");
        assert!(matches!(result, Some(ExEffect::Substituted { .. })));
    }

    // ---- Phase 8a: search-as-address -----------------------------------------

    #[test]
    fn dispatch_search_forward_jumps_to_line() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["apple", "banana", "cherry"]);
        let result = try_dispatch(&reg, &mut editor, "/banana");
        assert_eq!(result, Some(ExEffect::Ok), "got: {result:?}");
        assert_eq!(editor.cursor().0, 1, "cursor should be on row 1 (banana)");
    }

    #[test]
    fn dispatch_search_backward_jumps_to_line() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["apple", "banana", "cherry"]);
        // Move cursor to row 2 (cherry) first.
        editor.goto_line(3);
        let result = try_dispatch(&reg, &mut editor, "?apple");
        assert_eq!(result, Some(ExEffect::Ok), "got: {result:?}");
        assert_eq!(editor.cursor().0, 0, "cursor should be on row 0 (apple)");
    }

    #[test]
    fn dispatch_search_bad_pattern_returns_error() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["foo"]);
        let result = try_dispatch(&reg, &mut editor, "/[bad");
        assert!(
            matches!(result, Some(ExEffect::Error(_))),
            "got: {result:?}"
        );
    }

    #[test]
    fn dispatch_search_empty_no_prior_returns_error() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["foo"]);
        let result = try_dispatch(&reg, &mut editor, "/");
        assert!(
            matches!(result, Some(ExEffect::Error(_))),
            "got: {result:?}"
        );
    }

    // ---- :& / :&& repeat-last-substitute ------------------------------------

    #[test]
    fn dispatch_amp_no_prior_sub_returns_error() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["foo"]);
        let result = try_dispatch(&reg, &mut editor, "&");
        assert!(
            matches!(result, Some(ExEffect::Error(_))),
            "expected Error, got {result:?}"
        );
    }

    #[test]
    fn dispatch_amp_repeats_last_sub_on_current_line() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["foo", "foo"]);
        let r1 = try_dispatch(&reg, &mut editor, "s/foo/bar/");
        assert!(
            matches!(r1, Some(ExEffect::Substituted { count: 1, .. })),
            "got: {r1:?}"
        );
        assert_eq!(buf_line(&editor, 0), "bar");
        editor.goto_line(2);
        let r2 = try_dispatch(&reg, &mut editor, "&");
        assert!(
            matches!(r2, Some(ExEffect::Substituted { count: 1, .. })),
            "expected Substituted(1), got {r2:?}"
        );
        assert_eq!(buf_line(&editor, 1), "bar");
    }

    #[test]
    fn dispatch_amp_amp_keeps_global_flag() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["x x x", "x x x"]);
        try_dispatch(&reg, &mut editor, "s/x/y/g").unwrap();
        assert_eq!(buf_line(&editor, 0), "y y y");
        editor.goto_line(2);
        let result = try_dispatch(&reg, &mut editor, "&&");
        assert!(
            matches!(result, Some(ExEffect::Substituted { count: 3, .. })),
            "expected Substituted(3), got {result:?}"
        );
        assert_eq!(buf_line(&editor, 1), "y y y");
    }

    #[test]
    fn dispatch_amp_drops_global_flag() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["x x x", "x x x"]);
        try_dispatch(&reg, &mut editor, "s/x/y/g").unwrap();
        assert_eq!(buf_line(&editor, 0), "y y y");
        editor.goto_line(2);
        let result = try_dispatch(&reg, &mut editor, "&");
        assert!(
            matches!(result, Some(ExEffect::Substituted { count: 1, .. })),
            "expected Substituted(1) (first only), got {result:?}"
        );
        assert_eq!(buf_line(&editor, 1), "y x x");
    }

    #[test]
    fn dispatch_percent_amp_repeats_on_whole_buffer() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor_with_lines(&["foo", "foo", "bar"]);
        try_dispatch(&reg, &mut editor, "s/foo/baz/").unwrap();
        assert_eq!(buf_line(&editor, 0), "baz");
        let result = try_dispatch(&reg, &mut editor, "%&");
        assert!(
            matches!(result, Some(ExEffect::Substituted { count: 1, .. })),
            "expected Substituted(1), got {result:?}"
        );
        assert_eq!(buf_line(&editor, 1), "baz");
        assert_eq!(buf_line(&editor, 2), "bar");
    }
}
