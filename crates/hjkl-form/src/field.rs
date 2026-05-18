//! Field types — the building blocks of a `Form`.

use crate::host::FormFieldHost;
use crate::validate::Validator;
use hjkl_buffer::Buffer;
use hjkl_engine::{Editor, Host, Input, Key, Options, VimMode};

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
///
/// Two construction paths:
///
/// - [`TextFieldEditor::with_meta`] — used by [`crate::Form`] to wire a
///   field with label / validator / placeholder metadata.
/// - [`TextFieldEditor::new`] / [`TextFieldEditor::with_text`] — the
///   standalone primitive: a vim-grammar one-line (or N-line) prompt
///   without a surrounding form. Used by hosts that need just the
///   editing surface (`:` command palette, `/` `?` search prompt, etc.).
///
/// In standalone single-line mode, `Enter` is **swallowed** by
/// [`TextFieldEditor::handle_input`]: there is no "next field" to jump
/// to, and the surrounding host typically interprets `Enter` as
/// "submit / commit" via its own dispatcher before the keystroke ever
/// reaches the field. This keeps the buffer single-line by construction.
pub struct TextFieldEditor {
    pub meta: FieldMeta,
    pub editor: Editor<Buffer, FormFieldHost>,
    pub validator: Option<Validator>,
    /// Visible body height for multi-line fields. Single-line is 1.
    pub rows: u16,
    /// True when the field is single-line — gates Enter swallowing in
    /// the standalone `handle_input` path and label rendering choices.
    pub(crate) single_line: bool,
    /// `dirty_gen` of the buffer at the moment the user entered Insert
    /// on this field. Used to decide whether a `Changed` event fires
    /// on Esc.
    pub(crate) enter_gen: u64,
}

impl TextFieldEditor {
    /// Build a standalone vim-grammar text field with empty buffer.
    /// `single_line=true` suppresses Enter from inserting newlines and
    /// sets `rows=1`; multi-line fields default to `rows=3`. Hosts that
    /// want a different multi-line height should bump `rows` afterwards
    /// or use [`TextFieldEditor::with_meta`].
    pub fn new(single_line: bool) -> Self {
        let buffer = Buffer::new();
        let host = FormFieldHost::new();
        let editor = Editor::new(buffer, host, Options::default());
        Self {
            meta: FieldMeta::new(""),
            editor,
            validator: None,
            rows: if single_line { 1 } else { 3 },
            single_line,
            enter_gen: 0,
        }
    }

    /// Standalone variant pre-populated with `text`. Cursor lands at the
    /// end of the inserted content; mode is `Normal`.
    pub fn with_text(text: &str, single_line: bool) -> Self {
        let mut me = Self::new(single_line);
        me.set_text(text);
        me
    }

    /// Form-style constructor: full metadata + render-height. The label
    /// / placeholder / required marker render via
    /// [`crate::Form`]'s ratatui adapter.
    pub fn with_meta(meta: FieldMeta, rows: u16) -> Self {
        let buffer = Buffer::new();
        let host = FormFieldHost::new();
        let editor = Editor::new(buffer, host, Options::default());
        Self {
            meta,
            editor,
            validator: None,
            single_line: rows <= 1,
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

    /// Borrow the underlying [`Buffer`] for span rendering, snapshots,
    /// or other read-only consumers.
    pub fn buffer(&self) -> &Buffer {
        self.editor.buffer()
    }

    /// Mutable buffer access. Rare — prefer [`TextFieldEditor::handle_input`]
    /// which routes through the vim FSM and keeps the cursor consistent.
    pub fn buffer_mut(&mut self) -> &mut Buffer {
        self.editor.buffer_mut()
    }

    /// Snapshot the buffer's current text. Multi-line buffers join with
    /// `'\n'`.
    pub fn text(&self) -> String {
        self.editor.buffer().as_string()
    }

    /// Replace contents wholesale, e.g. when opening a prompt with a
    /// preset value. Drops the inner editor and rebuilds it; cursor
    /// lands at the end of the new content and mode is `Normal`.
    pub fn set_text(&mut self, text: &str) {
        let buffer = Buffer::from_str(text);
        let host = FormFieldHost::new();
        self.editor = Editor::new(buffer, host, Options::default());
        // Land cursor at end-of-text so `enter_insert_at_end` puts the
        // caret right after the last character.
        let lines = self.editor.buffer().lines();
        if let Some(last) = lines.last() {
            let row = lines.len().saturating_sub(1);
            let col = last.chars().count();
            self.editor
                .buffer_mut()
                .set_cursor(hjkl_buffer::Position::new(row, col));
        }
    }

    /// Cursor position as `(row, col)` in chars. Use directly to place
    /// the terminal cursor in the prompt's render rect.
    pub fn cursor(&self) -> (usize, usize) {
        self.editor.cursor()
    }

    /// Current vim mode of the inner editor (Normal / Insert / Visual / ...).
    pub fn vim_mode(&self) -> VimMode {
        self.editor.vim_mode()
    }

    /// Force the inner editor into Insert mode at the end of the
    /// current line. Used by hosts that open a prompt and want the user
    /// typing immediately.
    pub fn enter_insert_at_end(&mut self) {
        // Move cursor to end of last line.
        let lines = self.editor.buffer().lines().to_vec();
        let row = lines.len().saturating_sub(1);
        let col = lines.last().map(|s| s.chars().count()).unwrap_or(0);
        self.editor
            .buffer_mut()
            .set_cursor(hjkl_buffer::Position::new(row, col));
        // Normalise FSM to Normal first so `enter_insert_shift_a` cleanly
        // transitions (it expects Normal mode as the entry point).
        self.editor.force_normal();
        // `A` (append at end of line) puts the editor in Insert at EOL —
        // exactly the entry point we want for prompts.
        self.editor.enter_insert_shift_a(1);
    }

    /// Force the inner editor back to Normal mode (Esc).
    pub fn enter_normal(&mut self) {
        self.editor.force_normal();
    }

    /// Forward a key event to the inner editor's vim FSM. Returns
    /// `true` when the buffer's `dirty_gen` advanced — useful for
    /// triggering incremental search on `/` `?` prompts.
    ///
    /// In single-line standalone mode, `Enter` while in Insert is
    /// swallowed: there is no next field to jump to. Hosts intercept
    /// `Enter` upstream as "submit", so the field never sees it in
    /// practice; this guard is the belt-and-suspenders.
    pub fn handle_input(&mut self, input: Input) -> bool {
        // Single-line: drop newline-producing Enter in Insert mode.
        if self.single_line && input.key == Key::Enter && self.editor.vim_mode() == VimMode::Insert
        {
            return false;
        }
        let before = self.editor.buffer().dirty_gen();
        hjkl_vim::dispatch_input(&mut self.editor, input);
        self.editor.buffer().dirty_gen() != before
    }

    /// Buffer dirty generation — bumps on every content edit.
    pub fn dirty_gen(&self) -> u64 {
        self.editor.buffer().dirty_gen()
    }

    /// Set the field's host viewport width. The renderer should call
    /// this every frame so motions / scroll stay in-bounds.
    pub fn set_viewport_width(&mut self, width: u16) {
        self.editor.host_mut().viewport_mut().width = width;
    }

    /// Set the field's host viewport height. Single-line fields pass 1.
    pub fn set_viewport_height(&mut self, height: u16) {
        self.editor.host_mut().viewport_mut().height = height;
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

#[cfg(test)]
mod standalone_tests {
    //! Tests for the standalone `TextFieldEditor` API used by host
    //! prompts (`:` palette, `/` `?` search). The form-side path is
    //! covered by `crate::fsm::tests`.

    use super::*;

    fn ki(c: char) -> Input {
        Input {
            key: Key::Char(c),
            ..Input::default()
        }
    }

    #[test]
    fn text_round_trips_via_set_text() {
        let mut f = TextFieldEditor::new(true);
        f.set_text("hello");
        assert_eq!(f.text(), "hello");
    }

    #[test]
    fn with_text_constructor_pre_populates() {
        let f = TextFieldEditor::with_text("abc", true);
        assert_eq!(f.text(), "abc");
    }

    #[test]
    fn handle_input_i_enters_insert() {
        let mut f = TextFieldEditor::new(true);
        f.handle_input(ki('i'));
        assert_eq!(f.vim_mode(), VimMode::Insert);
    }

    #[test]
    fn handle_input_types_and_esc_returns_to_normal() {
        let mut f = TextFieldEditor::new(true);
        f.handle_input(ki('i'));
        f.handle_input(ki('h'));
        f.handle_input(ki('i'));
        f.handle_input(Input {
            key: Key::Esc,
            ..Input::default()
        });
        assert_eq!(f.text(), "hi");
        assert_eq!(f.vim_mode(), VimMode::Normal);
    }

    #[test]
    fn dirty_gen_advances_after_insert() {
        let mut f = TextFieldEditor::new(true);
        let before = f.dirty_gen();
        f.handle_input(ki('i'));
        f.handle_input(ki('x'));
        assert!(f.dirty_gen() > before);
    }

    #[test]
    fn enter_insert_at_end_lands_cursor_at_eol() {
        let mut f = TextFieldEditor::with_text("abc", true);
        f.enter_insert_at_end();
        let (row, col) = f.cursor();
        assert_eq!(row, 0);
        // After `A` + Insert mode, cursor is past the last char.
        assert_eq!(col, 3);
        assert_eq!(f.vim_mode(), VimMode::Insert);
    }

    #[test]
    fn single_line_swallows_enter_in_insert() {
        let mut f = TextFieldEditor::new(true);
        f.enter_insert_at_end();
        let dirty = f.handle_input(Input {
            key: Key::Enter,
            ..Input::default()
        });
        assert!(!dirty, "Enter must not mutate buffer in single-line Insert");
        assert_eq!(f.text(), "");
    }

    #[test]
    fn multi_line_accepts_enter_in_insert() {
        let mut f = TextFieldEditor::new(false);
        f.enter_insert_at_end();
        f.handle_input(ki('a'));
        f.handle_input(Input {
            key: Key::Enter,
            ..Input::default()
        });
        f.handle_input(ki('b'));
        assert_eq!(f.text(), "a\nb");
    }
}
