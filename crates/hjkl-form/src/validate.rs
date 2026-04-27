//! Field validators.

use crate::field::Field;

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
