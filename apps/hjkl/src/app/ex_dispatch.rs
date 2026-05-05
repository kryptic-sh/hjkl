use hjkl_bonsai::DotFallbackTheme;
use hjkl_editor::runtime::ex::{self, ExEffect};
use hjkl_engine::{Host, Query};
use std::path::PathBuf;
use std::sync::Arc;

use super::{App, DiskState};

impl App {
    /// Execute an ex command string (without the leading `:`).
    pub(crate) fn dispatch_ex(&mut self, cmd: &str) {
        let canon = ex::canonical_command_name(cmd);
        let cmd: &str = canon.as_ref();
        if cmd == "perf" {
            self.perf_overlay = !self.perf_overlay;
            self.recompute_hits = 0;
            self.recompute_throttled = 0;
            self.recompute_runs = 0;
            self.status_message = Some(if self.perf_overlay {
                "perf overlay: on (counters reset)".into()
            } else {
                "perf overlay: off".into()
            });
            return;
        }
        if let Some(rest) = cmd.strip_prefix("set background=") {
            match rest.trim() {
                "dark" => {
                    self.syntax.set_theme(Arc::new(DotFallbackTheme::dark()));
                    self.active_mut().last_recompute_key = None;
                    self.recompute_and_install();
                    self.status_message = Some("background=dark".into());
                    return;
                }
                "light" => {
                    self.syntax.set_theme(Arc::new(DotFallbackTheme::light()));
                    self.active_mut().last_recompute_key = None;
                    self.recompute_and_install();
                    self.status_message = Some("background=light".into());
                    return;
                }
                other => {
                    self.status_message = Some(format!("E: unknown background value: {other}"));
                    return;
                }
            }
        }

        if cmd == "picker" {
            self.open_picker();
            return;
        }

        // `:rg [pattern]` — open the ripgrep content-search picker.
        if cmd == "rg" || cmd.starts_with("rg ") {
            let pattern = cmd.strip_prefix("rg ").map(str::trim);
            self.open_grep_picker(pattern);
            return;
        }

        // E1 — `:b [num|name]` — must be matched BEFORE the `bn`/`bp` block.
        if cmd == "b" || cmd.starts_with("b ") {
            let arg = cmd.strip_prefix("b ").map(str::trim).unwrap_or("").trim();
            if arg.is_empty() {
                self.status_message = Some("E94: No matching buffer".into());
            } else if arg.chars().all(|c| c.is_ascii_digit()) {
                let n: usize = arg.parse().unwrap_or(0);
                if n == 0 || n > self.slots.len() {
                    self.status_message = Some(format!("E86: Buffer {n} does not exist"));
                } else {
                    self.switch_to(n - 1);
                }
            } else {
                let arg_lower = arg.to_lowercase();
                let matches: Vec<usize> = self
                    .slots
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| {
                        s.filename
                            .as_ref()
                            .and_then(|p| p.file_name())
                            .and_then(|n| n.to_str())
                            .map(|n| n.to_lowercase().contains(&arg_lower))
                            .unwrap_or(false)
                    })
                    .map(|(i, _)| i)
                    .collect();
                match matches.len() {
                    0 => {
                        self.status_message = Some(format!("E94: No matching buffer for {arg}"));
                    }
                    1 => {
                        self.switch_to(matches[0]);
                    }
                    _ => {
                        self.status_message = Some(format!("E93: More than one match for {arg}"));
                    }
                }
            }
            return;
        }

        // Multi-buffer commands — canonical names from COMMAND_NAMES table.
        match cmd {
            "bnext" => {
                self.buffer_next();
                return;
            }
            "bprevious" | "bNext" => {
                self.buffer_prev();
                return;
            }
            "bdelete" => {
                self.buffer_delete(false);
                return;
            }
            "bdelete!" => {
                self.buffer_delete(true);
                return;
            }
            "bfirst" => {
                self.switch_to(0);
                return;
            }
            "blast" => {
                let last = self.slots.len().saturating_sub(1);
                self.switch_to(last);
                return;
            }
            "buffers" | "ls" | "files" => {
                self.status_message = Some(self.list_buffers());
                return;
            }
            "clipboard" => {
                self.status_message = Some(self.clipboard_status());
                return;
            }
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
            "bpicker" => {
                self.open_buffer_picker();
                return;
            }
            _ => {}
        }

        if cmd == "edit" || cmd == "edit!" || cmd.starts_with("edit ") || cmd.starts_with("edit!") {
            let force = cmd.starts_with("edit!");
            let arg = if let Some(rest) = cmd.strip_prefix("edit!") {
                rest.trim()
            } else if let Some(rest) = cmd.strip_prefix("edit ") {
                rest.trim()
            } else {
                ""
            };
            self.do_edit(arg, force);
            return;
        }

        // `:checktime` — check all open buffers for changes on disk.
        if cmd == "checktime" {
            self.checktime_all();
            return;
        }

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
                self.status_message =
                    Some("E13: file has changed on disk (add ! to override)".into());
                return;
            }
            if self.do_save(target) {
                self.active_mut().disk_state = DiskState::Synced;
            }
            return;
        }

        match ex::run(&mut self.slots[self.active].editor, cmd) {
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
                // E4: multi-slot — close active slot, stay in app.
                if self.slots.len() > 1 {
                    self.buffer_delete(force);
                    return;
                }
                // Last slot: original quit semantics.
                if force || save {
                    self.exit_requested = true;
                } else if self.active().dirty {
                    self.status_message =
                        Some("E37: No write since last change (add ! to override)".into());
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
                if self.slots[self.active].editor.take_dirty() {
                    let elapsed = self.slots[self.active].refresh_dirty_against_saved();
                    self.last_signature_us = elapsed;
                    let buffer_id = self.slots[self.active].buffer_id;
                    if self.slots[self.active].editor.take_content_reset() {
                        self.syntax.reset(buffer_id);
                    }
                    let edits = self.slots[self.active].editor.take_content_edits();
                    if !edits.is_empty() {
                        self.syntax.apply_edits(buffer_id, &edits);
                    }
                    self.recompute_and_install();
                }
                self.status_message = Some(if count == 0 {
                    "Pattern not found".into()
                } else {
                    format!("{count} substitutions on {lines_changed} lines")
                });
            }
            ExEffect::Info(msg) => {
                if msg.contains('\n') {
                    self.info_popup = Some(msg);
                } else {
                    self.status_message = Some(msg);
                }
            }
            ExEffect::Error(msg) => {
                self.status_message = Some(format!("E: {msg}"));
            }
            ExEffect::Unknown(c) => {
                self.status_message = Some(format!("E492: Not an editor command: :{c}"));
            }
        }
    }

    /// Format a one-line summary of the active clipboard backend for the
    /// status line. Used by `:clipboard`.
    fn clipboard_status(&self) -> String {
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
        let idx = self.active;
        self.save_slot(idx, path)
    }

    /// Write slot `idx`'s buffer to `path` (or the slot's own filename if
    /// `path` is `None`). Updates `status_message` on success or failure.
    /// Does NOT change `self.active`. Returns `true` on success.
    fn save_slot(&mut self, idx: usize, path: Option<PathBuf>) -> bool {
        if self.slots[idx].editor.is_readonly() {
            self.status_message = Some("E45: 'readonly' option is set (add ! to override)".into());
            return false;
        }
        let target = path.or_else(|| self.slots[idx].filename.clone());
        match target {
            None => {
                self.status_message = Some("E32: No file name".into());
                false
            }
            Some(p) => {
                let lines = self.slots[idx].editor.buffer().lines();
                let content = if lines.is_empty() {
                    String::new()
                } else {
                    let mut s = lines.join("\n");
                    s.push('\n');
                    s
                };
                match std::fs::write(&p, &content) {
                    Ok(()) => {
                        let line_count = lines.len();
                        let byte_count = content.len();
                        self.status_message = Some(format!(
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
                        self.slots[idx].filename = Some(p);
                        self.slots[idx].is_new_file = false;
                        self.slots[idx].snapshot_saved();
                        if idx == self.active {
                            self.refresh_git_signs_force();
                        }
                        true
                    }
                    Err(e) => {
                        self.status_message = Some(format!("E: {}: {e}", p.display()));
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
        self.status_message = Some(format!("{written} buffer(s) written, {skipped} skipped"));
    }

    /// `:qa[!]` — quit all. Blocks when any slot is dirty unless `force`.
    fn quit_all(&mut self, force: bool) {
        if !force && let Some(idx) = self.slots.iter().position(|s| s.dirty) {
            let name = self.slots[idx]
                .filename
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "[No Name]".into());
            self.status_message = Some(format!(
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
                    self.status_message = Some("E499: Empty file name for '%'".into());
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
            if idx == self.active {
                self.reload_current(force);
                return;
            }
            self.switch_to(idx);
            self.status_message = Some(format!(
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
        let prev_idx = self.active;
        let prev_pristine = {
            let s = &self.slots[prev_idx];
            s.filename.is_none() && !s.dirty
        };
        match self.open_new_slot(path) {
            Ok(idx) => {
                // Track alt-buffer before switching.
                self.prev_active = Some(self.active);
                self.active = idx;
                let line_count = self.active().editor.buffer().line_count() as usize;
                let path_display = self
                    .active()
                    .filename
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                self.status_message = Some(format!("\"{path_display}\" {line_count}L"));
                self.refresh_git_signs_force();

                // Drop the pristine default buffer once a real file is open.
                if prev_pristine && self.slots.len() > 1 {
                    let removed = self.slots.remove(prev_idx);
                    self.syntax.forget(removed.buffer_id);
                    if prev_idx < self.active {
                        self.active -= 1;
                    }
                    self.prev_active = None;
                }
            }
            Err(msg) => {
                self.status_message = Some(msg);
            }
        }
    }

    /// Reload the active slot from disk (`:e` no-arg / `:e %`).
    fn reload_current(&mut self, force: bool) {
        let path = match self.active().filename.clone() {
            Some(p) => p,
            None => {
                self.status_message = Some("E32: No file name".into());
                return;
            }
        };
        if !force && self.active().dirty {
            self.status_message =
                Some("E37: No write since last change (add ! to override)".into());
            return;
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                self.status_message =
                    Some(format!("E484: Can't open file {}: {e}", path.display()));
                return;
            }
        };
        let trimmed = content.strip_suffix('\n').unwrap_or(&content);
        let line_count = trimmed.lines().count();
        let byte_count = content.len();
        self.active_mut().editor.set_content(trimmed);
        self.active_mut().editor.goto_line(1);
        {
            let vp = self.active_mut().editor.host_mut().viewport_mut();
            vp.top_row = 0;
            vp.top_col = 0;
        }
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
        self.active_mut().last_recompute_key = None;
        self.active_mut()
            .editor
            .install_ratatui_syntax_spans(Vec::new());
        let (vp_top, vp_height) = {
            let vp = self.active().editor.host().viewport();
            (vp.top_row, vp.height as usize)
        };
        if let Some(out) =
            self.syntax
                .preview_render(buffer_id, self.active().editor.buffer(), vp_top, vp_height)
        {
            self.active_mut()
                .editor
                .install_ratatui_syntax_spans(out.spans);
        }
        self.recompute_and_install();
        self.active_mut().snapshot_saved();
        self.refresh_git_signs_force();
        self.status_message = Some(format!(
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
                    // File changed. If dirty — warn once; don't reload.
                    if self.slots[idx].dirty {
                        let prev = self.slots[idx].disk_state;
                        self.slots[idx].disk_state = DiskState::ChangedOnDisk;
                        if prev != DiskState::ChangedOnDisk {
                            messages.push(format!(
                                "W: \"{}\" changed on disk (buffer is dirty, use :e! to reload)",
                                path.display()
                            ));
                        }
                    } else {
                        // Clean buffer — reload automatically.
                        let content = match std::fs::read_to_string(&path) {
                            Ok(c) => c,
                            Err(_) => continue,
                        };
                        let trimmed = content.strip_suffix('\n').unwrap_or(&content);
                        // Preserve cursor position clamped to new line count.
                        let (cur_row, cur_col) = self.slots[idx].editor.cursor();
                        self.slots[idx].editor.set_content(trimmed);
                        let new_line_count = self.slots[idx].editor.buffer().line_count() as usize;
                        let clamped_row = cur_row.min(new_line_count.saturating_sub(1));
                        self.slots[idx].editor.goto_line(clamped_row + 1);
                        let _ = cur_col; // column is reset by goto_line
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
                        self.slots[idx].last_recompute_key = None;
                        if idx == self.active {
                            self.slots[idx]
                                .editor
                                .install_ratatui_syntax_spans(Vec::new());
                            let (vp_top, vp_height) = {
                                let vp = self.slots[idx].editor.host().viewport();
                                (vp.top_row, vp.height as usize)
                            };
                            if let Some(out) = self.syntax.preview_render(
                                buffer_id,
                                self.slots[idx].editor.buffer(),
                                vp_top,
                                vp_height,
                            ) {
                                self.slots[idx]
                                    .editor
                                    .install_ratatui_syntax_spans(out.spans);
                            }
                            self.recompute_and_install();
                            self.refresh_git_signs_force();
                        }
                        messages.push(format!("\"{}\" reloaded from disk", path.display()));
                    }
                }
            }
        }
        if !messages.is_empty() {
            self.status_message = Some(messages.join(" | "));
        }
    }
}
