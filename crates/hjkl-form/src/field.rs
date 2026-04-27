//! Field types — the building blocks of a `Form`.

use crate::host::FormFieldHost;
use crate::validate::Validator;
use hjkl_buffer::Buffer;
use hjkl_engine::{Editor, Options};

/// Metadata shared by every field variant. Holds the label,
/// required-marker, the most recent validator error, and an optional
/// placeholder shown when text fields are empty.
pub struct FieldMeta {
    pub label: String,
    pub required: bool,
    pub error: Option<String>,
    pub placeholder: Option<String>,
}

impl FieldMeta {
    /// Construct a field with just a label. Use the builder methods to
    /// layer on `required`, `placeholder`, etc.
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            required: false,
            error: None,
            placeholder: None,
        }
    }

    /// Mark the field as required (renderers prefix the label with `*`).
    pub fn required(mut self, required: bool) -> Self {
        self.required = required;
        self
    }

    /// Set the placeholder text shown when the field is empty and not
    /// being edited.
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
    /// on Esc.
    pub(crate) enter_gen: u64,
}

impl TextFieldEditor {
    /// Construct an empty text field. `rows` is the visible body
    /// height in render rows; multi-line fields may scroll within it.
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

    /// Attach a validator that runs on field-blur and on submit.
    pub fn with_validator(mut self, validator: Validator) -> Self {
        self.validator = Some(validator);
        self
    }

    /// Pre-fill the editor's buffer with `text`.
    pub fn with_initial(mut self, text: &str) -> Self {
        let buffer = Buffer::from_str(text);
        let host = FormFieldHost::new();
        self.editor = Editor::new(buffer, host, Options::default());
        self
    }

    /// Snapshot the buffer's current text.
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
    /// Construct an unchecked checkbox.
    pub fn new(meta: FieldMeta) -> Self {
        Self { meta, value: false }
    }

    /// Set the initial checked state.
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
    /// Construct a select field with a list of options. The first
    /// option is selected by default.
    pub fn new(meta: FieldMeta, options: Vec<String>) -> Self {
        Self {
            meta,
            options,
            index: 0,
        }
    }

    /// Currently-selected option (`None` if `options` is empty).
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
    /// Construct a submit "button" field.
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
    /// Borrow the field's metadata.
    pub fn meta(&self) -> &FieldMeta {
        match self {
            Field::SingleLineText(f) | Field::MultiLineText(f) => &f.meta,
            Field::Select(f) => &f.meta,
            Field::Checkbox(f) => &f.meta,
            Field::Submit(f) => &f.meta,
        }
    }

    /// Mutably borrow the field's metadata.
    pub fn meta_mut(&mut self) -> &mut FieldMeta {
        match self {
            Field::SingleLineText(f) | Field::MultiLineText(f) => &mut f.meta,
            Field::Select(f) => &mut f.meta,
            Field::Checkbox(f) => &mut f.meta,
            Field::Submit(f) => &mut f.meta,
        }
    }

    /// True for text fields (single- or multi-line).
    pub fn is_text(&self) -> bool {
        matches!(self, Field::SingleLineText(_) | Field::MultiLineText(_))
    }

    /// True for single-line text fields specifically.
    pub fn is_single_line_text(&self) -> bool {
        matches!(self, Field::SingleLineText(_))
    }

    /// True if the field can take focus. All variants are focusable in
    /// v1; v2 may add static "presentational" rows.
    pub fn is_focusable(&self) -> bool {
        // TODO(v2): non-focusable presentational rows (headings, hints).
        let _ = self;
        true
    }
}
