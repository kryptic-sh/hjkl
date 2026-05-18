/// Phase 6.6c: `step_search_prompt` relocated from `hjkl-engine::vim`.
///
/// This module owns the search-prompt FSM arm. It is dispatched by
/// [`crate::dispatch_input`] *before* the deprecated `Editor::step_input`
/// shim so callers that have migrated to `dispatch_input` get the
/// hjkl-vim–hosted implementation.
///
/// The engine's `vim::step` still contains an in-engine copy of this body
/// (reached only via the deprecated `Editor::step_input` → `vim::step`
/// shim path). Both copies are intentionally kept in sync until Phase 6.6h
/// removes the engine-side `step` / `step_input` entirely.
use hjkl_engine::{Host, Input, Key};

/// Drive the search-prompt FSM for one keystroke.
///
/// Returns `true` (consumed) unconditionally — every key inside the prompt
/// is swallowed regardless of whether it produced a visible effect.
pub fn step_search_prompt<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    input: Input,
) -> bool {
    // Ctrl-P / Ctrl-N (and Up / Down) walk the search history. Handled
    // before the regular char/backspace branches so `Ctrl-P` doesn't
    // type a literal `p`.
    let history_dir = match (input.key, input.ctrl) {
        (Key::Char('p'), true) | (Key::Up, _) => Some(-1isize),
        (Key::Char('n'), true) | (Key::Down, _) => Some(1isize),
        _ => None,
    };
    if let Some(dir) = history_dir {
        ed.walk_search_history(dir);
        return true;
    }
    match input.key {
        Key::Esc => {
            // Cancel. Drop the prompt but keep the highlighted matches
            // so `n` / `N` can repeat whatever was typed.
            let text = ed
                .take_search_prompt_state()
                .map(|p| p.text)
                .unwrap_or_default();
            if !text.is_empty() {
                ed.set_last_search_pattern_only(Some(text));
            }
            ed.set_search_history_cursor(None);
        }
        Key::Enter => {
            let prompt = ed.take_search_prompt_state();
            if let Some(p) = prompt {
                // Empty `/<CR>` (or `?<CR>`) re-runs the previous search
                // pattern in the prompt's direction — vim parity.
                let pattern: Option<String> = if p.text.is_empty() {
                    ed.last_search_pattern().map(str::to_owned)
                } else {
                    Some(p.text.clone())
                };
                if let Some(pattern) = pattern {
                    ed.push_search_pattern(&pattern);
                    let pre = ed.cursor();
                    if p.forward {
                        ed.search_advance_forward(true);
                    } else {
                        ed.search_advance_backward(true);
                    }
                    ed.push_buffer_cursor_to_textarea();
                    if ed.cursor() != pre {
                        ed.push_jump(pre);
                    }
                    ed.record_search_history(&pattern);
                    ed.set_last_search_pattern_only(Some(pattern));
                    ed.set_last_search_forward_only(p.forward);
                }
            }
            ed.set_search_history_cursor(None);
        }
        Key::Backspace => {
            ed.set_search_history_cursor(None);
            let new_text = ed.search_prompt_state_mut().and_then(|p| {
                if p.text.pop().is_some() {
                    p.cursor = p.text.chars().count();
                    Some(p.text.clone())
                } else {
                    None
                }
            });
            if let Some(text) = new_text {
                ed.push_search_pattern(&text);
            }
        }
        Key::Char(c) => {
            ed.set_search_history_cursor(None);
            let new_text = ed.search_prompt_state_mut().map(|p| {
                p.text.push(c);
                p.cursor = p.text.chars().count();
                p.text.clone()
            });
            if let Some(text) = new_text {
                ed.push_search_pattern(&text);
            }
        }
        _ => {}
    }
    true
}
