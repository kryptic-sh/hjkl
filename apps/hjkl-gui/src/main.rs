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

use std::{cell::RefCell, path::PathBuf, rc::Rc};

use clap::Parser;
use floem::{
    IntoView,
    event::{Event, EventListener, EventPropagation},
    keyboard::{Key, NamedKey},
    peniko::Color,
    reactive::{RwSignal, SignalGet, SignalUpdate},
    views::{Decorators, container, label, v_stack},
};
use hjkl_buffer::Buffer;
use hjkl_editor::runtime::{DefaultHost, Editor, Options};
use hjkl_editor_gui::{EditorHandle, editor_view, floem_key_to_input};

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

    // Build buffer from file or empty.
    let (buffer, open_path) = if let Some(p) = args.path {
        let contents = std::fs::read_to_string(&p).unwrap_or_default();
        (Buffer::from_str(&contents), Some(p))
    } else {
        (Buffer::new(), None)
    };

    let editor = Editor::new(buffer, DefaultHost::new(), Options::default());
    let handle = EditorHandle::new(editor);
    let save_path = Rc::new(RefCell::new(open_path));

    floem::launch(move || app_view(handle, save_path));
}

// ─── App view ────────────────────────────────────────────────────────────────

/// Tiny ex-command accumulator living outside the engine.
///
/// When vim is in Normal mode and the user presses `:`, we switch to
/// `Collecting` and gather chars until Enter or Esc.
#[derive(Clone, Default, PartialEq, Eq)]
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
        "q" | "q!" => {
            floem::quit_app();
        }
        "w" => {
            let content = handle.with(|ed| ed.content());
            if let Some(path) = save_path.borrow().as_ref() {
                if let Err(e) = std::fs::write(path, content.as_bytes()) {
                    eprintln!("hjkl-gui: write error: {e}");
                }
            } else {
                eprintln!("hjkl-gui: no file path; use :w <path> or open a file");
            }
        }
        "wq" => {
            let content = handle.with(|ed| ed.content());
            if let Some(path) = save_path.borrow().as_ref() {
                if let Err(e) = std::fs::write(path, content.as_bytes()) {
                    eprintln!("hjkl-gui: write error: {e}");
                }
            } else {
                eprintln!("hjkl-gui: no file path set");
            }
            floem::quit_app();
        }
        _ => {
            // Unknown command — silently ignore (Phase A).
        }
    }
}
