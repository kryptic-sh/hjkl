//! Submit handler types.

use crate::form::Form;

/// Form submit closure. Consumed via `Option::take` on first call.
pub type SubmitFn = Box<dyn FnOnce() -> SubmitOutcome + Send>;

/// Outcome returned by [`SubmitFn`]. Surfaced through `FormEvent::Submitted`.
#[derive(Debug, Clone)]
pub enum SubmitOutcome {
    Ok,
    Err(String),
}

impl Form {
    /// Validate every field and, on success, fire the submit closure
    /// (consumed via `Option::take`). Returns:
    /// - `Some(SubmitOutcome::Ok)` / `Some(SubmitOutcome::Err(_))` —
    ///   submit ran (or no submit was registered, in which case `Ok`).
    /// - `None` — validation failed; per-field errors populated.
    pub fn try_submit(&mut self) -> Option<SubmitOutcome> {
        if self.validate_all().is_err() {
            self.bump_dirty();
            return None;
        }
        Some(match self.submit.take() {
            Some(submit) => submit(),
            None => SubmitOutcome::Ok,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::field::{Field, FieldMeta, SubmitField, TextFieldEditor};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn successful_submit_consumes_fn() {
        let count = Arc::new(AtomicUsize::new(0));
        let c2 = count.clone();
        let mut form = Form::new()
            .with_field(Field::Submit(SubmitField::new(FieldMeta::new("Sub"))))
            .with_submit(Box::new(move || {
                c2.fetch_add(1, Ordering::SeqCst);
                SubmitOutcome::Ok
            }));
        let r = form.try_submit();
        assert!(matches!(r, Some(SubmitOutcome::Ok)));
        assert_eq!(count.load(Ordering::SeqCst), 1);
        // Second call: closure already consumed, but validation still
        // passes (no validators), so we get a None-fallback Ok.
        let r2 = form.try_submit();
        assert!(matches!(r2, Some(SubmitOutcome::Ok)));
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn try_submit_blocked_by_validator() {
        let count = Arc::new(AtomicUsize::new(0));
        let c2 = count.clone();
        let mut name = TextFieldEditor::with_meta(FieldMeta::new("Name").required(true), 1);
        name.validator = Some(Box::new(|s: &str| {
            if s.is_empty() {
                Err("required".into())
            } else {
                Ok(())
            }
        }));
        let mut form = Form::new()
            .with_field(Field::SingleLineText(name))
            .with_field(Field::Submit(SubmitField::new(FieldMeta::new("Sub"))))
            .with_submit(Box::new(move || {
                c2.fetch_add(1, Ordering::SeqCst);
                SubmitOutcome::Ok
            }));
        let r = form.try_submit();
        assert!(r.is_none());
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }
}
