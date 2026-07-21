use crate::editor_ext::VimEditorExt;
/// Phase 6.6c: `step_search_prompt` relocated from `hjkl-engine::vim`.
///
/// This module owns the search-prompt FSM arm. It is dispatched by
/// [`crate::dispatch_input`] before `begin_step`, since it needs no
/// prelude/epilogue.
use hjkl_engine::{Host, Input, Key};

/// Drive the search-prompt FSM for one keystroke.
///
/// Returns `true` (consumed) unconditionally — every key inside the prompt
/// is swallowed regardless of whether it produced a visible effect.
pub fn step_search_prompt<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
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
                // Split a trailing search offset (`/pat/e`, `/pat/+1`,
                // `/pat/s-2`, ...) from the pattern. The delimiter is whatever
                // opened the prompt (`/` forward, `?` backward).
                let delim = if p.forward { '/' } else { '?' };
                let (pat_text, offset) = split_search_offset(&p.text, delim);
                // Empty `/<CR>` (or `?<CR>`) re-runs the previous search
                // pattern in the prompt's direction — vim parity.
                let pattern: Option<String> = if pat_text.is_empty() {
                    ed.last_search_pattern()
                } else {
                    Some(pat_text)
                };
                if let Some(pattern) = pattern {
                    ed.push_search_pattern(&pattern);
                    let pre = ed.cursor();
                    if p.forward {
                        ed.search_advance_forward(true);
                    } else {
                        ed.search_advance_backward(true);
                    }
                    if let Some(off) = offset.as_deref().filter(|s| !s.is_empty()) {
                        apply_search_offset(ed, off);
                    }
                    ed.push_buffer_cursor_to_textarea();
                    // Operator-pending search (`d/pat`, `c/pat`, `y/pat`): apply
                    // the operator over the range to the match instead of just
                    // moving the cursor / pushing a jump.
                    if let Some((op, _count, origin)) = p.operator {
                        ed.apply_op_search_range(op, origin);
                    } else if ed.cursor() != pre {
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

/// Split a search string into `(pattern, offset)` on the first UNESCAPED
/// delimiter (`/` for forward search, `?` for backward). Returns `offset =
/// None` when no delimiter is present (a bare pattern).
fn split_search_offset(text: &str, delim: char) -> (String, Option<String>) {
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\' {
            i += 2;
            continue;
        }
        if chars[i] == delim {
            let pat: String = chars[..i].iter().collect();
            let off: String = chars[i + 1..].iter().collect();
            return (pat, Some(off));
        }
        i += 1;
    }
    (text.to_string(), None)
}

/// Apply a vim search offset to the cursor, which the search just left at the
/// match start. Supports:
/// - `e[+-N]` — end of match, ± N chars
/// - `s[+-N]` / `b[+-N]` — start of match, ± N chars
/// - `[+-]N` / `N` — line offset (N lines down/up, first non-blank)
fn apply_search_offset<H: Host>(ed: &mut hjkl_engine::Editor<hjkl_buffer::View, H>, offset: &str) {
    let (row, col) = ed.cursor();
    match offset.chars().next() {
        Some('e') => {
            // Saturating arithmetic: `n` is user input and can be `isize::MAX`
            // / `isize::MIN`, which would overflow a plain add (panic in
            // debug builds).
            let n: isize = offset[1..].parse().unwrap_or(0);
            let len = match_len_at(ed, row, col) as isize;
            let end = (col as isize)
                .saturating_add((len - 1).max(0))
                .saturating_add(n);
            ed.jump_cursor(row, end.max(0) as usize);
        }
        Some('s') | Some('b') => {
            let n: isize = offset[1..].parse().unwrap_or(0);
            ed.jump_cursor(row, (col as isize).saturating_add(n).max(0) as usize);
        }
        _ => {
            // Line offset: move N lines and land on the first non-blank.
            let n: isize = offset.parse().unwrap_or(0);
            let rope = ed.buffer().rope();
            let last = rope.len_lines().saturating_sub(1);
            let new_row = (row as isize).saturating_add(n).clamp(0, last as isize) as usize;
            let line = hjkl_buffer::rope_line_str(&rope, new_row);
            let fnb = line.chars().take_while(|c| *c == ' ' || *c == '\t').count();
            drop(rope);
            ed.jump_cursor(new_row, fnb);
        }
    }
}

/// Char-length of the regex match that begins at `(row, col)`. Falls back to 1
/// when no pattern is set or no match starts there.
fn match_len_at<H: Host>(
    ed: &hjkl_engine::Editor<hjkl_buffer::View, H>,
    row: usize,
    col: usize,
) -> usize {
    let Some(re) = ed.search_state().pattern.as_ref() else {
        return 1;
    };
    let line = hjkl_buffer::rope_line_str(&ed.buffer().rope(), row);
    let byte_col = line
        .char_indices()
        .nth(col)
        .map(|(b, _)| b)
        .unwrap_or(line.len());
    if let Some(m) = re.find(&line[byte_col..])
        && m.start() == 0
    {
        return line[byte_col..byte_col + m.end()].chars().count();
    }
    1
}

#[cfg(test)]
mod offset_tests {
    use super::split_search_offset;

    #[test]
    fn plain_pattern_no_offset() {
        assert_eq!(split_search_offset("bar", '/'), ("bar".into(), None));
    }
    #[test]
    fn word_boundary_pattern_preserved() {
        assert_eq!(
            split_search_offset("\\<bar\\>", '/'),
            ("\\<bar\\>".into(), None)
        );
    }
    #[test]
    fn trailing_delim_empty_offset() {
        assert_eq!(
            split_search_offset("bar/", '/'),
            ("bar".into(), Some(String::new()))
        );
    }
    #[test]
    fn end_offset() {
        assert_eq!(
            split_search_offset("bar/e", '/'),
            ("bar".into(), Some("e".into()))
        );
    }
    #[test]
    fn escaped_delim_not_split() {
        assert_eq!(split_search_offset("a\\/b", '/'), ("a\\/b".into(), None));
    }
}
