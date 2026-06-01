use crossterm::event::{KeyCode, KeyModifiers};
use hjkl_engine::{CursorShape, Input as EngineInput, Key as EngineKey, VimMode};
use hjkl_form::TextFieldEditor;
use hjkl_prompt::{history_next, history_prev, push_history};

use super::{App, CmdLineKind, CmdLineWindow, STATUS_LINE_HEIGHT, SearchDir};
use crate::completion::{Completion, CompletionItem, CompletionKind};

/// Replace the full text of a TextFieldEditor, leaving cursor at the end in
/// Insert mode.
pub(crate) fn set_field_text(field: &mut TextFieldEditor, text: &str) {
    field.set_text(text);
    field.enter_insert_at_end();
}

/// Build an [`hjkl_ex::ExpandContext`] from app state for tab-time inline
/// expansion. Same wiring as `build_expand_context` in ex_dispatch.rs.
/// cword/cwword stay None (see TODO in ex_dispatch.rs).
fn build_inline_expand_context(app: &App) -> hjkl_ex::ExpandContext<'_> {
    let alt_path = app
        .prev_active
        .and_then(|i| app.slots.get(i))
        .and_then(|s| s.filename.as_deref());

    hjkl_ex::ExpandContext {
        current_path: app.active().filename.as_deref(),
        alt_path,
        cword: None,
        cwword: None,
        cwd: None,
    }
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
        hjkl_ex::CompletionKind::Buffer => CompletionKind::Variable,
        hjkl_ex::CompletionKind::Register => CompletionKind::Other,
        hjkl_ex::CompletionKind::Mark => CompletionKind::Other,
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
    let registers: Vec<String> = {
        let r = app.active().editor.registers();
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
    };
    let marks: Vec<String> = app
        .active()
        .editor
        .marks()
        .map(|(c, _)| c.to_string())
        .collect();
    (cwd, settings, buffers, registers, marks)
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
            .map(|f| f.vim_mode() != hjkl_form::VimMode::Insert)
            .unwrap_or(false)
        {
            self.completion = None;
            self.command_completion_range = None;
            return;
        }

        let line = self.command_field.as_ref().unwrap().text();
        let (_, col) = self.command_field.as_ref().unwrap().cursor();
        // Convert char-indexed col to a byte index (safe for ASCII command
        // lines, UTF-8-correct via char_indices for non-ASCII).
        let caret = line
            .char_indices()
            .nth(col)
            .map(|(b, _)| b)
            .unwrap_or(line.len());

        let host_reg = super::ex_host_cmds::host_registry();
        let editor_reg = hjkl_ex::default_registry::<crate::host::TuiHost>();

        // Try command-name position first.
        let (range, metas) = hjkl_ex::complete_command_meta(&line, caret, &editor_reg, host_reg);

        if !metas.is_empty() {
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
            let typed_prefix = &line[range.start..caret.min(range.end)];
            if !typed_prefix.is_empty() {
                popup.set_prefix(typed_prefix);
                if popup.is_empty() {
                    self.completion = None;
                    self.command_completion_range = None;
                    return;
                }
            }
            self.completion = Some(popup);
            return;
        }

        // Fall back to arg-position completion.
        let (cwd, settings, buffers, registers, marks) = build_arg_sources_data(self);
        let sources = hjkl_ex::ArgSources {
            cwd: cwd.as_deref(),
            settings: &settings,
            buffers: &buffers,
            registers: &registers,
            marks: &marks,
        };
        let comp = hjkl_ex::complete(&line, caret, &editor_reg, host_reg, &sources);
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
        let start = range.map(|r| r.start).unwrap_or(0);
        let end = range.map(|r| r.end).unwrap_or(line.len());

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
                let line = self.command_field.as_ref().unwrap().text();
                let caret = line.len();
                let token_start = find_token_start(&line, caret);
                let token = &line[token_start..caret];
                if token.starts_with('%')
                    || token.starts_with('#')
                    || token.starts_with("<cword>")
                    || token.starts_with("<cWORD>")
                {
                    let ctx = build_inline_expand_context(self);
                    if let Some(expanded) = hjkl_ex::expand_filename(&ctx, token) {
                        let new_text =
                            format!("{}{}{}", &line[..token_start], expanded, &line[caret..]);
                        let field = self.command_field.as_mut().unwrap();
                        set_field_text(field, &new_text);
                        self.refresh_command_completion();
                        return;
                    }
                }
            }
            if let Some(ref mut popup) = self.completion {
                popup.select_next();
                return;
            }
            // No popup — refresh (may open one) then no-op.
            self.refresh_command_completion();
            return;
        }
        if key.code == KeyCode::BackTab {
            if let Some(ref mut popup) = self.completion {
                popup.select_prev();
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
                    popup.select_prev();
                } else {
                    popup.select_next();
                }
                return;
            }
            // Otherwise, history navigation.
            let history = self.ex_history.clone();
            if !history.is_empty() {
                // Save current typed input on first history nav.
                if self.prompt_history_index.is_none() {
                    let cur = self
                        .command_field
                        .as_ref()
                        .map(|f| f.text())
                        .unwrap_or_default();
                    self.prompt_user_input = Some(cur);
                }
                let len = history.len();
                let new_idx = if is_ctrl_p {
                    history_prev(self.prompt_history_index, len)
                } else {
                    history_next(self.prompt_history_index, len)
                };
                self.prompt_history_index = new_idx;
                let text = match new_idx {
                    Some(i) => history[i].clone(),
                    None => self.prompt_user_input.clone().unwrap_or_default(),
                };
                if let Some(f) = self.command_field.as_mut() {
                    set_field_text(f, &text);
                }
            }
            return;
        }

        // ── <C-f> mid-prompt: switch into the ex cmdline window ──────────────
        if key.code == KeyCode::Char('f') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if let Some(field) = self.command_field.take() {
                let text = field.text();
                let (_, col) = field.cursor();
                self.completion = None;
                self.command_completion_range = None;
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
            let field = match self.command_field.as_mut() {
                Some(f) => f,
                None => return,
            };
            // Behavior keyed purely on vim mode (matches buffer/LSP popup):
            //   Insert → leave Insert (one Esc both dismisses the popup AND
            //            steps out of Insert; no second press needed).
            //   Normal → close the prompt.
            if field.vim_mode() == VimMode::Insert {
                field.enter_normal();
            } else {
                self.command_field = None;
                self.prompt_history_index = None;
                self.prompt_user_input = None;
            }
            return;
        }

        // Computed before the mutable `command_field` borrow below so it can
        // immutably inspect the popup + field together. When the popup is open,
        // Enter accepts the selected item (then a second Enter runs) — UNLESS
        // accepting would be a no-op because the line already equals the
        // selected candidate (e.g. an exact match like `:wq`), in which case we
        // execute directly instead of requiring a second press.
        let enter_should_accept =
            self.completion.is_some() && self.command_accept_would_change_line();

        let field = match self.command_field.as_mut() {
            Some(f) => f,
            None => return,
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
            return;
        }

        // ── Any other key resets history navigation ──────────────────────────
        if self.prompt_history_index.is_some() {
            self.prompt_history_index = None;
            self.prompt_user_input = None;
        }

        let field = self.command_field.as_mut().unwrap();
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
        self.active_mut().editor.set_search_pattern(None);
    }

    pub(crate) fn cancel_search_prompt(&mut self) {
        self.search_field = None;
        let last = self.active().editor.last_search().map(str::to_owned);
        match last {
            Some(p) if !p.is_empty() => {
                if let Ok(re) = regex::Regex::new(&p) {
                    self.active_mut().editor.set_search_pattern(Some(re));
                } else {
                    self.active_mut().editor.set_search_pattern(None);
                }
            }
            _ => self.active_mut().editor.set_search_pattern(None),
        }
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
            if !history.is_empty() {
                if self.prompt_history_index.is_none() {
                    let cur = self
                        .search_field
                        .as_ref()
                        .map(|f| f.text())
                        .unwrap_or_default();
                    self.prompt_user_input = Some(cur);
                }
                let len = history.len();
                let new_idx = if is_ctrl_p {
                    history_prev(self.prompt_history_index, len)
                } else {
                    history_next(self.prompt_history_index, len)
                };
                self.prompt_history_index = new_idx;
                let text = match new_idx {
                    Some(i) => history[i].clone(),
                    None => self.prompt_user_input.clone().unwrap_or_default(),
                };
                if let Some(f) = self.search_field.as_mut() {
                    set_field_text(f, &text);
                }
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
                let last = self.active().editor.last_search().map(str::to_owned);
                match last {
                    Some(p) if !p.is_empty() => {
                        if let Ok(re) = regex::Regex::new(&p) {
                            self.active_mut().editor.set_search_pattern(Some(re));
                        } else {
                            self.active_mut().editor.set_search_pattern(None);
                        }
                    }
                    _ => self.active_mut().editor.set_search_pattern(None),
                }
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
        let field = match self.search_field.as_mut() {
            Some(f) => f,
            None => return,
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
            if field.vim_mode() == VimMode::Insert {
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
        let pattern = match self.search_field.as_ref() {
            Some(f) => f.text(),
            None => return,
        };
        if pattern.is_empty() {
            self.active_mut().editor.set_search_pattern(None);
            return;
        }
        let case_insensitive = self.active().editor.settings().ignore_case
            && !(self.active().editor.settings().smartcase
                && pattern.chars().any(|c| c.is_uppercase()));
        let effective: std::borrow::Cow<'_, str> = if case_insensitive {
            std::borrow::Cow::Owned(format!("(?i){pattern}"))
        } else {
            std::borrow::Cow::Borrowed(pattern.as_str())
        };
        match regex::Regex::new(&effective) {
            Ok(re) => self.active_mut().editor.set_search_pattern(Some(re)),
            Err(_) => self.active_mut().editor.set_search_pattern(None),
        }
    }

    pub(crate) fn commit_search(&mut self, pattern: &str) {
        let effective: Option<String> = if pattern.is_empty() {
            self.active().editor.last_search().map(str::to_owned)
        } else {
            Some(pattern.to_owned())
        };
        let Some(p) = effective else {
            self.active_mut().editor.set_search_pattern(None);
            return;
        };
        let case_insensitive = self.active().editor.settings().ignore_case
            && !(self.active().editor.settings().smartcase && p.chars().any(|c| c.is_uppercase()));
        let compile_src: std::borrow::Cow<'_, str> = if case_insensitive {
            std::borrow::Cow::Owned(format!("(?i){p}"))
        } else {
            std::borrow::Cow::Borrowed(p.as_str())
        };
        match regex::Regex::new(&compile_src) {
            Ok(re) => {
                self.active_mut().editor.set_search_pattern(Some(re));
                let forward = self.search_dir == SearchDir::Forward;
                if forward {
                    self.active_mut().editor.search_advance_forward(false);
                } else {
                    self.active_mut().editor.search_advance_backward(true);
                }
                self.active_mut().editor.ensure_cursor_in_scrolloff();
                self.sync_viewport_from_editor();
                self.active_mut()
                    .editor
                    .set_last_search(Some(p.clone()), forward);
                if forward {
                    push_history(&mut self.search_history_forward, &p);
                } else {
                    push_history(&mut self.search_history_backward, &p);
                }
            }
            Err(e) => {
                self.active_mut().editor.set_search_pattern(None);
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
        let field = match self.filter_field.as_mut() {
            Some(f) => f,
            None => return,
        };

        if input.key == EngineKey::Enter {
            let command = field.text();
            let range = self.filter_pending_range.take();
            self.filter_field = None;
            if let Some((top, bot)) = range {
                let result = self
                    .active_mut()
                    .editor
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
            } else if field.vim_mode() == VimMode::Insert {
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
pub(crate) fn prompt_cursor_shape(field: &TextFieldEditor) -> CursorShape {
    match field.vim_mode() {
        hjkl_form::VimMode::Insert => CursorShape::Bar,
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
        use crate::host::TuiHost;
        use hjkl_buffer::Buffer;
        use hjkl_engine::{BufferEdit, Editor, Host, Options};
        use std::time::Instant;

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
        let host = TuiHost::new();
        let mut editor = Editor::new(Buffer::new(), host, Options::default());
        if let Ok(size) = crossterm::terminal::size() {
            let h = size.1.saturating_sub(STATUS_LINE_HEIGHT);
            {
                let vp = editor.host_mut().viewport_mut();
                vp.width = size.0;
                vp.height = h;
            }
            editor.set_viewport_height(h);
        }
        if !content.is_empty() {
            BufferEdit::replace_all(editor.buffer_mut(), &content);
        }
        let line_count = editor.buffer().row_count();
        // Position cursor: when prefill is Some, land at (last_row, prefill_col);
        // otherwise land at last history row col 0 (existing behaviour).
        let (cursor_row, cursor_col) = if let Some((_, col)) = prefill {
            (line_count.saturating_sub(1), col)
        } else {
            (line_count.saturating_sub(1), 0)
        };
        editor.jump_cursor(cursor_row, cursor_col);
        let _ = editor.take_content_edits();
        let _ = editor.take_content_reset();

        let slot = super::BufferSlot {
            buffer_id,
            editor,
            filename: None,
            dirty: false,
            is_new_file: true,
            is_untracked: false,
            diag_signs: Vec::new(),
            diag_signs_lsp: Vec::new(),
            lsp_diags: Vec::new(),
            last_lsp_dirty_gen: None,
            git_signs: Vec::new(),
            last_git_dirty_gen: None,
            last_git_refresh_at: Instant::now(),
            blame: Vec::new(),
            last_blame_dirty_gen: None,
            last_blame_refresh_at: Instant::now(),
            blame_column: false,
            saved_hash: 0,
            saved_len: 0,
            signature_cache: None,
            disk_mtime: None,
            disk_len: None,
            disk_state: crate::app::DiskState::Synced,
            swap_path: None,
            last_swap_dirty_gen: None,
            last_fold_dirty_gen: None,
        };
        self.slots.push(slot);
        let slot_idx = self.slots.len() - 1;

        // Win height accounts for the prefill line too.
        let total_lines = history.len() + if prefill.is_some() { 1 } else { 0 };
        let win_rows = (total_lines + 1).clamp(1, 7);

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
        self.windows.push(Some(Window::with_scroll(
            slot_idx,
            0,
            0,
            win_cursor_row,
            win_cursor_col,
        )));

        let total_h = crossterm::terminal::size()
            .map(|(_, h)| h as usize)
            .unwrap_or(24)
            .saturating_sub(1);
        let ratio_b = (win_rows as f32 / total_h as f32).clamp(0.05, 0.45);
        let ratio_a = 1.0 - ratio_b;

        // Save current window's state before the layout changes.
        self.sync_viewport_from_editor();
        self.layout_mut()
            .replace_leaf(focused, move |id| LayoutTree::Split {
                dir: SplitDir::Horizontal,
                ratio: ratio_a,
                a: Box::new(LayoutTree::Leaf(id)),
                b: Box::new(LayoutTree::Leaf(new_win_id)),
                last_rect: None,
            });

        // Focus the new cmdline window and restore its snapshot.
        self.set_focused_window(new_win_id);
        self.sync_viewport_to_editor();

        self.cmdline_win = Some(CmdLineWindow {
            win_id: new_win_id,
            slot_idx,
            kind,
        });
    }

    /// Close the command-line window (without executing the current line).
    pub(crate) fn close_cmdline_window(&mut self) {
        let Some(cw) = self.cmdline_win.take() else {
            return;
        };
        let new_focus = match self.layout_mut().remove_leaf(cw.win_id) {
            Ok(f) => f,
            Err(_) => return,
        };
        self.windows[cw.win_id] = None;
        let slot_idx = cw.slot_idx;
        if slot_idx < self.slots.len() {
            self.slots.remove(slot_idx);
            let slot_count = self.slots.len();
            for win in self.windows.iter_mut().flatten() {
                if win.slot == slot_idx {
                    win.slot = 0;
                } else if win.slot > slot_idx {
                    win.slot -= 1;
                }
                win.slot = win.slot.min(slot_count.saturating_sub(1));
            }
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
            let slot = &self.slots[cw.slot_idx];
            let (row, _) = slot.editor.cursor();
            {
                let rope = slot.editor.buffer().rope();
                if row < rope.len_lines() {
                    hjkl_buffer::rope_line_str(&rope, row)
                } else {
                    String::new()
                }
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
