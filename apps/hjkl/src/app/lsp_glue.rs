//! LSP glue — bridges `App` state with `hjkl_lsp::LspManager`.

use ratatui::style::{Color, Modifier, Style};

use super::{App, DiagSeverity, LspDiag, LspServerInfo};

/// Small inline map: file extension → LSP language id.
fn language_id_for_ext(ext: &str) -> Option<&'static str> {
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
                hjkl_lsp::LspEvent::Response { request_id, .. } => {
                    tracing::debug!(request_id, "lsp response");
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
            Some(p) => p.clone(),
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
}
