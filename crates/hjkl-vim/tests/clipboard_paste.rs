//! `"+p` / `"*p` / `<C-r>+` read the LIVE OS clipboard, not a stale
//! in-editor slot (audit-r2 fix 4).
//!
//! `Editor::sync_clipboard_register` (the engine's OS-clipboard import
//! hook) had zero callers before this fix — the `"+`/`"*` register slot
//! only ever reflected the last in-editor `"+y`, so `"+p` after copying
//! something in a browser (or any other app) pasted stale or empty text.
//!
//! These use a mock `Host` whose `read_clipboard` is scripted, since
//! `DefaultHost`'s clipboard is round-trip-only (it just echoes back
//! whatever `write_clipboard` last stored, never an externally-set OS
//! value) and can't simulate "something else copied text."

use hjkl_engine::{Editor, Input, Key};
use hjkl_vim::VimEditorExt;

struct MockClipboardHost {
    os_clipboard: Option<String>,
    viewport: hjkl_engine::types::Viewport,
}

impl hjkl_engine::types::Host for MockClipboardHost {
    type Intent = ();
    fn write_clipboard(&mut self, _text: String) {}
    fn read_clipboard(&mut self) -> Option<String> {
        self.os_clipboard.clone()
    }
    fn now(&self) -> core::time::Duration {
        core::time::Duration::ZERO
    }
    fn prompt_search(&mut self) -> Option<String> {
        None
    }
    fn emit_cursor_shape(&mut self, _shape: hjkl_engine::types::CursorShape) {}
    fn viewport(&self) -> &hjkl_engine::types::Viewport {
        &self.viewport
    }
    fn viewport_mut(&mut self) -> &mut hjkl_engine::types::Viewport {
        &mut self.viewport
    }
    fn emit_intent(&mut self, _intent: Self::Intent) {}
}

fn editor_with(
    content: &str,
    os_clipboard: Option<String>,
) -> Editor<hjkl_buffer::View, MockClipboardHost> {
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::new(),
        MockClipboardHost {
            os_clipboard,
            viewport: hjkl_engine::types::Viewport::default(),
        },
        hjkl_engine::Options::default(),
    );
    e.set_content(content);
    e
}

fn press(e: &mut Editor<hjkl_buffer::View, MockClipboardHost>, ch: char) {
    hjkl_vim::dispatch_input(
        e,
        Input {
            key: Key::Char(ch),
            ctrl: false,
            alt: false,
            shift: false,
        },
    );
}

fn press_ctrl(e: &mut Editor<hjkl_buffer::View, MockClipboardHost>, ch: char) {
    hjkl_vim::dispatch_input(
        e,
        Input {
            key: Key::Char(ch),
            ctrl: true,
            alt: false,
            shift: false,
        },
    );
}

#[test]
fn plus_p_pastes_the_live_os_clipboard() {
    let mut e = editor_with("abc\n", Some("XYZ".to_string()));

    // `"+p` — select the clipboard register, then paste after the cursor.
    press(&mut e, '"');
    press(&mut e, '+');
    press(&mut e, 'p');

    assert!(
        e.content().contains("XYZ"),
        "`\"+p` must paste the host's live clipboard text, got: {:?}",
        e.content()
    );
}

#[test]
fn star_p_pastes_the_live_os_clipboard() {
    let mut e = editor_with("abc\n", Some("STAR".to_string()));

    press(&mut e, '"');
    press(&mut e, '*');
    press(&mut e, 'p');

    assert!(
        e.content().contains("STAR"),
        "`\"*p` must paste the host's live clipboard text, got: {:?}",
        e.content()
    );
}

#[test]
fn clipboard_paste_falls_back_to_existing_slot_when_host_has_no_clipboard() {
    let mut e = editor_with("abc\n", None);
    // Simulate an earlier successful sync (e.g. an in-editor `"+y`) that
    // left something in the slot.
    e.sync_clipboard_register("seed".to_string(), false);

    press(&mut e, '"');
    press(&mut e, '+');
    press(&mut e, 'p');

    assert!(
        e.content().contains("seed"),
        "a clipboard-less host (CI/PTY) must fall back to the existing \
         slot rather than pasting nothing, got: {:?}",
        e.content()
    );
}

#[test]
fn ctrl_r_plus_inserts_the_live_os_clipboard() {
    let mut e = editor_with("abc\n", Some("INSERTED".to_string()));
    e.enter_insert_i(1);
    // `<C-r>+` — insert-mode register paste.
    press_ctrl(&mut e, 'r');
    press(&mut e, '+');

    assert!(
        e.content().contains("INSERTED"),
        "`<C-r>+` must insert the host's live clipboard text, got: {:?}",
        e.content()
    );
}
