//! Script-mode runner for `hjkl --headless`. No TUI, no event loop.
//!
//! Opens each file, dispatches the `+cmd` / `-c` command stream through the
//! editor ex dispatcher, writes back to disk when `:w` / `:wq` / `:x` runs,
//! and exits. ratatui and crossterm are never initialised.
//!
//! # No auto-write
//!
//! Like vim's `--headless` mode, hjkl does **not** auto-save buffers. You must
//! include an explicit write command (`:w`, `:wq`, `:x`) in your command
//! stream. Exiting without writing leaves the file on disk unchanged.
//!
//! # Exit codes
//!
//! - `0` — all commands completed without errors.
//! - `1` — at least one ex-command returned an `Error` effect, or an I/O
//!   failure occurred while reading or writing a file.
//!
//! # Command ordering
//!
//! All `-c CMD` commands are dispatched first (in flag order), then all `+cmd`
//! tokens (in argv order). Document this in your scripts if ordering matters.

use std::path::PathBuf;

use anyhow::Result;
use hjkl_buffer::View;
use hjkl_engine::{BufferEdit, DefaultHost, Editor, Options};
use hjkl_ex::ExEffect;

/// Run in headless (script) mode.
///
/// `files` — list of files to edit in sequence. When empty, a single
/// anonymous scratch buffer is used (mirrors `nvim --headless` behaviour).
///
/// `commands` — ex commands to dispatch against each file (without the
/// leading `:`). `-c` commands are prepended by the caller; `+cmd` tokens
/// are appended.
///
/// Returns the desired process exit code: `0` on full success, `1` on any
/// ex-command error or I/O failure.
pub fn run(files: Vec<PathBuf>, commands: Vec<String>) -> Result<i32> {
    // Stdout carries command output here — keep the clipboard off so an OSC 52
    // fallback can't inject escapes into it (#264).
    crate::host::disable_clipboard_for_rpc();
    if files.is_empty() && commands.is_empty() {
        eprintln!("hjkl --headless: no commands or files; exiting");
        return Ok(0);
    }

    let targets: Vec<Option<PathBuf>> = if files.is_empty() {
        vec![None]
    } else {
        files.into_iter().map(Some).collect()
    };

    let mut exit_code = 0i32;

    // Session-shared banks (registers, global marks, last :s command,
    // abbrevs, search pattern, changelist), created ONCE up front and wired
    // into every file's editor below — mirroring how `App::new` shares one
    // bank across every window/split. Without this, each file got a fresh
    // `vim_editor` with its own private banks, so e.g. a yank while
    // processing one file was invisible while processing the next — unlike
    // the TUI, where all editors share the six banks (audit R2, fix 4).
    let banks = SharedBanks::new();

    for maybe_path in targets {
        let display_name = maybe_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<scratch>".to_string());

        // --- load buffer ---
        let mut buffer = View::new();
        let mut is_new_file = false;

        if let Some(ref path) = maybe_path {
            match std::fs::read_to_string(path) {
                Ok(content) => {
                    let content = content.strip_suffix('\n').unwrap_or(&content);
                    BufferEdit::replace_all(&mut buffer, content);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    is_new_file = true;
                }
                Err(e) => {
                    eprintln!("hjkl: {display_name}: {e}");
                    exit_code = 1;
                    continue;
                }
            }
        }

        let _ = is_new_file; // tracked for callers; file is created on first :w

        // --- build editor ---
        let host = DefaultHost::new();
        let mut editor = hjkl_vim::vim_editor(buffer, host, Options::default());
        // Wire in the session-shared banks (see above) so registers/marks/
        // last-substitute/abbrevs/search/changelist state set while
        // processing one file is visible while processing the next.
        banks.apply(&mut editor);

        // Track current save target. Starts as the source path; `:w <path>`
        // updates it so subsequent `:w` writes to the new location.
        let mut current_filename: Option<PathBuf> = maybe_path.clone();
        let mut wrote = false;

        // --- dispatch commands ---
        for cmd in &commands {
            // Strip an optional leading `:` so both `-c ':wq'` and `-c 'wq'`
            // work — matches the `+:cmd` / `+cmd` tolerance for `+` tokens.
            let cmd = cmd.strip_prefix(':').unwrap_or(cmd);
            let reg = hjkl_ex::default_registry::<hjkl_engine::DefaultHost>();
            let effect = hjkl_ex::try_dispatch(&reg, &mut editor, cmd)
                .unwrap_or_else(|| ExEffect::Unknown(cmd.to_string()));
            match effect {
                ExEffect::None => {}

                // Quickfix / location list have no dock (or TUI at all) in
                // headless mode — no-op.
                ExEffect::Quickfix(_) | ExEffect::Location(_) => {}

                ExEffect::Ok => {}

                ExEffect::Info(_) | ExEffect::InfoTitled { .. } => {
                    // Suppress info/listing output in silent headless mode.
                    // Future: -v flag could enable it.
                }

                ExEffect::Substituted { .. } => {
                    // Suppress count output; errors already handled above.
                }

                ExEffect::Error(msg) => {
                    eprintln!("hjkl: {display_name}: {msg}");
                    exit_code = 1;
                }

                ExEffect::Unknown(name) => {
                    eprintln!("hjkl: {display_name}: unknown ex command: {name}");
                    exit_code = 1;
                }

                ExEffect::Save => {
                    if let Err(e) = write_buffer(&editor, &current_filename, &display_name) {
                        eprintln!("{e}");
                        exit_code = 1;
                    } else {
                        wrote = true;
                    }
                }

                ExEffect::SaveAs(path_str) => {
                    let new_path = PathBuf::from(&path_str);
                    if let Err(e) = write_buffer(&editor, &Some(new_path.clone()), &display_name) {
                        eprintln!("{e}");
                        exit_code = 1;
                    } else {
                        current_filename = Some(new_path);
                        wrote = true;
                    }
                }

                ExEffect::Quit { save, force: _ } => {
                    if save {
                        if let Err(e) = write_buffer(&editor, &current_filename, &display_name) {
                            eprintln!("{e}");
                            exit_code = 1;
                        } else {
                            wrote = true;
                        }
                    }
                    // Stop dispatching further commands for this file.
                    break;
                }

                ExEffect::EditFile { path, .. } => {
                    // In headless mode, treat :e as switching the current file target.
                    match std::fs::read_to_string(&path) {
                        Ok(content) => {
                            let content = content.strip_suffix('\n').unwrap_or(&content);
                            hjkl_engine::BufferEdit::replace_all(editor.buffer_mut(), content);
                            current_filename = Some(PathBuf::from(&path));
                        }
                        Err(e) => {
                            eprintln!("hjkl: {path}: {e}");
                            exit_code = 1;
                        }
                    }
                }

                ExEffect::BufferDelete { .. } => {
                    // No multi-buffer in headless mode — stop processing.
                    break;
                }

                ExEffect::PutRegister { .. } => {
                    // No multi-buffer paste support in headless mode — no-op.
                }

                ExEffect::SaveAndRename { path } => {
                    let new_path = PathBuf::from(&path);
                    if let Err(e) = write_buffer(&editor, &Some(new_path.clone()), &display_name) {
                        eprintln!("{e}");
                        exit_code = 1;
                    } else {
                        current_filename = Some(new_path);
                        wrote = true;
                    }
                }

                ExEffect::RenameBuffer { .. } => {
                    // In-memory rename — no write; no-op in headless mode.
                }

                ExEffect::Cwd(_) => {
                    // Directory already changed by the handler — no-op.
                }

                ExEffect::Redraw { .. } => {
                    // No terminal to clear in headless mode — no-op.
                }

                ExEffect::Preserve => {
                    // No swap files in headless mode — no-op.
                }

                ExEffect::Recover(_) => {
                    // No swap recovery in headless mode — no-op.
                }

                ExEffect::SubstituteConfirm { matches } => {
                    // Headless mode has no interactive prompt; apply all matches.
                    let count = matches.len();
                    if count > 0 {
                        let accepted = vec![true; count];
                        hjkl_engine::apply_collected_matches(&mut editor, &matches, &accepted);
                    }
                }
            }
        }

        let _ = wrote; // No auto-write; documented above.
    }

    Ok(exit_code)
}

/// Serialise the buffer and write it to `path`. Returns a formatted error
/// string on failure so the caller can print it and set `exit_code = 1`.
fn write_buffer(
    editor: &Editor<View, DefaultHost>,
    path: &Option<PathBuf>,
    display_name: &str,
) -> Result<(), String> {
    match path {
        None => Err(format!("hjkl: {display_name}: E32: No file name")),
        Some(p) => {
            let joined = editor.buffer().content_joined();
            let trailing_nl = !joined.is_empty();
            crate::save::save_file_durable(p, joined.as_bytes(), trailing_nl)
                .map_err(|e| format!("hjkl: {}: {e}", p.display()))
        }
    }
}

/// The six session-shared banks (registers, global marks, last `:s`
/// command, abbrevs, search pattern, changelist) that every buffer-slot
/// editor shares in the TUI (`App::new` / `App::nvim_create_buffer`), so
/// e.g. a yank in one window is pasteable in another. `--headless` gave
/// each file's editor its own private banks instead, so state set while
/// processing one file (a register, a global mark, `&hlsearch` pattern, …)
/// was lost by the time the next file's editor was built (audit R2, fix 4).
///
/// One `SharedBanks` is created per [`run`] call and applied to every
/// file's editor via [`SharedBanks::apply`].
struct SharedBanks {
    registers: std::sync::Arc<std::sync::Mutex<hjkl_engine::Registers>>,
    global_marks: std::sync::Arc<std::sync::Mutex<hjkl_engine::GlobalMarks>>,
    last_substitute: std::sync::Arc<std::sync::Mutex<Option<hjkl_engine::SubstituteCmd>>>,
    abbrevs: std::sync::Arc<std::sync::Mutex<Vec<hjkl_engine::Abbrev>>>,
    search: std::sync::Arc<std::sync::Mutex<hjkl_engine::SearchBank>>,
    // UNLIKE the TUI's per-`buffer_id`-keyed map (`App::change_banks`),
    // headless files are processed strictly one at a time — there is no
    // "switch back to a still-open earlier buffer" to preserve a distinct
    // changelist for — so a single shared bank for the whole session
    // suffices here.
    change_bank: std::sync::Arc<std::sync::Mutex<hjkl_engine::ChangeBank>>,
}

impl SharedBanks {
    fn new() -> Self {
        Self {
            registers: std::sync::Arc::new(
                std::sync::Mutex::new(hjkl_engine::Registers::default()),
            ),
            global_marks: std::sync::Arc::new(std::sync::Mutex::new(
                hjkl_engine::GlobalMarks::new(),
            )),
            last_substitute: std::sync::Arc::new(std::sync::Mutex::new(None)),
            abbrevs: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            search: std::sync::Arc::new(std::sync::Mutex::new(hjkl_engine::SearchBank::default())),
            change_bank: std::sync::Arc::new(std::sync::Mutex::new(
                hjkl_engine::ChangeBank::default(),
            )),
        }
    }

    /// Point `editor` at this session's shared banks — the same six-setter
    /// pattern `App::new`/`App::nvim_create_buffer` use to wire a freshly
    /// created editor into the app-wide banks.
    fn apply(&self, editor: &mut Editor<View, DefaultHost>) {
        editor.set_registers_arc(self.registers.clone());
        editor.set_global_marks_arc(self.global_marks.clone());
        editor.set_last_substitute_arc(self.last_substitute.clone());
        editor.set_abbrevs_arc(self.abbrevs.clone());
        editor.set_search_arc(self.search.clone());
        editor.set_change_bank_arc(self.change_bank.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression (audit R2, fix 4): before the fix, each file in a
    /// `--headless a b …` run got a fresh `vim_editor` with NO shared
    /// banks, so a register populated while processing one file was gone
    /// by the time the next file's (independent) editor was built. Drives
    /// real keystrokes (`yy` / `p`) through two SEPARATE editors — mirroring
    /// how `run` builds one editor per file — wired to the same
    /// `SharedBanks`, exactly as `run` wires every file's editor.
    ///
    /// (There is no ex-command-level way to exercise this: `:normal` is not
    /// implemented and no other ex command currently writes a register, so
    /// this has to drive the editor directly rather than through
    /// `hjkl_ex::try_dispatch` — see the crate-level docs on `run`'s command
    /// dispatch.)
    #[test]
    fn shared_banks_carry_a_yank_across_separate_editors() {
        let banks = SharedBanks::new();

        // "File A": yank its only line into the unnamed register.
        let mut view_a = View::new();
        BufferEdit::replace_all(&mut view_a, "hello from file a");
        let mut editor_a = hjkl_vim::vim_editor(view_a, DefaultHost::new(), Options::default());
        banks.apply(&mut editor_a);
        for input in hjkl_engine::decode_macro("yy") {
            hjkl_vim::dispatch_input(&mut editor_a, input);
        }

        // "File B": a wholly separate editor/buffer (as `run` builds fresh
        // per file), wired to the SAME `SharedBanks`. Paste with no yank of
        // its own first.
        let mut view_b = View::new();
        BufferEdit::replace_all(&mut view_b, "line already in file b");
        let mut editor_b = hjkl_vim::vim_editor(view_b, DefaultHost::new(), Options::default());
        banks.apply(&mut editor_b);
        for input in hjkl_engine::decode_macro("p") {
            hjkl_vim::dispatch_input(&mut editor_b, input);
        }

        let content_b = editor_b.buffer().content_joined();
        assert!(
            content_b.contains("hello from file a"),
            "pasting in editor B must see the yank from editor A when both \
             are wired to the same SharedBanks (mirrors --headless processing \
             file A, then file B); got: {content_b:?}"
        );
    }

    /// Sanity check for the regression test above: WITHOUT `SharedBanks`
    /// (i.e. each editor's own default private registers), the same paste
    /// must NOT see the other editor's yank — proving the assertion above
    /// actually exercises bank-sharing and isn't trivially true.
    #[test]
    fn unshared_editors_do_not_carry_a_yank_across() {
        let mut view_a = View::new();
        BufferEdit::replace_all(&mut view_a, "hello from file a");
        let mut editor_a = hjkl_vim::vim_editor(view_a, DefaultHost::new(), Options::default());
        for input in hjkl_engine::decode_macro("yy") {
            hjkl_vim::dispatch_input(&mut editor_a, input);
        }

        let mut view_b = View::new();
        BufferEdit::replace_all(&mut view_b, "line already in file b");
        let mut editor_b = hjkl_vim::vim_editor(view_b, DefaultHost::new(), Options::default());
        for input in hjkl_engine::decode_macro("p") {
            hjkl_vim::dispatch_input(&mut editor_b, input);
        }

        let content_b = editor_b.buffer().content_joined();
        assert!(
            !content_b.contains("hello from file a"),
            "editors with independent (non-shared) registers must not see \
             each other's yanks; got: {content_b:?}"
        );
    }
}
