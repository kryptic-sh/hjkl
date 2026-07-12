//! hjkl-gui — vim-modal floem GUI editor built on the hjkl engine.
//!
//! ## Usage
//!
//! ```
//! hjkl-gui [FILE]
//! ```
//!
//! Open FILE for editing (or start with an empty buffer). Type in normal/insert
//! mode. `:w` saves, `:q` quits.
//!
//! ## Ex-command shim (Phase A)
//!
//! We implement a binary-side modal state that intercepts `:` while in normal
//! mode rather than hooking into the engine's command-line mode. This keeps
//! the slice minimal: a `CmdState` tracks whether we are accumulating a `:…`
//! string, and on Enter it dispatches `:w` / `:q`.
//!
//! The `EditorHandle` still receives every other key so the engine FSM drives
//! normal/insert/visual editing.

#![forbid(unsafe_code)]

use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
};

use clap::Parser;
use floem::{
    IntoView,
    event::{Event, EventListener, EventPropagation},
    keyboard::{Key, NamedKey},
    peniko::Color,
    reactive::{RwSignal, SignalGet, SignalUpdate},
    views::{Decorators, container, label, v_stack},
};
use hjkl_app::{config, editorconfig};
use hjkl_buffer::Buffer;
use hjkl_editor::runtime::{DefaultHost, Editor, Options};
use hjkl_editor_gui::{EditorHandle, editor_view, floem_key_to_input};
use hjkl_vim::VimEditorExt;

// ─── CLI ──────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "hjkl-gui", about = "Vim-modal floem GUI editor")]
struct Cli {
    /// File to open (optional; starts with an empty buffer if omitted).
    path: Option<PathBuf>,
}

// ─── Main ────────────────────────────────────────────────────────────────────

fn main() {
    let args = Cli::parse();

    // Load user config (hjkl-app shared with TUI). Failure is non-fatal — we
    // continue with defaults — but the read happens early so a future status
    // bar can surface the source path.
    let _config = match config::load() {
        Ok((cfg, _src)) => Some(cfg),
        Err(e) => {
            eprintln!("hjkl-gui: config load skipped: {e}");
            None
        }
    };

    // Build buffer from file or empty. A missing file is a new buffer that
    // will be created on `:w`; any other read failure (permissions, invalid
    // UTF-8) refuses to open — binding an empty buffer to an existing path
    // would let `:w` silently overwrite the file (matches the TUI's E484).
    let (buffer, open_path) = if let Some(p) = args.path {
        let contents = match read_file_to_open(&p) {
            Ok(contents) => contents.unwrap_or_default(),
            Err(e) => {
                eprintln!("hjkl-gui: {e}");
                std::process::exit(1);
            }
        };
        (Buffer::from_str(&contents), Some(p))
    } else {
        (Buffer::new(), None)
    };

    // Apply .editorconfig overlay (indent style/size, max line len) when we
    // opened a real file — matches the TUI's behavior on file open.
    let mut opts = Options::default();
    if let Some(p) = open_path.as_ref() {
        editorconfig::overlay_for_path(&mut opts, p);
    }

    let editor = Editor::new(buffer, DefaultHost::new(), opts);
    let handle = EditorHandle::new(editor);
    let save_path = Rc::new(RefCell::new(open_path));

    floem::launch(move || app_view(handle, save_path));
}

/// Read the file to open, separating "new file" from "unreadable file".
///
/// Returns `Ok(None)` when the file does not exist yet (open an empty buffer
/// that `:w` will create) and `Err` for any other failure — permissions,
/// invalid UTF-8 — so the caller never binds an empty buffer to an existing
/// file it could not read. Mirrors the TUI's `E484` open contract.
fn read_file_to_open(p: &Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(p) {
        Ok(contents) => Ok(Some(contents)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("E484: Can't open file {}: {e}", p.display())),
    }
}

// ─── App view ────────────────────────────────────────────────────────────────

/// Tiny ex-command accumulator living outside the engine.
///
/// When vim is in Normal mode and the user presses `:`, we switch to
/// `Collecting` and gather chars until Enter or Esc.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum CmdState {
    #[default]
    Idle,
    Collecting(String),
}

fn app_view(handle: EditorHandle, save_path: Rc<RefCell<Option<PathBuf>>>) -> impl IntoView {
    // Signal driving the ex-command bar text.
    let cmd_signal: RwSignal<CmdState> = RwSignal::new(CmdState::Idle);

    // ── Ex-command bar (shown only while collecting) ──────────────────────
    let cmd_label = {
        let cs = cmd_signal;
        label(move || match cs.get() {
            CmdState::Idle => String::new(),
            CmdState::Collecting(s) => format!(":{s}"),
        })
        .style(|s| {
            s.font_family("monospace".to_string())
                .font_size(13.0)
                .color(Color::WHITE)
                .background(Color::rgb8(40, 40, 40))
                .width_full()
                .height(22.0)
        })
    };

    // ── Editor view (handles normal/insert/visual keys) ───────────────────
    let h_editor = handle.clone();
    let inner_view = editor_view(h_editor);

    // ── Outer container with ex-command intercept ─────────────────────────
    let h_outer = handle.clone();
    let sp = save_path.clone();

    container(v_stack((inner_view, cmd_label)).style(|s| s.width_full().height_full()))
        .keyboard_navigable()
        .on_event(EventListener::KeyDown, move |ev| {
            let Event::KeyDown(ke) = ev else {
                return EventPropagation::Continue;
            };

            // ── Ex-command accumulation ───────────────────────────────────────
            match cmd_signal.get() {
                CmdState::Idle => {
                    // In normal mode, `:` starts command collection.
                    // We check whether the editor is in Normal mode: if the key
                    // is `:` and no modifier is held, begin collecting.
                    let is_colon = matches!(&ke.key.logical_key, Key::Character(s) if s == ":");
                    if is_colon && !ke.modifiers.control() && !ke.modifiers.alt() {
                        // Only intercept when the engine is in normal mode.
                        let in_normal = h_outer
                            .with(|ed| matches!(ed.vim_mode(), hjkl_engine::VimMode::Normal));
                        if in_normal {
                            cmd_signal.set(CmdState::Collecting(String::new()));
                            return EventPropagation::Stop;
                        }
                    }
                    // Otherwise forward to the editor.
                    if let Some(input) = floem_key_to_input(ke) {
                        h_outer.with_mut(|ed| {
                            hjkl_vim::dispatch_input(ed, input);
                        });
                        return EventPropagation::Stop;
                    }
                    EventPropagation::Continue
                }

                CmdState::Collecting(mut buf) => {
                    match &ke.key.logical_key {
                        Key::Named(NamedKey::Escape) => {
                            // Cancel.
                            cmd_signal.set(CmdState::Idle);
                        }
                        Key::Named(NamedKey::Enter) => {
                            // Execute.
                            let cmd = buf.trim().to_string();
                            cmd_signal.set(CmdState::Idle);
                            execute_ex(&cmd, &h_outer, &sp);
                        }
                        Key::Named(NamedKey::Backspace) => {
                            buf.pop();
                            cmd_signal.set(CmdState::Collecting(buf));
                        }
                        Key::Character(s) => {
                            buf.push_str(s);
                            cmd_signal.set(CmdState::Collecting(buf));
                        }
                        _ => {}
                    }
                    EventPropagation::Stop
                }
            }
        })
        .style(|s| s.width_full().height_full())
}

/// Dispatch a completed ex command.
fn execute_ex(cmd: &str, handle: &EditorHandle, save_path: &Rc<RefCell<Option<PathBuf>>>) {
    match cmd {
        "q" | "q!" => floem::quit_app(),
        "w" => {
            write_buf(handle, save_path);
        }
        "wq" => {
            write_buf(handle, save_path);
            floem::quit_app();
        }
        _ => {
            // Unknown command — silently ignore (Phase A).
        }
    }
}

/// Write the current buffer to `save_path`. Logs to stderr on failure or
/// when no path is set; the caller decides what to do next (e.g. `:wq`
/// quits regardless).
fn write_buf(handle: &EditorHandle, save_path: &Rc<RefCell<Option<PathBuf>>>) {
    let Some(path) = save_path.borrow().clone() else {
        eprintln!("hjkl-gui: no file path; use :w <path> or open a file");
        return;
    };
    let content = handle.with(|ed| ed.content());
    if let Err(e) = std::fs::write(&path, content.as_bytes()) {
        eprintln!("hjkl-gui: write error: {e}");
    }
}

#[cfg(test)]
mod tests {
    use hjkl_buffer::Buffer;
    use hjkl_editor::runtime::{DefaultHost, Editor, Options};

    /// Smoke test: constructing an Editor from a string buffer succeeds and
    /// the buffer is non-empty. Ensures the hjkl-gui dependency graph
    /// compiles and the basic editor API is callable without a floem runtime.
    #[test]
    fn gui_editor_constructs_from_str() {
        let buf = Buffer::from_str("hello\nworld\n");
        let editor = Editor::new(buf, DefaultHost::new(), Options::default());
        assert!(editor.buffer().row_count() > 0);
    }

    /// Smoke test: `CmdState` default is `Idle` (compile-time check that the
    /// enum and its `Default` derive are intact).
    #[test]
    fn gui_app_builds() {
        let state = super::CmdState::default();
        assert_eq!(state, super::CmdState::Idle);
    }

    /// A readable file opens with its contents.
    #[test]
    fn read_file_to_open_reads_existing_file() {
        let path = std::env::temp_dir().join(format!("hjkl-gui-open-ok-{}", std::process::id()));
        std::fs::write(&path, "hello\n").unwrap();
        let got = super::read_file_to_open(&path);
        std::fs::remove_file(&path).ok();
        assert_eq!(got, Ok(Some("hello\n".to_string())));
    }

    /// A missing file is a legitimate new buffer (`Ok(None)`), created on
    /// save — not an error.
    #[test]
    fn read_file_to_open_missing_file_is_new_buffer() {
        let path = std::env::temp_dir().join(format!(
            "hjkl-gui-open-missing-{}-nonexistent",
            std::process::id()
        ));
        assert_eq!(super::read_file_to_open(&path), Ok(None));
    }

    /// Regression: a file that exists but cannot be decoded must be an error,
    /// never a silent empty buffer — `:w` would overwrite the real file.
    #[test]
    fn read_file_to_open_undecodable_file_is_error() {
        let path = std::env::temp_dir().join(format!("hjkl-gui-open-bad-{}", std::process::id()));
        std::fs::write(&path, [0xff, 0xfe, 0xfd]).unwrap();
        let got = super::read_file_to_open(&path);
        std::fs::remove_file(&path).ok();
        let err = got.expect_err("undecodable file must not open as empty buffer");
        assert!(err.starts_with("E484: Can't open file"), "got: {err}");
    }
}
