# hjkl-form

Vim-modal forms built on top of `hjkl-engine` — full vim grammar inside every
text field.

[![CI](https://github.com/kryptic-sh/hjkl-form/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl-form/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hjkl-form.svg)](https://crates.io/crates/hjkl-form)
[![docs.rs](https://img.shields.io/docsrs/hjkl-form)](https://docs.rs/hjkl-form)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Website](https://img.shields.io/badge/website-hjkl.kryptic.sh-7ee787)](https://hjkl.kryptic.sh)

Each text field hosts its own `Editor<Buffer, FormFieldHost>`, so users get the
full vim grammar (`hjkl`, `wb`, `ciw`, `dd`, ...) inside form inputs. The form
itself runs a tiny FSM over `Form-Normal` / `Form-Insert` modes for focus
navigation, validation, and submit dispatch — keystrokes delegate to the focused
field's editor when the form is in Insert mode.

Renderers live in adapter crates: `hjkl-ratatui::form::draw_form` ships the
ratatui flavor.

## Status

`dirty_gen` aggregates buffer mutations and form-level focus changes; renderers
can cheap-skip frames when it hasn't advanced.

## Usage

```toml
hjkl-form = "0.3"
```

```rust,no_run
use hjkl_form::{
    Field, FieldMeta, Form, FormEvent, SubmitField, SubmitOutcome, TextFieldEditor,
};

let mut name = TextFieldEditor::with_meta(FieldMeta::new("Name").required(true), 1);
name.validator = Some(Box::new(|s: &str| {
    if s.is_empty() {
        Err("name is required".into())
    } else {
        Ok(())
    }
}));

let mut form = Form::new()
    .with_title("New user")
    .with_field(Field::SingleLineText(name))
    .with_field(Field::Submit(SubmitField::new(FieldMeta::new("Create"))))
    .with_submit(Box::new(|| {
        // Persist, fire HTTP request, etc.
        SubmitOutcome::Ok
    }));

// Drive with `Form::handle_input(input)` from your event loop, then call
// `hjkl_ratatui::form::draw_form(...)` per frame.
let _ = form.handle_input(hjkl_engine::Input::default());
let _ = FormEvent::Changed;
```

## Key bindings

| Mode        | Key       | Action                           |
| ----------- | --------- | -------------------------------- |
| Form-Normal | `j`/`Tab` | Focus next field                 |
| Form-Normal | `k`       | Focus previous field             |
| Form-Normal | `gg`      | Focus first                      |
| Form-Normal | `G`       | Focus last                       |
| Form-Normal | `i`       | Enter Insert on a text field     |
| Form-Normal | `Space`   | Toggle checkbox / fire submit    |
| Form-Normal | `h`/`l`   | Cycle select / motion in text    |
| Form-Normal | `Enter`   | Fire submit on Submit field      |
| Form-Normal | `Esc`     | Emit `FormEvent::Cancelled`      |
| Form-Insert | `Enter`   | Jump to next field (single-line) |
| Form-Insert | `Esc`     | Return to Form-Normal            |

## License

MIT. See [LICENSE](LICENSE).
