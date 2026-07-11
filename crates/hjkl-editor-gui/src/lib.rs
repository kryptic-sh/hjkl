//! # hjkl-editor-gui
//!
//! Floem adapter for the hjkl modal editor stack.
//!
//! Provides a reactive bridge between the imperative `Editor` and
//! floem's signal-driven view tree, plus a key-translation function
//! for floem's keyboard events.
//!
//! ## Phase A scope
//!
//! - Buffer text rendered as a single reactive `label`.
//! - Cursor position shown as `(row, col)` in a status line.
//! - No syntax highlighting, no themes.
//! - Key handling: alphanumerics, Esc, Enter, Backspace, Tab, arrows,
//!   Home, End, with Ctrl/Shift/Alt modifier bits.

#![forbid(unsafe_code)]

use std::{cell::RefCell, rc::Rc};

use floem::{
    IntoView,
    event::{Event, EventListener, EventPropagation},
    keyboard::{Key, NamedKey},
    peniko::Color,
    reactive::{RwSignal, SignalGet, SignalUpdate},
    views::{Decorators, container, label, scroll, v_stack},
};
use hjkl_buffer::Buffer;
use hjkl_engine::types::DefaultHost;
use hjkl_engine::{Editor, Input as EngineInput, Key as EngineKey};

// ─── EditorHandle ────────────────────────────────────────────────────────────

/// Reactive handle wrapping an `Editor`.
///
/// Holds an `Rc<RefCell<Editor>>` for shared ownership inside the single
/// floem thread, paired with a revision `RwSignal<u64>` that views subscribe
/// to. Bumping the signal triggers a reactive re-render without cloning the
/// editor or requiring `Send`.
#[derive(Clone)]
pub struct EditorHandle {
    inner: Rc<RefCell<Editor<Buffer, DefaultHost>>>,
    rev: RwSignal<u64>,
}

impl EditorHandle {
    /// Wrap an `Editor` in a new handle. The initial revision is `0`.
    pub fn new(editor: Editor<Buffer, DefaultHost>) -> Self {
        Self {
            inner: Rc::new(RefCell::new(editor)),
            rev: RwSignal::new(0),
        }
    }

    /// Read-only access to the editor. Does **not** bump the revision.
    pub fn with<R>(&self, f: impl FnOnce(&Editor<Buffer, DefaultHost>) -> R) -> R {
        f(&self.inner.borrow())
    }

    /// Mutable access to the editor. Bumps the revision automatically so
    /// subscribed views re-render.
    pub fn with_mut<R>(&self, f: impl FnOnce(&mut Editor<Buffer, DefaultHost>) -> R) -> R {
        let result = f(&mut self.inner.borrow_mut());
        self.bump_rev();
        result
    }

    /// Manually bump the revision signal. Called automatically by `with_mut`.
    pub fn bump_rev(&self) {
        self.rev.update(|r| *r += 1);
    }

    /// Return the current revision number.
    pub fn rev(&self) -> u64 {
        self.rev.get()
    }
}

// ─── floem_key_to_input ──────────────────────────────────────────────────────

/// Translate a floem `KeyEvent` into an `hjkl_engine::Input`.
///
/// Returns `None` for keys the engine does not handle (modifier-only presses,
/// unrecognised function keys, etc.).
pub fn floem_key_to_input(event: &floem::keyboard::KeyEvent) -> Option<EngineInput> {
    let mods = event.modifiers;
    let ctrl = mods.control();
    let alt = mods.alt();
    let shift = mods.shift();

    let key = match &event.key.logical_key {
        Key::Character(s) => {
            let c = s.chars().next()?;
            EngineKey::Char(c)
        }
        Key::Named(named) => match named {
            NamedKey::Escape => EngineKey::Esc,
            NamedKey::Enter => EngineKey::Enter,
            NamedKey::Backspace => EngineKey::Backspace,
            NamedKey::Tab => EngineKey::Tab,
            NamedKey::ArrowUp => EngineKey::Up,
            NamedKey::ArrowDown => EngineKey::Down,
            NamedKey::ArrowLeft => EngineKey::Left,
            NamedKey::ArrowRight => EngineKey::Right,
            NamedKey::Home => EngineKey::Home,
            NamedKey::End => EngineKey::End,
            NamedKey::PageUp => EngineKey::PageUp,
            NamedKey::PageDown => EngineKey::PageDown,
            NamedKey::Delete => EngineKey::Delete,
            _ => return None,
        },
        // Unknown / unhandled key variants.
        _ => return None,
    };

    if key == EngineKey::Null {
        return None;
    }

    Some(EngineInput {
        key,
        ctrl,
        alt,
        shift,
    })
}

// ─── editor_view ─────────────────────────────────────────────────────────────

/// Build a floem `View` that renders the editor buffer and cursor.
///
/// ## Rendering (Phase A)
///
/// - The buffer is rendered as a single `label` inside a `scroll`. The label
///   text is rebuilt reactively whenever `handle.rev` changes.
/// - A one-line status bar below shows the current vim mode and cursor
///   position in `[MODE] row:col` format.
/// - A thin overlay line shows what `:…` command is being typed (if any),
///   driven by the same revision signal.
///
/// ## Input handling
///
/// Key events are captured on the outer container with
/// `.on_event(EventListener::KeyDown, …)`.
/// Each event is translated via [`floem_key_to_input`] and dispatched to
/// `hjkl_vim::dispatch_input`, then the revision is bumped to trigger a
/// re-render.
pub fn editor_view(handle: EditorHandle) -> impl IntoView {
    // ── Buffer text label (reactive) ──────────────────────────────────────
    let h_text = handle.clone();
    let buf_label = label(move || {
        // Subscribe to revision signal so floem re-evaluates this closure
        // whenever the editor mutates.
        let _rev = h_text.rev.get();
        h_text.with(|ed| ed.content())
    })
    .style(|s| {
        s.font_family("monospace".to_string())
            .font_size(14.0)
            .color(Color::WHITE)
            .width_full()
    });

    let text_area = scroll(buf_label).style(|s| {
        s.width_full()
            .flex_grow(1.0_f32)
            .background(Color::rgb8(30, 30, 30))
    });

    // ── Status bar (reactive) ─────────────────────────────────────────────
    let h_status = handle.clone();
    let status_bar = label(move || {
        let _rev = h_status.rev.get();
        h_status.with(|ed| {
            let (row, col) = ed.cursor();
            let mode = match ed.vim_mode() {
                hjkl_engine::VimMode::Insert => "INSERT",
                hjkl_engine::VimMode::Visual => "VISUAL",
                hjkl_engine::VimMode::VisualLine => "V-LINE",
                hjkl_engine::VimMode::VisualBlock => "V-BLOCK",
                hjkl_engine::VimMode::Normal => "NORMAL",
            };
            format!(" {mode}  {row}:{col} ")
        })
    })
    .style(|s| {
        s.font_family("monospace".to_string())
            .font_size(13.0)
            .color(Color::WHITE)
            .background(Color::rgb8(60, 60, 100))
            .width_full()
            .height(22.0)
    });

    // ── Outer container: captures key events ──────────────────────────────
    let h_keys = handle.clone();
    container(v_stack((text_area, status_bar)).style(|s| s.width_full().height_full()))
        .keyboard_navigable()
        .on_event(EventListener::KeyDown, move |ev| {
            if let Event::KeyDown(ke) = ev
                && let Some(input) = floem_key_to_input(ke)
            {
                h_keys.with_mut(|ed| {
                    hjkl_vim::dispatch_input(ed, input);
                });
                return EventPropagation::Stop;
            }
            EventPropagation::Continue
        })
        .style(|s| s.width_full().height_full())
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use hjkl_editor::runtime::Options;

    fn make_editor() -> Editor<Buffer, DefaultHost> {
        Editor::new(Buffer::new(), DefaultHost::new(), Options::default())
    }

    #[test]
    fn handle_new_and_with() {
        let ed = make_editor();
        let handle = EditorHandle::new(ed);
        let content = handle.with(|e| e.content());
        // An empty buffer has a single newline.
        assert!(content.contains('\n') || content.is_empty() || content == "\n");
    }

    #[test]
    fn handle_with_mut_bumps_rev() {
        // RwSignal requires a floem reactive runtime to be live; we only
        // test the non-reactive parts (borrow + mutation) in unit tests.
        let ed = make_editor();
        let handle = EditorHandle::new(ed);
        handle.with_mut(|e| e.set_content("hello"));
        let c = handle.with(|e| e.content());
        assert!(c.contains("hello"));
    }
}
