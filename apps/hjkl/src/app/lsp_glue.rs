//! LSP glue — bridges `App` state with `hjkl_lsp::LspManager`.

use super::App;

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

impl App {
    /// Drain all pending LSP events and dispatch them to logging.
    /// Called at the top of every event-loop iteration.
    pub fn drain_lsp_events(&mut self) {
        if let Some(ref mgr) = self.lsp {
            while let Some(evt) = mgr.try_recv_event() {
                match evt {
                    hjkl_lsp::LspEvent::ServerInitialized { key, .. } => {
                        tracing::info!(?key, "lsp server initialized");
                    }
                    hjkl_lsp::LspEvent::ServerExited { key, status } => {
                        tracing::warn!(?key, ?status, "lsp server exited");
                    }
                    hjkl_lsp::LspEvent::Notification { key, method, .. } => {
                        tracing::debug!(?key, method, "lsp notification");
                    }
                    hjkl_lsp::LspEvent::Response { request_id, .. } => {
                        tracing::debug!(request_id, "lsp response");
                    }
                }
            }
        }
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

        let text = slot.editor.buffer().lines().to_vec().join("\n");

        let buffer_id = slot.buffer_id as hjkl_lsp::BufferId;
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
