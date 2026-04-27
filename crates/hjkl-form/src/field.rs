//! Field types — the building blocks of a `Form`.

use crate::host::FormFieldHost;
use crate::validate::Validator;
use hjkl_buffer::Buffer;
use hjkl_engine::{Editor, Options};

/// Metadata shared by every field variant.
pub struct FieldMeta {
    pub label: String,
    pub required: bool,
    pub error: Option<String>,
    pub placeholder: Option<String>,
}

impl FieldMeta {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            required: false,
            error: None,
            placeholder: None,
        }
    }

    pub fn required(mut self, required: bool) -> Self {
        self.required = required;
        self
    }

    pub fn placeholder(mut self, text: impl Into<String>) -> Self {
        self.placeholder = Some(text.into());
        self
    }
}

/// A text input field — either single-line or multi-line. Owns its own
/// `Editor<Buffer, FormFieldHost>` so the full vim grammar applies.
pub struct TextFieldEditor {
    pub meta: FieldMeta,
    pub editor: Editor<Buffer, FormFieldHost>,
    pub validator: Option<Validator>,
    /// Visible body height for multi-line fields. Single-line is 1.
    pub rows: u16,
    /// `dirty_gen` of the buffer at the moment the user entered Insert
    /// on this field. Used to decide whether a `Changed` event fires
    /// on Esc. Wired up in I2.
    #[allow(dead_code)]
    pub(crate) enter_gen: u64,
}

impl TextFieldEditor {
    pub fn new(meta: FieldMeta, rows: u16) -> Self {
        let buffer = Buffer::new();
        let host = FormFieldHost::new();
        let editor = Editor::new(buffer, host, Options::default());
        Self {
            meta,
            editor,
            validator: None,
            rows,
            enter_gen: 0,
        }
    }

    pub fn with_validator(mut self, validator: Validator) -> Self {
        self.validator = Some(validator);
        self
    }

    pub fn with_initial(mut self, text: &str) -> Self {
        let buffer = Buffer::from_str(text);
        let host = FormFieldHost::new();
        self.editor = Editor::new(buffer, host, Options::default());
        self
    }

    pub fn text(&self) -> String {
        self.editor.buffer().as_string()
    }
}

/// A checkbox field. `value` is the toggled state.
pub struct CheckboxField {
    pub meta: FieldMeta,
    pub value: bool,
}

impl CheckboxField {
    pub fn new(meta: FieldMeta) -> Self {
        Self { meta, value: false }
    }

    pub fn with_value(mut self, value: bool) -> Self {
        self.value = value;
        self
    }
}

/// A select field — the user cycles through `options` with `h` / `l`.
pub struct SelectField {
    pub meta: FieldMeta,
    pub options: Vec<String>,
    pub index: usize,
}

impl SelectField {
    pub fn new(meta: FieldMeta, options: Vec<String>) -> Self {
        Self {
            meta,
            options,
            index: 0,
        }
    }

    pub fn selected(&self) -> Option<&str> {
        self.options.get(self.index).map(String::as_str)
    }
}

/// A submit "button" — an `Enter` while focused fires the form's submit
/// handler.
pub struct SubmitField {
    pub meta: FieldMeta,
}

impl SubmitField {
    pub fn new(meta: FieldMeta) -> Self {
        Self { meta }
    }
}

/// Sum-type for all field variants. The form holds a `Vec<Field>`.
pub enum Field {
    SingleLineText(TextFieldEditor),
    MultiLineText(TextFieldEditor),
    Select(SelectField),
    Checkbox(CheckboxField),
    Submit(SubmitField),
}

impl Field {
    pub fn meta(&self) -> &FieldMeta {
        match self {
            Field::SingleLineText(f) | Field::MultiLineText(f) => &f.meta,
            Field::Select(f) => &f.meta,
            Field::Checkbox(f) => &f.meta,
            Field::Submit(f) => &f.meta,
        }
    }

    pub fn meta_mut(&mut self) -> &mut FieldMeta {
        match self {
            Field::SingleLineText(f) | Field::MultiLineText(f) => &mut f.meta,
            Field::Select(f) => &mut f.meta,
            Field::Checkbox(f) => &mut f.meta,
            Field::Submit(f) => &mut f.meta,
        }
    }

    pub fn is_text(&self) -> bool {
        matches!(self, Field::SingleLineText(_) | Field::MultiLineText(_))
    }

    pub fn is_single_line_text(&self) -> bool {
        matches!(self, Field::SingleLineText(_))
    }

    pub fn is_focusable(&self) -> bool {
        // All field types are focusable in v1.
        let _ = self;
        true
    }
}
