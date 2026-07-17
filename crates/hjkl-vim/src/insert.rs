//! Phase 6.6d: insert-mode FSM body relocated from `hjkl-engine::vim`.
//!
//! Dispatched by [`crate::dispatch_input`] before the deprecated
//! `Editor::step_input` shim. The engine keeps an in-engine duplicate
//! body in `vim::step` (`Mode::Insert` arm) for back-compat with the
//! deprecated shim path until Phase 6.6h.
use crate::editor_ext::VimEditorExt;
use hjkl_engine::{Host, Input, Key};

/// Drive the insert-mode FSM for one keystroke.
///
/// Returns `true` (consumed) unconditionally — every key inside insert mode
/// is swallowed regardless of whether it produced a visible effect.
pub fn step_insert<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    input: Input,
) -> bool {
    // `Ctrl-R {reg}` paste — the previous keystroke armed the wait. Any
    // non-char key cancels (matches vim, which beeps on selectors like
    // Esc and re-emits the literal text otherwise).
    if ed.insert_pending_register() {
        ed.set_insert_pending_register(false);
        if let Key::Char(c) = input.key
            && !input.ctrl
        {
            ed.insert_paste_register(c);
        }
        return true;
    }

    if input.key == Key::Esc {
        ed.leave_insert_to_normal();
        return true;
    }

    // Ctrl-prefixed insert-mode shortcuts — thin dispatcher to public Editor methods.
    if input.ctrl {
        match input.key {
            Key::Char('w') => {
                ed.insert_ctrl_w();
                return true;
            }
            Key::Char('u') => {
                ed.insert_ctrl_u();
                return true;
            }
            Key::Char('h') => {
                ed.insert_ctrl_h();
                return true;
            }
            Key::Char('o') => {
                ed.insert_ctrl_o_arm();
                return true;
            }
            Key::Char('r') => {
                ed.insert_ctrl_r_arm();
                return true;
            }
            Key::Char('t') => {
                ed.insert_ctrl_t();
                return true;
            }
            Key::Char('d') => {
                ed.insert_ctrl_d();
                return true;
            }
            Key::Char(']') => {
                // `<C-]>` — expand abbreviation WITHOUT inserting any character.
                ed.insert_ctrl_bracket();
                return true;
            }
            // B1: `<C-a>` re-inserts the text typed during the last insert
            // session, `<C-e>`/`<C-y>` copy the char below/above the cursor.
            Key::Char('a') => {
                ed.insert_ctrl_a();
                return true;
            }
            Key::Char('e') => {
                ed.insert_ctrl_e();
                return true;
            }
            Key::Char('y') => {
                ed.insert_ctrl_y();
                return true;
            }
            // B1: any other ctrl-key combo has no dedicated insert-mode
            // binding. Real nvim inserts the raw control byte for most of
            // these (verified: `<C-b>` types a literal ^B) — reproducing
            // that here would put unprintable bytes in the buffer for no
            // benefit, so we consume the key as a no-op instead (documented
            // as an accepted divergence in DIVERGE.md). What this fixes is
            // the PREVIOUS behaviour of falling through to `handle_insert_key`
            // below, which typed the ctrl letter itself as a literal
            // character (e.g. `<C-a>` inserted a literal "a").
            Key::Char(_) => {
                return true;
            }
            _ => {}
        }
    }

    // Widen the session's visited row window *before* handling the key
    // so navigation-only keystrokes (arrow keys) still extend the range.
    let (row, _) = ed.cursor();
    if let Some(session) = ed.insert_session_mut() {
        session.row_min = session.row_min.min(row);
        session.row_max = session.row_max.max(row);
    }
    let mutated = handle_insert_key(ed, input);
    if mutated {
        ed.mark_content_dirty();
        let (row, _) = ed.cursor();
        if let Some(session) = ed.insert_session_mut() {
            session.row_min = session.row_min.min(row);
            session.row_max = session.row_max.max(row);
        }
    }
    true
}

/// Insert-mode key dispatcher — thin shim that routes each key to the
/// corresponding public `Editor::*` method (Phase 6.6a). Returns `true`
/// when the buffer mutated (editing keys), `false` for navigation-only keys.
pub(crate) fn handle_insert_key<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    input: Input,
) -> bool {
    use hjkl_engine::InsertDir;
    match input.key {
        Key::Char(c) => {
            ed.insert_char(c);
            true
        }
        Key::Enter => {
            ed.insert_newline();
            true
        }
        Key::Tab => {
            ed.insert_tab();
            true
        }
        Key::Backspace => {
            ed.insert_backspace();
            true
        }
        Key::Delete => {
            ed.insert_delete();
            true
        }
        Key::Left => {
            ed.insert_arrow(InsertDir::Left);
            false
        }
        Key::Right => {
            ed.insert_arrow(InsertDir::Right);
            false
        }
        Key::Up => {
            ed.insert_arrow(InsertDir::Up);
            false
        }
        Key::Down => {
            ed.insert_arrow(InsertDir::Down);
            false
        }
        Key::Home => {
            ed.insert_home();
            false
        }
        Key::End => {
            ed.insert_end();
            false
        }
        Key::PageUp => {
            let h = ed.viewport_height_value();
            ed.insert_pageup(h);
            false
        }
        Key::PageDown => {
            let h = ed.viewport_height_value();
            ed.insert_pagedown(h);
            false
        }
        // F-keys, mouse scroll, copy/cut/paste virtual keys, Null —
        // no insert-mode behaviour.
        _ => false,
    }
}
