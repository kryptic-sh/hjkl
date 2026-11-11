//! Terminal driver for [`hjkl_vim`].
//!
//! Wires ttyr's [`KeyEvent`](hjkl_engine_tui::KeyEvent) through the vim FSM
//! exposed by [`hjkl_vim::dispatch_input`]. This is the public entry point
//! for TUI apps that receive terminal events.
//!
//! `hjkl-vim` and `hjkl-engine` are fully toolkit-agnostic; the terminal
//! coupling lives here.

/// Drive the vim FSM with a ttyr [`hjkl_engine_tui::KeyEvent`].
///
/// Decodes the event to [`hjkl_engine::Input`] via
/// [`hjkl_engine_tui::ttyr_to_input`] and dispatches through
/// [`hjkl_vim::dispatch_input`]. Emits the cursor-shape change after the
/// FSM returns.
///
/// Returns `true` if the engine consumed the keystroke. Returns `false`
/// for keys the engine FSM does not model (maps to [`hjkl_engine::Key::Null`]).
pub fn handle_key<H: hjkl_engine::Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    key: &hjkl_engine_tui::KeyEvent,
) -> bool {
    let input = hjkl_engine_tui::ttyr_to_input(key);
    if input.key == hjkl_engine::Key::Null {
        return false;
    }
    let consumed = hjkl_vim::dispatch_input(editor, input);
    editor.emit_cursor_shape_if_changed();
    consumed
}
