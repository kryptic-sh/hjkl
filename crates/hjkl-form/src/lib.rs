//! # hjkl-form
//!
//! Vim-modal forms for hjkl-based apps.
//!
//! Each text field hosts its own [`hjkl_engine::Editor`], so users get
//! the full vim grammar inside form inputs. The form itself runs a
//! small FSM over `Form-Normal` / `Form-Insert` modes for focus
//! navigation and validation, delegating keystrokes to the focused
//! field's editor when in insert mode.
//!
//! Renderers live in adapter crates: `hjkl-ratatui::form::draw_form`
//! ships the ratatui flavor.
#![forbid(unsafe_code)]

pub mod field;
pub mod form;
pub mod fsm;
pub mod host;
pub mod submit;
pub mod validate;

pub use field::{CheckboxField, Field, FieldMeta, SelectField, SubmitField, TextFieldEditor};
pub use form::{Form, FormEvent, FormMode};
pub use host::FormFieldHost;
pub use submit::{SubmitFn, SubmitOutcome};
pub use validate::{Validator, validate_field};

// Convenience re-exports — consumers using `TextFieldEditor` standalone
// shouldn't need to depend on `hjkl-engine` directly just to talk about
// inputs and modes.
pub use hjkl_engine::{Input, Key, VimMode};

#[cfg(test)]
mod smoke_tests {
    use super::*;

    #[test]
    fn empty_form_constructs() {
        let form = Form::new();
        assert_eq!(form.focused(), 0);
        assert_eq!(form.mode, FormMode::Normal);
    }

    #[test]
    fn two_field_form_focuses_first() {
        let form = Form::new()
            .with_title("Test")
            .with_field(Field::SingleLineText(TextFieldEditor::with_meta(
                FieldMeta::new("Name"),
                1,
            )))
            .with_field(Field::Submit(SubmitField::new(FieldMeta::new("Submit"))));
        assert_eq!(form.fields.len(), 2);
        assert_eq!(form.focused(), 0);
        assert_eq!(form.mode, FormMode::Normal);
    }

    #[test]
    fn dirty_gen_advances_on_field_edit() {
        use hjkl_engine::{Input, Key};
        let mut form = Form::new()
            .with_field(Field::SingleLineText(TextFieldEditor::with_meta(
                FieldMeta::new("Name"),
                1,
            )))
            .with_field(Field::Submit(SubmitField::new(FieldMeta::new("Submit"))));
        let before = form.dirty_gen();
        form.handle_input(Input {
            key: Key::Char('i'),
            ..Input::default()
        });
        form.handle_input(Input {
            key: Key::Char('x'),
            ..Input::default()
        });
        let after = form.dirty_gen();
        assert!(after != before, "dirty_gen should advance after edit");
    }

    #[test]
    fn dirty_gen_advances_on_focus_change() {
        use hjkl_engine::{Input, Key};
        let mut form = Form::new()
            .with_field(Field::SingleLineText(TextFieldEditor::with_meta(
                FieldMeta::new("A"),
                1,
            )))
            .with_field(Field::SingleLineText(TextFieldEditor::with_meta(
                FieldMeta::new("B"),
                1,
            )));
        let before = form.dirty_gen();
        form.handle_input(Input {
            key: Key::Char('j'),
            ..Input::default()
        });
        let after = form.dirty_gen();
        assert!(after != before);
    }
}
