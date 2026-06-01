use hjkl_bonsai::DotFallbackTheme;
use hjkl_engine::{Host, Query};
use hjkl_engine_tui::EditorRatatuiExt;
use hjkl_ex::ExEffect;
use hjkl_info_popup::InfoPopup;
use std::path::PathBuf;
use std::sync::Arc;

// Used when handling SubstituteConfirm to compute char-column from byte offset.
use hjkl_buffer::rope_line_str;

use crate::host::TuiHost;

use super::{App, DiskState, ex_host_cmds};

/// Strip trailing `[ \t]` from every line in the buffer in-place.
///
/// Used by the `trim_trailing_whitespace` pre-save hook in [`App::save_slot`].
/// Walks every line of the buffer; if any line has trailing whitespace the
/// whole-buffer content is replaced via `set_content_undoable` so the
/// operation is a single undoable step and the syntax / LSP pipelines see a
/// clean `ContentReset` signal. When no line has trailing whitespace this is a
/// no-op (no allocation, no dirty-gen bump).
fn trim_trailing_whitespace_in_place<H: hjkl_engine::types::Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
) {
    use hjkl_engine::Query;
    let n = editor.buffer().line_count() as usize;
    let mut changed = false;
    let lines: Vec<String> = (0..n)
        .map(|r| {
            let line = editor.buffer().line(r as u32);
            let trimmed = line.trim_end_matches([' ', '\t']);
            if trimmed.len() != line.len() {
                changed = true;
                trimmed.to_string()
            } else {
                line
            }
        })
        .collect();
    if !changed {
        return;
    }
    // Preserve line count — don't collapse trailing blank lines. The per-line
    // trim above already stripped the whitespace; just rejoin and replace.
    let new_content = lines.join("\n");
    editor.set_content_undoable(&new_content);
}

impl App {
    /// Execute an ex command string (without the leading `:`).
    pub(crate) fn dispatch_ex(&mut self, cmd: &str) {
        let raw = cmd.trim();
        if raw.is_empty() {
            return;
        }
        // Capture for `@:` repeat (Phase 5d, kryptic-sh/hjkl#71).
        // Vim captures every executed command including errored ones; match that.
        // The literal user text is stored; replay re-runs through dispatch_ex so
        // behavior is identical.
        self.last_ex_command = Some(raw.to_string());
        // Phase 1 (#37): push to ex history ring.
        let raw_owned = raw.to_string();
        App::push_history(&mut self.ex_history, &raw_owned);

        // Phase 7: expand `%`, `#`, `<cword>`, `<cWORD>` tokens in the command
        // line BEFORE dispatch so commands see literal paths.
        let cmd_expanded = {
            let ctx = build_expand_context(self);
            hjkl_ex::expand_args(&ctx, raw)
        };
        // Shadow both `raw` and `cmd` so all dispatch arms see the expanded text.
        let raw = cmd_expanded.as_str();
        let cmd = raw;

        if self.try_handle_runtime_map(raw) {
            return;
        }

        // Resolve abbreviations via hjkl-ex registry (replaces legacy
        // ex::canonical_command_name). Splits the first word, resolves it
        // through the registry's prefix-match table, and reconstructs the
        // full command with the canonical name so the local match arms see
        // `"wall"` when the user typed `:wa`, etc.
        let canon_buf: String;
        let cmd: &str = {
            let reg = hjkl_ex::default_registry::<TuiHost>();
            let first_space = cmd.find(' ');
            let first_word = first_space.map(|i| &cmd[..i]).unwrap_or(cmd);
            if let Some(resolved) = reg.resolve(first_word) {
                if resolved.name != first_word {
                    canon_buf = match first_space {
                        Some(i) => format!("{}{}", resolved.name, &cmd[i..]),
                        None => resolved.name.to_string(),
                    };
                    &canon_buf
                } else {
                    cmd
                }
            } else {
                cmd
            }
        };

        // App-level `:set mouse` / `:set nomouse` / `:set mouse=<flags>` / etc.
        // Mouse capture is a terminal-I/O concern, not an editor-engine
        // setting, so the app intercepts these tokens here. Residual
        // tokens (if any) flow through to the engine as a rebuilt
        // `:set ...` line so combined forms like `:set nu nomouse` work.
        //
        // `:set mouse=<flags>` additionally updates `mouse_flags` to control
        // per-mode event gating (P11.2 / issue #114).
        let rebuilt: String;
        let cmd: &str = if let Some(body) = cmd.strip_prefix("set ") {
            let body = body.trim();
            if body.is_empty() {
                cmd
            } else {
                let mut remaining: Vec<&str> = Vec::new();
                let mut consumed_any = false;
                for tok in body.split_whitespace() {
                    match tok {
                        "mouse" => {
                            self.set_mouse_capture(true);
                            self.mouse_flags = crate::app::MouseFlags::all();
                            consumed_any = true;
                        }
                        "nomouse" => {
                            self.set_mouse_capture(false);
                            self.mouse_flags = crate::app::MouseFlags::none();
                            consumed_any = true;
                        }
                        "mouse!" => {
                            let new_on = !self.mouse_enabled;
                            self.set_mouse_capture(new_on);
                            self.mouse_flags = if new_on {
                                crate::app::MouseFlags::all()
                            } else {
                                crate::app::MouseFlags::none()
                            };
                            consumed_any = true;
                        }
                        "mouse?" => {
                            let flags_str = self.mouse_flags.as_flags_str();
                            self.bus.info(if self.mouse_enabled {
                                format!("mouse={flags_str}")
                            } else {
                                "nomouse".to_string()
                            });
                            consumed_any = true;
                        }
                        other if other.starts_with("mouse=") => {
                            let flags_str = &other["mouse=".len()..];
                            let flags = crate::app::MouseFlags::from_flags(flags_str);
                            let any_on = flags.normal
                                || flags.visual
                                || flags.insert
                                || flags.command
                                || flags.help;
                            self.mouse_flags = flags;
                            self.set_mouse_capture(any_on);
                            consumed_any = true;
                        }
                        other => remaining.push(other),
                    }
                }
                if consumed_any {
                    if remaining.is_empty() {
                        return;
                    }
                    rebuilt = format!("set {}", remaining.join(" "));
                    rebuilt.as_str()
                } else {
                    cmd
                }
            }
        } else {
            cmd
        };

        // `:perf` — migrated to Phase 4d2 host registry (ex_host_cmds.rs).

        // stays inline: :set background= reloads theme + recomputes; intricate enough to justify legacy intercept.
        if let Some(rest) = cmd.strip_prefix("set background=") {
            match rest.trim() {
                "dark" => {
                    self.apply_colorscheme("dark");
                    self.bus.info("background=dark");
                    return;
                }
                "light" => {
                    self.apply_colorscheme("light");
                    self.bus.info("background=light");
                    return;
                }
                other => {
                    self.bus
                        .error(format!("E: unknown background value: {other}"));
                    return;
                }
            }
        }

        // `:colorscheme [name]` / `:colo` — vim alias for switching the active
        // theme. Bundled schemes: `dark`, `light`. Bare or `?` reports current.
        {
            let mut parts = cmd.split_whitespace();
            if let Some(kw) = parts.next()
                && matches!(kw, "colorscheme" | "colorsc" | "colors" | "color" | "colo")
            {
                let arg = parts.next().unwrap_or("").trim();
                match arg {
                    "" | "?" => {
                        let cur = self.colorscheme.clone();
                        self.bus.info(format!("colorscheme {cur}"));
                    }
                    "dark" | "light" => {
                        self.apply_colorscheme(arg);
                        self.bus.info(format!("colorscheme {arg}"));
                    }
                    other => {
                        self.bus
                            .error(format!("E185: cannot find colorscheme '{other}'"));
                    }
                }
                return;
            }
        }

        // `:picker` — migrated to Phase 4d2 host registry (ex_host_cmds.rs).
        // `:rg [pattern]` — migrated to Phase 4d2 host registry (ex_host_cmds.rs).
        // `:b [num|name]` — migrated to Phase 4d2 host registry (ex_host_cmds.rs).

        // Multi-buffer commands — canonical names from COMMAND_NAMES table.
        // NOTE: bnext, bprevious/bNext, bfirst, blast, buffers/ls/files, clipboard,
        // bpicker, b <arg> are handled by HostCmd impls in ex_host_cmds.rs (Phase 4c/4d2).
        // NOTE: wall/qall/qall!/wqall/wqall! are KEPT here — hjkl-ex returns
        // single-buffer Save/Quit effects which diverge from the app's write_all /
        // quit_all semantics that iterate all slots or set exit_requested directly.
        match cmd {
            // TODO(4c): `:b#` cannot be migrated — split_name_args treats the
            // trailing `#` as an arg, so the resolved name is never "b#".
            // Leave this legacy arm in place until parse::split_name_args is
            // extended to accept a trailing `#` for buffer commands.
            "b#" => {
                self.buffer_alt();
                return;
            }
            "wall" => {
                self.write_all();
                return;
            }
            "qall" => {
                self.quit_all(false);
                return;
            }
            "qall!" => {
                self.quit_all(true);
                return;
            }
            "wqall" => {
                self.write_quit_all(false);
                return;
            }
            "wqall!" => {
                self.write_quit_all(true);
                return;
            }
            _ => {}
        }

        // `:checktime` — migrated to Phase 4d2 host registry (ex_host_cmds.rs).

        // `:write[!]` — intercept before the engine to enforce disk-state guard.
        if cmd == "write"
            || cmd == "write!"
            || cmd.starts_with("write ")
            || cmd.starts_with("write!")
        {
            let force = cmd == "write!" || cmd.starts_with("write!");
            let path_arg = if let Some(rest) = cmd.strip_prefix("write!") {
                rest.trim()
            } else if let Some(rest) = cmd.strip_prefix("write ") {
                rest.trim()
            } else {
                ""
            };
            let target = if path_arg.is_empty() {
                None
            } else {
                Some(PathBuf::from(path_arg))
            };
            if !force && self.active().disk_state == DiskState::ChangedOnDisk {
                self.bus
                    .error("E13: file has changed on disk (add ! to override)");
                return;
            }
            if self.do_save(target) {
                self.active_mut().disk_state = DiskState::Synced;
            }
            return;
        }

        // `:sp[lit]` / `:vsp[lit]` — migrated to Phase 4b host registry (ex_host_cmds.rs).
        // `:vnew` — migrated to Phase 4d2 host registry (ex_host_cmds.rs).
        // `:close` / `:only` — migrated to Phase 4b host registry (ex_host_cmds.rs).
        // `:new` — migrated to Phase 4d2 host registry (ex_host_cmds.rs).

        // ─── Tab commands ────────────────────────────────────────────────────

        // `:tabnew` / `:tabedit` / `:tabe` — migrated to Phase 4b host registry (ex_host_cmds.rs).
        // `:tabnext` / `:tabn` — migrated to Phase 4a host registry (ex_host_cmds.rs).
        // `:tabprev` / `:tabp` / `:tabN` — migrated to Phase 4b host registry (ex_host_cmds.rs).
        // `:tabclose` / `:tabc` — migrated to Phase 4b host registry (ex_host_cmds.rs).
        // `:tabfirst` / `:tabrewind` / `:tabr` — migrated to Phase 4d2 host registry (ex_host_cmds.rs).
        // `:tablast` — migrated to Phase 4d2 host registry (ex_host_cmds.rs).

        // `:tabonly` / `:tabo` — migrated to Phase 4f host registry (ex_host_cmds.rs).

        // `:tabmove` — migrated to Phase 4b host registry (ex_host_cmds.rs).

        // `:tabs` — migrated to Phase 4f host registry (ex_host_cmds.rs).

        // `:resize [+|-]N` — migrated to Phase 4f host registry (ex_host_cmds.rs).
        // `:vertical resize [+|-]N` / `:vert res [+|-]N` — migrated to Phase 4f host registry.

        // ── LSP / diag / Anvil — migrated to Phase 4f host registry (ex_host_cmds.rs) ───
        // `:Rename <newname>`, `:LspFormat`, `:Format`, `:LspCodeAction`, `:CodeAction`,
        // `:lopen`, `:lnext`, `:lprev`, `:lfirst`, `:llast`, `:LspInfo`, `:Anvil [...]`

        // Phase 4a–4d2: try the app-level host registry first.
        // Commands live in ex_host_cmds.rs; see host_registry() for the full list.
        {
            let host_reg = ex_host_cmds::host_registry();
            if let Some(eff) = hjkl_ex::try_dispatch_host(host_reg, self, cmd) {
                match eff {
                    ExEffect::EditFile { path, force } => {
                        self.do_edit(&path, force);
                        return;
                    }
                    ExEffect::BufferDelete { force, wipe } => {
                        if wipe {
                            self.buffer_wipe(force);
                        } else {
                            self.buffer_delete(force);
                        }
                        return;
                    }
                    other => {
                        self.sync_viewport_from_editor();
                        self.handle_ex_effect(other);
                        return;
                    }
                }
            }
        }

        let active_slot = self.focused_slot_idx();
        // hjkl-ex is the sole dispatcher — no legacy fallback.
        let new_reg = hjkl_ex::default_registry::<TuiHost>();
        let effect = if let Some(eff) =
            hjkl_ex::try_dispatch(&new_reg, &mut self.slots[active_slot].editor, cmd)
        {
            match eff {
                ExEffect::EditFile { path, force } => {
                    self.do_edit(&path, force);
                    return;
                }
                ExEffect::BufferDelete { force, wipe } => {
                    if wipe {
                        self.buffer_wipe(force);
                    } else {
                        self.buffer_delete(force);
                    }
                    return;
                }
                other => other,
            }
        } else {
            // No command matched — surface as E492.
            ExEffect::Unknown(cmd.to_string())
        };
        // ex commands like `:100` (goto-line), `:/pat` (search address),
        // and `:nohl` mutate engine cursor / viewport without flipping
        // the dirty flag — they return ExEffect::Ok. The window cursor
        // cache (used at render time) must be re-synced or the cursor
        // appears stuck at its pre-`:` position even though the engine
        // moved it.
        self.sync_viewport_from_editor();
        self.handle_ex_effect(effect);
    }

    /// Apply the side-effects encoded in an [`ExEffect`] value.
    ///
    /// Extracted from `dispatch_ex` so both the host-registry path and the
    /// editor-registry path can share identical effect handling without
    /// duplicating the match arms.
    fn handle_ex_effect(&mut self, effect: ExEffect) {
        match effect {
            ExEffect::None => {}
            ExEffect::Ok => {}
            ExEffect::Save => {
                self.do_save(None);
            }
            ExEffect::SaveAs(path) => {
                self.do_save(Some(PathBuf::from(path)));
            }
            ExEffect::Quit { force, save } => {
                if save && !self.do_save(None) {
                    // Save failed (E32 / E45 / IO error). Status_message
                    // already set by do_save; refuse to exit so the user
                    // doesn't lose unsaved content.
                    return;
                }
                // Vim parity: :q with multiple windows closes the focused
                // window (same as :close). :q! with multiple windows also
                // just closes the focused window (force discards dirty state
                // for that window but doesn't quit the app).
                if self.layout().leaves().len() > 1 {
                    self.close_focused_window();
                    return;
                }
                // E4: multi-slot — close active slot, stay in app.
                if self.slots.len() > 1 {
                    self.buffer_delete(force);
                    return;
                }
                // Last slot, last window: original quit semantics.
                if force || save {
                    self.exit_requested = true;
                } else if self.active().dirty {
                    self.bus
                        .error("E37: No write since last change (add ! to override)");
                } else {
                    self.exit_requested = true;
                }
            }
            ExEffect::Substituted {
                count,
                lines_changed,
            } => {
                // Engine applied the substitution in-place; propagate dirty
                // and fan ContentEdits into the syntax tree.
                let aslot = self.focused_slot_idx();
                if self.slots[aslot].editor.take_dirty() {
                    let elapsed = self.slots[aslot].refresh_dirty_against_saved();
                    self.last_signature_us = elapsed;
                    let buffer_id = self.slots[aslot].buffer_id;
                    if self.slots[aslot].editor.take_content_reset() {
                        self.syntax.reset(buffer_id);
                    }
                    let edits = self.slots[aslot].editor.take_content_edits();
                    if !edits.is_empty() {
                        self.syntax.apply_edits(buffer_id, &edits);
                    }
                    self.recompute_and_install();
                }
                if count == 0 {
                    self.bus.warn("Pattern not found");
                } else {
                    self.bus
                        .info(format!("{count} substitutions on {lines_changed} lines"));
                }
            }
            ExEffect::InfoTitled { title, content } => {
                self.info_popup = Some(InfoPopup::new(title, content));
            }
            ExEffect::Info(msg) => {
                if msg.contains('\n') {
                    self.info_popup = Some(InfoPopup::new("info", msg));
                } else {
                    self.bus.info(msg);
                }
            }
            ExEffect::Error(msg) => {
                self.bus.error(format!("E: {msg}"));
            }
            ExEffect::Unknown(c) => {
                self.bus.error(format!("E492: Not an editor command: :{c}"));
            }
            ExEffect::EditFile { path, force } => {
                self.do_edit(&path, force);
            }
            ExEffect::BufferDelete { force, wipe } => {
                if wipe {
                    self.buffer_wipe(force);
                } else {
                    self.buffer_delete(force);
                }
            }
            ExEffect::PutRegister { reg, above } => {
                self.do_put_register(reg, above);
            }
            ExEffect::SaveAndRename { path } => {
                // `:saveas {path}`: write to path AND update the buffer's identity.
                // save_slot already updates slot.filename when saving to a new path.
                let p = PathBuf::from(&path);
                let idx = self.focused_slot_idx();
                self.save_slot(idx, Some(p));
            }
            ExEffect::RenameBuffer { name } => {
                // `:file {name}`: rename buffer in-memory without writing.
                let idx = self.focused_slot_idx();
                let p = PathBuf::from(&name);
                self.slots[idx].filename = Some(p.clone());
                self.slots[idx]
                    .editor
                    .registers_mut()
                    .set_filename(Some(name.clone()));
                self.bus.info(format!("\"{}\" [Not edited]", p.display()));
            }
            ExEffect::Cwd(new_cwd) => {
                // `:cd` already applied std::env::set_current_dir; show new path.
                self.bus.info(new_cwd);
            }
            ExEffect::Redraw { clear } => {
                if clear {
                    // `:redraw!` — clear the terminal before the next draw.
                    self.force_clear_screen = true;
                }
                // `:redraw` (no `!`) — ratatui's diff-based renderer will
                // repaint on the next event-loop tick without a full clear.
            }
            ExEffect::Preserve => {
                // Force-write the swap for the active slot immediately.
                let idx = self.focused_slot_idx();
                self.write_swap_for_slot(idx);
            }
            ExEffect::Recover(path) => {
                self.do_recover(&path);
            }
            ExEffect::SubstituteConfirm { matches } => {
                if matches.is_empty() {
                    self.bus.warn("Pattern not found");
                    return;
                }
                let len = matches.len();
                // Jump cursor to the first match so it is visible.
                let first_row = matches[0].row as usize;
                let first_col = {
                    let rope = hjkl_engine::Query::rope(self.active().editor.buffer());
                    let line = rope_line_str(&rope, first_row);
                    line[..matches[0].byte_start as usize].chars().count()
                };
                self.active_mut().editor.jump_cursor(first_row, first_col);
                self.sync_after_engine_mutation();

                self.confirming_substitute = Some(crate::app::ConfirmingSubstitute {
                    matches,
                    accepted: vec![false; len],
                    idx: 0,
                });
                // Status line renders the prompt by reading confirming_substitute directly.
            }
        }
    }

    /// `:put [{reg}]` — paste register contents as a new line below (or above)
    /// the cursor. Reads the register text, then inserts it as a fresh line.
    fn do_put_register(&mut self, reg: char, above: bool) {
        use hjkl_buffer::{Edit, Position};
        let idx = self.focused_slot_idx();
        let slot_text = self.slots[idx]
            .editor
            .registers()
            .read(reg)
            .map(|s| s.text.clone())
            .unwrap_or_default();
        if slot_text.is_empty() {
            self.bus.warn(format!("E: register \"{reg}\" is empty"));
            return;
        }
        // Strip trailing newline that linewise yanks carry so we don't
        // introduce a blank line at the end.
        let text = slot_text.trim_end_matches('\n').to_string();
        let editor = &mut self.slots[idx].editor;
        let (row, _) = editor.cursor();
        if above {
            editor.mutate_edit(Edit::InsertStr {
                at: Position::new(row, 0),
                text: format!("{text}\n"),
            });
        } else {
            let paste_rope = editor.buffer().rope();
            let line_len = hjkl_buffer::rope_line_str(&paste_rope, row).chars().count();
            editor.mutate_edit(Edit::InsertStr {
                at: Position::new(row, line_len),
                text: format!("\n{text}"),
            });
        }
        // Sync dirty state and propagate to syntax engine.
        let slot = &mut self.slots[idx];
        if slot.editor.take_dirty() {
            slot.refresh_dirty_against_saved();
        }
    }

    /// `:sp [file]` / `:split [file]` — open a horizontal split.
    ///
    /// With no argument: duplicates the current window (same slot, same
    /// scroll).  With a filename: opens a new slot in the upper half.
    pub(super) fn do_split(&mut self, arg: &str) {
        use crate::app::window::{LayoutTree, SplitDir, Window};
        let focused = self.focused_window();
        let cur_slot = self.windows[focused]
            .as_ref()
            .expect("focused_window open")
            .slot;
        let (top_row, top_col) = {
            let win = self.windows[focused].as_ref().unwrap();
            (win.top_row, win.top_col)
        };
        let (cursor_row, cursor_col) = self.active().editor.cursor();

        let new_slot = if arg.is_empty() {
            // Duplicate — same slot.
            cur_slot
        } else {
            match self.open_new_slot(std::path::PathBuf::from(arg)) {
                Ok(idx) => idx,
                Err(msg) => {
                    self.bus.info(msg);
                    return;
                }
            }
        };

        let new_win_id = self.next_window_id;
        self.next_window_id += 1;
        self.windows.push(Some(Window::with_scroll(
            new_slot, top_row, top_col, cursor_row, cursor_col,
        )));
        // Replace the focused leaf with a horizontal split:
        // new window on top (a), existing window below (b).
        self.layout_mut()
            .replace_leaf(focused, move |id| LayoutTree::Split {
                dir: SplitDir::Horizontal,
                ratio: 0.5,
                a: Box::new(LayoutTree::Leaf(new_win_id)),
                b: Box::new(LayoutTree::Leaf(id)),
                last_rect: None,
            });
        self.set_focused_window(new_win_id);
        self.bus.info("split");
    }

    /// `:vsp [file]` / `:vsplit [file]` — open a vertical split.
    ///
    /// With no argument: duplicates the current window (same slot, same
    /// scroll).  With a filename: opens a new slot in the left half.
    /// New window goes on the left (vim convention).
    pub(super) fn do_vsplit(&mut self, arg: &str) {
        use crate::app::window::{LayoutTree, SplitDir, Window};
        let focused = self.focused_window();
        let cur_slot = self.windows[focused]
            .as_ref()
            .expect("focused_window open")
            .slot;
        let (top_row, top_col) = {
            let win = self.windows[focused].as_ref().unwrap();
            (win.top_row, win.top_col)
        };
        let (cursor_row, cursor_col) = self.active().editor.cursor();

        let new_slot = if arg.is_empty() {
            // Duplicate — same slot.
            cur_slot
        } else {
            match self.open_new_slot(std::path::PathBuf::from(arg)) {
                Ok(idx) => idx,
                Err(msg) => {
                    self.bus.info(msg);
                    return;
                }
            }
        };

        let new_win_id = self.next_window_id;
        self.next_window_id += 1;
        self.windows.push(Some(Window::with_scroll(
            new_slot, top_row, top_col, cursor_row, cursor_col,
        )));
        // Replace the focused leaf with a vertical split:
        // new window on the left (a), existing window on the right (b).
        self.layout_mut()
            .replace_leaf(focused, move |id| LayoutTree::Split {
                dir: SplitDir::Vertical,
                ratio: 0.5,
                a: Box::new(LayoutTree::Leaf(new_win_id)),
                b: Box::new(LayoutTree::Leaf(id)),
                last_rect: None,
            });
        self.set_focused_window(new_win_id);
        self.bus.info("vsplit");
    }

    /// `:vnew` — open a vertical split with a fresh empty unnamed buffer.
    pub(super) fn do_vnew(&mut self) {
        use crate::app::window::{LayoutTree, SplitDir, Window};
        let focused = self.focused_window();
        let (top_row, top_col) = {
            let win = self.windows[focused].as_ref().expect("focused_window open");
            (win.top_row, win.top_col)
        };

        // Create a fresh empty unnamed slot.
        use crate::app::STATUS_LINE_HEIGHT;
        use crate::host::TuiHost;
        use hjkl_buffer::Buffer;
        use hjkl_engine::{Editor, Options};

        let new_slot_idx = {
            let buffer_id = self.next_buffer_id;
            self.next_buffer_id += 1;
            let host = TuiHost::new();
            let mut editor = Editor::new(Buffer::new(), host, Options::default());
            if let Ok(size) = crossterm::terminal::size() {
                let vp = editor.host_mut().viewport_mut();
                vp.width = size.0;
                vp.height = size.1.saturating_sub(STATUS_LINE_HEIGHT);
            }
            let _ = editor.take_content_edits();
            let _ = editor.take_content_reset();
            let mut slot = super::BufferSlot {
                buffer_id,
                editor,
                filename: None,
                dirty: false,
                is_new_file: false,
                is_untracked: false,
                diag_signs: Vec::new(),
                diag_signs_lsp: Vec::new(),
                lsp_diags: Vec::new(),
                last_lsp_dirty_gen: None,
                git_signs: Vec::new(),
                last_git_dirty_gen: None,
                last_git_refresh_at: std::time::Instant::now(),
                blame: Vec::new(),
                last_blame_dirty_gen: None,
                last_blame_refresh_at: std::time::Instant::now(),
                saved_hash: 0,
                saved_len: 0,
                signature_cache: None,
                disk_mtime: None,
                disk_len: None,
                disk_state: super::DiskState::Synced,
                swap_path: None,
                last_swap_dirty_gen: None,
                last_fold_dirty_gen: None,
            };
            slot.snapshot_saved();
            self.slots.push(slot);
            self.slots.len() - 1
        };

        let new_win_id = self.next_window_id;
        self.next_window_id += 1;
        self.windows.push(Some(Window::with_scroll(
            new_slot_idx,
            top_row,
            top_col,
            0,
            0,
        )));
        // New window on the left (a), existing on the right (b).
        self.layout_mut()
            .replace_leaf(focused, move |id| LayoutTree::Split {
                dir: SplitDir::Vertical,
                ratio: 0.5,
                a: Box::new(LayoutTree::Leaf(new_win_id)),
                b: Box::new(LayoutTree::Leaf(id)),
                last_rect: None,
            });
        self.set_focused_window(new_win_id);
        self.bus.info("vnew");
    }

    /// `:new` — open a horizontal split with a fresh empty unnamed buffer.
    ///
    /// New window appears on top (a), existing window stays below (b).
    pub(super) fn do_new(&mut self) {
        use crate::app::window::{LayoutTree, SplitDir, Window};
        let focused = self.focused_window();
        let (top_row, top_col) = {
            let win = self.windows[focused].as_ref().expect("focused_window open");
            (win.top_row, win.top_col)
        };

        // Create a fresh empty unnamed slot.
        use crate::app::STATUS_LINE_HEIGHT;
        use crate::host::TuiHost;
        use hjkl_buffer::Buffer;
        use hjkl_engine::{Editor, Options};

        let new_slot_idx = {
            let buffer_id = self.next_buffer_id;
            self.next_buffer_id += 1;
            let host = TuiHost::new();
            let mut editor = Editor::new(Buffer::new(), host, Options::default());
            if let Ok(size) = crossterm::terminal::size() {
                let vp = editor.host_mut().viewport_mut();
                vp.width = size.0;
                vp.height = size.1.saturating_sub(STATUS_LINE_HEIGHT);
            }
            let _ = editor.take_content_edits();
            let _ = editor.take_content_reset();
            let mut slot = super::BufferSlot {
                buffer_id,
                editor,
                filename: None,
                dirty: false,
                is_new_file: false,
                is_untracked: false,
                diag_signs: Vec::new(),
                diag_signs_lsp: Vec::new(),
                lsp_diags: Vec::new(),
                last_lsp_dirty_gen: None,
                git_signs: Vec::new(),
                last_git_dirty_gen: None,
                last_git_refresh_at: std::time::Instant::now(),
                blame: Vec::new(),
                last_blame_dirty_gen: None,
                last_blame_refresh_at: std::time::Instant::now(),
                saved_hash: 0,
                saved_len: 0,
                signature_cache: None,
                disk_mtime: None,
                disk_len: None,
                disk_state: super::DiskState::Synced,
                swap_path: None,
                last_swap_dirty_gen: None,
                last_fold_dirty_gen: None,
            };
            slot.snapshot_saved();
            self.slots.push(slot);
            self.slots.len() - 1
        };

        let new_win_id = self.next_window_id;
        self.next_window_id += 1;
        self.windows.push(Some(Window::with_scroll(
            new_slot_idx,
            top_row,
            top_col,
            0,
            0,
        )));
        // New window on top (a), existing window below (b).
        self.layout_mut()
            .replace_leaf(focused, move |id| LayoutTree::Split {
                dir: SplitDir::Horizontal,
                ratio: 0.5,
                a: Box::new(LayoutTree::Leaf(new_win_id)),
                b: Box::new(LayoutTree::Leaf(id)),
                last_rect: None,
            });
        self.set_focused_window(new_win_id);
        self.bus.info("new");
    }

    /// Format a one-line summary of the active clipboard backend for the
    /// status line. Used by `:clipboard`.
    pub(crate) fn clipboard_status(&self) -> String {
        let Some(cb) = self.active().editor.host().clipboard() else {
            return "clipboard: unavailable (probe failed)".into();
        };
        let kind = cb.kind();
        let caps = cb.capabilities();
        let flags = [
            (hjkl_clipboard::Capabilities::WRITE, "WRITE"),
            (hjkl_clipboard::Capabilities::READ, "READ"),
            (hjkl_clipboard::Capabilities::CLEAR, "CLEAR"),
            (hjkl_clipboard::Capabilities::AVAILABLE, "AVAILABLE"),
            (hjkl_clipboard::Capabilities::PRIMARY, "PRIMARY"),
            (hjkl_clipboard::Capabilities::IMAGE, "IMAGE"),
            (hjkl_clipboard::Capabilities::RICH_TEXT, "RICH_TEXT"),
            (hjkl_clipboard::Capabilities::URI_LIST, "URI_LIST"),
            (hjkl_clipboard::Capabilities::ASYNC_WRITE, "ASYNC_WRITE"),
            (hjkl_clipboard::Capabilities::ASYNC_READ, "ASYNC_READ"),
            (hjkl_clipboard::Capabilities::ASYNC_CLEAR, "ASYNC_CLEAR"),
            (
                hjkl_clipboard::Capabilities::ASYNC_AVAILABLE,
                "ASYNC_AVAILABLE",
            ),
        ];
        let active: Vec<&str> = flags
            .iter()
            .filter_map(|(f, name)| caps.contains(*f).then_some(*name))
            .collect();
        format!("clipboard: {kind} | {}", active.join(" "))
    }

    /// Write buffer content to `path` (or `self.active().filename` if `path` is `None`).
    /// Returns `true` on success, `false` on any failure (E32 / E45 / IO error).
    pub(crate) fn do_save(&mut self, path: Option<PathBuf>) -> bool {
        let idx = self.focused_slot_idx();
        self.save_slot(idx, path)
    }

    /// Write slot `idx`'s buffer to `path` (or the slot's own filename if
    /// `path` is `None`). Pushes a notification on success or failure.
    /// Does NOT change `self.active`. Returns `true` on success.
    fn save_slot(&mut self, idx: usize, path: Option<PathBuf>) -> bool {
        if self.slots[idx].editor.is_readonly() {
            self.bus
                .error("E45: 'readonly' option is set (add ! to override)");
            return false;
        }
        let target = path.or_else(|| self.slots[idx].filename.clone());
        match target {
            None => {
                self.bus.error("E32: No file name");
                false
            }
            Some(p) => {
                // ── Pre-save hooks ──────────────────────────────────────────
                //
                // trim_trailing_whitespace runs first so that if format_on_save
                // is also on, the formatter sees already-trimmed input (formatters
                // like rustfmt/prettier normalise trailing whitespace themselves,
                // but trimming first keeps the two hooks independent and correct
                // on their own).
                //
                // Note: `:w!` is not distinguished from `:w` at the ExEffect
                // level in v1 — both hooks always fire when their option is true.
                {
                    let s = self.slots[idx].editor.settings().clone();
                    if s.trim_trailing_whitespace {
                        trim_trailing_whitespace_in_place(&mut self.slots[idx].editor);
                    }
                    if s.format_on_save
                        && let Some(formatter) = hjkl_mangler::formatter_for_path(&p)
                    {
                        if !hjkl_mangler::is_tool_installed(formatter.tool_name()) {
                            self.bus.warn(format!(
                                "format-on-save: {} not installed, skipping",
                                formatter.tool_name()
                            ));
                        } else {
                            let content = self.slots[idx].editor.buffer().content_joined();
                            let project_root = p.parent().unwrap_or(std::path::Path::new("."));
                            match formatter.format(&content, project_root, None) {
                                Ok(formatted) => {
                                    self.slots[idx].editor.set_content_undoable(&formatted);
                                }
                                Err(e) => {
                                    self.bus.error(format!("format-on-save error: {e}"));
                                    return false;
                                }
                            }
                        }
                        // No formatter registered for this extension → silent
                        // fall-through (save proceeds unformatted).
                    }
                }
                // ── End pre-save hooks ──────────────────────────────────────

                // The format-on-save / trim hooks may have rewritten the buffer
                // (via `set_content_undoable` → whole-buffer reset, or content
                // edits). Fan those changes into the syntax tree NOW so the next
                // render doesn't query a stale tree (old byte offsets) against
                // the new rope — that mismatch slices a node range mid-char and
                // panics `ropey::byte_slice`.
                {
                    let bid = self.slots[idx].buffer_id;
                    let was_reset = self.slots[idx].editor.take_content_reset();
                    if was_reset {
                        self.syntax.reset(bid);
                    }
                    let edits = self.slots[idx].editor.take_content_edits();
                    if !edits.is_empty() {
                        self.syntax.apply_edits(bid, &edits);
                    }
                    // Rebuild spans for the active buffer before the next draw.
                    if (was_reset || !edits.is_empty()) && idx == self.focused_slot_idx() {
                        self.pending_recompute = true;
                    }
                }

                // Reuse the per-dirty_gen Arc<String> from content_joined() so
                // saves share the same allocation that LSP / git / syntax / dirty
                // signature paths already paid for. Was Buffer::lines().join()
                // which re-cloned every line (162k allocs on a 162k-row buffer).
                // Write in two pieces so the trailing newline doesn't force a
                // full-buffer clone just to push a byte.
                use hjkl_engine::Query;
                use std::io::Write;
                let joined = self.slots[idx].editor.buffer().content_joined();
                let body: &[u8] = joined.as_bytes();
                let needs_trailing_nl = !body.is_empty() && !body.ends_with(b"\n");
                let line_count = self.slots[idx].editor.buffer().line_count() as usize;
                let byte_count = body.len() + usize::from(needs_trailing_nl);
                // Create parent dir(s) if missing so writing into a fresh
                // path like ~/.config/hjkl/config.toml works first try.
                if let Some(parent) = p.parent()
                    && !parent.as_os_str().is_empty()
                    && !parent.exists()
                    && let Err(e) = std::fs::create_dir_all(parent)
                {
                    self.bus.error(format!("E: {}: {e}", parent.display()));
                    return false;
                }
                let write_result = (|| -> std::io::Result<()> {
                    let mut f = std::fs::File::create(&p)?;
                    f.write_all(body)?;
                    if needs_trailing_nl {
                        f.write_all(b"\n")?;
                    }
                    Ok(())
                })();
                match write_result {
                    Ok(()) => {
                        self.bus.info(format!(
                            "\"{}\" {}L, {}B written",
                            p.display(),
                            line_count,
                            byte_count,
                        ));
                        // Record disk metadata so checktime knows the new baseline.
                        if let Ok(meta) = std::fs::metadata(&p) {
                            self.slots[idx].disk_mtime = meta.modified().ok();
                            self.slots[idx].disk_len = Some(meta.len());
                        }
                        self.slots[idx].disk_state = DiskState::Synced;
                        self.slots[idx].filename = Some(p.clone());
                        // Keep `"%` in sync when the buffer gets a (new) filename.
                        self.slots[idx]
                            .editor
                            .registers_mut()
                            .set_filename(Some(p.to_string_lossy().into_owned()));
                        self.slots[idx].is_new_file = false;
                        self.slots[idx].snapshot_saved();
                        // Delete the swap file on successful save (#185).
                        if let Some(ref sp) = self.slots[idx].swap_path.clone() {
                            let _ = hjkl_app::swap::remove_swap(sp);
                            self.slots[idx].last_swap_dirty_gen = None;
                        }
                        if idx == self.focused_slot_idx() {
                            self.refresh_git_signs_force();
                        }
                        // Tell the language server the file was saved so its
                        // on-save flycheck (e.g. rust-analyzer clippy) re-runs.
                        self.lsp_notify_save_slot(idx);
                        true
                    }
                    Err(e) => {
                        self.bus.error(format!("E: {}: {e}", p.display()));
                        false
                    }
                }
            }
        }
    }

    /// `:wa` / `:wall` — write all named dirty slots.
    fn write_all(&mut self) {
        let mut written = 0usize;
        let mut skipped = 0usize;
        for i in 0..self.slots.len() {
            if self.slots[i].filename.is_none() {
                skipped += 1;
                continue;
            }
            if !self.slots[i].dirty {
                continue;
            }
            self.save_slot(i, None);
            written += 1;
        }
        self.bus
            .info(format!("{written} buffer(s) written, {skipped} skipped"));
    }

    // ── Swap file helpers (issue #185) ────────────────────────────────────────

    /// Write the swap file for slot `idx`.
    ///
    /// Named buffers (with a filename): skip when the slot has no `swap_path`
    /// or the buffer hasn't changed since the last write.
    ///
    /// Scratch buffers (no filename): skip when the buffer is empty (nothing
    /// worth recovering). Otherwise assign a `scratch_<pid>_<bufid>.swp` path
    /// lazily on the first write (leaving `swap_path = None` for pristine
    /// empties avoids littering the swap dir on every `:new`/`:vnew`).
    ///
    /// Errors during write are logged as debug (not shown to user — swap is
    /// best-effort).
    pub(crate) fn write_swap_for_slot(&mut self, idx: usize) {
        use hjkl_app::swap::{self, SwapHeader};

        let is_scratch = self.slots[idx].filename.is_none();

        if is_scratch {
            // Skip if the buffer is empty — no content worth recovering.
            // byte_len() == 0 means the rope is empty.
            let byte_len = self.slots[idx].editor.buffer().byte_len();
            if byte_len == 0 {
                return;
            }

            // Lazily assign a scratch swap path on the first non-empty write.
            if self.slots[idx].swap_path.is_none() {
                let pid = std::process::id();
                let buffer_id = self.slots[idx].buffer_id;
                match swap::scratch_swap_path(pid, buffer_id) {
                    Ok(p) => self.slots[idx].swap_path = Some(p),
                    Err(e) => {
                        tracing::debug!(err = %e, "scratch_swap_path failed");
                        return;
                    }
                }
            }
        } else {
            // Named buffer: must already have a swap_path (set by build_slot /
            // arm_swap_on_open). No-op if missing.
            if self.slots[idx].swap_path.is_none() {
                return;
            }
        }

        let swap_path = self.slots[idx].swap_path.as_ref().unwrap().clone();
        let current_gen = self.slots[idx].editor.buffer().dirty_gen();
        if self.slots[idx].last_swap_dirty_gen == Some(current_gen) {
            return; // Nothing changed since last swap.
        }

        let (cursor_row, cursor_col) = self.slots[idx].editor.cursor();

        let (canonical_path, file_mtime_unix_ms) = if is_scratch {
            // Scratch swap: empty canonical_path marks it as scratch.
            (String::new(), 0u64)
        } else {
            let filename = self.slots[idx].filename.as_ref().unwrap().clone();
            let mtime = self.slots[idx]
                .disk_mtime
                .and_then(|t| {
                    t.duration_since(std::time::SystemTime::UNIX_EPOCH)
                        .ok()
                        .map(|d| d.as_millis() as u64)
                })
                .unwrap_or(0);
            let cp = std::fs::canonicalize(&filename)
                .unwrap_or_else(|_| filename.clone())
                .to_string_lossy()
                .into_owned();
            (cp, mtime)
        };

        let header = SwapHeader {
            magic: SwapHeader::MAGIC,
            version: SwapHeader::VERSION,
            canonical_path,
            file_mtime_unix_ms,
            write_time_unix_ms: swap::now_unix_ms(),
            cursor: (cursor_row as u32, cursor_col as u32),
            writer_pid: std::process::id(),
        };

        let rope = self.slots[idx].editor.buffer().rope().clone();
        if let Err(e) = swap::write_swap(&swap_path, &header, &rope) {
            tracing::debug!(path = %swap_path.display(), err = %e, "swap write failed");
            return;
        }
        self.slots[idx].last_swap_dirty_gen = Some(current_gen);
    }

    /// Write the initial swap for a freshly-opened slot so the PID lock exists
    /// immediately (matching vim). No-op when: slot has no filename/swap_path,
    /// a recovery prompt is pending (don't clobber the swap the user is deciding
    /// on), or the slot index is out of range (e.g. removed by a lock-refusal).
    ///
    /// `write_swap_for_slot` already skips the write when `last_swap_dirty_gen
    /// == Some(current_gen)`; on a fresh open `last_swap_dirty_gen` is `None`,
    /// so it always writes — even for an unmodified buffer. That is intentional:
    /// the PID lock must exist immediately so a concurrent second open sees it.
    pub(crate) fn arm_swap_on_open(&mut self, slot_idx: usize) {
        if self.pending_recovery.is_some() {
            return;
        }
        if slot_idx >= self.slots.len() {
            return;
        }
        if self.slots[slot_idx].filename.is_none() || self.slots[slot_idx].swap_path.is_none() {
            return;
        }
        self.write_swap_for_slot(slot_idx);
    }

    /// Remove all slots' swap files. Called on graceful shutdown so a clean
    /// exit leaves no swap (no false recovery next open); a crash/kill bypasses
    /// this and the swap survives for recovery.
    ///
    /// Scratch buffers are covered: once their first non-empty idle write fires,
    /// `swap_path` is `Some` and is removed here on graceful exit just like
    /// named-file swaps. Empty scratch buffers never get a swap_path, so there
    /// is nothing to clean up for them.
    pub(crate) fn cleanup_swaps_on_exit(&mut self) {
        for slot in &mut self.slots {
            if let Some(p) = slot.swap_path.take() {
                let _ = hjkl_app::swap::remove_swap(&p);
            }
        }
    }

    /// Load any orphan scratch swaps (unsaved buffers from a crashed session)
    /// from `dir` into recovered unnamed buffers. Returns the count recovered.
    ///
    /// Each recovered buffer is dirty (nudging the user to `:w <name>`), and the
    /// originating orphan swap file is deleted so it is not re-recovered on the
    /// next launch.  The new buffers are appended as background slots — focus is
    /// NOT switched (user navigates to them via `:bnext` / buffer picker).
    ///
    /// The `_from(dir)` variant accepts a directory for testability without real
    /// XDG I/O.  `recover_orphan_scratch_buffers` calls the real `swap_dir()`.
    ///
    /// NOTE: auto-loads all orphans (MVP). A picker UI for many orphans is
    /// out of scope for issue #185.
    ///
    /// Called once from `main` after construction + config + CLI files are
    /// loaded — NOT from `App::new` (keeps tests and every App::new free of
    /// real-XDG scanning).
    pub(crate) fn recover_orphan_scratch_buffers_from(&mut self, dir: &std::path::Path) -> usize {
        use crate::app::STATUS_LINE_HEIGHT;
        use crate::host::TuiHost;
        use hjkl_app::swap;
        use hjkl_buffer::Buffer;
        use hjkl_engine::{Editor, Options};

        let orphans = swap::scan_orphan_scratch_swaps_in(dir);
        let n = orphans.len();
        if n == 0 {
            return 0;
        }

        for orphan in orphans {
            // Build a fresh unnamed slot (mirrors build_slot with path=None).
            let buffer_id = self.next_buffer_id;
            self.next_buffer_id += 1;
            let host = TuiHost::new();
            let mut editor = Editor::new(Buffer::new(), host, Options::default());
            if let Ok(size) = crossterm::terminal::size() {
                let vp = editor.host_mut().viewport_mut();
                vp.width = size.0;
                vp.height = size.1.saturating_sub(STATUS_LINE_HEIGHT);
            }
            // Drain initial (empty) content signals so they don't confuse syntax.
            let _ = editor.take_content_edits();
            let _ = editor.take_content_reset();

            // Install recovered content via set_content (full reset path so
            // syntax gets a clean parse_initial, not a stale incremental edit).
            let stripped = orphan.body.strip_suffix('\n').unwrap_or(&orphan.body);
            editor.set_content(stripped);

            // Restore cursor from swap header.
            let (row, col) = orphan.header.cursor;
            editor.jump_cursor(row as usize, col as usize);

            let mut slot = super::BufferSlot {
                buffer_id,
                editor,
                filename: None,
                dirty: true, // nudge user to :w as <name>
                is_new_file: false,
                is_untracked: false,
                diag_signs: Vec::new(),
                diag_signs_lsp: Vec::new(),
                lsp_diags: Vec::new(),
                last_lsp_dirty_gen: None,
                git_signs: Vec::new(),
                last_git_dirty_gen: None,
                last_git_refresh_at: std::time::Instant::now(),
                blame: Vec::new(),
                last_blame_dirty_gen: None,
                last_blame_refresh_at: std::time::Instant::now(),
                saved_hash: 0,
                saved_len: 0,
                signature_cache: None,
                disk_mtime: None,
                disk_len: None,
                disk_state: super::DiskState::Synced,
                // Leave swap_path None: the idle writer will assign a fresh
                // scratch path for this session on the next edit if needed.
                swap_path: None,
                last_swap_dirty_gen: None,
                last_fold_dirty_gen: None,
            };
            slot.snapshot_saved();
            // Re-mark dirty after snapshot (snapshot_saved clears dirty).
            slot.dirty = true;
            self.slots.push(slot);

            // Delete the orphan swap so it won't recover again next launch.
            let _ = swap::remove_swap(&orphan.swap_path);
        }

        self.bus.info(format!(
            "Recovered {n} unsaved buffer(s) from a previous session"
        ));
        n
    }

    /// Convenience wrapper: scan the real `swap_dir()`.
    ///
    /// Called once from `main` after construction — NOT from `App::new`.
    pub(crate) fn recover_orphan_scratch_buffers(&mut self) -> usize {
        match hjkl_app::swap::swap_dir() {
            Ok(d) => self.recover_orphan_scratch_buffers_from(&d),
            Err(_) => 0,
        }
    }

    /// `:recover [file]` — explicit swap-file recovery.
    ///
    /// Empty arg → force recovery on the current buffer's swap (bypasses the
    /// mtime-newer gate).
    /// Non-empty arg → open/switch to that file via `do_edit`, then force
    /// recovery on the resulting slot.
    /// No swap found → reports an info message to the user.
    pub(crate) fn do_recover(&mut self, path: &str) {
        use hjkl_app::swap;

        if path.is_empty() {
            // Recover the current buffer.
            let slot_idx = self.focused_slot_idx();

            // Guard: must have a filename and a swap path.
            let swap_path = match self.slots[slot_idx].swap_path.as_ref() {
                Some(p) => p.clone(),
                None => {
                    let name = self
                        .active()
                        .filename
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "[No Name]".into());
                    self.bus.info(format!("No swap file found for {name}"));
                    return;
                }
            };
            if !swap_path.exists() {
                let name = self
                    .active()
                    .filename
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "[No Name]".into());
                self.bus.info(format!("No swap file found for {name}"));
                return;
            }

            // Try to read the swap — if unreadable, treat as no-swap.
            if let Err(e) = swap::read_swap(&swap_path) {
                tracing::debug!(%e, ":recover failed to read swap");
                let name = self
                    .active()
                    .filename
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "[No Name]".into());
                self.bus.info(format!("No swap file found for {name}"));
                return;
            }

            // Force recovery (skip stale-mtime gate).
            let pending = self.force_recovery_on_open(slot_idx);
            if !pending {
                // force_recovery_on_open only returns false when the swap is
                // absent or unreadable — covered above; shouldn't reach here.
                let name = self
                    .active()
                    .filename
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "[No Name]".into());
                self.bus.info(format!("No swap file found for {name}"));
            }
        } else {
            // Open/switch to the specified file, then force recovery on it.
            self.do_edit(path, false);
            // After do_edit the focused slot is the newly opened (or switched-to)
            // slot. Force recovery regardless of mtime.
            let slot_idx = self.focused_slot_idx();
            let swap_path_exists = self.slots[slot_idx]
                .swap_path
                .as_ref()
                .map(|p| p.exists())
                .unwrap_or(false);
            if !swap_path_exists {
                self.bus.info(format!("No swap file found for {path}"));
                return;
            }
            let pending = self.force_recovery_on_open(slot_idx);
            if !pending {
                self.bus.info(format!("No swap file found for {path}"));
            }
        }
    }

    /// Switch the active syntax theme to a bundled colorscheme (`"dark"` /
    /// `"light"`), recompute the visible spans, and record the name for
    /// `:colorscheme?`. Shared by `:set background=` and `:colorscheme`.
    pub(crate) fn apply_colorscheme(&mut self, name: &str) {
        let theme: Arc<dyn hjkl_bonsai::Theme + Send + Sync> = match name {
            "light" => Arc::new(DotFallbackTheme::light()),
            _ => Arc::new(DotFallbackTheme::dark()),
        };
        self.syntax.set_theme(theme);
        self.recompute_and_install();
        self.colorscheme = name.to_string();
    }

    /// Check whether opening `slot_idx` (which has just been loaded) requires
    /// a recovery prompt.  If a swap file newer than the on-disk content exists,
    /// sets `self.pending_recovery` and returns `true` (caller should not show
    /// normal "N lines" message).  Returns `false` when no recovery is needed.
    ///
    /// Stale swaps (older than the on-disk file) are deleted silently.
    ///
    /// When `force` is `true` the stale-delete branch is skipped — the user
    /// explicitly asked for recovery (`:recover`) regardless of mtime.
    pub(crate) fn check_recovery_on_open(&mut self, slot_idx: usize) -> bool {
        self.check_recovery_on_open_inner(slot_idx, false)
    }

    /// Force-recovery variant of [`check_recovery_on_open`]: bypasses the
    /// mtime-newer gate so the recovery prompt appears even when the on-disk
    /// file looks newer than the swap (used by `:recover`).
    pub(crate) fn force_recovery_on_open(&mut self, slot_idx: usize) -> bool {
        self.check_recovery_on_open_inner(slot_idx, true)
    }

    fn check_recovery_on_open_inner(&mut self, slot_idx: usize, force: bool) -> bool {
        use hjkl_app::swap;

        let filename = match self.slots[slot_idx].filename.as_ref() {
            Some(p) => p.clone(),
            None => return false,
        };
        let swap_path = match self.slots[slot_idx].swap_path.as_ref() {
            Some(p) => p.clone(),
            None => return false,
        };
        if !swap_path.exists() {
            return false;
        }
        let (header, body) = match swap::read_swap(&swap_path) {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(%e, "failed to read swap on open");
                return false;
            }
        };

        // PID-lock check: if another LIVE process wrote this swap, open the
        // buffer READ-ONLY (vim's "[O]pen Read-Only" for a locked swap). This
        // is uniform across single-file, multi-file, and `:e` opens: the file
        // the user requested stays visible but can't be `:w`-saved over the
        // owning instance. The slot's swap_path is cleared so this process
        // never overwrites the owner's swap. We do NOT remove the slot —
        // dropping an explicitly-requested file (e.g. `hjkl a b c`) would be
        // surprising, and a sole buffer can't be removed at all.
        let our_pid = std::process::id();
        if header.writer_pid != our_pid && swap::pid_is_alive(header.writer_pid) {
            let name = filename.display().to_string();
            let pid = header.writer_pid;
            self.slots[slot_idx].editor.settings_mut().readonly = true;
            self.slots[slot_idx].swap_path = None;
            self.bus.error(format!(
                "E325: \"{name}\" is already open in another hjkl (pid {pid}) — opened read-only"
            ));
            return false;
        }

        // Determine file mtime.
        let file_mtime_ms = std::fs::metadata(&filename)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| {
                t.duration_since(std::time::SystemTime::UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_millis() as u64)
            })
            .unwrap_or(0);
        if !force && header.write_time_unix_ms <= file_mtime_ms {
            // Swap is stale (on-disk is newer). Delete silently.
            let _ = swap::remove_swap(&swap_path);
            return false;
        }
        // Swap is newer than disk (or forced) → prompt for recovery.
        let written_ago = format_swap_age(header.write_time_unix_ms);
        self.pending_recovery = Some(super::PendingRecovery {
            file_path: filename,
            header,
            body,
            swap_path,
            slot_idx,
            written_ago,
        });
        true
    }

    /// Install the recovered swap `body` into slot `slot_idx` and restore the
    /// cursor. Used by the recovery-prompt `y` path.
    ///
    /// MUST signal a full content reset (not an incremental edit): the slot's
    /// syntax tree was parsed against the on-disk content during `build_slot`,
    /// so swapping in the swap body wholesale requires a fresh `parse_initial`.
    /// A raw `BufferEdit::replace_all` only emits an incremental ContentEdit,
    /// which drifts the retained tree against the new bytes → broken
    /// highlighting (#185). `Editor::set_content` sets `pending_content_reset`,
    /// which `sync_after_engine_mutation` routes to `syntax.reset`.
    pub(crate) fn recover_install_content(
        &mut self,
        slot_idx: usize,
        body: &str,
        row: usize,
        col: usize,
    ) {
        // Strip trailing newline — the engine's content format omits it.
        let stripped = body.strip_suffix('\n').unwrap_or(body);
        self.slots[slot_idx].editor.set_content(stripped);
        self.slots[slot_idx].editor.jump_cursor(row, col);
        self.slots[slot_idx].dirty = true;
    }

    /// Handle a keypress while the recovery prompt is active.
    ///
    /// - `y` → load the swap body into the buffer, mark dirty, keep swap.
    /// - `N` / Esc → load the file fresh (already loaded in slot), dismiss prompt.
    /// - `q` → clear the slot content entirely (abort open).
    pub(crate) fn handle_recovery_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        use crossterm::event::KeyCode;
        let pr = match self.pending_recovery.as_ref() {
            Some(p) => p,
            None => return false,
        };
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                let body = pr.body.clone();
                let slot_idx = pr.slot_idx;
                let (row, col) = pr.header.cursor;
                self.recover_install_content(slot_idx, &body, row as usize, col as usize);
                self.pending_recovery = None;
                self.bus.info("Recovered from swap file. Use :w to save.");
                self.sync_after_engine_mutation();
            }
            KeyCode::Char('q') | KeyCode::Char('Q') => {
                // Abort: remove the slot we just added and switch back if possible.
                let slot_idx = self.pending_recovery.as_ref().map(|p| p.slot_idx);
                self.pending_recovery = None;
                if let Some(idx) = slot_idx
                    && self.slots.len() > 1
                {
                    let removed = self.slots.remove(idx);
                    self.syntax.forget(removed.buffer_id);
                    // Fix window pointers.
                    let slot_count = self.slots.len();
                    for win in self.windows.iter_mut().flatten() {
                        if win.slot >= idx && win.slot > 0 {
                            win.slot -= 1;
                        }
                        win.slot = win.slot.min(slot_count.saturating_sub(1));
                    }
                    self.bus.info("Aborted file open.");
                }
            }
            // N, n, Esc → load file fresh (slot already loaded; just dismiss).
            KeyCode::Char('N') | KeyCode::Char('n') | KeyCode::Esc => {
                self.pending_recovery = None;
                self.bus.info("Swap ignored; loaded from disk.");
            }
            _ => {
                // Consume unknown keys — stay in prompt.
            }
        }
        true
    }

    /// `:qa[!]` — quit all. Blocks when any slot is dirty unless `force`.
    fn quit_all(&mut self, force: bool) {
        if !force && let Some(idx) = self.slots.iter().position(|s| s.dirty) {
            let name = self.slots[idx]
                .filename
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "[No Name]".into());
            self.bus.error(format!(
                "E37: No write since last change for buffer \"{name}\" (add ! to override)"
            ));
            return;
        }
        self.exit_requested = true;
    }

    /// `:wqa[!]` — write all named dirty slots then quit.
    fn write_quit_all(&mut self, force: bool) {
        self.write_all();
        self.quit_all(force);
    }

    /// Open or reload a file via `:e [path]` / `:e!`.
    ///
    /// Switch-or-create semantics (Phase C):
    /// - `:e` with no arg → reload current buffer (blocked when dirty
    ///   unless `force`).
    /// - `:e %` → reload current (`%` expands to current filename).
    /// - `:e <path>` where `<path>` matches an open slot → switch to it.
    /// - `:e <path>` for a new path → load the file in a new slot,
    ///   append, and switch active. The previous slot is untouched.
    pub(crate) fn do_edit(&mut self, arg: &str, force: bool) {
        if arg.is_empty() {
            self.reload_current(force);
            return;
        }
        let path_str = if arg.contains('%') {
            let curr = match self.active().filename.as_ref().and_then(|p| p.to_str()) {
                Some(s) => s,
                None => {
                    self.bus.error("E499: Empty file name for '%'");
                    return;
                }
            };
            arg.replace('%', curr)
        } else {
            arg.to_string()
        };
        let path = PathBuf::from(&path_str);
        let target = super::canon_for_match(&path);

        // Switch when the path matches an open slot.
        if let Some(idx) = self
            .slots
            .iter()
            .position(|s| s.filename.as_deref().map(super::canon_for_match) == Some(target.clone()))
        {
            if idx == self.focused_slot_idx() {
                self.reload_current(force);
                return;
            }
            self.switch_to(idx);
            self.bus.info(format!(
                "switched to buffer {}: \"{}\"",
                idx + 1,
                self.active()
                    .filename
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            ));
            return;
        }

        // Otherwise create a new slot.
        let prev_idx = self.focused_slot_idx();
        let prev_pristine = {
            let s = &self.slots[prev_idx];
            s.filename.is_none() && !s.dirty
        };
        match self.open_new_slot(path) {
            Ok(new_slot_idx) => {
                // Track alt-buffer before switching.
                self.prev_active = Some(prev_idx);
                // Point the focused window at the new slot.
                let fw = self.focused_window();
                self.windows[fw].as_mut().expect("focused_window open").slot = new_slot_idx;

                // Drop the pristine default buffer once a real file is open.
                if prev_pristine && self.slots.len() > 1 {
                    let removed = self.slots.remove(prev_idx);
                    self.syntax.forget(removed.buffer_id);
                    // Fix all window slot pointers.
                    let slot_count = self.slots.len();
                    for win in self.windows.iter_mut().flatten() {
                        if win.slot > prev_idx {
                            win.slot -= 1;
                        }
                        win.slot = win.slot.min(slot_count.saturating_sub(1));
                    }
                    self.prev_active = None;
                }

                // Recovery check: if a swap file is newer than the on-disk
                // content, enter the recovery prompt instead of reporting
                // normal line count.  The slot is already loaded with the
                // disk content; 'y' replaces it from the swap.
                let current_slot_idx = self.focused_slot_idx();
                let recovery_pending = self.check_recovery_on_open(current_slot_idx);
                if !recovery_pending {
                    // Arm the PID-lock swap immediately on successful open so a
                    // concurrent second instance sees it before any edit is made.
                    self.arm_swap_on_open(current_slot_idx);
                    let line_count = self.active().editor.buffer().line_count() as usize;
                    let path_display = self
                        .active()
                        .filename
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    self.bus.info(format!("\"{path_display}\" {line_count}L"));
                }
                self.refresh_git_signs_force();
            }
            Err(msg) => {
                self.bus.info(msg);
            }
        }
    }

    /// Reload the active slot from disk (`:e` no-arg / `:e %`).
    pub(crate) fn reload_current(&mut self, force: bool) {
        let path = match self.active().filename.clone() {
            Some(p) => p,
            None => {
                self.bus.error("E32: No file name");
                return;
            }
        };
        if !force && self.active().dirty {
            self.bus
                .error("E37: No write since last change (add ! to override)");
            return;
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                self.bus
                    .error(format!("E484: Can't open file {}: {e}", path.display()));
                return;
            }
        };
        let trimmed = content.strip_suffix('\n').unwrap_or(&content);
        let line_count = trimmed.lines().count();
        let byte_count = content.len();
        // Preserve cursor across reload (vim/nvim :e behaviour). Clamp the
        // saved (row, col) to the new buffer size; reload_current is only
        // called for the active slot so the pre-reload cursor is the user's
        // current position.
        let (prev_row, prev_col) = self.active().editor.cursor();
        self.active_mut().editor.set_content(trimmed);
        let new_rows = self.active().editor.buffer().line_count() as usize;
        let target_row = prev_row.min(new_rows.saturating_sub(1));
        self.active_mut().editor.jump_cursor(target_row, prev_col);
        // Reposition viewport so the restored cursor is visible (with scrolloff).
        // Without this the viewport stays at its pre-reload top_row and the
        // cursor can land offscreen if the file shrank or grew.
        self.active_mut().editor.ensure_cursor_in_scrolloff();
        self.active_mut().is_new_file = false;
        // Record fresh disk metadata and clear the disk-change flag.
        if let Ok(meta) = std::fs::metadata(&path) {
            self.active_mut().disk_mtime = meta.modified().ok();
            self.active_mut().disk_len = Some(meta.len());
        }
        self.active_mut().disk_state = DiskState::Synced;
        let buffer_id = self.active().buffer_id;
        // Non-blocking: Loading case activates via poll_grammar_loads each tick.
        let outcome = self.syntax.set_language_for_path(buffer_id, &path);
        let _ = outcome.is_known(); // Suppresses unused-result warning.
        self.syntax.reset(buffer_id);

        self.active_mut()
            .editor
            .install_ratatui_syntax_spans(Vec::new());
        // recompute_and_install runs render_viewport sync — no preview warm-up needed.
        self.recompute_and_install();
        self.active_mut().snapshot_saved();
        self.refresh_git_signs_force();
        self.bus.info(format!(
            "\"{}\" {line_count}L, {byte_count}B",
            path.display()
        ));
    }

    /// Check all open slots for external changes. Called on focus-regain
    /// and by `:checktime`. Non-dirty slots whose file changed are reloaded
    /// automatically; dirty slots and deleted files get a status warning
    /// (emitted once per state transition).
    pub(crate) fn checktime_all(&mut self) {
        let mut messages: Vec<String> = Vec::new();
        for idx in 0..self.slots.len() {
            let path = match self.slots[idx].filename.clone() {
                Some(p) => p,
                None => continue,
            };
            match std::fs::metadata(&path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    let prev = self.slots[idx].disk_state;
                    self.slots[idx].disk_state = DiskState::DeletedOnDisk;
                    if prev != DiskState::DeletedOnDisk {
                        messages.push(format!("W: \"{}\" deleted on disk", path.display()));
                    }
                }
                Err(_) => {
                    // Non-NotFound I/O error — skip silently.
                }
                Ok(meta) => {
                    let new_mtime = meta.modified().ok();
                    let new_len = meta.len();
                    // Compare against stored baseline.
                    let changed = self.slots[idx].disk_mtime != new_mtime
                        || self.slots[idx].disk_len != Some(new_len);
                    if !changed {
                        if self.slots[idx].disk_state == DiskState::DeletedOnDisk {
                            // File reappeared. If we were in Deleted state,
                            // reset to Synced so a follow-up checktime can pick
                            // it up properly (the next block below handles
                            // the actual reload when dirty==false).
                            self.slots[idx].disk_state = DiskState::Synced;
                        }
                        continue;
                    }
                    // File changed. Warn (don't reload) when the buffer is dirty
                    // OR `:set noautoreload` is active; otherwise auto-reload.
                    let autoreload = self.slots[idx].editor.settings().autoreload;
                    if self.slots[idx].dirty || !autoreload {
                        let prev = self.slots[idx].disk_state;
                        self.slots[idx].disk_state = DiskState::ChangedOnDisk;
                        if prev != DiskState::ChangedOnDisk {
                            let why = if self.slots[idx].dirty {
                                "buffer is dirty, use :e! to reload"
                            } else {
                                "autoreload off, use :e to reload"
                            };
                            messages
                                .push(format!("W: \"{}\" changed on disk ({why})", path.display()));
                        }
                    } else {
                        // Clean buffer — reload automatically.
                        let content = match std::fs::read_to_string(&path) {
                            Ok(c) => c,
                            Err(_) => continue,
                        };
                        let trimmed = content.strip_suffix('\n').unwrap_or(&content);
                        // Preserve cursor row + column, clamped to the new
                        // content (vim's autoread keeps the cursor where it was).
                        let (cur_row, cur_col) = self.slots[idx].editor.cursor();
                        self.slots[idx].editor.set_content(trimmed);
                        let new_line_count = self.slots[idx].editor.buffer().line_count() as usize;
                        let clamped_row = cur_row.min(new_line_count.saturating_sub(1));
                        self.slots[idx].editor.jump_cursor(clamped_row, cur_col);
                        self.slots[idx].is_new_file = false;
                        // Update disk metadata baseline.
                        self.slots[idx].disk_mtime = new_mtime;
                        self.slots[idx].disk_len = Some(new_len);
                        self.slots[idx].disk_state = DiskState::Synced;
                        self.slots[idx].snapshot_saved();
                        // Refresh syntax + git for the reloaded slot.
                        let buffer_id = self.slots[idx].buffer_id;
                        // Non-blocking: Loading case activates via poll_grammar_loads each tick.
                        let outcome = self.syntax.set_language_for_path(buffer_id, &path);
                        let _ = outcome.is_known(); // Suppresses unused-result warning.
                        self.syntax.reset(buffer_id);

                        if idx == self.focused_slot_idx() {
                            self.slots[idx]
                                .editor
                                .install_ratatui_syntax_spans(Vec::new());
                            // recompute_and_install runs render_viewport sync — no
                            // preview warm-up needed.
                            self.recompute_and_install();
                            self.refresh_git_signs_force();
                        }
                        messages.push(format!("\"{}\" reloaded from disk", path.display()));
                    }
                }
            }
        }
        if !messages.is_empty() {
            self.bus.info(messages.join(" | "));
        }
    }

    // ─── Phase 2 Tab helpers ──────────────────────────────────────────────────

    /// `:tabfirst` / `:tabrewind` / `:tabr` — jump to the first tab.
    pub(super) fn do_tabfirst(&mut self) {
        if self.active_tab == 0 {
            let m = self.tabs.len();
            self.bus.info(format!("tab 1/{m}"));
            return;
        }
        self.switch_tab(0);
        let m = self.tabs.len();
        self.bus.info(format!("tab 1/{m}"));
    }

    /// `:tablast` — jump to the last tab.
    pub(super) fn do_tablast(&mut self) {
        let last = self.tabs.len() - 1;
        if self.active_tab == last {
            let m = self.tabs.len();
            self.bus.info(format!("tab {m}/{m}"));
            return;
        }
        self.switch_tab(last);
        let m = self.tabs.len();
        self.bus.info(format!("tab {m}/{m}"));
    }

    /// `:tabonly` / `:tabo` — close all tabs except the current one.
    pub(super) fn do_tabonly(&mut self) {
        if self.tabs.len() <= 1 {
            self.bus.info("tabonly");
            return;
        }
        // Collect window ids from all tabs except the active one and drop them.
        for (i, tab) in self.tabs.iter().enumerate() {
            if i == self.active_tab {
                continue;
            }
            for wid in tab.layout.leaves() {
                self.windows[wid] = None;
            }
        }

        // Keep only the active tab.
        let active_tab = self.tabs[self.active_tab].clone();
        self.tabs = vec![active_tab];
        self.active_tab = 0;

        self.bus.info("tabonly");
    }

    /// Close all tabs whose index is strictly greater than `active_tab`.
    ///
    /// After this call `self.tabs.len() == self.active_tab + 1`. No-op when
    /// the active tab is already the last one.
    pub(crate) fn close_tabs_to_right(&mut self) {
        let active = self.active_tab;
        if active + 1 >= self.tabs.len() {
            // Already the last tab — nothing to close.
            return;
        }
        // Drop window slots for every tab that lives to the right.
        for i in (active + 1)..self.tabs.len() {
            for wid in self.tabs[i].layout.leaves() {
                self.windows[wid] = None;
            }
        }
        self.tabs.truncate(active + 1);
        // active_tab index is unchanged; it still points to the same tab.
    }

    /// Close all tabs whose index is strictly less than `active_tab`.
    ///
    /// After this call `self.tabs.len() == self.tabs.len() - active_tab`
    /// and `active_tab == 0`. No-op when the active tab is already the first.
    pub(crate) fn close_tabs_to_left(&mut self) {
        let active = self.active_tab;
        if active == 0 {
            // Already the first tab — nothing to close.
            return;
        }
        // Drop window slots for every tab that lives to the left.
        for i in 0..active {
            for wid in self.tabs[i].layout.leaves() {
                self.windows[wid] = None;
            }
        }
        // Drain the prefix, shifting remaining tabs to the front.
        self.tabs.drain(0..active);
        self.active_tab = 0;
    }

    /// `:tabmove [N|+N|-N]` — reorder tabs.
    ///
    /// No arg → move to end. `N` → absolute 0-based position. `+N`/`-N` →
    /// relative. Out-of-range positions are clamped silently.
    pub(super) fn do_tabmove(&mut self, arg: &str) {
        let len = self.tabs.len();
        let target = if arg.is_empty() {
            // No arg: move to end.
            len - 1
        } else if let Some(rest) = arg.strip_prefix('+') {
            let delta: usize = rest.trim().parse().unwrap_or(0);
            (self.active_tab + delta).min(len - 1)
        } else if let Some(rest) = arg.strip_prefix('-') {
            let delta: usize = rest.trim().parse().unwrap_or(0);
            self.active_tab.saturating_sub(delta)
        } else {
            let pos: usize = arg.trim().parse().unwrap_or(self.active_tab);
            pos.min(len - 1)
        };

        if target == self.active_tab {
            self.bus.info("tabmove");
            return;
        }

        let tab = self.tabs.remove(self.active_tab);
        self.tabs.insert(target, tab);
        self.active_tab = target;
        self.bus.info("tabmove");
    }

    /// `:tabs` — show an info popup listing all tabs with their active buffer
    /// name. The `>` marker indicates the active tab.
    pub(super) fn do_tabs(&mut self) {
        let mut lines: Vec<String> = Vec::new();
        for (i, tab) in self.tabs.iter().enumerate() {
            let label = format!("Tab page {}", i + 1);
            lines.push(label);
            // The name of the focused window's buffer.
            let name = if let Some(win) = self.windows[tab.focused_window].as_ref() {
                let slot = &self.slots[win.slot];
                slot.filename
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "[No Name]".to_string())
            } else {
                "[No Name]".to_string()
            };
            let marker = if i == self.active_tab { '>' } else { ' ' };
            lines.push(format!("{marker} {name}"));
        }
        self.info_popup = Some(InfoPopup::new("tabs", lines.join("\n")));
    }

    // ─── Tab helpers ─────────────────────────────────────────────────────────

    /// `:tabnew [file]` / `:tabedit [file]` / `:tabe [file]`
    ///
    /// Open a new tab. With a file argument: load the file into a new slot.
    /// Without: open an empty unnamed buffer. The new tab gets its own layout
    /// and focused window; windows and slots are shared globally.
    pub(super) fn do_tabnew(&mut self, arg: &str) {
        use crate::app::STATUS_LINE_HEIGHT;
        use crate::app::window::{LayoutTree, Tab, Window};
        use crate::host::TuiHost;
        use hjkl_buffer::Buffer;
        use hjkl_engine::{Editor, Options};

        // Save current tab's viewport state before switching.
        self.sync_viewport_from_editor();

        // Determine the slot for the new tab.
        let new_slot_idx = if arg.is_empty() {
            // Empty scratch buffer.
            let buffer_id = self.next_buffer_id;
            self.next_buffer_id += 1;
            let host = TuiHost::new();
            let mut editor = Editor::new(Buffer::new(), host, Options::default());
            if let Ok(size) = crossterm::terminal::size() {
                let vp = editor.host_mut().viewport_mut();
                vp.width = size.0;
                vp.height = size.1.saturating_sub(STATUS_LINE_HEIGHT);
            }
            let _ = editor.take_content_edits();
            let _ = editor.take_content_reset();
            let mut slot = super::BufferSlot {
                buffer_id,
                editor,
                filename: None,
                dirty: false,
                is_new_file: false,
                is_untracked: false,
                diag_signs: Vec::new(),
                diag_signs_lsp: Vec::new(),
                lsp_diags: Vec::new(),
                last_lsp_dirty_gen: None,
                git_signs: Vec::new(),
                last_git_dirty_gen: None,
                last_git_refresh_at: std::time::Instant::now(),
                blame: Vec::new(),
                last_blame_dirty_gen: None,
                last_blame_refresh_at: std::time::Instant::now(),
                saved_hash: 0,
                saved_len: 0,
                signature_cache: None,
                disk_mtime: None,
                disk_len: None,
                disk_state: super::DiskState::Synced,
                swap_path: None,
                last_swap_dirty_gen: None,
                last_fold_dirty_gen: None,
            };
            slot.snapshot_saved();
            self.slots.push(slot);
            self.slots.len() - 1
        } else {
            match self.open_new_slot(std::path::PathBuf::from(arg)) {
                Ok(idx) => idx,
                Err(msg) => {
                    self.bus.info(msg);
                    return;
                }
            }
        };

        // Allocate a new window for the new tab.
        let new_win_id = self.next_window_id;
        self.next_window_id += 1;
        self.windows.push(Some(Window::new(new_slot_idx)));

        // Push the new tab and switch to it.
        self.tabs
            .push(Tab::new(LayoutTree::Leaf(new_win_id), new_win_id));
        self.active_tab = self.tabs.len() - 1;

        // Sync viewport for the new tab's editor.
        self.sync_viewport_to_editor();
        self.bus.info("tabnew");
    }

    /// `:tabnext` / `:tabn` — cycle to the next tab (wraps).
    pub(super) fn do_tabnext(&mut self) {
        if self.tabs.len() <= 1 {
            self.bus.warn("only one tab");
            return;
        }
        let new_tab = (self.active_tab + 1) % self.tabs.len();
        self.switch_tab(new_tab);
        let n = self.active_tab + 1;
        let m = self.tabs.len();
        self.bus.info(format!("tab {n}/{m}"));
    }

    /// `:tabprev` / `:tabp` / `:tabN` — cycle to the previous tab (wraps).
    pub(super) fn do_tabprev(&mut self) {
        if self.tabs.len() <= 1 {
            self.bus.warn("only one tab");
            return;
        }
        let new_tab = (self.active_tab + self.tabs.len() - 1) % self.tabs.len();
        self.switch_tab(new_tab);
        let n = self.active_tab + 1;
        let m = self.tabs.len();
        self.bus.info(format!("tab {n}/{m}"));
    }

    /// `:tabclose` / `:tabc` — close the current tab.
    ///
    /// Refuses when only one tab remains (E444). On success, drops all windows
    /// that belonged exclusively to this tab and adjusts `active_tab`.
    pub(super) fn do_tabclose(&mut self) {
        if self.tabs.len() <= 1 {
            self.bus.error("E444: Cannot close last tab");
            return;
        }
        // Collect window ids in the closing tab.
        let closing_leaves = self.tabs[self.active_tab].layout.leaves();
        // Drop those windows.
        for wid in closing_leaves {
            self.windows[wid] = None;
        }

        // Remove the tab.
        self.tabs.remove(self.active_tab);

        // Adjust active_tab: clamp to last if we were at the end.
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        }

        // Restore the new active tab's cursor/scroll into the editor.
        self.sync_viewport_to_editor();
        let n = self.active_tab + 1;
        let m = self.tabs.len();
        self.bus.info(format!("tab {n}/{m}"));
    }

    // ── LSP diagnostic navigation ─────────────────────────────────────────────

    /// `:lopen` — open a picker listing all LSP diagnostics for the active buffer.
    pub(crate) fn open_diag_picker(&mut self) {
        let diags = self.active().lsp_diags.clone();
        if diags.is_empty() {
            self.bus.warn("no diagnostics");
            return;
        }

        let entries: Vec<crate::picker_sources::DiagEntry> = diags
            .iter()
            .map(|d| {
                let src = d.source.as_deref().unwrap_or("");
                let code = d.code.as_deref().unwrap_or("");
                let annotation = match (src, code) {
                    ("", "") => String::new(),
                    (s, "") | ("", s) => format!(" ({s})"),
                    (s, c) => format!(" ({s}[{c}])"),
                };
                crate::picker_sources::DiagEntry {
                    label: format!(
                        "{}:{} [{}]{} {}",
                        d.start_row + 1,
                        d.start_col + 1,
                        sev_label(d.severity),
                        annotation,
                        d.message.lines().next().unwrap_or("")
                    ),
                    start_row: d.start_row,
                    start_col: d.start_col,
                }
            })
            .collect();

        let source = Box::new(crate::picker_sources::DiagSource::new(entries));
        self.picker = Some(crate::picker::Picker::new(source));
    }

    /// Jump to the next diagnostic (optionally filtered by minimum severity).
    /// `severity` = `Some(Error)` means skip non-Error diags.
    pub(crate) fn lnext_severity(&mut self, severity: Option<super::DiagSeverity>) {
        let (row, col) = self.active().editor.cursor();
        // Clone data we need before any mutable borrow.
        let candidates: Vec<super::LspDiag> = self
            .active()
            .lsp_diags
            .iter()
            .filter(|d| severity.is_none_or(|s| d.severity <= s))
            .cloned()
            .collect();

        if candidates.is_empty() {
            self.bus.warn("no diagnostics");
            return;
        }

        // Find the first diag after the current cursor position; wrap around.
        let target = candidates
            .iter()
            .find(|d| (d.start_row, d.start_col) > (row, col))
            .or_else(|| candidates.first())
            .cloned();

        if let Some(d) = target {
            self.active_mut()
                .editor
                .jump_cursor(d.start_row, d.start_col);
            self.active_mut().editor.ensure_cursor_in_scrolloff();
            self.sync_viewport_from_editor();
            let msg = d.message.lines().next().unwrap_or("").to_string();
            self.bus
                .info(format!("[{}] {}", sev_label(d.severity), msg));
        }
    }

    /// Jump to the previous diagnostic (optionally filtered).
    pub(crate) fn lprev_severity(&mut self, severity: Option<super::DiagSeverity>) {
        let (row, col) = self.active().editor.cursor();
        let candidates: Vec<super::LspDiag> = self
            .active()
            .lsp_diags
            .iter()
            .filter(|d| severity.is_none_or(|s| d.severity <= s))
            .cloned()
            .collect();

        if candidates.is_empty() {
            self.bus.warn("no diagnostics");
            return;
        }

        // Find the last diag before the current cursor position; wrap around.
        let target = candidates
            .iter()
            .rev()
            .find(|d| (d.start_row, d.start_col) < (row, col))
            .or_else(|| candidates.last())
            .cloned();

        if let Some(d) = target {
            self.active_mut()
                .editor
                .jump_cursor(d.start_row, d.start_col);
            self.active_mut().editor.ensure_cursor_in_scrolloff();
            self.sync_viewport_from_editor();
            let msg = d.message.lines().next().unwrap_or("").to_string();
            self.bus
                .info(format!("[{}] {}", sev_label(d.severity), msg));
        }
    }

    /// `:lfirst` — jump to the first diagnostic.
    pub(crate) fn ldiag_first(&mut self) {
        let target = self.active().lsp_diags.first().cloned();
        match target {
            None => {
                self.bus.warn("no diagnostics");
            }
            Some(d) => {
                self.active_mut()
                    .editor
                    .jump_cursor(d.start_row, d.start_col);
                self.active_mut().editor.ensure_cursor_in_scrolloff();
                self.sync_viewport_from_editor();
                let msg = d.message.lines().next().unwrap_or("").to_string();
                self.bus
                    .info(format!("[{}] {}", sev_label(d.severity), msg));
            }
        }
    }

    /// `:llast` — jump to the last diagnostic.
    pub(crate) fn ldiag_last(&mut self) {
        let target = self.active().lsp_diags.last().cloned();
        match target {
            None => {
                self.bus.warn("no diagnostics");
            }
            Some(d) => {
                self.active_mut()
                    .editor
                    .jump_cursor(d.start_row, d.start_col);
                self.active_mut().editor.ensure_cursor_in_scrolloff();
                self.sync_viewport_from_editor();
                let msg = d.message.lines().next().unwrap_or("").to_string();
                self.bus
                    .info(format!("[{}] {}", sev_label(d.severity), msg));
            }
        }
    }

    /// Whether the active buffer has any LSP diagnostic that touches `row`
    /// (its span covers the row). Used by the gutter context menu (#114 P6)
    /// to decide whether to surface diagnostic-specific entries.
    pub(crate) fn diagnostic_on_row(&self, row: usize) -> bool {
        self.active()
            .lsp_diags
            .iter()
            .any(|d| d.start_row <= row && row <= d.end_row)
    }

    /// `<leader>d` — show all diagnostics overlapping the cursor in info popup.
    pub(crate) fn show_diag_at_cursor(&mut self) {
        let (row, col) = self.active().editor.cursor();
        let diags = &self.active().lsp_diags;
        let hits: Vec<_> = diags
            .iter()
            .filter(|d| {
                // Overlaps cursor if cursor falls within [start, end).
                let after_start = (row, col) >= (d.start_row, d.start_col);
                let before_end = (row, col) < (d.end_row, d.end_col)
                    || (row == d.end_row && d.end_col == 0 && row == d.start_row);
                after_start && (before_end || row == d.start_row)
            })
            .collect();

        if hits.is_empty() {
            self.bus.warn("no diagnostics at cursor");
            return;
        }

        let text = hits
            .iter()
            .map(|d| format!("[{}] {}", sev_label(d.severity), d.message))
            .collect::<Vec<_>>()
            .join("\n---\n");
        self.info_popup = Some(InfoPopup::new("diagnostics", text));
    }

    /// `:LspInfo` — show running LSP servers + diagnostic info about the
    /// active buffer's attach state. Designed to surface the most common
    /// causes of "why isn't LSP working".
    pub(crate) fn show_lsp_info(&mut self) {
        let mut lines = Vec::new();

        // Top: enabled / disabled state.
        if self.lsp.is_none() {
            lines.push("LSP: disabled (set [lsp] enabled = true in config)".into());
            self.info_popup = Some(InfoPopup::new("lsp info", lines.join("\n")));
            return;
        }
        lines.push("LSP: enabled".into());

        // Active buffer diagnostic.
        let slot = self.active();
        match slot.filename.as_ref() {
            None => lines.push("Active buffer: [No Name] — no LSP attach possible".into()),
            Some(p) => {
                let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("(none)");
                let lang = super::lsp_glue::language_id_for_ext(ext);
                lines.push(format!("Active buffer: {} (ext: {ext})", p.display()));
                match lang {
                    None => lines.push(format!("  → no language id mapped for .{ext} extension")),
                    Some(lang_id) => {
                        let configured = self.config.lsp.servers.contains_key(lang_id);
                        lines.push(format!("  → language: {lang_id}"));
                        if !configured {
                            lines.push(format!(
                                "  → NO server configured for {lang_id} \
                                 (add [lsp.servers.{lang_id}] to your config.toml)"
                            ));
                        } else {
                            lines.push(format!(
                                "  → server configured: {}",
                                self.config.lsp.servers[lang_id].command
                            ));
                        }
                    }
                }
            }
        }

        lines.push(String::new());

        // Configured servers in user config.
        lines.push("Configured servers:".into());
        if self.config.lsp.servers.is_empty() {
            lines.push("  (none — add [lsp.servers.<lang>] blocks)".into());
        } else {
            for (lang, cfg) in &self.config.lsp.servers {
                lines.push(format!("  {lang}: {}", cfg.command));
            }
        }

        lines.push(String::new());

        // Running servers.
        if self.lsp_state.is_empty() {
            lines.push("Running servers: (none)".into());
        } else {
            lines.push("Running servers:".into());
            for (i, (key, info)) in self.lsp_state.iter().enumerate() {
                let state = if info.initialized {
                    "initialized"
                } else {
                    "starting"
                };
                let caps = summarize_capabilities(&info.capabilities);
                lines.push(format!(
                    "  [{}] {} @ {}",
                    i + 1,
                    key.language,
                    key.root.display()
                ));
                lines.push(format!("      state: {state}"));
                if !caps.is_empty() {
                    lines.push(format!("      capabilities: {caps}"));
                }
            }
        }

        // Anvil section: show anvil status for each configured server's binary.
        lines.push(String::new());
        lines.push("Anvil tool status:".into());
        if let Some(registry) = self.anvil_registry.as_ref() {
            let mut found_any = false;
            for (lang, cfg) in &self.config.lsp.servers {
                let server_bin = &cfg.command;
                // Look up the binary name in the registry.
                if let Some(spec) = registry.get(server_bin) {
                    found_any = true;
                    match hjkl_anvil::store::read_rev(server_bin) {
                        Ok(Some(rev)) => {
                            lines.push(format!(
                                "  {lang} / {server_bin} (anvil: installed {})",
                                rev.version
                            ));
                        }
                        Ok(None) => {
                            lines.push(format!(
                                "  {lang} / {server_bin} (anvil: not installed, available {})",
                                spec.version
                            ));
                        }
                        Err(_) => {
                            lines.push(format!("  {lang} / {server_bin} (anvil: store error)"));
                        }
                    }
                } else {
                    lines.push(format!("  {lang} / {server_bin} (anvil: not in registry)"));
                }
            }
            if !found_any {
                lines.push("  (no LSP servers configured)".into());
            }
        } else {
            lines.push("  (registry not available)".into());
        }

        self.info_popup = Some(InfoPopup::new("lsp info", lines.join("\n")));
    }

    /// `:Anvil install <name>` — queue a background install job.
    pub(crate) fn anvil_install(&mut self, name: &str) {
        let Some(registry) = self.anvil_registry.as_ref() else {
            self.bus.error("anvil: registry not available");
            return;
        };
        let Some(spec) = registry.get(name) else {
            self.bus.error(format!("anvil: unknown tool `{name}`"));
            return;
        };
        let spec = spec.clone();
        let handle = self.anvil_pool.install(name.to_string(), spec);
        self.anvil_handles.insert(name.to_string(), handle);
        self.bus.info(format!("anvil: installing {name}\u{2026}"));
    }

    /// `:Anvil uninstall <name>` — remove the tool's package dir and bin symlink.
    pub(crate) fn anvil_uninstall(&mut self, name: &str) {
        let pkg_dir = hjkl_anvil::store::package_dir(name);
        let bin_dir = hjkl_anvil::store::bin_dir();
        match (pkg_dir, bin_dir) {
            (Ok(pkg), Ok(bin)) => {
                let _ = std::fs::remove_dir_all(&pkg);
                // Remove the bin symlink (name comes from spec.bin, not the tool key).
                if let Some(spec) = self.anvil_registry.as_ref().and_then(|r| r.get(name)) {
                    let link = bin.join(&spec.bin);
                    let _ = std::fs::remove_file(&link);
                }
                self.bus.info(format!("anvil: removed {name}"));
            }
            _ => {
                self.bus
                    .error(format!("anvil: failed to resolve paths for {name}"));
            }
        }
    }

    /// `:Anvil update <name>` — reinstall if the on-disk `.rev` doesn't match
    /// the registry's pinned version.
    pub(crate) fn anvil_update(&mut self, name: &str) {
        let Some(registry) = self.anvil_registry.as_ref() else {
            self.bus.error("anvil: registry not available");
            return;
        };
        let Some(spec) = registry.get(name) else {
            self.bus.error(format!("anvil: unknown tool `{name}`"));
            return;
        };
        let spec_version = spec.version.clone();
        let installed = hjkl_anvil::store::read_rev(name).ok().flatten();
        let needs_update = installed.map(|r| r.version != spec_version).unwrap_or(true);
        if !needs_update {
            self.bus.info(format!("anvil: {name} already up to date"));
            return;
        }
        self.anvil_install(name);
    }

    /// `:Anvil update` (no args) — update every installed-but-outdated tool.
    pub(crate) fn anvil_update_all(&mut self) {
        let Some(registry) = self.anvil_registry.as_ref() else {
            self.bus.error("anvil: registry not available");
            return;
        };
        let names: Vec<String> = registry.names().map(String::from).collect();
        for name in &names {
            if let Ok(Some(_)) = hjkl_anvil::store::read_rev(name) {
                self.anvil_update(name);
            }
        }
        self.bus.info("anvil: update sweep started");
    }
}

/// Short severity label.
fn sev_label(s: super::DiagSeverity) -> &'static str {
    match s {
        super::DiagSeverity::Error => "E",
        super::DiagSeverity::Warning => "W",
        super::DiagSeverity::Info => "I",
        super::DiagSeverity::Hint => "H",
    }
}

/// Build an [`hjkl_ex::ExpandContext`] from current app state for Phase 7
/// filename expansion (`%`, `#`, `<cword>`, `<cWORD>`).
///
/// `cword` / `cwword` are wired to `None` for now — the engine API for
/// word-under-cursor is not trivially accessible without a borrow chain
/// that conflicts with the `&mut App` call sites.
/// TODO(phase7): wire cword/cwword once `Editor::word_under_cursor()` is
/// stable in hjkl-engine.
fn build_expand_context(app: &App) -> hjkl_ex::ExpandContext<'_> {
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

/// Build a comma-separated list of capability names present in an LSP
/// server capabilities JSON object.
fn summarize_capabilities(caps: &serde_json::Value) -> String {
    let known = &[
        ("hoverProvider", "hover"),
        ("definitionProvider", "definition"),
        ("completionProvider", "completion"),
        ("referencesProvider", "references"),
        ("documentFormattingProvider", "formatting"),
        ("renameProvider", "rename"),
        ("codeActionProvider", "codeAction"),
        ("signatureHelpProvider", "signatureHelp"),
    ];
    let mut out = Vec::new();
    if let Some(obj) = caps.as_object() {
        for (key, label) in known {
            if obj
                .get(*key)
                .is_some_and(|v| v.as_bool().unwrap_or(true) && !v.is_null())
            {
                out.push(*label);
            }
        }
    }
    out.join(", ")
}

/// Format milliseconds-since-epoch delta as a human-readable relative time.
fn format_swap_age(write_time_unix_ms: u64) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(write_time_unix_ms);
    let delta_secs = now_ms.saturating_sub(write_time_unix_ms) / 1000;
    if delta_secs < 60 {
        format!("{delta_secs}s ago")
    } else if delta_secs < 3600 {
        format!("{}m ago", delta_secs / 60)
    } else {
        format!("{}h ago", delta_secs / 3600)
    }
}
