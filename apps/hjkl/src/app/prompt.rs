use crossterm::event::{KeyCode, KeyModifiers};
use hjkl_engine::{Input as EngineInput, Key as EngineKey, VimMode};
use hjkl_form::TextFieldEditor;

use super::{App, SearchDir};

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

/// Active wildmenu state for the command-line prompt. `None` outside
/// completion (no Tab pressed yet, or after acceptance/cancel).
#[derive(Clone, Debug)]
pub(crate) struct CommandCompletion {
    /// Original typed text the user can revert to with <Esc>.
    pub original: String,
    /// Sorted, dedup'd candidate strings.
    pub candidates: Vec<String>,
    /// Currently selected candidate index, or None on initial Tab when
    /// we replaced with the longest common prefix (no specific selection yet).
    pub selected: Option<usize>,
    /// Byte range in the field text that the candidate replaces.
    pub replace_range: std::ops::Range<usize>,
}

/// Replace the full text of a TextFieldEditor, leaving cursor at the end in
/// Insert mode. Uses the public `set_text` method (rebuilds the inner editor)
/// then calls `enter_insert_at_end`.
fn set_field_text(field: &mut TextFieldEditor, text: &str) {
    field.set_text(text);
    field.enter_insert_at_end();
}

impl App {
    pub(crate) fn open_command_prompt(&mut self) {
        let mut field = TextFieldEditor::new(true);
        field.enter_insert_at_end();
        self.command_field = Some(field);
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
    }

    pub(crate) fn handle_command_field_key(&mut self, key: crossterm::event::KeyEvent) {
        // Intercept Tab / S-Tab BEFORE converting to EngineInput.
        // crossterm sends KeyCode::BackTab for Shift+Tab (terminal sends \x1b[Z).
        if key.code == KeyCode::Tab && !key.modifiers.contains(KeyModifiers::CONTROL) {
            self.advance_command_completion(true);
            return;
        }
        if key.code == KeyCode::BackTab {
            self.advance_command_completion(false);
            return;
        }

        let input: EngineInput = key.into();
        let field = match self.command_field.as_mut() {
            Some(f) => f,
            None => return,
        };

        if input.key == EngineKey::Enter {
            let text = field.text();
            self.command_field = None;
            self.command_completion = None;
            self.dispatch_ex(text.trim());
            return;
        }

        if input.key == EngineKey::Esc {
            if let Some(comp) = self.command_completion.take() {
                // Revert field text to the original typed text.
                let field = self.command_field.as_mut().unwrap();
                set_field_text(field, &comp.original);
                return;
            }
            let field = self.command_field.as_mut().unwrap();
            if field.text().is_empty() {
                self.command_field = None;
            } else if field.vim_mode() == VimMode::Insert {
                field.enter_normal();
            } else {
                self.command_field = None;
            }
            return;
        }

        // Any other key while completion is active: commit current candidate
        // (field text already has it) and clear completion state.
        if self.command_completion.is_some() {
            self.command_completion = None;
        }

        let field = self.command_field.as_mut().unwrap();
        field.handle_input(input);
    }

    /// Advance (or initialize) wildmenu completion state.
    /// `forward=true` means Tab (next); `false` means S-Tab (prev).
    pub(crate) fn advance_command_completion(&mut self, forward: bool) {
        if self.command_field.is_none() {
            return;
        }

        if let Some(comp) = self.command_completion.as_mut() {
            // Cycling phase — bump selected index.
            if comp.candidates.is_empty() {
                return;
            }
            let n = comp.candidates.len();
            let new_idx = match comp.selected {
                None => {
                    if forward {
                        0
                    } else {
                        n - 1
                    }
                }
                Some(i) if forward => (i + 1) % n,
                Some(i) => (i + n - 1) % n,
            };
            comp.selected = Some(new_idx);
            let candidate = comp.candidates[new_idx].clone();
            let field = self.command_field.as_mut().unwrap();
            let new_text = format!("{}{}", &field.text()[..comp.replace_range.start], candidate);
            let new_replace_end = comp.replace_range.start + candidate.len();
            comp.replace_range = comp.replace_range.start..new_replace_end;
            set_field_text(field, &new_text);
            return;
        }

        // First Tab — compute completion.
        let line = {
            let field = self.command_field.as_ref().unwrap();
            field.text()
        };
        let caret = line.len(); // caret at end for completion

        // Phase 7: tab-time inline expansion. If the token under the caret
        // starts with a filename-expansion prefix (`%`, `#`, `<cword>`,
        // `<cWORD>`), expand it in place so the user sees the literal path
        // before pressing Enter.
        {
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
                    return; // don't fall through to candidate completion
                }
            }
        }

        let host_reg = super::ex_host_cmds::host_registry();
        let editor_reg = hjkl_ex::default_registry::<crate::host::TuiHost>();

        // Build arg sources.
        let cwd = std::env::current_dir().ok();
        let settings: Vec<String> = hjkl_ex::all_setting_names();
        let buffers: Vec<String> = self
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
        // TODO(phase6): wire register names from live editor state.
        let registers: Vec<String> = {
            let r = self.active().editor.registers();
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
        let marks: Vec<String> = self
            .active()
            .editor
            .marks()
            .map(|(c, _)| c.to_string())
            .collect();
        let sources = hjkl_ex::ArgSources {
            cwd: cwd.as_deref(),
            settings: &settings,
            buffers: &buffers,
            registers: &registers,
            marks: &marks,
        };
        let comp = hjkl_ex::complete(&line, caret, &editor_reg, host_reg, &sources);
        if comp.candidates.is_empty() {
            return;
        }
        let original = line.clone();
        if comp.candidates.len() == 1 {
            // Single match — insert fully, close menu.
            let cand = comp.candidates[0].clone();
            let new_text = format!("{}{}", &line[..comp.replace_range.start], cand);
            let field = self.command_field.as_mut().unwrap();
            set_field_text(field, &new_text);
            return; // no command_completion stored — menu stays closed
        }
        // Multiple matches — replace with longest common prefix and store state.
        let lcp = hjkl_ex::longest_common_prefix(&comp.candidates);
        let prefix_text = if lcp.len() > comp.replace_range.len() {
            format!("{}{}", &line[..comp.replace_range.start], lcp)
        } else {
            line.clone()
        };
        {
            let field = self.command_field.as_mut().unwrap();
            set_field_text(field, &prefix_text);
        }
        self.command_completion = Some(CommandCompletion {
            original,
            candidates: comp.candidates,
            selected: None,
            replace_range: comp.replace_range,
        });
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
        let input: EngineInput = key.into();
        let field = match self.search_field.as_mut() {
            Some(f) => f,
            None => return,
        };

        if input.key == EngineKey::Enter {
            let pattern = field.text();
            self.search_field = None;
            self.commit_search(&pattern);
            return;
        }

        if input.key == EngineKey::Esc {
            if field.text().is_empty() {
                self.cancel_search_prompt();
                return;
            }
            if field.vim_mode() == VimMode::Insert {
                field.enter_normal();
            } else {
                self.cancel_search_prompt();
            }
            return;
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
                // Vim semantics for the / and ? prompts are asymmetric:
                //   /<pat><CR> — searches AT-OR-AFTER the cursor (cursor
                //                stays on the match if it's already on one)
                //   ?<pat><CR> — searches strictly BEFORE the cursor
                //                (always moves to a previous match)
                // skip_current=false on forward prevents /<CR> from
                // double-stepping past the cursor's match (counter went
                // 0/3 → 2/3 because the cursor advanced past M1).
                // skip_current=true on backward keeps the existing /?:
                // behavior of jumping to the previous match.
                if forward {
                    self.active_mut().editor.search_advance_forward(false);
                } else {
                    self.active_mut().editor.search_advance_backward(true);
                }
                // search_advance_* moves the cursor without going through
                // the engine's vim::step end-of-step hook, so the viewport
                // doesn't auto-scroll. Reveal the cursor + sync the
                // focused window's stored top_row so the next render
                // shows the match instead of the old viewport.
                self.active_mut().editor.ensure_cursor_in_scrolloff();
                self.sync_viewport_from_editor();
                self.active_mut().editor.set_last_search(Some(p), forward);
            }
            Err(e) => {
                self.active_mut().editor.set_search_pattern(None);
                self.status_message = Some(format!("E: bad search pattern: {e}"));
            }
        }
    }
}
