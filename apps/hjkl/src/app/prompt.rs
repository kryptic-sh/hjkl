use crossterm::event::{KeyCode, KeyModifiers};
use hjkl_buffer::Buffer;
use hjkl_engine::{
    BufferEdit, CursorShape, Editor, Host, Input as EngineInput, Key as EngineKey, Options, VimMode,
};
use hjkl_form::TextFieldEditor;

use super::{App, CmdLineKind, CmdLineWindow, STATUS_LINE_HEIGHT, SearchDir};

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

        // Phase 2 (#37): Ctrl-P / Up → previous history entry.
        let is_ctrl_p = key.code == KeyCode::Up
            || (key.code == KeyCode::Char('p') && key.modifiers.contains(KeyModifiers::CONTROL));
        let is_ctrl_n = key.code == KeyCode::Down
            || (key.code == KeyCode::Char('n') && key.modifiers.contains(KeyModifiers::CONTROL));

        if is_ctrl_p || is_ctrl_n {
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
                    match self.prompt_history_index {
                        None => Some(len - 1),
                        Some(0) => Some(0), // clamp at oldest
                        Some(i) => Some(i - 1),
                    }
                } else {
                    // Ctrl-N
                    match self.prompt_history_index {
                        None => None,
                        Some(i) if i + 1 >= len => None, // past newest → restore
                        Some(i) => Some(i + 1),
                    }
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

        let input: EngineInput = key.into();
        let field = match self.command_field.as_mut() {
            Some(f) => f,
            None => return,
        };

        if input.key == EngineKey::Enter {
            let text = field.text();
            self.command_field = None;
            self.command_completion = None;
            self.prompt_history_index = None;
            self.prompt_user_input = None;
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
                self.prompt_history_index = None;
                self.prompt_user_input = None;
            } else if field.vim_mode() == VimMode::Insert {
                field.enter_normal();
            } else {
                self.command_field = None;
                self.prompt_history_index = None;
                self.prompt_user_input = None;
            }
            return;
        }

        // Backspace on an empty prompt dismisses it (vim/neovim parity:
        // the leading `:` itself counts as the dismissable prefix).
        if input.key == EngineKey::Backspace
            && self
                .command_field
                .as_ref()
                .is_some_and(|f| f.text().is_empty())
        {
            self.command_field = None;
            self.command_completion = None;
            self.prompt_history_index = None;
            self.prompt_user_input = None;
            return;
        }

        // Any key that isn't Ctrl-P/N resets history navigation position.
        if self.prompt_history_index.is_some() {
            self.prompt_history_index = None;
            self.prompt_user_input = None;
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
        // Phase 2 (#37): Ctrl-P / Up → previous history entry.
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
                    match self.prompt_history_index {
                        None => Some(len - 1),
                        Some(0) => Some(0),
                        Some(i) => Some(i - 1),
                    }
                } else {
                    match self.prompt_history_index {
                        None => None,
                        Some(i) if i + 1 >= len => None,
                        Some(i) => Some(i + 1),
                    }
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

        let input: EngineInput = key.into();
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

        // Backspace on an empty prompt dismisses it (vim/neovim parity:
        // the leading `/` or `?` itself counts as the dismissable prefix).
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
                self.active_mut()
                    .editor
                    .set_last_search(Some(p.clone()), forward);
                // Phase 1 (#37): push to search history ring.
                if forward {
                    App::push_history(&mut self.search_history_forward, &p);
                } else {
                    App::push_history(&mut self.search_history_backward, &p);
                }
            }
            Err(e) => {
                self.active_mut().editor.set_search_pattern(None);
                self.status_message = Some(format!("E: bad search pattern: {e}"));
            }
        }
    }

    /// Dispatch a prompt-opening [`crate::keymap_actions::AppAction`].
    ///
    /// Handles variants:
    ///   - OpenCommandPrompt (`:` — open the ex command prompt)
    ///   - OpenSearchPrompt  (`/` / `?` — open incremental search)
    pub(crate) fn dispatch_prompt_action(&mut self, action: crate::keymap_actions::AppAction) {
        use crate::keymap_actions::AppAction;
        match action {
            // Guard: `@:` chord expects `:` as the register name, so
            // pending_state.is_some() means this key is already claimed by
            // the pending-state reducer (route_chord_key dispatched here via
            // dispatch_keymap_in_mode). In that scenario the keymap itself
            // should not have matched `:` (pending_state blocks the Normal
            // trie path in route_chord_key), so this guard is defensive.
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
    /// - Builds a transient buffer whose content is the relevant history.
    /// - Opens a horizontal split below the current window sized to
    ///   `min(7, history.len() + 1)` lines (enforced by the layout ratio).
    /// - Cursor lands on the last (empty) line.
    /// - Stores `CmdLineWindow` so `<CR>` knows how to re-dispatch.
    pub(crate) fn open_cmdline_window(&mut self, kind: CmdLineKind) {
        use crate::app::window::{LayoutTree, SplitDir, Window};
        use crate::host::TuiHost;
        use std::time::Instant;

        // Already open — no-op (don't nest).
        if self.cmdline_win.is_some() {
            return;
        }

        let history: Vec<String> = match kind {
            CmdLineKind::Ex => self.ex_history.clone(),
            CmdLineKind::SearchForward => self.search_history_forward.clone(),
            CmdLineKind::SearchBackward => self.search_history_backward.clone(),
        };

        // Build the transient buffer content: history lines (oldest first) +
        // one trailing empty line.
        let content = history.join("\n");

        // Create the slot.
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
        // Move cursor to the last line (the empty line after history, or line 0).
        let line_count = editor.buffer().row_count();
        editor.jump_cursor(line_count.saturating_sub(1), 0);
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
            last_recompute_at: Instant::now(),
            last_recompute_key: None,
            saved_hash: 0,
            saved_len: 0,
            disk_mtime: None,
            disk_len: None,
            disk_state: crate::app::DiskState::Synced,
            viewport_render_output: None,
            top_render_output: None,
            bottom_render_output: None,
            dirty_rows_log: Vec::new(),
        };
        self.slots.push(slot);
        let slot_idx = self.slots.len() - 1;

        // Determine window height: clamp to [1, 7] rows.
        let win_rows = (history.len() + 1).clamp(1, 7);

        // Build the split: current window on top (a), cmdline below (b).
        let focused = self.focused_window();
        let new_win_id = self.next_window_id;
        self.next_window_id += 1;
        self.windows.push(Some(Window {
            slot: slot_idx,
            top_row: 0,
            top_col: 0,
            cursor_row: history.len().saturating_sub(1),
            cursor_col: 0,
            last_rect: None,
        }));

        // Compute ratio: cmdline gets `win_rows` rows of total height.
        // We don't know exact height here, use a heuristic: cap 7/24 ≈ 0.29.
        let total_h = crossterm::terminal::size()
            .map(|(_, h)| h as usize)
            .unwrap_or(24)
            .saturating_sub(1); // status line
        let ratio_b = (win_rows as f32 / total_h as f32).clamp(0.05, 0.45);
        let ratio_a = 1.0 - ratio_b;

        self.sync_viewport_from_editor();
        self.layout_mut()
            .replace_leaf(focused, move |id| LayoutTree::Split {
                dir: SplitDir::Horizontal,
                ratio: ratio_a,
                a: Box::new(LayoutTree::Leaf(id)),
                b: Box::new(LayoutTree::Leaf(new_win_id)),
                last_rect: None,
            });

        self.set_focused_window(new_win_id);
        self.sync_viewport_to_editor();

        self.cmdline_win = Some(CmdLineWindow {
            win_id: new_win_id,
            slot_idx,
            kind,
        });
    }

    /// Close the command-line window (without executing the current line).
    ///
    /// Used by `:q` / `<C-c>` from within the cmdline window.
    pub(crate) fn close_cmdline_window(&mut self) {
        let Some(cw) = self.cmdline_win.take() else {
            return;
        };
        // Remove the window from the layout.
        let new_focus = match self.layout_mut().remove_leaf(cw.win_id) {
            Ok(f) => f,
            Err(_) => return, // can't remove last window
        };
        self.windows[cw.win_id] = None;
        // Remove the transient slot. Fix up window slot pointers.
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
        self.set_focused_window(new_focus);
        self.sync_viewport_to_editor();
    }

    /// Execute the line at the cursor in the command-line window, then close it.
    ///
    /// For `q:` windows: dispatches the line as an ex command.
    /// For `q/` / `q?` windows: commits the line as a search pattern.
    pub(crate) fn commit_cmdline_window(&mut self) {
        let Some(cw) = self.cmdline_win.clone() else {
            return;
        };
        // Read the line at the cursor position.
        let line_text = {
            let slot = &self.slots[cw.slot_idx];
            let (row, _) = slot.editor.cursor();
            slot.editor
                .buffer()
                .lines()
                .get(row)
                .cloned()
                .unwrap_or_default()
        };
        // Close first, then dispatch (so dispatch sees the previous focused window).
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
