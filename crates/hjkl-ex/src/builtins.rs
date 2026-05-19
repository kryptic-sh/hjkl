use crate::{
    effect::ExEffect,
    range::LineRange,
    registry::{ArgKind, ExCommand, Registry},
};
use hjkl_engine::Host;

// ---- folds / global / shell are in their own modules -----------------------
use crate::folds::{apply_fold_indent, apply_fold_syntax};
use crate::global::{global_match_handler, vglobal_handler};

// ---- quit ------------------------------------------------------------------

fn quit_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Quit {
        force: false,
        save: false,
    })
}

fn quit_force_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
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
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
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
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
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
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
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
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
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
    let line_chars = editor
        .buffer()
        .line(row)
        .map(|l| l.chars().count())
        .unwrap_or(0);
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
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
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
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
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
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
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
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
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
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Save)
}

// ---- wq / x ----------------------------------------------------------------

fn wq_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Quit {
        force: false,
        save: true,
    })
}

fn wq_force_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
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
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
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
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Quit {
        force: false,
        save: false,
    })
}

fn qall_force_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
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
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    editor.set_search_pattern(None);
    Some(ExEffect::Ok)
}

// ---- undo / redo -----------------------------------------------------------

fn undo_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    editor.undo();
    Some(ExEffect::Ok)
}

fn redo_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    editor.redo();
    Some(ExEffect::Ok)
}

// ---- saveas / file ---------------------------------------------------------

/// `:saveas {path}` / `:sav {path}` — write buffer to `path` AND rename the
/// buffer identity so future `:w` writes there.
fn saveas_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
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
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    let name = args.trim();
    if name.is_empty() {
        // No arg: surface filename + readonly info. Dirty state lives in the
        // app's slot (not the engine) so only readonly is checked here.
        let filename = editor
            .registers()
            .read('%')
            .map(|s| s.text.clone())
            .unwrap_or_else(|| "[No Name]".into());
        let ro_flag = if editor.is_readonly() { " [RO]" } else { "" };
        Some(ExEffect::Info(format!("\"{filename}\"{ro_flag}")))
    } else {
        Some(ExEffect::RenameBuffer {
            name: name.to_string(),
        })
    }
}

// ---- put -------------------------------------------------------------------

/// `:put [{reg}]` / `:put!` — paste a register's contents as a new line.
///
/// Without `!`: paste below the current line.
/// With `!`: paste above the current line.
/// Default register when no arg: `"` (unnamed).
fn put_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    let reg = args.trim().chars().next().unwrap_or('"');
    Some(ExEffect::PutRegister { reg, above: false })
}

fn put_above_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    let reg = args.trim().chars().next().unwrap_or('"');
    Some(ExEffect::PutRegister { reg, above: true })
}

// ---- registers / marks / jumps / changes -----------------------------------

fn registers_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::InfoTitled {
        title: "registers",
        content: crate::listings::format_registers(editor),
    })
}

fn marks_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::InfoTitled {
        title: "marks",
        content: crate::listings::format_marks(editor),
    })
}

fn jumps_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::InfoTitled {
        title: "jumps",
        content: crate::listings::format_jumps(editor),
    })
}

fn changes_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::InfoTitled {
        title: "changes",
        content: crate::listings::format_changes(editor),
    })
}

// ---- delete ----------------------------------------------------------------

/// `:[range]d` / `:[range]delete` — delete lines in range (default: cursor line).
///
/// `LineRange` is 1-based inclusive. Legacy `Range` (in hjkl-editor) is 0-based;
/// we convert here before mutating the buffer.
fn delete_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
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
            let line_chars = editor
                .buffer()
                .line(0)
                .map(|l| l.chars().count())
                .unwrap_or(0);
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
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
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

    let mut all_lines: Vec<String> = editor.buffer().lines().to_vec();
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
/// `:&` and `:~` (repeat-last-substitute shortcuts) are NOT registered here.
/// They use non-alphabetic names that `split_name_args` cannot parse; defer
/// them to a future phase once the parse layer can handle bare-symbol commands.
fn substitute_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    args: &str,
    range: Option<LineRange>,
) -> Option<ExEffect> {
    use hjkl_engine::substitute::{apply_substitute, parse_substitute};

    // args already starts with `/` (the delimiter); pass straight to engine.
    let cmd = match parse_substitute(args) {
        Ok(c) => c,
        Err(e) => return Some(ExEffect::Error(e.to_string())),
    };

    // Resolve range to 0-based inclusive u32 bounds.
    // No range → current cursor line (cursor() returns 0-based (row, col)).
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

    match apply_substitute(editor, &cmd, r) {
        Ok(out) => Some(ExEffect::Substituted {
            count: out.replacements,
            lines_changed: out.lines_changed,
        }),
        Err(e) => Some(ExEffect::Error(e.to_string())),
    }
}

// ---- set -------------------------------------------------------------------

/// `:set [option ...]` — query / assign vim settings.
fn set_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
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
}

// ---- unit tests ------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::range::LineRange;
    use hjkl_engine::{DefaultHost, Editor, Options};

    // ── helpers ──────────────────────────────────────────────────────────────

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
        let lines = ed.buffer().lines().to_vec();
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
        let lines = ed.buffer().lines().to_vec();
        assert_eq!(lines.len(), 2, "expected 2 remaining lines: {lines:?}");
        assert!(lines.contains(&"l1".to_string()));
        assert!(lines.contains(&"l4".to_string()));
    }

    #[test]
    fn delete_handler_single_line_buffer_clears_content() {
        let mut ed = make_editor_with_lines(&["only line"]);
        let result = delete_handler(&mut ed, "", None);
        assert_eq!(result, Some(ExEffect::Ok));
        // Buffer keeps one empty line rather than zero rows.
        assert_eq!(ed.buffer().row_count(), 1);
        assert_eq!(ed.buffer().line(0).unwrap_or_default(), "");
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
        let lines = ed.buffer().lines().to_vec();
        assert_eq!(lines, vec!["apple", "banana", "cherry"]);
    }

    #[test]
    fn sort_handler_reverse_flag() {
        let mut ed = make_editor_with_lines(&["banana", "apple", "cherry"]);
        let result = sort_handler(&mut ed, "!", None);
        assert_eq!(result, Some(ExEffect::Ok));
        let lines = ed.buffer().lines().to_vec();
        assert_eq!(lines, vec!["cherry", "banana", "apple"]);
    }

    #[test]
    fn sort_handler_unique_flag_removes_duplicates() {
        let mut ed = make_editor_with_lines(&["b", "a", "b", "c", "a"]);
        let result = sort_handler(&mut ed, "u", None);
        assert_eq!(result, Some(ExEffect::Ok));
        let lines = ed.buffer().lines().to_vec();
        // sorted + unique: a, b, c
        assert_eq!(lines, vec!["a", "b", "c"]);
    }

    #[test]
    fn sort_handler_ignore_case_flag() {
        let mut ed = make_editor_with_lines(&["Banana", "apple", "Cherry"]);
        let result = sort_handler(&mut ed, "i", None);
        assert_eq!(result, Some(ExEffect::Ok));
        let lines = ed.buffer().lines().to_vec();
        // case-insensitive: apple < Banana < Cherry
        let lower: Vec<String> = lines.iter().map(|s| s.to_lowercase()).collect();
        assert_eq!(lower, vec!["apple", "banana", "cherry"]);
    }

    #[test]
    fn sort_handler_numeric_flag() {
        let mut ed = make_editor_with_lines(&["10 items", "2 things", "20 stuff"]);
        let result = sort_handler(&mut ed, "n", None);
        assert_eq!(result, Some(ExEffect::Ok));
        let lines = ed.buffer().lines().to_vec();
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
        let lines = ed.buffer().lines().to_vec();
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
        let line = ed.buffer().line(0).unwrap_or_default();
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
        assert_eq!(ed.buffer().line(0).unwrap_or_default(), "aYbYcY");
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
        assert_eq!(
            ed.buffer().line(0).unwrap_or_default(),
            "foo",
            "line 1 untouched"
        );
        assert_eq!(
            ed.buffer().line(1).unwrap_or_default(),
            "bar",
            "line 2 changed"
        );
        assert_eq!(
            ed.buffer().line(2).unwrap_or_default(),
            "foo",
            "line 3 untouched"
        );
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
        let lines = ed.buffer().lines().to_vec();
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
        let lines = ed.buffer().lines().to_vec();
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
}
