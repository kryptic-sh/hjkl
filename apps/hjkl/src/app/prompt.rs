use crossterm::event::{KeyCode, KeyModifiers};
use hjkl_engine::{CursorShape, Input as EngineInput, Key as EngineKey};
use hjkl_form::TextFieldEditor;
use hjkl_prompt::{history_next, history_prev, push_history};

use super::{App, CmdLineKind, CmdLineWindow, SearchDir};
use crate::completion::{Completion, CompletionItem, CompletionKind};

/// Replace the full text of a TextFieldEditor, leaving cursor at the end in
/// Insert mode.
pub fn set_field_text(field: &mut TextFieldEditor, text: &str) {
    field.set_text(text);
    field.enter_insert_at_end();
}

/// Byte offset of the field's cursor caret within its own text: `cursor()`
/// reports the column in chars, while callers index the text by bytes (safe
/// for ASCII command lines, UTF-8-correct via char_indices for non-ASCII).
fn field_caret_byte(field: &TextFieldEditor) -> usize {
    let line = field.text();
    let (_, col) = field.cursor();
    line.char_indices().nth(col).map_or(line.len(), |(b, _)| b)
}

/// Step a prompt history on Up/Down (C-p/C-n): save the current typed input
/// on the first nav, then apply the entry at the new index to `field`
/// (falling back to the saved input when the nav leaves the history).
/// Returns `true` when history existed and the field was updated.
fn step_history(
    history: &[String],
    is_prev: bool,
    prompt_history_index: &mut Option<usize>,
    prompt_user_input: &mut Option<String>,
    field: &mut TextFieldEditor,
) -> bool {
    if history.is_empty() {
        return false;
    }
    // Save current typed input on first history nav.
    if prompt_history_index.is_none() {
        *prompt_user_input = Some(field.text());
    }
    let len = history.len();
    let new_idx = if is_prev {
        history_prev(*prompt_history_index, len)
    } else {
        history_next(*prompt_history_index, len)
    };
    *prompt_history_index = new_idx;
    let text = match new_idx {
        Some(i) => history[i].clone(),
        None => prompt_user_input.clone().unwrap_or_default(),
    };
    set_field_text(field, &text);
    true
}

/// Walk backwards from `caret` to find the start of the token under the
/// caret. A token starts at the beginning of the string or after any
/// ASCII whitespace character.
fn find_token_start(line: &str, caret: usize) -> usize {
    let bytes = line.as_bytes();
    let mut i = caret;
    while i > 0 {
        let b = bytes[i - 1];
        if b.is_ascii_whitespace() {
            break;
        }
        i -= 1;
    }
    i
}

/// Map an `hjkl_ex::CompletionKind` to a `hjkl_completion::CompletionKind`.
fn map_ex_kind(kind: hjkl_ex::CompletionKind) -> CompletionKind {
    match kind {
        hjkl_ex::CompletionKind::Command => CompletionKind::Keyword,
        hjkl_ex::CompletionKind::Path => CompletionKind::File,
        hjkl_ex::CompletionKind::Setting => CompletionKind::Variable,
        hjkl_ex::CompletionKind::SettingValue => CompletionKind::Variable,
        hjkl_ex::CompletionKind::View => CompletionKind::Variable,
        hjkl_ex::CompletionKind::Register => CompletionKind::Other,
        hjkl_ex::CompletionKind::Mark => CompletionKind::Other,
        hjkl_ex::CompletionKind::Colorscheme => CompletionKind::Variable,
        hjkl_ex::CompletionKind::Choice => CompletionKind::Keyword,
        hjkl_ex::CompletionKind::None => CompletionKind::Other,
    }
}

/// Owned data for building an [`hjkl_ex::ArgSources`].
type ArgSourcesData = (
    Option<std::path::PathBuf>, // cwd
    Vec<String>,                // settings
    Vec<String>,                // buffers
    Vec<String>,                // registers
    Vec<String>,                // marks
    Vec<String>,                // colorschemes
);

/// Build the arg sources (cwd / settings / buffers / registers / marks) for
/// use in `complete()` / `refresh_command_completion`. Extracted so both the
/// live-recompute and (optionally) the Tab path can share it.
fn build_arg_sources_data(app: &App) -> ArgSourcesData {
    let cwd = std::env::current_dir().ok();
    let settings: Vec<String> = hjkl_ex::all_setting_names();
    let buffers: Vec<String> = app
        .slots
        .iter()
        .filter_map(|s| {
            let name = s
                .filename
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            if name.is_empty() { None } else { Some(name) }
        })
        .collect();
    let registers: Vec<String> = app.active_editor().with_registers(|r| {
        let mut regs: Vec<String> = Vec::new();
        if !r.unnamed.text.is_empty() {
            regs.push("\"\"".into());
        }
        if !r.yank_zero.text.is_empty() {
            regs.push("\"0".into());
        }
        for (i, slot) in r.delete_ring.iter().enumerate() {
            if !slot.text.is_empty() {
                regs.push(format!("\"{}", i + 1));
            }
        }
        for (i, slot) in r.named.iter().enumerate() {
            if !slot.text.is_empty() {
                regs.push(format!("\"{}", (b'a' + i as u8) as char));
            }
        }
        regs
    });
    let marks: Vec<String> = app
        .active_editor()
        .marks()
        .map(|(c, _)| c.to_string())
        .collect();
    let colorschemes: Vec<String> = crate::theme::bundled_theme_names()
        .into_iter()
        .map(String::from)
        .collect();
    (cwd, settings, buffers, registers, marks, colorschemes)
}

impl App {
    pub(crate) fn open_command_prompt(&mut self) {
        let mut field = TextFieldEditor::new(true);
        field.enter_insert_at_end();
        self.command_field = Some(field);
        self.refresh_command_completion();
    }

    /// Open the command prompt with `prefill` pre-typed and the cursor at end.
    /// Used by the visual-mode `:` interceptor to seed `'<,'>` so the user
    /// can append a range-aware command like `sort`.
    pub(crate) fn open_command_prompt_with(&mut self, prefill: &str) {
        let mut field = TextFieldEditor::new(true);
        field.enter_insert_at_end();
        for c in prefill.chars() {
            let input = EngineInput {
                key: EngineKey::Char(c),
                ctrl: false,
                alt: false,
                shift: false,
            };
            field.handle_input(input);
        }
        self.command_field = Some(field);
        self.refresh_command_completion();
    }

    /// Recompute the `:` completion popup from the current field text and
    /// caret position. Called after every text-changing key while the command
    /// prompt is open.
    pub(crate) fn refresh_command_completion(&mut self) {
        if self.command_field.is_none() {
            self.completion = None;
            self.command_completion_range = None;
            return;
        }

        // Only show completion popup while in Insert mode (the user is typing
        // the command). In Normal mode they are navigating/editing the field
        // with vim motions — don't interrupt that with a popup.
        if self
            .command_field
            .as_ref()
            .is_some_and(|f| f.coarse_mode() != hjkl_form::CoarseMode::Insert)
        {
            self.completion = None;
            self.command_completion_range = None;
            return;
        }

        let field = self
            .command_field
            .as_ref()
            .expect("cmdline mode implies command_field (is_none guard above)");
        let line = field.text();
        // Convert char-indexed col to a byte index (safe for ASCII command
        // lines, UTF-8-correct via char_indices for non-ASCII).
        let caret = field_caret_byte(field);

        let host_reg = super::ex_host_cmds::host_registry();
        let editor_reg = hjkl_ex::default_registry::<crate::host::TuiHost>();
        // Supplemental command names for app-intercepted commands that live in
        // neither registry (`:map` family, `:debug`, `:b#`) — see issue #307.
        let extra_names = super::ex_dispatch::extra_ex_command_names();

        // Try command-name position first.
        let (range, metas) =
            hjkl_ex::complete_command_meta(&line, caret, &editor_reg, host_reg, &extra_names);

        if !metas.is_empty() {
            // Don't surface the popup on a bare `:` (no command name typed yet):
            // it would dump every command, intercept <C-p>/<C-n> history recall,
            // and force a second <Esc> to close the empty prompt. Wait until the
            // user has typed at least one character of a command name.
            let typed_prefix = &line[range.start..caret.min(range.end)];
            if typed_prefix.is_empty() {
                self.completion = None;
                self.command_completion_range = None;
                return;
            }
            let items: Vec<CompletionItem> = metas
                .iter()
                .map(|m| {
                    let mut item = CompletionItem::new(m.name.clone());
                    item.detail = Some(if m.usage.is_empty() {
                        "no args".to_string()
                    } else {
                        m.usage.to_string()
                    });
                    item.kind = CompletionKind::Keyword;
                    item
                })
                .collect();
            self.command_completion_range = Some(range.clone());
            let mut popup = Completion::new(0, range.start, items);
            // Filter by typed prefix so the popup highlights correctly.
            popup.set_prefix(typed_prefix);
            if popup.is_empty() {
                self.completion = None;
                self.command_completion_range = None;
                return;
            }
            self.completion = Some(popup);
            return;
        }

        // Fall back to arg-position completion.
        let (cwd, settings, buffers, registers, marks, colorschemes) = build_arg_sources_data(self);
        let sources = hjkl_ex::ArgSources {
            cwd: cwd.as_deref(),
            settings: &settings,
            buffers: &buffers,
            registers: &registers,
            marks: &marks,
            colorschemes: &colorschemes,
            // Populated inside `complete()` from the resolved command's
            // `arg_choices()` for `ArgKind::Enum` args.
            enum_choices: &[],
        };
        let comp = hjkl_ex::complete(&line, caret, &editor_reg, host_reg, &sources, &extra_names);
        if comp.kind == hjkl_ex::CompletionKind::None || comp.candidates.is_empty() {
            self.completion = None;
            self.command_completion_range = None;
            return;
        }
        let kind = map_ex_kind(comp.kind);
        let items: Vec<CompletionItem> = comp
            .candidates
            .iter()
            .map(|c| {
                let mut item = CompletionItem::new(c.clone());
                item.kind = kind;
                item
            })
            .collect();
        self.command_completion_range = Some(comp.replace_range.clone());
        let popup = Completion::new(0, comp.replace_range.start, items);
        self.completion = Some(popup);
    }

    /// Accept the currently selected item from the `:` completion popup:
    /// replaces the token in the command field and closes the popup.
    /// Does NOT execute the command — the user presses Enter again for that.
    /// Compute the command-line text that accepting the currently-selected
    /// completion candidate would produce, without mutating anything. Returns
    /// `None` when there is no popup / no selection / no command field.
    ///
    /// Shared by [`Self::accept_command_completion`] (which applies the result)
    /// and [`Self::command_accept_would_change_line`] (which compares it to the
    /// current line to decide whether Enter should accept or execute directly).
    fn computed_command_accept_text(&self) -> Option<String> {
        let popup = self.completion.as_ref()?;
        let item = popup.selected_item()?;
        let field = self.command_field.as_ref()?;
        let line = field.text();
        let range = self.command_completion_range.as_ref();
        let start = range.map_or(0, |r| r.start);
        let end = range.map_or(line.len(), |r| r.end);

        // Determine if this command takes an argument (add trailing space).
        // We check by trying to resolve the accepted label as a command name.
        let host_reg = super::ex_host_cmds::host_registry();
        let editor_reg = hjkl_ex::default_registry::<crate::host::TuiHost>();
        let takes_arg = host_reg
            .resolve(&item.label)
            .map(|c| c.arg_kind() != hjkl_ex::ArgKind::None)
            .or_else(|| {
                editor_reg
                    .resolve(&item.label)
                    .map(|c| c.arg_kind != hjkl_ex::ArgKind::None)
            })
            .unwrap_or(false);

        let suffix = if takes_arg && end >= line.len() {
            " "
        } else {
            ""
        };

        Some(format!(
            "{}{}{}{}",
            &line[..start],
            item.insert_text,
            suffix,
            &line[end.min(line.len())..],
        ))
    }

    /// `true` when accepting the selected completion would change the command
    /// line. When `false` (the line already equals the candidate — e.g. an
    /// exact match like `:wq`), Enter should execute directly instead of
    /// requiring a second press.
    pub(crate) fn command_accept_would_change_line(&self) -> bool {
        match (
            self.computed_command_accept_text(),
            self.command_field.as_ref(),
        ) {
            (Some(new_text), Some(field)) => new_text != field.text(),
            // No candidate/selection → nothing to accept → don't intercept Enter.
            _ => false,
        }
    }

    /// `true` when the leading command word of the current line resolves to a
    /// real ex command — by canonical name **or alias**. The completion popup
    /// only lists canonical names (e.g. `write`, `wall`), so a typed alias like
    /// `w` never appears as an item; without this check Enter would "accept"
    /// the top-ranked candidate (`wall`) instead of running `:w`. Aliases
    /// resolve here so `:w<Enter>` executes directly.
    pub(crate) fn command_line_is_runnable(&self) -> bool {
        // Resolve the same leading command word the completion popup operates
        // on (see [`Self::command_word_range`]). The word class is deliberately
        // ASCII-only — ex command registry names are ASCII, so a leading
        // non-ASCII-alphanumeric token can never resolve to a command.
        let Some(word) = self.command_word_range() else {
            return false;
        };
        let line = match self.command_field.as_ref() {
            Some(f) => f.text(),
            None => return false,
        };
        let token = &line[word];
        if token.is_empty() {
            return false;
        }
        let host_reg = super::ex_host_cmds::host_registry();
        let editor_reg = hjkl_ex::default_registry::<crate::host::TuiHost>();
        host_reg.resolve(token).is_some() || editor_reg.resolve(token).is_some()
    }

    /// Byte range of the leading command word — what
    /// [`Self::command_line_is_runnable`] resolves — after any range/count
    /// prefix (`%`, `2`, `.,$`). `None` when the field is closed.
    fn command_word_range(&self) -> Option<std::ops::Range<usize>> {
        let line = self.command_field.as_ref()?.text();
        // Both the prefix and the word are ASCII, so byte and char lengths
        // agree and these offsets index the line safely.
        let prefix = hjkl_ex::range_prefix_len(&line).min(line.len());
        let after = &line[prefix..];
        let ws = after.len() - after.trim_start().len();
        let word = after
            .trim_start()
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
            .count();
        let start = prefix + ws;
        Some(start..start + word)
    }

    /// `true` when the open popup is completing an ARGUMENT rather than the
    /// command name — its replace range starts past the leading command word.
    ///
    /// Enter treats the two differently. For a command name, a runnable line
    /// executes instead of accepting, so `:w<Enter>` writes rather than
    /// accepting `wall` (see [`Self::command_line_is_runnable`]). That rule
    /// must not reach argument completion: `:set foldmet`, `:colorscheme drac`
    /// and `:e src/ma` all have a runnable leading word (`set`, `colorscheme`,
    /// `e`), so Enter ran the half-typed line and the popup's selection was
    /// simply discarded unless the user had first moved off item 0.
    pub(crate) fn command_completion_is_arg(&self) -> bool {
        let (Some(range), _) = (
            self.command_completion_range.as_ref(),
            self.command_field.as_ref(),
        ) else {
            return false;
        };
        self.command_word_range()
            .is_some_and(|word| range.start > word.end)
    }

    /// `true` when the open popup is offering colorscheme names — the argument
    /// of `:colorscheme` / `:colo`. Drives the live theme preview.
    pub(crate) fn command_completion_is_colorscheme(&self) -> bool {
        if !self.command_completion_is_arg() {
            return false;
        }
        let Some(field) = self.command_field.as_ref() else {
            return false;
        };
        let line = field.text();
        let Some(word) = self.command_word_range() else {
            return false;
        };
        super::ex_host_cmds::host_registry()
            .resolve(&line[word])
            .is_some_and(|c| c.name() == "colorscheme")
    }

    /// Apply the highlighted colorscheme candidate so moving through the popup
    /// shows the theme instead of just its name. The scheme in effect when the
    /// preview started is remembered once, and
    /// [`Self::restore_previewed_theme`] puts it back if the user leaves
    /// without running the command.
    ///
    /// No-op unless the popup is colorscheme completion and the selected label
    /// is a real bundled scheme, so a stale or partial candidate can't leave
    /// the editor in a theme the user never chose.
    pub(crate) fn preview_selected_colorscheme(&mut self) {
        if !self.command_completion_is_colorscheme() {
            return;
        }
        let Some(name) = self
            .completion
            .as_ref()
            .and_then(|p| p.selected_item().map(|i| i.label.clone()))
        else {
            return;
        };
        if name == self.colorscheme {
            return;
        }
        if crate::theme::load_named(&name).is_none() {
            return;
        }
        if self.theme_preview_restore.is_none() {
            self.theme_preview_restore = Some(self.colorscheme.clone());
        }
        self.apply_named_theme(&name);
    }

    /// Put back the colorscheme that was active before the preview started.
    /// Called on every path that leaves the `:` prompt, including the one that
    /// runs the command — the command then applies its own theme, so a
    /// previewed scheme never becomes permanent by accident.
    pub(crate) fn restore_previewed_theme(&mut self) {
        let Some(name) = self.theme_preview_restore.take() else {
            return;
        };
        if name != self.colorscheme {
            self.apply_named_theme(&name);
        }
    }

    pub(crate) fn accept_command_completion(&mut self) {
        let new_text = self.computed_command_accept_text();
        // Clear popup state regardless (accept consumes it).
        self.completion = None;
        self.command_completion_range = None;
        let Some(new_text) = new_text else { return };
        if let Some(field) = self.command_field.as_mut() {
            set_field_text(field, &new_text);
        }
    }

    pub(crate) fn handle_command_field_key(&mut self, key: crossterm::event::KeyEvent) {
        // ── Tab / S-Tab ──────────────────────────────────────────────────────
        if key.code == KeyCode::Tab && !key.modifiers.contains(KeyModifiers::CONTROL) {
            // Tab-time inline expansion (%, #, <cword>) takes priority.
            if self.command_field.is_some() {
                let field = self
                    .command_field
                    .as_ref()
                    .expect("guarded by command_field.is_some() above");
                let line = field.text();
                // Expand at the REAL caret, not end-of-line — `cursor()`
                // reports the column in chars, and the token slice / splice
                // below index by bytes.
                let caret = field_caret_byte(field);
                let token_start = find_token_start(&line, caret);
                let token = &line[token_start..caret];
                if token.starts_with('%')
                    || token.starts_with('#')
                    || token.starts_with("<cword>")
                    || token.starts_with("<cWORD>")
                    || token.starts_with("<cfile>")
                {
                    let ctx = super::ex_dispatch::build_expand_context(self);
                    if let Some(expanded) = hjkl_ex::expand_filename(&ctx, token) {
                        let new_text =
                            format!("{}{}{}", &line[..token_start], expanded, &line[caret..]);
                        let field = self
                            .command_field
                            .as_mut()
                            .expect("guarded by command_field.is_some() above");
                        set_field_text(field, &new_text);
                        self.refresh_command_completion();
                        return;
                    }
                }
            }
            if let Some(ref mut popup) = self.completion {
                popup.cycle_down();
                self.preview_selected_colorscheme();
                return;
            }
            // No popup — refresh (may open one) then no-op.
            self.refresh_command_completion();
            return;
        }
        if key.code == KeyCode::BackTab {
            if let Some(ref mut popup) = self.completion {
                popup.cycle_up();
                self.preview_selected_colorscheme();
                return;
            }
            return;
        }

        // ── Up / Down / C-p / C-n ───────────────────────────────────────────
        let is_ctrl_p = key.code == KeyCode::Up
            || (key.code == KeyCode::Char('p') && key.modifiers.contains(KeyModifiers::CONTROL));
        let is_ctrl_n = key.code == KeyCode::Down
            || (key.code == KeyCode::Char('n') && key.modifiers.contains(KeyModifiers::CONTROL));

        if is_ctrl_p || is_ctrl_n {
            // If popup is open, navigate it.
            if let Some(ref mut popup) = self.completion {
                if is_ctrl_p {
                    popup.cycle_up();
                } else {
                    popup.cycle_down();
                }
                // Moving through `:colorscheme` candidates applies the theme
                // so it can be judged on the real buffer, not by its name.
                self.preview_selected_colorscheme();
                return;
            }
            // Otherwise, history navigation.
            let history = self.ex_history.clone();
            let Some(field) = self.command_field.as_mut() else {
                return;
            };
            step_history(
                &history,
                is_ctrl_p,
                &mut self.prompt_history_index,
                &mut self.prompt_user_input,
                field,
            );
            return;
        }

        // ── <C-f> mid-prompt: switch into the ex cmdline window ──────────────
        if key.code == KeyCode::Char('f') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if let Some(field) = self.command_field.take() {
                let text = field.text();
                let (_, col) = field.cursor();
                self.completion = None;
                self.command_completion_range = None;
                self.restore_previewed_theme();
                self.prompt_history_index = None;
                self.prompt_user_input = None;
                let prefill = Some((text, col));
                self.open_cmdline_window(CmdLineKind::Ex, prefill);
            }
            return;
        }

        // ── Esc / C-e: dismiss popup or close prompt ─────────────────────────
        let input: EngineInput = hjkl_engine_tui::crossterm_to_input(key);

        if input.key == EngineKey::Esc {
            // Dismiss the popup if open, but DON'T stop here — Esc must also
            // propagate to the field's normal handling (leave insert mode /
            // close prompt). A single Esc both closes the popup and steps the
            // prompt's mode, matching the buffer/LSP popup behavior.
            self.completion = None;
            self.command_completion_range = None;
            let Some(field) = self.command_field.as_mut() else {
                return;
            };
            // Esc semantics:
            //   empty field   → close the prompt outright (a single Esc on a
            //                   bare `:` dismisses everything; no popup shows on
            //                   an empty command line, so there's nothing to
            //                   step through first).
            //   Insert + text → leave Insert (one Esc both dismisses the popup
            //                   AND steps out of Insert; matches the buffer/LSP
            //                   popup behavior).
            //   Normal + text → close the prompt.
            if field.text().is_empty() {
                self.command_field = None;
                self.prompt_history_index = None;
                self.prompt_user_input = None;
                self.restore_previewed_theme();
            } else if field.coarse_mode() == hjkl_form::CoarseMode::Insert {
                field.enter_normal();
            } else {
                self.command_field = None;
                self.prompt_history_index = None;
                self.prompt_user_input = None;
                self.restore_previewed_theme();
            }
            return;
        }

        // Computed before the mutable `command_field` borrow below so it can
        // immutably inspect the popup + field together. When the popup is open,
        // Enter accepts the selected item (then a second Enter runs) — UNLESS:
        //   - accepting would be a no-op because the line already equals the
        //     selected candidate (an exact match like `:wq`), OR
        //   - the popup is completing the COMMAND NAME, the typed line is
        //     itself a runnable command (by name or alias, e.g. `:w` →
        //     `write`), and the user hasn't navigated the popup,
        // in which case we execute directly instead of accepting `:wall`.
        // Argument completion never takes that second exit: its leading word
        // is runnable by construction (`:set …`, `:e …`), so applying it there
        // threw the selection away on every Enter.
        let user_navigated = self.completion.as_ref().is_some_and(|p| p.selected != 0);
        let enter_should_accept = self.completion.is_some()
            && self.command_accept_would_change_line()
            && (self.command_completion_is_arg()
                || user_navigated
                || !self.command_line_is_runnable());

        let Some(field) = self.command_field.as_mut() else {
            return;
        };

        // ── Enter ────────────────────────────────────────────────────────────
        if input.key == EngineKey::Enter {
            if enter_should_accept {
                self.accept_command_completion();
                return;
            }
            let text = field.text();
            self.command_field = None;
            self.completion = None;
            self.command_completion_range = None;
            self.prompt_history_index = None;
            self.prompt_user_input = None;
            // Undo the preview BEFORE dispatching: the command about to run
            // decides the theme. `:colorscheme dracula` re-applies dracula a
            // line later; anything else gets the scheme the user came in with.
            self.restore_previewed_theme();
            self.dispatch_ex(text.trim());
            return;
        }

        // ── Backspace on an empty prompt dismisses it ────────────────────────
        if input.key == EngineKey::Backspace
            && self
                .command_field
                .as_ref()
                .is_some_and(|f| f.text().is_empty())
        {
            self.command_field = None;
            self.completion = None;
            self.command_completion_range = None;
            self.prompt_history_index = None;
            self.prompt_user_input = None;
            self.restore_previewed_theme();
            return;
        }

        // ── Any other key resets history navigation ──────────────────────────
        if self.prompt_history_index.is_some() {
            self.prompt_history_index = None;
            self.prompt_user_input = None;
        }

        // Still Some here: every path above that clears the field returns. Match
        // the earlier Esc/Enter guards and ignore the key rather than panic.
        let Some(field) = self.command_field.as_mut() else {
            return;
        };
        let text_changed = field.handle_input(input);
        // Recompute popup live when text actually changed.
        if text_changed {
            self.refresh_command_completion();
        }
    }

    pub(crate) fn open_search_prompt(&mut self, dir: SearchDir) {
        let mut field = TextFieldEditor::new(true);
        field.enter_insert_at_end();
        self.search_field = Some(field);
        self.search_dir = dir;
        self.active_editor_mut().set_search_pattern(None);
    }

    /// Restore the search pattern from the last committed search, clearing it
    /// when there is none or it no longer compiles. Both [`Self::cancel_search_prompt`]
    /// and the `<C-f>` search-field branch cancel the live-preview side-effect
    /// this way.
    fn restore_last_search_pattern(&mut self) {
        let last = self.active_editor().last_search();
        match last {
            Some(p) if !p.is_empty() => {
                if let Ok(re) = regex::Regex::new(&p) {
                    self.active_editor_mut().set_search_pattern(Some(re));
                } else {
                    self.active_editor_mut().set_search_pattern(None);
                }
            }
            _ => self.active_editor_mut().set_search_pattern(None),
        }
    }

    pub(crate) fn cancel_search_prompt(&mut self) {
        self.search_field = None;
        self.restore_last_search_pattern();
    }

    pub(crate) fn handle_search_field_key(&mut self, key: crossterm::event::KeyEvent) {
        let is_ctrl_p = key.code == KeyCode::Up
            || (key.code == KeyCode::Char('p') && key.modifiers.contains(KeyModifiers::CONTROL));
        let is_ctrl_n = key.code == KeyCode::Down
            || (key.code == KeyCode::Char('n') && key.modifiers.contains(KeyModifiers::CONTROL));

        if is_ctrl_p || is_ctrl_n {
            let history = if self.search_dir == SearchDir::Forward {
                self.search_history_forward.clone()
            } else {
                self.search_history_backward.clone()
            };
            let Some(field) = self.search_field.as_mut() else {
                return;
            };
            if step_history(
                &history,
                is_ctrl_p,
                &mut self.prompt_history_index,
                &mut self.prompt_user_input,
                field,
            ) {
                self.live_preview_search();
            }
            return;
        }

        // <C-f> mid-prompt: switch into the matching search cmdline window
        // (issue #132). Capture text + cursor col, close the search prompt
        // WITHOUT committing or updating the last-search pattern, then open
        // q/ or q? with the in-progress text as the trailing line.
        if key.code == KeyCode::Char('f') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if let Some(field) = self.search_field.take() {
                let text = field.text();
                let (_, col) = field.cursor();
                self.prompt_history_index = None;
                self.prompt_user_input = None;
                // Restore the previous pattern (cancel live-preview side-effect).
                self.restore_last_search_pattern();
                let win_kind = match self.search_dir {
                    SearchDir::Forward => CmdLineKind::SearchForward,
                    SearchDir::Backward => CmdLineKind::SearchBackward,
                };
                let prefill = Some((text, col));
                self.open_cmdline_window(win_kind, prefill);
            }
            return;
        }

        let input: EngineInput = hjkl_engine_tui::crossterm_to_input(key);
        let Some(field) = self.search_field.as_mut() else {
            return;
        };

        if input.key == EngineKey::Enter {
            let pattern = field.text();
            self.search_field = None;
            self.prompt_history_index = None;
            self.prompt_user_input = None;
            self.commit_search(&pattern);
            return;
        }

        if input.key == EngineKey::Esc {
            if field.text().is_empty() {
                self.prompt_history_index = None;
                self.prompt_user_input = None;
                self.cancel_search_prompt();
                return;
            }
            if field.coarse_mode() == hjkl_form::CoarseMode::Insert {
                field.enter_normal();
            } else {
                self.prompt_history_index = None;
                self.prompt_user_input = None;
                self.cancel_search_prompt();
            }
            return;
        }

        // Backspace on an empty prompt dismisses it.
        if input.key == EngineKey::Backspace && field.text().is_empty() {
            self.prompt_history_index = None;
            self.prompt_user_input = None;
            self.cancel_search_prompt();
            return;
        }

        // Any non-history key resets history navigation.
        if self.prompt_history_index.is_some() {
            self.prompt_history_index = None;
            self.prompt_user_input = None;
        }

        let dirty = field.handle_input(input);
        if dirty {
            self.live_preview_search();
        }
    }

    pub(crate) fn live_preview_search(&mut self) {
        // `:set noincsearch` — no preview while typing. The pattern is armed
        // only on submit (`commit_search`), which is vim's behaviour: the
        // buffer stays exactly as it was until Enter.
        if !self.active_editor().settings().incsearch {
            return;
        }
        let pattern = match self.search_field.as_ref() {
            Some(f) => f.text(),
            None => return,
        };
        if pattern.is_empty() {
            self.active_editor_mut().set_search_pattern(None);
            return;
        }
        let case_insensitive = self.active_editor().settings().ignore_case
            && !(self.active_editor().settings().smartcase
                && pattern.chars().any(|c| c.is_uppercase()));
        let effective: std::borrow::Cow<'_, str> = if case_insensitive {
            std::borrow::Cow::Owned(format!("(?i){pattern}"))
        } else {
            std::borrow::Cow::Borrowed(pattern.as_str())
        };
        match regex::Regex::new(&effective) {
            Ok(re) => self.active_editor_mut().set_search_pattern(Some(re)),
            Err(_) => self.active_editor_mut().set_search_pattern(None),
        }
    }

    pub(crate) fn commit_search(&mut self, pattern: &str) {
        let effective: Option<String> = if pattern.is_empty() {
            self.active_editor().last_search()
        } else {
            Some(pattern.to_owned())
        };
        let Some(p) = effective else {
            self.active_editor_mut().set_search_pattern(None);
            return;
        };
        let case_insensitive = self.active_editor().settings().ignore_case
            && !(self.active_editor().settings().smartcase && p.chars().any(|c| c.is_uppercase()));
        let compile_src: std::borrow::Cow<'_, str> = if case_insensitive {
            std::borrow::Cow::Owned(format!("(?i){p}"))
        } else {
            std::borrow::Cow::Borrowed(p.as_str())
        };
        match regex::Regex::new(&compile_src) {
            Ok(re) => {
                self.active_editor_mut().set_search_pattern(Some(re));
                let forward = self.search_dir == SearchDir::Forward;
                if forward {
                    self.active_editor_mut().search_advance_forward(false);
                } else {
                    self.active_editor_mut().search_advance_backward(true);
                }
                self.active_editor_mut().ensure_cursor_in_scrolloff();
                self.sync_viewport_from_editor();
                self.active_editor_mut()
                    .set_last_search(Some(p.clone()), forward);
                if forward {
                    push_history(&mut self.search_history_forward, &p);
                } else {
                    push_history(&mut self.search_history_backward, &p);
                }
            }
            Err(e) => {
                self.active_editor_mut().set_search_pattern(None);
                self.bus.error(format!("E: bad search pattern: {e}"));
            }
        }
    }

    /// Open the `!` filter prompt for the row range `(top, bot)` (inclusive).
    /// The user types a shell command; on Enter the range is piped through it.
    pub(crate) fn open_filter_prompt(&mut self, top: usize, bot: usize) {
        let mut field = hjkl_form::TextFieldEditor::new(true);
        field.enter_insert_at_end();
        self.filter_field = Some(field);
        self.filter_pending_range = Some((top, bot));
    }

    /// Handle a key event while the `!` filter prompt is active.
    pub(crate) fn handle_filter_field_key(&mut self, key: crossterm::event::KeyEvent) {
        let input: EngineInput = hjkl_engine_tui::crossterm_to_input(key);
        let Some(field) = self.filter_field.as_mut() else {
            return;
        };

        if input.key == EngineKey::Enter {
            let command = field.text();
            let range = self.filter_pending_range.take();
            self.filter_field = None;
            if let Some((top, bot)) = range {
                let result = self
                    .active_editor_mut()
                    .filter_range(top, bot, command.trim(), None);
                match result {
                    Ok(()) => {
                        self.sync_after_engine_mutation();
                    }
                    Err(msg) => {
                        self.bus.error(format!("filter: {msg}"));
                    }
                }
            }
            return;
        }

        if input.key == EngineKey::Esc {
            if field.text().is_empty() {
                self.filter_field = None;
                self.filter_pending_range = None;
            } else if field.coarse_mode() == hjkl_form::CoarseMode::Insert {
                field.enter_normal();
            } else {
                self.filter_field = None;
                self.filter_pending_range = None;
            }
            return;
        }

        // Backspace on an empty prompt dismisses it.
        if input.key == EngineKey::Backspace && field.text().is_empty() {
            self.filter_field = None;
            self.filter_pending_range = None;
            return;
        }

        field.handle_input(input);
    }

    /// Dispatch a prompt-opening [`crate::keymap_actions::AppAction`].
    ///
    /// Handles variants:
    ///   - OpenCommandPrompt (`:` — open the ex command prompt)
    ///   - OpenSearchPrompt  (`/` / `?` — open incremental search)
    pub(crate) fn dispatch_prompt_action(&mut self, action: crate::keymap_actions::AppAction) {
        use crate::keymap_actions::AppAction;
        match action {
            AppAction::OpenCommandPrompt if self.pending_state.is_none() => {
                self.open_command_prompt();
            }
            AppAction::OpenCommandPrompt => {}
            AppAction::OpenSearchPrompt(dir) => {
                self.open_search_prompt(dir);
            }
            _ => {}
        }
    }
}

/// Resolve the cursor shape for an active prompt field (`command_field` or
/// `search_field`). Insert mode → Bar; anything else → Block.
pub fn prompt_cursor_shape(field: &TextFieldEditor) -> CursorShape {
    match field.coarse_mode() {
        hjkl_form::CoarseMode::Insert => CursorShape::Bar,
        _ => CursorShape::Block,
    }
}

// ── Command-line window (issue #37) ──────────────────────────────────────────

impl App {
    /// Open the command-line window for `kind` (`q:` / `q/` / `q?`).
    ///
    /// `prefill` — when `Some((text, col))`, appends `text` as a trailing line
    /// after the history rows and positions the cursor at `(last_row, col)`.
    /// Used by `<C-f>` mid-prompt to carry in-progress text into the window
    /// (issue #132). Pass `None` for the normal `q:` / `q/` / `q?` path.
    pub(crate) fn open_cmdline_window(
        &mut self,
        kind: CmdLineKind,
        prefill: Option<(String, usize)>,
    ) {
        use crate::app::window::{LayoutTree, SplitDir, Window};
        use hjkl_buffer::View;
        use hjkl_engine::BufferEdit;

        if self.cmdline_win.is_some() {
            return;
        }

        let history: Vec<String> = match kind {
            CmdLineKind::Ex => self.ex_history.clone(),
            CmdLineKind::SearchForward => self.search_history_forward.clone(),
            CmdLineKind::SearchBackward => self.search_history_backward.clone(),
        };

        // Build buffer content: history lines + optional prefill line.
        let content = if let Some((ref text, _)) = prefill {
            if history.is_empty() {
                text.clone()
            } else {
                format!("{}\n{}", history.join("\n"), text)
            }
        } else {
            history.join("\n")
        };

        let buffer_id = self.next_buffer_id;
        self.next_buffer_id += 1;
        let mut view = View::new();
        if !content.is_empty() {
            BufferEdit::replace_all(&mut view, &content);
        }
        // No manual cursor placement / viewport sizing / editor-channel drain
        // here (#151 Stage 2b — see `build_slot`'s doc): this slot has no
        // Editor, and `seed_window_editor` below places the real cursor on
        // the window editor once it exists, superseding any placement here.

        let slot = super::BufferSlot {
            // The history scratch buffer is no more a user buffer than the
            // explorer's (#63 Phase 4): `kind` keeps it out of `:ls` / `:bn`
            // / the buffer line, where `slot_is_special` used to compare
            // against `cmdline_win.slot_idx` — a positional index that is
            // never re-indexed when another slot is removed.
            kind: super::BufKind::CmdLine,
            is_new_file: true,
            ..super::BufferSlot::new(buffer_id, view, hjkl_engine::Settings::default())
        };
        self.slots.push(slot);
        let slot_idx = self.slots.len() - 1;

        // Win height accounts for the prefill line too.
        let total_lines = history.len() + if prefill.is_some() { 1 } else { 0 };
        let win_rows = (total_lines + 1).clamp(1, 7);

        // Split off a REGULAR window, never a dock: the cmdline window would
        // otherwise land inside the explorer's fixed column, sized by the
        // explorer's config width, and would break the "explorer is the root
        // split's first child" shape `install_bottom_dock` keys the quickfix
        // dock's placement off (#63 Phase 3). Same reroute `:sp` does.
        self.focus_editor_window_for_open();
        let focused = self.focused_window();
        let new_win_id = self.next_window_id;
        self.next_window_id += 1;
        // The window snapshot's cursor position must match where we placed the
        // editor cursor — sync_viewport_to_editor() restores from this snapshot.
        let (win_cursor_row, win_cursor_col) = if let Some((_, col)) = prefill {
            // Prefill adds one extra line after the history rows.
            (history.len(), col)
        } else {
            (history.len().saturating_sub(1), 0)
        };
        self.windows.push(Some(Window::new(slot_idx)));
        self.reconcile_window_editors();
        self.seed_window_editor(new_win_id, win_cursor_row, win_cursor_col, 0, 0);

        let total_h = crossterm::terminal::size()
            .map_or(24, |(_, h)| h as usize)
            .saturating_sub(1);
        let ratio_b = (win_rows as f32 / total_h as f32).clamp(0.05, 0.45);
        let ratio_a = 1.0 - ratio_b;

        // Save current window's state before the layout changes.
        self.sync_viewport_from_editor();
        self.layout_mut()
            .replace_leaf(focused, move |id| LayoutTree::Split {
                dir: SplitDir::Horizontal,
                ratio: ratio_a,
                fixed: None,
                a: Box::new(LayoutTree::Leaf(id)),
                b: Box::new(LayoutTree::Leaf(new_win_id)),
                last_rect: None,
            });

        // Focus the new cmdline window and restore its snapshot.
        self.set_focused_window(new_win_id);
        self.sync_viewport_to_editor();

        self.cmdline_win = Some(CmdLineWindow {
            win_id: new_win_id,
            kind,
        });
    }

    /// Close the command-line window (without executing the current line).
    pub(crate) fn close_cmdline_window(&mut self) {
        let Some(cw) = self.cmdline_win.take() else {
            return;
        };
        let Ok(new_focus) = self.layout_mut().remove_leaf(cw.win_id) else {
            return;
        };
        // Remove the slot this window ACTUALLY points at. The old code
        // removed the creation-time index, which is positional: close the
        // explorer while `q:` is open and every later slot shifts down one,
        // so the recorded index names a different buffer (removing the wrong
        // one) or runs off the end (leaking the history scratch slot for the
        // rest of the session). `dispose_dock_window` already reads the
        // window's own `slot` for exactly this reason; the recorded index is
        // gone from `CmdLineWindow` entirely so it can't drift again.
        let slot_idx = self.windows[cw.win_id].as_ref().map(|w| w.slot);
        self.windows[cw.win_id] = None;
        if let Some(slot_idx) = slot_idx
            && slot_idx < self.slots.len()
        {
            self.slots.remove(slot_idx);
            // Shared with the dock teardown: fixes up every window's `slot`
            // AND `prev_active`, which the hand-rolled loop here used to
            // leave pointing one slot too far right after the removal.
            self.reindex_after_slot_removal(slot_idx);
        }
        // The closed cmdline window is already gone; just restore the new focus.
        self.set_focused_window(new_focus);
        self.sync_viewport_to_editor();
    }

    /// Execute the line at the cursor in the command-line window, then close it.
    pub(crate) fn commit_cmdline_window(&mut self) {
        let Some(cw) = self.cmdline_win.clone() else {
            return;
        };
        let line_text = {
            // Cursor lives on the cmdline window's own editor (#151).
            let (row, _) = self.window_cursor(cw.win_id);
            let ed = self.window_editor(cw.win_id);
            let rope = ed.buffer().rope();
            if row < rope.len_lines() {
                hjkl_buffer::rope_line_str(&rope, row)
            } else {
                String::new()
            }
        };
        self.close_cmdline_window();

        let text = line_text.trim().to_string();
        if text.is_empty() {
            return;
        }
        match cw.kind {
            CmdLineKind::Ex => {
                self.dispatch_ex(&text);
            }
            CmdLineKind::SearchForward => {
                self.search_dir = SearchDir::Forward;
                self.commit_search(&text);
            }
            CmdLineKind::SearchBackward => {
                self.search_dir = SearchDir::Backward;
                self.commit_search(&text);
            }
        }
    }

    /// Returns `true` if the currently focused window is the command-line window.
    pub(crate) fn is_cmdline_win_focused(&self) -> bool {
        self.cmdline_win
            .as_ref()
            .is_some_and(|cw| cw.win_id == self.focused_window())
    }
}
