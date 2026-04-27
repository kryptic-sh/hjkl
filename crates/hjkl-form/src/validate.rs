//! Field validators.

use crate::field::Field;
use crate::form::Form;

/// Validation function for a single text field. Returns `Err(message)`
/// to surface an error on the field; `Ok(())` clears any prior error.
pub type Validator = Box<dyn Fn(&str) -> Result<(), String> + Send>;

/// Run a single field's validator (if any). Populates `meta.error` on
/// failure and clears it on success. Returns `true` when the field is
/// valid (or has no validator), `false` when it failed.
pub fn validate_field(field: &mut Field) -> bool {
    let (text, validator, meta) = match field {
        Field::SingleLineText(f) => {
            let text = f.editor.buffer().as_string();
            (text, &f.validator, &mut f.meta)
        }
        Field::MultiLineText(f) => {
            let text = f.editor.buffer().as_string();
            (text, &f.validator, &mut f.meta)
        }
        // Non-text fields don't run text validators.
        Field::Select(_) | Field::Checkbox(_) | Field::Submit(_) => return true,
    };
    match validator {
        Some(v) => match v(&text) {
            Ok(()) => {
                meta.error = None;
                true
            }
            Err(msg) => {
                meta.error = Some(msg);
                false
            }
        },
        None => {
            meta.error = None;
            true
        }
    }
}

impl Form {
    /// Validate the focused field. Returns the same boolean as
    /// [`validate_field`].
    pub fn validate_focused(&mut self) -> bool {
        let idx = self.focused();
        match self.fields.get_mut(idx) {
            Some(field) => validate_field(field),
            None => true,
        }
    }

    /// Validate every field. On error, returns the list of
    /// `(field_index, error_message)` tuples and leaves each field's
    /// `meta.error` populated.
    pub fn validate_all(&mut self) -> Result<(), Vec<(usize, String)>> {
        let mut errors = Vec::new();
        for (idx, field) in self.fields.iter_mut().enumerate() {
            if !validate_field(field)
                && let Some(msg) = field.meta().error.clone()
            {
                errors.push((idx, msg));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::field::{FieldMeta, SubmitField, TextFieldEditor};
    use hjkl_engine::{Input, Key};

    fn key(c: char) -> Input {
        Input {
            key: Key::Char(c),
            ..Input::default()
        }
    }

    #[test]
    fn empty_validator_blocks_and_populates_error() {
        let mut name = TextFieldEditor::new(FieldMeta::new("Name").required(true), 1);
        name.validator = Some(Box::new(|s: &str| {
            if s.is_empty() {
                Err("required".into())
            } else {
                Ok(())
            }
        }));
        let mut form = Form::new()
            .with_field(Field::SingleLineText(name))
            .with_field(Field::Submit(SubmitField::new(FieldMeta::new("Sub"))));
        let res = form.validate_all();
        assert!(res.is_err());
        let errs = res.unwrap_err();
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].0, 0);
        assert_eq!(form.fields[0].meta().error.as_deref(), Some("required"));
    }

    #[test]
    fn blur_validate_runs_on_focus_change() {
        let mut name = TextFieldEditor::new(FieldMeta::new("Name").required(true), 1);
        name.validator = Some(Box::new(|s: &str| {
            if s.is_empty() {
                Err("required".into())
            } else {
                Ok(())
            }
        }));
        let mut form = Form::new()
            .with_field(Field::SingleLineText(name))
            .with_field(Field::Submit(SubmitField::new(FieldMeta::new("Sub"))));
        // Move off field 0 — blur validator runs and populates error.
        form.handle_input(key('j'));
        assert_eq!(form.fields[0].meta().error.as_deref(), Some("required"));
    }
}
