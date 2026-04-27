//! Interactive demo for `hjkl-form`.
//!
//! Builds a form with one of every field type, runs a crossterm raw-mode
//! event loop, and prints the submitted values to stderr on `Submit`.

use anyhow::Result;
use crossterm::event::{self, Event, KeyEventKind};
use crossterm::{execute, terminal};
use hjkl_engine::Input;
use hjkl_form::{
    CheckboxField, Field, FieldMeta, Form, FormEvent, SelectField, SubmitField, SubmitOutcome,
    TextFieldEditor,
};
use hjkl_ratatui::form::{FormPalette, draw_form};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io::{self, stdout};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

fn build_form(should_quit: Arc<AtomicBool>) -> Form {
    let mut name = TextFieldEditor::with_meta(
        FieldMeta::new("Name")
            .required(true)
            .placeholder("Your name"),
        1,
    );
    name.validator = Some(Box::new(|s: &str| {
        if s.trim().is_empty() {
            Err("name is required".into())
        } else {
            Ok(())
        }
    }));

    let mut email =
        TextFieldEditor::with_meta(FieldMeta::new("Email").placeholder("you@example.com"), 1);
    email.validator = Some(Box::new(|s: &str| {
        if s.contains('@') || s.is_empty() {
            Ok(())
        } else {
            Err("must contain @".into())
        }
    }));

    let description =
        TextFieldEditor::with_meta(FieldMeta::new("Description").placeholder("(multi-line)"), 3);

    let save = CheckboxField::new(FieldMeta::new("Save"));
    let format = SelectField::new(
        FieldMeta::new("Format"),
        vec!["json".into(), "yaml".into(), "toml".into()],
    );
    let submit_field = SubmitField::new(FieldMeta::new("Submit"));

    Form::new()
        .with_title("Form Demo (j/k navigate, i to edit, Esc cancel)")
        .with_field(Field::SingleLineText(name))
        .with_field(Field::SingleLineText(email))
        .with_field(Field::MultiLineText(description))
        .with_field(Field::Checkbox(save))
        .with_field(Field::Select(format))
        .with_field(Field::Submit(submit_field))
        .with_submit(Box::new(move || {
            should_quit.store(true, Ordering::SeqCst);
            SubmitOutcome::Ok
        }))
}

fn print_field_values(form: &Form) {
    eprintln!("--- form submitted ---");
    for field in &form.fields {
        let label = &field.meta().label;
        match field {
            Field::SingleLineText(f) | Field::MultiLineText(f) => {
                eprintln!("{label}: {:?}", f.text());
            }
            Field::Checkbox(c) => eprintln!("{label}: {}", c.value),
            Field::Select(s) => eprintln!("{label}: {:?}", s.selected()),
            Field::Submit(_) => {}
        }
    }
}

fn main() -> Result<()> {
    let should_quit = Arc::new(AtomicBool::new(false));
    let mut form = build_form(should_quit.clone());

    terminal::enable_raw_mode()?;
    execute!(stdout(), terminal::EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;
    let palette = FormPalette::dark();

    let result: Result<()> = loop {
        if should_quit.load(Ordering::SeqCst) {
            break Ok(());
        }
        let mut cursor: Option<(u16, u16)> = None;
        terminal.draw(|frame| {
            let area = frame.area();
            let res = draw_form(frame, area, &mut form, &palette);
            cursor = res.cursor;
        })?;
        if let Some((cx, cy)) = cursor {
            terminal.set_cursor_position((cx, cy))?;
            terminal.show_cursor()?;
        } else {
            terminal.hide_cursor()?;
        }

        let ev = event::read()?;
        if let Event::Key(key) = ev
            && key.kind != KeyEventKind::Release
        {
            let input = Input::from(key);
            if let Some(form_ev) = form.handle_input(input) {
                match form_ev {
                    FormEvent::Cancelled => break Ok(()),
                    FormEvent::Submitted(outcome) => {
                        print_field_values(&form);
                        match outcome {
                            SubmitOutcome::Ok => {}
                            SubmitOutcome::Err(msg) => eprintln!("submit error: {msg}"),
                        }
                        break Ok(());
                    }
                    _ => {}
                }
            }
        }
    };

    let _ = terminal::disable_raw_mode();
    let _ = execute!(io::stdout(), terminal::LeaveAlternateScreen);
    result
}
