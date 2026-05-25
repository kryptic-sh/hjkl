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

// ---- cd / pwd --------------------------------------------------------------

/// `:cd [{path}]` — change working directory. No arg → `$HOME`.
fn cd_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
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
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
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
        Ok(out) => {
            // Store so `:&` / `:&&` can repeat this substitution.
            editor.set_last_substitute(cmd);
            Some(ExEffect::Substituted {
                count: out.replacements,
                lines_changed: out.lines_changed,
            })
        }
        Err(e) => Some(ExEffect::Error(e.to_string())),
    }
}

/// `:&` / `:&&` / `:[range]&` / `:[range]&&` — repeat last substitute.
///
/// `:&`  — repeat with original flags dropped (pattern and replacement kept).
/// `:&&` — repeat with original flags preserved.
///
/// `keep_flags` is `true` for `&&`, `false` for `&`.
pub(crate) fn repeat_substitute_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    keep_flags: bool,
    range: Option<LineRange>,
) -> ExEffect {
    use hjkl_engine::substitute::{SubstFlags, apply_substitute};

    let cmd = match editor.last_substitute().cloned() {
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
}

// ---- :redraw ---------------------------------------------------------------

/// `:redraw` — signal the host to repaint without clearing.
fn redraw_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Redraw { clear: false })
}

/// `:redraw!` — signal the host to clear the terminal then repaint.
fn redraw_clear_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    Some(ExEffect::Redraw { clear: true })
}

// ---- :syntax ---------------------------------------------------------------

/// `:syntax [on|off|enable|disable|...]` — engine-side no-op for vim parity.
///
/// Recognised subcommands return `ExEffect::Ok`. Unknown args also return
/// `Ok` (vim's `:syntax <bareword>` is permissive — many forms like
/// `:syntax sync`, `:syntax clear`, `:syntax reset` are accepted without
/// error).
fn syntax_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
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
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
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
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
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
    editor: &hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
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
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    args: &str,
    range: Option<LineRange>,
) -> Option<ExEffect> {
    retab_impl(editor, args, range, false)
}

/// `:[range]retab! [N]` — also convert internal whitespace runs.
fn retab_bang_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    args: &str,
    range: Option<LineRange>,
) -> Option<ExEffect> {
    retab_impl(editor, args, range, true)
}

fn retab_impl<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
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

    fn buf_line(editor: &Editor<hjkl_buffer::Buffer, DefaultHost>, row: usize) -> String {
        hjkl_buffer::rope_line_str(&editor.buffer().rope(), row)
    }

    fn buf_lines(editor: &Editor<hjkl_buffer::Buffer, DefaultHost>) -> Vec<String> {
        let rope = editor.buffer().rope();
        (0..rope.len_lines())
            .map(|i| hjkl_buffer::rope_line_str(&rope, i))
            .collect()
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
        // Buffer keeps one empty line rather than zero rows.
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

    fn make_rust_editor() -> Editor<hjkl_buffer::Buffer, DefaultHost> {
        let buf = hjkl_buffer::Buffer::from_str("let a = 1;\nlet b = 2;\nlet c = 3;");
        let host = DefaultHost::new();
        let opts = Options {
            filetype: "rust".to_string(),
            ..Options::default()
        };
        Editor::new(buf, host, opts)
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
        let buf = hjkl_buffer::Buffer::from_str("// let a = 1;\nlet b = 2;");
        let host = DefaultHost::new();
        let opts = Options {
            filetype: "rust".to_string(),
            ..Options::default()
        };
        let mut ed = Editor::new(buf, host, opts);
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
        let buf = hjkl_buffer::Buffer::from_str(
            "let a = 1;\n// let b = 2;\nlet c = 3;\n// let d = 4;\nlet e = 5;",
        );
        let host = DefaultHost::new();
        let opts = Options {
            filetype: "rust".to_string(),
            ..Options::default()
        };
        let mut ed = Editor::new(buf, host, opts);
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
    ) -> Editor<hjkl_buffer::Buffer, DefaultHost> {
        let buf = hjkl_buffer::Buffer::from_str(content);
        let host = DefaultHost::new();
        let opts = Options {
            expandtab,
            tabstop: tabstop as u32,
            ..Options::default()
        };
        Editor::new(buf, host, opts)
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
