use hjkl_bonsai::DotFallbackTheme;
use hjkl_editor::runtime::ex::{self, ExEffect};
use hjkl_engine::{Host, Query};
use std::path::PathBuf;
use std::sync::Arc;

use super::{App, DiskState};

/// Parse a resize argument string into a line/column delta.
///
/// Accepts:
/// - `"+N"` → `+N` (relative grow)
/// - `"-N"` → `-N` (relative shrink)
/// - `"N"` → treated as a relative delta for simplicity
fn parse_resize_arg(arg: &str) -> Option<i32> {
    let arg = arg.trim();
    if arg.is_empty() {
        return None;
    }
    if let Some(rest) = arg.strip_prefix('+') {
        rest.trim().parse::<i32>().ok()
    } else if let Some(rest) = arg.strip_prefix('-') {
        rest.trim().parse::<i32>().ok().map(|n| -n)
    } else {
        arg.parse::<i32>().ok()
    }
}

impl App {
    /// Execute an ex command string (without the leading `:`).
    pub(crate) fn dispatch_ex(&mut self, cmd: &str) {
        let canon = ex::canonical_command_name(cmd);
        let cmd: &str = canon.as_ref();

        // App-level `:set mouse` / `:set nomouse` / `:set mouse!` / `:set mouse?`.
        // Mouse capture is a terminal-I/O concern, not an editor-engine
        // setting, so the app intercepts these tokens here. Residual
        // tokens (if any) flow through to the engine as a rebuilt
        // `:set ...` line so combined forms like `:set nu nomouse` work.
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
                            consumed_any = true;
                        }
                        "nomouse" => {
                            self.set_mouse_capture(false);
                            consumed_any = true;
                        }
                        "mouse!" => {
                            self.set_mouse_capture(!self.mouse_enabled);
                            consumed_any = true;
                        }
                        "mouse?" => {
                            self.status_message = Some(
                                if self.mouse_enabled {
                                    "mouse"
                                } else {
                                    "nomouse"
                                }
                                .into(),
                            );
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

        // `:sp[lit] [file]` — horizontal split.
        if cmd == "split" || cmd == "sp" || cmd.starts_with("split ") || cmd.starts_with("sp ") {
            let arg = if let Some(rest) = cmd.strip_prefix("split ") {
                rest.trim()
            } else if let Some(rest) = cmd.strip_prefix("sp ") {
                rest.trim()
            } else {
                ""
            };
            self.do_split(arg);
            return;
        }

        // `:vsp[lit] [file]` — vertical split.
        if cmd == "vsplit" || cmd == "vsp" || cmd.starts_with("vsplit ") || cmd.starts_with("vsp ")
        {
            let arg = if let Some(rest) = cmd.strip_prefix("vsplit ") {
                rest.trim()
            } else if let Some(rest) = cmd.strip_prefix("vsp ") {
                rest.trim()
            } else {
                ""
            };
            self.do_vsplit(arg);
            return;
        }

        // `:vnew` — vertical split with a fresh empty unnamed buffer.
        if cmd == "vnew" {
            self.do_vnew();
            return;
        }

        // `:close` / `:clo` — close the focused window.
        if cmd == "close" || cmd == "clo" {
            self.close_focused_window();
            return;
        }

        // `:only` / `:on` — close all windows except the focused one.
        if cmd == "only" || cmd == "on" {
            self.only_focused_window();
            return;
        }

        // `:new` — horizontal split with a fresh empty unnamed buffer.
        if cmd == "new" {
            self.do_new();
            return;
        }

        // ─── Tab commands ────────────────────────────────────────────────────

        // `:tabnew [file]` / `:tabedit [file]` / `:tabe [file]`
        if cmd == "tabnew"
            || cmd.starts_with("tabnew ")
            || cmd == "tabedit"
            || cmd.starts_with("tabedit ")
            || cmd == "tabe"
            || cmd.starts_with("tabe ")
        {
            let arg = if let Some(rest) = cmd.strip_prefix("tabnew ") {
                rest.trim()
            } else if let Some(rest) = cmd.strip_prefix("tabedit ") {
                rest.trim()
            } else if let Some(rest) = cmd.strip_prefix("tabe ") {
                rest.trim()
            } else {
                ""
            };
            self.do_tabnew(arg);
            return;
        }

        // `:tabnext` / `:tabn`
        if cmd == "tabnext" || cmd == "tabn" {
            self.do_tabnext();
            return;
        }

        // `:tabprev` / `:tabp` / `:tabN` (uppercase N = previous in vim)
        if cmd == "tabprev" || cmd == "tabp" || cmd == "tabN" {
            self.do_tabprev();
            return;
        }

        // `:tabclose` / `:tabc`
        if cmd == "tabclose" || cmd == "tabc" {
            self.do_tabclose();
            return;
        }

        // `:tabfirst` / `:tabrewind` / `:tabr` — jump to the first tab.
        if cmd == "tabfirst" || cmd == "tabrewind" || cmd == "tabr" {
            self.do_tabfirst();
            return;
        }

        // `:tablast` — jump to the last tab.
        if cmd == "tablast" {
            self.do_tablast();
            return;
        }

        // `:tabonly` / `:tabo` — close all tabs except the current one.
        if cmd == "tabonly" || cmd == "tabo" {
            self.do_tabonly();
            return;
        }

        // `:tabmove [N|+N|-N]` — reorder tabs.
        if cmd == "tabmove" || cmd.starts_with("tabmove ") {
            let arg = cmd.strip_prefix("tabmove ").map(str::trim).unwrap_or("");
            self.do_tabmove(arg);
            return;
        }

        // `:tabs` — show info popup listing all tabs.
        if cmd == "tabs" {
            self.do_tabs();
            return;
        }

        // `:resize [+|-]N` — adjust focused window height.
        // `:vertical resize [+|-]N` / `:vert res [+|-]N` — adjust width.
        let (is_resize, is_vertical) = if cmd == "resize" || cmd.starts_with("resize ") {
            (true, false)
        } else if cmd.starts_with("vertical resize ")
            || cmd.starts_with("vert res ")
            || cmd.starts_with("vert resize ")
        {
            (true, true)
        } else {
            (false, false)
        };
        if is_resize {
            let arg = cmd
                .trim_start_matches("vertical")
                .trim_start_matches("vert")
                .trim_start_matches("resize")
                .trim_start_matches("res")
                .trim();
            if let Some(delta) = parse_resize_arg(arg) {
                if is_vertical {
                    self.resize_width(delta);
                } else {
                    self.resize_height(delta);
                }
                self.status_message = Some("resize".into());
            } else {
                self.status_message = Some("E: invalid resize argument".into());
            }
            return;
        }

        // ── LSP diagnostic commands ───────────────────────────────────────────
        // `:Rename <newname>` — LSP rename.
        if cmd.starts_with("Rename ") || cmd == "Rename" {
            if let Some(new_name) = cmd.strip_prefix("Rename ") {
                let new_name = new_name.trim().to_string();
                if new_name.is_empty() {
                    self.status_message = Some("E: usage: :Rename <newname>".into());
                } else {
                    self.lsp_rename(new_name);
                }
            } else {
                // TODO: open prompt-based rename UI (Phase 6).
                self.status_message = Some("E: usage: :Rename <newname>".into());
            }
            return;
        }

        // `:LspFormat` / `:Format` — LSP format.
        if cmd == "LspFormat" || cmd == "Format" {
            // TODO: range formatting when invoked from visual mode (Phase 6).
            self.lsp_format();
            return;
        }

        // `:LspCodeAction` — LSP code actions.
        if cmd == "LspCodeAction" || cmd == "CodeAction" {
            self.lsp_code_actions();
            return;
        }

        match cmd {
            "lopen" => {
                self.open_diag_picker();
                return;
            }
            "lnext" => {
                self.lnext_severity(None);
                return;
            }
            "lprev" => {
                self.lprev_severity(None);
                return;
            }
            "lfirst" => {
                self.ldiag_first();
                return;
            }
            "llast" => {
                self.ldiag_last();
                return;
            }
            "LspInfo" => {
                self.show_lsp_info();
                return;
            }
            _ => {}
        }

        let active_slot = self.focused_slot_idx();
        match ex::run(&mut self.slots[active_slot].editor, cmd) {
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

    /// `:sp [file]` / `:split [file]` — open a horizontal split.
    ///
    /// With no argument: duplicates the current window (same slot, same
    /// scroll).  With a filename: opens a new slot in the upper half.
    fn do_split(&mut self, arg: &str) {
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

        let new_slot = if arg.is_empty() {
            // Duplicate — same slot.
            cur_slot
        } else {
            match self.open_new_slot(std::path::PathBuf::from(arg)) {
                Ok(idx) => idx,
                Err(msg) => {
                    self.status_message = Some(msg);
                    return;
                }
            }
        };

        let new_win_id = self.next_window_id;
        self.next_window_id += 1;
        self.windows.push(Some(Window {
            slot: new_slot,
            top_row,
            top_col,
            last_rect: None,
        }));
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
        self.status_message = Some("split".into());
    }

    /// `:vsp [file]` / `:vsplit [file]` — open a vertical split.
    ///
    /// With no argument: duplicates the current window (same slot, same
    /// scroll).  With a filename: opens a new slot in the left half.
    /// New window goes on the left (vim convention).
    fn do_vsplit(&mut self, arg: &str) {
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

        let new_slot = if arg.is_empty() {
            // Duplicate — same slot.
            cur_slot
        } else {
            match self.open_new_slot(std::path::PathBuf::from(arg)) {
                Ok(idx) => idx,
                Err(msg) => {
                    self.status_message = Some(msg);
                    return;
                }
            }
        };

        let new_win_id = self.next_window_id;
        self.next_window_id += 1;
        self.windows.push(Some(Window {
            slot: new_slot,
            top_row,
            top_col,
            last_rect: None,
        }));
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
        self.status_message = Some("vsplit".into());
    }

    /// `:vnew` — open a vertical split with a fresh empty unnamed buffer.
    fn do_vnew(&mut self) {
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
                last_recompute_at: std::time::Instant::now() - std::time::Duration::from_secs(1),
                last_recompute_key: None,
                saved_hash: 0,
                saved_len: 0,
                disk_mtime: None,
                disk_len: None,
                disk_state: super::DiskState::Synced,
            };
            slot.snapshot_saved();
            self.slots.push(slot);
            self.slots.len() - 1
        };

        let new_win_id = self.next_window_id;
        self.next_window_id += 1;
        self.windows.push(Some(Window {
            slot: new_slot_idx,
            top_row,
            top_col,
            last_rect: None,
        }));
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
        self.status_message = Some("vnew".into());
    }

    /// `:new` — open a horizontal split with a fresh empty unnamed buffer.
    ///
    /// New window appears on top (a), existing window stays below (b).
    fn do_new(&mut self) {
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
                last_recompute_at: std::time::Instant::now() - std::time::Duration::from_secs(1),
                last_recompute_key: None,
                saved_hash: 0,
                saved_len: 0,
                disk_mtime: None,
                disk_len: None,
                disk_state: super::DiskState::Synced,
            };
            slot.snapshot_saved();
            self.slots.push(slot);
            self.slots.len() - 1
        };

        let new_win_id = self.next_window_id;
        self.next_window_id += 1;
        self.windows.push(Some(Window {
            slot: new_slot_idx,
            top_row,
            top_col,
            last_rect: None,
        }));
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
        self.status_message = Some("new".into());
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
        let idx = self.focused_slot_idx();
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
                // Create parent dir(s) if missing so writing into a fresh
                // path like ~/.config/hjkl/config.toml works first try.
                if let Some(parent) = p.parent()
                    && !parent.as_os_str().is_empty()
                    && !parent.exists()
                    && let Err(e) = std::fs::create_dir_all(parent)
                {
                    self.status_message = Some(format!("E: {}: {e}", parent.display()));
                    return false;
                }
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
                        if idx == self.focused_slot_idx() {
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
            if idx == self.focused_slot_idx() {
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
                        if idx == self.focused_slot_idx() {
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

    // ─── Phase 2 Tab helpers ──────────────────────────────────────────────────

    /// `:tabfirst` / `:tabrewind` / `:tabr` — jump to the first tab.
    fn do_tabfirst(&mut self) {
        if self.active_tab == 0 {
            let m = self.tabs.len();
            self.status_message = Some(format!("tab 1/{m}"));
            return;
        }
        self.sync_viewport_from_editor();
        self.active_tab = 0;
        self.sync_viewport_to_editor();
        let m = self.tabs.len();
        self.status_message = Some(format!("tab 1/{m}"));
    }

    /// `:tablast` — jump to the last tab.
    fn do_tablast(&mut self) {
        let last = self.tabs.len() - 1;
        if self.active_tab == last {
            let m = self.tabs.len();
            self.status_message = Some(format!("tab {m}/{m}"));
            return;
        }
        self.sync_viewport_from_editor();
        self.active_tab = last;
        self.sync_viewport_to_editor();
        let m = self.tabs.len();
        self.status_message = Some(format!("tab {m}/{m}"));
    }

    /// `:tabonly` / `:tabo` — close all tabs except the current one.
    fn do_tabonly(&mut self) {
        if self.tabs.len() <= 1 {
            self.status_message = Some("tabonly".into());
            return;
        }
        self.sync_viewport_from_editor();

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

        self.sync_viewport_to_editor();
        self.status_message = Some("tabonly".into());
    }

    /// `:tabmove [N|+N|-N]` — reorder tabs.
    ///
    /// No arg → move to end. `N` → absolute 0-based position. `+N`/`-N` →
    /// relative. Out-of-range positions are clamped silently.
    fn do_tabmove(&mut self, arg: &str) {
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
            self.status_message = Some("tabmove".into());
            return;
        }

        self.sync_viewport_from_editor();
        let tab = self.tabs.remove(self.active_tab);
        self.tabs.insert(target, tab);
        self.active_tab = target;
        self.sync_viewport_to_editor();
        self.status_message = Some("tabmove".into());
    }

    /// `:tabs` — show an info popup listing all tabs with their active buffer
    /// name. The `>` marker indicates the active tab.
    fn do_tabs(&mut self) {
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
        self.info_popup = Some(lines.join("\n"));
    }

    // ─── Tab helpers ─────────────────────────────────────────────────────────

    /// `:tabnew [file]` / `:tabedit [file]` / `:tabe [file]`
    ///
    /// Open a new tab. With a file argument: load the file into a new slot.
    /// Without: open an empty unnamed buffer. The new tab gets its own layout
    /// and focused window; windows and slots are shared globally.
    fn do_tabnew(&mut self, arg: &str) {
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
                last_recompute_at: std::time::Instant::now() - std::time::Duration::from_secs(1),
                last_recompute_key: None,
                saved_hash: 0,
                saved_len: 0,
                disk_mtime: None,
                disk_len: None,
                disk_state: super::DiskState::Synced,
            };
            slot.snapshot_saved();
            self.slots.push(slot);
            self.slots.len() - 1
        } else {
            match self.open_new_slot(std::path::PathBuf::from(arg)) {
                Ok(idx) => idx,
                Err(msg) => {
                    self.status_message = Some(msg);
                    return;
                }
            }
        };

        // Allocate a new window for the new tab.
        let new_win_id = self.next_window_id;
        self.next_window_id += 1;
        self.windows.push(Some(Window {
            slot: new_slot_idx,
            top_row: 0,
            top_col: 0,
            last_rect: None,
        }));

        // Push the new tab and switch to it.
        self.tabs.push(Tab {
            layout: LayoutTree::Leaf(new_win_id),
            focused_window: new_win_id,
        });
        self.active_tab = self.tabs.len() - 1;

        // Sync viewport for the new tab's editor.
        self.sync_viewport_to_editor();
        self.status_message = Some("tabnew".into());
    }

    /// `:tabnext` / `:tabn` — cycle to the next tab (wraps).
    fn do_tabnext(&mut self) {
        if self.tabs.len() <= 1 {
            self.status_message = Some("only one tab".into());
            return;
        }
        self.sync_viewport_from_editor();
        self.active_tab = (self.active_tab + 1) % self.tabs.len();
        self.sync_viewport_to_editor();
        let n = self.active_tab + 1;
        let m = self.tabs.len();
        self.status_message = Some(format!("tab {n}/{m}"));
    }

    /// `:tabprev` / `:tabp` / `:tabN` — cycle to the previous tab (wraps).
    fn do_tabprev(&mut self) {
        if self.tabs.len() <= 1 {
            self.status_message = Some("only one tab".into());
            return;
        }
        self.sync_viewport_from_editor();
        self.active_tab = (self.active_tab + self.tabs.len() - 1) % self.tabs.len();
        self.sync_viewport_to_editor();
        let n = self.active_tab + 1;
        let m = self.tabs.len();
        self.status_message = Some(format!("tab {n}/{m}"));
    }

    /// `:tabclose` / `:tabc` — close the current tab.
    ///
    /// Refuses when only one tab remains (E444). On success, drops all windows
    /// that belonged exclusively to this tab and adjusts `active_tab`.
    fn do_tabclose(&mut self) {
        if self.tabs.len() <= 1 {
            self.status_message = Some("E444: Cannot close last tab".into());
            return;
        }
        self.sync_viewport_from_editor();

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

        self.sync_viewport_to_editor();
        let n = self.active_tab + 1;
        let m = self.tabs.len();
        self.status_message = Some(format!("tab {n}/{m}"));
    }

    // ── LSP diagnostic navigation ─────────────────────────────────────────────

    /// `:lopen` — open a picker listing all LSP diagnostics for the active buffer.
    pub(crate) fn open_diag_picker(&mut self) {
        let diags = self.active().lsp_diags.clone();
        if diags.is_empty() {
            self.status_message = Some("no diagnostics".into());
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
            self.status_message = Some("no diagnostics".into());
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
            self.status_message = Some(format!("[{}] {}", sev_label(d.severity), msg));
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
            self.status_message = Some("no diagnostics".into());
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
            self.status_message = Some(format!("[{}] {}", sev_label(d.severity), msg));
        }
    }

    /// `:lfirst` — jump to the first diagnostic.
    pub(crate) fn ldiag_first(&mut self) {
        let target = self.active().lsp_diags.first().cloned();
        match target {
            None => {
                self.status_message = Some("no diagnostics".into());
            }
            Some(d) => {
                self.active_mut()
                    .editor
                    .jump_cursor(d.start_row, d.start_col);
                self.active_mut().editor.ensure_cursor_in_scrolloff();
                self.sync_viewport_from_editor();
                let msg = d.message.lines().next().unwrap_or("").to_string();
                self.status_message = Some(format!("[{}] {}", sev_label(d.severity), msg));
            }
        }
    }

    /// `:llast` — jump to the last diagnostic.
    pub(crate) fn ldiag_last(&mut self) {
        let target = self.active().lsp_diags.last().cloned();
        match target {
            None => {
                self.status_message = Some("no diagnostics".into());
            }
            Some(d) => {
                self.active_mut()
                    .editor
                    .jump_cursor(d.start_row, d.start_col);
                self.active_mut().editor.ensure_cursor_in_scrolloff();
                self.sync_viewport_from_editor();
                let msg = d.message.lines().next().unwrap_or("").to_string();
                self.status_message = Some(format!("[{}] {}", sev_label(d.severity), msg));
            }
        }
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
            self.status_message = Some("no diagnostics at cursor".into());
            return;
        }

        let text = hits
            .iter()
            .map(|d| format!("[{}] {}", sev_label(d.severity), d.message))
            .collect::<Vec<_>>()
            .join("\n---\n");
        self.info_popup = Some(text);
    }

    /// `:LspInfo` — show running LSP servers + diagnostic info about the
    /// active buffer's attach state. Designed to surface the most common
    /// causes of "why isn't LSP working".
    pub(crate) fn show_lsp_info(&mut self) {
        let mut lines = Vec::new();

        // Top: enabled / disabled state.
        if self.lsp.is_none() {
            lines.push("LSP: disabled (set [lsp] enabled = true in config)".into());
            self.info_popup = Some(lines.join("\n"));
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

        self.info_popup = Some(lines.join("\n"));
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
