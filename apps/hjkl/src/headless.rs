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
                .unwrap_or(ExEffect::Unknown(cmd.to_string()));
            match effect {
                ExEffect::None => {}

                // Quickfix / location list have no popup in headless mode — no-op.
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
            let mut content = String::with_capacity(joined.len() + 1);
            content.push_str(&joined);
            if !joined.is_empty() {
                content.push('\n');
            }
            std::fs::write(p, &content).map_err(|e| format!("hjkl: {}: {e}", p.display()))
        }
    }
}
