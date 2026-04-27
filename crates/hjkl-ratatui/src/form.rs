//! Ratatui renderer for [`hjkl_form::Form`].
//!
//! Lays out fields top-to-bottom: optional title row, then per-field
//! label / body / optional error rows. Returns the focused field's
//! cursor position so the caller can `frame.set_cursor_position(...)`.

use hjkl_engine::Host;
use hjkl_form::{Field, Form, FormMode, TextFieldEditor};
use ratatui::{
    Frame,
    buffer::Buffer as RBuffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

/// Color / style palette for [`draw_form`]. Hosts can override any
/// field; [`FormPalette::dark`] returns a sensible default for dark
/// terminals.
pub struct FormPalette {
    pub label: Style,
    pub focused_label: Style,
    pub error: Style,
    pub placeholder: Style,
    pub checkbox_on: Style,
    pub checkbox_off: Style,
    pub submit: Style,
    pub focused_submit: Style,
}

impl FormPalette {
    pub fn dark() -> Self {
        Self {
            label: Style::default().fg(Color::Gray),
            focused_label: Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
            error: Style::default().fg(Color::LightRed),
            placeholder: Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
            checkbox_on: Style::default().fg(Color::LightGreen),
            checkbox_off: Style::default().fg(Color::Gray),
            submit: Style::default().fg(Color::Cyan),
            focused_submit: Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        }
    }
}

impl Default for FormPalette {
    fn default() -> Self {
        Self::dark()
    }
}

/// Result of [`draw_form`]. The cursor is `Some` when a text field is
/// focused and the cursor lands inside the visible body rect.
pub struct FormRenderResult {
    pub cursor: Option<(u16, u16)>,
}

/// Render a [`Form`] into `area`. The `&mut Form` is needed because
/// each field's `FormFieldHost::viewport` is updated to match its
/// rendered body rect.
pub fn draw_form(
    frame: &mut Frame,
    area: Rect,
    form: &mut Form,
    palette: &FormPalette,
) -> FormRenderResult {
    let buf = frame.buffer_mut();
    draw_form_into(buf, area, form, palette)
}

/// Variant that renders into a `&mut Buffer` (used by tests with
/// `TestBackend`).
pub fn draw_form_into(
    buf: &mut RBuffer,
    area: Rect,
    form: &mut Form,
    palette: &FormPalette,
) -> FormRenderResult {
    let mut y = area.y;
    let bottom = area.y.saturating_add(area.height);
    let focused_idx = form.focused();
    let in_insert = form.mode == FormMode::Insert;

    if let Some(title) = form.title.clone()
        && y < bottom
    {
        Paragraph::new(title)
            .style(Style::default().add_modifier(Modifier::BOLD))
            .render(Rect::new(area.x, y, area.width, 1), buf);
        y = y.saturating_add(1);
        if y < bottom {
            y = y.saturating_add(1); // blank gap row
        }
    }

    let mut cursor: Option<(u16, u16)> = None;

    for (idx, field) in form.fields.iter_mut().enumerate() {
        if y >= bottom {
            break;
        }
        let focused = idx == focused_idx;
        let label_style = if focused {
            palette.focused_label
        } else {
            palette.label
        };
        let mut label_text = String::new();
        if focused {
            label_text.push('>');
            label_text.push(' ');
        } else {
            label_text.push_str("  ");
        }
        if field.meta().required {
            label_text.push('*');
        }
        label_text.push_str(&field.meta().label);
        Paragraph::new(label_text)
            .style(label_style)
            .render(Rect::new(area.x, y, area.width, 1), buf);
        y = y.saturating_add(1);
        if y >= bottom {
            break;
        }

        // Body
        let body_height = field_body_height(field);
        let body_rect = Rect::new(
            area.x.saturating_add(2),
            y,
            area.width.saturating_sub(2),
            body_height.min(bottom.saturating_sub(y)),
        );

        if body_rect.height > 0 {
            let cur = render_body(buf, body_rect, field, palette, focused, in_insert);
            if focused && let Some(pos) = cur {
                cursor = Some(pos);
            }
        }
        y = y.saturating_add(body_rect.height);

        // Error row
        if let Some(err) = field.meta().error.clone()
            && y < bottom
        {
            let err_text = format!("  {err}");
            Paragraph::new(err_text)
                .style(palette.error)
                .render(Rect::new(area.x, y, area.width, 1), buf);
            y = y.saturating_add(1);
        }

        // Spacer
        if y < bottom {
            y = y.saturating_add(1);
        }
    }

    FormRenderResult { cursor }
}

fn field_body_height(field: &Field) -> u16 {
    match field {
        Field::SingleLineText(_) | Field::Checkbox(_) | Field::Select(_) | Field::Submit(_) => 1,
        Field::MultiLineText(f) => f.rows.max(1),
    }
}

fn render_body(
    buf: &mut RBuffer,
    rect: Rect,
    field: &mut Field,
    palette: &FormPalette,
    focused: bool,
    in_insert: bool,
) -> Option<(u16, u16)> {
    match field {
        Field::SingleLineText(f) => {
            update_field_viewport(f, rect);
            let text = f.editor.buffer().as_string();
            let display = text.lines().next().unwrap_or("").to_string();
            if display.is_empty() && !(focused && in_insert) {
                if let Some(ph) = f.meta.placeholder.clone() {
                    Paragraph::new(ph)
                        .style(palette.placeholder)
                        .render(rect, buf);
                }
            } else {
                Paragraph::new(display).render(rect, buf);
            }
            if focused { cursor_xy(f, rect) } else { None }
        }
        Field::MultiLineText(f) => {
            update_field_viewport(f, rect);
            let text = f.editor.buffer().as_string();
            let lines: Vec<Line> = text
                .lines()
                .take(rect.height as usize)
                .map(|l| Line::raw(l.to_string()))
                .collect();
            // Show placeholder if buffer is empty and not actively editing.
            if text.is_empty() && !(focused && in_insert) {
                if let Some(ph) = f.meta.placeholder.clone() {
                    Paragraph::new(ph)
                        .style(palette.placeholder)
                        .render(rect, buf);
                }
            } else {
                Paragraph::new(lines).render(rect, buf);
            }
            if focused { cursor_xy(f, rect) } else { None }
        }
        Field::Checkbox(c) => {
            let (prefix, style) = if c.value {
                ("[x] ", palette.checkbox_on)
            } else {
                ("[ ] ", palette.checkbox_off)
            };
            Paragraph::new(format!("{prefix}{}", c.meta.label))
                .style(style)
                .render(rect, buf);
            None
        }
        Field::Select(s) => {
            let label = s.selected().unwrap_or("");
            let text = format!("< {label} >");
            Paragraph::new(text).render(rect, buf);
            None
        }
        Field::Submit(s) => {
            let style = if focused {
                palette.focused_submit
            } else {
                palette.submit
            };
            let label = format!("[ {} ]", s.meta.label);
            // Center the button
            let pad = rect.width.saturating_sub(label.chars().count() as u16) / 2;
            let line = Line::from(vec![
                Span::raw(" ".repeat(pad as usize)),
                Span::styled(label, style),
            ]);
            Paragraph::new(line).render(rect, buf);
            None
        }
    }
}

fn update_field_viewport(f: &mut TextFieldEditor, rect: Rect) {
    let cursor = f.editor.buffer().cursor();
    let v = f.editor.host_mut().viewport_mut();
    v.width = rect.width;
    v.height = rect.height;
    // Crude horizontal scroll: keep cursor visible.
    if cursor.col < v.top_col {
        v.top_col = cursor.col;
    }
    if rect.width > 0 && cursor.col >= v.top_col + rect.width as usize {
        v.top_col = cursor.col + 1 - rect.width as usize;
    }
    // Vertical scroll for multi-line.
    if cursor.row < v.top_row {
        v.top_row = cursor.row;
    }
    if rect.height > 0 && cursor.row >= v.top_row + rect.height as usize {
        v.top_row = cursor.row + 1 - rect.height as usize;
    }
}

fn cursor_xy(f: &TextFieldEditor, rect: Rect) -> Option<(u16, u16)> {
    let cursor = f.editor.buffer().cursor();
    let v = f.editor.host().viewport();
    if cursor.row < v.top_row || cursor.col < v.top_col {
        return None;
    }
    let dy = (cursor.row - v.top_row) as u16;
    let dx = (cursor.col - v.top_col) as u16;
    if dy >= rect.height || dx >= rect.width {
        return None;
    }
    Some((rect.x.saturating_add(dx), rect.y.saturating_add(dy)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hjkl_form::{CheckboxField, FieldMeta, SelectField, SubmitField, TextFieldEditor};
    use ratatui::backend::TestBackend;

    fn render_to_buffer(form: &mut Form) -> (RBuffer, FormRenderResult) {
        let backend = TestBackend::new(40, 20);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        let mut result = FormRenderResult { cursor: None };
        term.draw(|frame| {
            result = draw_form(frame, frame.area(), form, &FormPalette::dark());
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        (buf, result)
    }

    fn buf_contains(buf: &RBuffer, needle: &str) -> bool {
        for y in 0..buf.area.height {
            let mut row = String::new();
            for x in 0..buf.area.width {
                row.push_str(buf[(x, y)].symbol());
            }
            if row.contains(needle) {
                return true;
            }
        }
        false
    }

    #[test]
    fn renders_required_label_with_star() {
        let mut form = Form::new().with_field(Field::SingleLineText(TextFieldEditor::new(
            FieldMeta::new("Name").required(true),
            1,
        )));
        let (buf, _) = render_to_buffer(&mut form);
        assert!(buf_contains(&buf, "*Name"), "expected *Name in render");
    }

    #[test]
    fn renders_checked_checkbox() {
        let mut form = Form::new().with_field(Field::Checkbox(
            CheckboxField::new(FieldMeta::new("Save")).with_value(true),
        ));
        let (buf, _) = render_to_buffer(&mut form);
        assert!(buf_contains(&buf, "[x]"));
    }

    #[test]
    fn renders_select_with_arrows() {
        let mut form = Form::new().with_field(Field::Select(SelectField::new(
            FieldMeta::new("Format"),
            vec!["json".into(), "yaml".into()],
        )));
        let (buf, _) = render_to_buffer(&mut form);
        assert!(buf_contains(&buf, "< json >"));
    }

    #[test]
    fn renders_submit_button() {
        let mut form =
            Form::new().with_field(Field::Submit(SubmitField::new(FieldMeta::new("Save"))));
        let (buf, _) = render_to_buffer(&mut form);
        assert!(buf_contains(&buf, "[ Save ]"));
    }

    #[test]
    fn focused_text_field_returns_cursor_in_body() {
        let mut form = Form::new().with_field(Field::SingleLineText(TextFieldEditor::new(
            FieldMeta::new("Name"),
            1,
        )));
        let (_buf, result) = render_to_buffer(&mut form);
        let (cx, cy) = result.cursor.expect("expected cursor for focused text");
        // Body rect is at y=1 (label row 0), x=2 (indent), so cursor
        // should fall within those bounds.
        assert!(cy >= 1, "cursor y out of body");
        assert!(cx >= 2, "cursor x out of body");
    }

    #[test]
    fn unfocused_text_field_shows_placeholder() {
        let mut form = Form::new()
            .with_field(Field::Submit(SubmitField::new(FieldMeta::new("S"))))
            .with_field(Field::SingleLineText(TextFieldEditor::new(
                FieldMeta::new("Email").placeholder("you@example.com"),
                1,
            )));
        // Submit is focused (idx 0) so the email field is not focused.
        let (buf, _) = render_to_buffer(&mut form);
        assert!(buf_contains(&buf, "you@example.com"));
    }
}
