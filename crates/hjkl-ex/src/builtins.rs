use crate::{
    effect::{ExEffect, QfCommand},
    range::LineRange,
    registry::{ArgKind, ExCommand, Registry},
};
use hjkl_engine::Host;
use hjkl_vim::VimEditorExt;

// ---- folds / global / shell are in their own modules -----------------------
use crate::folds::{apply_fold_indent, apply_fold_syntax};
use crate::global::{global_match_handler, vglobal_handler};

// ---- quit ------------------------------------------------------------------

fn quit_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Quit {
        force: false,
        save: false,
    })
}

fn quit_force_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Quit {
        force: true,
        save: false,
    })
}

// ---- write -----------------------------------------------------------------

/// `:w` / `:write` — save current buffer, or save to `<path>` when given.
fn write_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    let path = args.trim();
    if path.is_empty() {
        Some(ExEffect::Save)
    } else {
        Some(ExEffect::SaveAs(path.to_string()))
    }
}

// ---- edit ------------------------------------------------------------------

/// `:e [path]` / `:edit [path]` — open or reload a file.
/// Returns `None` (defer to legacy) when no path given — legacy handles
/// the reload-current-buffer case via app-side logic.
fn edit_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::EditFile {
        path: args.trim().to_string(),
        force: false,
    })
}

/// `:e! [path]` / `:edit! [path]` — open or force-reload a file.
fn edit_force_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::EditFile {
        path: args.trim().to_string(),
        force: true,
    })
}

// ---- read ------------------------------------------------------------------

/// `:r <path>` / `:read <path>` / `:r !cmd` — insert file or shell output
/// below the cursor row.
///
/// Replaces the Phase 2b stub that returned `ExEffect::ReadFile`. Now handles
/// the operation fully in hjkl-ex so the app no longer round-trips through
/// the legacy `ex::run("read {path}")` path.
///
/// Returns `None` when no path/cmd is given (vim errors on `:r` alone).
fn read_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    range: Option<LineRange>,
) -> Option<ExEffect> {
    use hjkl_buffer::{Edit, Position};

    let path = args.trim();
    if path.is_empty() {
        return None;
    }

    // `:r !cmd` — run `cmd` through `sh -c` and capture stdout.
    let content = if let Some(cmd) = path.strip_prefix('!') {
        let cmd = cmd.trim();
        if cmd.is_empty() {
            return Some(ExEffect::Error(":r ! needs a shell command".into()));
        }
        match std::process::Command::new("sh").arg("-c").arg(cmd).output() {
            Ok(out) if out.status.success() => match String::from_utf8(out.stdout) {
                Ok(s) => s,
                Err(_) => return Some(ExEffect::Error("command output was not UTF-8".into())),
            },
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let trimmed = stderr.trim();
                let label = if trimmed.is_empty() {
                    "no stderr".to_string()
                } else {
                    trimmed.to_string()
                };
                return Some(ExEffect::Error(format!(
                    "command exited {} ({label})",
                    out.status
                        .code()
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "?".into())
                )));
            }
            Err(e) => return Some(ExEffect::Error(format!("cannot run `{cmd}`: {e}"))),
        }
    } else {
        match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => return Some(ExEffect::Error(format!("cannot read `{path}`: {e}"))),
        }
    };

    // Vim's `:r` inserts after the current row (or range's last row if
    // specified); trailing newline in file is dropped (vim does the same).
    let trimmed = content.strip_suffix('\n').unwrap_or(&content);
    editor.push_undo();
    // Insert below range end if range given, else below cursor.
    let row = match range {
        Some(r) => r.end_one_based().saturating_sub(1),
        None => editor.cursor().0,
    };
    let line_chars = hjkl_buffer::rope_line_str(&editor.buffer().rope(), row)
        .chars()
        .count();
    let insert_text = format!("\n{trimmed}");
    editor.mutate_edit(Edit::InsertStr {
        at: Position::new(row, line_chars),
        text: insert_text,
    });
    // Cursor lands on the first inserted row at col 0.
    editor.jump_cursor(row + 1, 0);
    editor.mark_content_dirty();
    Some(ExEffect::Ok)
}

// ---- bdelete / bwipeout ----------------------------------------------------

/// `:bd` / `:bdelete` — close current buffer (no force).
fn bdelete_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::BufferDelete {
        force: false,
        wipe: false,
    })
}

/// `:bd!` / `:bdelete!` — close current buffer (force).
fn bdelete_force_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::BufferDelete {
        force: true,
        wipe: false,
    })
}

/// `:bw` / `:bwipeout` — wipe current buffer (no force).
fn bwipeout_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::BufferDelete {
        force: false,
        wipe: true,
    })
}

/// `:bw!` / `:bwipeout!` — wipe current buffer (force).
fn bwipeout_force_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::BufferDelete {
        force: true,
        wipe: true,
    })
}

/// `:wa` / `:wall` — write all modified buffers.
/// hjkl owns one buffer per Editor; behaviour parity with legacy: same as `:w`.
fn wall_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Save)
}

// ---- wq / x ----------------------------------------------------------------

fn wq_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Quit {
        force: false,
        save: true,
    })
}

fn wq_force_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Quit {
        force: true,
        save: true,
    })
}

// ---- wqall -----------------------------------------------------------------

fn wqall_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Quit {
        force: false,
        save: true,
    })
}

// ---- qall ------------------------------------------------------------------

fn qall_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Quit {
        force: false,
        save: false,
    })
}

fn qall_force_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Quit {
        force: true,
        save: false,
    })
}

// ---- nohlsearch ------------------------------------------------------------

fn nohlsearch_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    editor.set_search_pattern(None);
    Some(ExEffect::Ok)
}

// ---- undo / redo -----------------------------------------------------------

fn undo_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    editor.undo();
    Some(ExEffect::Ok)
}

fn redo_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    editor.redo();
    Some(ExEffect::Ok)
}

// ---- earlier / later (time-travel undo) ------------------------------------

/// Parsed form of a `:earlier` / `:later` argument.
enum EarlierLaterArg {
    Steps(usize),
    Duration(std::time::Duration),
}

/// Parse a `:earlier` / `:later` argument string.
///
/// Accepted forms:
/// - `""` or `"N"` (no suffix) → `Steps(N)`, default N=1 when empty.
/// - `"Ns"` → `Duration(N seconds)`
/// - `"Nm"` → `Duration(N*60 seconds)`
/// - `"Nh"` → `Duration(N*3600 seconds)`
///
/// Returns `Err(msg)` on a parse failure.
fn parse_earlier_later_arg(arg: &str) -> Result<EarlierLaterArg, String> {
    let arg = arg.trim();
    if arg.is_empty() {
        return Ok(EarlierLaterArg::Steps(1));
    }
    if let Some(rest) = arg.strip_suffix('s') {
        let n: u64 = rest
            .parse()
            .map_err(|_| format!("invalid count before 's': {rest:?}"))?;
        return Ok(EarlierLaterArg::Duration(std::time::Duration::from_secs(n)));
    }
    if let Some(rest) = arg.strip_suffix('m') {
        let n: u64 = rest
            .parse()
            .map_err(|_| format!("invalid count before 'm': {rest:?}"))?;
        return Ok(EarlierLaterArg::Duration(std::time::Duration::from_secs(
            n * 60,
        )));
    }
    if let Some(rest) = arg.strip_suffix('h') {
        let n: u64 = rest
            .parse()
            .map_err(|_| format!("invalid count before 'h': {rest:?}"))?;
        return Ok(EarlierLaterArg::Duration(std::time::Duration::from_secs(
            n * 3600,
        )));
    }
    let n: usize = arg
        .parse()
        .map_err(|_| format!("invalid argument: {arg:?}"))?;
    Ok(EarlierLaterArg::Steps(n))
}

fn earlier_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    match parse_earlier_later_arg(args) {
        Err(msg) => Some(ExEffect::Error(msg)),
        Ok(EarlierLaterArg::Steps(n)) => {
            let applied = editor.earlier_by_steps(n);
            let s = if applied == 1 { "" } else { "s" };
            Some(ExEffect::Info(format!("{applied} change{s} earlier")))
        }
        Ok(EarlierLaterArg::Duration(duration)) => {
            let target = std::time::SystemTime::now()
                .checked_sub(duration)
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            let applied = editor.earlier_by_time(target);
            let s = if applied == 1 { "" } else { "s" };
            Some(ExEffect::Info(format!("{applied} change{s} earlier")))
        }
    }
}

fn later_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    match parse_earlier_later_arg(args) {
        Err(msg) => Some(ExEffect::Error(msg)),
        Ok(EarlierLaterArg::Steps(n)) => {
            let applied = editor.later_by_steps(n);
            let s = if applied == 1 { "" } else { "s" };
            Some(ExEffect::Info(format!("{applied} change{s} later")))
        }
        Ok(EarlierLaterArg::Duration(duration)) => {
            // "later by duration" = advance from now toward the future.
            // The redo stack holds entries timestamped at their original edit
            // time; restore all entries whose timestamp ≤ (now + duration).
            let target = std::time::SystemTime::now()
                .checked_add(duration)
                .unwrap_or(std::time::SystemTime::now());
            let applied = editor.later_by_time(target);
            let s = if applied == 1 { "" } else { "s" };
            Some(ExEffect::Info(format!("{applied} change{s} later")))
        }
    }
}

// ---- saveas / file ---------------------------------------------------------

/// `:saveas {path}` / `:sav {path}` — write buffer to `path` AND rename the
/// buffer identity so future `:w` writes there.
fn saveas_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    let path = args.trim();
    if path.is_empty() {
        return Some(ExEffect::Error("E471: Argument required".into()));
    }
    Some(ExEffect::SaveAndRename {
        path: path.to_string(),
    })
}

/// `:file [{name}]` — no-arg: print filename + status; with-arg: rename
/// buffer in-memory without writing.
fn file_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    let name = args.trim();
    if name.is_empty() {
        // No arg: surface filename + readonly info. Dirty state lives in the
        // app's slot (not the engine) so only readonly is checked here.
        let filename = editor
            .with_registers(|r| r.read('%').map(|s| s.text.clone()))
            .unwrap_or_else(|| "[No Name]".into());
        let ro_flag = if editor.is_readonly() { " [RO]" } else { "" };
        Some(ExEffect::Info(format!("\"{filename}\"{ro_flag}")))
    } else {
        Some(ExEffect::RenameBuffer {
            name: name.to_string(),
        })
    }
}

// ---- cd / pwd --------------------------------------------------------------

/// `:cd [{path}]` — change working directory. No arg → `$HOME`.
fn cd_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    let raw = args.trim();
    let target = if raw.is_empty() {
        std::env::var("HOME").unwrap_or_else(|_| ".".to_string())
    } else {
        raw.to_string()
    };
    match std::env::set_current_dir(&target) {
        Ok(()) => {
            let new_cwd = std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or(target.clone());
            Some(ExEffect::Cwd(new_cwd))
        }
        Err(e) => Some(ExEffect::Error(format!("{target}: {e}"))),
    }
}

/// `:pwd` — print working directory.
fn pwd_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "?".to_string());
    Some(ExEffect::Info(cwd))
}

// ---- put -------------------------------------------------------------------

/// `:put [{reg}]` / `:put!` — paste a register's contents as a new line.
///
/// Without `!`: paste below the current line.
/// With `!`: paste above the current line.
/// Default register when no arg: `"` (unnamed).
fn put_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    let reg = args.trim().chars().next().unwrap_or('"');
    Some(ExEffect::PutRegister { reg, above: false })
}

fn put_above_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    let reg = args.trim().chars().next().unwrap_or('"');
    Some(ExEffect::PutRegister { reg, above: true })
}

// ---- registers / marks / jumps / changes -----------------------------------

fn registers_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::InfoTitled {
        title: "registers",
        content: crate::listings::format_registers(editor),
    })
}

fn marks_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::InfoTitled {
        title: "marks",
        content: crate::listings::format_marks(editor),
    })
}

fn jumps_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::InfoTitled {
        title: "jumps",
        content: crate::listings::format_jumps(editor),
    })
}

fn changes_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::InfoTitled {
        title: "changes",
        content: crate::listings::format_changes(editor),
    })
}

// ---- join --------------------------------------------------------------------

/// `:[range]j[oin][!] [count]` — join lines (default: current + next line).
///
/// Without `!`: delegates to the same per-row join primitive as normal-mode
/// `J` (`Editor::join_line`), so the single-space-insertion /
/// leading-whitespace-stripping behavior is byte-for-byte identical.
///
/// With `!` (registered separately as `join!` / `j!`): `gJ` semantics — no
/// separating space, no leading-whitespace strip — via `Edit::JoinLines`
/// directly (which already implements exactly that; it's `with_space=true`
/// that doesn't strip leading whitespace and so can't stand in for real `J`).
///
/// Range / count resolution mirrors `:d [count]` (verified against nvim
/// v0.12.4): no range + no count joins the current + next line; a range with
/// no count joins every line in the range; an explicit trailing `[count]`
/// OVERRIDES the join to start at the range's LAST line and join `count`
/// total lines from there.
fn join_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    range: Option<LineRange>,
) -> Option<ExEffect> {
    join_handler_inner(editor, args, range, false)
}

/// `:[range]j[oin]! [count]` — see [`join_handler`]; `raw = true` (gJ).
fn join_bang_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    range: Option<LineRange>,
) -> Option<ExEffect> {
    join_handler_inner(editor, args, range, true)
}

fn join_handler_inner<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    range: Option<LineRange>,
    raw: bool,
) -> Option<ExEffect> {
    let total = editor.buffer().row_count();
    if total == 0 {
        return Some(ExEffect::Ok);
    }

    // Resolve range to 0-based inclusive rows; default = current cursor line.
    let (start_row, end_row) = match range {
        Some(r) => {
            let s = r.start_one_based().saturating_sub(1);
            let e = (r.end_one_based().saturating_sub(1)).min(total.saturating_sub(1));
            (s, e)
        }
        None => {
            let row = editor.cursor().0;
            (row, row)
        }
    };
    if start_row > end_row {
        return Some(ExEffect::Ok);
    }

    // Optional trailing [count]: overrides the join to start at the range's
    // LAST line and join `count` total lines (same start-from-range-end
    // rule as `:d [count]`).
    let trimmed = args.trim();
    let (join_start, total_lines) = if trimmed.is_empty() {
        (start_row, (end_row - start_row + 1).max(2))
    } else {
        match trimmed.parse::<usize>() {
            Ok(n) if n > 0 => (end_row, n),
            _ => return Some(ExEffect::Error(format!("invalid count: {trimmed:?}"))),
        }
    };

    editor.jump_cursor(join_start, 0);
    if raw {
        use hjkl_buffer::Edit;
        editor.push_undo();
        editor.mutate_edit(Edit::JoinLines {
            row: join_start,
            count: total_lines.saturating_sub(1),
            with_space: false,
        });
        editor.mark_content_dirty();
    } else {
        use hjkl_vim::VimEditorExt;
        editor.join_line(total_lines);
    }

    // `:j` (ex command) lands the cursor on the first non-blank of the
    // joined line — unlike normal-mode `J`/`gJ`, which park it on the join
    // point. `join_start` is unaffected by the join itself (it's always the
    // TOP row of the merge), so re-derive the column here.
    let joined_row = join_start.min(editor.buffer().row_count().saturating_sub(1));
    let line = hjkl_buffer::rope_line_str(&editor.buffer().rope(), joined_row);
    let first_non_blank = line.chars().take_while(|c| *c == ' ' || *c == '\t').count();
    editor.jump_cursor(joined_row, first_non_blank);

    Some(ExEffect::Ok)
}

// ---- delete ----------------------------------------------------------------

/// `:[range]d` / `:[range]delete` — delete lines in range (default: cursor line).
///
/// `LineRange` is 1-based inclusive. Legacy `Range` (in hjkl-editor) is 0-based;
/// we convert here before mutating the buffer.
fn delete_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    range: Option<LineRange>,
) -> Option<ExEffect> {
    use hjkl_buffer::{Edit, MotionKind, Position};

    // No range → current line (1-based cursor row + 1).
    let r = range.unwrap_or_else(|| LineRange::single(editor.cursor().0 + 1));
    // Convert 1-based inclusive to 0-based inclusive row indices.
    let start_row = r.start_one_based().saturating_sub(1);
    let total = editor.buffer().row_count();
    if total == 0 {
        return Some(ExEffect::Ok);
    }
    let end_row = (r.end_one_based().saturating_sub(1)).min(total.saturating_sub(1));
    if start_row > end_row {
        return Some(ExEffect::Ok);
    }

    editor.push_undo();
    // Delete bottom-up so row indices stay valid as rows are removed.
    for row in (start_row..=end_row).rev() {
        if editor.buffer().row_count() == 1 {
            // Last remaining row: clear content rather than deleting the row.
            let line_chars = hjkl_buffer::rope_line_str(&editor.buffer().rope(), 0)
                .chars()
                .count();
            if line_chars > 0 {
                editor.mutate_edit(Edit::DeleteRange {
                    start: Position::new(0, 0),
                    end: Position::new(0, line_chars),
                    kind: MotionKind::Char,
                });
            }
            continue;
        }
        editor.mutate_edit(Edit::DeleteRange {
            start: Position::new(row, 0),
            end: Position::new(row, 0),
            kind: MotionKind::Line,
        });
    }
    editor.mark_content_dirty();
    Some(ExEffect::Ok)
}

// ---- sort ------------------------------------------------------------------

/// `:[range]sort[!iun]` — sort lines in range (default: whole buffer).
///
/// Flags (trailing args): `!` reverse, `i` ignore-case, `u` unique, `n` numeric.
fn sort_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    range: Option<LineRange>,
) -> Option<ExEffect> {
    let trimmed = args.trim();
    let mut reverse = false;
    let mut unique = false;
    let mut numeric = false;
    let mut ignore_case = false;
    for c in trimmed.chars() {
        match c {
            '!' => reverse = true,
            'u' => unique = true,
            'n' => numeric = true,
            'i' => ignore_case = true,
            ' ' | '\t' => {}
            other => return Some(ExEffect::Error(format!("bad :sort flag `{other}`"))),
        }
    }

    let rope = editor.buffer().rope();
    let mut all_lines: Vec<String> = (0..rope.len_lines())
        .map(|i| hjkl_buffer::rope_line_str(&rope, i))
        .collect();
    drop(rope);
    let total = all_lines.len();
    if total == 0 {
        return Some(ExEffect::Ok);
    }

    // Default range: whole buffer (0-based: 0..=total-1).
    let (start_row, end_row) = match range {
        Some(r) => {
            let s = r.start_one_based().saturating_sub(1);
            let e = (r.end_one_based().saturating_sub(1)).min(total - 1);
            (s, e)
        }
        None => (0, total - 1),
    };
    if start_row > end_row {
        return Some(ExEffect::Ok);
    }

    // Sort only the slice in range; keep the rest of the buffer intact.
    let mut slice: Vec<String> = all_lines[start_row..=end_row].to_vec();
    if numeric {
        slice.sort_by_key(|l| extract_leading_number(l));
    } else if ignore_case {
        slice.sort_by_key(|s| s.to_lowercase());
    } else {
        slice.sort();
    }
    if reverse {
        slice.reverse();
    }
    if unique {
        let cmp_key = |s: &str| -> String {
            if ignore_case {
                s.to_lowercase()
            } else {
                s.to_string()
            }
        };
        let mut seen = std::collections::HashSet::new();
        slice.retain(|line| seen.insert(cmp_key(line)));
    }
    // Splice the sorted slice back. `unique` may have shortened it.
    let after: Vec<String> = all_lines.split_off(end_row + 1);
    all_lines.truncate(start_row);
    all_lines.extend(slice);
    all_lines.extend(after);

    editor.push_undo();
    editor.restore(all_lines, (start_row, 0));
    editor.mark_content_dirty();
    Some(ExEffect::Ok)
}

/// `:[range]m {addr}` / `:move` — move the range to after line `addr`
/// (`0` = before the first line). Default range is the current line.
fn move_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    range: Option<LineRange>,
) -> Option<ExEffect> {
    line_relocate_handler(editor, args, range, false)
}

/// `:[range]t {addr}` / `:co` / `:copy` — copy the range to after line `addr`
/// (`0` = before the first line). Default range is the current line.
fn copy_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    range: Option<LineRange>,
) -> Option<ExEffect> {
    line_relocate_handler(editor, args, range, true)
}

/// Shared body for `:move` (`copy = false`) and `:copy` (`copy = true`).
fn line_relocate_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    range: Option<LineRange>,
    copy: bool,
) -> Option<ExEffect> {
    let dest = match crate::range::parse_dest_address(args, editor) {
        Ok(d) => d,
        Err(e) => return Some(ExEffect::Error(e)),
    };

    let rope = editor.buffer().rope();
    let mut all_lines: Vec<String> = (0..rope.len_lines())
        .map(|i| hjkl_buffer::rope_line_str(&rope, i))
        .collect();
    drop(rope);
    let content_rows = editor.buffer().row_count();
    if content_rows == 0 {
        return Some(ExEffect::Ok);
    }

    // Source range (1-based inclusive); default to the current line.
    let (s, e) = match range {
        Some(r) => (
            r.start_one_based().max(1),
            r.end_one_based().min(content_rows),
        ),
        None => {
            let cur = editor.cursor().0 + 1;
            (cur, cur)
        }
    };
    if s > e || e > all_lines.len() {
        return Some(ExEffect::Ok);
    }

    // vim (E134) rejects moving a range strictly *into* itself: the
    // destination line lies in `[s, e-1]`. `dest == e` ("move to after the
    // last source line") and `dest == s-1` ("move to before the first") are
    // legal no-ops in vim — e.g. `:1,3m3` — so only reject the inner range.
    if !copy && dest >= s && dest < e {
        return Some(ExEffect::Error("cannot move lines into themselves".into()));
    }

    let block: Vec<String> = all_lines[(s - 1)..=(e - 1)].to_vec();
    let removed = e - s + 1;

    let cursor_row = if copy {
        // Insert a clone after `dest` (0 = top).
        let insert_at = dest.min(all_lines.len());
        for (i, line) in block.iter().enumerate() {
            all_lines.insert(insert_at + i, line.clone());
        }
        insert_at + block.len() - 1
    } else {
        // Remove the source block, then insert after the (shift-adjusted) dest.
        all_lines.drain((s - 1)..=(e - 1));
        let insert_at = if dest == 0 {
            0
        } else if dest < s {
            dest
        } else {
            dest - removed
        };
        let insert_at = insert_at.min(all_lines.len());
        for (i, line) in block.iter().enumerate() {
            all_lines.insert(insert_at + i, line.clone());
        }
        insert_at + block.len() - 1
    };

    editor.push_undo();
    editor.restore(all_lines, (cursor_row, 0));
    editor.mark_content_dirty();
    Some(ExEffect::Ok)
}

/// Parse the first signed decimal integer from `line` for `:sort n`.
/// Lines with no leading number sort as `i64::MIN` (cluster at top, vim compat).
fn extract_leading_number(line: &str) -> i64 {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && !bytes[i].is_ascii_digit() && bytes[i] != b'-' {
        i += 1;
    }
    if i >= bytes.len() {
        return i64::MIN;
    }
    let mut j = i;
    if bytes[j] == b'-' {
        j += 1;
    }
    let start = j;
    while j < bytes.len() && bytes[j].is_ascii_digit() {
        j += 1;
    }
    if j == start {
        return i64::MIN;
    }
    line[i..j].parse().unwrap_or(i64::MIN)
}

// ---- substitute ------------------------------------------------------------

/// `:[range]s/pattern/replacement/[flags]` — substitute text in lines.
///
/// `args` arrives already stripped of the leading `s` command name, so it
/// begins with the delimiter (`/`) that `hjkl_engine::substitute::parse_substitute`
/// expects.  No-range → current cursor line; with range the engine receives a
/// 0-based inclusive `RangeInclusive<u32>`.
///
/// On success the parsed `SubstituteCmd` is stored on the editor so `:&` / `:&&`
/// can repeat it (part of #171).
///
/// # B17: bare `:s` (repeat last substitute)
///
/// When `args` (trimmed) is empty or doesn't start with `/`, vim treats this
/// as "repeat the last substitute": both the PATTERN and the REPLACEMENT are
/// reused from `editor.last_substitute()` (unlike `:s//rep/`, which reuses
/// only `last_search()`'s pattern). Flags are NOT reused — `:s` alone always
/// runs with default (no) flags; `:s g` / `:s 3` parse `args` as a bare
/// flags+count tail via `parse_flags` (verified against nvim v0.12.4).
fn substitute_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    range: Option<LineRange>,
) -> Option<ExEffect> {
    use hjkl_engine::substitute::{
        SubstituteCmd, apply_substitute, collect_substitute_matches, parse_flags, parse_substitute,
    };

    let trimmed = args.trim_start();
    let mut cmd = if trimmed.starts_with('/') {
        // args already starts with `/` (the delimiter); pass straight to engine.
        match parse_substitute(args) {
            Ok(c) => c,
            Err(e) => return Some(ExEffect::Error(e.to_string())),
        }
    } else {
        // Bare `:s [flags] [count]` — repeat the last substitute's pattern
        // AND replacement (`:h :s` — "If a pattern is not given ... the
        // pattern and replacement string ... are used from the last
        // substitute").
        let Some(prev) = editor.last_substitute() else {
            return Some(ExEffect::Error(
                "E33: No previous substitute regular expression".into(),
            ));
        };
        let (flags, count) = match parse_flags(trimmed) {
            Ok(fc) => fc,
            Err(e) => return Some(ExEffect::Error(e.to_string())),
        };
        SubstituteCmd {
            pattern: prev.pattern,
            replacement: prev.replacement,
            flags,
            count,
        }
    };

    // `&` flag: reuse the previous substitute's flags (`:h :s_flags`), unioned
    // with any flags typed alongside it. Resolved here because the parser has
    // no access to `last_substitute`.
    if cmd.flags.reuse_previous {
        let prev = editor.last_substitute().map(|c| c.flags);
        cmd.flags.reuse_previous = false;
        if let Some(pf) = prev {
            cmd.flags.all |= pf.all;
            cmd.flags.ignore_case |= pf.ignore_case;
            cmd.flags.case_sensitive |= pf.case_sensitive;
            cmd.flags.confirm |= pf.confirm;
            cmd.flags.report_only |= pf.report_only;
            cmd.flags.no_error |= pf.no_error;
            cmd.flags.print |= pf.print;
            cmd.flags.print_num |= pf.print_num;
            cmd.flags.print_list |= pf.print_list;
        }
    }

    // Resolve range to 0-based inclusive u32 bounds.
    // No range → current cursor line (cursor() returns 0-based (row, col)).
    let mut r = match range {
        Some(lr) => {
            let start = lr.start_one_based().saturating_sub(1) as u32;
            let end = lr.end_one_based().saturating_sub(1) as u32;
            start..=end
        }
        None => {
            let row = editor.cursor().0 as u32;
            row..=row
        }
    };

    // Trailing `[count]` (`:s/a/b/g 3`): operate on `count` lines starting at
    // the range's last line (vim semantics). apply_substitute clamps the end
    // to the buffer, so a huge count is harmless.
    if let Some(n) = cmd.count {
        let start = *r.end();
        let end = start.saturating_add(n as u32 - 1);
        r = start..=end;
    }

    // `/c` flag: collect matches without mutating buffer; let the host prompt.
    if cmd.flags.confirm {
        // Store so `:&` / `:&&` can repeat this substitution.
        editor.set_last_substitute(cmd.clone());
        return match collect_substitute_matches(editor, &cmd, r) {
            Ok(matches) => Some(ExEffect::SubstituteConfirm { matches }),
            Err(e) => Some(ExEffect::Error(e.to_string())),
        };
    }

    match apply_substitute(editor, &cmd, r) {
        Ok(out) => {
            let (print, print_num, print_list) =
                (cmd.flags.print, cmd.flags.print_num, cmd.flags.print_list);
            // Store so `:&` / `:&&` can repeat this substitution.
            editor.set_last_substitute(cmd);
            // `p` / `#` / `l` flags: echo the last changed line.
            if print && let Some(row) = out.last_row {
                let line = hjkl_buffer::rope_line_str(&editor.buffer().rope(), row);
                let msg =
                    format_print_line(line.trim_end_matches('\n'), row, print_num, print_list);
                return Some(ExEffect::Info(msg));
            }
            Some(ExEffect::Substituted {
                count: out.replacements,
                lines_changed: out.lines_changed,
            })
        }
        Err(e) => Some(ExEffect::Error(e.to_string())),
    }
}

/// Format a substitute `p` / `#` / `l` print line.
///
/// - `list` renders `:list`-style: tabs as `^I` and a trailing `$` marking EOL.
/// - `num` prefixes the 1-based line number.
fn format_print_line(line: &str, row: usize, num: bool, list: bool) -> String {
    let body = if list {
        let mut s = String::with_capacity(line.len() + 1);
        for ch in line.chars() {
            if ch == '\t' {
                s.push_str("^I");
            } else {
                s.push(ch);
            }
        }
        s.push('$');
        s
    } else {
        line.to_string()
    };
    if num {
        format!("{} {}", row + 1, body)
    } else {
        body
    }
}

/// `:&` / `:&&` / `:[range]&` / `:[range]&&` — repeat last substitute.
///
/// `:[range]>` / `:[range]<` — shift lines right / left by `shiftwidth`.
///
/// `cmd_str` is the command text after the range, beginning with one or more
/// `>` (or `<`) characters. The number of repeated shift chars is the number of
/// indent levels (`:>>` = 2 levels). An optional trailing numeric count is the
/// number of lines to operate on, starting at the *last* line of the range
/// (vim `:[range]> {count}` semantics). With no range the current line is used.
///
/// Cursor lands on the first non-blank of the last shifted line, matching vim.
pub(crate) fn shift_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    cmd_str: &str,
    range: Option<LineRange>,
) -> ExEffect {
    let shift_char = match cmd_str.chars().next() {
        Some(c @ ('>' | '<')) => c,
        _ => return ExEffect::Error("not a shift command".into()),
    };
    let levels = cmd_str.chars().take_while(|c| *c == shift_char).count();
    let rest = cmd_str[levels..].trim();

    let total = editor.buffer().row_count();
    if total == 0 {
        return ExEffect::Ok;
    }

    // Resolve the range (default: current line).
    let r = range.unwrap_or_else(|| LineRange::single(editor.cursor().0 + 1));
    let mut start_row = r.start_one_based().saturating_sub(1);
    let mut end_row = r.end_one_based().saturating_sub(1);

    // Optional trailing count: operate on `count` lines from the range's last
    // line (vim semantics). Anything else trailing is an error.
    if !rest.is_empty() {
        match rest.parse::<usize>() {
            Ok(0) | Err(_) => return ExEffect::Error("Trailing characters".into()),
            Ok(n) => {
                start_row = end_row;
                // Saturate: a huge count (e.g. `:2> 18446744073709551615`)
                // must clamp to the buffer end, not overflow and panic.
                end_row = start_row.saturating_add(n - 1);
            }
        }
    }

    end_row = end_row.min(total.saturating_sub(1));
    if start_row > end_row {
        return ExEffect::Ok;
    }

    let signed = if shift_char == '>' {
        levels as i32
    } else {
        -(levels as i32)
    };
    editor.indent_range((start_row, 0), (end_row, 0), signed, 0);

    // Cursor → first non-blank of the last shifted line (vim behaviour).
    let line = hjkl_buffer::rope_line_str(&editor.buffer().rope(), end_row);
    let first_non_blank = line.chars().take_while(|c| *c == ' ' || *c == '\t').count();
    editor.jump_cursor(end_row, first_non_blank);

    ExEffect::Ok
}

/// `:&`  — repeat with original flags dropped (pattern and replacement kept).
/// `:&&` — repeat with original flags preserved.
///
/// `keep_flags` is `true` for `&&`, `false` for `&`.
pub(crate) fn repeat_substitute_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    keep_flags: bool,
    range: Option<LineRange>,
) -> ExEffect {
    use hjkl_engine::substitute::{SubstFlags, apply_substitute};

    let cmd = match editor.last_substitute() {
        Some(c) => c,
        None => return ExEffect::Error("no previous substitute".into()),
    };

    // `:&` drops flags; `:&&` keeps them (vim semantics).
    let effective_cmd = if keep_flags {
        cmd
    } else {
        hjkl_engine::substitute::SubstituteCmd {
            flags: SubstFlags::default(),
            ..cmd
        }
    };

    // Resolve range; default to current line.
    let r = match range {
        Some(lr) => {
            let start = lr.start_one_based().saturating_sub(1) as u32;
            let end = lr.end_one_based().saturating_sub(1) as u32;
            start..=end
        }
        None => {
            let row = editor.cursor().0 as u32;
            row..=row
        }
    };

    match apply_substitute(editor, &effective_cmd, r) {
        Ok(out) => {
            // Keep last_substitute updated with what was actually run.
            editor.set_last_substitute(effective_cmd);
            ExEffect::Substituted {
                count: out.replacements,
                lines_changed: out.lines_changed,
            }
        }
        Err(e) => ExEffect::Error(e.to_string()),
    }
}

// ---- set -------------------------------------------------------------------

/// `:set [option ...]` — query / assign vim settings.
fn set_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(crate::setopt::apply_set(editor, args))
}

// ---- registration ----------------------------------------------------------

/// Register all Phase 1 + Phase 2a built-in commands.
pub(crate) fn register_builtins<H: Host>(reg: &mut Registry<H>) {
    // `:quit` / `:q`
    reg.add(ExCommand {
        name: "quit",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 1,
        run: quit_handler::<H>,
    });

    // `:quit!` / `:q!`
    reg.add(ExCommand {
        name: "quit!",
        aliases: &["q!"],
        arg_kind: ArgKind::None,
        min_prefix: 2,
        run: quit_force_handler::<H>,
    });

    // `:write` / `:w`  (min_prefix=1, but `:wa` resolves to `:wall` not `:write`)
    reg.add(ExCommand {
        name: "write",
        aliases: &[],
        arg_kind: ArgKind::Path,
        min_prefix: 1,
        run: write_handler::<H>,
    });

    // `:wall` / `:wa`
    reg.add(ExCommand {
        name: "wall",
        aliases: &["wa"],
        arg_kind: ArgKind::None,
        min_prefix: 2,
        run: wall_handler::<H>,
    });

    // `:wq`  (min_prefix=2 so `:w` still resolves to `:write`)
    reg.add(ExCommand {
        name: "wq",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 2,
        run: wq_handler::<H>,
    });

    // `:wq!`
    reg.add(ExCommand {
        name: "wq!",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 3,
        run: wq_force_handler::<H>,
    });

    // `:x`  (exact alias for wq)
    reg.add(ExCommand {
        name: "x",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 1,
        run: wq_handler::<H>,
    });

    // `:x!`
    reg.add(ExCommand {
        name: "x!",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 2,
        run: wq_force_handler::<H>,
    });

    // `:wqall` / `:wqa` — force=false (vim treats force as save errors, save=true)
    reg.add(ExCommand {
        name: "wqall",
        aliases: &["wqa"],
        arg_kind: ArgKind::None,
        min_prefix: 3,
        run: wqall_handler::<H>,
    });

    // `:wqall!` / `:wqa!`
    reg.add(ExCommand {
        name: "wqall!",
        aliases: &["wqa!"],
        arg_kind: ArgKind::None,
        min_prefix: 4,
        run: wqall_handler::<H>,
    });

    // `:xall` / `:xa` — alias for `:wqall` / `:wqa`
    reg.add(ExCommand {
        name: "xall",
        aliases: &["xa"],
        arg_kind: ArgKind::None,
        min_prefix: 2,
        run: wqall_handler::<H>,
    });

    // `:xall!` / `:xa!`
    reg.add(ExCommand {
        name: "xall!",
        aliases: &["xa!"],
        arg_kind: ArgKind::None,
        min_prefix: 3,
        run: wqall_handler::<H>,
    });

    // `:qall` / `:qa`
    reg.add(ExCommand {
        name: "qall",
        aliases: &["qa"],
        arg_kind: ArgKind::None,
        min_prefix: 2,
        run: qall_handler::<H>,
    });

    // `:qall!` / `:qa!`
    reg.add(ExCommand {
        name: "qall!",
        aliases: &["qa!"],
        arg_kind: ArgKind::None,
        min_prefix: 3,
        run: qall_force_handler::<H>,
    });

    // `:nohlsearch` / `:noh` / `:nohl` (min_prefix=3)
    reg.add(ExCommand {
        name: "nohlsearch",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 3,
        run: nohlsearch_handler::<H>,
    });

    // `:undo` / `:u`
    reg.add(ExCommand {
        name: "undo",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 1,
        run: undo_handler::<H>,
    });

    // `:redo` (min_prefix=3; `:r` resolves to `:read`, `:re` is ambiguous)
    reg.add(ExCommand {
        name: "redo",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 3,
        run: redo_handler::<H>,
    });

    // `:earlier` (min_prefix=2; vim compat: `:ea`)
    reg.add(ExCommand {
        name: "earlier",
        aliases: &[],
        arg_kind: ArgKind::Raw,
        min_prefix: 2,
        run: earlier_handler::<H>,
    });

    // `:later` (min_prefix=3; `:lat` — avoids collision with any prefix of `:last*`)
    reg.add(ExCommand {
        name: "later",
        aliases: &[],
        arg_kind: ArgKind::Raw,
        min_prefix: 3,
        run: later_handler::<H>,
    });

    // `:edit` / `:e` (min_prefix=1; no other registered command starts with `e`)
    reg.add(ExCommand {
        name: "edit",
        aliases: &[],
        arg_kind: ArgKind::Path,
        min_prefix: 1,
        run: edit_handler::<H>,
    });

    // `:edit!` / `:e!` (min_prefix=2)
    reg.add(ExCommand {
        name: "edit!",
        aliases: &["e!"],
        arg_kind: ArgKind::Path,
        min_prefix: 2,
        run: edit_force_handler::<H>,
    });

    // `:read` / `:r` (min_prefix=1; `:re` still ambiguous with `:redo` at min=3)
    reg.add(ExCommand {
        name: "read",
        aliases: &[],
        arg_kind: ArgKind::Path,
        min_prefix: 1,
        run: read_handler::<H>,
    });

    // `:bdelete` / `:bd` (min_prefix=2)
    reg.add(ExCommand {
        name: "bdelete",
        aliases: &["bd"],
        arg_kind: ArgKind::None,
        min_prefix: 2,
        run: bdelete_handler::<H>,
    });

    // `:bdelete!` / `:bd!` (min_prefix=3)
    reg.add(ExCommand {
        name: "bdelete!",
        aliases: &["bd!"],
        arg_kind: ArgKind::None,
        min_prefix: 3,
        run: bdelete_force_handler::<H>,
    });

    // `:bwipeout` / `:bw` (min_prefix=2)
    reg.add(ExCommand {
        name: "bwipeout",
        aliases: &["bw"],
        arg_kind: ArgKind::None,
        min_prefix: 2,
        run: bwipeout_handler::<H>,
    });

    // `:bwipeout!` / `:bw!` (min_prefix=3)
    reg.add(ExCommand {
        name: "bwipeout!",
        aliases: &["bw!"],
        arg_kind: ArgKind::None,
        min_prefix: 3,
        run: bwipeout_force_handler::<H>,
    });

    // `:saveas {path}` / `:sav {path}` — write and rename buffer (min_prefix=3).
    reg.add(ExCommand {
        name: "saveas",
        aliases: &["sav"],
        arg_kind: ArgKind::Path,
        min_prefix: 3,
        run: saveas_handler::<H>,
    });

    // `:file [{name}]` — no-arg: show filename; with-arg: rename buffer (min_prefix=1).
    reg.add(ExCommand {
        name: "file",
        aliases: &[],
        arg_kind: ArgKind::Path,
        min_prefix: 1,
        run: file_handler::<H>,
    });

    // `:cd [{path}]` — change working directory (no-arg → $HOME) (min_prefix=2).
    reg.add(ExCommand {
        name: "cd",
        aliases: &[],
        arg_kind: ArgKind::Path,
        min_prefix: 2,
        run: cd_handler::<H>,
    });

    // `:pwd` — print working directory (min_prefix=3).
    reg.add(ExCommand {
        name: "pwd",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 3,
        run: pwd_handler::<H>,
    });

    // `:put [{reg}]` / `:pu [{reg}]` — paste register as new line below cursor.
    reg.add(ExCommand {
        name: "put",
        aliases: &["pu"],
        arg_kind: ArgKind::Raw,
        min_prefix: 2,
        run: put_handler::<H>,
    });

    // `:put! [{reg}]` / `:pu!` — paste register as new line above cursor.
    reg.add(ExCommand {
        name: "put!",
        aliases: &["pu!"],
        arg_kind: ArgKind::Raw,
        min_prefix: 3,
        run: put_above_handler::<H>,
    });

    // `:registers` / `:reg` (min_prefix=3; `:reg` via alias since "reg" < 3 chars)
    reg.add(ExCommand {
        name: "registers",
        aliases: &["reg"],
        arg_kind: ArgKind::None,
        min_prefix: 3,
        run: registers_handler::<H>,
    });

    // `:marks` (min_prefix=5)
    reg.add(ExCommand {
        name: "marks",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 5,
        run: marks_handler::<H>,
    });

    // `:jumps` (min_prefix=5)
    reg.add(ExCommand {
        name: "jumps",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 5,
        run: jumps_handler::<H>,
    });

    // `:changes` (min_prefix=7)
    reg.add(ExCommand {
        name: "changes",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 7,
        run: changes_handler::<H>,
    });

    // `:join` / `:j` (min_prefix=1; range-aware; optional trailing [count]).
    reg.add(ExCommand {
        name: "join",
        aliases: &["j"],
        arg_kind: ArgKind::Raw,
        min_prefix: 1,
        run: join_handler::<H>,
    });

    // `:join!` / `:j!` — gJ semantics (no space, no leading-whitespace strip).
    // Registered as its own exact name/alias because `split_name_args` glues
    // a trailing `!` onto the command NAME (not into `args`), so `:j!` never
    // reaches the bare `join` entry above.
    reg.add(ExCommand {
        name: "join!",
        aliases: &["j!"],
        arg_kind: ArgKind::Raw,
        min_prefix: 5,
        run: join_bang_handler::<H>,
    });

    // `:delete` / `:d` (min_prefix=1; range-aware)
    reg.add(ExCommand {
        name: "delete",
        aliases: &["d"],
        arg_kind: ArgKind::None,
        min_prefix: 1,
        run: delete_handler::<H>,
    });

    // `:sort` (min_prefix=3; range-aware)
    reg.add(ExCommand {
        name: "sort",
        aliases: &[],
        arg_kind: ArgKind::Raw,
        min_prefix: 3,
        run: sort_handler::<H>,
    });

    // `:move` / `:m` — relocate lines (range-aware; takes a dest address).
    reg.add(ExCommand {
        name: "move",
        aliases: &["m"],
        arg_kind: ArgKind::Raw,
        min_prefix: 2,
        run: move_handler::<H>,
    });

    // `:copy` / `:co` / `:t` — duplicate lines (range-aware; dest address).
    reg.add(ExCommand {
        name: "copy",
        aliases: &["co", "t"],
        arg_kind: ArgKind::Raw,
        min_prefix: 2,
        run: copy_handler::<H>,
    });

    // `:substitute` / `:s` (min_prefix=1; range-aware)
    // `:&` and `:~` (repeat-last-substitute) are NOT registered here — their
    // non-alphabetic names cannot be parsed by `split_name_args`.  Deferred to
    // a future phase that extends the command-name parser.
    reg.add(ExCommand {
        name: "substitute",
        aliases: &[],
        arg_kind: ArgKind::Raw,
        min_prefix: 1,
        run: substitute_handler::<H>,
    });

    // `:set` / `:se` (min_prefix=2 — matches legacy COMMAND_NAMES line 47)
    reg.add(ExCommand {
        name: "set",
        aliases: &[],
        arg_kind: ArgKind::Setting,
        min_prefix: 2,
        run: set_handler::<H>,
    });

    // ---- Phase 8a ----------------------------------------------------------

    // `:foldindent` (min_prefix=5; `:foldi` is the shortest unambiguous form)
    reg.add(ExCommand {
        name: "foldindent",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 5,
        run: |editor, args, range| apply_fold_indent(editor, args, range),
    });

    // `:foldsyntax` (min_prefix=5; `:folds` is shortest — same as foldindent)
    reg.add(ExCommand {
        name: "foldsyntax",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 5,
        run: |editor, args, range| apply_fold_syntax(editor, args, range),
    });

    // `:global` / `:g` (min_prefix=1; range-aware)
    // `:global!/pat/cmd` is handled by global_match_handler (strips leading `!`).
    reg.add(ExCommand {
        name: "global",
        aliases: &["g"],
        arg_kind: ArgKind::Raw,
        min_prefix: 1,
        run: |editor, args, range| global_match_handler(editor, args, range),
    });

    // `:vglobal` / `:v` (min_prefix=1; range-aware)
    reg.add(ExCommand {
        name: "vglobal",
        aliases: &["v"],
        arg_kind: ArgKind::Raw,
        min_prefix: 1,
        run: |editor, args, range| vglobal_handler(editor, args, range),
    });

    // ---- Phase #187 -----------------------------------------------------------

    // `:comment` (min_prefix=3; toggle line comments; range-aware)
    reg.add(ExCommand {
        name: "comment",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 3,
        run: comment_handler::<H>,
    });

    // `:uncomment` (min_prefix=5; force-strip line comments; range-aware)
    reg.add(ExCommand {
        name: "uncomment",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 5,
        run: uncomment_handler::<H>,
    });

    // `:syntax [on|off|enable|disable]` — vim-compat syntax-highlight toggle.
    // Engine never touches highlighting; this returns Ok so headless/nvim-api
    // paths see success. The TUI app overrides via the host registry to
    // actually drop/re-attach bonsai layers (see ex_host_cmds::SyntaxCmd).
    reg.add(ExCommand {
        name: "syntax",
        aliases: &[],
        arg_kind: ArgKind::Raw,
        min_prefix: 3,
        run: syntax_handler::<H>,
    });

    // `:redraw` — repaint without clearing (min_prefix=6; "redraw" is 6 chars).
    reg.add(ExCommand {
        name: "redraw",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 6,
        run: redraw_handler::<H>,
    });

    // `:redraw!` — clear terminal then repaint (min_prefix=7).
    reg.add(ExCommand {
        name: "redraw!",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 7,
        run: redraw_clear_handler::<H>,
    });

    // ---- Phase #207 ----------------------------------------------------------

    // `:retab [N]` — convert leading whitespace per expandtab/tabstop (min_prefix=3).
    reg.add(ExCommand {
        name: "retab",
        aliases: &[],
        arg_kind: ArgKind::Raw,
        min_prefix: 3,
        run: retab_handler::<H>,
    });

    // `:retab! [N]` — also convert internal whitespace runs (min_prefix=4).
    reg.add(ExCommand {
        name: "retab!",
        aliases: &[],
        arg_kind: ArgKind::Raw,
        min_prefix: 4,
        run: retab_bang_handler::<H>,
    });

    // ---- quickfix (#184) -----------------------------------------------------
    // min_prefix chosen so each 2-3 char prefix resolves unambiguously and does
    // not shadow `:cd`/`:colorscheme` etc.
    reg.add(ExCommand {
        name: "copen",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 4, // "cope"
        run: copen_handler::<H>,
    });
    reg.add(ExCommand {
        name: "cclose",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 3, // "ccl"
        run: cclose_handler::<H>,
    });
    reg.add(ExCommand {
        name: "cnext",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 2, // "cn"
        run: cnext_handler::<H>,
    });
    reg.add(ExCommand {
        name: "cprevious",
        aliases: &["cprev", "cp"],
        arg_kind: ArgKind::None,
        min_prefix: 2, // "cp"
        run: cprev_handler::<H>,
    });
    reg.add(ExCommand {
        name: "cfirst",
        aliases: &["crewind"],
        arg_kind: ArgKind::None,
        min_prefix: 2, // "cf"
        run: cfirst_handler::<H>,
    });
    reg.add(ExCommand {
        name: "clast",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 3, // "cla"
        run: clast_handler::<H>,
    });
    reg.add(ExCommand {
        name: "cc",
        aliases: &[],
        arg_kind: ArgKind::Raw,
        min_prefix: 2, // exact "cc"
        run: cc_handler::<H>,
    });
    reg.add(ExCommand {
        name: "grep",
        aliases: &["vimgrep"],
        arg_kind: ArgKind::Raw,
        min_prefix: 3, // "gre"
        run: grep_handler::<H>,
    });
    reg.add(ExCommand {
        name: "make",
        aliases: &[],
        arg_kind: ArgKind::Raw,
        min_prefix: 4, // "make"
        run: make_handler::<H>,
    });

    // :cexpr / :cgetexpr / :caddexpr — populate quickfix from expression (#261)
    reg.add(ExCommand {
        name: "cexpr",
        aliases: &[],
        arg_kind: ArgKind::Raw,
        min_prefix: 3, // "cex"
        run: cexpr_handler::<H>,
    });
    reg.add(ExCommand {
        name: "cgetexpr",
        aliases: &[],
        arg_kind: ArgKind::Raw,
        min_prefix: 5, // "cgete"
        run: cgetexpr_handler::<H>,
    });
    reg.add(ExCommand {
        name: "caddexpr",
        aliases: &[],
        arg_kind: ArgKind::Raw,
        min_prefix: 3, // "cad"
        run: caddexpr_handler::<H>,
    });

    // location-list family (#184 phase 3)
    reg.add(ExCommand {
        name: "lopen",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 3, // "lop"
        run: lopen_handler::<H>,
    });
    reg.add(ExCommand {
        name: "lclose",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 3, // "lcl"
        run: lclose_handler::<H>,
    });
    reg.add(ExCommand {
        name: "lnext",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 3, // "lne"
        run: lnext_handler::<H>,
    });
    reg.add(ExCommand {
        name: "lprevious",
        aliases: &["lprev", "lp"],
        arg_kind: ArgKind::None,
        min_prefix: 2, // "lp"
        run: lprev_handler::<H>,
    });
    reg.add(ExCommand {
        name: "lfirst",
        aliases: &["lrewind"],
        arg_kind: ArgKind::None,
        min_prefix: 3, // "lfi"
        run: lfirst_handler::<H>,
    });
    reg.add(ExCommand {
        name: "llast",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 3, // "lla"
        run: llast_handler::<H>,
    });
    reg.add(ExCommand {
        name: "ll",
        aliases: &[],
        arg_kind: ArgKind::Raw,
        min_prefix: 2, // exact "ll"
        run: ll_handler::<H>,
    });
    reg.add(ExCommand {
        name: "lgrep",
        aliases: &["lvimgrep"],
        arg_kind: ArgKind::Raw,
        min_prefix: 3, // "lgr"
        run: lgrep_handler::<H>,
    });
    reg.add(ExCommand {
        name: "lmake",
        aliases: &[],
        arg_kind: ArgKind::Raw,
        min_prefix: 4, // "lmak"
        run: lmake_handler::<H>,
    });

    // :lexpr / :lgetexpr / :laddexpr — populate location list from expression (#261)
    reg.add(ExCommand {
        name: "lexpr",
        aliases: &[],
        arg_kind: ArgKind::Raw,
        min_prefix: 3, // "lex"
        run: lexpr_handler::<H>,
    });
    reg.add(ExCommand {
        name: "lgetexpr",
        aliases: &[],
        arg_kind: ArgKind::Raw,
        min_prefix: 5, // "lgete"
        run: lgetexpr_handler::<H>,
    });
    reg.add(ExCommand {
        name: "laddexpr",
        aliases: &[],
        arg_kind: ArgKind::Raw,
        min_prefix: 3, // "lad"
        run: laddexpr_handler::<H>,
    });

    // :cbuffer / :cgetbuffer / :caddbuffer — populate quickfix from current buffer (#261)
    reg.add(ExCommand {
        name: "cbuffer",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 2, // "cb"
        run: cbuffer_handler::<H>,
    });
    reg.add(ExCommand {
        name: "cgetbuffer",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 5, // "cgetb"
        run: cgetbuffer_handler::<H>,
    });
    reg.add(ExCommand {
        name: "caddbuffer",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 5, // "caddb"
        run: caddbuffer_handler::<H>,
    });

    // :cfile / :cgetfile / :caddfile — populate quickfix from file on disk (#261)
    reg.add(ExCommand {
        name: "cfile",
        aliases: &[],
        arg_kind: ArgKind::Path,
        min_prefix: 4, // "cfil" — avoids ambiguity with :cfirst (min=2 "cf")
        run: cfile_handler::<H>,
    });
    reg.add(ExCommand {
        name: "cgetfile",
        aliases: &[],
        arg_kind: ArgKind::Path,
        min_prefix: 5, // "cgetf"
        run: cgetfile_handler::<H>,
    });
    reg.add(ExCommand {
        name: "caddfile",
        aliases: &[],
        arg_kind: ArgKind::Path,
        min_prefix: 5, // "caddf"
        run: caddfile_handler::<H>,
    });

    // :lbuffer / :lgetbuffer / :laddbuffer — populate location list from current buffer (#261)
    reg.add(ExCommand {
        name: "lbuffer",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 2, // "lb"
        run: lbuffer_handler::<H>,
    });
    reg.add(ExCommand {
        name: "lgetbuffer",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 5, // "lgetb"
        run: lgetbuffer_handler::<H>,
    });
    reg.add(ExCommand {
        name: "laddbuffer",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 5, // "laddb"
        run: laddbuffer_handler::<H>,
    });

    // :lfile / :lgetfile / :laddfile — populate location list from file on disk (#261)
    reg.add(ExCommand {
        name: "lfile",
        aliases: &[],
        arg_kind: ArgKind::Path,
        min_prefix: 4, // "lfil" — avoids ambiguity with :lfirst (min=3 "lfi")
        run: lfile_handler::<H>,
    });
    reg.add(ExCommand {
        name: "lgetfile",
        aliases: &[],
        arg_kind: ArgKind::Path,
        min_prefix: 5, // "lgetf"
        run: lgetfile_handler::<H>,
    });
    reg.add(ExCommand {
        name: "laddfile",
        aliases: &[],
        arg_kind: ArgKind::Path,
        min_prefix: 5, // "laddf"
        run: laddfile_handler::<H>,
    });

    // :colder / :cnewer / :lolder / :lnewer — list-stack navigation (#261 Phase 5b)
    // min_prefix for colder: "col" (3) — no clash with :colorscheme (min=5 "color")
    //   or :copen (min=4 "cope"). "col" is unambiguous.
    // min_prefix for cnewer: "cnew" (4) — needed to avoid matching :cnext (min=2 "cn"):
    //   "cn" and "cne" both resolve to cnext; "cnew" is the first prefix exclusive to cnewer.
    // min_prefix for lolder: "lol" (3) — no conflicts.
    // min_prefix for lnewer: "lnew" (4) — "lne" resolves to lnext; "lnew" is exclusive.
    reg.add(ExCommand {
        name: "colder",
        aliases: &[],
        arg_kind: ArgKind::Raw,
        min_prefix: 3, // "col"
        run: colder_handler::<H>,
    });
    reg.add(ExCommand {
        name: "cnewer",
        aliases: &[],
        arg_kind: ArgKind::Raw,
        min_prefix: 4, // "cnew" — avoids clash with :cnext (min=2 "cn")
        run: cnewer_handler::<H>,
    });
    reg.add(ExCommand {
        name: "lolder",
        aliases: &[],
        arg_kind: ArgKind::Raw,
        min_prefix: 3, // "lol"
        run: lolder_handler::<H>,
    });
    reg.add(ExCommand {
        name: "lnewer",
        aliases: &[],
        arg_kind: ArgKind::Raw,
        min_prefix: 4, // "lnew" — avoids clash with :lnext (min=3 "lne")
        run: lnewer_handler::<H>,
    });

    // :cdo / :cfdo / :ldo / :lfdo — run a command per entry / per file (#261 Phase 5b "A2")
    // min_prefix for cdo:  "cdo" (3) — "cd" is `:cd` (exact), "cdo" is unambiguous.
    // min_prefix for cfdo: "cfd" (3) — "cf" is `:cfirst` (min=2); "cfd" is exclusive to cfdo.
    // min_prefix for ldo:  "ldo" (3) — no conflicts.
    // min_prefix for lfdo: "lfd" (3) — "lf" < lfirst min=3; "lfd" is exclusive to lfdo.
    reg.add(ExCommand {
        name: "cdo",
        aliases: &[],
        arg_kind: ArgKind::Raw,
        min_prefix: 3, // "cdo"
        run: cdo_handler::<H>,
    });
    reg.add(ExCommand {
        name: "cfdo",
        aliases: &[],
        arg_kind: ArgKind::Raw,
        min_prefix: 3, // "cfd" — avoids :cfirst (min=2 "cf")
        run: cfdo_handler::<H>,
    });
    reg.add(ExCommand {
        name: "ldo",
        aliases: &[],
        arg_kind: ArgKind::Raw,
        min_prefix: 3, // "ldo"
        run: ldo_handler::<H>,
    });
    reg.add(ExCommand {
        name: "lfdo",
        aliases: &[],
        arg_kind: ArgKind::Raw,
        min_prefix: 3, // "lfd" — avoids :lfirst (min=3 "lfi")
        run: lfdo_handler::<H>,
    });

    // `:preserve` — force-write the swap file immediately (issue #185).
    // min_prefix=3 so `:pre` resolves here.
    reg.add(ExCommand {
        name: "preserve",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 3,
        run: preserve_handler::<H>,
    });

    // `:recover [file]` — explicit swap-file recovery (issue #185).
    // min_prefix=3 so `:rec` resolves here.
    reg.add(ExCommand {
        name: "recover",
        aliases: &[],
        arg_kind: ArgKind::Raw,
        min_prefix: 3,
        run: recover_handler::<H>,
    });

    // ---- :diagnostics / :ldiagnostics (#261 Phase 5b A3) --------------------

    // `:diagnostics` — populate quickfix from all-buffer LSP diags.
    // min_prefix=4 ("diag") — avoids clash with `:delete` (min=1 "d"),
    // `:delete` only claims single-letter prefixes; "dia" is unambiguous but
    // "diag" makes intent clear without shadowing anything.
    // No clash with `:diffthis`, `:diffget`, `:diffput` etc. (all start "diff").
    reg.add(ExCommand {
        name: "diagnostics",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 4, // "diag"
        run: diagnostics_handler::<H>,
    });

    // `:ldiagnostics` — populate location list from current-buffer LSP diags.
    // min_prefix=5 ("ldiag") — "ld" is ambiguous with :ldo (min=3 "ldo") only
    // at 2 chars; "ldi" is exclusive to ldiagnostics.  Use 5 for readability.
    reg.add(ExCommand {
        name: "ldiagnostics",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 5, // "ldiag"
        run: ldiagnostics_handler::<H>,
    });

    // ---- abbreviations (#abbreviations) -------------------------------------

    // `:abbreviate {lhs} {rhs}` / `:ab {lhs} {rhs}` — insert + cmdline abbrev.
    reg.add(ExCommand {
        name: "abbreviate",
        aliases: &["ab"],
        arg_kind: ArgKind::Raw,
        min_prefix: 2,
        run: abbreviate_handler::<H>,
    });

    // `:iabbrev {lhs} {rhs}` / `:iab {lhs} {rhs}` — insert-only abbrev.
    reg.add(ExCommand {
        name: "iabbrev",
        aliases: &["iab"],
        arg_kind: ArgKind::Raw,
        min_prefix: 3,
        run: iabbrev_handler::<H>,
    });

    // `:cabbrev {lhs} {rhs}` / `:cab {lhs} {rhs}` — cmdline-only abbrev.
    reg.add(ExCommand {
        name: "cabbrev",
        aliases: &["cab"],
        arg_kind: ArgKind::Raw,
        min_prefix: 3,
        run: cabbrev_handler::<H>,
    });

    // `:noreabbrev {lhs} {rhs}` — non-recursive insert + cmdline abbrev.
    reg.add(ExCommand {
        name: "noreabbrev",
        aliases: &[],
        arg_kind: ArgKind::Raw,
        min_prefix: 5,
        run: noreabbrev_handler::<H>,
    });

    // `:inoreabbrev {lhs} {rhs}` — non-recursive insert-only abbrev.
    reg.add(ExCommand {
        name: "inoreabbrev",
        aliases: &[],
        arg_kind: ArgKind::Raw,
        min_prefix: 6,
        run: inoreabbrev_handler::<H>,
    });

    // `:cnoreabbrev {lhs} {rhs}` — non-recursive cmdline-only abbrev.
    reg.add(ExCommand {
        name: "cnoreabbrev",
        aliases: &[],
        arg_kind: ArgKind::Raw,
        min_prefix: 6,
        run: cnoreabbrev_handler::<H>,
    });

    // `:unabbreviate {lhs}` / `:una {lhs}` — remove insert + cmdline abbrev.
    reg.add(ExCommand {
        name: "unabbreviate",
        aliases: &["una"],
        arg_kind: ArgKind::Raw,
        min_prefix: 3,
        run: unabbreviate_handler::<H>,
    });

    // `:iunabbrev {lhs}` / `:iun {lhs}` — remove insert-only abbrev.
    reg.add(ExCommand {
        name: "iunabbrev",
        aliases: &["iun"],
        arg_kind: ArgKind::Raw,
        min_prefix: 3,
        run: iunabbrev_handler::<H>,
    });

    // `:cunabbrev {lhs}` / `:cun {lhs}` — remove cmdline-only abbrev.
    reg.add(ExCommand {
        name: "cunabbrev",
        aliases: &["cun"],
        arg_kind: ArgKind::Raw,
        min_prefix: 3,
        run: cunabbrev_handler::<H>,
    });

    // `:abclear` — clear all insert + cmdline abbreviations.
    reg.add(ExCommand {
        name: "abclear",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 3,
        run: abclear_handler::<H>,
    });

    // `:iabclear` — clear all insert-only abbreviations.
    reg.add(ExCommand {
        name: "iabclear",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 4,
        run: iabclear_handler::<H>,
    });

    // `:cabclear` — clear all cmdline-only abbreviations.
    reg.add(ExCommand {
        name: "cabclear",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 4,
        run: cabclear_handler::<H>,
    });
}

// ---- :redraw ---------------------------------------------------------------

/// `:redraw` — signal the host to repaint without clearing.
fn redraw_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Redraw { clear: false })
}

/// `:redraw!` — signal the host to clear the terminal then repaint.
fn redraw_clear_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Redraw { clear: true })
}

// ---- quickfix (#184) -------------------------------------------------------

macro_rules! qf_handler {
    ($name:ident, $cmd:expr) => {
        fn $name<H: Host>(
            _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
            _args: &str,
            _range: Option<LineRange>,
        ) -> Option<ExEffect> {
            Some(ExEffect::Quickfix($cmd))
        }
    };
}

qf_handler!(copen_handler, QfCommand::Open);
qf_handler!(cclose_handler, QfCommand::Close);
qf_handler!(cnext_handler, QfCommand::Next);
qf_handler!(cprev_handler, QfCommand::Prev);
qf_handler!(cfirst_handler, QfCommand::First);
qf_handler!(clast_handler, QfCommand::Last);

/// `:cc [N]` — jump to the 1-based entry `N`; no arg means "current" (`Nth(0)`).
fn cc_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    let n = args.trim().parse::<usize>().unwrap_or(0);
    Some(ExEffect::Quickfix(QfCommand::Nth(n)))
}

/// `:grep <pattern>` — run ripgrep, populate the quickfix list, open the popup.
fn grep_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    let pat = args.trim();
    if pat.is_empty() {
        return Some(ExEffect::Error("E471: Argument required: grep".into()));
    }
    Some(ExEffect::Quickfix(QfCommand::Grep(pat.to_string())))
}

/// `:make [args]` — run `makeprg` (appending `args`), parse output via the
/// errorformat, populate the quickfix list, open the popup.
fn make_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Quickfix(QfCommand::Make(args.trim().to_string())))
}

/// `:cexpr {expr}` — parse `expr` via errorformat, replace quickfix list, jump to first.
fn cexpr_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Quickfix(QfCommand::Expr {
        text: args.trim().to_string(),
        append: false,
        jump: true,
    }))
}

/// `:cgetexpr {expr}` — parse `expr` via errorformat, replace quickfix list, no jump.
fn cgetexpr_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Quickfix(QfCommand::Expr {
        text: args.trim().to_string(),
        append: false,
        jump: false,
    }))
}

/// `:caddexpr {expr}` — parse `expr` via errorformat, append to quickfix list, no jump.
fn caddexpr_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Quickfix(QfCommand::Expr {
        text: args.trim().to_string(),
        append: true,
        jump: false,
    }))
}

// ---- location list (#184 phase 3) ------------------------------------------
// The `:l*` family mirrors the `:c*` family but targets the window-local
// location list via `ExEffect::Location`.

macro_rules! loc_handler {
    ($name:ident, $cmd:expr) => {
        fn $name<H: Host>(
            _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
            _args: &str,
            _range: Option<LineRange>,
        ) -> Option<ExEffect> {
            Some(ExEffect::Location($cmd))
        }
    };
}

loc_handler!(lopen_handler, QfCommand::Open);
loc_handler!(lclose_handler, QfCommand::Close);
loc_handler!(lnext_handler, QfCommand::Next);
loc_handler!(lprev_handler, QfCommand::Prev);
loc_handler!(lfirst_handler, QfCommand::First);
loc_handler!(llast_handler, QfCommand::Last);

/// `:ll [N]` — jump to the 1-based location entry `N`; no arg means "current".
fn ll_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    let n = args.trim().parse::<usize>().unwrap_or(0);
    Some(ExEffect::Location(QfCommand::Nth(n)))
}

/// `:lgrep <pattern>` — run ripgrep, populate the location list, open popup.
fn lgrep_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    let pat = args.trim();
    if pat.is_empty() {
        return Some(ExEffect::Error("E471: Argument required: lgrep".into()));
    }
    Some(ExEffect::Location(QfCommand::Grep(pat.to_string())))
}

/// `:lmake [args]` — run `makeprg` into the location list.
fn lmake_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Location(QfCommand::Make(args.trim().to_string())))
}

/// `:lexpr {expr}` — parse `expr` via errorformat, replace location list, jump to first.
fn lexpr_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Location(QfCommand::Expr {
        text: args.trim().to_string(),
        append: false,
        jump: true,
    }))
}

/// `:lgetexpr {expr}` — parse `expr` via errorformat, replace location list, no jump.
fn lgetexpr_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Location(QfCommand::Expr {
        text: args.trim().to_string(),
        append: false,
        jump: false,
    }))
}

/// `:laddexpr {expr}` — parse `expr` via errorformat, append to location list, no jump.
fn laddexpr_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Location(QfCommand::Expr {
        text: args.trim().to_string(),
        append: true,
        jump: false,
    }))
}

// ---- :cbuffer / :cgetbuffer / :caddbuffer (#261) ---------------------------

/// `:cbuffer` — parse current buffer via errorformat, replace quickfix list, jump to first.
fn cbuffer_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Quickfix(QfCommand::FromBuffer {
        append: false,
        jump: true,
    }))
}

/// `:cgetbuffer` — parse current buffer via errorformat, replace quickfix list, no jump.
fn cgetbuffer_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Quickfix(QfCommand::FromBuffer {
        append: false,
        jump: false,
    }))
}

/// `:caddbuffer` — parse current buffer via errorformat, append to quickfix list, no jump.
fn caddbuffer_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Quickfix(QfCommand::FromBuffer {
        append: true,
        jump: false,
    }))
}

// ---- :cfile / :cgetfile / :caddfile (#261) ---------------------------------

/// `:cfile [path]` — read file, parse via errorformat, replace quickfix list, jump to first.
fn cfile_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Quickfix(QfCommand::FromFile {
        path: args.trim().to_string(),
        append: false,
        jump: true,
    }))
}

/// `:cgetfile [path]` — read file, parse via errorformat, replace quickfix list, no jump.
fn cgetfile_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Quickfix(QfCommand::FromFile {
        path: args.trim().to_string(),
        append: false,
        jump: false,
    }))
}

/// `:caddfile [path]` — read file, parse via errorformat, append to quickfix list, no jump.
fn caddfile_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Quickfix(QfCommand::FromFile {
        path: args.trim().to_string(),
        append: true,
        jump: false,
    }))
}

// ---- :lbuffer / :lgetbuffer / :laddbuffer (#261) ---------------------------

/// `:lbuffer` — parse current buffer via errorformat, replace location list, jump to first.
fn lbuffer_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Location(QfCommand::FromBuffer {
        append: false,
        jump: true,
    }))
}

/// `:lgetbuffer` — parse current buffer via errorformat, replace location list, no jump.
fn lgetbuffer_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Location(QfCommand::FromBuffer {
        append: false,
        jump: false,
    }))
}

/// `:laddbuffer` — parse current buffer via errorformat, append to location list, no jump.
fn laddbuffer_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Location(QfCommand::FromBuffer {
        append: true,
        jump: false,
    }))
}

// ---- :lfile / :lgetfile / :laddfile (#261) ---------------------------------

/// `:lfile [path]` — read file, parse via errorformat, replace location list, jump to first.
fn lfile_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Location(QfCommand::FromFile {
        path: args.trim().to_string(),
        append: false,
        jump: true,
    }))
}

/// `:lgetfile [path]` — read file, parse via errorformat, replace location list, no jump.
fn lgetfile_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Location(QfCommand::FromFile {
        path: args.trim().to_string(),
        append: false,
        jump: false,
    }))
}

/// `:laddfile [path]` — read file, parse via errorformat, append to location list, no jump.
fn laddfile_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Location(QfCommand::FromFile {
        path: args.trim().to_string(),
        append: true,
        jump: false,
    }))
}

// ---- :colder / :cnewer / :lolder / :lnewer (#261 Phase 5b) ----------------

/// `:colder [N]` — activate an older quickfix list (default 1 step).
fn colder_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    let n = args.trim().parse::<usize>().unwrap_or(1).max(1);
    Some(ExEffect::Quickfix(QfCommand::Older(n)))
}

/// `:cnewer [N]` — activate a newer quickfix list (default 1 step).
fn cnewer_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    let n = args.trim().parse::<usize>().unwrap_or(1).max(1);
    Some(ExEffect::Quickfix(QfCommand::Newer(n)))
}

/// `:lolder [N]` — activate an older location list (default 1 step).
fn lolder_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    let n = args.trim().parse::<usize>().unwrap_or(1).max(1);
    Some(ExEffect::Location(QfCommand::Older(n)))
}

/// `:lnewer [N]` — activate a newer location list (default 1 step).
fn lnewer_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    let n = args.trim().parse::<usize>().unwrap_or(1).max(1);
    Some(ExEffect::Location(QfCommand::Newer(n)))
}

// ---- :cdo / :cfdo / :ldo / :lfdo (#261 Phase 5b "A2") ---------------------

/// `:cdo {cmd}` — run `cmd` once per valid quickfix entry.
fn cdo_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Quickfix(QfCommand::Do {
        cmd: args.to_string(),
        per_file: false,
    }))
}

/// `:cfdo {cmd}` — run `cmd` once per distinct file in the quickfix list.
fn cfdo_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Quickfix(QfCommand::Do {
        cmd: args.to_string(),
        per_file: true,
    }))
}

/// `:ldo {cmd}` — run `cmd` once per valid location-list entry.
fn ldo_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Location(QfCommand::Do {
        cmd: args.to_string(),
        per_file: false,
    }))
}

/// `:lfdo {cmd}` — run `cmd` once per distinct file in the location list.
fn lfdo_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Location(QfCommand::Do {
        cmd: args.to_string(),
        per_file: true,
    }))
}

// ---- :syntax ---------------------------------------------------------------

/// `:syntax [on|off|enable|disable|...]` — engine-side no-op for vim parity.
///
/// Recognised subcommands return `ExEffect::Ok`. Unknown args also return
/// `Ok` (vim's `:syntax <bareword>` is permissive — many forms like
/// `:syntax sync`, `:syntax clear`, `:syntax reset` are accepted without
/// error).
fn syntax_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Ok)
}

// ---- comment / uncomment (#187) --------------------------------------------

/// `:[range]comment` — toggle line comments on the range.
///
/// Toggle algorithm (vim-commentary parity):
/// - Scan non-blank lines.  If every non-blank line is commented → uncomment.
/// - Otherwise → comment all non-blank lines.
///
/// No range → current cursor line.
fn comment_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    range: Option<LineRange>,
) -> Option<ExEffect> {
    let (top, bot) = resolve_comment_range(editor, range);
    editor.toggle_comment_range(top, bot);
    Some(ExEffect::Ok)
}

/// `:[range]uncomment` — force-remove comment markers (idempotent no-op when
/// not commented).
///
/// Achieves "force uncomment" by temporarily overriding the all-commented
/// check: scan each non-blank line and strip exactly one occurrence of the
/// comment marker if present. Lines that are not commented are left unchanged.
fn uncomment_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    range: Option<LineRange>,
) -> Option<ExEffect> {
    use hjkl_lang::comment::commentstring_for_lang;

    let (top, bot) = resolve_comment_range(editor, range);
    let lang = editor.settings().filetype.clone();

    // Resolve comment markers (same priority as toggle_comment_range).
    let (start, end): (String, Option<String>) = if !editor.settings().commentstring.is_empty() {
        let cs = editor.settings().commentstring.clone();
        if let Some(idx) = cs.find("%s") {
            let s = cs[..idx].trim_end().to_string();
            let e_raw = cs[idx + 2..].trim_start();
            let e = if e_raw.is_empty() {
                None
            } else {
                Some(e_raw.to_string())
            };
            (s, e)
        } else {
            (cs, None)
        }
    } else {
        match commentstring_for_lang(&lang) {
            Some((s, e)) => (s.to_string(), e.map(|v| v.to_string())),
            None => return Some(ExEffect::Ok), // no-op
        }
    };

    // Collect lines using the rope API.
    let row_count = editor.buffer().row_count();
    let top_c = top.min(row_count.saturating_sub(1));
    let bot_c = bot.min(row_count.saturating_sub(1));

    let rope = editor.buffer().rope();
    let lines: Vec<String> = (top_c..=bot_c)
        .map(|r| hjkl_buffer::rope_line_str(&rope, r))
        .collect();

    let mut new_lines: Vec<String> = Vec::with_capacity(lines.len());
    for line in &lines {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            new_lines.push(line.clone());
            continue;
        }
        let indent_len = line.len() - trimmed.len();
        let indent = &line[..indent_len];

        if let Some(after_start) = trimmed.strip_prefix(start.as_str()) {
            let after_space = after_start.strip_prefix(' ').unwrap_or(after_start);
            let text = if let Some(ref end_marker) = end {
                after_space
                    .trim_end()
                    .strip_suffix(end_marker.as_str())
                    .map(|s| s.trim_end())
                    .unwrap_or(after_space)
            } else {
                after_space
            };
            new_lines.push(format!("{indent}{text}"));
        } else {
            // Not commented — leave unchanged.
            new_lines.push(line.clone());
        }
    }

    editor.push_undo();
    let total_rows = editor.buffer().row_count();
    let all_before: Vec<String> = (0..top_c)
        .map(|r| hjkl_buffer::rope_line_str(&rope, r))
        .collect();
    let all_after: Vec<String> = ((bot_c + 1)..total_rows)
        .map(|r| hjkl_buffer::rope_line_str(&rope, r))
        .collect();
    let mut all: Vec<String> = all_before;
    all.extend(new_lines);
    all.extend(all_after);
    editor.restore(all, (top_c, 0));

    Some(ExEffect::Ok)
}

/// Resolve a `LineRange` to `(top, bot)` 0-based row indices.
/// No range → current cursor line.
fn resolve_comment_range<H: Host>(
    editor: &hjkl_engine::Editor<hjkl_buffer::View, H>,
    range: Option<LineRange>,
) -> (usize, usize) {
    match range {
        Some(lr) => {
            let top = lr.start_one_based().saturating_sub(1);
            let bot = lr.end_one_based().saturating_sub(1);
            (top, bot)
        }
        None => {
            let row = editor.cursor().0;
            (row, row)
        }
    }
}

// ---- retab (#207) ----------------------------------------------------------

/// Convert a single whitespace character sequence.
///
/// When `expandtab=true`: replace tabs with spaces (column-aware tab stops).
/// When `expandtab=false`: replace runs of `tabstop` consecutive spaces with a tab.
///
/// `col` is the starting visual column (0-based) of `ws`. Returns the converted
/// string and the visual column after the whitespace.
fn convert_whitespace(ws: &str, col: usize, tabstop: usize, expandtab: bool) -> (String, usize) {
    let mut out = String::with_capacity(ws.len() * 2);
    let mut vcol = col;
    if expandtab {
        // Tabs → spaces, column-aware
        for ch in ws.chars() {
            match ch {
                '\t' => {
                    let advance = tabstop - (vcol % tabstop);
                    for _ in 0..advance {
                        out.push(' ');
                    }
                    vcol += advance;
                }
                ' ' => {
                    out.push(' ');
                    vcol += 1;
                }
                other => {
                    out.push(other);
                    vcol += 1;
                }
            }
        }
    } else {
        // Spaces → tabs: greedy replacement of space-runs that align to tabstop.
        // First, expand any existing tabs so we can reason in columns.
        let mut expanded = String::with_capacity(ws.len());
        let mut evcol = col;
        for ch in ws.chars() {
            match ch {
                '\t' => {
                    let advance = tabstop - (evcol % tabstop);
                    for _ in 0..advance {
                        expanded.push(' ');
                    }
                    evcol += advance;
                }
                ' ' => {
                    expanded.push(' ');
                    evcol += 1;
                }
                other => {
                    expanded.push(other);
                    evcol += 1;
                }
            }
        }
        // Now re-encode the expanded spaces as tabs where possible.
        let mut space_count = 0usize;
        let mut cur_col = col;
        for ch in expanded.chars() {
            if ch == ' ' {
                space_count += 1;
                cur_col += 1;
                // Emit a tab whenever we hit a tabstop boundary.
                if cur_col.is_multiple_of(tabstop) {
                    out.push('\t');
                    space_count = 0;
                }
            } else {
                // Flush any trailing spaces that didn't hit a boundary.
                for _ in 0..space_count {
                    out.push(' ');
                }
                space_count = 0;
                out.push(ch);
                cur_col += 1;
            }
        }
        // Flush remaining spaces.
        for _ in 0..space_count {
            out.push(' ');
        }
        vcol = cur_col;
    }
    (out, vcol)
}

/// Retab a single line.
///
/// `bang=false`: only convert the leading whitespace.
/// `bang=true`: also convert internal whitespace runs.
fn retab_line(line: &str, tabstop: usize, expandtab: bool, bang: bool) -> String {
    if line.is_empty() {
        return String::new();
    }

    if !bang {
        // Leading whitespace only.
        let leading_end = line.chars().take_while(|c| *c == ' ' || *c == '\t').count();
        let leading: &str = &line[..leading_end];
        let rest: &str = &line[leading_end..];
        let (converted, _) = convert_whitespace(leading, 0, tabstop, expandtab);
        format!("{converted}{rest}")
    } else {
        // Walk the entire line, converting every whitespace run.
        let mut out = String::with_capacity(line.len());
        let mut vcol = 0usize;
        let mut iter = line.char_indices().peekable();
        while let Some((byte_idx, ch)) = iter.next() {
            if ch == ' ' || ch == '\t' {
                // Collect the full whitespace run.
                let ws_start = byte_idx;
                let mut ws_end = byte_idx + ch.len_utf8();
                while let Some(&(_, nc)) = iter.peek() {
                    if nc == ' ' || nc == '\t' {
                        iter.next();
                        ws_end += nc.len_utf8();
                    } else {
                        break;
                    }
                }
                let ws = &line[ws_start..ws_end];
                let (converted, new_col) = convert_whitespace(ws, vcol, tabstop, expandtab);
                out.push_str(&converted);
                vcol = new_col;
            } else {
                out.push(ch);
                vcol += 1;
            }
        }
        out
    }
}

/// `:[range]retab [N]` — convert leading whitespace per expandtab/tabstop.
fn retab_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    range: Option<LineRange>,
) -> Option<ExEffect> {
    retab_impl(editor, args, range, false)
}

/// `:[range]retab! [N]` — also convert internal whitespace runs.
fn retab_bang_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    range: Option<LineRange>,
) -> Option<ExEffect> {
    retab_impl(editor, args, range, true)
}

/// `:preserve` — force-write the swap file for the active buffer immediately.
///
/// The engine side is a pure pass-through; the host (TUI app) handles the
/// actual write when it receives `ExEffect::Preserve`. Issue #185.
fn preserve_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Preserve)
}

/// `:recover [file]` — explicit swap-file recovery (issue #185).
///
/// No arg → recover the current buffer's swap (force recovery prompt even if
/// the swap appears stale).  With a path arg → open/switch to that file then
/// force recovery on it.  The host (TUI app) handles the logic when it
/// receives `ExEffect::Recover`.
fn recover_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Recover(args.trim().to_string()))
}

// ---- :diagnostics / :ldiagnostics (#261 Phase 5b A3) ----------------------

/// `:diagnostics` — populate the QUICKFIX list from LSP diagnostics across all
/// non-explorer buffer slots.
fn diagnostics_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Quickfix(QfCommand::Diagnostics))
}

/// `:ldiagnostics` — populate the LOCATION list from LSP diagnostics of the
/// current buffer slot.
fn ldiagnostics_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Location(QfCommand::Diagnostics))
}

fn retab_impl<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    range: Option<LineRange>,
    bang: bool,
) -> Option<ExEffect> {
    let trimmed = args.trim();

    // Parse optional explicit tabstop argument.
    let explicit_tabstop: Option<usize> = if trimmed.is_empty() {
        None
    } else {
        match trimmed.parse::<usize>() {
            Ok(n) if n > 0 => Some(n),
            Ok(_) => {
                return Some(ExEffect::Error("tabstop must be > 0".into()));
            }
            Err(_) => {
                return Some(ExEffect::Error(format!("invalid tabstop: {trimmed}")));
            }
        }
    };

    let tabstop = explicit_tabstop.unwrap_or(editor.settings().tabstop);
    let expandtab = editor.settings().expandtab;

    let rope = editor.buffer().rope();
    let total = rope.len_lines();
    if total == 0 {
        return Some(ExEffect::Ok);
    }

    // Collect all lines.
    let all_lines: Vec<String> = (0..total)
        .map(|i| hjkl_buffer::rope_line_str(&rope, i))
        .collect();
    drop(rope);

    // Resolve the range (default: whole buffer).
    let (start_row, end_row) = match range {
        Some(r) => {
            let s = r.start_one_based().saturating_sub(1);
            let e = (r.end_one_based().saturating_sub(1)).min(total - 1);
            (s, e)
        }
        None => (0, total - 1),
    };

    if start_row > end_row {
        return Some(ExEffect::Ok);
    }

    // Convert lines in range.
    let mut new_lines: Vec<String> = all_lines.clone();
    let mut changed = false;
    for row in start_row..=end_row {
        let original = &all_lines[row];
        let converted = retab_line(original, tabstop, expandtab, bang);
        if converted != *original {
            new_lines[row] = converted;
            changed = true;
        }
    }

    if !changed {
        return Some(ExEffect::Ok);
    }

    editor.push_undo();
    editor.restore(new_lines, (start_row, 0));
    editor.mark_content_dirty();
    Some(ExEffect::Ok)
}

// ---- abbreviations ---------------------------------------------------------

/// Parse abbreviation args: `{lhs} {rhs}`.
///
/// The lhs is the first whitespace-delimited token.  The rhs is everything
/// after the first run of whitespace (preserving internal spaces).  A
/// `<buffer>` arg is silently stripped.
fn parse_abbrev_args(args: &str) -> Option<(String, String)> {
    let args = args.trim();
    // Strip leading `<buffer>` modifier (unsupported; ignore it).
    let args = if let Some(rest) = args.strip_prefix("<buffer>") {
        rest.trim_start()
    } else {
        args
    };
    let mut parts = args.splitn(2, [' ', '\t']);
    let lhs = parts.next()?.trim();
    if lhs.is_empty() {
        return None;
    }
    let rhs = parts.next().unwrap_or("").trim();
    Some((lhs.to_string(), rhs.to_string()))
}

// `:abbreviate {lhs} {rhs}` — define insert + cmdline abbreviation.
fn abbreviate_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    let Some((lhs, rhs)) = parse_abbrev_args(args) else {
        return Some(ExEffect::Ok); // no args → list (not implemented; no-op)
    };
    editor.add_abbrev(&lhs, &rhs, true, true, false);
    Some(ExEffect::Ok)
}

// `:iabbrev {lhs} {rhs}` — insert-mode only abbreviation.
fn iabbrev_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    let Some((lhs, rhs)) = parse_abbrev_args(args) else {
        return Some(ExEffect::Ok);
    };
    editor.add_abbrev(&lhs, &rhs, true, false, false);
    Some(ExEffect::Ok)
}

// `:cabbrev {lhs} {rhs}` — cmdline-mode only abbreviation (stored, not expanded).
fn cabbrev_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    let Some((lhs, rhs)) = parse_abbrev_args(args) else {
        return Some(ExEffect::Ok);
    };
    editor.add_abbrev(&lhs, &rhs, false, true, false);
    Some(ExEffect::Ok)
}

// `:noreabbrev {lhs} {rhs}` — non-recursive insert + cmdline abbreviation.
fn noreabbrev_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    let Some((lhs, rhs)) = parse_abbrev_args(args) else {
        return Some(ExEffect::Ok);
    };
    editor.add_abbrev(&lhs, &rhs, true, true, true);
    Some(ExEffect::Ok)
}

// `:inoreabbrev {lhs} {rhs}` — non-recursive insert-only abbreviation.
fn inoreabbrev_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    let Some((lhs, rhs)) = parse_abbrev_args(args) else {
        return Some(ExEffect::Ok);
    };
    editor.add_abbrev(&lhs, &rhs, true, false, true);
    Some(ExEffect::Ok)
}

// `:cnoreabbrev {lhs} {rhs}` — non-recursive cmdline-only abbreviation.
fn cnoreabbrev_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    let Some((lhs, rhs)) = parse_abbrev_args(args) else {
        return Some(ExEffect::Ok);
    };
    editor.add_abbrev(&lhs, &rhs, false, true, true);
    Some(ExEffect::Ok)
}

// `:unabbreviate {lhs}` — remove insert + cmdline abbreviation.
fn unabbreviate_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    let lhs = args.trim();
    if lhs.is_empty() {
        return Some(ExEffect::Ok);
    }
    editor.remove_abbrev(lhs, true, true);
    Some(ExEffect::Ok)
}

// `:iunabbrev {lhs}` — remove insert-only abbreviation.
fn iunabbrev_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    let lhs = args.trim();
    if lhs.is_empty() {
        return Some(ExEffect::Ok);
    }
    editor.remove_abbrev(lhs, true, false);
    Some(ExEffect::Ok)
}

// `:cunabbrev {lhs}` — remove cmdline-only abbreviation.
fn cunabbrev_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    let lhs = args.trim();
    if lhs.is_empty() {
        return Some(ExEffect::Ok);
    }
    editor.remove_abbrev(lhs, false, true);
    Some(ExEffect::Ok)
}

// `:abclear` — clear all insert + cmdline abbreviations.
fn abclear_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    editor.clear_abbrevs(true, true);
    Some(ExEffect::Ok)
}

// `:iabclear` — clear all insert-only abbreviations.
fn iabclear_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    editor.clear_abbrevs(true, false);
    Some(ExEffect::Ok)
}

// `:cabclear` — clear all cmdline-only abbreviations.
fn cabclear_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    editor.clear_abbrevs(false, true);
    Some(ExEffect::Ok)
}

// ---- unit tests ------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::range::LineRange;
    use hjkl_engine::{DefaultHost, Editor, Options};

    // ── helpers ──────────────────────────────────────────────────────────────

    fn make_editor() -> Editor<hjkl_buffer::View, DefaultHost> {
        let buf = hjkl_buffer::View::new();
        let host = DefaultHost::new();
        hjkl_vim::vim_editor(buf, host, Options::default())
    }

    fn make_editor_with_lines(lines: &[&str]) -> Editor<hjkl_buffer::View, DefaultHost> {
        let content = lines.join("\n");
        let buf = hjkl_buffer::View::from_str(&content);
        let host = DefaultHost::new();
        hjkl_vim::vim_editor(buf, host, Options::default())
    }

    fn buf_line(editor: &Editor<hjkl_buffer::View, DefaultHost>, row: usize) -> String {
        hjkl_buffer::rope_line_str(&editor.buffer().rope(), row)
    }

    fn buf_lines(editor: &Editor<hjkl_buffer::View, DefaultHost>) -> Vec<String> {
        let rope = editor.buffer().rope();
        (0..rope.len_lines())
            .map(|i| hjkl_buffer::rope_line_str(&rope, i))
            .collect()
    }

    // ── :> / :< shift commands ────────────────────────────────────────────────

    fn shift_editor(lines: &[&str]) -> Editor<hjkl_buffer::View, DefaultHost> {
        let mut ed = make_editor_with_lines(lines);
        ed.settings_mut().expandtab = true;
        ed.settings_mut().shiftwidth = 4;
        ed
    }

    fn dispatch(ed: &mut Editor<hjkl_buffer::View, DefaultHost>, input: &str) -> Option<ExEffect> {
        let mut reg = crate::registry::Registry::<DefaultHost>::new();
        register_builtins(&mut reg);
        crate::try_dispatch(&reg, ed, input)
    }

    #[test]
    fn shift_right_range_one_level() {
        let mut ed = shift_editor(&["a", "b", "c"]);
        assert_eq!(dispatch(&mut ed, "1,2>"), Some(ExEffect::Ok));
        assert_eq!(buf_lines(&ed), vec!["    a", "    b", "c"]);
    }

    #[test]
    fn shift_right_range_two_levels() {
        let mut ed = shift_editor(&["a", "b", "c"]);
        assert_eq!(dispatch(&mut ed, "1,2>>"), Some(ExEffect::Ok));
        assert_eq!(buf_lines(&ed), vec!["        a", "        b", "c"]);
    }

    #[test]
    fn shift_right_whole_buffer() {
        let mut ed = shift_editor(&["a", "b", "c"]);
        assert_eq!(dispatch(&mut ed, "%>"), Some(ExEffect::Ok));
        assert_eq!(buf_lines(&ed), vec!["    a", "    b", "    c"]);
    }

    #[test]
    fn shift_left_range_outdents() {
        let mut ed = shift_editor(&["    a", "    b", "    c"]);
        assert_eq!(dispatch(&mut ed, "1,3<"), Some(ExEffect::Ok));
        assert_eq!(buf_lines(&ed), vec!["a", "b", "c"]);
    }

    #[test]
    fn shift_no_range_uses_current_line() {
        let mut ed = shift_editor(&["a", "b", "c"]);
        ed.jump_cursor(1, 0); // cursor on line 2 (row 1)
        assert_eq!(dispatch(&mut ed, ">"), Some(ExEffect::Ok));
        assert_eq!(buf_lines(&ed), vec!["a", "    b", "c"]);
    }

    #[test]
    fn shift_trailing_count_is_line_count() {
        // `:1> 2` shifts 2 lines starting at the range's last line (line 1).
        let mut ed = shift_editor(&["a", "b", "c"]);
        assert_eq!(dispatch(&mut ed, "1> 2"), Some(ExEffect::Ok));
        assert_eq!(buf_lines(&ed), vec!["    a", "    b", "c"]);
    }

    #[test]
    fn shift_trailing_garbage_errors() {
        let mut ed = shift_editor(&["a", "b"]);
        assert!(matches!(dispatch(&mut ed, "1>x"), Some(ExEffect::Error(_))));
    }

    #[test]
    fn shift_huge_trailing_count_does_not_overflow() {
        // Regression: `start_row + n - 1` overflowed usize when the trailing
        // count was near usize::MAX and the range started past line 1.
        let mut ed = shift_editor(&["a", "b", "c"]);
        let cmd = format!("2> {}", usize::MAX);
        assert_eq!(dispatch(&mut ed, &cmd), Some(ExEffect::Ok));
        // Count clamps to the buffer end: lines 2..=3 shifted, line 1 untouched.
        assert_eq!(buf_lines(&ed), vec!["a", "    b", "    c"]);
    }

    // ── quit_handler ─────────────────────────────────────────────────────────

    #[test]
    fn quit_handler_returns_quit_no_force_no_save() {
        let mut ed = make_editor();
        let result = quit_handler(&mut ed, "", None);
        assert_eq!(
            result,
            Some(ExEffect::Quit {
                force: false,
                save: false
            })
        );
    }

    // ── quit_force_handler ───────────────────────────────────────────────────

    #[test]
    fn quit_force_handler_returns_quit_force_no_save() {
        let mut ed = make_editor();
        let result = quit_force_handler(&mut ed, "", None);
        assert_eq!(
            result,
            Some(ExEffect::Quit {
                force: true,
                save: false
            })
        );
    }

    // ── write_handler ────────────────────────────────────────────────────────

    #[test]
    fn write_handler_no_args_returns_save() {
        let mut ed = make_editor();
        let result = write_handler(&mut ed, "", None);
        assert_eq!(result, Some(ExEffect::Save));
    }

    #[test]
    fn write_handler_with_path_returns_save_as() {
        let mut ed = make_editor();
        let result = write_handler(&mut ed, "  /tmp/test.txt  ", None);
        assert_eq!(result, Some(ExEffect::SaveAs("/tmp/test.txt".to_string())));
    }

    #[test]
    fn write_handler_whitespace_only_returns_save() {
        let mut ed = make_editor();
        let result = write_handler(&mut ed, "   ", None);
        // trim() → empty → Save
        assert_eq!(result, Some(ExEffect::Save));
    }

    // ── wall_handler ─────────────────────────────────────────────────────────

    #[test]
    fn wall_handler_returns_save() {
        let mut ed = make_editor();
        let result = wall_handler(&mut ed, "", None);
        assert_eq!(result, Some(ExEffect::Save));
    }

    // ── edit_handler ─────────────────────────────────────────────────────────

    #[test]
    fn edit_handler_with_path_returns_edit_file_no_force() {
        let mut ed = make_editor();
        let result = edit_handler(&mut ed, "  foo.txt  ", None);
        assert_eq!(
            result,
            Some(ExEffect::EditFile {
                path: "foo.txt".to_string(),
                force: false,
            })
        );
    }

    #[test]
    fn edit_force_handler_sets_force_flag() {
        let mut ed = make_editor();
        let result = edit_force_handler(&mut ed, "bar.txt", None);
        assert_eq!(
            result,
            Some(ExEffect::EditFile {
                path: "bar.txt".to_string(),
                force: true,
            })
        );
    }

    // ── bdelete / bwipeout ───────────────────────────────────────────────────

    #[test]
    fn bdelete_handler_returns_buffer_delete_no_force_no_wipe() {
        let mut ed = make_editor();
        let result = bdelete_handler(&mut ed, "", None);
        assert_eq!(
            result,
            Some(ExEffect::BufferDelete {
                force: false,
                wipe: false,
            })
        );
    }

    #[test]
    fn bdelete_force_handler_returns_buffer_delete_force_no_wipe() {
        let mut ed = make_editor();
        let result = bdelete_force_handler(&mut ed, "", None);
        assert_eq!(
            result,
            Some(ExEffect::BufferDelete {
                force: true,
                wipe: false,
            })
        );
    }

    #[test]
    fn bwipeout_handler_returns_buffer_delete_no_force_wipe() {
        let mut ed = make_editor();
        let result = bwipeout_handler(&mut ed, "", None);
        assert_eq!(
            result,
            Some(ExEffect::BufferDelete {
                force: false,
                wipe: true,
            })
        );
    }

    #[test]
    fn bwipeout_force_handler_returns_buffer_delete_force_wipe() {
        let mut ed = make_editor();
        let result = bwipeout_force_handler(&mut ed, "", None);
        assert_eq!(
            result,
            Some(ExEffect::BufferDelete {
                force: true,
                wipe: true,
            })
        );
    }

    // bdelete vs bwipeout — wipe flag must differ
    #[test]
    fn bdelete_and_bwipeout_differ_only_in_wipe_flag() {
        let mut ed = make_editor();
        let bd = bdelete_handler(&mut ed, "", None);
        let bw = bwipeout_handler(&mut ed, "", None);
        match (bd, bw) {
            (
                Some(ExEffect::BufferDelete {
                    wipe: bd_wipe,
                    force: bd_force,
                }),
                Some(ExEffect::BufferDelete {
                    wipe: bw_wipe,
                    force: bw_force,
                }),
            ) => {
                assert!(!bd_wipe, "bdelete wipe must be false");
                assert!(bw_wipe, "bwipeout wipe must be true");
                assert_eq!(bd_force, bw_force, "force flag should match");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    // ── wq_handler / wq_force_handler ────────────────────────────────────────

    #[test]
    fn wq_handler_returns_quit_save_no_force() {
        let mut ed = make_editor();
        let result = wq_handler(&mut ed, "", None);
        assert_eq!(
            result,
            Some(ExEffect::Quit {
                force: false,
                save: true,
            })
        );
    }

    #[test]
    fn wq_force_handler_returns_quit_save_force() {
        let mut ed = make_editor();
        let result = wq_force_handler(&mut ed, "", None);
        assert_eq!(
            result,
            Some(ExEffect::Quit {
                force: true,
                save: true,
            })
        );
    }

    // ── wqall / qall / qall_force ────────────────────────────────────────────

    #[test]
    fn wqall_handler_returns_quit_save_no_force() {
        let mut ed = make_editor();
        let result = wqall_handler(&mut ed, "", None);
        assert_eq!(
            result,
            Some(ExEffect::Quit {
                force: false,
                save: true,
            })
        );
    }

    // ── xa / xall dispatch ───────────────────────────────────────────────────

    #[test]
    fn dispatch_xa_returns_write_quit_all() {
        let mut reg = crate::registry::Registry::<hjkl_engine::DefaultHost>::new();
        register_builtins(&mut reg);
        let mut ed = make_editor();
        let cmd = reg.resolve("xa").expect(":xa must resolve");
        let result = (cmd.run)(&mut ed, "", None);
        assert_eq!(
            result,
            Some(ExEffect::Quit {
                force: false,
                save: true,
            })
        );
    }

    #[test]
    fn dispatch_xall_returns_write_quit_all() {
        let mut reg = crate::registry::Registry::<hjkl_engine::DefaultHost>::new();
        register_builtins(&mut reg);
        let mut ed = make_editor();
        let cmd = reg.resolve("xall").expect(":xall must resolve");
        let result = (cmd.run)(&mut ed, "", None);
        assert_eq!(
            result,
            Some(ExEffect::Quit {
                force: false,
                save: true,
            })
        );
    }

    #[test]
    fn dispatch_xa_force_returns_write_quit_all_force() {
        // :xa! mirrors :wqa! — the registry maps both to wqall_handler which
        // returns force=false (same as :wqa! by design; the ! means "force
        // write errors", not the Quit::force flag which controls unsaved-buffer
        // checking).
        let mut reg = crate::registry::Registry::<hjkl_engine::DefaultHost>::new();
        register_builtins(&mut reg);
        let mut ed = make_editor();
        let cmd = reg.resolve("xa!").expect(":xa! must resolve");
        let result = (cmd.run)(&mut ed, "", None);
        assert_eq!(
            result,
            Some(ExEffect::Quit {
                force: false,
                save: true,
            })
        );
    }

    #[test]
    fn dispatch_xa_matches_wqa_effect() {
        // Verify :xa and :wqa resolve to handlers that return identical ExEffect.
        let mut reg = crate::registry::Registry::<hjkl_engine::DefaultHost>::new();
        register_builtins(&mut reg);
        let mut ed = make_editor();
        let xa_cmd = reg.resolve("xa").expect(":xa must resolve");
        let wqa_cmd = reg.resolve("wqa").expect(":wqa must resolve");
        let xa_result = (xa_cmd.run)(&mut ed, "", None);
        let wqa_result = (wqa_cmd.run)(&mut ed, "", None);
        assert_eq!(xa_result, wqa_result);
    }

    #[test]
    fn dispatch_xa_force_matches_wqa_force_effect() {
        let mut reg = crate::registry::Registry::<hjkl_engine::DefaultHost>::new();
        register_builtins(&mut reg);
        let mut ed = make_editor();
        let xa_cmd = reg.resolve("xa!").expect(":xa! must resolve");
        let wqa_cmd = reg.resolve("wqa!").expect(":wqa! must resolve");
        let xa_result = (xa_cmd.run)(&mut ed, "", None);
        let wqa_result = (wqa_cmd.run)(&mut ed, "", None);
        assert_eq!(xa_result, wqa_result);
    }

    #[test]
    fn qall_handler_returns_quit_no_save_no_force() {
        let mut ed = make_editor();
        let result = qall_handler(&mut ed, "", None);
        assert_eq!(
            result,
            Some(ExEffect::Quit {
                force: false,
                save: false,
            })
        );
    }

    #[test]
    fn qall_force_handler_returns_quit_force_no_save() {
        let mut ed = make_editor();
        let result = qall_force_handler(&mut ed, "", None);
        assert_eq!(
            result,
            Some(ExEffect::Quit {
                force: true,
                save: false,
            })
        );
    }

    // ── nohlsearch_handler ───────────────────────────────────────────────────

    #[test]
    fn nohlsearch_clears_active_search_pattern() {
        let mut ed = make_editor();
        // Install a pattern then clear it.
        ed.set_search_pattern(Some(regex::Regex::new("foo").unwrap()));
        assert!(
            ed.search_state().pattern.is_some(),
            "setup: pattern must be set"
        );
        let result = nohlsearch_handler(&mut ed, "", None);
        assert_eq!(result, Some(ExEffect::Ok));
        assert!(
            ed.search_state().pattern.is_none(),
            "pattern should be cleared"
        );
    }

    #[test]
    fn nohlsearch_on_already_clear_returns_ok() {
        let mut ed = make_editor();
        // Pattern is None by default.
        let result = nohlsearch_handler(&mut ed, "", None);
        assert_eq!(result, Some(ExEffect::Ok));
        assert!(ed.search_state().pattern.is_none());
    }

    // ── undo / redo ──────────────────────────────────────────────────────────

    #[test]
    fn undo_handler_returns_ok() {
        let mut ed = make_editor();
        let result = undo_handler(&mut ed, "", None);
        assert_eq!(result, Some(ExEffect::Ok));
    }

    #[test]
    fn redo_handler_returns_ok() {
        let mut ed = make_editor();
        let result = redo_handler(&mut ed, "", None);
        assert_eq!(result, Some(ExEffect::Ok));
    }

    // ── earlier / later ──────────────────────────────────────────────────────

    #[test]
    fn earlier_no_arg_undoes_one_step() {
        let mut ed = make_editor();
        ed.push_undo();
        ed.push_undo();
        let before = ed.undo_stack_len();
        let result = earlier_handler(&mut ed, "", None);
        assert!(
            matches!(result, Some(ExEffect::Info(_))),
            "expected Info, got {result:?}"
        );
        assert_eq!(ed.undo_stack_len(), before - 1);
    }

    #[test]
    fn earlier_numeric_arg_undoes_n_steps() {
        let mut ed = make_editor();
        ed.push_undo();
        ed.push_undo();
        ed.push_undo();
        let result = earlier_handler(&mut ed, "2", None);
        assert!(
            matches!(result, Some(ExEffect::Info(_))),
            "expected Info, got {result:?}"
        );
        assert_eq!(ed.undo_stack_len(), 1);
    }

    #[test]
    fn earlier_5s_passes_5_second_duration() {
        // Parses without error; no undo entries present so 0 steps applied.
        let mut ed = make_editor();
        let result = earlier_handler(&mut ed, "5s", None);
        assert!(
            matches!(result, Some(ExEffect::Info(_))),
            "expected Info, got {result:?}"
        );
    }

    #[test]
    fn earlier_2m_passes_120_seconds() {
        let mut ed = make_editor();
        let result = earlier_handler(&mut ed, "2m", None);
        assert!(
            matches!(result, Some(ExEffect::Info(_))),
            "expected Info, got {result:?}"
        );
    }

    #[test]
    fn earlier_3h_passes_3_hours() {
        let mut ed = make_editor();
        let result = earlier_handler(&mut ed, "3h", None);
        assert!(
            matches!(result, Some(ExEffect::Info(_))),
            "expected Info, got {result:?}"
        );
    }

    #[test]
    fn earlier_bad_arg_returns_error() {
        let mut ed = make_editor();
        let result = earlier_handler(&mut ed, "xyz", None);
        assert!(
            matches!(result, Some(ExEffect::Error(_))),
            "expected Error, got {result:?}"
        );
    }

    #[test]
    fn later_no_arg_redoes_one_step() {
        let mut ed = make_editor();
        ed.push_undo();
        // Undo so there's something to redo.
        ed.undo();
        let result = later_handler(&mut ed, "", None);
        assert!(
            matches!(result, Some(ExEffect::Info(_))),
            "expected Info, got {result:?}"
        );
    }

    #[test]
    fn later_numeric_arg_redoes_n_steps() {
        let mut ed = make_editor();
        ed.push_undo();
        ed.push_undo();
        ed.push_undo();
        ed.earlier_by_steps(3);
        let result = later_handler(&mut ed, "2", None);
        assert!(
            matches!(result, Some(ExEffect::Info(_))),
            "expected Info, got {result:?}"
        );
        assert_eq!(ed.undo_stack_len(), 2);
    }

    #[test]
    fn later_5s_passes_5_second_duration() {
        let mut ed = make_editor();
        let result = later_handler(&mut ed, "5s", None);
        assert!(
            matches!(result, Some(ExEffect::Info(_))),
            "expected Info, got {result:?}"
        );
    }

    #[test]
    fn later_bad_arg_returns_error() {
        let mut ed = make_editor();
        let result = later_handler(&mut ed, "abc", None);
        assert!(
            matches!(result, Some(ExEffect::Error(_))),
            "expected Error, got {result:?}"
        );
    }

    #[test]
    fn earlier_dispatch_resolves_min_prefix_2() {
        let reg = crate::default_registry::<hjkl_engine::DefaultHost>();
        assert!(reg.resolve("ea").is_some(), ":ea must resolve to :earlier");
        assert_eq!(reg.resolve("ea").unwrap().name, "earlier");
        assert!(reg.resolve("earlier").is_some(), ":earlier must resolve");
    }

    #[test]
    fn later_dispatch_resolves_min_prefix_3() {
        let reg = crate::default_registry::<hjkl_engine::DefaultHost>();
        assert!(reg.resolve("lat").is_some(), ":lat must resolve to :later");
        assert_eq!(reg.resolve("lat").unwrap().name, "later");
        assert!(reg.resolve("later").is_some(), ":later must resolve");
    }

    // ── registers / marks / jumps / changes ──────────────────────────────────

    #[test]
    fn registers_handler_returns_info_titled() {
        let mut ed = make_editor();
        let result = registers_handler(&mut ed, "", None);
        match result {
            Some(ExEffect::InfoTitled { title, content }) => {
                assert_eq!(title, "registers");
                assert!(
                    content.contains("Registers"),
                    "expected Registers header, got: {content}"
                );
            }
            other => panic!("expected InfoTitled, got {other:?}"),
        }
    }

    #[test]
    fn marks_handler_returns_info_titled() {
        let mut ed = make_editor();
        let result = marks_handler(&mut ed, "", None);
        match result {
            Some(ExEffect::InfoTitled { title, content }) => {
                assert_eq!(title, "marks");
                assert!(
                    content.contains("Marks"),
                    "expected Marks header, got: {content}"
                );
            }
            other => panic!("expected InfoTitled, got {other:?}"),
        }
    }

    #[test]
    fn jumps_handler_returns_info_titled() {
        let mut ed = make_editor();
        let result = jumps_handler(&mut ed, "", None);
        // Empty jump list → "(no jumps recorded)" as Info (single line), not InfoTitled.
        match result {
            Some(ExEffect::InfoTitled { title, content }) => {
                assert_eq!(title, "jumps");
                assert!(
                    content.contains("jump") || content.contains("no jumps"),
                    "unexpected content: {content}"
                );
            }
            other => panic!("expected InfoTitled, got {other:?}"),
        }
    }

    #[test]
    fn changes_handler_returns_info_titled() {
        let mut ed = make_editor();
        let result = changes_handler(&mut ed, "", None);
        match result {
            Some(ExEffect::InfoTitled { title, content }) => {
                assert_eq!(title, "changes");
                assert!(
                    content.contains("change") || content.contains("no changes"),
                    "unexpected content: {content}"
                );
            }
            other => panic!("expected InfoTitled, got {other:?}"),
        }
    }

    // ── delete_handler ───────────────────────────────────────────────────────

    #[test]
    fn delete_handler_default_range_deletes_cursor_line() {
        let mut ed = make_editor_with_lines(&["aaa", "bbb", "ccc"]);
        // Cursor at row 0 (default). Deletes line 1 (1-based).
        let result = delete_handler(&mut ed, "", None);
        assert_eq!(result, Some(ExEffect::Ok));
        let lines = buf_lines(&ed);
        assert!(
            !lines.contains(&"aaa".to_string()),
            "first line should be deleted: {lines:?}"
        );
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn delete_handler_explicit_range_deletes_lines() {
        let mut ed = make_editor_with_lines(&["l1", "l2", "l3", "l4"]);
        let range = LineRange::new(2, 3);
        let result = delete_handler(&mut ed, "", Some(range));
        assert_eq!(result, Some(ExEffect::Ok));
        let lines = buf_lines(&ed);
        assert_eq!(lines.len(), 2, "expected 2 remaining lines: {lines:?}");
        assert!(lines.contains(&"l1".to_string()));
        assert!(lines.contains(&"l4".to_string()));
    }

    #[test]
    fn delete_handler_single_line_buffer_clears_content() {
        let mut ed = make_editor_with_lines(&["only line"]);
        let result = delete_handler(&mut ed, "", None);
        assert_eq!(result, Some(ExEffect::Ok));
        // View keeps one empty line rather than zero rows.
        assert_eq!(ed.buffer().row_count(), 1);
        assert_eq!(buf_line(&ed, 0), "");
    }

    #[test]
    fn delete_handler_empty_buffer_returns_ok() {
        let mut ed = make_editor();
        let result = delete_handler(&mut ed, "", None);
        assert_eq!(result, Some(ExEffect::Ok));
    }

    // ── sort_handler ─────────────────────────────────────────────────────────

    #[test]
    fn sort_handler_basic_alphabetical() {
        let mut ed = make_editor_with_lines(&["banana", "apple", "cherry"]);
        let result = sort_handler(&mut ed, "", None);
        assert_eq!(result, Some(ExEffect::Ok));
        let lines = buf_lines(&ed);
        assert_eq!(lines, vec!["apple", "banana", "cherry"]);
    }

    #[test]
    fn sort_handler_reverse_flag() {
        let mut ed = make_editor_with_lines(&["banana", "apple", "cherry"]);
        let result = sort_handler(&mut ed, "!", None);
        assert_eq!(result, Some(ExEffect::Ok));
        let lines = buf_lines(&ed);
        assert_eq!(lines, vec!["cherry", "banana", "apple"]);
    }

    #[test]
    fn sort_handler_unique_flag_removes_duplicates() {
        let mut ed = make_editor_with_lines(&["b", "a", "b", "c", "a"]);
        let result = sort_handler(&mut ed, "u", None);
        assert_eq!(result, Some(ExEffect::Ok));
        let lines = buf_lines(&ed);
        // sorted + unique: a, b, c
        assert_eq!(lines, vec!["a", "b", "c"]);
    }

    #[test]
    fn sort_handler_ignore_case_flag() {
        let mut ed = make_editor_with_lines(&["Banana", "apple", "Cherry"]);
        let result = sort_handler(&mut ed, "i", None);
        assert_eq!(result, Some(ExEffect::Ok));
        let lines = buf_lines(&ed);
        // case-insensitive: apple < Banana < Cherry
        let lower: Vec<String> = lines.iter().map(|s| s.to_lowercase()).collect();
        assert_eq!(lower, vec!["apple", "banana", "cherry"]);
    }

    #[test]
    fn sort_handler_numeric_flag() {
        let mut ed = make_editor_with_lines(&["10 items", "2 things", "20 stuff"]);
        let result = sort_handler(&mut ed, "n", None);
        assert_eq!(result, Some(ExEffect::Ok));
        let lines = buf_lines(&ed);
        assert_eq!(lines[0], "2 things");
        assert_eq!(lines[1], "10 items");
        assert_eq!(lines[2], "20 stuff");
    }

    #[test]
    fn sort_handler_bad_flag_returns_error() {
        let mut ed = make_editor_with_lines(&["a", "b"]);
        let result = sort_handler(&mut ed, "z", None);
        assert!(
            matches!(result, Some(ExEffect::Error(_))),
            "got: {result:?}"
        );
    }

    #[test]
    fn sort_handler_range_sorts_only_slice() {
        let mut ed = make_editor_with_lines(&["zzz", "banana", "apple", "aaa"]);
        // Sort lines 2-3 (1-based): "banana","apple" → "apple","banana"
        let range = LineRange::new(2, 3);
        let result = sort_handler(&mut ed, "", Some(range));
        assert_eq!(result, Some(ExEffect::Ok));
        let lines = buf_lines(&ed);
        assert_eq!(lines[0], "zzz", "line 1 untouched");
        assert_eq!(lines[1], "apple");
        assert_eq!(lines[2], "banana");
        assert_eq!(lines[3], "aaa", "line 4 untouched");
    }

    // ── extract_leading_number (helper) ──────────────────────────────────────

    #[test]
    fn extract_leading_number_positive() {
        assert_eq!(extract_leading_number("42 items"), 42);
    }

    #[test]
    fn extract_leading_number_negative() {
        assert_eq!(extract_leading_number("-5 below zero"), -5);
    }

    #[test]
    fn extract_leading_number_no_number_returns_min() {
        assert_eq!(extract_leading_number("no numbers here"), i64::MIN);
    }

    #[test]
    fn extract_leading_number_bare_minus_returns_min() {
        // "-" with no digits after is not a valid number
        assert_eq!(extract_leading_number("-"), i64::MIN);
    }

    // ── substitute_handler ───────────────────────────────────────────────────

    #[test]
    fn substitute_simple_replace() {
        let mut ed = make_editor_with_lines(&["hello world"]);
        let result = substitute_handler(&mut ed, "/hello/goodbye", None);
        match result {
            Some(ExEffect::Substituted { count, .. }) => {
                assert_eq!(count, 1);
            }
            other => panic!("expected Substituted, got {other:?}"),
        }
        let line = buf_line(&ed, 0);
        assert!(line.contains("goodbye"), "line: {line}");
    }

    #[test]
    fn substitute_global_flag_replaces_all() {
        let mut ed = make_editor_with_lines(&["aXbXcX"]);
        let result = substitute_handler(&mut ed, "/X/Y/g", None);
        match result {
            Some(ExEffect::Substituted { count, .. }) => {
                assert_eq!(count, 3);
            }
            other => panic!("expected Substituted, got {other:?}"),
        }
        assert_eq!(buf_line(&ed, 0), "aYbYcY");
    }

    #[test]
    fn substitute_ignore_case_flag() {
        let mut ed = make_editor_with_lines(&["Hello World"]);
        let result = substitute_handler(&mut ed, "/hello/bye/i", None);
        match result {
            Some(ExEffect::Substituted { count, .. }) => assert!(count >= 1),
            other => panic!("expected Substituted, got {other:?}"),
        }
    }

    #[test]
    fn substitute_no_match_returns_substituted_zero() {
        let mut ed = make_editor_with_lines(&["no match here"]);
        let result = substitute_handler(&mut ed, "/zzz/yyy", None);
        match result {
            Some(ExEffect::Substituted {
                count,
                lines_changed,
            }) => {
                assert_eq!(count, 0);
                assert_eq!(lines_changed, 0);
            }
            other => panic!("expected Substituted, got {other:?}"),
        }
    }

    #[test]
    fn substitute_bad_pattern_returns_error() {
        let mut ed = make_editor_with_lines(&["text"]);
        let result = substitute_handler(&mut ed, "/[bad/ok", None);
        assert!(
            matches!(result, Some(ExEffect::Error(_))),
            "got: {result:?}"
        );
    }

    #[test]
    fn substitute_range_limits_scope() {
        let mut ed = make_editor_with_lines(&["foo", "foo", "foo"]);
        // Only substitute on line 2 (1-based).
        let range = LineRange::new(2, 2);
        let result = substitute_handler(&mut ed, "/foo/bar", Some(range));
        match result {
            Some(ExEffect::Substituted { count, .. }) => assert_eq!(count, 1),
            other => panic!("expected Substituted, got {other:?}"),
        }
        assert_eq!(buf_line(&ed, 0), "foo", "line 1 untouched");
        assert_eq!(buf_line(&ed, 1), "bar", "line 2 changed");
        assert_eq!(buf_line(&ed, 2), "foo", "line 3 untouched");
    }

    // ── read_handler ─────────────────────────────────────────────────────────

    #[test]
    fn read_handler_empty_args_returns_none() {
        let mut ed = make_editor_with_lines(&["first"]);
        let result = read_handler(&mut ed, "   ", None);
        assert!(
            result.is_none(),
            "empty args should return None, got {result:?}"
        );
    }

    #[test]
    fn read_handler_missing_file_returns_error() {
        let mut ed = make_editor_with_lines(&["first"]);
        let result = read_handler(&mut ed, "/nonexistent/path/that/does/not/exist.txt", None);
        assert!(
            matches!(result, Some(ExEffect::Error(_))),
            "got: {result:?}"
        );
    }

    #[test]
    fn read_handler_file_inserts_content_below_cursor() {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "inserted line").unwrap();
        let path = tmp.path().to_str().unwrap().to_string();

        let mut ed = make_editor_with_lines(&["first"]);
        let result = read_handler(&mut ed, &path, None);
        assert_eq!(result, Some(ExEffect::Ok));
        let lines = buf_lines(&ed);
        assert!(
            lines.contains(&"inserted line".to_string()),
            "lines: {lines:?}"
        );
    }

    #[test]
    fn read_handler_shell_cmd_success_inserts_output() {
        let mut ed = make_editor_with_lines(&["first"]);
        let result = read_handler(&mut ed, "!echo hello", None);
        assert_eq!(result, Some(ExEffect::Ok));
        let lines = buf_lines(&ed);
        assert!(lines.contains(&"hello".to_string()), "lines: {lines:?}");
    }

    #[test]
    fn read_handler_shell_cmd_nonzero_exit_returns_error() {
        let mut ed = make_editor_with_lines(&["first"]);
        // `false` always exits 1
        let result = read_handler(&mut ed, "!false", None);
        assert!(
            matches!(result, Some(ExEffect::Error(_))),
            "got: {result:?}"
        );
    }

    #[test]
    fn read_handler_shell_cmd_stderr_included_in_error() {
        let mut ed = make_editor_with_lines(&["first"]);
        // Output to stderr then exit non-zero so we can observe it in the error
        let result = read_handler(&mut ed, "!sh -c 'echo boom >&2; exit 1'", None);
        match result {
            Some(ExEffect::Error(msg)) => {
                assert!(msg.contains("boom"), "expected stderr in error, got: {msg}");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn read_handler_empty_shell_cmd_returns_error() {
        let mut ed = make_editor();
        let result = read_handler(&mut ed, "! ", None);
        match result {
            Some(ExEffect::Error(msg)) => {
                assert!(
                    msg.contains("needs a shell command"),
                    "unexpected error: {msg}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    // ── set_handler (smoke — deep coverage is in setopt.rs) ──────────────────

    #[test]
    fn set_handler_bare_returns_info() {
        let mut ed = make_editor();
        let result = set_handler(&mut ed, "", None);
        assert!(matches!(result, Some(ExEffect::Info(_))), "got: {result:?}");
    }

    #[test]
    fn set_handler_known_option_returns_ok() {
        let mut ed = make_editor();
        let result = set_handler(&mut ed, "number", None);
        assert_eq!(result, Some(ExEffect::Ok));
        assert!(ed.settings().number);
    }

    #[test]
    fn set_handler_unknown_option_returns_error() {
        let mut ed = make_editor();
        let result = set_handler(&mut ed, "nosuchoption", None);
        assert!(
            matches!(result, Some(ExEffect::Error(_))),
            "got: {result:?}"
        );
    }

    // ── :syntax (engine handler is a no-op; app overrides via host registry) ──

    #[test]
    fn syntax_handler_on_returns_ok() {
        let mut ed = make_editor();
        let result = syntax_handler(&mut ed, "on", None);
        assert_eq!(result, Some(ExEffect::Ok));
    }

    #[test]
    fn syntax_handler_off_returns_ok() {
        let mut ed = make_editor();
        let result = syntax_handler(&mut ed, "off", None);
        assert_eq!(result, Some(ExEffect::Ok));
    }

    #[test]
    fn syntax_handler_unknown_arg_returns_ok() {
        let mut ed = make_editor();
        let result = syntax_handler(&mut ed, "sync", None);
        assert_eq!(result, Some(ExEffect::Ok));
    }

    #[test]
    fn syntax_resolves_via_prefix_syn() {
        let reg = crate::default_registry::<hjkl_engine::DefaultHost>();
        assert!(reg.resolve("syn").is_some(), ":syn must resolve");
        assert!(reg.resolve("syntax").is_some(), ":syntax must resolve");
        // Below min_prefix=3: must NOT resolve to syntax via prefix path.
        // (`:s` correctly resolves to `substitute` instead.)
        let sy = reg.resolve("sy");
        assert!(
            sy.map(|c| c.name != "syntax").unwrap_or(true),
            "`:sy` must not resolve to syntax (min_prefix=3)"
        );
    }

    // ── register_builtins registry smoke ─────────────────────────────────────

    #[test]
    fn register_builtins_populates_quit_and_write() {
        let mut reg = crate::registry::Registry::<DefaultHost>::new();
        register_builtins(&mut reg);
        assert!(reg.resolve("quit").is_some());
        assert!(reg.resolve("q").is_some());
        assert!(reg.resolve("write").is_some());
        assert!(reg.resolve("w").is_some());
        assert!(reg.resolve("bdelete").is_some());
        assert!(reg.resolve("bd").is_some());
        assert!(reg.resolve("bwipeout").is_some());
        assert!(reg.resolve("bw").is_some());
        assert!(reg.resolve("substitute").is_some());
        assert!(reg.resolve("sort").is_some());
        assert!(reg.resolve("set").is_some());
        assert!(reg.resolve("nohlsearch").is_some());
        assert!(reg.resolve("noh").is_some());
    }

    // ── put_handler ──────────────────────────────────────────────────────────

    #[test]
    fn put_handler_no_args_uses_unnamed_register() {
        let mut ed = make_editor();
        let result = put_handler(&mut ed, "", None);
        assert_eq!(
            result,
            Some(ExEffect::PutRegister {
                reg: '"',
                above: false
            })
        );
    }

    #[test]
    fn put_handler_with_reg_uses_given_register() {
        let mut ed = make_editor();
        let result = put_handler(&mut ed, "a", None);
        assert_eq!(
            result,
            Some(ExEffect::PutRegister {
                reg: 'a',
                above: false
            })
        );
    }

    #[test]
    fn put_above_handler_no_args_uses_unnamed_register() {
        let mut ed = make_editor();
        let result = put_above_handler(&mut ed, "", None);
        assert_eq!(
            result,
            Some(ExEffect::PutRegister {
                reg: '"',
                above: true
            })
        );
    }

    #[test]
    fn put_handler_percent_register() {
        let mut ed = make_editor();
        let result = put_handler(&mut ed, "%", None);
        assert_eq!(
            result,
            Some(ExEffect::PutRegister {
                reg: '%',
                above: false
            })
        );
    }

    // ── cd_handler / pwd_handler ─────────────────────────────────────────────

    #[test]
    fn cd_handler_valid_dir_returns_cwd() {
        let mut ed = make_editor();
        let tmp = std::env::temp_dir();
        let result = cd_handler(&mut ed, &tmp.to_string_lossy(), None);
        match result {
            Some(ExEffect::Cwd(path)) => {
                assert!(!path.is_empty(), "Cwd path must not be empty");
            }
            other => panic!("expected Cwd, got {other:?}"),
        }
    }

    #[test]
    fn cd_handler_invalid_dir_returns_error() {
        let mut ed = make_editor();
        let bogus = std::env::temp_dir().join("nonexistent_hjkl_test_dir_xyz");
        let result = cd_handler(&mut ed, &bogus.to_string_lossy(), None);
        assert!(
            matches!(result, Some(ExEffect::Error(_))),
            "expected Error, got {result:?}"
        );
    }

    #[test]
    fn pwd_handler_returns_info() {
        let mut ed = make_editor();
        let result = pwd_handler(&mut ed, "", None);
        assert!(
            matches!(result, Some(ExEffect::Info(_))),
            "expected Info, got {result:?}"
        );
    }

    // ── repeat_substitute_handler (:& / :&&) ─────────────────────────────────

    #[test]
    fn repeat_substitute_no_prior_returns_error() {
        let mut ed = make_editor_with_lines(&["foo"]);
        let result = repeat_substitute_handler(&mut ed, false, None);
        assert!(
            matches!(result, ExEffect::Error(_)),
            "expected Error, got {result:?}"
        );
    }

    #[test]
    fn repeat_substitute_repeats_on_current_line() {
        let mut ed = make_editor_with_lines(&["foo", "foo"]);
        substitute_handler(&mut ed, "/foo/bar", None);
        assert_eq!(buf_line(&ed, 0), "bar");
        ed.goto_line(2);
        let result = repeat_substitute_handler(&mut ed, false, None);
        assert!(
            matches!(result, ExEffect::Substituted { count: 1, .. }),
            "expected Substituted(1), got {result:?}"
        );
        assert_eq!(buf_line(&ed, 1), "bar");
    }

    #[test]
    fn repeat_substitute_amp_amp_keeps_global_flag() {
        let mut ed = make_editor_with_lines(&["x x x", "x x x"]);
        substitute_handler(&mut ed, "/x/y/g", None);
        assert_eq!(buf_line(&ed, 0), "y y y");
        ed.goto_line(2);
        let result = repeat_substitute_handler(&mut ed, true, None);
        assert!(
            matches!(result, ExEffect::Substituted { count: 3, .. }),
            "expected Substituted(3), got {result:?}"
        );
        assert_eq!(buf_line(&ed, 1), "y y y");
    }

    #[test]
    fn repeat_substitute_amp_drops_global_flag() {
        let mut ed = make_editor_with_lines(&["x x x", "x x x"]);
        substitute_handler(&mut ed, "/x/y/g", None);
        assert_eq!(buf_line(&ed, 0), "y y y");
        ed.goto_line(2);
        let result = repeat_substitute_handler(&mut ed, false, None);
        assert!(
            matches!(result, ExEffect::Substituted { count: 1, .. }),
            "expected Substituted(1) (first only), got {result:?}"
        );
        assert_eq!(buf_line(&ed, 1), "y x x");
    }

    // ── comment_handler / uncomment_handler (#187) ────────────────────────────

    fn make_rust_editor() -> Editor<hjkl_buffer::View, DefaultHost> {
        let buf = hjkl_buffer::View::from_str("let a = 1;\nlet b = 2;\nlet c = 3;");
        let host = DefaultHost::new();
        let opts = Options {
            filetype: "rust".to_string(),
            ..Options::default()
        };
        hjkl_vim::vim_editor(buf, host, opts)
    }

    #[test]
    fn comment_handler_gcc_toggles_current_line() {
        let mut ed = make_rust_editor();
        // No range → cursor line (row 0).
        let result = comment_handler(&mut ed, "", None);
        assert_eq!(result, Some(ExEffect::Ok));
        assert_eq!(buf_line(&ed, 0), "// let a = 1;");
        assert_eq!(buf_line(&ed, 1), "let b = 2;");
    }

    #[test]
    fn comment_handler_range_toggles_range() {
        let mut ed = make_rust_editor();
        let range = LineRange::new(1, 3); // 1-based: lines 1–3
        let result = comment_handler(&mut ed, "", Some(range));
        assert_eq!(result, Some(ExEffect::Ok));
        assert_eq!(buf_line(&ed, 0), "// let a = 1;");
        assert_eq!(buf_line(&ed, 1), "// let b = 2;");
        assert_eq!(buf_line(&ed, 2), "// let c = 3;");
    }

    #[test]
    fn comment_handler_whole_buffer_toggle() {
        // :%comment equivalent — toggle all lines.
        let mut ed = make_rust_editor();
        let range = LineRange::new(1, 3);
        comment_handler(&mut ed, "", Some(range));
        // All should be commented now.
        assert!(buf_line(&ed, 0).starts_with("//"));
        assert!(buf_line(&ed, 1).starts_with("//"));
        // Toggle again → all uncommented.
        comment_handler(&mut ed, "", Some(range));
        assert!(!buf_line(&ed, 0).starts_with("//"));
        assert!(!buf_line(&ed, 1).starts_with("//"));
    }

    #[test]
    fn uncomment_handler_strips_comments_idempotent() {
        let buf = hjkl_buffer::View::from_str("// let a = 1;\nlet b = 2;");
        let host = DefaultHost::new();
        let opts = Options {
            filetype: "rust".to_string(),
            ..Options::default()
        };
        let mut ed = hjkl_vim::vim_editor(buf, host, opts);
        let range = LineRange::new(1, 2);
        let result = uncomment_handler(&mut ed, "", Some(range));
        assert_eq!(result, Some(ExEffect::Ok));
        // Line 0: comment stripped.
        assert_eq!(buf_line(&ed, 0), "let a = 1;");
        // Line 1: already uncommented — unchanged.
        assert_eq!(buf_line(&ed, 1), "let b = 2;");
    }

    #[test]
    fn mixed_state_range_gets_fully_commented() {
        // 3 uncommented + 2 commented → all 5 get commented (vim-commentary parity).
        let buf = hjkl_buffer::View::from_str(
            "let a = 1;\n// let b = 2;\nlet c = 3;\n// let d = 4;\nlet e = 5;",
        );
        let host = DefaultHost::new();
        let opts = Options {
            filetype: "rust".to_string(),
            ..Options::default()
        };
        let mut ed = hjkl_vim::vim_editor(buf, host, opts);
        let range = LineRange::new(1, 5);
        comment_handler(&mut ed, "", Some(range));
        for row in 0..5 {
            let l = buf_line(&ed, row);
            assert!(
                l.trim_start().starts_with("//"),
                "row {row} should be commented; got {l:?}"
            );
        }
    }

    // ── redraw_handler / redraw_clear_handler ─────────────────────────────────

    #[test]
    fn dispatch_redraw_returns_redraw_effect() {
        let mut reg = crate::registry::Registry::<hjkl_engine::DefaultHost>::new();
        register_builtins(&mut reg);
        let mut ed = make_editor();
        let cmd = reg.resolve("redraw").expect(":redraw must resolve");
        let result = (cmd.run)(&mut ed, "", None);
        assert_eq!(result, Some(ExEffect::Redraw { clear: false }));
    }

    #[test]
    fn dispatch_redraw_bang_returns_redraw_clear_effect() {
        let mut reg = crate::registry::Registry::<hjkl_engine::DefaultHost>::new();
        register_builtins(&mut reg);
        let mut ed = make_editor();
        let cmd = reg.resolve("redraw!").expect(":redraw! must resolve");
        let result = (cmd.run)(&mut ed, "", None);
        assert_eq!(result, Some(ExEffect::Redraw { clear: true }));
    }

    // ── retab_handler (#207) ─────────────────────────────────────────────────

    fn make_editor_with_opts(
        content: &str,
        expandtab: bool,
        tabstop: usize,
    ) -> Editor<hjkl_buffer::View, DefaultHost> {
        let buf = hjkl_buffer::View::from_str(content);
        let host = DefaultHost::new();
        let opts = Options {
            expandtab,
            tabstop: tabstop as u32,
            ..Options::default()
        };
        hjkl_vim::vim_editor(buf, host, opts)
    }

    #[test]
    fn retab_leading_spaces_to_tabs_when_noexpandtab() {
        // expandtab=false, tabstop=4, leading 8 spaces → "\t\t"
        let mut ed = make_editor_with_opts("        hello", false, 4);
        let result = retab_handler(&mut ed, "", None);
        assert_eq!(result, Some(ExEffect::Ok));
        assert_eq!(buf_line(&ed, 0), "\t\thello");
    }

    #[test]
    fn retab_leading_tabs_to_spaces_when_expandtab() {
        // expandtab=true, tabstop=4, leading "\t\t" → 8 spaces
        let mut ed = make_editor_with_opts("\t\thello", true, 4);
        let result = retab_handler(&mut ed, "", None);
        assert_eq!(result, Some(ExEffect::Ok));
        assert_eq!(buf_line(&ed, 0), "        hello");
    }

    #[test]
    fn retab_bang_converts_internal_whitespace() {
        // expandtab=true, tabstop=4, "a\tb" → "a   b" (col 1 + tab → col 4 = 3 spaces)
        let mut ed = make_editor_with_opts("a\tb", true, 4);
        let result = retab_bang_handler(&mut ed, "", None);
        assert_eq!(result, Some(ExEffect::Ok));
        assert_eq!(buf_line(&ed, 0), "a   b");
    }

    #[test]
    fn retab_with_explicit_tabstop_arg() {
        // ":retab 2" — leading "\t" → 2 spaces (tabstop 2, expandtab on)
        let mut ed = make_editor_with_opts("\thello", true, 4); // editor default is 4
        let result = retab_handler(&mut ed, "2", None);
        assert_eq!(result, Some(ExEffect::Ok));
        // With tabstop=2, one tab → 2 spaces.
        assert_eq!(buf_line(&ed, 0), "  hello");
        // Editor's tabstop setting should NOT be persisted.
        assert_eq!(ed.settings().tabstop, 4);
    }

    #[test]
    fn retab_respects_range() {
        // 1,2retab on 3-line buffer only converts first two lines
        let content = "\thello\n\tworld\n\tfoo";
        let mut ed = make_editor_with_opts(content, true, 4);
        let range = LineRange::new(1, 2); // 1-based lines 1–2
        let result = retab_handler(&mut ed, "", Some(range));
        assert_eq!(result, Some(ExEffect::Ok));
        assert_eq!(buf_line(&ed, 0), "    hello");
        assert_eq!(buf_line(&ed, 1), "    world");
        // Line 3 untouched.
        assert_eq!(buf_line(&ed, 2), "\tfoo");
    }

    #[test]
    fn retab_dispatch_resolves_min_prefix_3() {
        // ":ret" resolves to retab (min_prefix=3)
        let reg = crate::default_registry::<hjkl_engine::DefaultHost>();
        assert!(reg.resolve("ret").is_some(), ":ret must resolve to :retab");
        assert_eq!(reg.resolve("ret").unwrap().name, "retab");
        assert!(reg.resolve("retab").is_some(), ":retab must resolve");
        assert!(reg.resolve("retab!").is_some(), ":retab! must resolve");
    }
}
