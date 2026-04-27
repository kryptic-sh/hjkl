//! `Form` — the top-level container.

use crate::field::Field;
use crate::submit::{SubmitFn, SubmitOutcome};

/// Form-level mode. Insert delegates to the focused field's `Editor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormMode {
    Normal,
    Insert,
}

/// Events emitted by [`crate::Form::handle_input`] back to the host.
#[derive(Debug)]
pub enum FormEvent {
    /// Focus moved, value mutated, or any user-visible state changed.
    Changed,
    /// User pressed `Esc` outside Insert mode.
    Cancelled,
    /// Submit fired (validators ran). Outcome is the `SubmitFn` return
    /// or `Err(...)` if validation failed.
    Submitted(SubmitOutcome),
    /// Submit was attempted but blocked by validation. Field errors
    /// are populated; the host should re-render.
    ValidationFailed,
}

/// A vim-modal form. Holds an ordered list of fields, the focused
/// index, the current `FormMode`, an optional title, and a `submit`
/// closure consumed on first call.
pub struct Form {
    pub title: Option<String>,
    pub fields: Vec<Field>,
    pub mode: FormMode,
    pub(crate) focused: usize,
    pub(crate) submit: Option<SubmitFn>,
    pub(crate) dirty_gen: u64,
    pub(crate) pending_g: bool,
}

impl Form {
    /// Build an empty form.
    pub fn new() -> Self {
        Self {
            title: None,
            fields: Vec::new(),
            mode: FormMode::Normal,
            focused: 0,
            submit: None,
            dirty_gen: 0,
            pending_g: false,
        }
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn with_field(mut self, field: Field) -> Self {
        self.fields.push(field);
        self
    }

    pub fn with_submit(mut self, submit: SubmitFn) -> Self {
        self.submit = Some(submit);
        self
    }

    pub fn focused(&self) -> usize {
        self.focused
    }

    pub fn focused_field(&self) -> Option<&Field> {
        self.fields.get(self.focused)
    }

    pub fn focused_field_mut(&mut self) -> Option<&mut Field> {
        self.fields.get_mut(self.focused)
    }

    pub fn set_focus(&mut self, index: usize) {
        if index < self.fields.len() {
            self.focused = index;
            self.dirty_gen = self.dirty_gen.wrapping_add(1);
        }
    }

    /// Sum of the form-level dirty counter and every text field's
    /// buffer `dirty_gen`. Renderers can cheap-check this to skip
    /// redraws.
    pub fn dirty_gen(&self) -> u64 {
        let mut sum = self.dirty_gen;
        for field in &self.fields {
            match field {
                Field::SingleLineText(f) | Field::MultiLineText(f) => {
                    sum = sum.wrapping_add(f.editor.buffer().dirty_gen());
                }
                _ => {}
            }
        }
        sum
    }

    pub(crate) fn bump_dirty(&mut self) {
        self.dirty_gen = self.dirty_gen.wrapping_add(1);
    }
}

impl Default for Form {
    fn default() -> Self {
        Self::new()
    }
}
