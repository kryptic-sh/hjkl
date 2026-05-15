use crate::{
    effect::ExEffect,
    registry::{ArgKind, ExCommand, Registry},
};
use hjkl_engine::Host;

// ---- quit ------------------------------------------------------------------

fn quit_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    _args: &str,
) -> Option<ExEffect> {
    Some(ExEffect::Quit {
        force: false,
        save: false,
    })
}

fn quit_force_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    _args: &str,
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
) -> Option<ExEffect> {
    Some(ExEffect::EditFile {
        path: args.trim().to_string(),
        force: true,
    })
}

// ---- read ------------------------------------------------------------------

/// `:r <path>` / `:read <path>` — insert file contents below cursor row.
/// Returns `None` when no path is given (vim errors; we defer to legacy).
fn read_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    args: &str,
) -> Option<ExEffect> {
    let path = args.trim();
    if path.is_empty() {
        None
    } else {
        Some(ExEffect::ReadFile {
            path: path.to_string(),
        })
    }
}

// ---- bdelete / bwipeout ----------------------------------------------------

/// `:bd` / `:bdelete` — close current buffer (no force).
fn bdelete_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    _args: &str,
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
) -> Option<ExEffect> {
    Some(ExEffect::Save)
}

// ---- wq / x ----------------------------------------------------------------

fn wq_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    _args: &str,
) -> Option<ExEffect> {
    Some(ExEffect::Quit {
        force: false,
        save: true,
    })
}

fn wq_force_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    _args: &str,
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
) -> Option<ExEffect> {
    Some(ExEffect::Quit {
        force: false,
        save: false,
    })
}

fn qall_force_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    _args: &str,
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
) -> Option<ExEffect> {
    editor.set_search_pattern(None);
    Some(ExEffect::Ok)
}

// ---- undo / redo -----------------------------------------------------------

fn undo_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    _args: &str,
) -> Option<ExEffect> {
    editor.undo();
    Some(ExEffect::Ok)
}

fn redo_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    _args: &str,
) -> Option<ExEffect> {
    editor.redo();
    Some(ExEffect::Ok)
}

// ---- registers / marks / jumps / changes -----------------------------------

fn registers_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    _args: &str,
) -> Option<ExEffect> {
    Some(ExEffect::Info(crate::listings::format_registers(editor)))
}

fn marks_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    _args: &str,
) -> Option<ExEffect> {
    Some(ExEffect::Info(crate::listings::format_marks(editor)))
}

fn jumps_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    _args: &str,
) -> Option<ExEffect> {
    Some(ExEffect::Info(crate::listings::format_jumps(editor)))
}

fn changes_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    _args: &str,
) -> Option<ExEffect> {
    Some(ExEffect::Info(crate::listings::format_changes(editor)))
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
}
