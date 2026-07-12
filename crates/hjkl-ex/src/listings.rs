use hjkl_engine::Host;
use hjkl_vim::VimEditorExt;

// ---- registers -------------------------------------------------------------

/// `:reg` / `:registers` — tabular dump of every non-empty register slot.
pub(crate) fn format_registers<H: Host>(
    editor: &hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
) -> String {
    let r = editor.registers();
    let mut lines = vec!["--- Registers ---".to_string()];
    let mut push = |sel: &str, text: &str, linewise: bool| {
        if text.is_empty() {
            return;
        }
        let marker = if linewise { "L" } else { " " };
        lines.push(format!("{sel:<3} {marker} {}", display_register(text)));
    };
    push("\"\"", &r.unnamed.text, r.unnamed.linewise);
    push("\"0", &r.yank_zero.text, r.yank_zero.linewise);
    for (i, slot) in r.delete_ring.iter().enumerate() {
        let sel = format!("\"{}", i + 1);
        push(&sel, &slot.text, slot.linewise);
    }
    for (i, slot) in r.named.iter().enumerate() {
        let sel = format!("\"{}", (b'a' + i as u8) as char);
        push(&sel, &slot.text, slot.linewise);
    }
    if lines.len() == 1 {
        lines.push("(no registers set)".to_string());
    }
    lines.join("\n")
}

/// Escape control chars + truncate so a multi-line register fits a single row
/// of the toast table.
fn display_register(text: &str) -> String {
    let escaped: String = text
        .chars()
        .map(|c| match c {
            '\n' => "\\n".to_string(),
            '\t' => "\\t".to_string(),
            '\r' => "\\r".to_string(),
            c => c.to_string(),
        })
        .collect();
    const MAX: usize = 60;
    if escaped.chars().count() > MAX {
        let head: String = escaped.chars().take(MAX - 3).collect();
        format!("{head}...")
    } else {
        escaped
    }
}

// ---- marks -----------------------------------------------------------------

/// `:marks` — list every set mark with `(line, col)`. Lines are 1-based to
/// match vim; cols are 0-based. Uppercase global marks include the buffer id.
pub(crate) fn format_marks<H: Host>(
    editor: &hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
) -> String {
    let mut lines = vec!["--- Marks ---".to_string(), "mark  line  col".to_string()];
    // Lowercase (buffer-local) marks — from the unified `Editor::marks` map.
    for (c, (r, col)) in editor.marks() {
        if c.is_ascii_lowercase() || !c.is_ascii_alphabetic() {
            lines.push(format!(" {c}    {:>4}  {:>3}", r + 1, col));
        }
    }
    // Uppercase global marks — include buffer_id for cross-buffer context.
    for (c, bid, r, col) in editor.global_marks_iter() {
        lines.push(format!(" {c}    {:>4}  {:>3}  buf:{bid}", r + 1, col));
    }
    if let Some((r, col)) = editor.last_jump_back() {
        lines.push(format!(" '    {:>4}  {:>3}", r + 1, col));
    }
    if let Some((r, col)) = editor.last_edit_pos() {
        lines.push(format!(" .    {:>4}  {:>3}", r + 1, col));
    }
    if lines.len() == 2 {
        lines.push("(no marks set)".to_string());
    }
    lines.join("\n")
}

// ---- jumps -----------------------------------------------------------------

/// `:jumps` — list the jump-back and jump-forward lists.
/// Format mirrors vim: `jump  line  col  file` columns. Newest items
/// have the smallest `jump` number; current position is `0`.
pub(crate) fn format_jumps<H: Host>(
    editor: &hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
) -> String {
    let (back, fwd) = editor.jump_list();
    if back.is_empty() && fwd.is_empty() {
        return "(no jumps recorded)".to_string();
    }
    let mut lines = vec![
        "--- Jump list ---".to_string(),
        " jump  line   col".to_string(),
    ];
    // jump_back: oldest at index 0, newest at the end.
    // Display as descending jump numbers: back.len() at oldest, 1 at newest, 0 = current.
    // Then jump_fwd (forward history) at negative-1, -2, …  vim shows >0 for fwd.
    // We keep it simple: back list reversed with ascending index, then fwd list.
    let back_len = back.len();
    for (i, &(row, col)) in back.iter().rev().enumerate() {
        let jump_num = i + 1;
        lines.push(format!("{jump_num:>5}  {:>4}  {:>4}", row + 1, col));
    }
    // Mark current position (not in list — just a separator).
    lines.push(format!("{:>5}  (current position)", 0));
    for (i, &(row, col)) in fwd.iter().enumerate() {
        let jump_num = -(i as isize + 1);
        lines.push(format!("{jump_num:>5}  {:>4}  {:>4}", row + 1, col));
    }
    let _ = back_len; // used above
    lines.join("\n")
}

// ---- changes ---------------------------------------------------------------

/// `:changes` — list the change list (bounded ring of recent edit positions).
/// Newest entries have lower index numbers, matching vim's `:changes` output.
pub(crate) fn format_changes<H: Host>(
    editor: &hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
) -> String {
    let (list, cursor) = editor.change_list();
    if list.is_empty() {
        return "(no changes recorded)".to_string();
    }
    let mut lines = vec![
        "--- Change list ---".to_string(),
        "change  line   col".to_string(),
    ];
    let len = list.len();
    // List is oldest-at-front, newest-at-back; display newest first (change 1 = most recent).
    for (display_idx, &(row, col)) in list.iter().rev().enumerate() {
        let change_num = display_idx + 1;
        // Mark the current walk position, if any. `change_list_cursor` is an
        // index into the original (oldest-first) vec; invert to display-index.
        let marker = match cursor {
            Some(c) if c == len - 1 - display_idx => " <",
            _ => "",
        };
        lines.push(format!(
            "{change_num:>6}  {:>4}  {:>4}{marker}",
            row + 1,
            col
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use hjkl_engine::{DefaultHost, Editor, Options};

    fn make_editor() -> Editor<hjkl_buffer::Buffer, DefaultHost> {
        let buf = hjkl_buffer::Buffer::new();
        let host = DefaultHost::new();
        Editor::new(buf, host, Options::default())
    }

    // ---- registers ---------------------------------------------------------

    #[test]
    fn format_registers_empty() {
        let editor = make_editor();
        let out = format_registers(&editor);
        assert_eq!(out, "--- Registers ---\n(no registers set)");
    }

    #[test]
    fn format_registers_after_yank() {
        let editor = make_editor();
        editor
            .registers_mut()
            .record_yank("hello".into(), false, None);
        let out = format_registers(&editor);
        assert!(out.contains("hello"), "expected 'hello' in: {out}");
        assert!(out.starts_with("--- Registers ---"));
    }

    // ---- marks -------------------------------------------------------------

    #[test]
    fn format_marks_empty() {
        let editor = make_editor();
        let out = format_marks(&editor);
        assert_eq!(out, "--- Marks ---\nmark  line  col\n(no marks set)");
    }

    #[test]
    fn format_marks_after_set_mark() {
        let mut editor = make_editor();
        editor.set_mark_at_cursor('a');
        let out = format_marks(&editor);
        assert!(out.contains("a"), "expected mark 'a' in: {out}");
        assert!(out.starts_with("--- Marks ---"));
    }

    // ---- jumps -------------------------------------------------------------

    #[test]
    fn format_jumps_empty() {
        let editor = make_editor();
        let out = format_jumps(&editor);
        assert_eq!(out, "(no jumps recorded)");
    }

    // ---- changes -----------------------------------------------------------

    #[test]
    fn format_changes_empty() {
        let editor = make_editor();
        let out = format_changes(&editor);
        assert_eq!(out, "(no changes recorded)");
    }
}
