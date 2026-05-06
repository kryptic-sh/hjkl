//! LSP glue — bridges `App` state with `hjkl_lsp::LspManager`.

use std::path::PathBuf;

use ratatui::style::{Color, Modifier, Style};
use serde_json::json;

use crate::completion::{Completion, item_from_lsp};

use super::{App, DiagSeverity, LspDiag, LspPendingRequest, LspServerInfo};

/// Resolve a (possibly relative) buffer path against `current_dir` so the
/// resulting `PathBuf` is absolute. `url::Url::from_file_path` (used by
/// `hjkl_lsp::uri::from_path`) requires an absolute path; without this
/// helper, opening hjkl with a relative path like `apps/hjkl/src/main.rs`
/// silently fails to attach to the LSP server.
fn absolutize(p: &std::path::Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .ok()
            .map(|cwd| cwd.join(p))
            .unwrap_or_else(|| p.to_path_buf())
    }
}

/// Small inline map: file extension → LSP language id.
pub(super) fn language_id_for_ext(ext: &str) -> Option<&'static str> {
    match ext {
        "rs" => Some("rust"),
        "ts" | "tsx" => Some("typescript"),
        "js" | "jsx" => Some("javascript"),
        "py" => Some("python"),
        "go" => Some("go"),
        "c" | "h" => Some("c"),
        "cpp" | "cc" | "cxx" | "hpp" => Some("cpp"),
        "lua" => Some("lua"),
        "toml" => Some("toml"),
        "json" => Some("json"),
        "md" => Some("markdown"),
        _ => None,
    }
}

/// Convert a `lsp_types::DiagnosticSeverity` to our `DiagSeverity`.
fn convert_severity(s: Option<lsp_types::DiagnosticSeverity>) -> DiagSeverity {
    match s {
        Some(lsp_types::DiagnosticSeverity::ERROR) => DiagSeverity::Error,
        Some(lsp_types::DiagnosticSeverity::WARNING) => DiagSeverity::Warning,
        Some(lsp_types::DiagnosticSeverity::INFORMATION) => DiagSeverity::Info,
        Some(lsp_types::DiagnosticSeverity::HINT) => DiagSeverity::Hint,
        _ => DiagSeverity::Error, // default unknown to Error
    }
}

/// Style for a gutter sign by severity.
fn severity_sign(sev: DiagSeverity) -> (char, Style) {
    match sev {
        DiagSeverity::Error => (
            'E',
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        DiagSeverity::Warning => (
            'W',
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        DiagSeverity::Info => ('I', Style::default().fg(Color::Blue)),
        DiagSeverity::Hint => ('H', Style::default().fg(Color::Cyan)),
    }
}

/// Priority for gutter signs by severity (lower number = higher priority).
fn severity_priority(sev: DiagSeverity) -> u8 {
    match sev {
        DiagSeverity::Error => 100,
        DiagSeverity::Warning => 80,
        DiagSeverity::Info => 60,
        DiagSeverity::Hint => 40,
    }
}

impl App {
    /// Drain all pending LSP events and dispatch them.
    /// Called at the top of every event-loop iteration.
    pub fn drain_lsp_events(&mut self) {
        // Collect events first to avoid holding the borrow on self.lsp
        // while we mutate self in handlers.
        let events: Vec<hjkl_lsp::LspEvent> = if let Some(ref mgr) = self.lsp {
            let mut v = Vec::new();
            while let Some(evt) = mgr.try_recv_event() {
                v.push(evt);
            }
            v
        } else {
            return;
        };

        for evt in events {
            match evt {
                hjkl_lsp::LspEvent::ServerInitialized { key, capabilities } => {
                    tracing::info!(?key, "lsp server initialized");
                    self.lsp_state.insert(
                        key,
                        LspServerInfo {
                            initialized: true,
                            capabilities,
                        },
                    );
                }
                hjkl_lsp::LspEvent::ServerExited { key, status } => {
                    tracing::warn!(?key, ?status, "lsp server exited");
                    self.lsp_state.remove(&key);
                }
                hjkl_lsp::LspEvent::Notification {
                    key,
                    method,
                    params,
                } => {
                    tracing::debug!(?key, method, "lsp notification");
                    if method == "textDocument/publishDiagnostics" {
                        self.handle_publish_diagnostics(params);
                    }
                }
                hjkl_lsp::LspEvent::Response { request_id, result } => {
                    tracing::debug!(request_id, "lsp response");
                    if let Some(pending) = self.lsp_pending.remove(&request_id) {
                        self.handle_lsp_response(pending, result);
                    }
                }
            }
        }
    }

    /// Handle a `textDocument/publishDiagnostics` notification.
    pub(crate) fn handle_publish_diagnostics(&mut self, params: serde_json::Value) {
        let parsed: lsp_types::PublishDiagnosticsParams = match serde_json::from_value(params) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("publishDiagnostics: failed to parse: {e}");
                return;
            }
        };

        // Convert the LSP URI to a PathBuf for slot matching.
        // lsp_types::Uri is a newtype around url::Url.
        let uri_url: url::Url = match url::Url::parse(parsed.uri.as_str()) {
            Ok(u) => u,
            Err(e) => {
                tracing::warn!("publishDiagnostics: bad URI: {e}");
                return;
            }
        };
        let uri_path = match hjkl_lsp::uri::to_path(&uri_url) {
            Some(p) => p,
            None => {
                tracing::debug!("publishDiagnostics: non-file URI, skipping");
                return;
            }
        };

        // Find the slot whose filename matches the URI path.
        let slot_idx = self.slots.iter().position(|s| {
            s.filename
                .as_ref()
                .map(|p| {
                    let abs = if p.is_absolute() {
                        p.clone()
                    } else {
                        std::env::current_dir().unwrap_or_default().join(p)
                    };
                    abs == uri_path
                })
                .unwrap_or(false)
        });

        let slot_idx = match slot_idx {
            Some(i) => i,
            None => {
                tracing::debug!("publishDiagnostics: no matching slot for {:?}", uri_path);
                return;
            }
        };

        // Convert LSP diagnostics to our internal format.
        let mut lsp_diags: Vec<LspDiag> = Vec::new();
        let mut sign_map: std::collections::HashMap<usize, (DiagSeverity, char, Style, u8)> =
            std::collections::HashMap::new();

        for d in &parsed.diagnostics {
            let start_row = d.range.start.line as usize;
            let start_col = d.range.start.character as usize;
            let end_row = d.range.end.line as usize;
            let end_col = d.range.end.character as usize;
            let severity = convert_severity(d.severity);
            let code = d.code.as_ref().map(|c| match c {
                lsp_types::NumberOrString::Number(n) => n.to_string(),
                lsp_types::NumberOrString::String(s) => s.clone(),
            });
            let source = d.source.clone();

            lsp_diags.push(LspDiag {
                start_row,
                start_col,
                end_row,
                end_col,
                severity,
                message: d.message.clone(),
                source,
                code,
            });

            // For the gutter sign: highest-priority severity per row wins.
            let prio = severity_priority(severity);
            let entry = sign_map
                .entry(start_row)
                .or_insert((severity, 'E', Style::default(), 0));
            if prio > entry.3 {
                let (ch, style) = severity_sign(severity);
                *entry = (severity, ch, style, prio);
            }
        }

        // Build gutter signs from the map.
        let diag_signs_lsp: Vec<hjkl_buffer::Sign> = sign_map
            .into_iter()
            .map(|(row, (_, ch, style, priority))| hjkl_buffer::Sign {
                row,
                ch,
                style,
                priority,
            })
            .collect();

        let slot = &mut self.slots[slot_idx];
        slot.lsp_diags = lsp_diags;
        slot.diag_signs_lsp = diag_signs_lsp;
    }

    /// Send a `textDocument/didChange` notification for the active buffer,
    /// but only when the buffer's `dirty_gen` has advanced since the last
    /// send. This naturally batches rapid keystroke edits.
    pub(crate) fn lsp_notify_change_active(&mut self) {
        let mgr = match self.lsp.as_ref() {
            Some(m) => m,
            None => return,
        };

        let slot_idx = self.focused_slot_idx();
        let slot = &mut self.slots[slot_idx];
        let dg = slot.editor.buffer().dirty_gen();

        // Skip if dirty_gen unchanged since last send.
        if slot.last_lsp_dirty_gen == Some(dg) {
            return;
        }
        slot.last_lsp_dirty_gen = Some(dg);

        let buffer_id = slot.buffer_id as hjkl_lsp::BufferId;
        let text = slot.editor.buffer().lines().to_vec().join("\n");
        mgr.notify_change(buffer_id, &text);
    }

    /// Attach `slot_idx` to the appropriate language server (if configured).
    pub(crate) fn lsp_attach_buffer(&mut self, slot_idx: usize) {
        let mgr = match self.lsp.as_ref() {
            Some(m) => m,
            None => return,
        };

        let slot = &self.slots[slot_idx];
        let path = match slot.filename.as_ref() {
            Some(p) => absolutize(p),
            None => return,
        };

        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let language_id = match language_id_for_ext(ext) {
            Some(id) => id,
            None => return,
        };

        // Only attach if there's a configured server for this language.
        if !self.config.lsp.servers.contains_key(language_id) {
            return;
        }

        let text = self.slots[slot_idx]
            .editor
            .buffer()
            .lines()
            .to_vec()
            .join("\n");

        let buffer_id = self.slots[slot_idx].buffer_id as hjkl_lsp::BufferId;
        mgr.attach_buffer(buffer_id, &path, language_id, &text);
    }

    /// Close the LSP document for `slot_idx`.
    pub(crate) fn lsp_detach_buffer(&mut self, slot_idx: usize) {
        let mgr = match self.lsp.as_ref() {
            Some(m) => m,
            None => return,
        };
        let buffer_id = self.slots[slot_idx].buffer_id as hjkl_lsp::BufferId;
        mgr.detach_buffer(buffer_id);
    }

    /// Allocate a fresh monotonic request id for an outgoing LSP request.
    pub(crate) fn lsp_alloc_request_id(&mut self) -> i64 {
        let id = self.lsp_next_request_id;
        self.lsp_next_request_id += 1;
        id
    }

    // ── Request helpers ───────────────────────────────────────────────────

    /// Build `TextDocumentPositionParams` JSON for the current cursor position.
    fn lsp_position_params(
        &self,
    ) -> Option<(serde_json::Value, hjkl_lsp::BufferId, (usize, usize))> {
        let slot = self.active();
        let path = absolutize(slot.filename.as_ref()?);
        let uri = hjkl_lsp::uri::from_path(&path).ok()?;
        let cursor = slot.editor.buffer().cursor();
        let row = cursor.row;
        let col = cursor.col;
        let params = json!({
            "textDocument": { "uri": uri.as_str() },
            "position": { "line": row as u32, "character": col as u32 },
        });
        let buffer_id = slot.buffer_id as hjkl_lsp::BufferId;
        Some((params, buffer_id, (row, col)))
    }

    /// Internal: send a goto-flavour request and register it as pending.
    fn lsp_send_goto(
        &mut self,
        method: &str,
        make_pending: impl FnOnce(hjkl_lsp::BufferId, (usize, usize)) -> LspPendingRequest,
    ) {
        if self.lsp.is_none() {
            self.status_message =
                Some("LSP: not enabled (set [lsp] enabled = true in config)".into());
            return;
        }
        let (params, buffer_id, origin) = match self.lsp_position_params() {
            Some(v) => v,
            None => {
                self.status_message = Some(
                    "LSP: no file open in this buffer (use :e <file> or open from the picker)"
                        .into(),
                );
                return;
            }
        };
        let request_id = self.lsp_alloc_request_id();
        let pending = make_pending(buffer_id, origin);
        self.lsp_pending.insert(request_id, pending);
        // Reborrow after mutable ops are done.
        if let Some(mgr) = self.lsp.as_ref() {
            mgr.send_request(request_id, buffer_id, method, params);
        }
    }

    // ── Public goto / hover entry points ─────────────────────────────────

    /// `gd` — goto definition.
    pub(crate) fn lsp_goto_definition(&mut self) {
        self.lsp_send_goto("textDocument/definition", |buf, orig| {
            LspPendingRequest::GotoDefinition {
                buffer_id: buf,
                origin: orig,
            }
        });
    }

    /// `gD` — goto declaration.
    pub(crate) fn lsp_goto_declaration(&mut self) {
        self.lsp_send_goto("textDocument/declaration", |buf, orig| {
            LspPendingRequest::GotoDeclaration {
                buffer_id: buf,
                origin: orig,
            }
        });
    }

    /// `gy` — goto type definition.
    pub(crate) fn lsp_goto_type_definition(&mut self) {
        self.lsp_send_goto("textDocument/typeDefinition", |buf, orig| {
            LspPendingRequest::GotoTypeDefinition {
                buffer_id: buf,
                origin: orig,
            }
        });
    }

    /// `gi` — goto implementation.
    pub(crate) fn lsp_goto_implementation(&mut self) {
        self.lsp_send_goto("textDocument/implementation", |buf, orig| {
            LspPendingRequest::GotoImplementation {
                buffer_id: buf,
                origin: orig,
            }
        });
    }

    /// `gr` — goto references (always opens picker).
    pub(crate) fn lsp_goto_references(&mut self) {
        self.lsp_send_goto("textDocument/references", |buf, orig| {
            LspPendingRequest::GotoReferences {
                buffer_id: buf,
                origin: orig,
            }
        });
    }

    /// `K` — show hover info.
    pub(crate) fn lsp_hover(&mut self) {
        self.lsp_send_goto("textDocument/hover", |buf, orig| LspPendingRequest::Hover {
            buffer_id: buf,
            origin: orig,
        });
    }

    // ── Response handlers ─────────────────────────────────────────────────

    /// Dispatch a received LSP response to the appropriate handler.
    pub(crate) fn handle_lsp_response(
        &mut self,
        pending: LspPendingRequest,
        result: Result<serde_json::Value, hjkl_lsp::RpcError>,
    ) {
        match pending {
            LspPendingRequest::GotoDefinition { buffer_id, origin } => {
                self.handle_goto_response(buffer_id, origin, result, "definition");
            }
            LspPendingRequest::GotoDeclaration { buffer_id, origin } => {
                self.handle_goto_response(buffer_id, origin, result, "declaration");
            }
            LspPendingRequest::GotoTypeDefinition { buffer_id, origin } => {
                self.handle_goto_response(buffer_id, origin, result, "type definition");
            }
            LspPendingRequest::GotoImplementation { buffer_id, origin } => {
                self.handle_goto_response(buffer_id, origin, result, "implementation");
            }
            LspPendingRequest::GotoReferences { buffer_id, origin } => {
                self.handle_references_response(buffer_id, origin, result);
            }
            LspPendingRequest::Hover { buffer_id, origin } => {
                self.handle_hover_response(buffer_id, origin, result);
            }
            LspPendingRequest::Completion {
                buffer_id,
                anchor_row,
                anchor_col,
            } => {
                self.handle_completion_response(buffer_id, anchor_row, anchor_col, result);
            }
            LspPendingRequest::CodeAction {
                buffer_id,
                anchor_row,
                anchor_col,
            } => {
                self.handle_code_action_response(buffer_id, anchor_row, anchor_col, result);
            }
            LspPendingRequest::Rename {
                buffer_id,
                anchor_row,
                anchor_col,
                new_name,
            } => {
                self.handle_rename_response(buffer_id, anchor_row, anchor_col, new_name, result);
            }
            LspPendingRequest::Format { buffer_id, range } => {
                self.handle_format_response(buffer_id, range, result);
            }
        }
    }

    /// Normalize a goto-style response into a `Vec<lsp_types::Location>`.
    fn parse_goto_locations(result: serde_json::Value) -> Vec<lsp_types::Location> {
        // The result can be: null, Location, Location[], or LocationLink[].
        if result.is_null() {
            return Vec::new();
        }
        // Try GotoDefinitionResponse (covers all three variants).
        if let Ok(resp) =
            serde_json::from_value::<lsp_types::GotoDefinitionResponse>(result.clone())
        {
            return match resp {
                lsp_types::GotoDefinitionResponse::Scalar(loc) => vec![loc],
                lsp_types::GotoDefinitionResponse::Array(locs) => locs,
                lsp_types::GotoDefinitionResponse::Link(links) => links
                    .into_iter()
                    .map(|l| lsp_types::Location {
                        uri: l.target_uri,
                        range: l.target_selection_range,
                    })
                    .collect(),
            };
        }
        // Fall back to a plain Vec<Location>.
        if let Ok(locs) = serde_json::from_value::<Vec<lsp_types::Location>>(result.clone()) {
            return locs;
        }
        // Single Location.
        if let Ok(loc) = serde_json::from_value::<lsp_types::Location>(result) {
            return vec![loc];
        }
        Vec::new()
    }

    /// Jump the cursor (and possibly switch buffer) to `loc`.
    fn jump_to_location(&mut self, loc: &lsp_types::Location) {
        let target_path: Option<PathBuf> = {
            let url: url::Url = match url::Url::parse(loc.uri.as_str()) {
                Ok(u) => u,
                Err(_) => return,
            };
            hjkl_lsp::uri::to_path(&url)
        };
        let row = loc.range.start.line as usize;
        let col = loc.range.start.character as usize;

        // Determine if target matches an already-open slot.
        let slot_idx = if let Some(ref tp) = target_path {
            self.slots.iter().position(|s| {
                s.filename
                    .as_ref()
                    .map(|p| {
                        let abs_p = if p.is_absolute() {
                            p.clone()
                        } else {
                            std::env::current_dir().unwrap_or_default().join(p)
                        };
                        &abs_p == tp
                    })
                    .unwrap_or(false)
            })
        } else {
            None
        };

        if let Some(idx) = slot_idx {
            // Already open — switch if needed, then move cursor.
            if idx != self.focused_slot_idx() {
                self.switch_to(idx);
            }
        } else if let Some(ref tp) = target_path {
            // Open new slot.
            match self.open_new_slot(tp.clone()) {
                Ok(idx) => {
                    self.switch_to(idx);
                }
                Err(e) => {
                    self.status_message = Some(format!("LSP goto: {e}"));
                    return;
                }
            }
        } else {
            self.status_message = Some("LSP goto: non-file URI".into());
            return;
        }

        self.active_mut().editor.jump_cursor(row, col);
        // jump_cursor only sets cursor; the engine doesn't auto-scroll on
        // host-side jumps. Reveal the cursor before syncing the focused
        // window's stored top_row/top_col back from the editor viewport.
        self.active_mut().editor.ensure_cursor_in_scrolloff();
        self.sync_viewport_from_editor();
    }

    /// Handle a goto-flavour response (definition/declaration/type/implementation).
    pub(crate) fn handle_goto_response(
        &mut self,
        _buffer_id: hjkl_lsp::BufferId,
        _origin: (usize, usize),
        result: Result<serde_json::Value, hjkl_lsp::RpcError>,
        kind_label: &str,
    ) {
        let val = match result {
            Ok(v) => v,
            Err(e) => {
                self.status_message = Some(format!("LSP {kind_label}: {}", e.message));
                return;
            }
        };
        let locs = Self::parse_goto_locations(val);
        if locs.is_empty() {
            self.status_message = Some(format!("no {kind_label} found"));
            return;
        }
        if locs.len() == 1 {
            self.jump_to_location(&locs[0]);
        } else {
            self.open_lsp_locations_picker(&locs, kind_label);
        }
    }

    /// Handle a references response — always opens picker (even single result).
    pub(crate) fn handle_references_response(
        &mut self,
        _buffer_id: hjkl_lsp::BufferId,
        _origin: (usize, usize),
        result: Result<serde_json::Value, hjkl_lsp::RpcError>,
    ) {
        let val = match result {
            Ok(v) => v,
            Err(e) => {
                self.status_message = Some(format!("LSP references: {}", e.message));
                return;
            }
        };
        let locs = Self::parse_goto_locations(val);
        if locs.is_empty() {
            self.status_message = Some("no references found".into());
            return;
        }
        self.open_lsp_locations_picker(&locs, "references");
    }

    /// Open a picker over a set of LSP locations.
    fn open_lsp_locations_picker(&mut self, locs: &[lsp_types::Location], kind_label: &str) {
        use crate::picker_action::AppAction;

        // Build (label, action) pairs.
        let entries: Vec<(String, AppAction)> = locs
            .iter()
            .filter_map(|loc| {
                let url: url::Url = url::Url::parse(loc.uri.as_str()).ok()?;
                let path = hjkl_lsp::uri::to_path(&url)?;
                let row = loc.range.start.line;
                let col = loc.range.start.character as usize;
                let label = format!("{}:{}: col {}", path.display(), row + 1, col + 1);
                // Use OpenPathAtLine for the action — goto_line is 1-based.
                Some((label, AppAction::OpenPathAtLine(path, row + 1)))
            })
            .collect();

        if entries.is_empty() {
            self.status_message = Some(format!("no {kind_label} found"));
            return;
        }

        let source = Box::new(crate::picker_sources::StaticListSource::new(
            kind_label.to_string(),
            entries,
        ));
        self.picker = Some(crate::picker::Picker::new(source));
    }

    // ── Completion ────────────────────────────────────────────────────────

    /// Check if `ch` is a trigger character for the active LSP server, and if
    /// so, fire a completion request. Called after inserting a char in insert mode.
    pub(crate) fn maybe_auto_trigger_completion(&mut self, ch: char) {
        // Need an active LSP server with capabilities.
        let triggers: Vec<String> = self
            .active()
            .filename
            .as_ref()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .and_then(|ext| language_id_for_ext(ext))
            .and_then(|lang| {
                // Find a server key matching this language in lsp_state.
                self.lsp_state.iter().find_map(|(key, info)| {
                    if key.language == lang {
                        // Pull triggerCharacters from capabilities.
                        info.capabilities
                            .pointer("/completionProvider/triggerCharacters")
                            .and_then(|v| v.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|s| s.as_str().map(|s| s.to_string()))
                                    .collect()
                            })
                    } else {
                        None
                    }
                })
            })
            .unwrap_or_default();

        let ch_str = ch.to_string();
        if triggers.contains(&ch_str) {
            self.lsp_request_completion();
        }
    }

    /// Send a `textDocument/completion` request for the current cursor position.
    pub(crate) fn lsp_request_completion(&mut self) {
        if self.lsp.is_none() {
            self.status_message =
                Some("LSP: not enabled (set [lsp] enabled = true in config)".into());
            return;
        }
        let (params, buffer_id, (row, col)) = match self.lsp_position_params() {
            Some(v) => v,
            None => {
                self.status_message = Some(
                    "LSP: no file open in this buffer (use :e <file> or open from the picker)"
                        .into(),
                );
                return;
            }
        };
        let request_id = self.lsp_alloc_request_id();
        self.lsp_pending.insert(
            request_id,
            LspPendingRequest::Completion {
                buffer_id,
                anchor_row: row,
                anchor_col: col,
            },
        );
        if let Some(mgr) = self.lsp.as_ref() {
            mgr.send_request(request_id, buffer_id, "textDocument/completion", params);
        }
    }

    // ── Code actions ──────────────────────────────────────────────────────

    /// `<leader>ca` — request code actions at the cursor.
    pub(crate) fn lsp_code_actions(&mut self) {
        if self.lsp.is_none() {
            self.status_message =
                Some("LSP: not enabled (set [lsp] enabled = true in config)".into());
            return;
        }
        let slot = self.active();
        let path = match slot.filename.as_ref() {
            Some(p) => absolutize(p),
            None => {
                self.status_message = Some(
                    "LSP: no file open in this buffer (use :e <file> or open from the picker)"
                        .into(),
                );
                return;
            }
        };
        let uri = match hjkl_lsp::uri::from_path(&path).ok() {
            Some(u) => u,
            None => {
                self.status_message = Some("LSP: cannot build URI".into());
                return;
            }
        };
        let cursor = slot.editor.buffer().cursor();
        let row = cursor.row as u32;
        let col = cursor.col as u32;
        let buffer_id = slot.buffer_id as hjkl_lsp::BufferId;

        // Collect diagnostics that overlap the cursor position.
        let overlapping_diags: Vec<lsp_types::Diagnostic> = slot
            .lsp_diags
            .iter()
            .filter(|d| {
                let after_start = (cursor.row, cursor.col) >= (d.start_row, d.start_col);
                let before_end = cursor.row < d.end_row
                    || (cursor.row == d.end_row && cursor.col < d.end_col)
                    || (cursor.row == d.start_row && d.start_row == d.end_row);
                after_start && (before_end || cursor.row == d.start_row)
            })
            .map(|d| {
                let severity = match d.severity {
                    super::DiagSeverity::Error => Some(lsp_types::DiagnosticSeverity::ERROR),
                    super::DiagSeverity::Warning => Some(lsp_types::DiagnosticSeverity::WARNING),
                    super::DiagSeverity::Info => Some(lsp_types::DiagnosticSeverity::INFORMATION),
                    super::DiagSeverity::Hint => Some(lsp_types::DiagnosticSeverity::HINT),
                };
                let code = d.code.as_ref().map(|c| {
                    if let Ok(n) = c.parse::<i32>() {
                        lsp_types::NumberOrString::Number(n)
                    } else {
                        lsp_types::NumberOrString::String(c.clone())
                    }
                });
                lsp_types::Diagnostic {
                    range: lsp_types::Range {
                        start: lsp_types::Position {
                            line: d.start_row as u32,
                            character: d.start_col as u32,
                        },
                        end: lsp_types::Position {
                            line: d.end_row as u32,
                            character: d.end_col as u32,
                        },
                    },
                    severity,
                    code,
                    source: d.source.clone(),
                    message: d.message.clone(),
                    ..Default::default()
                }
            })
            .collect();

        let params = json!({
            "textDocument": { "uri": uri.as_str() },
            "range": {
                "start": { "line": row, "character": col },
                "end": { "line": row, "character": col },
            },
            "context": {
                "diagnostics": overlapping_diags,
                "triggerKind": 1, // CodeActionTriggerKind::Invoked
            },
        });

        let request_id = self.lsp_alloc_request_id();
        self.lsp_pending.insert(
            request_id,
            LspPendingRequest::CodeAction {
                buffer_id,
                anchor_row: cursor.row,
                anchor_col: cursor.col,
            },
        );
        if let Some(mgr) = self.lsp.as_ref() {
            mgr.send_request(request_id, buffer_id, "textDocument/codeAction", params);
        }
    }

    /// Handle a `textDocument/codeAction` response.
    pub(crate) fn handle_code_action_response(
        &mut self,
        _buffer_id: hjkl_lsp::BufferId,
        _anchor_row: usize,
        _anchor_col: usize,
        result: Result<serde_json::Value, hjkl_lsp::RpcError>,
    ) {
        let val = match result {
            Ok(v) => v,
            Err(e) => {
                self.status_message = Some(format!("LSP codeAction: {}", e.message));
                return;
            }
        };

        if val.is_null() {
            self.status_message = Some("no code actions".into());
            return;
        }

        let actions: Vec<lsp_types::CodeActionOrCommand> = match serde_json::from_value(val) {
            Ok(a) => a,
            Err(_) => {
                self.status_message = Some("LSP codeAction: could not parse response".into());
                return;
            }
        };

        if actions.is_empty() {
            self.status_message = Some("no code actions".into());
            return;
        }

        if actions.len() == 1 {
            let action = actions.into_iter().next().unwrap();
            self.apply_code_action_or_command(action);
            return;
        }

        // Multiple actions — open picker. Store actions in pending_code_actions
        // so the picker can index into them via ApplyCodeAction(i).
        use crate::picker_action::AppAction;
        let entries: Vec<(String, AppAction)> = actions
            .iter()
            .enumerate()
            .map(|(i, action)| {
                let label = match action {
                    lsp_types::CodeActionOrCommand::CodeAction(ca) => ca.title.clone(),
                    lsp_types::CodeActionOrCommand::Command(cmd) => cmd.title.clone(),
                };
                (label, AppAction::ApplyCodeAction(i))
            })
            .collect();

        self.pending_code_actions = actions;
        let source = Box::new(crate::picker_sources::StaticListSource::new(
            "code actions".to_string(),
            entries,
        ));
        self.picker = Some(crate::picker::Picker::new(source));
    }

    /// Apply a single code action or command.
    pub(crate) fn apply_code_action_or_command(&mut self, item: lsp_types::CodeActionOrCommand) {
        match item {
            lsp_types::CodeActionOrCommand::CodeAction(ca) => {
                // Apply workspace edit first, then execute command if present.
                if let Some(edit) = ca.edit {
                    match self.apply_workspace_edit(edit) {
                        Ok(count) => {
                            self.status_message = Some(format!("{count} files changed"));
                        }
                        Err(e) => {
                            self.status_message = Some(format!("LSP codeAction: {e}"));
                            return;
                        }
                    }
                }
                if let Some(cmd) = ca.command {
                    self.lsp_execute_command(&cmd.command, cmd.arguments.unwrap_or_default());
                }
            }
            lsp_types::CodeActionOrCommand::Command(cmd) => {
                self.lsp_execute_command(&cmd.command, cmd.arguments.unwrap_or_default());
            }
        }
    }

    /// Fire-and-forget `workspace/executeCommand`. Phase 5: no response handling.
    fn lsp_execute_command(&mut self, command: &str, args: Vec<serde_json::Value>) {
        let buffer_id = self.active().buffer_id as hjkl_lsp::BufferId;
        let request_id = self.lsp_alloc_request_id();
        let params = json!({
            "command": command,
            "arguments": args,
        });
        // Fire and forget — don't register a pending entry.
        if let Some(mgr) = self.lsp.as_ref() {
            mgr.send_request(request_id, buffer_id, "workspace/executeCommand", params);
        }
    }

    // ── WorkspaceEdit application ─────────────────────────────────────────

    /// Apply a `WorkspaceEdit` to the open slots (and open new ones as needed).
    /// Returns the count of files changed on success, or an error string.
    pub(crate) fn apply_workspace_edit(
        &mut self,
        edit: lsp_types::WorkspaceEdit,
    ) -> Result<usize, String> {
        // Collect (url, Vec<TextEdit>) pairs from either .changes or .document_changes.
        let mut file_edits: Vec<(url::Url, Vec<lsp_types::TextEdit>)> = Vec::new();

        if let Some(doc_changes) = edit.document_changes {
            match doc_changes {
                lsp_types::DocumentChanges::Edits(edits) => {
                    for tde in edits {
                        let url: url::Url = url::Url::parse(tde.text_document.uri.as_str())
                            .map_err(|e| format!("bad URI: {e}"))?;
                        let text_edits: Vec<lsp_types::TextEdit> = tde
                            .edits
                            .into_iter()
                            .filter_map(|e| match e {
                                lsp_types::OneOf::Left(te) => Some(te),
                                lsp_types::OneOf::Right(_) => None, // skip annotated edits
                            })
                            .collect();
                        file_edits.push((url, text_edits));
                    }
                }
                lsp_types::DocumentChanges::Operations(ops) => {
                    for op in ops {
                        match op {
                            lsp_types::DocumentChangeOperation::Edit(tde) => {
                                let url: url::Url = url::Url::parse(tde.text_document.uri.as_str())
                                    .map_err(|e| format!("bad URI: {e}"))?;
                                let text_edits: Vec<lsp_types::TextEdit> = tde
                                    .edits
                                    .into_iter()
                                    .filter_map(|e| match e {
                                        lsp_types::OneOf::Left(te) => Some(te),
                                        lsp_types::OneOf::Right(_) => None,
                                    })
                                    .collect();
                                file_edits.push((url, text_edits));
                            }
                            lsp_types::DocumentChangeOperation::Op(_) => {
                                // TODO: file create/rename/delete not supported in Phase 5.
                            }
                        }
                    }
                }
            }
        } else if let Some(changes) = edit.changes {
            for (uri, edits) in changes {
                let url: url::Url =
                    url::Url::parse(uri.as_str()).map_err(|e| format!("bad URI: {e}"))?;
                file_edits.push((url, edits));
            }
        }

        let count = file_edits.len();
        for (url, mut edits) in file_edits {
            // Find or open the slot for this URI.
            let target_path = hjkl_lsp::uri::to_path(&url);
            let slot_idx = if let Some(ref tp) = target_path {
                // Try to find an existing slot.
                let existing = self.slots.iter().position(|s| {
                    s.filename
                        .as_ref()
                        .map(|p| {
                            let abs = if p.is_absolute() {
                                p.clone()
                            } else {
                                std::env::current_dir().unwrap_or_default().join(p)
                            };
                            &abs == tp
                        })
                        .unwrap_or(false)
                });
                match existing {
                    Some(idx) => idx,
                    None => self.open_new_slot(tp.clone())?,
                }
            } else {
                return Err(format!("non-file URI: {url}"));
            };

            // Sort edits by range END descending so applying later edits
            // doesn't shift the positions of earlier ones.
            edits.sort_by(|a, b| {
                let ea = (a.range.end.line, a.range.end.character);
                let eb = (b.range.end.line, b.range.end.character);
                eb.cmp(&ea)
            });

            use hjkl_engine::{BufferEdit, Pos};
            for te in edits {
                let start = Pos {
                    line: te.range.start.line,
                    col: te.range.start.character,
                };
                let end = Pos {
                    line: te.range.end.line,
                    col: te.range.end.character,
                };
                BufferEdit::replace_range(
                    self.slots[slot_idx].editor.buffer_mut(),
                    start..end,
                    &te.new_text,
                );
            }

            // Mark dirty.
            let _ = self.slots[slot_idx].editor.take_dirty();
            self.slots[slot_idx].dirty = true;
        }

        Ok(count)
    }

    // ── Rename ────────────────────────────────────────────────────────────

    /// Send a `textDocument/rename` request.
    pub(crate) fn lsp_rename(&mut self, new_name: String) {
        if self.lsp.is_none() {
            self.status_message =
                Some("LSP: not enabled (set [lsp] enabled = true in config)".into());
            return;
        }
        let slot = self.active();
        let path = match slot.filename.as_ref() {
            Some(p) => absolutize(p),
            None => {
                self.status_message = Some(
                    "LSP: no file open in this buffer (use :e <file> or open from the picker)"
                        .into(),
                );
                return;
            }
        };
        let uri = match hjkl_lsp::uri::from_path(&path).ok() {
            Some(u) => u,
            None => {
                self.status_message = Some("LSP: cannot build URI".into());
                return;
            }
        };
        let cursor = slot.editor.buffer().cursor();
        let buffer_id = slot.buffer_id as hjkl_lsp::BufferId;

        let params = json!({
            "textDocument": { "uri": uri.as_str() },
            "position": { "line": cursor.row as u32, "character": cursor.col as u32 },
            "newName": new_name,
        });

        let request_id = self.lsp_alloc_request_id();
        self.lsp_pending.insert(
            request_id,
            LspPendingRequest::Rename {
                buffer_id,
                anchor_row: cursor.row,
                anchor_col: cursor.col,
                new_name,
            },
        );
        if let Some(mgr) = self.lsp.as_ref() {
            mgr.send_request(request_id, buffer_id, "textDocument/rename", params);
        }
    }

    /// Handle a `textDocument/rename` response.
    pub(crate) fn handle_rename_response(
        &mut self,
        _buffer_id: hjkl_lsp::BufferId,
        _anchor_row: usize,
        _anchor_col: usize,
        _new_name: String,
        result: Result<serde_json::Value, hjkl_lsp::RpcError>,
    ) {
        let val = match result {
            Ok(v) => v,
            Err(e) => {
                self.status_message = Some(format!("LSP rename: {}", e.message));
                return;
            }
        };

        if val.is_null() {
            self.status_message = Some("E: cannot rename here".into());
            return;
        }

        let workspace_edit: lsp_types::WorkspaceEdit = match serde_json::from_value(val) {
            Ok(we) => we,
            Err(_) => {
                self.status_message = Some("LSP rename: could not parse response".into());
                return;
            }
        };

        match self.apply_workspace_edit(workspace_edit) {
            Ok(count) => {
                self.status_message = Some(format!("renamed: {count} files changed"));
            }
            Err(e) => {
                self.status_message = Some(format!("LSP rename: {e}"));
            }
        }
    }

    // ── Format ────────────────────────────────────────────────────────────

    /// `:LspFormat` — send a `textDocument/formatting` request.
    pub(crate) fn lsp_format(&mut self) {
        if self.lsp.is_none() {
            self.status_message =
                Some("LSP: not enabled (set [lsp] enabled = true in config)".into());
            return;
        }
        let slot = self.active();
        let path = match slot.filename.as_ref() {
            Some(p) => absolutize(p),
            None => {
                self.status_message = Some(
                    "LSP: no file open in this buffer (use :e <file> or open from the picker)"
                        .into(),
                );
                return;
            }
        };
        let uri = match hjkl_lsp::uri::from_path(&path).ok() {
            Some(u) => u,
            None => {
                self.status_message = Some("LSP: cannot build URI".into());
                return;
            }
        };
        let buffer_id = slot.buffer_id as hjkl_lsp::BufferId;
        let tab_size = slot.editor.settings().tabstop as u32;
        let insert_spaces = slot.editor.settings().expandtab;

        let params = json!({
            "textDocument": { "uri": uri.as_str() },
            "options": {
                "tabSize": tab_size,
                "insertSpaces": insert_spaces,
            },
        });

        let request_id = self.lsp_alloc_request_id();
        self.lsp_pending.insert(
            request_id,
            LspPendingRequest::Format {
                buffer_id,
                range: None,
            },
        );
        if let Some(mgr) = self.lsp.as_ref() {
            mgr.send_request(request_id, buffer_id, "textDocument/formatting", params);
        }
    }

    /// Handle a `textDocument/formatting` response.
    pub(crate) fn handle_format_response(
        &mut self,
        buffer_id: hjkl_lsp::BufferId,
        _range: Option<(usize, usize, usize, usize)>,
        result: Result<serde_json::Value, hjkl_lsp::RpcError>,
    ) {
        let val = match result {
            Ok(v) => v,
            Err(e) => {
                self.status_message = Some(format!("LSP format: {}", e.message));
                return;
            }
        };

        if val.is_null() {
            self.status_message = Some("no formatting changes".into());
            return;
        }

        let edits: Vec<lsp_types::TextEdit> = match serde_json::from_value(val) {
            Ok(e) => e,
            Err(_) => {
                self.status_message = Some("LSP format: could not parse response".into());
                return;
            }
        };

        if edits.is_empty() {
            self.status_message = Some("no formatting changes".into());
            return;
        }

        // Find the slot matching buffer_id.
        let slot_idx = self
            .slots
            .iter()
            .position(|s| s.buffer_id as hjkl_lsp::BufferId == buffer_id);
        let slot_idx = match slot_idx {
            Some(i) => i,
            None => {
                self.status_message = Some("LSP format: buffer no longer open".into());
                return;
            }
        };

        // Sort edits by range END descending.
        let mut sorted = edits;
        sorted.sort_by(|a, b| {
            let ea = (a.range.end.line, a.range.end.character);
            let eb = (b.range.end.line, b.range.end.character);
            eb.cmp(&ea)
        });

        use hjkl_engine::{BufferEdit, Pos};
        for te in sorted {
            let start = Pos {
                line: te.range.start.line,
                col: te.range.start.character,
            };
            let end = Pos {
                line: te.range.end.line,
                col: te.range.end.character,
            };
            BufferEdit::replace_range(
                self.slots[slot_idx].editor.buffer_mut(),
                start..end,
                &te.new_text,
            );
        }

        let _ = self.slots[slot_idx].editor.take_dirty();
        self.slots[slot_idx].dirty = true;
        self.status_message = Some("formatted".into());
    }

    /// Handle a `textDocument/completion` response.
    pub(crate) fn handle_completion_response(
        &mut self,
        buffer_id: hjkl_lsp::BufferId,
        anchor_row: usize,
        anchor_col: usize,
        result: Result<serde_json::Value, hjkl_lsp::RpcError>,
    ) {
        let val = match result {
            Ok(v) => v,
            Err(e) => {
                self.status_message = Some(format!("LSP completion: {}", e.message));
                return;
            }
        };

        // Guard: discard if user left insert mode or switched buffer.
        use hjkl_engine::VimMode;
        if self.active().editor.vim_mode() != VimMode::Insert {
            return;
        }
        if (self.active().buffer_id as hjkl_lsp::BufferId) != buffer_id {
            return;
        }

        // Parse CompletionResponse (null | CompletionList | Vec<CompletionItem>).
        let lsp_items: Vec<lsp_types::CompletionItem> = if val.is_null() {
            Vec::new()
        } else if let Ok(list) = serde_json::from_value::<lsp_types::CompletionList>(val.clone()) {
            list.items
        } else {
            serde_json::from_value::<Vec<lsp_types::CompletionItem>>(val).unwrap_or_default()
        };

        if lsp_items.is_empty() {
            self.status_message = Some("no completions".into());
            return;
        }

        let items: Vec<crate::completion::CompletionItem> =
            lsp_items.into_iter().map(item_from_lsp).collect();
        self.completion = Some(Completion::new(anchor_row, anchor_col, items));
    }

    /// Accept the currently selected completion item, inserting its text
    /// into the buffer and dismissing the popup.
    ///
    /// Replace strategy: delete from `anchor_col` to the current cursor col
    /// on the same row, then insert `insert_text` at that position.
    /// TODO: honour `text_edit` with non-prefix ranges (filed for follow-up).
    pub(crate) fn accept_completion(&mut self) {
        let popup = match self.completion.take() {
            Some(p) => p,
            None => return,
        };
        let item = match popup.selected_item() {
            Some(i) => i.clone(),
            None => return,
        };

        use hjkl_engine::{BufferEdit, Pos};
        let cursor = self.active().editor.buffer().cursor();
        let row = cursor.row;
        let cur_col = cursor.col;
        let anchor_col = popup.anchor_col.min(cur_col);

        let start = Pos {
            line: row as u32,
            col: anchor_col as u32,
        };
        let end = Pos {
            line: row as u32,
            col: cur_col as u32,
        };

        // Replace [anchor_col, cur_col) with insert_text.
        BufferEdit::replace_range(
            self.active_mut().editor.buffer_mut(),
            start..end,
            &item.insert_text,
        );

        // Move cursor past the inserted text.
        let new_col = anchor_col + item.insert_text.len();
        self.active_mut().editor.jump_cursor(row, new_col);
        // completion was already taken via `take()` above, so it's already None.
    }

    /// Handle a hover response — set `info_popup` with extracted text.
    pub(crate) fn handle_hover_response(
        &mut self,
        _buffer_id: hjkl_lsp::BufferId,
        _origin: (usize, usize),
        result: Result<serde_json::Value, hjkl_lsp::RpcError>,
    ) {
        let val = match result {
            Ok(v) => v,
            Err(e) => {
                self.status_message = Some(format!("LSP hover: {}", e.message));
                return;
            }
        };
        if val.is_null() {
            self.status_message = Some("no hover info".into());
            return;
        }
        let hover: lsp_types::Hover = match serde_json::from_value(val) {
            Ok(h) => h,
            Err(_) => {
                self.status_message = Some("LSP hover: could not parse response".into());
                return;
            }
        };
        let text = extract_hover_text(&hover.contents);
        if text.trim().is_empty() {
            self.status_message = Some("no hover info".into());
        } else {
            self.info_popup = Some(text);
        }
    }
}

/// Extract plain text from LSP hover contents.
/// TODO(#15): replace with proper Markdown rendering once hjkl-md lands.
fn extract_hover_text(contents: &lsp_types::HoverContents) -> String {
    match contents {
        lsp_types::HoverContents::Scalar(ms) => marked_string_text(ms),
        lsp_types::HoverContents::Array(items) => items
            .iter()
            .map(marked_string_text)
            .collect::<Vec<_>>()
            .join("\n\n"),
        lsp_types::HoverContents::Markup(mc) => strip_markdown(&mc.value),
    }
}

fn marked_string_text(ms: &lsp_types::MarkedString) -> String {
    match ms {
        lsp_types::MarkedString::String(s) => strip_markdown(s),
        lsp_types::MarkedString::LanguageString(ls) => {
            format!("[{}]\n{}", ls.language, ls.value)
        }
    }
}

/// Minimal markdown stripper — removes backtick fences and asterisks.
/// TODO(#15): replace with proper renderer.
fn strip_markdown(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_fence = false;
    for line in s.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            if in_fence {
                // Emit language hint as a label if present.
                let lang = trimmed.trim_start_matches('`').trim();
                if !lang.is_empty() {
                    out.push_str(lang);
                    out.push('\n');
                }
            }
            continue;
        }
        // Strip inline code backticks and bold/italic asterisks.
        let stripped: String = line.replace("**", "").replace(['*', '`'], "");
        out.push_str(&stripped);
        out.push('\n');
    }
    out
}
