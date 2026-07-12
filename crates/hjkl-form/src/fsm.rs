//! Form-level FSM. Routes keys between focus navigation, field
//! delegation, and submit firing.

use crate::field::Field;
use crate::form::{Form, FormEvent, FormMode};
use crate::validate::validate_field;
use hjkl_engine::{CoarseMode, Input, Key};

impl Form {
    /// Route a key event through the form. Returns an event if the
    /// keystroke produced one (focus moved, value changed, submit
    /// fired, etc.).
    pub fn handle_input(&mut self, input: Input) -> Option<FormEvent> {
        if self.fields.is_empty() {
            return None;
        }
        match self.mode {
            FormMode::Normal => self.handle_normal(input),
            FormMode::Insert => self.handle_insert(input),
        }
    }

    fn handle_normal(&mut self, input: Input) -> Option<FormEvent> {
        // `gg` is the only two-key chord we model at the form level —
        // every other input clears the pending state.
        let was_pending_g = self.pending_g;
        if input.key != Key::Char('g') || input.ctrl || input.alt {
            self.pending_g = false;
        }

        // Esc cancels the form (host decides whether to close).
        if input.key == Key::Esc {
            return Some(FormEvent::Cancelled);
        }

        // Focus-navigation keys that work uniformly regardless of
        // focused-field type.
        if let Some(ev) = self.try_navigate(input, was_pending_g) {
            return Some(ev);
        }

        // Per-field-type handling.
        let field_kind = field_kind(&self.fields[self.focused]);
        match field_kind {
            FieldKind::Checkbox => self.handle_normal_checkbox(input),
            FieldKind::Select => self.handle_normal_select(input),
            FieldKind::Submit => self.handle_normal_submit(input),
            FieldKind::SingleLineText | FieldKind::MultiLineText => {
                self.handle_normal_text(input, field_kind == FieldKind::SingleLineText)
            }
        }
    }

    fn try_navigate(&mut self, input: Input, was_pending_g: bool) -> Option<FormEvent> {
        let len = self.fields.len();
        let mut moved = false;
        let prev = self.focused;
        match input.key {
            // BackTab on most terminals comes through as Tab + shift;
            // crossterm-bridged inputs use Key::Tab with shift. This arm
            // must precede the forward Tab arm, whose guard does not
            // exclude shift.
            Key::Tab if input.shift => {
                self.focused = self.focused.saturating_sub(1);
                moved = true;
            }
            Key::Char('j') | Key::Down | Key::Tab if !input.ctrl && !input.alt => {
                self.focused = (self.focused + 1) % len;
                moved = true;
            }
            Key::Char('k') | Key::Up if !input.ctrl && !input.alt => {
                self.focused = self.focused.saturating_sub(1);
                moved = true;
            }
            Key::Char('g') if !input.ctrl && !input.alt && !input.shift => {
                if was_pending_g {
                    self.focused = 0;
                    moved = true;
                } else {
                    self.pending_g = true;
                    return None;
                }
            }
            Key::Char('G') if !input.ctrl && !input.alt => {
                self.focused = len - 1;
                moved = true;
            }
            _ => {}
        }
        if moved {
            if prev != self.focused {
                // Blur: force the previous text field back to Normal so it
                // isn't left stuck in Visual (e.g. after `v`/`V`) or Insert
                // when the user navigates away with j/k.
                if let Field::SingleLineText(f) | Field::MultiLineText(f) = &mut self.fields[prev] {
                    f.enter_normal();
                }
                // Run blur validator on the previous field.
                validate_field(&mut self.fields[prev]);
                self.bump_dirty();
            }
            return Some(FormEvent::Changed);
        }
        None
    }

    fn handle_normal_checkbox(&mut self, input: Input) -> Option<FormEvent> {
        match input.key {
            Key::Char(' ') | Key::Enter => {
                if let Field::Checkbox(c) = &mut self.fields[self.focused] {
                    c.value = !c.value;
                }
                self.bump_dirty();
                Some(FormEvent::Changed)
            }
            _ => None,
        }
    }

    fn handle_normal_select(&mut self, input: Input) -> Option<FormEvent> {
        match input.key {
            Key::Char('l') | Key::Right if !input.ctrl && !input.alt => {
                if let Field::Select(s) = &mut self.fields[self.focused]
                    && !s.options.is_empty()
                {
                    s.index = (s.index + 1) % s.options.len();
                }
                self.bump_dirty();
                Some(FormEvent::Changed)
            }
            Key::Char('h') | Key::Left if !input.ctrl && !input.alt => {
                if let Field::Select(s) = &mut self.fields[self.focused]
                    && !s.options.is_empty()
                {
                    s.index = if s.index == 0 {
                        s.options.len() - 1
                    } else {
                        s.index - 1
                    };
                }
                self.bump_dirty();
                Some(FormEvent::Changed)
            }
            _ => None,
        }
    }

    fn handle_normal_submit(&mut self, input: Input) -> Option<FormEvent> {
        if input.key == Key::Enter || (input.key == Key::Char(' ') && !input.ctrl && !input.alt) {
            return Some(self.try_submit_event());
        }
        None
    }

    fn handle_normal_text(&mut self, input: Input, _single_line: bool) -> Option<FormEvent> {
        // Forward the key to the inner editor; if it lands in Insert
        // (i/I/a/A, but also o/O, s/S, C, cw, ...), mirror that at the
        // form level so subsequent keys are delegated instead of being
        // treated as focus navigation.
        if let Field::SingleLineText(f) | Field::MultiLineText(f) = &mut self.fields[self.focused] {
            let prev_gen_before = f.editor.buffer().dirty_gen();
            hjkl_vim::dispatch_input(&mut f.editor, input);
            if f.editor.coarse_mode() == CoarseMode::Insert {
                f.enter_gen = prev_gen_before;
                self.mode = FormMode::Insert;
                return Some(FormEvent::Changed);
            }
        }
        // Other motion keys (h/l/w/b/etc) just forwarded; emit Changed
        // so renderers refresh cursor.
        Some(FormEvent::Changed)
    }

    fn handle_insert(&mut self, input: Input) -> Option<FormEvent> {
        // Resync guard: if focus somehow landed on a non-text field while
        // the form is in Insert (e.g. a host called `set_focus`), fall
        // back to Normal so navigation and Esc keep working.
        if !self.fields[self.focused].is_text() {
            self.mode = FormMode::Normal;
            return self.handle_normal(input);
        }

        // Single-line text: Enter jumps to next field instead of
        // inserting a newline.
        let single = matches!(self.fields[self.focused], Field::SingleLineText(_));
        if single && input.key == Key::Enter {
            let len = self.fields.len();
            if self.focused + 1 < len {
                let prev = self.focused;
                self.focused += 1;
                // Leave the previous field's editor in Normal so later
                // form-Normal keys don't leak into its Insert mode.
                if let Field::SingleLineText(f) | Field::MultiLineText(f) = &mut self.fields[prev] {
                    f.enter_normal();
                }
                validate_field(&mut self.fields[prev]);
                // Keep form-Insert only when the next field is text —
                // and put its editor into Insert so keystrokes insert
                // instead of running Normal-mode commands. Otherwise
                // drop to form-Normal to match the focused field kind.
                match &mut self.fields[self.focused] {
                    Field::SingleLineText(f) | Field::MultiLineText(f) => {
                        f.enter_gen = f.editor.buffer().dirty_gen();
                        f.enter_insert_at_end();
                    }
                    _ => self.mode = FormMode::Normal,
                }
                self.bump_dirty();
            }
            return Some(FormEvent::Changed);
        }

        // Forward to the focused field's editor.
        if let Field::SingleLineText(f) | Field::MultiLineText(f) = &mut self.fields[self.focused] {
            let before_gen = f.editor.buffer().dirty_gen();
            hjkl_vim::dispatch_input(&mut f.editor, input);
            let after_mode = f.editor.coarse_mode();
            let after_gen = f.editor.buffer().dirty_gen();
            if after_mode == CoarseMode::Normal {
                // User pressed Esc — leave insert mode.
                self.mode = FormMode::Normal;
                let prev_focus = self.focused;
                validate_field(&mut self.fields[prev_focus]);
                self.bump_dirty();
                return Some(FormEvent::Changed);
            }
            if after_gen != before_gen {
                self.bump_dirty();
                return Some(FormEvent::Changed);
            }
        }
        None
    }

    /// Run all validators; if all pass, fire the submit closure.
    /// Returns the appropriate `FormEvent`.
    fn try_submit_event(&mut self) -> FormEvent {
        match self.try_submit() {
            Some(outcome) => FormEvent::Submitted(outcome),
            None => FormEvent::ValidationFailed,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FieldKind {
    SingleLineText,
    MultiLineText,
    Select,
    Checkbox,
    Submit,
}

fn field_kind(field: &Field) -> FieldKind {
    match field {
        Field::SingleLineText(_) => FieldKind::SingleLineText,
        Field::MultiLineText(_) => FieldKind::MultiLineText,
        Field::Select(_) => FieldKind::Select,
        Field::Checkbox(_) => FieldKind::Checkbox,
        Field::Submit(_) => FieldKind::Submit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::field::{CheckboxField, FieldMeta, SelectField, SubmitField, TextFieldEditor};
    use crate::submit::SubmitOutcome;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn key(c: char) -> Input {
        Input {
            key: Key::Char(c),
            ..Input::default()
        }
    }

    fn special(k: Key) -> Input {
        Input {
            key: k,
            ..Input::default()
        }
    }

    fn make_form() -> Form {
        Form::new()
            .with_field(Field::SingleLineText(TextFieldEditor::with_meta(
                FieldMeta::new("Name"),
                1,
            )))
            .with_field(Field::SingleLineText(TextFieldEditor::with_meta(
                FieldMeta::new("Email"),
                1,
            )))
            .with_field(Field::Checkbox(CheckboxField::new(FieldMeta::new("Save"))))
            .with_field(Field::Select(SelectField::new(
                FieldMeta::new("Format"),
                vec!["json".into(), "yaml".into(), "toml".into()],
            )))
            .with_field(Field::Submit(SubmitField::new(FieldMeta::new("Submit"))))
    }

    #[test]
    fn j_advances_focus() {
        let mut form = make_form();
        form.handle_input(key('j'));
        assert_eq!(form.focused(), 1);
    }

    #[test]
    fn blur_resets_field_from_visual_to_normal() {
        let mut form = make_form();
        // Enter Visual on the focused (field 0) text editor.
        form.handle_input(key('v'));
        let in_visual = matches!(
            &form.fields[0],
            Field::SingleLineText(f) | Field::MultiLineText(f) if f.coarse_mode() != CoarseMode::Normal
        );
        assert!(
            in_visual,
            "`v` must put the field editor in a non-Normal mode"
        );

        // Navigate away with `j`.
        form.handle_input(key('j'));
        assert_eq!(form.focused(), 1);

        // The blurred field must be reset to Normal, not left in Visual.
        match &form.fields[0] {
            Field::SingleLineText(f) | Field::MultiLineText(f) => {
                assert_eq!(
                    f.coarse_mode(),
                    CoarseMode::Normal,
                    "blurred field must reset to Normal"
                );
            }
            _ => panic!("field 0 must be a text field"),
        }
    }

    #[test]
    fn j_past_end_wraps_to_zero() {
        let mut form = make_form();
        for _ in 0..form.fields.len() {
            form.handle_input(key('j'));
        }
        assert_eq!(form.focused(), 0);
    }

    #[test]
    fn k_past_zero_saturates() {
        let mut form = make_form();
        form.handle_input(key('k'));
        assert_eq!(form.focused(), 0);
    }

    #[test]
    fn gg_jumps_to_zero() {
        let mut form = make_form();
        form.handle_input(key('j'));
        form.handle_input(key('j'));
        assert_eq!(form.focused(), 2);
        form.handle_input(key('g'));
        form.handle_input(key('g'));
        assert_eq!(form.focused(), 0);
    }

    #[test]
    fn capital_g_jumps_to_last() {
        let mut form = make_form();
        let last = form.fields.len() - 1;
        form.handle_input(Input {
            key: Key::Char('G'),
            shift: true,
            ..Input::default()
        });
        assert_eq!(form.focused(), last);
    }

    #[test]
    fn i_on_text_enters_insert() {
        let mut form = make_form();
        form.handle_input(key('i'));
        assert_eq!(form.mode, FormMode::Insert);
    }

    #[test]
    fn esc_after_insert_returns_to_normal() {
        let mut form =
            make_form().with_field(Field::Submit(SubmitField::new(FieldMeta::new("Sub"))));
        // Enter insert, type a char, then Esc.
        form.handle_input(key('i'));
        assert_eq!(form.mode, FormMode::Insert);
        form.handle_input(key('x'));
        form.handle_input(special(Key::Esc));
        assert_eq!(form.mode, FormMode::Normal);
    }

    #[test]
    fn enter_on_submit_fires_submit_fn() {
        let fired = Arc::new(AtomicBool::new(false));
        let f2 = fired.clone();
        let mut form = make_form().with_submit(Box::new(move || {
            f2.store(true, Ordering::SeqCst);
            SubmitOutcome::Ok
        }));
        // Jump to last (Submit) field.
        form.handle_input(Input {
            key: Key::Char('G'),
            shift: true,
            ..Input::default()
        });
        let ev = form.handle_input(special(Key::Enter));
        assert!(matches!(ev, Some(FormEvent::Submitted(SubmitOutcome::Ok))));
        assert!(fired.load(Ordering::SeqCst));
    }

    #[test]
    fn submit_with_failing_validator_does_not_fire() {
        let fired = Arc::new(AtomicBool::new(false));
        let f2 = fired.clone();
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
            .with_field(Field::Submit(SubmitField::new(FieldMeta::new("Submit"))))
            .with_submit(Box::new(move || {
                f2.store(true, Ordering::SeqCst);
                SubmitOutcome::Ok
            }));
        form.handle_input(key('j'));
        let ev = form.handle_input(special(Key::Enter));
        assert!(matches!(ev, Some(FormEvent::ValidationFailed)));
        assert!(!fired.load(Ordering::SeqCst));
    }

    #[test]
    fn enter_in_insert_jumps_focus_to_next() {
        let mut form = make_form();
        form.handle_input(key('i'));
        assert_eq!(form.mode, FormMode::Insert);
        form.handle_input(special(Key::Enter));
        assert_eq!(form.focused(), 1);
        // Stays in Insert after the focus jump.
        assert_eq!(form.mode, FormMode::Insert);
    }

    #[test]
    fn shift_tab_moves_focus_backwards() {
        let mut form = make_form();
        form.handle_input(key('j'));
        form.handle_input(key('j'));
        assert_eq!(form.focused(), 2);
        form.handle_input(Input {
            key: Key::Tab,
            shift: true,
            ..Input::default()
        });
        assert_eq!(form.focused(), 1, "Shift-Tab must move focus backwards");
    }

    #[test]
    fn enter_in_insert_puts_next_text_field_in_insert() {
        let mut form = make_form();
        form.handle_input(key('i'));
        form.handle_input(key('a')); // type into Name
        form.handle_input(special(Key::Enter)); // jump to Email
        assert_eq!(form.focused(), 1);
        assert_eq!(form.mode, FormMode::Insert);
        // Typing must insert into Email, not run Normal-mode commands.
        form.handle_input(key('x'));
        if let Field::SingleLineText(f) = &form.fields[1] {
            assert_eq!(f.text(), "x");
        } else {
            panic!("expected text field");
        }
        // The previous field's inner editor must be back in Normal.
        if let Field::SingleLineText(f) = &form.fields[0] {
            assert_eq!(f.coarse_mode(), CoarseMode::Normal);
        } else {
            panic!("expected text field");
        }
    }

    #[test]
    fn enter_in_insert_onto_non_text_field_returns_to_normal() {
        let mut form = make_form();
        // Focus Email (index 1) — the field before the checkbox.
        form.handle_input(key('j'));
        form.handle_input(key('i'));
        assert_eq!(form.mode, FormMode::Insert);
        form.handle_input(special(Key::Enter)); // jump to checkbox
        assert_eq!(form.focused(), 2);
        assert_eq!(
            form.mode,
            FormMode::Normal,
            "form must not stay in Insert on a non-text field"
        );
        // Sanity: form still responds (no soft-lock) — Esc cancels.
        let ev = form.handle_input(special(Key::Esc));
        assert!(matches!(ev, Some(FormEvent::Cancelled)));
    }

    #[test]
    fn o_on_text_field_enters_insert_mode() {
        let mut form = make_form();
        form.handle_input(key('o'));
        assert_eq!(
            form.mode,
            FormMode::Insert,
            "any Normal command landing in editor-Insert must switch the form to Insert"
        );
    }

    #[test]
    fn checkbox_toggles_on_space() {
        let mut form = make_form();
        // Move to checkbox at index 2.
        form.handle_input(key('j'));
        form.handle_input(key('j'));
        assert_eq!(form.focused(), 2);
        form.handle_input(key(' '));
        if let Field::Checkbox(c) = &form.fields[2] {
            assert!(c.value);
        } else {
            panic!("expected checkbox");
        }
    }

    #[test]
    fn select_cycles_on_h_l() {
        let mut form = make_form();
        // Move to select at index 3.
        for _ in 0..3 {
            form.handle_input(key('j'));
        }
        assert_eq!(form.focused(), 3);
        form.handle_input(key('l'));
        if let Field::Select(s) = &form.fields[3] {
            assert_eq!(s.index, 1);
        } else {
            panic!("expected select");
        }
        form.handle_input(key('h'));
        form.handle_input(key('h'));
        if let Field::Select(s) = &form.fields[3] {
            assert_eq!(s.index, 2); // wrapped
        } else {
            panic!("expected select");
        }
    }
}
