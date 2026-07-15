//! Single-line prompt renderer for [`hjkl_form::TextFieldEditor`].
//!
//! Used by hosts that drop a vim-grammar prompt onto a one-row strip:
//! the `:` ex-command palette in `apps/hjkl`, the `/` `?` search
//! prompt, and similar one-shot inputs in downstream binaries.
//!
//! The prefix (`:`, `/`, `?`, ...) is rendered un-styled before the
//! field's text. The prompt sets the field's host viewport so the
//! engine's horizontal scroll keeps the cursor on screen.

use crate::col_span_width;
use hjkl_engine::Host;
use hjkl_form::TextFieldEditor;
use ratatui::{
    Frame,
    buffer::Buffer as RBuffer,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};
use unicode_width::UnicodeWidthStr;

/// Render a single-line `:foo` / `/bar` / `?baz` prompt into `area`.
///
/// `prefix` is rendered first (1 column per ASCII char assumed — every
/// prompt prefix in the wild is a single-byte sigil). The field's text
/// follows, styled with `style`. Returns the terminal-cursor `(x, y)`
/// the caller should pass to `frame.set_cursor_position`. `None` only
/// when `area.height == 0`.
pub fn draw_prompt_line(
    frame: &mut Frame,
    area: Rect,
    prefix: &str,
    field: &mut TextFieldEditor,
    style: Style,
) -> Option<(u16, u16)> {
    let buf = frame.buffer_mut();
    draw_prompt_line_into(buf, area, prefix, field, style)
}

/// Variant that renders into a `&mut View` — used by tests with
/// `TestBackend`.
pub fn draw_prompt_line_into(
    buf: &mut RBuffer,
    area: Rect,
    prefix: &str,
    field: &mut TextFieldEditor,
    style: Style,
) -> Option<(u16, u16)> {
    if area.height == 0 || area.width == 0 {
        return None;
    }
    // Publish viewport so the engine's horizontal scroll math stays
    // accurate as the prompt grows past `width - prefix_width`. Prefix
    // width is a display width (sigils are single-column, but stay honest).
    let prefix_w = UnicodeWidthStr::width(prefix).min(u16::MAX as usize) as u16;
    let field_w = area.width.saturating_sub(prefix_w);
    field.set_viewport_width(field_w.max(1));
    field.set_viewport_height(area.height.max(1));

    // Horizontal scroll: keep the cursor inside the field window so long
    // prompts show the region being edited (not a stale head) and the
    // terminal cursor lands over the right cell. Measured in display
    // columns, so wide (CJK/emoji) chars scroll by the right cell count.
    let (_, ccol) = field.cursor();
    let field_cols = field_w.max(1) as usize;
    let text = field.text();
    let src_line: String = text.lines().next().unwrap_or("").to_string();
    let top_col = {
        let v = field.editor.host_mut().viewport_mut();
        if ccol < v.top_col {
            v.top_col = ccol;
        }
        // Advance the left edge until the columns before the cursor fit
        // (leaving room for the cursor cell itself).
        while v.top_col < ccol && col_span_width(&src_line, v.top_col, ccol) >= field_cols {
            v.top_col += 1;
        }
        v.top_col
    };

    let display: String = src_line.chars().skip(top_col).collect();

    // Pad with spaces so the prompt's `style` (typically a status-line
    // background) fills the row.
    let pad = (area.width as usize)
        .saturating_sub(prefix_w as usize + UnicodeWidthStr::width(display.as_str()));
    let line = Line::from(vec![
        Span::raw(prefix.to_owned()),
        Span::styled(display.clone(), style),
        Span::styled(" ".repeat(pad), style),
    ]);
    Paragraph::new(line).style(style).render(area, buf);

    // Terminal cursor lands one column past the last char-before-cursor,
    // offset by the display width of the scrolled-in prefix text.
    let dx = prefix_w.saturating_add(col_span_width(&src_line, top_col, ccol) as u16);
    let cx = area.x.saturating_add(dx.min(area.width.saturating_sub(1)));
    Some((cx, area.y))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hjkl_form::{Input, Key, TextFieldEditor};
    use ratatui::backend::TestBackend;

    fn render(field: &mut TextFieldEditor, prefix: &str) -> (RBuffer, Option<(u16, u16)>) {
        let backend = TestBackend::new(20, 1);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        let mut cursor = None;
        term.draw(|frame| {
            cursor = draw_prompt_line(frame, frame.area(), prefix, field, Style::default());
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        (buf, cursor)
    }

    fn row_string(buf: &RBuffer, y: u16) -> String {
        let mut s = String::new();
        for x in 0..buf.area.width {
            s.push_str(buf[(x, y)].symbol());
        }
        s
    }

    #[test]
    fn renders_prefix_and_text() {
        let mut f = TextFieldEditor::with_text("foo", true);
        let (buf, cursor) = render(&mut f, ":");
        let row = row_string(&buf, 0);
        assert!(row.starts_with(":foo"), "got row: {row:?}");
        let (cx, cy) = cursor.expect("cursor");
        assert_eq!(cy, 0);
        // Cursor at end of buffer (col 3 in chars) + prefix width 1 = x=4.
        // The standalone `with_text` lands cursor at end (col 3). Vim
        // Normal mode clamps cursor to last printable column, so it
        // sits at col 2 ('o'). The prompt cursor should land at col 3
        // (`:` + 'f' + 'o' + 'o'-cursor) — i.e. on the trailing 'o'.
        assert!((3..=4).contains(&cx), "cursor x out of range: {cx}");
    }

    #[test]
    fn long_text_scrolls_to_keep_cursor_region_visible() {
        // 20-col area, ':' prefix → 19 field columns. Insert 30 chars:
        // the tail (not the head) must be visible and the cursor must
        // sit inside the area over the last typed char.
        let mut f = TextFieldEditor::new(true);
        f.enter_insert_at_end();
        for i in 0..30u8 {
            f.handle_input(Input {
                key: Key::Char(char::from(b'a' + (i % 26))),
                ..Input::default()
            });
        }
        let (buf, cursor) = render(&mut f, ":");
        let row = row_string(&buf, 0);
        // Head of the text ("abc…") must have scrolled out of view.
        assert!(
            !row.starts_with(":abc"),
            "expected scrolled view, got {row:?}"
        );
        // Last typed char ('d' = index 29 → 'd') must be on screen.
        assert!(row.contains('d'), "tail must be visible: {row:?}");
        let (cx, _) = cursor.expect("cursor");
        assert!(cx < 20, "cursor must stay inside the area, got {cx}");
    }

    #[test]
    fn wide_char_cursor_and_scroll_stay_within_area() {
        // Typing 20 double-width CJK chars into a 20-col prompt: the cursor
        // must stay inside the area (display-width scroll, not char count).
        let mut f = TextFieldEditor::new(true);
        f.enter_insert_at_end();
        for _ in 0..20 {
            f.handle_input(Input {
                key: Key::Char('世'),
                ..Input::default()
            });
        }
        let (_buf, cursor) = render(&mut f, ":");
        let (cx, _) = cursor.expect("cursor");
        assert!(cx < 20, "cursor must stay inside area, got {cx}");
    }

    #[test]
    fn cursor_advances_after_typing() {
        let mut f = TextFieldEditor::new(true);
        f.enter_insert_at_end();
        f.handle_input(Input {
            key: Key::Char('a'),
            ..Input::default()
        });
        f.handle_input(Input {
            key: Key::Char('b'),
            ..Input::default()
        });
        let (buf, cursor) = render(&mut f, "/");
        let row = row_string(&buf, 0);
        assert!(row.starts_with("/ab"));
        let (cx, _) = cursor.unwrap();
        // After typing "ab" in Insert: cursor sits one past the 'b'
        // (Insert convention). Prefix '/' + 2 chars = x=3.
        assert_eq!(cx, 3);
    }
}
