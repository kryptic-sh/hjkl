//! Crossterm/ratatui driver for [`hjkl_vim`].
//!
//! Wires [`crossterm::event::KeyEvent`] through the vim FSM exposed by
//! [`hjkl_vim::dispatch_input`]. This is the public entry point for TUI
//! apps that receive crossterm events.
//!
//! Extracted from `hjkl-vim`'s `crossterm` feature gate as part of #162
//! phase 3. `hjkl-vim` and `hjkl-engine` are now fully toolkit-agnostic;
//! crossterm/ratatui coupling lives here.

/// Drive the vim FSM with a crossterm [`hjkl_engine_tui::KeyEvent`].
///
/// Decodes the event to [`hjkl_engine::Input`] via
/// [`hjkl_engine_tui::crossterm_to_input`] and dispatches through
/// [`hjkl_vim::dispatch_input`]. Emits the cursor-shape change after the
/// FSM returns.
///
/// Returns `true` if the engine consumed the keystroke. Returns `false`
/// for keys the engine FSM does not model (maps to [`hjkl_engine::Key::Null`]).
pub fn handle_key<H: hjkl_engine::Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    key: hjkl_engine_tui::KeyEvent,
) -> bool {
    let input = hjkl_engine_tui::crossterm_to_input(key);
    if input.key == hjkl_engine::Key::Null {
        return false;
    }
    let consumed = hjkl_vim::dispatch_input(editor, input);
    editor.emit_cursor_shape_if_changed();
    consumed
}
