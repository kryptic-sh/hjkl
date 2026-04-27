# hjkl-form

Vim-modal forms built on top of `hjkl-engine`.

Each text field hosts its own `Editor<Buffer, FormFieldHost>`, so users get the
full vim grammar (`hjkl`, `wb`, `ciw`, `dd`, ...) inside form inputs. The form
itself runs a tiny FSM over `Form-Normal` / `Form-Insert` modes for focus
navigation and validation, delegating keystrokes to the focused field's editor
when in insert mode.

Renderers live in adapter crates: `hjkl-ratatui::form::draw_form` ships the
ratatui flavor.
